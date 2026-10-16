//! OSM landmarks (v2 Phase 10, `--pois`): a query naming a landmark and no
//! street answers with the BeSt addresses around it, `confidence:
//! landmark`. Nothing OSM is returned but the echo of how the query was
//! understood.

mod common;

use common::{App, enc, features, prop};
use crabfinder_core::Address;
use crabfinder_test_support::{AddressBuilder, col, parse_csv};

const NO_PLACES: &str = "osm_id,place,name,name_fr,name_nl,name_de,lat,lon\n";

const POIS: &str = "osm_type,osm_id,kind,name,name_fr,name_nl,name_de,name_en,lat,lon
node,240866019,tourism=attraction,Manneken Pis,Manneken-Pis,Manneken Pis,Manneken Pis,Manneken Pis,50.8449861,4.3499932
relation,19189218,tourism=attraction,Atomium,Atomium,Atomium,,Atomium,50.8949213,4.3415005
node,4878787366,railway=station,Gare du Midi - Zuidstation,Gare du Midi,Zuidstation,,,50.8364862,4.3378960
way,493778720,historic=castle,Gravensteen,,,,,51.0573,3.7207
way,1,amenity=place_of_worship,Gravensteen,,,,,51.2,4.4
way,182727710,tourism=attraction,Belfort,Beffroi,Belfort,Belfried,Belfry,51.2082320,3.2247852
way,114133990,man_made=tower,Belfort,,,,,50.8275651,3.2656095
way,121629346,historic=castle,Citadelle de Namur,,,,Citadel of Namur,50.4593511,4.8626502
";

fn brussels(b: AddressBuilder, pc: &str) -> AddressBuilder {
    b.municipality("21004", Some("Bruxelles"), Some("Brussel"), None)
        .postal(pc)
}

fn flemish(b: AddressBuilder, nis: &str, name: &str, pc: &str) -> AddressBuilder {
    b.municipality(nis, None, Some(name), None)
        .postal(pc)
        .region(crabfinder_core::Region::Flanders)
}

/// Buildings next to each landmark except the Namur citadel (the nearest,
/// in Namur, is 1 km away), plus a "Square de l'Atomium", a "Galerie
/// Ravenstein" one typo away from "Gravensteen", and a "Rue de la Gare" in
/// Namur for the OR tier to latch onto.
fn corpus() -> Vec<Address> {
    vec![
        brussels(AddressBuilder::new("e46", "600", "46"), "1000")
            .street(Some("Rue de l'Étuve"), Some("Stoofstraat"))
            .at(50.8451, 4.3501)
            .build(),
        brussels(AddressBuilder::new("c20", "601", "20"), "1020")
            .street(Some("Boulevard du Centenaire"), Some("Eeuwfeestlaan"))
            .at(50.8952, 4.3420)
            .build(),
        brussels(AddressBuilder::new("sa1", "602", "1"), "1020")
            .street(Some("Square de l'Atomium"), Some("Atomiumsquare"))
            .at(50.8960, 4.3400)
            .build(),
        AddressBuilder::new("f2", "603", "2")
            .street(Some("Rue de France"), Some("Frankrijkstraat"))
            .municipality("21001", Some("Anderlecht"), Some("Anderlecht"), None)
            .postal("1070")
            .at(50.8366, 4.3380)
            .build(),
        flemish(
            AddressBuilder::new("v1", "604", "1"),
            "44021",
            "Gent",
            "9000",
        )
        .street(None, Some("Sint-Veerleplein"))
        .at(51.0571, 3.7210)
        .build(),
        flemish(
            AddressBuilder::new("m1", "605", "1"),
            "31005",
            "Brugge",
            "8000",
        )
        .street(None, Some("Markt"))
        .at(51.2085, 3.2250)
        .build(),
        flemish(
            AddressBuilder::new("k1", "606", "1"),
            "34022",
            "Kortrijk",
            "8500",
        )
        .street(None, Some("Grote Markt"))
        .at(50.8278, 3.2650)
        .build(),
        brussels(AddressBuilder::new("r1", "608", "1"), "1000")
            .street(Some("Galerie Ravenstein"), Some("Ravensteingalerij"))
            .at(50.8445, 4.3570)
            .build(),
        AddressBuilder::new("g10", "607", "10")
            .street(Some("Rue de la Gare"), None)
            .municipality("92094", Some("Namur"), None, None)
            .postal("5000")
            .region(crabfinder_core::Region::Wallonia)
            .at(50.4680, 4.8620)
            .build(),
    ]
}

fn app() -> App {
    App::with_pois(&corpus(), NO_PLACES, POIS)
}

async fn geocode(app: &App, q: &str) -> serde_json::Value {
    app.ok(&format!("/geocode?q={}&limit=3", enc(q))).await
}

fn top(body: &serde_json::Value) -> serde_json::Value {
    features(body)
        .first()
        .cloned()
        .unwrap_or(serde_json::Value::Null)
}

