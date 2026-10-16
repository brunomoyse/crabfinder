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
async fn mid_street_fallback_disabled_yields_not_found() {
    let app = app();
    let body = serde_json::json!({
        "addresses": [
            {"id": "b", "street": "Avenue Louise", "house_number": "999", "postal_code": "1050"},
            {"id": "a", "street": "Avenue Louise", "house_number": "10", "postal_code": "1050"},
        ],
        "options": {"mid_street_fallback": false}
    });
    let (status, _, body) = app
        .post("/batch/geocode", "application/json", body.to_string())
        .await;
    assert_eq!(status, StatusCode::OK);
    let (h, rows) = parse_csv(&body);
    assert_eq!(col(&h, &rows[0], "confidence"), "not_found");
    assert_eq!(col(&h, &rows[0], "fallback_reason"), "no_match");
    assert_eq!(col(&h, &rows[1], "confidence"), "exact");
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

// --- Phase 6: free text, GeoJSON, lang ---------------------------------------

#[tokio::test]
async fn free_text_rows_are_parsed_like_geocode() {
    let app = app();
    let csv = "id,address\n\
               1,\"Avenue Louise 20, 1050 Bruxelles\"\n\
               2,Grand-Place 1 1000\n\
               3,Nowhere Street 1\n";
    let (status, _, body) = app.post("/batch/geocode", "text/csv", csv).await;
    assert_eq!(status, StatusCode::OK);
    let (h, rows) = parse_csv(&body);
    assert_eq!(rows.len(), 3, "1-in-1-out");
    assert_eq!(col(&h, &rows[0], "confidence"), "exact");
    assert_eq!(col(&h, &rows[0], "matched_house_number"), "20");
    assert_eq!(
        col(&h, &rows[0], "address"),
        "Avenue Louise 20, 1050 Bruxelles"
    );
    assert_eq!(
        col(&h, &rows[0], "label"),
        "Avenue Louise 20, 1050 Bruxelles"
    );
    assert_eq!(col(&h, &rows[0], "position_source"), "measured");
    assert_eq!(col(&h, &rows[0], "ambiguous"), "false");
    assert_eq!(col(&h, &rows[1], "matched_street"), "Grand-Place");
    assert_ne!(col(&h, &rows[2], "confidence"), "exact");

    // JSON rows too; a postal_code column overrides the parsed one.
    let body = serde_json::json!({"addresses": [
        {"id": "a", "address": "Avenue Louise 30 1000", "postal_code": "1050"},
    ]});
    let (_, _, body) = app
        .post("/batch/geocode", "application/json", body.to_string())
        .await;
    let (h, rows) = parse_csv(&body);
    assert_eq!(col(&h, &rows[0], "confidence"), "exact");
    assert_eq!(col(&h, &rows[0], "matched_postal_code"), "1050");
}

#[tokio::test]
async fn geojson_output_and_query_options() {
    let app = app();
    let csv = format!("{HEADER}1,Avenue Louise,10,1050\n2,Nowhere,1,1000\n");
    let (status, ct, body) = app
        .post("/batch/geocode?format=geojson&lang=nl", "text/csv", csv)
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(ct, "application/geo+json");
    let v: serde_json::Value = serde_json::from_str(&body).unwrap();
    let fs = v["features"].as_array().unwrap();
    assert_eq!(fs.len(), 2);
    assert_eq!(fs[0]["geometry"]["type"], "Point");
    assert_eq!(fs[0]["properties"]["id"], "1");
    assert_eq!(fs[0]["properties"]["confidence"], "exact");
    assert!(fs[1]["geometry"].is_null());
    assert_eq!(fs[1]["properties"]["confidence"], "not_found");

    // Options in a JSON body work the same; the query string wins.
    let body = serde_json::json!({
        "addresses": [{"id": "a", "street": "Avenue Louise", "house_number": "999", "postal_code": "1050"}],
        "options": {"format": "geojson", "mid_street_fallback": true}
    });
    let (_, ct, out) = app
        .post(
            "/batch/geocode?mid_street_fallback=false",
            "application/json",
            body.to_string(),
        )
        .await;
    assert_eq!(ct, "application/geo+json");
    let v: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(v["features"][0]["properties"]["confidence"], "not_found");

    // Empty input still yields a valid document.
    let (_, ct, out) = app
        .post("/batch/geocode?format=geojson", "text/csv", HEADER)
        .await;
    assert_eq!(ct, "application/geo+json");
    assert_eq!(out, r#"{"type":"FeatureCollection","features":[]}"#);
}

#[tokio::test]
async fn unknown_format_lang_or_query_is_400() {
    let app = app();
    let csv = format!("{HEADER}1,Avenue Louise,10,1050\n");
    for (uri, msg) in [
        (
            "/batch/geocode?format=xml",
            "unknown format 'xml' (expected csv or geojson)",
        ),
        (
            "/batch/geocode?lang=en",
            "unknown lang 'en' (expected fr, nl or de)",
        ),
    ] {
        let (status, _, body) = app.post(uri, "text/csv", csv.clone()).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{uri}");
        assert_eq!(body, msg);
    }
    let (status, _, body) = app
        .post(
            "/batch/geocode?mid_street_fallback=maybe",
            "text/csv",
            csv.clone(),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(body.starts_with("query string:"), "{body}");
    let body = serde_json::json!({
        "addresses": [{"id": "a", "street": "Avenue Louise"}],
        "options": {"format": "pdf"}
    });
    let (status, _, _) = app
        .post("/batch/geocode", "application/json", body.to_string())
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn label_language_follows_lang() {
    let app = app();
    let csv = format!("{HEADER}1,Grand-Place,1,1000\n");
    let (_, _, body) = app.post("/batch/geocode?lang=nl", "text/csv", csv).await;
    let (h, rows) = parse_csv(&body);
    assert_eq!(col(&h, &rows[0], "label"), "Grote Markt 1, 1000 Brussel");
}

#[tokio::test]
async fn corrections_option_adds_the_validate_columns() {
    let app = app();
    let csv = "id,street,house_number,postal_code,city\n\
               1,Avenue Louise,10,1050,\n\
               2,Avenue Louisse,20,,Ixelles\n\
               3,Nowhere,1,1000,\n";
    // Off by default: the columns are unchanged.
    let (_, _, body) = app.post("/batch/geocode", "text/csv", csv).await;
    let (h, _) = parse_csv(&body);
    assert!(
        !h.iter().any(|c| c == "status" || c == "corrections"),
        "{h:?}"
    );

    let (status, _, body) = app
        .post("/batch/geocode?corrections=true", "text/csv", csv)
        .await;
    assert_eq!(status, StatusCode::OK);
    let (h, rows) = parse_csv(&body);
    assert_eq!(h[h.len() - 2..], ["status", "corrections"]);
    assert_eq!(col(&h, &rows[0], "status"), "valid");
    assert_eq!(col(&h, &rows[0], "corrections"), "");
    assert_eq!(col(&h, &rows[1], "status"), "corrected");
    assert_eq!(
        col(&h, &rows[1], "corrections"),
        "street_spelling:street:louisse->louise; municipality_changed:city:ixelles->Bruxelles; \
         postal_code_added:postal_code:->1050"
    );
    assert_eq!(col(&h, &rows[2], "status"), "not_found");

    // Same through JSON options and GeoJSON output.
    let body = serde_json::json!({
        "addresses": [{"id": "a", "street": "Avenue Louise", "house_number": "10", "postal_code": "1050"}],
        "options": {"format": "geojson", "corrections": true}
    });
    let (_, _, out) = app
        .post("/batch/geocode", "application/json", body.to_string())
        .await;
    let v: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(v["features"][0]["properties"]["status"], "valid");
}
