//! The benchmark end to end (v3 Phase 11.1): crabfinder over a tiny
//! `TestIndex` next to a mock "Brussels regional geocoder" on loopback,
//! through `cli::run_with`; plus the HTTP adapter runner's retries,
//! fallback, cache and failure handling.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use axum::Router;
use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::routing::get;
use clap::Parser;
use crabfinder_core::{Address, Coordinates};
use crabfinder_eval::adapters::brussels::Brussels;
use crabfinder_eval::adapters::{Geocoder, HttpGeocoder};
use crabfinder_eval::benchmark::BenchmarkReport;
use crabfinder_eval::cli::{Args, run_with};
use crabfinder_eval::eval_set::{EvalSet, flatten};
use crabfinder_service::{AppState, LoadOptions, http};
use crabfinder_test_support::{TestIndex, fixtures};
use tempfile::TempDir;

fn serve(router: Router) -> String {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let addr = listener.local_addr().unwrap();
    std::thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async move {
            let l = tokio::net::TcpListener::from_std(listener).unwrap();
            axum::serve(l, router).await.unwrap();
        });
    });
    format!("http://{addr}")
}

/// Rue de la Loi 16 as the Brussels service answers it (Lambert 72).
const LOI_16: &str = r#"{"resultsList":[{"objectType":"Address","result":[{"score":29,"address":{
  "code":{"namespace":"https://databrussels.be/id/address","objectIdentifier":"210225","versionIdentifier":"1"},
  "houseNumber":"16","position":{"pointGeometry":{"point":{"pos":[{"value":[149843.389,170639.759]}]}}},
  "hasStreetName":{"namespace":"https://databrussels.be/id/streetname","objectIdentifier":"1679","versionIdentifier":"1"}}}]}]}"#;
const EMPTY: &str = r#"{"resultsList":[]}"#;

#[derive(Default)]
struct Mock {
    hits: AtomicUsize,
    flaky: AtomicUsize,
}

/// Knows Rue de la Loi 16 in French only; "flaky" fails once with 503;
/// "broken" always answers 500; "teapot" answers 418.
fn mock() -> (String, Arc<Mock>) {
    let m = Arc::new(Mock::default());
    async fn answer(
        State(m): State<Arc<Mock>>,
        Query(p): Query<std::collections::HashMap<String, String>>,
    ) -> axum::response::Response {
        m.hits.fetch_add(1, Ordering::SeqCst);
        let text = p
            .get("freeText")
            .or(p.get("streetName"))
            .cloned()
            .unwrap_or_default();
        let lang = p.get("searchLanguage").map(String::as_str);
        if text.contains("broken") {
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        }
        if text.contains("teapot") {
            return StatusCode::IM_A_TEAPOT.into_response();
        }
        if text.contains("flaky") && m.flaky.fetch_add(1, Ordering::SeqCst) == 0 {
            return StatusCode::SERVICE_UNAVAILABLE.into_response();
        }
        if text.contains("garbage") {
            return "not json".into_response();
        }
        let known = text.contains("Loi") || text.contains("flaky");
        let body = if known && lang == Some("fr") {
            LOI_16
        } else {
            EMPTY
        };
        body.into_response()
    }
    let router = Router::new()
        .route("/searchAddress", get(answer))
        .route("/freeSearch/searchAnyObject", get(answer))
        .with_state(m.clone());
    (serve(router), m)
}

fn args(argv: &[&str]) -> Args {
    let mut full = vec!["crabfinder-eval"];
    full.extend_from_slice(argv);
    Args::try_parse_from(full).expect("valid argv")
}

fn s(p: &Path) -> &str {
    p.to_str().unwrap()
}

fn write(dir: &Path, name: &str, body: &str) -> PathBuf {
    let p = dir.join(name);
    fs::write(&p, body).unwrap();
    p
}

fn point(a: &Address) -> (f64, f64) {
    match a.coordinates {
        Coordinates::Wgs84(w) => (w.lat, w.lon),
        Coordinates::Lambert72(_) => panic!("fixtures are WGS84"),
    }
}