#[tokio::test]
async fn a_landmark_name_answers_with_the_buildings_around_it() {
    let app = app();
    let body = geocode(&app, "Manneken Pis").await;
    let f = top(&body);
    assert_eq!(f["confidence"], "landmark", "{body}");
    assert_eq!(f["fallback_reason"], "street_missing");
    assert_eq!(f["match_type"], "spatial");
    assert_eq!(prop(&f, "house_number"), "46");
    let echo = &body["locality"];
    assert_eq!(echo["kind"], "landmark");
    assert_eq!(echo["name"]["fr"], "Manneken-Pis");
    // The landmark is only how the query was understood: no OSM position
    // is returned, the features are BeSt addresses.
    assert_eq!(echo["source"], "osm");
    assert!(echo.get("coordinates").is_none(), "{echo}");

    // Any of its names, and either side of a bilingual name.
    for q in ["manneken-pis", "Zuidstation", "gare du midi"] {
        let f = top(&geocode(&app, q).await);
        assert_eq!(f["confidence"], "landmark", "{q}: {f}");
    }
    let f = top(&geocode(&app, "Gare du Midi").await);
    assert_eq!(
        prop(&f, "postal_code"),
        "1070",
        "not Namur's Rue de la Gare"
    );
}

#[tokio::test]
async fn a_street_of_the_same_name_wins() {
    let app = app();
    let f = top(&geocode(&app, "Atomium").await);
    assert_eq!(f["confidence"], "median_street", "{f}");
    assert_eq!(prop(&f, "street_name")["fr"], "Square de l'Atomium");
}

#[tokio::test]
async fn a_house_number_or_far_postal_code_rules_the_landmark_out() {
    let app = app();
    let f = top(&geocode(&app, "Manneken Pis 5").await);
    assert_ne!(f["confidence"], "landmark", "{f}");
    // 9000 is ~50 km from Manneken Pis.
    let f = top(&geocode(&app, "Manneken Pis 9000").await);
    assert_ne!(f["confidence"], "landmark", "{f}");
    // An unknown postal code vouches for nothing.
    let f = top(&geocode(&app, "Manneken Pis 1999").await);
    assert_ne!(f["confidence"], "landmark", "{f}");
    // 1000 is next to it; the building found may lie in another code.
    let f = top(&geocode(&app, "Manneken Pis 1000").await);
    assert_eq!(f["confidence"], "landmark", "{f}");
    // No building within reach of the citadel: the regular tiers answer.
    let f = top(&geocode(&app, "Citadelle de Namur").await);
    assert_ne!(f["confidence"], "landmark", "{f}");
}

#[tokio::test]
async fn namesakes_are_told_apart_by_rank_locality_and_focus() {
    let app = app();
    // A castle before a church of the same name — and before a
    // typo-corrected street (Galerie Ravenstein).
    let f = top(&geocode(&app, "Gravensteen").await);
    assert_eq!(f["confidence"], "landmark", "{f}");
    assert_eq!(prop(&f, "postal_code"), "9000", "{f}");
    // A real typo still reaches the street.
    let f = top(&geocode(&app, "Galerie Ravenstien").await);
    assert_eq!(f["confidence"], "fuzzy_street", "{f}");
    // "<landmark> <locality>".
    let f = top(&geocode(&app, "Gravensteen Gent").await);
    assert_eq!(f["confidence"], "landmark", "{f}");
    assert_eq!(prop(&f, "postal_code"), "9000");
    // An attraction before a tower, unless the focus point is nearer the
    // tower.
    let f = top(&geocode(&app, "Belfort").await);
    assert_eq!(prop(&f, "postal_code"), "8000", "{f}");
    let body = app.ok("/geocode?q=Belfort&focus_point=50.83,3.26").await;
    assert_eq!(prop(&top(&body), "postal_code"), "8500", "{body}");
    // A bbox keeps only the landmarks inside it.
    let body = app.ok("/geocode?q=Belfort&bbox=3.2,50.8,3.3,50.9").await;
    assert_eq!(prop(&top(&body), "postal_code"), "8500", "{body}");
}

#[tokio::test]
async fn without_pois_landmarks_are_unknown() {
    let app = App::with_places(&corpus(), NO_PLACES);
    let f = top(&geocode(&app, "Manneken Pis").await);
    assert_ne!(f["confidence"], "landmark", "{f}");
}

#[tokio::test]
async fn batch_rows_are_the_nearest_best_address() {
    let app = app();
    let csv = "id,address\n1,Manneken Pis\n2,Gravensteen Gent\n";
    let (status, _, body) = app.post("/batch/geocode", "text/csv", csv).await;
    assert_eq!(status, 200, "{body}");
    let (h, rows) = parse_csv(&body);
    assert_eq!(col(&h, &rows[0], "confidence"), "landmark");
    assert_eq!(col(&h, &rows[0], "fallback_reason"), "street_missing");
    // Everything is BeSt's: the address next to Manneken Pis, its id and
    // its position — not the OSM node.
    assert_eq!(
        col(&h, &rows[0], "label"),
        "Rue de l'Étuve 46, 1000 Bruxelles"
    );
    assert_eq!(col(&h, &rows[0], "lat"), "50.8451");
    assert_eq!(col(&h, &rows[0], "matched_house_number"), "46");
    assert_ne!(col(&h, &rows[0], "best_id"), "");
    assert_eq!(col(&h, &rows[1], "label"), "Sint-Veerleplein 1, 9000 Gent");
}
