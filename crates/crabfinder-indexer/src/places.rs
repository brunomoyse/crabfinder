//! OpenStreetMap place names (schema v8, `--places`).
//!
//! BeSt names streets, municipalities, Walloon parts of municipalities and
//! (Flemish) postal areas, but not hamlets: "Maredsous" or "Frahan" only
//! exist in OSM. `scripts/build_osm_places.py` extracts Belgian
//! `place=hamlet|village|locality|isolated_dwelling` nodes into
//! `data/osm-places-be-v1.csv`; this module loads that CSV into an R-tree
//! and attaches to each address the nearest place within a radius that
//! depends on the place type. A place whose name the BeSt hierarchy already
//! carries for that address (municipality, part of municipality, postal
//! name) adds nothing and is skipped.
//!
//! Data © OpenStreetMap contributors, ODbL 1.0.

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::Path;

use anyhow::{Context, Result, bail};
use rstar::{AABB, PointDistance, RTree, RTreeObject};
use serde::{Deserialize, Serialize};
use tantivy::tokenizer::TextAnalyzer;

use crabfinder_core::{Address, Coordinates, MultilingualName, Region};

use crate::index::build_text_analyzer;

/// OSM `place=*` values the indexer understands.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlaceKind {
    Village,
    Hamlet,
    Locality,
    IsolatedDwelling,
}

impl PlaceKind {
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "village" => Some(Self::Village),
            "hamlet" => Some(Self::Hamlet),
            "locality" => Some(Self::Locality),
            "isolated_dwelling" => Some(Self::IsolatedDwelling),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Village => "village",
            Self::Hamlet => "hamlet",
            Self::Locality => "locality",
            Self::IsolatedDwelling => "isolated_dwelling",
        }
    }

    /// Farthest an address may be from the node to belong to the place.
    pub fn radius_m(self) -> f64 {
        match self {
            Self::Village => 3_000.0,
            Self::Hamlet => 1_500.0,
            Self::Locality | Self::IsolatedDwelling => 1_000.0,
        }
    }
}

/// Largest `radius_m` — the R-tree walk stops beyond it.
const MAX_RADIUS_M: f64 = 3_000.0;

/// One OSM place node.
#[derive(Debug, Clone, PartialEq)]
pub struct Place {
    pub osm_id: i64,
    pub kind: PlaceKind,
    /// OSM `name` (local language).
    pub name: String,
    /// `name:fr` / `name:nl` / `name:de` when tagged.
    pub names: MultilingualName,
    pub lat: f64,
    pub lon: f64,
}

impl Place {
    /// Names to attach to an address in `region`: the language-tagged
    /// names, with the untagged local name filled into the region's
    /// language when that slot is empty.
    pub fn names_for(&self, region: Region) -> MultilingualName {
        let mut n = self.names.clone();
        let tagged = [&n.fr, &n.nl, &n.de]
            .iter()
            .any(|v| v.as_deref() == Some(self.name.as_str()));
        let slot = match region {
            Region::Flanders => &mut n.nl,
            Region::Brussels | Region::Wallonia => &mut n.fr,
        };
        if slot.is_none() && !tagged {
            *slot = Some(self.name.clone());
        }
        n
    }

    fn all_names(&self) -> impl Iterator<Item = &str> {
        std::iter::once(self.name.as_str()).chain(
            [&self.names.fr, &self.names.nl, &self.names.de]
                .into_iter()
                .flatten()
                .map(String::as_str),
        )
    }
}

/// Planar metres around Belgium's mean latitude: accurate to a few percent
/// over the country, plenty for kilometre radii.
fn to_xy(lat: f64, lon: f64) -> [f64; 2] {
    const M_PER_DEG_LAT: f64 = 111_132.0;
    let m_per_deg_lon = M_PER_DEG_LAT * 50.5_f64.to_radians().cos();
    [lon * m_per_deg_lon, lat * M_PER_DEG_LAT]
}

struct Node {
    xy: [f64; 2],
    idx: usize,
}

impl RTreeObject for Node {
    type Envelope = AABB<[f64; 2]>;
    fn envelope(&self) -> Self::Envelope {
        AABB::from_point(self.xy)
    }
}

