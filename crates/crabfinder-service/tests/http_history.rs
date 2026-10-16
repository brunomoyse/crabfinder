//! `/lookup` of old BeSt ids: versions, the history side-file and derived
//! successors (v3 Phase 11.4, schema v12).

mod common;

use common::{App, enc, prop};
use crabfinder_core::{Address, Status};
use crabfinder_test_support::{AddressBuilder, fixtures};
use serde_json::Value;

fn louise(oid: &str, house: &str) -> AddressBuilder {
    AddressBuilder::new(oid, fixtures::LOUISE_OID, house)
        .street(Some("Avenue Louise"), Some("Louizalaan"))
        .postal("1050")
}

/// Avenue Louise addresses that BeSt no longer lists as current.
fn gone(current: &[Address]) -> Vec<Address> {
    let a50 = current.iter().find(|a| a.house_number == "50").unwrap();
    vec![
        // 32 had a box B: 32 (no box) is still there.
        louise("r32", "32").box_no("B").build(),
        // 40 without box: 40 without box is still there.
        louise("r40", "40").build(),
        // 99 is gone, but a50 stands where it stood.
        louise("r99", "99").at(a50.lat(), a50.lon()).build(),
        // Never assigned, nowhere near anything.
        louise("r98", "98")
            .status(Status::Reserved)
            .at(50.70, 4.20)
            .build(),
    ]
}

trait LatLon {
    fn lat(&self) -> f64;
    fn lon(&self) -> f64;
}

impl LatLon for Address {
    fn lat(&self) -> f64 {
        match self.coordinates {
            crabfinder_core::Coordinates::Wgs84(w) => w.lat,
            crabfinder_core::Coordinates::Lambert72(_) => unreachable!(),
        }
    }
    fn lon(&self) -> f64 {
        match self.coordinates {
            crabfinder_core::Coordinates::Wgs84(w) => w.lon,
            crabfinder_core::Coordinates::Lambert72(_) => unreachable!(),
        }
    }
}

fn results(body: &Value) -> &Vec<Value> {
    body["results"].as_array().expect("results array")
}

#[tokio::test]
async fn ids_resolve_across_versions_history_and_successors() {
    let current = fixtures::louise_street();
    let gone = gone(&current);
    let app = App::with_history(&current, &gone);
    let a10 = current.iter().find(|a| a.house_number == "10").unwrap();
    let object_key = a10.id.0.object_key();
    let ids = [
        a10.id.to_string(),
        object_key.clone(),
        format!("{object_key}/0"),
        gone[0].id.to_string(),
        gone[1].id.0.object_key(),
        gone[2].id.to_string(),
        gone[3].id.to_string(),
        "nope/1/1".to_string(),
    ];
    let body = app.ok(&format!("/lookup?id={}", enc(&ids.join(",")))).await;

    // `features` / `missing` keep their v2 meaning: addresses today.
    assert_eq!(body["count"], 3, "{body}");
    assert_eq!(body["missing"].as_array().unwrap().len(), 5);

    let r = results(&body);
    assert_eq!(r.len(), ids.len());
    let status: Vec<&str> = r.iter().map(|x| x["status"].as_str().unwrap()).collect();
    assert_eq!(
        status,
        [
            "current",
            "current",
            "outdated_version",
            "retired",
            "retired",
            "retired",
            "reserved",
            "unknown"
        ]
    );
    for x in &r[..3] {
        assert_eq!(x["current_id"], a10.id.to_string());
        assert_eq!(prop(&x["feature"], "house_number"), "10");
        assert!(x.get("successor").is_none());
    }

    let successor = |x: &Value| {
        (
            x["successor"]["rule"].as_str().unwrap().to_string(),
            prop(&x["successor"]["feature"], "house_number")
                .as_str()
                .unwrap()
                .to_string(),
        )
    };
    assert_eq!(successor(&r[3]), ("same_number".into(), "32".into()));
    assert_eq!(r[3]["record"]["box_number"], "B");
    assert_eq!(r[3]["record"]["end"], "2026-01-01T00:00:00Z");
    assert!(r[3].get("feature").is_none());
    assert_eq!(successor(&r[4]), ("same_number_box".into(), "40".into()));
    assert_eq!(successor(&r[5]), ("same_position".into(), "50".into()));
    assert_eq!(r[5]["successor"]["distance_m"], 0.0);
    assert_eq!(r[6]["record"]["status"], "reserved");
    assert!(r[6].get("successor").is_none());
    assert!(r[7].get("record").is_none() && r[7].get("feature").is_none());
}

