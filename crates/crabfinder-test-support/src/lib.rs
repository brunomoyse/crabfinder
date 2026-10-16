//! Shared test fixtures for the crabfinder workspace.
//!
//! Dev-dependency only (`publish = false`). Depends on `core` + `indexer`
//! but deliberately *not* on `service`: a dev-dependency cycle would compile
//! two copies of the service types for its unit tests. Service-level state
//! is built from a `TestIndex` directory through the service's own
//! `AppState::load`, so tests exercise the production startup path.

use std::path::Path;

use crabfinder_core::{
    Address, AddressId, BestId, BuildingGroupId, Coordinates, MultilingualName, MunicipalityId,
    PartOfMunicipalityId, PositionSource, PostalInfoId, Region, Status, StreetId, StreetType,
    WgsCoord,
};
use crabfinder_indexer::index::{CrabIndexWriter, CrabSchema, register_tokenizers};
use crabfinder_indexer::localities::LocalitiesBuilder;
use crabfinder_indexer::meta::write_sidecar;
use crabfinder_indexer::mid_street::{MID_STREET_FILENAME, MidStreetBuilder};
use crabfinder_indexer::places::PlaceIndex;
use tantivy::{Index, IndexReader, ReloadPolicy};
use tempfile::TempDir;

pub mod best_bundle;

// --- Address builder ---------------------------------------------------------

/// Per-region BeSt namespace roots, matching what the real dump publishes
/// (see `Region::from_namespace`).
fn namespaces(region: Region) -> [&'static str; 5] {
    match region {
        Region::Brussels => [
            "https://databrussels.be/id/address",
            "https://databrussels.be/id/streetname",
            "https://databrussels.be/id/municipality",
            "https://databrussels.be/id/postalinfo",
            "https://databrussels.be/id/partofmunicipality",
        ],
        Region::Flanders => [
            "https://data.vlaanderen.be/id/adres/",
            "https://data.vlaanderen.be/id/straatnaam/",
            "https://data.vlaanderen.be/id/gemeente/",
            "https://data.vlaanderen.be/id/postinfo/",
            "https://data.vlaanderen.be/id/deelgemeente/",
        ],
        Region::Wallonia => [
            "geodata.wallonie.be/id/Address",
            "geodata.wallonie.be/id/Streetname",
            "geodata.wallonie.be/id/Municipality",
            "geodata.wallonie.be/id/PostalInfo",
            "geodata.wallonie.be/id/PartOfMunicipality",
        ],
    }
}

fn name(fr: Option<&str>, nl: Option<&str>, de: Option<&str>) -> MultilingualName {
    MultilingualName {
        fr: fr.map(Into::into),
        nl: nl.map(Into::into),
        de: de.map(Into::into),
    }
}

/// Fluent builder for `Address` fixtures. Defaults: Brussels region,
/// municipality 21001 "Bruxelles/Brussel", postal 1000, `Status::Current`.
#[derive(Debug, Clone)]
pub struct AddressBuilder {
    addr_oid: String,
    street_oid: String,
    house: String,
    box_no: Option<String>,
    region: Region,
    street: MultilingualName,
    municipality_oid: String,
    municipality: MultilingualName,
    postal: String,
    postal_name: MultilingualName,
    part_of_municipality: Option<(String, MultilingualName)>,
    status: Status,
    lat: f64,
    lon: f64,
    position_source: PositionSource,
}

impl AddressBuilder {
    pub fn new(addr_oid: &str, street_oid: &str, house: &str) -> Self {
        Self {
            addr_oid: addr_oid.into(),
            street_oid: street_oid.into(),
            house: house.into(),
            box_no: None,
            region: Region::Brussels,
            street: MultilingualName::default(),
            municipality_oid: "21001".into(),
            municipality: name(Some("Bruxelles"), Some("Brussel"), None),
            postal: "1000".into(),
            postal_name: MultilingualName::default(),
            part_of_municipality: None,
            status: Status::Current,
            lat: 50.8466,
            lon: 4.3525,
            position_source: PositionSource::Measured,
        }
    }

    pub fn street(mut self, fr: Option<&str>, nl: Option<&str>) -> Self {
        self.street = name(fr, nl, self.street.de.as_deref());
        self
    }

    pub fn street_de(mut self, de: &str) -> Self {
        self.street.de = Some(de.into());
        self
    }

