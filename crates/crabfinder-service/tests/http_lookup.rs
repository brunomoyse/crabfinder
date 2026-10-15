//! `/lookup` by BeSt id and the `/geocode?bbox=` filter (v2 Phase 10).

mod common;

use common::{App, enc, features, prop};
use crabfinder_test_support::fixtures;

#[tokio::test]
async fn lookup_returns_ids_in_order_and_lists_the_missing() {
    let addrs = fixtures::brussels_small();
    let app = App::new(&addrs);
    let loi = addrs.iter().find(|a| a.house_number == "200").unwrap();
    let place = addrs
        .iter()
        .find(|a| a.street_name.fr.as_deref() == Some("Grand-Place"))
        .unwrap();
    let uri = format!(
        "/lookup?id={}&lang=nl",
        enc(&format!("{}, nope/1/1 ,{}", loi.id, place.id))
    );
    let body = app.ok(&uri).await;
    assert_eq!(body["type"], "FeatureCollection");
    assert_eq!(body["count"], 2);
    assert_eq!(body["missing"], serde_json::json!(["nope/1/1"]));
    let fs = features(&body);
    assert_eq!(prop(&fs[0], "house_number"), "200");
    assert_eq!(fs[0]["match_type"], "id");
    assert_eq!(fs[0]["building_group_id"], loi.building_group_id.0);
    assert_eq!(prop(&fs[0], "label"), "Wetstraat 200, 1000 Brussel");
    assert_eq!(prop(&fs[1], "house_number"), place.house_number.as_str());
}

#[tokio::test]
async fn lookup_rejects_bad_requests() {
    let app = App::new(&fixtures::brussels_small());
    for (uri, want) in [
        ("/lookup?id=%20,%20", "id is empty".to_string()),
        (
            &*format!("/lookup?id={}", vec!["a/1/1"; 51].join(",")),
            "too many ids: 51 (max 50)".to_string(),
        ),
        ("/lookup?id=a/1/1&lang=en", "unknown lang 'en'".to_string()),
    ] {
        let (status, body) = app.get(uri).await;
        assert_eq!(status, 400, "{uri}");
        assert!(body["error"].as_str().unwrap().contains(&want), "{body}");
    }
    let (status, _) = app.get("/lookup").await;
    assert_eq!(status, 400);
}

#[tokio::test]
async fn bbox_is_a_hard_filter() {
    let app = App::new(&fixtures::brussels_small());
    // Rue de la Loi 16 sits at (50.8453, 4.3658), 200 at (50.8457, 4.3712).
    let around_16 = "4.3650,50.8450,4.3670,50.8460";
    let body = app
        .ok(&format!(
            "/geocode?q={}&bbox={around_16}",
            enc("Rue de la Loi")
        ))
        .await;
    let fs = features(&body);
    assert_eq!(fs.len(), 1, "{body}");
    assert_eq!(prop(&fs[0], "house_number"), "16");

    // Nothing of the street inside the box: no result outside it either.
    let elsewhere = "4.30,50.80,4.31,50.81";
    let body = app
        .ok(&format!(
            "/geocode?q={}&bbox={elsewhere}",
            enc("Rue de la Loi 200")
        ))
        .await;
    assert!(features(&body).is_empty(), "{body}");
}

#[tokio::test]
async fn malformed_bboxes_are_rejected() {
    let app = App::new(&fixtures::brussels_small());
    for bbox in ["1,2,3", "4.4,50.8,4.3,50.9", "a,b,c,d", "4.3,50.8,4.4,95"] {
        let (status, body) = app.get(&format!("/geocode?q=Loi&bbox={}", enc(bbox))).await;
        assert_eq!(status, 400, "{bbox}");
        assert!(
            body["error"]
                .as_str()
                .unwrap()
                .starts_with(&format!("invalid bbox '{bbox}'")),
            "{body}"
        );
    }
}
