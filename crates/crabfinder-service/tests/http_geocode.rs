//! `GET /geocode` through the real router. Ported from the pre-v2 test
//! module in `main.rs`, which re-implemented the handler instead of calling
//! it.

mod common;

use axum::http::StatusCode;
use common::{App, enc, features, prop, street_fr, street_nl};
use crabfinder_test_support::fixtures;

fn app() -> App {
    App::new(&fixtures::brussels_small())
}

fn geocode_uri(q: &str, extra: &str) -> String {
    format!("/geocode?q={}{extra}", enc(q))
}

#[tokio::test]
async fn grand_place_hyphen_equals_space() {
    let app = app();
    let a = app.ok(&geocode_uri("Grand-Place", "")).await;
    let b = app.ok(&geocode_uri("Grand Place", "")).await;
    assert!(!features(&a).is_empty());
    assert_eq!(
        features(&a)[0]["building_group_id"],
        features(&b)[0]["building_group_id"]
    );
}

#[tokio::test]
async fn grote_markt_does_not_return_grote_ringlaan_top() {
    let app = app();
    let body = app.ok(&geocode_uri("Grote Markt", "")).await;
    let fs = features(&body);
    assert!(street_nl(&fs[0]).contains("grote markt"));
    assert!(fs.iter().all(|f| !street_nl(f).contains("ringlaan")));
}

#[tokio::test]
async fn house_number_filters_results() {
    let app = app();
    let body = app.ok(&geocode_uri("Avenue Louise 32", "")).await;
    let fs = features(&body);
    assert!(!fs.is_empty());
    assert!(fs.iter().all(|f| prop(f, "house_number") == "32"));
    assert_eq!(body["query"]["parsed"]["house_number"], "32");
}

#[tokio::test]
async fn rue_de_la_loi_ranks_above_loi_distractor() {
    let app = app();
    let body = app.ok(&geocode_uri("Rue de la Loi", "")).await;
    let top = street_fr(&features(&body)[0]);
    assert!(top.contains("rue de la loi"), "top was {top}");
}

#[tokio::test]
async fn accent_insensitive_search() {
    let app = app();
    let a = app.ok(&geocode_uri("Étienne", "")).await;
    let b = app.ok(&geocode_uri("Etienne", "")).await;
    assert_eq!(a["count"], b["count"]);
    assert_eq!(
        features(&a)[0]["building_group_id"],
        features(&b)[0]["building_group_id"]
    );
}

#[tokio::test]
async fn fuzzy_fallback_marks_match_type() {
    let app = app();
    let body = app.ok(&geocode_uri("Louise quetzal", "")).await;
    assert!(!features(&body).is_empty());
    assert_eq!(body["match_type"], "fuzzy");
    assert_eq!(features(&body)[0]["match_type"], "fuzzy");
}

#[tokio::test]
async fn postal_code_in_query_narrows_results() {
    let app = app();
    let body = app.ok(&geocode_uri("Louise 1050", "")).await;
    let fs = features(&body);
    assert!(!fs.is_empty());
    assert!(fs.iter().all(|f| prop(f, "postal_code") == "1050"));
}

#[tokio::test]
async fn postal_code_param_overrides_query() {
    let app = app();
    // "1000" in q would match Grand-Place; the param forces 1040.
    let body = app.ok(&geocode_uri("Rue 1000", "&postal_code=1040")).await;
    let fs = features(&body);
    assert!(!fs.is_empty());
    assert!(fs.iter().all(|f| prop(f, "postal_code") == "1040"));
}

#[tokio::test]
async fn dedup_collapses_louise_32_to_one_feature() {
    let app = app();
    let body = app.ok(&geocode_uri("Avenue Louise 32", "")).await;
    let fs = features(&body);
    assert_eq!(fs.len(), 1);
    assert_eq!(fs[0]["unit_count"], 5);
    assert_eq!(body["raw_hits"], 5);
    assert_eq!(body["deduplicated"], true);
}

#[tokio::test]
async fn expand_true_returns_one_feature_per_row() {
    let app = app();
    let body = app
        .ok(&geocode_uri("Avenue Louise 32", "&expand=true"))
        .await;
    let fs = features(&body);
    assert_eq!(fs.len(), 5);
    assert!(fs.iter().all(|f| f["unit_count"] == 1));
    assert_eq!(body["deduplicated"], false);
    assert!(body["_note"].as_str().unwrap().contains("expand=true"));
}

