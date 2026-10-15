//! `GET /reverse` through the real router.

mod common;

use axum::http::StatusCode;
use common::{App, features, street_fr};
use crabfinder_test_support::fixtures;

fn app() -> App {
    App::new(&fixtures::brussels_small())
}

fn dist(f: &serde_json::Value) -> f64 {
    f["distance_m"].as_f64().expect("distance_m")
}

#[tokio::test]
async fn exact_point_returns_grand_place() {
    let app = app();
    let body = app.ok("/reverse?lat=50.8466&lng=4.3525").await;
    let fs = features(&body);
    assert_eq!(fs.len(), 1, "default limit is 1");
    assert!(street_fr(&fs[0]).contains("grand-place"));
    assert!(dist(&fs[0]) < 5.0);
    assert_eq!(fs[0]["match_type"], "spatial");
    assert_eq!(body["query"]["limit"], 1);
    assert!(body["query"].get("radius_m").is_none());
}

#[tokio::test]
async fn dedupes_units_of_one_building() {
    let app = app();
    let body = app.ok("/reverse?lat=50.8345&lng=4.3567&limit=5").await;
    let top = &features(&body)[0];
    assert!(dist(top) < 5.0);
    assert_eq!(top["unit_count"], 5);
    assert!(body["raw_hits"].as_u64().unwrap() >= 5);
    assert!(
        top["building_group_id"]
            .as_str()
            .unwrap()
            .contains("|3919|32")
    );
}

#[tokio::test]
async fn distances_are_non_decreasing() {
    let app = app();
    let body = app.ok("/reverse?lat=50.84&lng=4.36&limit=10").await;
    let ds: Vec<f64> = features(&body).iter().map(dist).collect();
    assert!(ds.len() >= 3);
    assert!(ds.windows(2).all(|w| w[0] <= w[1]));
}

#[tokio::test]
async fn radius_filters() {
    let app = app();
    let body = app
        .ok("/reverse?lat=50.86&lng=4.40&limit=5&radius_m=10")
        .await;
    assert!(features(&body).is_empty());
    assert_eq!(body["query"]["radius_m"], 10.0);

    let body = app
        .ok("/reverse?lat=50.8467&lng=4.36&limit=10&radius_m=2000")
        .await;
    let fs = features(&body);
    assert!(fs.len() >= 3);
    assert!(fs.iter().all(|f| dist(f) <= 2000.0));
}

#[tokio::test]
async fn far_point_unbounded_returns_nearest() {
    let app = app();
    let body = app.ok("/reverse?lat=51.2194&lng=4.4025").await;
    assert!(dist(&features(&body)[0]) > 30_000.0);
    let body = app
        .ok("/reverse?lat=51.2194&lng=4.4025&limit=5&radius_m=5000")
        .await;
    assert!(features(&body).is_empty());
}

#[tokio::test]
async fn limit_counts_buildings_after_dedup() {
    let app = app();
    let body = app.ok("/reverse?lat=50.8345&lng=4.3567&limit=2").await;
    let fs = features(&body);
    assert_eq!(fs.len(), 2);
    assert_eq!(fs[0]["unit_count"], 5);
}

#[tokio::test]
async fn radius_zero_at_exact_point() {
    let app = app();
    let body = app
        .ok("/reverse?lat=50.8466&lng=4.3525&limit=5&radius_m=0")
        .await;
    assert!(dist(&features(&body)[0]) < 1e-6);
}

#[tokio::test]
async fn invalid_parameters_are_400() {
    let app = app();
    for uri in [
        "/reverse?lat=91&lng=4",
        "/reverse?lat=50&lng=181",
        "/reverse?lat=50&lng=4&radius_m=-1",
        "/reverse?lat=abc&lng=4",
        "/reverse?lng=4",
    ] {
        let (status, body) = app.get(uri).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{uri}");
        assert!(body["error"].is_string(), "{uri} → {body}");
    }
}

#[tokio::test]
async fn spatial_disabled_returns_nothing() {
    let app = App::with_options(&fixtures::brussels_small(), |mut o| {
        o.spatial = false;
        o
    });
    let body = app.ok("/reverse?lat=50.8466&lng=4.3525").await;
    assert!(features(&body).is_empty());
}