#[test]
fn benchmark_compares_crabfinder_with_a_regional_geocoder() {
    let mut addrs = fixtures::brussels_small();
    addrs.extend(fixtures::multi_region());
    let ti = TestIndex::build(&addrs);
    let crab = serve(http::router(
        AppState::load(&LoadOptions::new(ti.path())).unwrap(),
    ));
    let (bru, m) = mock();
    let tmp = TempDir::new().unwrap();

    let loi = addrs
        .iter()
        .find(|a| a.street_name.fr.as_deref() == Some("Rue de la Loi") && a.house_number == "16")
        .unwrap();
    let flemish = addrs
        .iter()
        .find(|a| a.region == crabfinder_core::Region::Flanders)
        .unwrap();
    // An address-level adversarial entry without a point (filled by
    // --with-points) and two real-world entries.
    let adv = write(
        tmp.path(),
        "adv.yaml",
        &format!(
            r#"version: t
queries:
  - id: adv-cross_language-001
    category: cross_language
    subcategory: x
    query: "Rue de la Loi 16 1000 Bruxelles"
    expected_best_id: "{}"
    expected_building_group_id: "{}"
"#,
            loi.id, loi.building_group_id.0
        ),
    );
    let (lat, lon) = point(flemish);
    let (loi_lat, loi_lon) = crabfinder_eval::adapters::lambert72(149843.389, 170639.759).unwrap();
    let rw = write(
        tmp.path(),
        "rw.yaml",
        &format!(
            r#"version: t
kind: realworld
queries:
  - id: rw-bru-1
    source: brussels-schools
    region: brussels
    query: "Rue de la Loi 16, 1000 Bruxelles"
    street: "Rue de la Loi"
    house_number: "16"
    postal_code: "1000"
    city: "Bruxelles"
    expected_point: [{loi_lat}, {loi_lon}]
  - id: rw-vla-1
    source: flanders-schools
    region: flanders
    query: "somewhere in flanders"
    expected_point: [{lat}, {lon}]
"#
        ),
    );
    let out = tmp.path().join("reports/bench.json");
    let cache = tmp.path().join("cache");
    let md_path = tmp.path().join("reports/bench.md");
    let bench = |extra: &[&str]| {
        let mut argv = vec![
            "--benchmark",
            "--eval-set",
            s(&adv),
            "--eval-set",
            s(&rw),
            "--service",
            &crab,
            "--with-points",
            "--bench-cache",
            s(&cache),
            "--output",
            s(&out),
            "--output-md",
            s(&md_path),
        ];
        argv.extend_from_slice(extra);
        run_with(&args(&argv))
    };
    let crab_spec = format!("crabfinder={crab}");
    let bru_spec = format!("bru:brussels={bru}");
    bench(&[
        "--geocoder",
        &crab_spec,
        "--geocoder",
        &bru_spec,
        "--rate",
        "bru=1000",
    ])
    .expect("benchmark");

    let r: BenchmarkReport = serde_json::from_str(&fs::read_to_string(&out).unwrap()).unwrap();
    assert_eq!(r.query_count, 3);
    assert_eq!(
        r.with_reference_point, 3,
        "--with-points filled the adversarial one"
    );
    let (c, b) = (&r.geocoders[0], &r.geocoders[1]);
    assert_eq!((c.name.as_str(), c.covered), ("crabfinder", 3));
    assert!(c.returns_best_ids);
    assert_eq!(c.metrics.distance.n, 3);
    // Brussels only: the Flemish entry isn't asked.
    assert_eq!(
        (b.name.as_str(), b.kind.as_str(), b.covered),
        ("bru", "brussels", 2)
    );
    assert_eq!(b.metrics.distance.n, 2);
    let d = &b.metrics.distance_by["realworld:brussels-schools"];
    assert_eq!(d.within_25m, 1.0, "{d:?}");
    assert_eq!(r.head_to_head[0].other, "bru");
    assert!(tmp.path().join("reports/bench-detail.json.gz").exists());
    let md = fs::read_to_string(tmp.path().join("reports/bench.md")).unwrap();
    assert!(md.contains("## Head to head (crabfinder"), "{md}");

    // A rerun is served from the cache: the mock isn't asked again.
    let before = m.hits.load(Ordering::SeqCst);
    assert!(before >= 2);
    bench(&["--geocoder", &crab_spec, "--geocoder", &bru_spec]).expect("rerun");
    assert_eq!(m.hits.load(Ordering::SeqCst), before);
    // Without the cache it is.
    bench(&["--geocoder", &bru_spec, "--no-bench-cache"]).expect("uncached");
    assert!(m.hits.load(Ordering::SeqCst) > before);
}

