//! `/geocode` alternatives by municipality and the batch `alternatives`
//! columns (v3 Phase 11.3).

mod common;

use axum::http::StatusCode;
use common::{App, enc, features};
use crabfinder_core::{Address, Region};
use crabfinder_test_support::{AddressBuilder, col, parse_csv};

/// "Rue de la Station" 10 in Charleroi (6000) and La Louvière (7100),
/// 12 in Charleroi (6001); "Avenue Louise" 32 only in Ixelles.
fn addresses() -> Vec<Address> {
    let station = |oid: &str, street: &str, hn: &str, nis: &str, name: &str, pc: &str| {
        AddressBuilder::new(oid, street, hn)
            .region(Region::Wallonia)
            .street(Some("Rue de la Station"), None)
            .municipality(nis, Some(name), None, None)
            .postal(pc)
            .build()
    };
    vec![
        station("c10", "1", "10", "52011", "Charleroi", "6000"),
        station("c12", "2", "12", "52011", "Charleroi", "6001"),
        station("l10", "3", "10", "58001", "La Louvière", "7100"),
        AddressBuilder::new("x32", "9", "32")
            .street(Some("Avenue Louise"), Some("Louizalaan"))
            .municipality("21009", Some("Ixelles"), Some("Elsene"), None)
            .postal("1050")
            .build(),
    ]
}

#[tokio::test]
async fn ambiguous_answers_come_with_their_municipalities() {
    let app = App::new(&addresses());
    let body = app
        .ok(&format!(
            "/geocode?q={}&limit=1",
            enc("Rue de la Station 10")
        ))
        .await;
    assert_eq!(body["ambiguous"], true);
    assert_eq!(features(&body).len(), 1, "limit still applies to features");
    let alts = body["alternatives"].as_array().unwrap();
    let mut nis: Vec<&str> = alts.iter().map(|a| a["nis"].as_str().unwrap()).collect();
    nis.sort();
    assert_eq!(nis, ["52011", "58001"]);
    let la = alts.iter().find(|a| a["nis"] == "58001").unwrap();
    assert_eq!(la["municipality_name"]["fr"], "La Louvière");
    assert_eq!(la["postal_codes"], serde_json::json!(["7100"]));
    assert_eq!(la["count"], 1);
    assert_eq!(la["feature"]["type"], "Feature");
    assert_eq!(
        la["feature"]["properties"]["label"],
        "Rue de la Station 10, 7100 La Louvière"
    );
}

#[tokio::test]
async fn alternatives_on_request_only() {
    let app = App::new(&addresses());
    // A clear answer: no alternatives unless asked for.
    let body = app.ok("/geocode?q=Avenue%20Louise%2032").await;
    assert_eq!(body["ambiguous"], false);
    assert!(body.get("alternatives").is_none());
    let body = app
        .ok("/geocode?q=Avenue%20Louise%2032&alternatives=true&lang=nl")
        .await;
    let alts = body["alternatives"].as_array().unwrap();
    assert_eq!(alts.len(), 1);
    assert_eq!(
        alts[0]["feature"]["properties"]["label"],
        "Louizalaan 32, 1050 Elsene"
    );
    // Without a number, every building counts in its group.
    let body = app
        .ok("/geocode?q=Rue%20de%20la%20Station&alternatives=true")
        .await;
    let charleroi = body["alternatives"]
        .as_array()
        .unwrap()
        .iter()
        .find(|a| a["nis"] == "52011")
        .unwrap()
        .clone();
    assert_eq!(charleroi["count"], 2);
    assert_eq!(
        charleroi["postal_codes"],
        serde_json::json!(["6000", "6001"])
    );
    let (status, _) = app.get("/geocode?q=x&alternatives=maybe").await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn batch_alternatives_columns() {
    let app = App::new(&addresses());
    let csv = "id,street,house_number,postal_code\n\
               1,Rue de la Station,10,\n\
               2,Avenue Louise,32,1050\n\
               3,Nowhere,1,\n";
    let (_, _, body) = app.post("/batch/geocode", "text/csv", csv).await;
    let (h, _) = parse_csv(&body);
    assert!(!h.iter().any(|c| c == "alternatives" || c == "score_gap"));

    let (status, _, body) = app
        .post(
            "/batch/geocode?alternatives=true&corrections=true",
            "text/csv",
            csv,
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    let (h, rows) = parse_csv(&body);
    assert_eq!(
        h[h.len() - 4..],
        ["status", "corrections", "score_gap", "alternatives"]
    );
    let mut alts: Vec<&str> = col(&h, &rows[0], "alternatives").split("; ").collect();
    alts.sort();
    assert_eq!(alts, ["Charleroi (6000)", "La Louvière (7100)"]);
    assert_eq!(col(&h, &rows[0], "ambiguous"), "true");
    assert_eq!(col(&h, &rows[0], "status"), "ambiguous");
    let gap: f32 = col(&h, &rows[0], "score_gap").parse().unwrap();
    assert!(gap < 0.1, "{gap}");
    assert_eq!(col(&h, &rows[1], "alternatives"), "Ixelles (1050)");
    assert_eq!(col(&h, &rows[1], "score_gap"), "", "one candidate: no gap");
    assert_eq!(col(&h, &rows[2], "alternatives"), "");
    assert_eq!(col(&h, &rows[2], "confidence"), "not_found");
}
