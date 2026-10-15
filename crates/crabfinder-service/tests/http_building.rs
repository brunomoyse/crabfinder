//! `GET /building/{id}/units` and `GET /health` through the real router.

mod common;

use common::{App, enc, features, prop};
use crabfinder_test_support::fixtures;

#[tokio::test]
async fn building_units_returns_all_units_in_stable_order() {
    let app = App::new(&fixtures::brussels_small());
    let body = app
        .ok(&format!("/geocode?q={}", enc("Avenue Louise 32")))
        .await;
    let bgid = features(&body)[0]["building_group_id"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(bgid.contains("|3919|32"));

    let uri = format!("/building/{}/units", enc(&bgid));
    let first = app.ok(&uri).await;
    let second = app.ok(&uri).await;
    assert_eq!(first["count"], 5);
    assert_eq!(first["building_group_id"], bgid.as_str());
    let ids = |b: &serde_json::Value| -> Vec<String> {
        features(b)
            .iter()
            .map(|f| prop(f, "id")["object_identifier"].as_str().unwrap().into())
            .collect()
    };
    assert_eq!(ids(&first), ids(&second));
    let boxes: Vec<&str> = features(&first)
        .iter()
        .map(|f| prop(f, "box_number").as_str().unwrap())
        .collect();
    assert_eq!(boxes, ["1", "2", "3", "4", "5"]);
    assert!(features(&first).iter().all(|f| f["unit_count"] == 1));
}

#[tokio::test]
async fn unknown_building_is_empty() {
    let app = App::new(&fixtures::brussels_small());
    let body = app
        .ok(&format!(
            "/building/{}/units",
            enc("https://example.invalid|0|0")
        ))
        .await;
    assert_eq!(body["count"], 0);
}

#[tokio::test]
async fn health_reports_doc_count() {
    let addrs = fixtures::brussels_small();
    let app = App::new(&addrs);
    let body = app.ok("/health").await;
    assert_eq!(body["status"], "ok");
    assert_eq!(body["index_docs"], addrs.len());
    assert!(body["uptime_secs"].is_u64());
    assert_eq!(
        body["index_path"],
        app.index.path().to_string_lossy().as_ref()
    );
}