#[tokio::test]
async fn without_history_old_ids_are_unknown() {
    let current = fixtures::louise_street();
    let gone = gone(&current);
    let app = App::new(&current);
    assert!(app.state.history.is_empty());
    let body = app
        .ok(&format!("/lookup?id={}", enc(&gone[0].id.to_string())))
        .await;
    assert_eq!(results(&body)[0]["status"], "unknown");
}

#[tokio::test]
async fn batch_best_id_column_resolves_old_ids() {
    use crabfinder_test_support::{col, parse_csv};
    let current = fixtures::louise_street();
    let gone = gone(&current);
    let app = App::with_history(&current, &gone);
    let a10 = current.iter().find(|a| a.house_number == "10").unwrap();
    let a32 = current.iter().find(|a| a.house_number == "32").unwrap();
    let csv = format!(
        "id,best_id,street,house_number,postal_code\n\
         1,{},,,\n\
         2,{},,,\n\
         3,{},,,\n\
         4,nope/1/1,Avenue Louise,45,1050\n\
         5,,Avenue Louise,13,1050\n",
        a10.id.0.object_key(),
        gone[0].id,
        gone[3].id,
    );
    let (status, _, body) = app
        .post("/batch/geocode?corrections=true", "text/csv", csv)
        .await;
    assert_eq!(status, 200, "{body}");
    let (h, rows) = parse_csv(&body);
    assert_eq!(h[h.len() - 2..], ["id_status", "successor_best_id"]);
    let row = |i: usize, c: &str| col(&h, &rows[i], c).to_string();

    // A version-free id is the current address, as found.
    assert_eq!(row(0, "confidence"), "id");
    assert_eq!(row(0, "id_status"), "current");
    assert_eq!(row(0, "best_id"), a10.id.to_string());
    assert_eq!(row(0, "status"), "valid");
    assert_eq!(row(0, "successor_best_id"), "");

    // A retired id becomes its successor.
    assert_eq!(row(1, "id_status"), "retired");
    assert_eq!(row(1, "matched_house_number"), "32");
    assert_eq!(row(1, "successor_best_id"), a32.id.to_string());
    assert_eq!(row(1, "status"), "corrected");
    assert!(row(1, "corrections").starts_with("id_replaced:best_id:"));

    // No successor and nothing else to go by: not found.
    assert_eq!(row(2, "id_status"), "reserved");
    assert_eq!(row(2, "confidence"), "not_found");

    // An unknown id falls back to the address columns.
    assert_eq!(row(3, "id_status"), "unknown");
    assert_eq!(row(3, "confidence"), "exact");
    assert_eq!(row(3, "matched_house_number"), "45");

    // Rows without an id keep empty id columns.
    assert_eq!(row(4, "id_status"), "");
    assert_eq!(row(4, "confidence"), "exact");

    // Without any best_id the columns don't appear.
    let (_, _, body) = app
        .post(
            "/batch/geocode",
            "text/csv",
            "id,street,house_number,postal_code\n1,Avenue Louise,13,1050\n",
        )
        .await;
    let (h, _) = parse_csv(&body);
    assert!(!h.iter().any(|c| c == "id_status"), "{h:?}");

    // An id-only file is accepted.
    let (status, _, body) = app
        .post(
            "/batch/geocode",
            "text/csv",
            format!("id,best_id\n1,{}\n", a10.id),
        )
        .await;
    assert_eq!(status, 200, "{body}");
    let (h, rows) = parse_csv(&body);
    assert_eq!(col(&h, &rows[0], "id_status"), "current");
}
