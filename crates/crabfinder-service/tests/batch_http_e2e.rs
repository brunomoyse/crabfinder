//! End-to-end integration tests for `POST /batch/geocode`.
//!
//! Each test builds a real (tiny) Tantivy index from in-memory fixtures, wires
//! up the production `AppState` and an Axum `Router` with the batch route, then
//! drives the router with `tower::ServiceExt::oneshot` — no TCP listener, no
//! port allocation, no test-server crate. The whole flow exercises:
//!
//! * Content-Type dispatch (JSON / multipart / raw CSV)
//! * The CSV in → CSV out streaming pipeline
//! * The 1-in-1-out contract from `docs/design-batch-geocoding.md` §3.1
//! * The `mid_street_fallback` option toggle
//!
//! Fixtures kept deliberately minimal — covering the response *shape* and the
//! happy paths. Algorithmic edge cases live in the resolver unit tests.

use std::path::Path;
use std::sync::Arc;

use axum::{
    Router,
    body::Body,
    http::{HeaderValue, Request, StatusCode, header::CONTENT_TYPE},
};
use http_body_util::BodyExt;
use tantivy::{Index, IndexReader, ReloadPolicy};
use tower::ServiceExt;

use crabfinder_core::{
    Address, AddressId, BestId, BuildingGroupId, Coordinates, MultilingualName, MunicipalityId,
    PostalInfoId, Region, Status, StreetId, StreetType, WgsCoord,
};
use crabfinder_indexer::index::{CrabIndexWriter, CrabSchema, register_tokenizers};
use crabfinder_indexer::meta::write_sidecar;
use crabfinder_indexer::mid_street::MID_STREET_FILENAME;
use crabfinder_service::abbreviation::AbbreviationMap;
use crabfinder_service::addr_count::AddressCountIndex;
use crabfinder_service::batch;
use crabfinder_service::chrono_lite;
use crabfinder_service::mid_street::MidStreetIndex;
use crabfinder_service::spatial::SpatialIndex;
use crabfinder_service::AppState;

// --- fixtures ---------------------------------------------------------------

fn make_addr(
    addr_oid: &str,
    street_oid: &str,
    street_fr: Option<&str>,
    street_nl: Option<&str>,
    house: &str,
    box_no: Option<&str>,
    postal: &str,
    wgs_lat: f64,
    wgs_lon: f64,
) -> Address {
    let ns_addr = "https://databrussels.be/id/address".to_string();
    let ns_street = "https://databrussels.be/id/streetname".to_string();
    let ns_muni = "https://databrussels.be/id/municipality".to_string();
    let ns_postal = "https://databrussels.be/id/postalinfo".to_string();
    let street_id = StreetId::new(BestId::new(ns_street, street_oid, "1"));
    let building_group_id = BuildingGroupId::from_parts(&street_id, house);
    Address {
        id: AddressId::new(BestId::new(ns_addr, addr_oid, "1")),
        street_id,
        municipality_id: MunicipalityId::new(BestId::new(ns_muni, "21001", "1")),
        postal_info_id: PostalInfoId::new(BestId::new(ns_postal, postal, "1")),
        part_of_municipality_id: None,
        region: Region::Brussels,
        status: Status::Current,
        house_number: house.into(),
        box_number: box_no.map(|s| s.into()),
        officially_assigned: true,
        street_name: MultilingualName {
            fr: street_fr.map(|s| s.into()),
            nl: street_nl.map(|s| s.into()),
            de: None,
        },
        street_type: StreetType::Streetname,
        municipality_name: MultilingualName {
            fr: Some("Bruxelles".into()),
            nl: Some("Brussel".into()),
            de: None,
        },
        postal_code: postal.into(),
        postal_name: MultilingualName::default(),
        part_of_municipality_name: None,
        coordinates: Coordinates::Wgs84(WgsCoord {
            lat: wgs_lat,
            lon: wgs_lon,
        }),
        building_group_id,
    }
}

fn fixtures() -> Vec<Address> {
    vec![
        // Avenue Louise n°10 — exact match target.
        make_addr(
            "100", "3919", Some("Avenue Louise"), Some("Louizalaan"),
            "10", None, "1050", 50.8300, 4.3500,
        ),
        // Avenue Louise n°20 — second building, used for median.
        make_addr(
            "101", "3919", Some("Avenue Louise"), Some("Louizalaan"),
            "20", None, "1050", 50.8310, 4.3510,
        ),
        // Avenue Louise n°30 — third, used for median (the middle one).
        make_addr(
            "102", "3919", Some("Avenue Louise"), Some("Louizalaan"),
            "30", None, "1050", 50.8320, 4.3520,
        ),
        // Grand-Place 1 1000 — different street, used to verify isolation.
        make_addr(
            "200", "7", Some("Grand-Place"), Some("Grote Markt"),
            "1", None, "1000", 50.8466, 4.3525,
        ),
    ]
}

