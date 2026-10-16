//! `GET /validate` (v3 Phase 11.2) through the production router. The
//! correction rules themselves are unit-tested in `validate.rs`.

mod common;

use axum::http::StatusCode;
use common::{App, enc};
use crabfinder_test_support::fixtures;
use serde_json::{Value, json};

fn app() -> App {
    App::new(&fixtures::brussels_small())
}

fn kinds(body: &Value) -> Vec<String> {
    body["corrections"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["kind"].as_str().unwrap().to_string())
        .collect()
}

#[tokio::test]
async fn a_valid_address_echoes_the_official_one() {
    let app = app();
    let body = app
        .ok(&format!("/validate?q={}", enc("rue de la loi 16, 1000")))
        .await;
    assert_eq!(body["status"], "valid");
    assert_eq!(body["confidence"], "exact");
    assert_eq!(body["corrections"], json!([]));
    assert!(body.get("fallback_reason").is_none());
    assert_eq!(body["query"]["q"], "rue de la loi 16, 1000");
    assert_eq!(body["query"]["reading"], 0);
    let a = &body["address"];
    assert_eq!(a["type"], "Feature");
    assert_eq!(a["properties"]["label"], "Rue de la Loi 16, 1000 Bruxelles");
    assert_eq!(a["confidence"], "exact");
    assert_eq!(a["house_number_delta"], 0);
}

#[tokio::test]
async fn structured_fields_and_corrections() {
    let app = app();
    // Misspelt street, a city that isn't the address's, no postal code.
    let body = app
        .ok("/validate?street=Avenue%20Louisse&housenumber=32&box=3&city=Namur&lang=nl")
        .await;
    assert_eq!(body["status"], "corrected");
    assert_eq!(
        kinds(&body),
        [
            "street_spelling",
            "street_language",
            "municipality_changed",
            "postal_code_added"
        ]
    );
    let c = &body["corrections"][0];
    assert_eq!(
        (
            c["field"].clone(),
            c["input"].clone(),
            c["official"].clone()
        ),
        (json!("street"), json!("louisse"), json!("louise"))
    );
    assert_eq!(body["corrections"][2]["official"], "Brussel");
    assert_eq!(body["address"]["properties"]["box_number"], "3");
    assert_eq!(
        body["address"]["properties"]["label"],
        "Louizalaan 32 bus 3, 1050 Brussel"
    );
    assert!(body["query"].get("q").is_none());
    assert_eq!(body["fallback_reason"], "street_spelling_fuzzy");
}

#[tokio::test]
async fn incomplete_and_not_found() {
    let app = app();
    let body = app
        .ok("/validate?street=Rue%20de%20la%20Loi&postcode=1000")
        .await;
    assert_eq!(body["status"], "incomplete");
    assert_eq!(body["confidence"], "median_street");
    let body = app
        .ok(&format!("/validate?q={}", enc("Rue de la Loi 999 1000")))
        .await;
    assert_eq!(body["status"], "incomplete");
    assert_eq!(kinds(&body), ["house_number_not_found"]);
    let body = app.ok("/validate?q=xyzzy%20plugh").await;
    assert_eq!(body["status"], "not_found");
    assert!(body["address"].is_null());
    assert_eq!(body["fallback_reason"], "no_match");
}

#[tokio::test]
async fn region_filter_and_bad_input() {
    let app = app();
    let body = app
        .ok(&format!(
            "/validate?q={}&region=flanders",
            enc("Rue de la Loi 16 1000")
        ))
        .await;
    assert_eq!(body["status"], "not_found");
    for (uri, msg) in [
        ("/validate", "empty query"),
        ("/validate?q=x&street=y", "q cannot be combined"),
        ("/validate?q=x&region=mars", "unknown region 'mars'"),
        ("/validate?q=x&lang=en", "unknown lang 'en'"),
        (
            "/validate?street=x&postcode=12",
            "postcode must be 4 digits",
        ),
    ] {
        let (status, body) = app.get(uri).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{uri}");
        assert!(body.to_string().contains(msg), "{uri}: {body}");
    }
}