#[tokio::test]
async fn singleton_building_unit_count_is_one() {
    let app = app();
    let body = app.ok(&geocode_uri("Grote-Ringlaan", "")).await;
    assert_eq!(features(&body)[0]["unit_count"], 1);
}

#[tokio::test]
async fn distinct_house_numbers_do_not_collapse() {
    let app = app();
    let body = app.ok(&geocode_uri("Grand-Place", "&limit=5")).await;
    assert_eq!(features(&body).len(), 2);
}

#[tokio::test]
async fn limit_counts_buildings_not_rows() {
    let app = app();
    let body = app.ok(&geocode_uri("Avenue Louise", "&limit=2")).await;
    assert_eq!(features(&body).len(), 2);
    assert!(body["raw_hits"].as_u64().unwrap() >= 7);
    assert_eq!(body["query"]["limit"], 2);
}

#[tokio::test]
async fn limit_is_clamped() {
    let app = app();
    let body = app.ok(&geocode_uri("Avenue Louise", "&limit=0")).await;
    assert_eq!(body["query"]["limit"], 1);
    let body = app.ok(&geocode_uri("Avenue Louise", "&limit=5000")).await;
    assert_eq!(body["query"]["limit"], 100);
}

#[tokio::test]
async fn strict_singleton_ranks_first() {
    let app = app();
    let body = app.ok(&geocode_uri("Étienne", "")).await;
    let top = &features(&body)[0];
    assert!(street_fr(top).contains("tienne"));
    assert_eq!(top["match_type"], "strict");
    assert_eq!(body["match_type"], "strict");
}

#[tokio::test]
async fn region_filter_accepts_aliases() {
    let app = app();
    for r in ["brussels", "BE-BRU", "bruxelles"] {
        let body = app
            .ok(&geocode_uri("Grand-Place", &format!("&region={r}")))
            .await;
        assert!(!features(&body).is_empty(), "region={r}");
    }
    // Every fixture is in Brussels, so a Flanders filter empties the result.
    let body = app
        .ok(&geocode_uri("Grand-Place", "&region=vlaanderen"))
        .await;
    assert!(features(&body).is_empty());
    assert_eq!(body["match_type"], "strict");
}

#[tokio::test]
async fn unknown_region_is_400() {
    let app = app();
    let (status, body) = app.get(&geocode_uri("Grand-Place", "&region=mars")).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(body["error"].as_str().unwrap().contains("unknown region"));
}

#[tokio::test]
async fn empty_q_is_400() {
    let app = app();
    let (status, body) = app.get("/geocode?q=%20%20").await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["error"], "empty q");
}

#[tokio::test]
async fn focus_point_reranks_and_validates() {
    let app = app();
    // Near Rue Loi-Belliard (1040) vs Rue de la Loi (1000): the OR-ish
    // query "Loi" matches both; focus near Belliard lifts it.
    let body = app
        .ok(&geocode_uri("Loi", "&focus_point=50.8400,4.3700"))
        .await;
    assert!(street_fr(&features(&body)[0]).contains("belliard"));

    let (status, body) = app
        .get(&geocode_uri("Loi", "&focus_point=4.37,50.84"))
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(body["error"].as_str().unwrap().contains("focus_point"));
}

#[tokio::test]
async fn no_match_returns_empty_collection() {
    let app = app();
    let body = app.ok(&geocode_uri("zzzqqq", "")).await;
    assert_eq!(body["count"], 0);
    assert!(features(&body).is_empty());
}

#[tokio::test]
async fn notability_disabled_still_ranks() {
    let app = App::with_options(&fixtures::brussels_small(), |mut o| {
        o.notability_enabled = false;
        o
    });
    let body = app.ok(&geocode_uri("Grand-Place 1", "")).await;
    assert_eq!(prop(&features(&body)[0], "house_number"), "1");
}

#[tokio::test]
async fn missing_q_is_400() {
    let app = app();
    let (status, _) = app.get("/geocode?limit=3").await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}