#[test]
fn benchmark_argument_errors() {
    let tmp = TempDir::new().unwrap();
    let rw = write(
        tmp.path(),
        "rw.yaml",
        "version: t\nkind: realworld\nqueries: []\n",
    );
    let out = tmp.path().join("o.json");
    for (argv, needle) in [
        (vec!["--benchmark"], "requires at least one --eval-set"),
        (
            vec!["--benchmark", "--eval-set", s(&rw)],
            "requires --output",
        ),
        (
            vec!["--benchmark", "--eval-set", s(&rw), "--output", s(&out)],
            "at least one --geocoder",
        ),
        (
            vec![
                "--benchmark",
                "--eval-set",
                s(&rw),
                "--output",
                s(&out),
                "--geocoder",
                "brussels",
                "--rate",
                "x",
            ],
            "expected label=N",
        ),
        (
            vec![
                "--benchmark",
                "--eval-set",
                s(&rw),
                "--output",
                s(&out),
                "--geocoder",
                "brussels",
                "--rate",
                "x=y",
            ],
            "must be a number",
        ),
        (
            vec![
                "--benchmark",
                "--eval-set",
                s(&rw),
                "--output",
                s(&out),
                "--geocoder",
                "=x",
            ],
            "bad --geocoder",
        ),
        (
            vec![
                "--benchmark",
                "--eval-set",
                s(&rw),
                "--output",
                s(&out),
                "--geocoder",
                "ngi",
            ],
            "no public endpoint",
        ),
    ] {
        let err = run_with(&args(&argv)).unwrap_err();
        assert!(format!("{err:#}").contains(needle), "{argv:?}: {err:#}");
    }
}

fn queries(texts: &[&str]) -> Vec<crabfinder_eval::eval_set::EvalQuery> {
    let yaml = format!(
        "version: t\nkind: realworld\nqueries:\n{}",
        texts
            .iter()
            .enumerate()
            .map(|(i, t)| format!(
                "  - id: q{i}\n    source: s\n    region: brussels\n    query: \"{t}\"\n    expected_point: [50.8466, 4.3669]\n"
            ))
            .collect::<String>()
    );
    let tmp = TempDir::new().unwrap();
    let p = write(tmp.path(), "q.yaml", &yaml);
    flatten(&[EvalSet::auto_load(&p).unwrap()])
}

#[tokio::test(flavor = "multi_thread")]
async fn http_runner_retries_falls_back_and_records_errors() {
    let (bru, m) = mock();
    let g = HttpGeocoder::new("bru", Brussels::new(&bru), Duration::from_secs(5))
        .unwrap()
        .with_retries(2);
    // "Wetstraat" is asked in Dutch first; the mock only knows French.
    let qs = queries(&[
        "Wetstraat Loi 16",
        "flaky Loi",
        "teapot",
        "garbage",
        "broken",
    ]);
    let mut r = g.run(qs, 5, 2).await.unwrap();
    r.sort_by(|a, b| a.query.id.cmp(&b.query.id));
    assert_eq!(r[0].points.len(), 1, "fallback to fr: {:?}", r[0]);
    assert!(
        r[1].error.is_none() && r[1].points.len() == 1,
        "retried: {:?}",
        r[1]
    );
    assert_eq!(r[2].error.as_deref(), Some("http 418"));
    assert!(
        r[3].error.as_deref().unwrap().starts_with("json:"),
        "{:?}",
        r[3]
    );
    assert_eq!(r[4].error.as_deref(), Some("http 500"), "5xx after retries");
    assert!(m.hits.load(Ordering::SeqCst) >= 8);

    // Every query failing is an error for the whole geocoder.
    let err = g.run(queries(&["broken"]), 5, 1).await.unwrap_err();
    assert!(err.to_string().contains("every query failed"), "{err}");
    // An unreachable service too (transport errors, no retries).
    let dead = HttpGeocoder::new(
        "dead",
        Brussels::new("http://127.0.0.1:9"),
        Duration::from_secs(1),
    )
    .unwrap()
    .with_retries(0);
    let err = dead.run(queries(&["x"]), 5, 1).await.unwrap_err();
    assert!(err.to_string().contains("transport"), "{err}");
}
