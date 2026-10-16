//! `GET /autocomplete` (v3 Phase 11.5): suggestions while typing from the
//! in-memory name index. Streets by any prefix of their name or of a later
//! word, BeSt localities, the street's buildings once a number is typed,
//! and OSM landmarks (`--pois`) only as a way to the nearest BeSt address.

mod common;

use axum::http::StatusCode;
use common::{App, enc, features, prop};
use crabfinder_core::{Address, Region};
use crabfinder_service::LoadOptions;
use crabfinder_test_support::AddressBuilder;
use serde_json::Value;

const NO_PLACES: &str = "osm_id,place,name,name_fr,name_nl,name_de,lat,lon\n";

const POIS: &str = "osm_type,osm_id,kind,name,name_fr,name_nl,name_de,name_en,lat,lon
node,240866019,tourism=attraction,Manneken Pis,Manneken-Pis,Manneken Pis,Manneken Pis,Manneken Pis,50.8449861,4.3499932
";

fn brussels(b: AddressBuilder, pc: &str) -> AddressBuilder {
    b.municipality("21004", Some("Bruxelles"), Some("Brussel"), None)
        .postal(pc)
}

fn walloon(b: AddressBuilder, nis: &str, name: &str, pc: &str) -> AddressBuilder {
    b.municipality(nis, Some(name), None, None)
        .postal(pc)
        .region(Region::Wallonia)
}

/// Rue de la Loi 16 (two boxes) / 18 / 20 / 116 in 1000, Avenue Louise in
/// 1050, Rue du 11 Novembre in 1040, a Rue de la Station in both Charleroi
/// and La Louvière, a Flemish Stationsstraat, a German-speaking Aachener
/// Straße, and the building next to the Manneken-Pis.
fn corpus() -> Vec<Address> {
    let loi = |oid: &str, n: &str, lat: f64| {
        brussels(AddressBuilder::new(oid, "100", n), "1000")
            .street(Some("Rue de la Loi"), Some("Wetstraat"))
            .at(lat, 4.3700)
    };
    let mut v = vec![
        loi("l16a", "16", 50.8460).box_no("1").build(),
        loi("l16b", "16", 50.8460).box_no("2").build(),
        loi("l18", "18", 50.8461).build(),
        loi("l20", "20", 50.8462).build(),
        loi("l116", "116", 50.8470).build(),
        AddressBuilder::new("al10", "3919", "10")
            .street(Some("Avenue Louise"), Some("Louizalaan"))
            .municipality("21009", Some("Ixelles"), Some("Elsene"), None)
            .postal("1050")
            .at(50.8355, 4.3567)
            .build(),
        AddressBuilder::new("n5", "200", "5")
            .street(Some("Rue du 11 Novembre"), Some("11 Novemberstraat"))
            .municipality("21005", Some("Etterbeek"), Some("Etterbeek"), None)
            .postal("1040")
            .at(50.8330, 4.3890)
            .build(),
        brussels(AddressBuilder::new("e46", "600", "46"), "1000")
            .street(Some("Rue de l'Étuve"), Some("Stoofstraat"))
            .at(50.8451, 4.3501)
            .build(),
        AddressBuilder::new("st1", "300", "1")
            .street(None, Some("Stationsstraat"))
            .municipality("44021", None, Some("Gent"), None)
            .postal("9000")
            .region(Region::Flanders)
            .at(51.0500, 3.7300)
            .build(),
        AddressBuilder::new("aa1", "400", "1")
            .street(None, None)
            .street_de("Aachener Straße")
            .municipality("63023", None, None, Some("Eupen"))
            .postal("4700")
            .region(Region::Wallonia)
            .at(50.6280, 6.0350)
            .build(),
    ];
    for (i, n) in ["1", "3", "5"].iter().enumerate() {
        v.push(
            walloon(
                AddressBuilder::new(&format!("ch{n}"), "500", n),
                "52011",
                "Charleroi",
                "6000",
            )
            .street(Some("Rue de la Station"), None)
            .at(50.4100 + i as f64 * 1e-4, 4.4400)
            .build(),
        );
    }
    v.push(
        walloon(
            AddressBuilder::new("ll2", "501", "2"),
            "55086",
            "La Louvière",
            "7100",
        )
        .street(Some("Rue de la Station"), None)
        .at(50.4790, 4.1860)
        .build(),
    );
    v
}

async fn ac(app: &App, q: &str, extra: &str) -> Value {
    app.ok(&format!("/autocomplete?q={}{extra}", enc(q))).await
}

