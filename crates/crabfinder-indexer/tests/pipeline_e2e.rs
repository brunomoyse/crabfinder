//! End-to-end indexer tests over generated BeSt bundles: XML readers, zip
//! discovery, Pass 1 / Pass 2 and the full `cli::run`.

use std::path::Path;

use crabfinder_core::{Coordinates, Region, Status};
use crabfinder_indexer::bundle::{Entity, Region as BundleRegion, detect_regions, find_zip};
use crabfinder_indexer::cli::{self, Args, RegionArg};
use crabfinder_indexer::index::{CrabSchema, register_tokenizers};
use crabfinder_indexer::meta::{SCHEMA_VERSION, check_sidecar};
use crabfinder_indexer::mid_street::MID_STREET_FILENAME;
use crabfinder_indexer::pipeline::{load_dictionaries, stream_resolved_addresses};
use crabfinder_indexer::xml::{
    read_municipalities, read_parts_of_municipality, read_postal_infos, read_streets,
    stream_addresses,
};
use crabfinder_indexer::zip_read::with_inner_xml;
use crabfinder_test_support::best_bundle::{AddrSpec, BestBundle, StreetSpec, write_zip};
use tantivy::collector::Count;
use tantivy::query::{AllQuery, TermQuery};
use tantivy::schema::IndexRecordOption;
use tantivy::{Index, Term};

fn bundle_dir(bundles: &[BestBundle]) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    for b in bundles {
        b.write(dir.path());
    }
    dir
}

fn open(path: &Path) -> (Index, CrabSchema) {
    let index = Index::open_in_dir(path).unwrap();
    register_tokenizers(&index);
    (index, CrabSchema::build())
}

fn count_term(index: &Index, field: tantivy::schema::Field, value: &str) -> usize {
    let searcher = index.reader().unwrap().searcher();
    let q = TermQuery::new(
        Term::from_field_text(field, value),
        IndexRecordOption::Basic,
    );
    searcher.search(&q, &Count).unwrap()
}

// --- XML readers -------------------------------------------------------------

#[test]
fn readers_parse_generated_xml() {
    let b = BestBundle::brussels_sample();
    let munis = read_municipalities(b.municipality_xml().as_bytes()).unwrap();
    assert_eq!(munis.len(), 2);
    assert_eq!(munis[1].names.fr.as_deref(), Some("Ixelles"));
    assert_eq!(munis[1].names.nl.as_deref(), Some("Elsene"));

    let postals = read_postal_infos(b.postal_xml().as_bytes()).unwrap();
    assert_eq!(postals[1].id.0.object_identifier, "1050");

    let streets = read_streets(b.street_xml().as_bytes()).unwrap();
    assert_eq!(streets.len(), 3);
    assert_eq!(streets[1].names.fr.as_deref(), Some("Avenue Louise"));
    assert_eq!(streets[1].status, Status::Current);
    assert_eq!(
        streets[1].assigned_by_municipality.0.object_identifier,
        "21009"
    );

    let w = BestBundle::wallonia_sample();
    let poms = read_parts_of_municipality(w.pom_xml().as_bytes()).unwrap();
    assert_eq!(poms[0].names.fr.as_deref(), Some("Han-sur-Lesse"));

    let mut raws = Vec::new();
    let n = stream_addresses(b.address_xml().as_bytes(), |a| {
        raws.push(a);
        Ok(true)
    })
    .unwrap();
    assert_eq!(n, 6);
    let boxed = raws
        .iter()
        .find(|a| a.id.object_identifier == "1003")
        .unwrap();
    assert_eq!(boxed.box_number.as_deref(), Some("2"));
    let retired = raws
        .iter()
        .find(|a| a.id.object_identifier == "1004")
        .unwrap();
    assert_eq!(retired.status, Status::Retired);
    let dangling = raws
        .iter()
        .find(|a| a.id.object_identifier == "1005")
        .unwrap();
    assert!(!dangling.officially_assigned);
    assert!((boxed.easting - 148_950.0).abs() < 1e-9);
}