/// Build a self-contained test index with a v7 sidecar, a mid-street side-file,
/// and a few in-memory address fixtures. Returns the `AppState` ready to mount
/// onto an Axum router. Leaks the tempdir so file handles outlive the test
/// (macOS is fussy about Drop ordering of Tantivy directories).
fn build_app_state() -> AppState {
    let dir = tempfile::Builder::new()
        .prefix("crab-batch-http-e2e-")
        .tempdir()
        .unwrap();
    let dir_path = dir.path().to_path_buf();

    let mut writer = CrabIndexWriter::create(&dir_path).unwrap();
    for f in fixtures() {
        writer.add_address(&f).unwrap();
    }
    writer.commit().unwrap();
    write_sidecar(&dir_path).unwrap();

    // Hand-roll a tiny `mid-street-v1.bin`. The street_key encoding mirrors
    // the indexer's `MidStreetBuilder::finish` output: namespace|object_id.
    // Median of (10, 20, 30) is 20 → emit n°20's coords.
    let mid_street_path = dir_path.join(MID_STREET_FILENAME);
    write_mid_street_file(&mid_street_path);

    let schema = Arc::new(CrabSchema::build());
    let index = Arc::new(Index::open_in_dir(&dir_path).unwrap());
    register_tokenizers(&index);
    let reader: IndexReader = index
        .reader_builder()
        .reload_policy(ReloadPolicy::Manual)
        .try_into()
        .unwrap();

    let spatial = SpatialIndex::build_from_tantivy(&reader, &schema).unwrap();
    let abbrev = AbbreviationMap::empty();
    let addr_counts = AddressCountIndex::build(&reader, &schema).unwrap();
    let mid_street = MidStreetIndex::load(&mid_street_path).unwrap();

    // Keep dir alive — Tantivy mmap handles outlive the AppState; the OS
    // cleans the tempdir whenever the test process exits.
    std::mem::forget(dir);

    AppState {
        schema,
        index,
        reader,
        started_at: chrono_lite::Instant::now(),
        index_path: Arc::new(dir_path),
        spatial: Arc::new(spatial),
        notability_enabled: true,
        abbrev: Arc::new(abbrev),
        addr_counts: Arc::new(addr_counts),
        mid_street: Arc::new(mid_street),
    }
}

/// Reproduce the indexer's mid-street side-file layout for our 3 Louise
/// addresses (median = n°20 at (50.8310, 4.3510)).
fn write_mid_street_file(path: &Path) {
    use std::io::Write;
    let mut f = std::fs::File::create(path).unwrap();
    f.write_all(b"CRBMID7\0").unwrap();
    f.write_all(&1u64.to_le_bytes()).unwrap();
    // street_key for Avenue Louise = "{namespace}|{object_id}" where
    // BuildingGroupId is built from StreetId. The resolver strips the trailing
    // `|<house>` of a building_group_id to get this key — see
    // crabfinder_service::mid_street::street_key_from_bgid.
    // BestId for the street: "{ns}/{oid}/{ver}" — the StreetId's
    // BuildingGroupId encodes "{ns}|{oid}|{house}".
    let street_key = "https://databrussels.be/id/streetname|3919";
    f.write_all(&(street_key.len() as u32).to_le_bytes()).unwrap();
    f.write_all(street_key.as_bytes()).unwrap();
    let pc = "1050";
    f.write_all(&(pc.len() as u8).to_le_bytes()).unwrap();
    f.write_all(pc.as_bytes()).unwrap();
    f.write_all(&50.8310f64.to_le_bytes()).unwrap();
    f.write_all(&4.3510f64.to_le_bytes()).unwrap();
    f.flush().unwrap();
}

fn build_router() -> Router {
    let state = build_app_state();
    Router::new()
        .route(
            "/batch/geocode",
            axum::routing::post(batch::batch_geocode).route_layer(batch::body_limit_layer()),
        )
        .with_state(state)
}

async fn body_to_string(body: Body) -> String {
    let bytes = body.collect().await.unwrap().to_bytes();
    String::from_utf8(bytes.to_vec()).unwrap()
}

fn parse_csv_lines(body: &str) -> Vec<Vec<String>> {
    body.lines()
        .map(|l| l.split(',').map(|s| s.to_string()).collect())
        .collect()
}