    pub fn box_no(mut self, b: &str) -> Self {
        self.box_no = Some(b.into());
        self
    }

    pub fn region(mut self, r: Region) -> Self {
        self.region = r;
        self
    }

    pub fn municipality(
        mut self,
        oid: &str,
        fr: Option<&str>,
        nl: Option<&str>,
        de: Option<&str>,
    ) -> Self {
        self.municipality_oid = oid.into();
        self.municipality = name(fr, nl, de);
        self
    }

    pub fn postal(mut self, pc: &str) -> Self {
        self.postal = pc.into();
        self
    }

    pub fn postal_name(mut self, fr: Option<&str>, nl: Option<&str>) -> Self {
        self.postal_name = name(fr, nl, None);
        self
    }

    pub fn part_of_municipality(mut self, oid: &str, fr: Option<&str>, nl: Option<&str>) -> Self {
        self.part_of_municipality = Some((oid.into(), name(fr, nl, None)));
        self
    }

    pub fn status(mut self, s: Status) -> Self {
        self.status = s;
        self
    }

    /// Mark the position as imputed (schema v9).
    pub fn position_source(mut self, p: PositionSource) -> Self {
        self.position_source = p;
        self
    }

    pub fn at(mut self, lat: f64, lon: f64) -> Self {
        self.lat = lat;
        self.lon = lon;
        self
    }

    pub fn build(self) -> Address {
        let [ns_addr, ns_street, ns_muni, ns_postal, ns_pom] = namespaces(self.region);
        let street_id = StreetId::new(BestId::new(ns_street, self.street_oid.as_str(), "1"));
        let building_group_id = BuildingGroupId::from_parts(&street_id, &self.house);
        let (pom_id, pom_name) = match self.part_of_municipality {
            Some((oid, n)) => (
                Some(PartOfMunicipalityId::new(BestId::new(ns_pom, oid, "1"))),
                Some(n),
            ),
            None => (None, None),
        };
        Address {
            id: AddressId::new(BestId::new(ns_addr, self.addr_oid, "1")),
            street_id,
            municipality_id: MunicipalityId::new(BestId::new(ns_muni, self.municipality_oid, "1")),
            postal_info_id: PostalInfoId::new(BestId::new(ns_postal, self.postal.as_str(), "1")),
            part_of_municipality_id: pom_id,
            region: self.region,
            status: self.status,
            house_number: self.house,
            box_number: self.box_no,
            officially_assigned: true,
            street_name: self.street,
            street_type: StreetType::Streetname,
            municipality_name: self.municipality,
            postal_code: self.postal,
            postal_name: self.postal_name,
            part_of_municipality_name: pom_name,
            coordinates: Coordinates::Wgs84(WgsCoord {
                lat: self.lat,
                lon: self.lon,
            }),
            building_group_id,
            position_source: self.position_source,
        }
    }
}

// --- Fixture sets -------------------------------------------------------------

pub mod fixtures {
    use super::*;

    /// Street object id of Avenue Louise in every fixture set.
    pub const LOUISE_OID: &str = "3919";

    fn louise(addr_oid: &str, house: &str) -> AddressBuilder {
        AddressBuilder::new(addr_oid, LOUISE_OID, house)
            .street(Some("Avenue Louise"), Some("Louizalaan"))
            .postal("1050")
    }

    /// Avenue Louise 10/11/13/20/32/40/45/50 in 1050, one unit each, plus
    /// "Rue Paire" 2/4/6 (even numbers only). Distinct latitudes so tests
    /// can tell which building came back. Used by resolver + nearest-number
    /// tests.
    pub fn louise_street() -> Vec<Address> {
        let mut out: Vec<Address> = [10u32, 11, 13, 20, 32, 40, 45, 50]
            .iter()
            .map(|n| {
                louise(&format!("a{n}"), &n.to_string())
                    .at(50.8345 + f64::from(*n) * 0.0001, 4.3567)
                    .build()
            })
            .collect();
        for n in [2u32, 4, 6] {
            out.push(
                AddressBuilder::new(&format!("p{n}"), "777", &n.to_string())
                    .street(Some("Rue Paire"), Some("Evenstraat"))
                    .postal("1050")
                    .at(50.8200 + f64::from(n) * 0.0001, 4.3600)
                    .build(),
            );
        }
        out
    }