#[test]
fn stream_addresses_stops_when_handler_says_so() {
    let b = BestBundle::brussels_sample();
    let mut seen = 0;
    stream_addresses(b.address_xml().as_bytes(), |_| {
        seen += 1;
        Ok(seen < 2)
    })
    .unwrap();
    assert_eq!(seen, 2);
}

#[test]
fn readers_reject_incomplete_records() {
    // <com:code> without an objectIdentifier.
    let xml = "<tns:municipality><com:code><com:namespace>ns</com:namespace>\
               </com:code></tns:municipality>";
    assert!(read_municipalities(xml.as_bytes()).is_err());
    // Street without a status.
    let xml = "<tns:streetName><com:code><com:namespace>ns</com:namespace>\
               <com:objectIdentifier>1</com:objectIdentifier></com:code></tns:streetName>";
    let err = read_streets(xml.as_bytes()).unwrap_err();
    assert!(err.to_string().contains("missing <com:status>"));
    // Address without a position.
    let xml = "<tns:address><com:code><com:namespace>ns</com:namespace>\
               <com:objectIdentifier>1</com:objectIdentifier></com:code></tns:address>";
    let err = stream_addresses(xml.as_bytes(), |_| Ok(true)).unwrap_err();
    assert!(err.to_string().contains("missing pos x"));
}

// --- Bundle discovery -----------------------------------------------------------

#[test]
fn find_zip_picks_latest_and_detects_regions() {
    let dir = bundle_dir(&[BestBundle::brussels_sample()]);
    // An older dump of the same entity must lose to the newer one.
    write_zip(
        &dir.path().join("BrusselsAddress20200101.zip"),
        "old.xml",
        "<x/>",
    );
    let p = find_zip(dir.path(), BundleRegion::Brussels, Entity::Address).unwrap();
    assert!(p.ends_with("BrusselsAddress20260101.zip"));
    assert_eq!(
        detect_regions(dir.path()).unwrap(),
        [BundleRegion::Brussels]
    );
    let err = find_zip(dir.path(), BundleRegion::Flanders, Entity::Address).unwrap_err();
    assert!(err.to_string().contains("FlandersAddress"));
}

#[test]
fn detect_regions_errors_on_empty_dir() {
    let dir = tempfile::tempdir().unwrap();
    assert!(detect_regions(dir.path()).is_err());
    assert!(detect_regions(&dir.path().join("missing")).is_err());
}

#[test]
fn zip_without_xml_entry_is_an_error() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("x.zip");
    write_zip(&p, "readme.txt", "hi");
    let err = with_inner_xml(&p, |_| Ok(())).unwrap_err();
    assert!(err.to_string().contains("no .xml entry"));
    assert!(with_inner_xml(&dir.path().join("nope.zip"), |_| Ok(())).is_err());
}

// --- Pass 1 + Pass 2 -------------------------------------------------------------

#[test]
fn pipeline_resolves_filters_and_reprojects() {
    let dir = bundle_dir(&[BestBundle::brussels_sample()]);
    let dicts = load_dictionaries(dir.path(), BundleRegion::Brussels).unwrap();
    assert!(
        dicts.summary().contains("municipalities=2"),
        "{}",
        dicts.summary()
    );
    let mut out = Vec::new();
    let report = stream_resolved_addresses(dir.path(), BundleRegion::Brussels, &dicts, None, |a| {
        out.push(a);
        Ok(true)
    })
    .unwrap();
    assert_eq!(report.parsed, 6);
    assert_eq!(report.skipped_status, 1, "retired is dropped");
    assert_eq!(report.skipped_unresolved, 1, "street 9999 is unknown");
    assert_eq!(report.emitted, 4);
    let louise = out.iter().find(|a| a.house_number == "10").unwrap();
    assert_eq!(louise.street_name.fr.as_deref(), Some("Avenue Louise"));
    assert_eq!(louise.municipality_name.nl.as_deref(), Some("Elsene"));
    assert_eq!(louise.postal_code, "1050");
    assert_eq!(louise.region, Region::Brussels);
    match louise.coordinates {
        Coordinates::Wgs84(w) => assert!(w.is_in_belgium(), "{w:?}"),
        Coordinates::Lambert72(_) => panic!("not reprojected"),
    }
    assert!(report.to_string().starts_with("parsed=6 emitted=4"));
}

