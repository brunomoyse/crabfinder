//! Ranking signals (v2 Phase 10): a street named exactly what was typed
//! beats streets that only contain the words.

mod common;

use common::{App, enc, features, prop};
use crabfinder_core::{Address, Region};
use crabfinder_test_support::AddressBuilder;

fn flemish(oid: &str, street_oid: &str, hn: &str, street: &str, muni: &str, pc: &str) -> Address {
    AddressBuilder::new(oid, street_oid, hn)
        .region(Region::Flanders)
        .street(None, Some(street))
        .municipality(pc, None, Some(muni), None)
        .postal(pc)
        .at(51.0, 4.0)
        .build()
}

/// "Markt" (one address) and the more populous "Grote Markt" and "Oude
/// Kerkstraat" / "Kerkstraat" pairs, so popularity alone would pick the
/// longer names.
fn corpus() -> Vec<Address> {
    let mut out = vec![flemish("m1", "1", "1", "Markt", "Brugge", "8000")];
    for n in 1..=6 {
        out.push(flemish(
            &format!("g{n}"),
            "2",
            &n.to_string(),
            "Grote Markt",
            "Antwerpen",
            "2000",
        ));
        out.push(flemish(
            &format!("o{n}"),
            "3",
            &n.to_string(),
            "Oude Kerkstraat",
            "Gent",
            "9000",
        ));
    }
    out.push(flemish("k1", "4", "1", "Kerkstraat", "Gent", "9000"));
    out
}

async fn top_street(app: &App, q: &str) -> String {
    let body = app.ok(&format!("/geocode?q={}&limit=5", enc(q))).await;
    prop(&features(&body)[0], "street_name")["nl"]
        .as_str()
        .unwrap()
        .to_string()
}

#[tokio::test]
async fn an_exact_street_name_outranks_a_longer_one() {
    let app = App::new(&corpus());
    assert_eq!(top_street(&app, "Markt").await, "Markt");
    assert_eq!(top_street(&app, "Grote Markt").await, "Grote Markt");
    // A leading run of the words counts: the municipality may follow.
    assert_eq!(top_street(&app, "Kerkstraat Gent").await, "Kerkstraat");
    assert_eq!(
        top_street(&app, "Oude Kerkstraat Gent").await,
        "Oude Kerkstraat"
    );
}
