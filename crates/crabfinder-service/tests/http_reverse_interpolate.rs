//! `/reverse?interpolate=true` (v2 Phase 10): the house number a point
//! would carry when it falls in a gap between two known numbers.

mod common;

use common::App;
use crabfinder_core::Address;
use crabfinder_test_support::AddressBuilder;

/// Metres east of 4.35 at lat 50.85 → longitude.
fn lon(m: f64) -> f64 {
    4.35 + m / 70_200.0
}

/// Rue Longue (one street object, 1000) along lat 50.85: odd 1, 3 at
/// 0 / 10 m, then a gap to 23 at 110 m; even numbers 2…42 every 5 m
/// across the street (12 m north). A namesake object in 1020 runs through
/// the gap with 13.
fn corpus() -> Vec<Address> {
    let mut out = Vec::new();
    let mut push = |id: &str, oid: &str, hn: &str, pc: &str, lat: f64, m: f64| {
        out.push(
            AddressBuilder::new(id, oid, hn)
                .street(Some("Rue Longue"), Some("Langestraat"))
                .postal(pc)
                .at(lat, lon(m))
                .build(),
        );
    };
    push("o1", "700", "1", "1000", 50.85, 0.0);
    push("o3", "700", "3", "1000", 50.85, 10.0);
    push("o23", "700", "23", "1000", 50.85, 110.0);
    for n in (2..=42).step_by(2) {
        push(
            &format!("e{n}"),
            "700",
            &n.to_string(),
            "1000",
            50.85011,
            (n as f64 - 2.0) * 5.5,
        );
    }
    push("x13", "701", "13", "1020", 50.8501, 60.0);
    out
}

#[tokio::test]
async fn a_point_in_a_gap_gets_an_interpolated_number() {
    let app = App::new(&corpus());
    // 3 m south of the odd side, halfway between 3 (10 m) and 23 (110 m).
    let uri = format!("/reverse?lat=50.84997&lng={}&interpolate=true", lon(60.0));
    let body = app.ok(&uri).await;
    let i = &body["interpolation"];
    assert_eq!(i["house_number"], "13", "{body}");
    assert_eq!(i["between"], serde_json::json!(["3", "23"]));
    assert_eq!(i["postal_code"], "1000");
    assert_eq!(i["street_name"]["fr"], "Rue Longue");
    assert_eq!(i["label"], "Rue Longue 13, 1000 Bruxelles");
    assert!(i["distance_m"].as_f64().unwrap() < 5.0, "{i}");
    assert!((i["coordinates"][0].as_f64().unwrap() - lon(60.0)).abs() < 1e-5);
    assert_eq!(body["query"]["interpolate"], true);
    // The features are unchanged: the real nearest buildings.
    assert_eq!(body["count"], 1);

    let body = app.ok(&format!("{uri}&lang=nl")).await;
    assert_eq!(
        body["interpolation"]["label"],
        "Langestraat 13, 1000 Brussel"
    );
}

#[tokio::test]
async fn no_interpolation_unless_asked_or_in_a_gap() {
    let app = App::new(&corpus());
    let body = app
        .ok(&format!("/reverse?lat=50.84997&lng={}", lon(60.0)))
        .await;
    assert!(body.get("interpolation").is_none(), "{body}");
    assert!(body["query"].get("interpolate").is_none());
    // On the even side every number exists: nothing to interpolate.
    let body = app
        .ok(&format!(
            "/reverse?lat=50.85014&lng={}&interpolate=true",
            lon(55.0)
        ))
        .await;
    assert!(body.get("interpolation").is_none(), "{body}");
    // Far from any address.
    let body = app.ok("/reverse?lat=50.80&lng=4.30&interpolate=true").await;
    assert!(body.get("interpolation").is_none(), "{body}");
}