    /// The pre-v2 `/geocode` handler fixture set: Louise 32 (5 boxes), 32A,
    /// 34; Grote-Ringlaan; Grand-Place 1 + 5; Rue de la Loi 16 + 200; a
    /// "Loi-Belliard" distractor; Place Saint-Étienne.
    pub fn brussels_small() -> Vec<Address> {
        let mut out = vec![];
        for (i, b) in ["1", "2", "3", "4", "5"].iter().enumerate() {
            out.push(
                louise(&format!("10{i}"), "32")
                    .box_no(b)
                    .at(50.8345, 4.3567)
                    .build(),
            );
        }
        out.push(louise("120", "32A").box_no("1").at(50.8345, 4.3568).build());
        out.push(louise("121", "34").box_no("1").at(50.8346, 4.3569).build());
        out.push(
            AddressBuilder::new("200", "4962", "1")
                .street(
                    Some("Boulevard de la Grande Ceinture"),
                    Some("Grote-Ringlaan"),
                )
                .box_no("0001")
                .postal("1070")
                .at(50.8466, 4.2981)
                .build(),
        );
        for (oid, hn, lat, lon) in [("300", "1", 50.8466, 4.3525), ("301", "5", 50.8467, 4.3526)] {
            out.push(
                AddressBuilder::new(oid, "7", hn)
                    .street(Some("Grand-Place"), Some("Grote Markt"))
                    .at(lat, lon)
                    .build(),
            );
        }
        for (oid, hn, lat, lon) in [
            ("400", "16", 50.8453, 4.3658),
            ("401", "200", 50.8457, 4.3712),
        ] {
            out.push(
                AddressBuilder::new(oid, "100", hn)
                    .street(Some("Rue de la Loi"), Some("Wetstraat"))
                    .at(lat, lon)
                    .build(),
            );
        }
        out.push(
            AddressBuilder::new("500", "101", "1")
                .street(Some("Rue Loi-Belliard"), Some("Loi-Belliardstraat"))
                .postal("1040")
                .at(50.8400, 4.3700)
                .build(),
        );
        out.push(
            AddressBuilder::new("600", "102", "1")
                .street(Some("Place Saint-Étienne"), Some("Sint-Étiennestraat"))
                .postal("1040")
                .at(50.8350, 4.3800)
                .build(),
        );
        out
    }

    /// The batch e2e fixture set: Louise 10/20/30 in 1050 + Grand-Place 1.
    pub fn batch_small() -> Vec<Address> {
        let mut out: Vec<Address> = [
            ("100", "10", 50.8300, 4.3500),
            ("101", "20", 50.8310, 4.3510),
            ("102", "30", 50.8320, 4.3520),
        ]
        .iter()
        .map(|(oid, hn, lat, lon)| louise(oid, hn).at(*lat, *lon).build())
        .collect();
        out.push(
            AddressBuilder::new("200", "7", "1")
                .street(Some("Grand-Place"), Some("Grote Markt"))
                .at(50.8467, 4.3524)
                .build(),
        );
        out
    }

    /// Cross-region set: a Flemish street, a Walloon street with a part of
    /// municipality, a German-speaking municipality, a street with a number
    /// in its name, and a 4-digit house number.
    pub fn multi_region() -> Vec<Address> {
        vec![
            AddressBuilder::new("f1", "5001", "1")
                .region(Region::Flanders)
                .street(None, Some("Grote Markt"))
                .municipality("11002", Some("Anvers"), Some("Antwerpen"), None)
                .postal("2000")
                .at(51.2213, 4.3997)
                .build(),
            AddressBuilder::new("w1", "6001", "3")
                .region(Region::Wallonia)
                .street(Some("Rue du Village"), None)
                .municipality("91114", Some("Rochefort"), None, None)
                .part_of_municipality("9111401", Some("Han-sur-Lesse"), None)
                .postal("5580")
                .at(50.1260, 5.1880)
                .build(),
            AddressBuilder::new("d1", "6002", "7")
                .region(Region::Wallonia)
                .street(None, None)
                .street_de("Hauptstraße")
                .municipality("63023", Some("Eupen"), None, Some("Eupen"))
                .postal("4700")
                .at(50.6280, 6.0350)
                .build(),
            AddressBuilder::new("n1", "200", "5")
                .street(Some("Rue du 11 Novembre"), Some("11 Novemberstraat"))
                .municipality("21009", Some("Ixelles"), Some("Elsene"), None)
                .postal("1050")
                .at(50.8250, 4.3800)
                .build(),
            AddressBuilder::new("h1", "300", "1357")
                .street(Some("Chaussée de Waterloo"), Some("Waterloosesteenweg"))
                .municipality("21016", Some("Uccle"), Some("Ukkel"), None)
                .postal("1180")
                .at(50.7900, 4.3700)
                .build(),
        ]
    }
}

