//! Localities side-file (schema v8): every named place a query can mean
//! without a street — BeSt municipalities, Walloon parts of municipality,
//! Flemish postal areas and OSM places — with a centre point and the postal
//! codes it covers. Written next to the index as `localities-v1.json`; the
//! service turns it into a name → localities map for locality-only queries.
//!
//! BeSt localities are centred on the mean of their addresses. OSM places
//! keep their node (usually the church or main square), which is a better
//! centre for a village than an average smeared over its hamlets.

use std::collections::{BTreeSet, HashMap};
use std::path::Path;

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

use crabfinder_core::{Address, Coordinates, MultilingualName, Region};

use crate::places::{PlaceIndex, PlaceKind};

pub const LOCALITIES_FILENAME: &str = "localities-v1.json";

/// File format version (independent of the index schema version).
const FORMAT_VERSION: u32 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LocalityKind {
    Municipality,
    PartOfMunicipality,
    PostalArea,
    Village,
    Hamlet,
    Locality,
    IsolatedDwelling,
}

impl LocalityKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Municipality => "municipality",
            Self::PartOfMunicipality => "part_of_municipality",
            Self::PostalArea => "postal_area",
            Self::Village => "village",
            Self::Hamlet => "hamlet",
            Self::Locality => "locality",
            Self::IsolatedDwelling => "isolated_dwelling",
        }
    }

    /// Whether the centre is an OSM node rather than an address mean.
    pub fn is_osm(self) -> bool {
        matches!(
            self,
            Self::Village | Self::Hamlet | Self::Locality | Self::IsolatedDwelling
        )
    }
}

