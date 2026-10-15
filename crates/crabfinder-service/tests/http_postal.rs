//! Postal-code relaxation (v2 Phase 10): when nothing matches inside the
//! given postal code, the street is searched around it and labelled
//! `adjacent_commune` / `postal_code_mismatch`.

mod common;

use common::{App, enc, features, prop};
use crabfinder_core::Address;
use crabfinder_test_support::{AddressBuilder, col, parse_csv};

const NO_PLACES: &str = "osm_id,place,name,name_fr,name_nl,name_de,lat,lon\n";

fn ixelles(b: AddressBuilder) -> AddressBuilder {
    b.municipality("21009", Some("Ixelles"), Some("Elsene"), None)
        .postal("1050")
}

fn saint_gilles(b: AddressBuilder) -> AddressBuilder {
    b.municipality("21013", Some("Saint-Gilles"), Some("Sint-Gillis"), None)
        .postal("1060")
}

/// Two neighbouring communes ~1.5 km apart, plus one in Ghent:
/// - Avenue Louise (one street object) crosses them: 10/20/30/50 in 1050,
///   54 in 1060;
/// - Rue Voisine exists only in 1060;
/// - Rue Paire 2/4/6 in 1050, and a namesake street with 5 in 1060;
/// - Rue Lointaine exists only in Ghent (9000), > 20 km away.
fn corpus() -> Vec<Address> {
    let mut out = Vec::new();
    for (i, n) in ["10", "20", "30", "50"].iter().enumerate() {
        out.push(
            ixelles(AddressBuilder::new(&format!("l{n}"), "500", n))
                .street(Some("Avenue Louise"), Some("Louizalaan"))
                .at(50.8300 + i as f64 * 0.0005, 4.3650)
                .build(),
        );
    }
    out.push(
        saint_gilles(AddressBuilder::new("l54", "500", "54"))
            .street(Some("Avenue Louise"), Some("Louizalaan"))
            .at(50.8250, 4.3500)
            .build(),
    );
    out.push(
        saint_gilles(AddressBuilder::new("v3", "501", "3"))
            .street(Some("Rue Voisine"), Some("Naburigestraat"))
            .at(50.8240, 4.3480)
            .build(),
    );
    for n in ["2", "4", "6"] {
        out.push(
            ixelles(AddressBuilder::new(&format!("p{n}"), "502", n))
                .street(Some("Rue Paire"), Some("Evenstraat"))
                .at(50.8310, 4.3660)
                .build(),
        );
    }
    out.push(
        saint_gilles(AddressBuilder::new("q5", "503", "5"))
            .street(Some("Rue Paire"), Some("Evenstraat"))
            .at(50.8245, 4.3490)
            .build(),
    );
    out.push(
        AddressBuilder::new("g1", "504", "1")
            .street(Some("Rue Lointaine"), Some("Verrestraat"))
            .municipality("44021", Some("Gand"), Some("Gent"), None)
            .postal("9000")
            .at(51.0540, 3.7250)
            .build(),
    );
    out
}

fn app() -> App {
    App::with_places(&corpus(), NO_PLACES)
}

async fn top(app: &App, q: &str) -> serde_json::Value {
    let body = app.ok(&format!("/geocode?q={}&limit=3", enc(q))).await;
    features(&body)
        .first()
        .cloned()
        .unwrap_or(serde_json::Value::Null)
}

#[tokio::test]
async fn a_street_missing_from_the_postal_code_is_found_next_door() {
    let app = app();
    let f = top(&app, "Rue Voisine 3 1050").await;
    assert_eq!(prop(&f, "postal_code"), "1060");
    assert_eq!(prop(&f, "house_number"), "3");
    assert_eq!(f["confidence"], "adjacent_commune");
    assert_eq!(f["fallback_reason"], "postal_code_mismatch");
    assert_eq!(f["house_number_delta"], 0);

    // Without a number: the street next door, street-level.
    let f = top(&app, "Rue Voisine 1050").await;
    assert_eq!(f["confidence"], "adjacent_commune");
    assert!(f.get("house_number_delta").is_none());
}

#[tokio::test]
async fn the_exact_number_across_the_boundary_beats_a_nearest_number() {
    let app = app();
    // 54 isn't in 1050 (nearest there: 50), but the same street object
    // carries it in 1060.
    let f = top(&app, "Avenue Louise 54 1050").await;
    assert_eq!(prop(&f, "postal_code"), "1060");
    assert_eq!(prop(&f, "house_number"), "54");
    assert_eq!(f["confidence"], "adjacent_commune");

    // A namesake street (another object) one number off doesn't: the
    // nearest number inside the postal code stays.
    let f = top(&app, "Rue Paire 5 1050").await;
    assert_eq!(prop(&f, "postal_code"), "1050");
    assert_eq!(f["confidence"], "nearest_any_side");
}

#[tokio::test]
async fn exact_matches_and_hard_filters_are_never_relaxed() {
    let app = app();
    let f = top(&app, "Avenue Louise 20 1050").await;
    assert_eq!(f["confidence"], "exact");

    // `postal_code=` is a hard filter: no adjacent commune.
    let body = app
        .ok(&format!(
            "/geocode?q={}&postal_code=1050",
            enc("Rue Voisine 3")
        ))
        .await;
    assert!(
        features(&body)
            .iter()
            .all(|f| prop(f, "postal_code") == "1050" && f["confidence"] != "adjacent_commune"),
        "{body}"
    );
}

#[tokio::test]
async fn relaxation_stays_near_a_known_postal_code() {
    let app = app();
    // Ghent is far beyond the search radius around 1050.
    let f = top(&app, "Rue Lointaine 1 1050").await;
    assert_ne!(f["confidence"], "adjacent_commune", "{f}");
    // An unknown postal code has no centre to search around.
    let f = top(&app, "Rue Voisine 3 1999").await;
    assert_ne!(f["confidence"], "adjacent_commune", "{f}");
}

#[tokio::test]
async fn batch_rows_report_the_mismatch() {
    let app = app();
    let csv = "id,street,house_number,postal_code\n1,Rue Voisine,3,1050\n2,Avenue Louise,20,1050\n";
    let (status, _, body) = app.post("/batch/geocode", "text/csv", csv).await;
    assert_eq!(status, 200, "{body}");
    let (header, rows) = parse_csv(&body);
    assert_eq!(col(&header, &rows[0], "confidence"), "adjacent_commune");
    assert_eq!(
        col(&header, &rows[0], "fallback_reason"),
        "postal_code_mismatch"
    );
    assert_eq!(col(&header, &rows[0], "matched_postal_code"), "1060");
    assert_eq!(col(&header, &rows[1], "confidence"), "exact");
}