// --- On-disk test index --------------------------------------------------------

/// A tiny on-disk Tantivy index with a current sidecar. The `TempDir` lives
/// as long as the struct, so the index is cleaned up when the test ends.
pub struct TestIndex {
    pub dir: TempDir,
    pub index: Index,
    pub reader: IndexReader,
    pub schema: CrabSchema,
}

impl TestIndex {
    /// Index `addrs` (notability 1.0 everywhere) without a mid-street file.
    pub fn build(addrs: &[Address]) -> Self {
        Self::build_inner(addrs, false)
    }

    /// Same as `build`, plus the schema-v7 mid-street side-file written by
    /// the indexer's own `MidStreetBuilder`.
    pub fn build_with_mid_street(addrs: &[Address]) -> Self {
        Self::build_inner(addrs, true)
    }

    /// `build_with_mid_street` plus the schema-v8 localities file, from
    /// the addresses and an OSM places CSV in the
    /// `data/osm-places-be-v1.csv` format (header line included).
    pub fn build_with_places(addrs: &[Address], places_csv: &str) -> Self {
        let t = Self::build_inner(addrs, true);
        let places = PlaceIndex::read_csv(places_csv.as_bytes()).expect("places csv");
        let mut loc = LocalitiesBuilder::new();
        for a in addrs {
            loc.record(a, places.assign(a).map(|x| x.idx));
        }
        loc.finish(&places, t.path()).expect("localities file");
        t
    }

    fn build_inner(addrs: &[Address], mid_street: bool) -> Self {
        let dir = tempfile::tempdir().expect("tempdir");
        write_index(dir.path(), addrs, mid_street);
        let index = Index::open_in_dir(dir.path()).expect("open test index");
        register_tokenizers(&index);
        let reader = index
            .reader_builder()
            .reload_policy(ReloadPolicy::Manual)
            .try_into()
            .expect("test index reader");
        Self {
            dir,
            index,
            reader,
            schema: CrabSchema::build(),
        }
    }

    pub fn path(&self) -> &Path {
        self.dir.path()
    }
}

/// Write `addrs` as a committed index + sidecar (+ optional mid-street
/// file) into `dir`. Exposed for tests that need an index at a path they
/// control (e.g. CLI tests).
pub fn write_index(dir: &Path, addrs: &[Address], mid_street: bool) {
    let mut writer = CrabIndexWriter::create(dir).expect("create index");
    let mut mid = MidStreetBuilder::new();
    for a in addrs {
        writer.add_address(a).expect("add address");
        mid.record(a);
    }
    writer.commit().expect("commit");
    write_sidecar(dir).expect("sidecar");
    if mid_street {
        mid.finish(&dir.join(MID_STREET_FILENAME))
            .expect("mid-street file");
    }
}

/// Write a mid-street side-file at `path` computed from `addrs` with the
/// indexer's own builder. Lets a test pin the median coordinates of a
/// street independently of the indexed buildings.
pub fn write_mid_street(path: &Path, addrs: &[Address]) {
    let mut mid = MidStreetBuilder::new();
    for a in addrs {
        mid.record(a);
    }
    mid.finish(path).expect("mid-street file");
}

// --- CSV helpers ----------------------------------------------------------------

/// Parse a CSV body into `(header, rows)`.
pub fn parse_csv(body: &str) -> (Vec<String>, Vec<Vec<String>>) {
    let mut rdr = csv::ReaderBuilder::new()
        .has_headers(true)
        .from_reader(body.as_bytes());
    let header = rdr
        .headers()
        .expect("csv header")
        .iter()
        .map(String::from)
        .collect();
    let rows = rdr
        .records()
        .map(|r| r.expect("csv row").iter().map(String::from).collect())
        .collect();
    (header, rows)
}

/// Value of column `name` in `row`, given the parsed `header`.
pub fn col<'a>(header: &[String], row: &'a [String], name: &str) -> &'a str {
    let i = header
        .iter()
        .position(|h| h == name)
        .unwrap_or_else(|| panic!("column {name:?} missing from {header:?}"));
    &row[i]
}