impl From<PlaceKind> for LocalityKind {
    fn from(k: PlaceKind) -> Self {
        match k {
            PlaceKind::Village => Self::Village,
            PlaceKind::Hamlet => Self::Hamlet,
            PlaceKind::Locality => Self::Locality,
            PlaceKind::IsolatedDwelling => Self::IsolatedDwelling,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Locality {
    pub kind: LocalityKind,
    pub name: MultilingualName,
    pub lat: f64,
    pub lon: f64,
    /// Postal codes of the addresses in the locality, sorted. Empty for an
    /// OSM place no address was assigned to.
    pub postal_codes: Vec<String>,
    pub address_count: u32,
    pub region: Option<Region>,
}

#[derive(Serialize, Deserialize)]
struct LocalitiesFile {
    version: u32,
    localities: Vec<Locality>,
}

#[derive(Default)]
struct Acc {
    name: MultilingualName,
    region: Option<Region>,
    sum_lat: f64,
    sum_lon: f64,
    n: u32,
    postal_codes: BTreeSet<String>,
}

impl Acc {
    fn add(&mut self, addr: &Address, lat: f64, lon: f64) {
        self.sum_lat += lat;
        self.sum_lon += lon;
        self.n += 1;
        self.region.get_or_insert(addr.region);
        self.postal_codes.insert(addr.postal_code.clone());
    }
}

/// Accumulates localities while the indexer streams addresses.
#[derive(Default)]
pub struct LocalitiesBuilder {
    best: HashMap<(LocalityKind, String), Acc>,
    /// OSM place index → (count, region, postal codes).
    osm: HashMap<usize, Acc>,
}

impl LocalitiesBuilder {
    pub fn new() -> Self {
        Self::default()
    }

    /// Record one address. `place` is the index (in `PlaceIndex::places`)
    /// of the OSM place covering it, if any.
    pub fn record(&mut self, addr: &Address, place: Option<usize>) {
        let Coordinates::Wgs84(w) = addr.coordinates else {
            return;
        };
        let mut add = |kind, id: String, name: &MultilingualName| {
            if name.fr.is_none() && name.nl.is_none() && name.de.is_none() {
                return;
            }
            let acc = self.best.entry((kind, id)).or_insert_with(|| Acc {
                name: name.clone(),
                ..Acc::default()
            });
            acc.add(addr, w.lat, w.lon);
        };
        add(
            LocalityKind::Municipality,
            addr.municipality_id.to_string(),
            &addr.municipality_name,
        );
        if let (Some(id), Some(name)) = (
            &addr.part_of_municipality_id,
            &addr.part_of_municipality_name,
        ) {
            add(LocalityKind::PartOfMunicipality, id.to_string(), name);
        }
        add(
            LocalityKind::PostalArea,
            addr.postal_info_id.to_string(),
            &addr.postal_name,
        );
        if let Some(i) = place {
            self.osm.entry(i).or_default().add(addr, w.lat, w.lon);
        }
    }

    /// Centroid of the addresses recorded so far in `addr`'s part of
    /// municipality, else its municipality (position imputation,
    /// `impute.rs`).
    pub fn centroid(&self, addr: &Address) -> Option<(f64, f64)> {
        let pom = addr
            .part_of_municipality_id
            .as_ref()
            .map(|id| (LocalityKind::PartOfMunicipality, id.to_string()));
        let muni = Some((LocalityKind::Municipality, addr.municipality_id.to_string()));
        [pom, muni]
            .into_iter()
            .flatten()
            .find_map(|k| self.best.get(&k))
            .filter(|a| a.n > 0)
            .map(|a| (a.sum_lat / f64::from(a.n), a.sum_lon / f64::from(a.n)))
    }

    /// Every BeSt locality seen plus every OSM place, sorted by kind then
    /// name for a stable file.
    pub fn build(self, places: &PlaceIndex) -> Vec<Locality> {
        let mut out: Vec<Locality> = self
            .best
            .into_iter()
            .map(|((kind, _), a)| Locality {
                kind,
                name: a.name,
                lat: a.sum_lat / f64::from(a.n),
                lon: a.sum_lon / f64::from(a.n),
                postal_codes: a.postal_codes.into_iter().collect(),
                address_count: a.n,
                region: a.region,
            })
            .collect();
        let mut osm = self.osm;
        for (i, p) in places.places().iter().enumerate() {
            let acc = osm.remove(&i).unwrap_or_default();
            out.push(Locality {
                kind: p.kind.into(),
                name: match acc.region {
                    Some(r) => p.names_for(r),
                    // Unassigned place: the local name, language unknown.
                    None => MultilingualName {
                        fr: Some(p.name.clone()),
                        ..p.names.clone()
                    },
                },
                lat: p.lat,
                lon: p.lon,
                postal_codes: acc.postal_codes.into_iter().collect(),
                address_count: acc.n,
                region: acc.region,
            });
        }
        out.sort_by(|a, b| {
            (a.kind, display(&a.name), a.postal_codes.first())
                .cmp(&(b.kind, display(&b.name), b.postal_codes.first()))
                .then(a.lat.total_cmp(&b.lat))
        });
        out
    }

    /// Build and write `LOCALITIES_FILENAME` into `index_dir`; returns the
    /// number of localities written.
    pub fn finish(self, places: &PlaceIndex, index_dir: &Path) -> Result<usize> {
        let localities = self.build(places);
        let n = localities.len();
        let path = index_dir.join(LOCALITIES_FILENAME);
        let body = serde_json::to_vec(&LocalitiesFile {
            version: FORMAT_VERSION,
            localities,
        })?;
        std::fs::write(&path, body).with_context(|| format!("writing {path:?}"))?;
        Ok(n)
    }
}

fn display(n: &MultilingualName) -> &str {
    n.fr.as_deref()
        .or(n.nl.as_deref())
        .or(n.de.as_deref())
        .unwrap_or("")
}

/// Read the localities file of `index_dir`. A missing file (index built
/// before v8 support, or by a test helper) is an empty list.
pub fn read_localities(index_dir: &Path) -> Result<Vec<Locality>> {
    let path = index_dir.join(LOCALITIES_FILENAME);
    let body = match std::fs::read(&path) {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e).with_context(|| format!("reading {path:?}")),
    };
    let file: LocalitiesFile =
        serde_json::from_slice(&body).with_context(|| format!("parsing {path:?}"))?;
    if file.version != FORMAT_VERSION {
        bail!(
            "{path:?} has format version {}, expected {FORMAT_VERSION}",
            file.version
        );
    }
    Ok(file.localities)
}

/// Centre of every postal code, address-weighted: the mean of its postal
/// areas; else of the municipalities and parts of municipality it is the
/// only code of; else of every municipality listing it. (A postal area is
/// only recorded when its name differs from its municipality's, so most
/// single-code municipalities have none.)
pub fn postal_centres(localities: &[Locality]) -> HashMap<String, (f64, f64)> {
    let tiers: [&dyn Fn(&Locality) -> bool; 3] = [
        &|l| l.kind == LocalityKind::PostalArea,
        &|l| {
            matches!(
                l.kind,
                LocalityKind::Municipality | LocalityKind::PartOfMunicipality
            ) && l.postal_codes.len() == 1
        },
        &|l| l.kind == LocalityKind::Municipality,
    ];
    let mut out: HashMap<String, (f64, f64)> = HashMap::new();
    for in_tier in tiers {
        let mut acc: HashMap<&str, (f64, f64, f64)> = HashMap::new();
        for l in localities.iter().filter(|l| in_tier(l)) {
            let w = f64::from(l.address_count.max(1));
            for pc in l.postal_codes.iter().filter(|pc| !out.contains_key(*pc)) {
                let e = acc.entry(pc).or_default();
                e.0 += l.lat * w;
                e.1 += l.lon * w;
                e.2 += w;
            }
        }
        for (pc, (lat, lon, w)) in acc {
            out.insert(pc.to_string(), (lat / w, lon / w));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crabfinder_core::{
        AddressId, BestId, BuildingGroupId, MunicipalityId, PartOfMunicipalityId, PostalInfoId,
        Status, StreetId, StreetType, WgsCoord,
    };

    fn ml(fr: &str) -> MultilingualName {
        MultilingualName {
            fr: Some(fr.into()),
            ..Default::default()
        }
    }

    fn addr(
        muni: (&str, &str),
        pom: Option<(&str, &str)>,
        pc: &str,
        lat: f64,
        lon: f64,
    ) -> Address {
        let id = |ns: &str, oid: &str| BestId::new(ns, oid, "1");
        let street_id = StreetId::new(id("s", "1"));
        Address {
            id: AddressId::new(id("a", "1")),
            building_group_id: BuildingGroupId::from_parts(&street_id, "1"),
            street_id,
            municipality_id: MunicipalityId::new(id("m", muni.0)),
            postal_info_id: PostalInfoId::new(id("p", pc)),
            part_of_municipality_id: pom.map(|p| PartOfMunicipalityId::new(id("pom", p.0))),
            region: Region::Wallonia,
            status: Status::Current,
            house_number: "1".into(),
            box_number: None,
            officially_assigned: true,
            street_name: ml("Rue"),
            street_type: StreetType::Streetname,
            municipality_name: ml(muni.1),
            postal_code: pc.into(),
            postal_name: MultilingualName::default(),
            part_of_municipality_name: pom.map(|p| ml(p.1)),
            coordinates: Coordinates::Wgs84(WgsCoord { lat, lon }),
            place_name: None,
            position_source: Default::default(),
        }
    }

    fn places() -> PlaceIndex {
        PlaceIndex::read_csv(
            "osm_id,place,name,name_fr,name_nl,name_de,lat,lon
1,village,Falaën,,,,50.2782,4.7934
2,hamlet,Nowhere,,,,49.6,5.8
"
            .as_bytes(),
        )
        .unwrap()
    }

    #[test]
    fn builds_centroids_postal_codes_and_osm_nodes() {
        let mut b = LocalitiesBuilder::new();
        b.record(
            &addr(
                ("91", "Onhaye"),
                Some(("9101", "Falaën")),
                "5522",
                50.0,
                4.0,
            ),
            Some(0),
        );
        b.record(
            &addr(
                ("91", "Onhaye"),
                Some(("9101", "Falaën")),
                "5522",
                50.2,
                4.2,
            ),
            Some(0),
        );
        b.record(&addr(("91", "Onhaye"), None, "5520", 50.4, 4.4), None);
        let mut lambert = addr(("91", "Onhaye"), None, "5520", 0.0, 0.0);
        lambert.coordinates = Coordinates::Lambert72(crabfinder_core::LambertCoord {
            easting: 0.0,
            northing: 0.0,
        });
        b.record(&lambert, None);
        let ls = b.build(&places());

        let find = |kind| ls.iter().find(|l| l.kind == kind).unwrap();
        let muni = find(LocalityKind::Municipality);
        assert_eq!(muni.name, ml("Onhaye"));
        assert_eq!(muni.address_count, 3);
        assert!((muni.lat - 50.2).abs() < 1e-9 && (muni.lon - 4.2).abs() < 1e-9);
        assert_eq!(muni.postal_codes, ["5520", "5522"]);
        assert_eq!(muni.region, Some(Region::Wallonia));

        let pom = find(LocalityKind::PartOfMunicipality);
        assert_eq!((pom.address_count, pom.lat), (2, 50.1));

        // OSM place keeps its node; addresses only add postal codes.
        let village = find(LocalityKind::Village);
        assert_eq!((village.lat, village.lon), (50.2782, 4.7934));
        assert_eq!(village.postal_codes, ["5522"]);
        assert_eq!(village.address_count, 2);
        assert_eq!(village.name, ml("Falaën"));

        // Unassigned OSM place: still listed, no postal codes.
        let hamlet = find(LocalityKind::Hamlet);
        assert!(hamlet.postal_codes.is_empty());
        assert_eq!(hamlet.region, None);

        // Postal areas without a name (outside Flanders) are skipped.
        assert!(ls.iter().all(|l| l.kind != LocalityKind::PostalArea));
        assert_eq!(ls.len(), 4);
    }

    #[test]
    fn file_round_trip_and_missing_file() {
        let dir = tempfile::tempdir().unwrap();
        assert!(read_localities(dir.path()).unwrap().is_empty());

        let mut b = LocalitiesBuilder::new();
        b.record(&addr(("91", "Onhaye"), None, "5520", 50.4, 4.4), None);
        let n = b.finish(&places(), dir.path()).unwrap();
        assert_eq!(n, 3);
        let back = read_localities(dir.path()).unwrap();
        assert_eq!(back.len(), 3);
        assert_eq!(back[0].kind, LocalityKind::Municipality);

        std::fs::write(
            dir.path().join(LOCALITIES_FILENAME),
            r#"{"version": 99, "localities": []}"#,
        )
        .unwrap();
        assert!(read_localities(dir.path()).is_err());
        std::fs::write(dir.path().join(LOCALITIES_FILENAME), "nope").unwrap();
        assert!(read_localities(dir.path()).is_err());
    }

    #[test]
    fn kind_strings() {
        assert_eq!(LocalityKind::PostalArea.as_str(), "postal_area");
        assert!(LocalityKind::Hamlet.is_osm());
        assert!(!LocalityKind::Municipality.is_osm());
        assert_eq!(
            LocalityKind::from(PlaceKind::IsolatedDwelling).as_str(),
            "isolated_dwelling"
        );
        for k in [
            LocalityKind::Municipality,
            LocalityKind::PartOfMunicipality,
            LocalityKind::Village,
            LocalityKind::Locality,
        ] {
            assert!(!k.as_str().is_empty());
        }
    }

    fn loc(kind: LocalityKind, codes: &[&str], lat: f64, lon: f64, n: u32) -> Locality {
        Locality {
            kind,
            name: ml("x"),
            lat,
            lon,
            postal_codes: codes.iter().map(|c| c.to_string()).collect(),
            address_count: n,
            region: None,
        }
    }

    #[test]
    fn postal_centres_prefer_postal_areas_then_single_code_localities() {
        use LocalityKind::*;
        let ls = vec![
            // 2000: a postal area wins over the multi-code municipality.
            loc(PostalArea, &["2000"], 51.22, 4.40, 10),
            loc(Municipality, &["2000", "2018", "2600"], 51.20, 4.42, 100),
            // 2018: two single-code parts, address-weighted.
            loc(PartOfMunicipality, &["2018"], 51.0, 4.0, 1),
            loc(PartOfMunicipality, &["2018"], 51.3, 4.3, 2),
            // 1160: its municipality's only code.
            loc(Municipality, &["1160"], 50.82, 4.42, 22_000),
            // OSM places never count.
            loc(Village, &["1160"], 49.0, 3.0, 5),
        ];
        let c = postal_centres(&ls);
        let near = |pc: &str, (lat, lon): (f64, f64)| {
            let (a, b) = c[pc];
            assert!(
                (a - lat).abs() < 1e-9 && (b - lon).abs() < 1e-9,
                "{pc}: {a},{b}"
            );
        };
        near("2000", (51.22, 4.40));
        near("2018", (51.2, 4.2));
        near("1160", (50.82, 4.42));
        // 2600: only the multi-code municipality lists it.
        near("2600", (51.20, 4.42));
        assert_eq!(c.len(), 4);
    }
}