#[test]
fn pipeline_limit_and_early_stop() {
    let dir = bundle_dir(&[BestBundle::brussels_sample()]);
    let dicts = load_dictionaries(dir.path(), BundleRegion::Brussels).unwrap();
    let r = stream_resolved_addresses(dir.path(), BundleRegion::Brussels, &dicts, Some(2), |_| {
        Ok(true)
    })
    .unwrap();
    assert_eq!(r.emitted, 2);
    let r = stream_resolved_addresses(dir.path(), BundleRegion::Brussels, &dicts, None, |_| {
        Ok(false)
    })
    .unwrap();
    assert_eq!(r.emitted, 1);
}

#[test]
fn wallonia_requires_and_uses_part_of_municipality() {
    let dir = bundle_dir(&[BestBundle::wallonia_sample()]);
    let dicts = load_dictionaries(dir.path(), BundleRegion::Wallonia).unwrap();
    let mut out = Vec::new();
    stream_resolved_addresses(dir.path(), BundleRegion::Wallonia, &dicts, None, |a| {
        out.push(a);
        Ok(true)
    })
    .unwrap();
    assert_eq!(
        out[0]
            .part_of_municipality_name
            .as_ref()
            .unwrap()
            .fr
            .as_deref(),
        Some("Han-sur-Lesse")
    );
    // Without the PartOfMunicipality zip, Pass 1 fails for Wallonia.
    std::fs::remove_file(dir.path().join("WalloniaPartOfMunicipality20260101.zip")).unwrap();
    assert!(load_dictionaries(dir.path(), BundleRegion::Wallonia).is_err());
}

// --- Full CLI run ------------------------------------------------------------------

#[test]
fn cli_run_builds_a_complete_index() {
    let best = bundle_dir(&[BestBundle::brussels_sample(), BestBundle::wallonia_sample()]);
    let out = tempfile::tempdir().unwrap();
    let whitelist = best.path().join("whitelist.yaml");
    std::fs::write(
        &whitelist,
        "version: v1\nentries:\n- notability_id: gp\n  canonical_name: GP\n  \
         target_building_group_ids: ['https://databrussels.be/id/streetname|7|1']\n  \
         boost_multiplier: 3.0\n",
    )
    .unwrap();
    let mut args = Args::new(best.path(), out.path().join("idx"));
    args.notability_whitelist = Some(whitelist);
    args.print_samples = 1;
    let report = cli::run(&args).unwrap();

    assert_eq!(report.regions.len(), 2);
    assert_eq!(report.total().emitted, 5);
    assert_eq!(report.regions[0].notability_stamped, 1);
    assert_eq!(report.whitelist_entries, 1);
    // Brussels skipped 1/6 = 16.7 % for status, far above its 0.3 % baseline.
    assert!(report.regions[0].status_drift_pct.is_some());
    assert!(report.regions[1].status_drift_pct.is_none());
    assert!(report.mid_street_entries >= 3);
    let summary = report.summary();
    assert!(summary.contains("TOTAL: parsed=7 emitted=5"), "{summary}");
    assert!(summary.contains("index size on disk"));

    let idx = out.path().join("idx");
    assert_eq!(check_sidecar(&idx).unwrap().schema_version, SCHEMA_VERSION);
    assert!(idx.join(MID_STREET_FILENAME).exists());
    let (index, schema) = open(&idx);
    let searcher = index.reader().unwrap().searcher();
    assert_eq!(searcher.search(&AllQuery, &Count).unwrap(), 5);
    assert_eq!(count_term(&index, schema.region, "BE-WAL"), 1);
    assert_eq!(count_term(&index, schema.box_number, "2"), 1);
}

#[test]
fn cli_run_single_region_and_limit() {
    let best = bundle_dir(&[BestBundle::brussels_sample(), BestBundle::wallonia_sample()]);
    let out = tempfile::tempdir().unwrap();
    let mut args = Args::new(best.path(), out.path());
    args.region = RegionArg::Wallonia;
    let report = cli::run(&args).unwrap();
    assert_eq!(report.regions.len(), 1);

    for (region, expected) in [(RegionArg::Brussels, 2), (RegionArg::Flanders, 0)] {
        let out = tempfile::tempdir().unwrap();
        let mut args = Args::new(best.path(), out.path());
        args.region = region;
        args.limit = Some(2);
        match cli::run(&args) {
            Ok(r) => assert_eq!(r.total().emitted, expected),
            // No Flanders zips in the bundle.
            Err(e) => assert!(expected == 0 && format!("{e:#}").contains("Flanders")),
        }
    }
}