impl PointDistance for Node {
    fn distance_2(&self, p: &[f64; 2]) -> f64 {
        let dx = self.xy[0] - p[0];
        let dy = self.xy[1] - p[1];
        dx * dx + dy * dy
    }
}

#[derive(Debug, Deserialize)]
struct CsvRow {
    osm_id: i64,
    place: String,
    name: String,
    #[serde(default)]
    name_fr: String,
    #[serde(default)]
    name_nl: String,
    #[serde(default)]
    name_de: String,
    lat: f64,
    lon: f64,
}

fn non_empty(s: String) -> Option<String> {
    let t = s.trim();
    (!t.is_empty()).then(|| t.to_string())
}

/// Result of `PlaceIndex::assign`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Assignment {
    /// Index into `PlaceIndex::places`.
    pub idx: usize,
    /// The place's name isn't already the address's municipality, part of
    /// municipality or postal name.
    pub attach: bool,
}

/// Spatial index over OSM places.
pub struct PlaceIndex {
    places: Vec<Place>,
    /// `fold`ed names of each place, parallel to `places`.
    folded: Vec<Vec<String>>,
    tree: RTree<Node>,
    folder: RefCell<Folder>,
}

/// `crab`-analyzer folding with a memo: municipality and postal names
/// repeat across millions of addresses.
struct Folder {
    analyzer: TextAnalyzer,
    memo: HashMap<String, String>,
}

impl Folder {
    fn new() -> Self {
        Self {
            analyzer: build_text_analyzer(),
            memo: HashMap::new(),
        }
    }

    fn fold(&mut self, s: &str) -> String {
        if let Some(f) = self.memo.get(s) {
            return f.clone();
        }
        let mut stream = self.analyzer.token_stream(s);
        let mut out = Vec::new();
        while let Some(t) = stream.next() {
            out.push(t.text.clone());
        }
        let folded = out.join(" ");
        self.memo.insert(s.to_string(), folded.clone());
        folded
    }
}

impl PlaceIndex {
    pub fn empty() -> Self {
        Self::from_places(Vec::new())
    }

    pub fn from_places(places: Vec<Place>) -> Self {
        let mut folder = Folder::new();
        let folded = places
            .iter()
            .map(|p| p.all_names().map(|n| folder.fold(n)).collect())
            .collect();
        let nodes = places
            .iter()
            .enumerate()
            .map(|(idx, p)| Node {
                xy: to_xy(p.lat, p.lon),
                idx,
            })
            .collect();
        Self {
            places,
            folded,
            tree: RTree::bulk_load(nodes),
            folder: RefCell::new(folder),
        }
    }

    /// Read the `build_osm_places.py` CSV. Rows with an unknown `place`
    /// value or an empty name are skipped.
    pub fn read_csv(reader: impl std::io::Read) -> Result<Self> {
        let mut places = Vec::new();
        let mut rdr = csv::Reader::from_reader(reader);
        for (i, row) in rdr.deserialize::<CsvRow>().enumerate() {
            let row = row.with_context(|| format!("places CSV row {}", i + 2))?;
            let Some(kind) = PlaceKind::parse(&row.place) else {
                continue;
            };
            let Some(name) = non_empty(row.name) else {
                continue;
            };
            if !(row.lat.is_finite() && row.lon.is_finite()) {
                bail!("places CSV row {}: non-finite coordinates", i + 2);
            }
            places.push(Place {
                osm_id: row.osm_id,
                kind,
                name,
                names: MultilingualName {
                    fr: non_empty(row.name_fr),
                    nl: non_empty(row.name_nl),
                    de: non_empty(row.name_de),
                },
                lat: row.lat,
                lon: row.lon,
            });
        }
        Ok(Self::from_places(places))
    }

    pub fn load(path: &Path) -> Result<Self> {
        let f = std::fs::File::open(path).with_context(|| format!("opening {path:?}"))?;
        Self::read_csv(std::io::BufReader::new(f))
    }

    pub fn len(&self) -> usize {
        self.places.len()
    }

    pub fn is_empty(&self) -> bool {
        self.places.is_empty()
    }