fn labels(body: &Value) -> Vec<String> {
    features(body)
        .iter()
        .map(|f| prop(f, "label").as_str().unwrap_or("").to_string())
        .collect()
}

fn postal(f: &Value) -> String {
    prop(f, "street")["postal_code"]
        .as_str()
        .unwrap_or("")
        .to_string()
}

#[tokio::test]
async fn a_prefix_of_the_name_suggests_the_street() {
    let app = App::new(&corpus());
    let body = ac(&app, "rue de la l", "").await;
    let f = &features(&body)[0];
    assert_eq!(prop(f, "kind"), "street", "{body}");
    assert_eq!(prop(f, "label"), "Rue de la Loi, 1000 Bruxelles");
    let st = prop(f, "street");
    assert_eq!(st["postal_code"], "1000");
    assert_eq!(st["address_count"], 5);
    assert_eq!(st["street_name"]["nl"], "Wetstraat");
    // The key lists a building through `/building/{key}|{number}/units`.
    let key = st["street_key"].as_str().unwrap();
    let units = app
        .ok(&format!("/building/{}/units", enc(&format!("{key}|16"))))
        .await;
    assert_eq!(features(&units).len(), 2, "{units}");
    // A street suggestion has no single address.
    assert!(prop(f, "address").is_null());
    assert!(prop(f, "osm_hint").is_null());
    assert_eq!(f["geometry"]["type"], "Point");
    assert_eq!(body["query"], "rue de la l");
    assert_eq!(body["count"], features(&body).len());
}

#[tokio::test]
async fn a_later_word_of_the_name_finds_it_too() {
    let app = App::new(&corpus());
    for q in ["loi", "Lo", "la loi"] {
        let body = ac(&app, q, "").await;
        assert!(
            labels(&body).iter().any(|l| l.starts_with("Rue de la Loi")),
            "{q}: {body}"
        );
    }
    // "louise" ends Avenue Louise; the street is offered whole.
    let body = ac(&app, "louise", "").await;
    assert_eq!(labels(&body)[0], "Avenue Louise, 1050 Ixelles");
    // Nothing named so.
    let body = ac(&app, "zzz", "").await;
    assert_eq!(body["count"], 0);
}

#[tokio::test]
async fn the_label_follows_the_typed_language_or_lang() {
    let app = App::new(&corpus());
    let body = ac(&app, "wetst", "").await;
    assert_eq!(labels(&body)[0], "Wetstraat, 1000 Brussel", "{body}");
    let body = ac(&app, "wetst", "&lang=fr").await;
    assert_eq!(labels(&body)[0], "Rue de la Loi, 1000 Bruxelles");
    let body = ac(&app, "rue de la loi", "&lang=nl").await;
    assert_eq!(labels(&body)[0], "Wetstraat, 1000 Brussel");
    // German-only name, accents folded both ways.
    let body = ac(&app, "aachener str", "").await;
    assert_eq!(labels(&body)[0], "Aachener Straße, 4700 Eupen", "{body}");
    let body = ac(&app, "Stationsstr", "").await;
    assert_eq!(labels(&body)[0], "Stationsstraat, 9000 Gent");
}

#[tokio::test]
async fn a_house_number_lists_the_buildings() {
    let app = App::new(&corpus());
    // Numbers starting with "1", in order, one unit per building (the
    // first box of 16).
    let body = ac(&app, "Rue de la Loi 1", "").await;
    assert!(
        features(&body).iter().all(|f| prop(f, "kind") == "address"),
        "{body}"
    );
    let numbers: Vec<&str> = features(&body)
        .iter()
        .map(|f| prop(f, "address")["house_number"].as_str().unwrap())
        .collect();
    assert_eq!(numbers, ["16", "18", "116"]);
    assert_eq!(labels(&body)[0], "Rue de la Loi 16 bte 1, 1000 Bruxelles");
    // The number as typed comes first.
    let body = ac(&app, "wetstraat 18", "").await;
    let f = &features(&body)[0];
    assert_eq!(prop(f, "address")["house_number"], "18");
    assert_eq!(prop(f, "label"), "Wetstraat 18, 1000 Brussel");
    assert_eq!(prop(f, "address")["id"]["object_identifier"], "l18");
    let coords = f["geometry"]["coordinates"].as_array().unwrap();
    assert_eq!(coords[1].as_f64().unwrap(), 50.8461);
    // No such number: the street is still offered.
    let body = ac(&app, "Rue de la Loi 9", "").await;
    assert_eq!(prop(&features(&body)[0], "kind"), "street", "{body}");
}