#[test]
fn cli_run_rejects_bad_inputs() {
    let best = bundle_dir(&[BestBundle::brussels_sample()]);
    let err = cli::run(&Args::new(best.path(), "")).unwrap_err();
    assert!(err.to_string().contains("--output is empty"));

    let mut args = Args::new(best.path(), best.path().join("idx"));
    args.notability_whitelist = Some(best.path().join("missing.yaml"));
    assert!(format!("{:#}", cli::run(&args).unwrap_err()).contains("notability"));

    let empty = tempfile::tempdir().unwrap();
    assert!(cli::run(&Args::new(empty.path(), empty.path().join("idx"))).is_err());
}

#[test]
fn cli_run_attaches_osm_places_and_writes_localities() {
    use crabfinder_indexer::localities::{LOCALITIES_FILENAME, LocalityKind, read_localities};

    let best = bundle_dir(&[BestBundle::wallonia_sample()]);
    // Where the Walloon address lands after reprojection.
    let region = detect_regions(best.path()).unwrap()[0];
    let dicts = load_dictionaries(best.path(), region).unwrap();
    let mut at = (0.0, 0.0);
    stream_resolved_addresses(best.path(), region, &dicts, None, |a| {
        if let Coordinates::Wgs84(w) = a.coordinates {
            at = (w.lat, w.lon);
        }
        Ok(true)
    })
    .unwrap();

    // A hamlet on top of it, and a village repeating the BeSt part of
    // municipality name (indexed nowhere, but listed as a locality).
    let csv = best.path().join("places.csv");
    std::fs::write(
        &csv,
        format!(
            "osm_id,place,name,name_fr,name_nl,name_de,lat,lon\n\
             1,hamlet,Maupas,,,,{},{}\n\
             2,village,Han-sur-Lesse,,,,{},{}\n",
            at.0 + 0.0005,
            at.1,
            at.0 + 0.01,
            at.1
        ),
    )
    .unwrap();
    let out = tempfile::tempdir().unwrap();
    let mut args = Args::new(best.path(), out.path().join("idx"));
    args.places = Some(csv.clone());
    let report = cli::run(&args).unwrap();
    assert_eq!(report.places_loaded, 2);
    assert_eq!(report.regions[0].places_attached, 1);
    assert!(report.summary().contains("localities: "));

    let idx = out.path().join("idx");
    let (index, schema) = open(&idx);
    // Attached to the payload, kept out of the searchable text.
    assert_eq!(count_term(&index, schema.searchable, "maupas"), 0);
    let searcher = index.reader().unwrap().searcher();
    let (_, addr) = searcher
        .search(
            &AllQuery,
            &tantivy::collector::TopDocs::with_limit(1).order_by_score(),
        )
        .unwrap()[0];
    let doc: tantivy::TantivyDocument = searcher.doc(addr).unwrap();
    let payload = tantivy::schema::Value::as_str(&doc.get_first(schema.payload).unwrap())
        .unwrap()
        .to_string();
    let a: crabfinder_core::Address = serde_json::from_str(&payload).unwrap();
    assert_eq!(a.place_name.unwrap().fr.as_deref(), Some("Maupas"));
    assert!(idx.join(LOCALITIES_FILENAME).exists());
    let ls = read_localities(&idx).unwrap();
    assert_eq!(report.localities_entries, ls.len());
    let kinds: Vec<LocalityKind> = ls.iter().map(|l| l.kind).collect();
    for k in [
        LocalityKind::Municipality,
        LocalityKind::PartOfMunicipality,
        LocalityKind::Hamlet,
        LocalityKind::Village,
    ] {
        assert!(kinds.contains(&k), "{k:?} missing from {kinds:?}");
    }
    let hamlet = ls.iter().find(|l| l.kind == LocalityKind::Hamlet).unwrap();
    assert_eq!(hamlet.postal_codes, ["5580"]);
    assert_eq!(hamlet.name.fr.as_deref(), Some("Maupas"));

    // A broken places file is reported.
    std::fs::write(&csv, "osm_id,place,name,lat,lon\nx,y,z,1,2\n").unwrap();
    assert!(format!("{:#}", cli::run(&args).unwrap_err()).contains("places"));
}