    pub fn places(&self) -> &[Place] {
        &self.places
    }

    /// Nearest place whose type radius covers `(lat, lon)`.
    pub fn nearest(&self, lat: f64, lon: f64) -> Option<&Place> {
        self.nearest_idx(lat, lon).map(|i| &self.places[i])
    }

    fn nearest_idx(&self, lat: f64, lon: f64) -> Option<usize> {
        let xy = to_xy(lat, lon);
        self.tree
            .nearest_neighbor_iter_with_distance_2(xy)
            .take_while(|(_, d2)| *d2 <= MAX_RADIUS_M * MAX_RADIUS_M)
            .find(|(n, d2)| d2.sqrt() <= self.places[n.idx].kind.radius_m())
            .map(|(n, _)| n.idx)
    }

    /// The place covering `addr` (an index into `places()`), and whether
    /// its name is new for the address. `None` when no place covers it.
    pub fn assign(&self, addr: &Address) -> Option<Assignment> {
        let Coordinates::Wgs84(w) = addr.coordinates else {
            return None;
        };
        let idx = self.nearest_idx(w.lat, w.lon)?;
        let mut folder = self.folder.borrow_mut();
        let known: Vec<String> = [
            Some(&addr.municipality_name),
            Some(&addr.postal_name),
            addr.part_of_municipality_name.as_ref(),
        ]
        .into_iter()
        .flatten()
        .flat_map(|n| [&n.fr, &n.nl, &n.de])
        .flatten()
        .map(|s| folder.fold(s))
        .collect();
        let attach = !self.folded[idx].iter().any(|n| known.contains(n));
        Some(Assignment { idx, attach })
    }

