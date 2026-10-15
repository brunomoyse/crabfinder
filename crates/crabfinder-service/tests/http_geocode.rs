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

#[tokio::test]
async fn features_carry_confidence() {
    let app = App::new(&fixtures::louise_street());

    let exact = app.ok(&geocode_uri("Avenue Louise 32 1050", "")).await;
    let f = &features(&exact)[0];
    assert_eq!(f["confidence"], "exact");
    assert_eq!(f["house_number_delta"], 0);
    assert!(f.get("fallback_reason").is_none());

    // No such number: the street-level tier answers at the median house.
    let median = app.ok(&geocode_uri("Avenue Louise 999 1050", "")).await;
    let fs = features(&median);
    assert!(!fs.is_empty());
    assert_eq!(fs[0]["confidence"], "median_street");
    assert_eq!(fs[0]["fallback_reason"], "house_number_out_of_range");
    assert!(fs[0].get("house_number_delta").is_none());
    assert_eq!(median["match_type"], "strict");

    // No number at all → street level, reason house_number_missing.
    let street = app.ok(&geocode_uri("Avenue Louise", "")).await;
    assert_eq!(features(&street)[0]["confidence"], "median_street");
    assert_eq!(
        features(&street)[0]["fallback_reason"],
        "house_number_missing"
    );

    // OR fallback → fuzzy_street.
    let fuzzy = app.ok(&geocode_uri("Louise Nowhere 32", "")).await;
    let fs = features(&fuzzy);
    assert!(!fs.is_empty());
    assert_eq!(fs[0]["confidence"], "fuzzy_street");
    assert_eq!(fuzzy["match_type"], "fuzzy");
}

#[tokio::test]
async fn parsed_echo_shows_box_alternatives_and_reading() {
    let app = App::new(&fixtures::multi_region());

    let body = app
        .ok(&geocode_uri("Chaussée de Waterloo 1357 Uccle", ""))
        .await;
    assert_eq!(body["query"]["parsed"]["postal_code"], "1357");
    let alts = body["query"]["parsed"]["alternatives"].as_array().unwrap();
    assert_eq!(alts.len(), 1);
    assert_eq!(alts[0]["house_number"], "1357");
    assert!(alts[0].get("alternatives").is_none());
    assert_eq!(body["query"]["reading"], 1);
    let f = &features(&body)[0];
    assert_eq!(prop(f, "house_number"), "1357");
    assert_eq!(f["confidence"], "exact");

    let body = app
        .ok(&geocode_uri("Rue du 11 Novembre 5, B-1050 Ixelles", ""))
        .await;
    let parsed = &body["query"]["parsed"];
    assert_eq!(parsed["house_number"], "5");
    assert_eq!(parsed["postal_code"], "1050");
    assert_eq!(parsed["box_number"], serde_json::Value::Null);
    assert!(parsed.get("alternatives").is_none());
    assert_eq!(body["query"]["reading"], 0);
    assert_eq!(features(&body)[0]["confidence"], "exact");

    let body = app.ok(&geocode_uri("Rue du 11 Novembre 5 bte 2", "")).await;
    assert_eq!(body["query"]["parsed"]["box_number"], "2");
}

#[tokio::test]
async fn postal_param_overrides_every_reading() {
    let app = App::new(&fixtures::multi_region());
    // With an explicit postal code, the reading that keeps 1357 as the
    // house number leads (the postal reading is demoted, not dropped).
    let body = app
        .ok(&geocode_uri(
            "Chaussée de Waterloo 1357 Uccle",
            "&postal_code=1180",
        ))
        .await;
    assert_eq!(body["query"]["reading"], 0);
    assert_eq!(body["query"]["parsed"]["house_number"], "1357");
    assert_eq!(
        body["query"]["parsed"]["alternatives"][0]["postal_code"],
        "1357"
    );
    assert_eq!(features(&body)[0]["confidence"], "exact");
    // A wrong explicit postal code applies to all readings → no exact.
    let body = app
        .ok(&geocode_uri(
            "Chaussée de Waterloo 1357 Uccle",
            "&postal_code=1000",
        ))
        .await;
    assert!(features(&body).is_empty());
}

#[tokio::test]
async fn typo_features_are_marked() {
    let app = App::new(&fixtures::louise_street());
    let body = app.ok(&geocode_uri("Avenue Loiuse 32, 1050", "")).await;
    let f = &features(&body)[0];
    assert_eq!(prop(f, "house_number"), "32");
    assert_eq!(f["match_type"], "typo");
    assert_eq!(f["confidence"], "fuzzy_street");
    assert_eq!(f["fallback_reason"], "street_spelling_fuzzy");
    assert_eq!(body["match_type"], "fuzzy");
}

#[tokio::test]
async fn nearest_number_features() {
    let app = App::new(&fixtures::louise_street());
    let body = app
        .ok(&geocode_uri("Avenue Louise 44, 1050", "&limit=3"))
        .await;
    let fs = features(&body);
    let hns: Vec<&str> = fs
        .iter()
        .map(|f| prop(f, "house_number").as_str().unwrap())
        .collect();
    assert_eq!(hns, ["40", "50", "32"]);
    assert_eq!(fs[0]["confidence"], "nearest_same_side");
    assert_eq!(fs[0]["house_number_delta"], -4);
    assert_eq!(fs[0]["fallback_reason"], "house_number_out_of_range");
    assert_eq!(fs[1]["house_number_delta"], 6);
}