#[test]
fn house_numbers_are_indexed_uppercase() {
    use crabfinder_indexer::index::CrabIndexWriter;
    use crabfinder_test_support::AddressBuilder;

    let dir = tempfile::tempdir().unwrap();
    let mut w = CrabIndexWriter::create(dir.path()).unwrap();
    w.add_address(&AddressBuilder::new("1", "1", "12a").build())
        .unwrap();
    w.commit().unwrap();
    let (index, schema) = open(dir.path());
    assert_eq!(count_term(&index, schema.house_number, "12A"), 1);
    assert_eq!(count_term(&index, schema.house_number, "12a"), 0);
}

#[test]
fn cli_run_imputes_missing_positions() {
    // Schema v9: BeSt `0 0` positions are imputed, never reprojected into
    // France. House 5 sits between 3 and 7 → interpolated; house 3 has a
    // positioned twin → same number; a street with no positioned address
    // → the part of municipality's centroid.
    let mut w = BestBundle::wallonia_sample();
    let mut seven = AddrSpec::new("2001", "6001", "91114", "5580", "7");
    seven.pom_oid = Some("9111401".into());
    seven.x = 205_400.0;
    seven.y = 95_000.0;
    let missing = |oid: &str, street: &str, house: &str| {
        let mut a = AddrSpec::new(oid, street, "91114", "5580", house);
        a.pom_oid = Some("9111401".into());
        a.x = 0.0;
        a.y = 0.0;
        a
    };
    w.addresses.push(seven);
    w.addresses.push(missing("2002", "6001", "5"));
    w.addresses.push(missing("2003", "6001", "3"));
    w.streets.push(StreetSpec {
        oid: "6002".into(),
        fr: Some("Rue Sans Position".into()),
        nl: None,
        de: None,
        status: "current".into(),
        muni_oid: "91114".into(),
    });
    w.addresses.push(missing("2004", "6002", "1"));
    let best = bundle_dir(&[w]);
    let out = tempfile::tempdir().unwrap();
    let report = cli::run(&Args::new(best.path(), out.path())).unwrap();
    assert_eq!(report.total().missing_position, 3);
    assert_eq!(
        (
            report.impute.same_number,
            report.impute.interpolated,
            report.impute.locality,
            report.impute.dropped
        ),
        (1, 1, 1, 0)
    );
    assert!(
        report
            .summary()
            .contains("imputed positions: same_number=1")
    );

    let (index, schema) = open(out.path());
    let searcher = index.reader().unwrap().searcher();
    let docs = searcher
        .search(&AllQuery, &tantivy::collector::DocSetCollector)
        .unwrap();
    assert_eq!(docs.len(), 5);
    let mut sources = Vec::new();
    for d in docs {
        let doc: tantivy::TantivyDocument = searcher.doc(d).unwrap();
        let payload = doc.get_first(schema.payload).unwrap();
        let addr: crabfinder_core::Address =
            serde_json::from_str(tantivy::schema::Value::as_str(&payload).unwrap()).unwrap();
        let Coordinates::Wgs84(p) = addr.coordinates else {
            panic!("Lambert left in the index");
        };
        assert!(p.is_in_belgium(), "{} at {p:?}", addr.house_number);
        sources.push((addr.house_number.clone(), addr.position_source));
    }
    sources.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.as_str().cmp(b.1.as_str())));
    use crabfinder_core::PositionSource as P;
    assert_eq!(
        sources,
        vec![
            ("1".into(), P::Locality),
            ("3".into(), P::Measured),
            ("3".into(), P::SameNumber),
            ("5".into(), P::Interpolated),
            ("7".into(), P::Measured),
        ]
    );
}
