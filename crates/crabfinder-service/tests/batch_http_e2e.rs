//! End-to-end tests for `POST /batch/geocode` through the production router.
//!
//! Covers Content-Type dispatch (JSON / multipart / raw CSV), the CSV in →
//! CSV out streaming pipeline, the 1-in-1-out contract
//! (`docs/design-batch-geocoding.md` §3.1) and the `mid_street_fallback`
//! option. Algorithmic edge cases live in the resolver unit tests.

mod common;

use axum::http::StatusCode;
use common::{App, multipart};
use crabfinder_test_support::{col, fixtures, parse_csv};

fn app() -> App {
    App::new(&fixtures::batch_small())
}

const HEADER: &str = "id,street,house_number,postal_code\n";

#[tokio::test]
async fn raw_csv_body_returns_streamed_csv_with_header() {
    let app = app();
    let csv = format!("{HEADER}1,Avenue Louise,10,1050\n2,Grand-Place,1,1000\n");
    let (status, ct, body) = app.post("/batch/geocode", "text/csv", csv).await;
    assert_eq!(status, StatusCode::OK);
    assert!(ct.starts_with("text/csv"), "{ct}");
    let (h, rows) = parse_csv(&body);
    assert_eq!(h[0], "id");
    assert_eq!(rows.len(), 2);
    assert!(rows.iter().all(|r| col(&h, r, "confidence") == "exact"));
}

#[tokio::test]
async fn semicolon_csv_with_unknown_content_type_is_sniffed() {
    let app = app();
    let csv = "id;street;house_number;postal_code\n1;Avenue Louise;20;1050\n";
    let (status, _, body) = app
        .post("/batch/geocode", "application/octet-stream", csv)
        .await;
    assert_eq!(status, StatusCode::OK);
    let (h, rows) = parse_csv(&body);
    assert_eq!(col(&h, &rows[0], "matched_house_number"), "20");
}

#[tokio::test]
async fn json_body_resolves_addresses() {
    let app = app();
    let body = serde_json::json!({
        "addresses": [
            {"id": "a", "street": "Avenue Louise", "house_number": "10", "postal_code": "1050"},
            {"id": "b", "street": "Avenue Louise", "house_number": "999", "postal_code": "1050"},
        ],
        "options": {"mid_street_fallback": true, "format": "csv"}
    });
    let (status, _, body) = app
        .post("/batch/geocode", "application/json", body.to_string())
        .await;
    assert_eq!(status, StatusCode::OK);
    let (h, rows) = parse_csv(&body);
    assert_eq!(rows.len(), 2);
    assert_eq!(col(&h, &rows[0], "confidence"), "exact");
    assert_eq!(col(&h, &rows[1], "confidence"), "median_street");
    assert_eq!(
        col(&h, &rows[1], "fallback_reason"),
        "house_number_out_of_range"
    );
    // Median of 10/20/30 is n°20.
    let lat: f64 = col(&h, &rows[1], "lat").parse().unwrap();
    assert!((lat - 50.8310).abs() < 1e-9);
}

#[tokio::test]
async fn json_payload_over_10000_is_413() {
    let app = app();
    let addresses: Vec<_> = (0..10_001)
        .map(|i| serde_json::json!({"id": i.to_string(), "street": "Avenue Louise"}))
        .collect();
    let body = serde_json::json!({ "addresses": addresses });
    let (status, _, _) = app
        .post("/batch/geocode", "application/json", body.to_string())
        .await;
    assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE);
}

#[tokio::test]
async fn mid_street_fallback_disabled_changes_coords_not_confidence() {
    // Pins the pre-v2 behaviour: an empty mid-street table only changes
    // the coordinates. Phase 1 makes the option skip the tier instead.
    let app = app();
    let body = serde_json::json!({
        "addresses": [{"id": "b", "street": "Avenue Louise", "house_number": "999", "postal_code": "1050"}],
        "options": {"mid_street_fallback": false}
    });
    let (status, _, body) = app
        .post("/batch/geocode", "application/json", body.to_string())
        .await;
    assert_eq!(status, StatusCode::OK);
    let (h, rows) = parse_csv(&body);
    assert_eq!(col(&h, &rows[0], "confidence"), "median_street");
    let lat: f64 = col(&h, &rows[0], "lat").parse().unwrap();
    // Some Louise building's own coordinates (10/20/30 span 50.830–50.832).
    assert!((50.8299..=50.8321).contains(&lat), "{lat}");
}

#[tokio::test]
async fn multipart_file_with_options() {
    let app = app();
    let csv = format!("{HEADER}1,Grand-Place,1,1000\n");
    let (ct, body) = multipart(&[
        ("options", None, r#"{"mid_street_fallback": true}"#),
        ("file", Some("in.csv"), &csv),
    ]);
    let (status, _, body) = app.post("/batch/geocode", &ct, body).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (h, rows) = parse_csv(&body);
    assert_eq!(col(&h, &rows[0], "confidence"), "exact");
}

#[tokio::test]
async fn multipart_without_file_is_400() {
    let app = app();
    let (ct, body) = multipart(&[("options", None, "{}")]);
    let (status, _, _) = app.post("/batch/geocode", &ct, body).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn multipart_with_bad_options_is_400() {
    let app = app();
    let csv = format!("{HEADER}1,Grand-Place,1,1000\n");
    let (ct, body) = multipart(&[("options", None, "{nope"), ("file", Some("in.csv"), &csv)]);
    let (status, _, _) = app.post("/batch/geocode", &ct, body).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn malformed_multipart_is_400() {
    let app = app();
    let (status, _, _) = app
        .post("/batch/geocode", "multipart/form-data", "garbage")
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn malformed_json_body_is_400() {
    let app = app();
    let (status, _, _) = app
        .post("/batch/geocode", "application/json", "{not valid json")
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn malformed_csv_row_is_400() {
    let app = app();
    // Missing required `street` column.
    let (status, _, _) = app
        .post("/batch/geocode", "text/csv", "id,house_number\n1,10\n")
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn empty_csv_input_returns_empty_body_200() {
    let app = app();
    let (status, _, body) = app.post("/batch/geocode", "text/csv", HEADER).await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.is_empty());
}

#[tokio::test]
async fn input_with_duplicates_preserves_order_and_count() {
    let app = app();
    let csv = format!(
        "{HEADER}1,Avenue Louise,10,1050\n2,Grand-Place,1,1000\n3,AVENUE LOUISE,10,1050\n\
         4,Avenue Louise,30,1050\n5,grand-place,1,1000\n"
    );
    let (status, _, body) = app.post("/batch/geocode", "text/csv", csv).await;
    assert_eq!(status, StatusCode::OK);
    let (h, rows) = parse_csv(&body);
    let ids: Vec<&str> = rows.iter().map(|r| col(&h, r, "id")).collect();
    assert_eq!(ids, ["1", "2", "3", "4", "5"]);
    assert_eq!(
        col(&h, &rows[0], "confidence"),
        col(&h, &rows[2], "confidence")
    );
    assert_eq!(
        col(&h, &rows[1], "confidence"),
        col(&h, &rows[4], "confidence")
    );
}

#[tokio::test]
async fn large_batch_streams_in_several_chunks() {
    let app = app();
    let mut csv = String::from(HEADER);
    for i in 0..300 {
        csv.push_str(&format!("{i},Avenue Louise,{},1050\n", [10, 20, 30][i % 3]));
    }
    let (status, _, body) = app.post("/batch/geocode", "text/csv", csv).await;
    assert_eq!(status, StatusCode::OK);
    let (_, rows) = parse_csv(&body);
    assert_eq!(rows.len(), 300);
}