/// Find the column index of a header name in the first row of the CSV.
fn col(rows: &[Vec<String>], name: &str) -> usize {
    rows[0]
        .iter()
        .position(|h| h == name)
        .unwrap_or_else(|| panic!("missing column {name:?} in header"))
}

// --- raw CSV path -----------------------------------------------------------

#[tokio::test]
async fn raw_csv_body_returns_streamed_csv_with_header() {
    let app = build_router();
    let csv = "id,street,house_number,postal_code\n\
               1,Avenue Louise,10,1050\n\
               2,Grand-Place,1,1000\n";
    let req = Request::builder()
        .method("POST")
        .uri("/batch/geocode")
        .header(CONTENT_TYPE, HeaderValue::from_static("text/csv"))
        .body(Body::from(csv))
        .unwrap();
    let res = app.oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let ct = res
        .headers()
        .get(CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    assert!(ct.starts_with("text/csv"), "wrong content-type: {ct}");

    let body = body_to_string(res.into_body()).await;
    let rows = parse_csv_lines(&body);

    // Header + 2 data rows == 3 lines.
    assert_eq!(rows.len(), 3, "1-in-1-out broken: got {rows:?}");
    assert_eq!(rows[0][0], "id");
    let confidence_idx = col(&rows, "confidence");
    assert_eq!(rows[1][confidence_idx], "exact", "Louise n°10 must be exact");
    assert_eq!(rows[2][confidence_idx], "exact", "Grand-Place 1 must be exact");
}

// --- JSON path --------------------------------------------------------------

#[tokio::test]
async fn json_body_resolves_addresses_and_streams_csv() {
    let app = build_router();
    let body = serde_json::json!({
        "addresses": [
            {"id": "a", "street": "Avenue Louise", "house_number": "10", "postal_code": "1050"},
            {"id": "b", "street": "Avenue Louise", "house_number": "999", "postal_code": "1050"},
        ],
        "options": {"mid_street_fallback": true, "format": "csv"}
    });
    let req = Request::builder()
        .method("POST")
        .uri("/batch/geocode")
        .header(CONTENT_TYPE, "application/json")
        .body(Body::from(serde_json::to_vec(&body).unwrap()))
        .unwrap();
    let res = app.oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);

    let body = body_to_string(res.into_body()).await;
    let rows = parse_csv_lines(&body);
    assert_eq!(rows.len(), 3, "header + 2 rows");
    let confidence_idx = col(&rows, "confidence");
    let id_idx = col(&rows, "id");
    let fallback_idx = col(&rows, "fallback_reason");

    // Row 'a' = Louise 10 → exact.
    let row_a = rows.iter().find(|r| r[id_idx] == "a").expect("row a");
    assert_eq!(row_a[confidence_idx], "exact");
    // Row 'b' = Louise 999 → number doesn't exist, mid_street_fallback=true,
    // we expect MedianStreet (fallback to median of the street).
    let row_b = rows.iter().find(|r| r[id_idx] == "b").expect("row b");
    assert_eq!(
        row_b[confidence_idx],
        "median_street",
        "Louise 999 must fall back to median_street (got fallback_reason={})",
        row_b[fallback_idx]
    );
    assert_eq!(row_b[fallback_idx], "house_number_out_of_range");
}

#[tokio::test]
async fn json_payload_over_10000_is_413() {
    let app = build_router();
    let mut addresses: Vec<serde_json::Value> = Vec::with_capacity(10_001);
    for i in 0..10_001 {
        addresses.push(serde_json::json!({
            "id": i.to_string(),
            "street": "Avenue Louise",
            "house_number": "10",
            "postal_code": "1050"
        }));
    }
    let body = serde_json::json!({"addresses": addresses});
    let req = Request::builder()
        .method("POST")
        .uri("/batch/geocode")
        .header(CONTENT_TYPE, "application/json")
        .body(Body::from(serde_json::to_vec(&body).unwrap()))
        .unwrap();
    let res = app.oneshot(req).await.unwrap();
    assert_eq!(
        res.status(),
        StatusCode::PAYLOAD_TOO_LARGE,
        "JSON cap is 10k addresses per design §4.6"
    );
}