#[tokio::test]
async fn a_number_inside_the_street_name_is_part_of_it() {
    let app = App::new(&corpus());
    for q in ["Rue du 11 Novembre", "rue du 11 nov", "11 novemberstr"] {
        let body = ac(&app, q, "").await;
        let f = &features(&body)[0];
        assert_eq!(prop(f, "kind"), "street", "{q}: {body}");
        assert_eq!(postal(f), "1040", "{q}");
    }
    // Its building, once the number follows the whole name (and a postal
    // code after it).
    for q in ["Rue du 11 Novembre 5", "rue du 11 novembre 5 1040"] {
        let body = ac(&app, q, "").await;
        let f = &features(&body)[0];
        assert_eq!(prop(f, "kind"), "address", "{q}: {body}");
        assert_eq!(prop(f, "address")["house_number"], "5");
    }
    let body = ac(&app, "rue du 11 novembre 5 1000", "").await;
    assert!(
        features(&body).iter().all(|f| prop(f, "kind") != "address"),
        "{body}"
    );
}

#[tokio::test]
async fn place_words_and_postal_codes_narrow_the_streets() {
    let app = App::new(&corpus());
    let body = ac(&app, "rue de la station", "").await;
    let mut both: Vec<String> = features(&body).iter().map(postal).collect();
    both.sort();
    assert_eq!(both, ["6000", "7100"], "{body}");

    for (q, pc) in [
        ("rue de la station charl", "6000"),
        ("rue de la station la louv", "7100"),
        ("rue de la station 7100", "7100"),
        ("6000 rue de la stat", "6000"),
    ] {
        let body = ac(&app, q, "").await;
        let got: Vec<String> = features(&body).iter().map(postal).collect();
        assert_eq!(got, [pc], "{q}: {body}");
    }
    let body = ac(&app, "rue de la station", "&postal_code=7100").await;
    let got: Vec<String> = features(&body).iter().map(postal).collect();
    assert_eq!(got, ["7100"]);
    // A place where the street isn't: the street words alone are kept
    // only when they name nothing else.
    let body = ac(&app, "rue de la station gent", "").await;
    assert!(
        features(&body).iter().all(|f| postal(f) != "6000"),
        "{body}"
    );
}

#[tokio::test]
async fn focus_point_puts_the_nearer_street_first() {
    let app = App::new(&corpus());
    for (focus, pc) in [("50.48,4.19", "7100"), ("50.41,4.44", "6000")] {
        let body = ac(&app, "rue de la station", &format!("&focus_point={focus}")).await;
        assert_eq!(postal(&features(&body)[0]), pc, "{focus}: {body}");
    }
}

#[tokio::test]
async fn bad_parameters_are_rejected() {
    let app = App::new(&corpus());
    for extra in [
        "&limit=0",
        "&limit=21",
        "&focus_point=abc",
        "&focus_point=0,0",
        "&postal_code=12",
        "&postal_code=10a0",
        "&lang=en",
    ] {
        let (status, body) = app.get(&format!("/autocomplete?q=loi{extra}")).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{extra}: {body}");
    }
    let (status, _) = app.get("/autocomplete").await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    // `limit` caps the list; nothing to suggest from a lone number.
    let body = ac(&app, "r", "&limit=1").await;
    assert_eq!(body["count"], 1);
    let body = ac(&app, "16", "").await;
    assert_eq!(body["count"], 0);
    let body = ac(&app, "  ", "").await;
    assert_eq!(body["count"], 0);
}

#[tokio::test]
async fn disabled_autocomplete_answers_503() {
    let app = App::with_options(&corpus(), |o| LoadOptions {
        autocomplete: false,
        ..o
    });
    let (status, body) = app.get("/autocomplete?q=loi").await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert!(body.to_string().contains("--no-autocomplete"), "{body}");
    // The rest of the service is unaffected.
    let body = app.ok("/geocode?q=Rue%20de%20la%20Loi%2018").await;
    assert_eq!(prop(&features(&body)[0], "house_number"), "18");
}