    /// The place to attach to `addr`, or `None` when no place covers it
    /// or the place only repeats a name the address already has.
    pub fn place_for(&self, addr: &Address) -> Option<&Place> {
        self.assign(addr)
            .filter(|a| a.attach)
            .map(|a| &self.places[a.idx])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crabfinder_core::{
        AddressId, BestId, BuildingGroupId, MunicipalityId, PostalInfoId, Status, StreetId,
        StreetType, WgsCoord,
    };

    const CSV: &str = "osm_id,place,name,name_fr,name_nl,name_de,lat,lon
1,village,Falaën,,,,50.2782,4.7934
2,hamlet,Maredsous,,,,50.3000,4.7700
3,locality,Le Bois,,,,50.3100,4.7700
4,isolated_dwelling,Ferme X,,,,50.3200,4.7700
5,city,Namur,,,,50.4600,4.8700
6,village,,,,,50.0,4.0
7,village,Leefdaal,Leefdael,Leefdaal,,50.8473,4.5912
";

    fn index() -> PlaceIndex {
        PlaceIndex::read_csv(CSV.as_bytes()).unwrap()
    }

    fn ml(fr: Option<&str>, nl: Option<&str>) -> MultilingualName {
        MultilingualName {
            fr: fr.map(Into::into),
            nl: nl.map(Into::into),
            de: None,
        }
    }

    fn addr(lat: f64, lon: f64, muni: &str, pom: Option<&str>) -> Address {
        let id = |ns: &str| BestId::new(ns, "1", "1");
        let street_id = StreetId::new(id("s"));
        Address {
            id: AddressId::new(id("a")),
            building_group_id: BuildingGroupId::from_parts(&street_id, "1"),
            street_id,
            municipality_id: MunicipalityId::new(id("m")),
            postal_info_id: PostalInfoId::new(id("p")),
            part_of_municipality_id: None,
            region: Region::Wallonia,
            status: Status::Current,
            house_number: "1".into(),
            box_number: None,
            officially_assigned: true,
            street_name: ml(Some("Rue"), None),
            street_type: StreetType::Streetname,
            municipality_name: ml(Some(muni), None),
            postal_code: "5522".into(),
            postal_name: MultilingualName::default(),
            part_of_municipality_name: pom.map(|p| ml(Some(p), None)),
            coordinates: Coordinates::Wgs84(WgsCoord { lat, lon }),
            position_source: Default::default(),
        }
    }

    #[test]
    fn csv_skips_unknown_types_and_empty_names() {
        let idx = index();
        assert_eq!(idx.len(), 5);
        assert!(!idx.is_empty());
        let leefdaal = idx.places().iter().find(|p| p.osm_id == 7).unwrap();
        assert_eq!(leefdaal.names, ml(Some("Leefdael"), Some("Leefdaal")));
        assert!(PlaceIndex::read_csv("osm_id,place\nx,village\n".as_bytes()).is_err());
        assert!(
            PlaceIndex::read_csv("osm_id,place,name,lat,lon\n1,village,X,NaN,4\n".as_bytes())
                .is_err()
        );
        assert!(PlaceIndex::empty().is_empty());
        assert!(PlaceIndex::load(Path::new("/nonexistent.csv")).is_err());
    }

    #[test]
    fn radius_depends_on_type() {
        let idx = index();
        // ~560 m from the Maredsous hamlet, ~2.5 km from Falaën.
        assert_eq!(idx.nearest(50.2950, 4.7700).unwrap().name, "Maredsous");
        // 2.0 km south of Falaën: inside a village's 3 km.
        assert_eq!(idx.nearest(50.2600, 4.7934).unwrap().name, "Falaën");
        // Maredsous is closest (1.7 km) but outside a hamlet's 1.5 km;
        // Falaën (2.0 km) still covers the point.
        assert_eq!(idx.nearest(50.2962, 4.7934).unwrap().name, "Falaën");
        // ~700 m from the Le Bois locality: covered.
        assert_eq!(idx.nearest(50.3100, 4.7800).unwrap().name, "Le Bois");
        // ~1.2 km from Le Bois (1 km), 1.6 km from the rest: nothing.
        assert!(idx.nearest(50.3100, 4.7870).is_none());
        // Far from everything.
        assert!(idx.nearest(51.0, 3.0).is_none());
    }

    #[test]
    fn nearest_wins_and_known_names_are_skipped() {
        let idx = index();
        // Next to the Falaën node.
        let a = addr(50.2783, 4.7935, "Onhaye", None);
        assert_eq!(idx.place_for(&a).unwrap().name, "Falaën");
        // BeSt already calls the part of municipality "Falaen" (accents
        // and case don't matter): nothing to add.
        let a = addr(50.2783, 4.7935, "Onhaye", Some("FALAEN"));
        assert!(idx.place_for(&a).is_none());
        assert_eq!(
            idx.assign(&a),
            Some(Assignment {
                idx: 0,
                attach: false
            })
        );
        // Lambert coordinates are never matched.
        let mut a = addr(50.2783, 4.7935, "Onhaye", None);
        a.coordinates = Coordinates::Lambert72(crabfinder_core::LambertCoord {
            easting: 0.0,
            northing: 0.0,
        });
        assert!(idx.place_for(&a).is_none());
    }

    #[test]
    fn names_fill_the_region_language() {
        let idx = index();
        let falaen = idx.places().iter().find(|p| p.osm_id == 1).unwrap();
        assert_eq!(falaen.names_for(Region::Wallonia), ml(Some("Falaën"), None));
        assert_eq!(falaen.names_for(Region::Flanders), ml(None, Some("Falaën")));
        // Tagged names already contain the local name: nothing duplicated.
        let leefdaal = idx.places().iter().find(|p| p.osm_id == 7).unwrap();
        assert_eq!(
            leefdaal.names_for(Region::Flanders),
            ml(Some("Leefdael"), Some("Leefdaal"))
        );
    }

    #[test]
    fn kinds_round_trip() {
        for k in [
            PlaceKind::Village,
            PlaceKind::Hamlet,
            PlaceKind::Locality,
            PlaceKind::IsolatedDwelling,
        ] {
            assert_eq!(PlaceKind::parse(k.as_str()), Some(k));
        }
        assert_eq!(PlaceKind::parse("city"), None);
        let mut f = Folder::new();
        assert_eq!(f.fold("Han-sur-Lesse"), "han sur lesse");
        assert_eq!(f.fold("Han-sur-Lesse"), "han sur lesse", "memoised");
    }
}