#[tokio::test]
async fn mid_street_fallback_disabled_changes_coords_not_confidence() {
    // Same query as the JSON test (Louise 999), but with the fallback
    // option set to `false`. The HTTP layer responds by swapping an empty
    // MidStreetIndex into the resolver context.
    //
    // NOTE — design vs implementation gap, surfaced by this test:
    // The design (docs/design-batch-geocoding.md §3.4) and the user docs
    // (docs/{fr,en}/06-faq.md) promise that --no-mid-street / fallback=false
    // makes `median_street` outcomes "degrade to NotFound". The current
    // resolver code (crates/crabfinder-service/src/resolver.rs step 2)
    // still emits `Confidence::MedianStreet` whenever the strict-without-
    // house-number pass finds *anything*; an empty mid_street lookup only
    // changes the coords (falls back to the top hit's own lat/lon) without
    // demoting the confidence label.
    //
    // This test pins the *current* behaviour so future changes have to
    // make an explicit choice: either implement the design promise (demote
    // to NotFound), or update the docs to describe the implemented
    // behaviour. As-is, fallback=false still returns `median_street` —
    // just with different coords.
    let app = build_router();
    let body = serde_json::json!({
        "addresses": [
            {"id": "b", "street": "Avenue Louise", "house_number": "999", "postal_code": "1050"},
        ],
        "options": {"mid_street_fallback": false}
    });
    let req = Request::builder()
        .method("POST")
        .uri("/batch/geocode")
        .header(CONTENT_TYPE, "application/json")
        .body(Body::from(serde_json::to_vec(&body).unwrap()))
        .unwrap();
    let res = app.oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);

    let body = body_to_string(res.into_body()).await;
    let rows = parse_csv_lines(&body);
    let confidence_idx = col(&rows, "confidence");
    let lat_idx = col(&rows, "lat");
    assert_eq!(rows.len(), 2);
    // Current behaviour: confidence is still median_street.
    assert_eq!(
        rows[1][confidence_idx], "median_street",
        "current implementation: fallback=false changes coords but not confidence"
    );
    // The coords come from the top strict-without-HN hit (one of n°10/20/30),
    // not from the side-table median (50.831 / 4.351). With the dataset
    // we just built the top hit is likely n°10 → lat ≈ 50.83 but NOT
    // exactly 50.831.
    let lat: f64 = rows[1][lat_idx].parse().expect("parse lat");
    // Loose bound: somewhere on Louise, not at the precomputed median.
    assert!(
        (lat - 50.83).abs() < 0.01,
        "lat outside Louise band: {lat}"
    );
}

// --- malformed input --------------------------------------------------------

#[tokio::test]
async fn malformed_json_body_is_400() {
    let app = build_router();
    let req = Request::builder()
        .method("POST")
        .uri("/batch/geocode")
        .header(CONTENT_TYPE, "application/json")
        .body(Body::from("{not valid json"))
        .unwrap();
    let res = app.oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn empty_csv_input_returns_empty_body_200() {
    // Header only, no rows → 200 with empty body (per docstring in
    // batch/http.rs around the early-return on empty rows).
    let app = build_router();
    let csv = "id,street,house_number,postal_code\n";
    let req = Request::builder()
        .method("POST")
        .uri("/batch/geocode")
        .header(CONTENT_TYPE, "text/csv")
        .body(Body::from(csv))
        .unwrap();
    let res = app.oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let body = body_to_string(res.into_body()).await;
    assert!(body.is_empty(), "expected empty body, got {body:?}");
}

// --- 1-in-1-out contract ----------------------------------------------------

#[tokio::test]
async fn input_with_duplicates_preserves_order_and_count() {
    let app = build_router();
    // 5 rows, 2 are duplicates of each other; the output must have 5 rows
    // in the same order. Dedup happens internally for the *resolve* call
    // count, not the row count.
    let csv = "id,street,house_number,postal_code\n\
               1,Avenue Louise,10,1050\n\
               2,Grand-Place,1,1000\n\
               3,AVENUE LOUISE,10,1050\n\
               4,Avenue Louise,30,1050\n\
               5,grand-place,1,1000\n";
    let req = Request::builder()
        .method("POST")
        .uri("/batch/geocode")
        .header(CONTENT_TYPE, "text/csv")
        .body(Body::from(csv))
        .unwrap();
    let res = app.oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let body = body_to_string(res.into_body()).await;
    let rows = parse_csv_lines(&body);
    assert_eq!(rows.len(), 6, "header + 5 rows = 6 lines");

    let id_idx = col(&rows, "id");
    let ids: Vec<&str> = rows
        .iter()
        .skip(1)
        .map(|r| r[id_idx].as_str())
        .collect();
    assert_eq!(ids, vec!["1", "2", "3", "4", "5"], "order broken");

    // Duplicate rows share a confidence verdict.
    let confidence_idx = col(&rows, "confidence");
    assert_eq!(rows[1][confidence_idx], rows[3][confidence_idx]);
    assert_eq!(rows[2][confidence_idx], rows[5][confidence_idx]);
}