#[tokio::test]
async fn best_localities_are_suggested() {
    let app = App::with_places(&corpus(), NO_PLACES);
    let body = ac(&app, "charler", "").await;
    let f = &features(&body)[0];
    assert_eq!(prop(f, "kind"), "locality", "{body}");
    assert_eq!(prop(f, "label"), "Charleroi");
    let loc = prop(f, "locality");
    assert_eq!(loc["kind"], "municipality");
    assert_eq!(loc["postal_codes"][0], "6000");
    assert_eq!(loc["address_count"], 3);
    // Positioned on a BeSt address of the place.
    assert_eq!(prop(f, "address")["postal_code"], "6000");
    assert!(prop(f, "osm_hint").is_null());
    // A place word after it narrows nothing: the street wins in its town.
    let body = ac(&app, "rue de la station charleroi", "").await;
    assert_eq!(postal(&features(&body)[0]), "6000", "{body}");

    // A part of municipality named like its municipality, away from the
    // other buildings: one "Charleroi" suggestion.
    let mut addrs = corpus();
    addrs.push(
        walloon(
            AddressBuilder::new("chp", "502", "9"),
            "52011",
            "Charleroi",
            "6000",
        )
        .street(Some("Rue du Centre"), None)
        .part_of_municipality("9", Some("Charleroi"), None)
        .at(50.4500, 4.5000)
        .build(),
    );
    let app = App::with_places(&addrs, NO_PLACES);
    let body = ac(&app, "charleroi", "").await;
    let n = labels(&body).iter().filter(|l| *l == "Charleroi").count();
    assert_eq!(n, 1, "{body}");
}

#[tokio::test]
async fn an_osm_landmark_leads_to_the_nearest_best_address() {
    let app = App::with_pois(&corpus(), NO_PLACES, POIS);
    let body = ac(&app, "manneken", "").await;
    let f = &features(&body)[0];
    assert_eq!(prop(f, "kind"), "landmark", "{body}");
    assert_eq!(prop(f, "osm_hint"), true);
    // Everything returned is BeSt's: the address next to it, its label.
    let a = prop(f, "address");
    assert_eq!(a["id"]["object_identifier"], "e46");
    assert_eq!(prop(f, "label"), "Rue de l'Étuve 46, 1000 Bruxelles");
    let coords = f["geometry"]["coordinates"].as_array().unwrap();
    assert_eq!(coords[0].as_f64().unwrap(), 4.3501);
    // Without --pois the landmark is unknown.
    let app = App::new(&corpus());
    let body = ac(&app, "manneken", "").await;
    assert_eq!(body["count"], 0, "{body}");
}

/// More than `COMMON_KEYS` (2000) street names under one prefix: the
/// precomputed best lists answer, with the focus cells around the point.
#[tokio::test]
async fn a_very_common_prefix_uses_the_precomputed_lists() {
    let letters: Vec<char> = ('a'..='z').collect();
    let mut addrs = Vec::new();
    for i in 0..2_100usize {
        let name = format!(
            "Rue B{}{}{}",
            letters[i / 676 % 26],
            letters[i / 26 % 26],
            letters[i % 26]
        );
        // Half near Brussels, half near Liège; one building per street.
        let (nis, town, pc, lat, lon) = if i % 2 == 0 {
            ("21004", "Bruxelles", "1000", 50.85, 4.35)
        } else {
            ("62063", "Liège", "4000", 50.63, 5.57)
        };
        addrs.push(
            AddressBuilder::new(&format!("x{i}"), &format!("{}", 10_000 + i), "1")
                .street(Some(&name), None)
                .municipality(nis, Some(town), None, None)
                .postal(pc)
                .at(lat + i as f64 * 1e-6, lon)
                .build(),
        );
    }
    let app = App::new(&addrs);
    let n = app.state.suggest.n_common();
    assert!(n > 0, "no common prefix");

    let body = ac(&app, "rue b", "&limit=20").await;
    assert_eq!(body["count"], 20);
    for (focus, pc) in [("50.63,5.57", "4000"), ("50.85,4.35", "1000")] {
        let body = ac(&app, "rue b", &format!("&limit=20&focus_point={focus}")).await;
        assert!(
            features(&body).iter().all(|f| postal(f) == pc),
            "{focus}: {body}"
        );
    }
    // A postal code narrows: the full key stream, that code only.
    let body = ac(&app, "rue b", "&limit=20&postal_code=4000").await;
    assert_eq!(body["count"], 20);
    assert!(features(&body).iter().all(|f| postal(f) == "4000"));
    // The exact key is found even under a common prefix.
    let body = ac(&app, "rue baab", "").await;
    assert_eq!(labels(&body)[0], "Rue Baab, 4000 Liège", "{body}");
}
