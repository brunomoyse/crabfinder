//! End-to-end tests of every `crabfinder-eval` mode, driven through
//! `cli::run_with` against a tiny `TestIndex` and the production
//! `http::router` served on an ephemeral loopback port (v2 Phase 7).

use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};

use axum::Router;
use axum::extract::Query;
use axum::routing::get;
use clap::Parser;
use crabfinder_core::{Address, Region};
use crabfinder_eval::cli::{Args, run_with};
use crabfinder_eval::eval_set::{EvalSet, SyntheticEvalSet, VariantType, flatten};
use crabfinder_eval::report::Report;
use crabfinder_eval::runner::{RunnerConfig, run_eval};
use crabfinder_service::{AppState, LoadOptions, http};
use crabfinder_test_support::{TestIndex, fixtures};
use tempfile::TempDir;

/// Every region, bilingual Brussels streets, a 4-digit house number.
fn corpus() -> Vec<Address> {
    let mut out = fixtures::brussels_small();
    out.extend(fixtures::louise_street());
    out.extend(fixtures::multi_region());
    out
}

/// Serve `router` on `127.0.0.1:0` from a background thread with its own
/// runtime (the CLI under test builds and blocks on its own runtime, so it
/// can't share one). Returns the base URL.
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

/// A live service over `ti`.
fn serve_index(ti: &TestIndex) -> String {
    let state = AppState::load(&LoadOptions::new(ti.path())).expect("load state");
    serve(http::router(state))
}

fn args(argv: &[&str]) -> Args {
    let mut full = vec!["crabfinder-eval"];
    full.extend_from_slice(argv);
    Args::try_parse_from(full).expect("valid argv")
}

fn s(p: &Path) -> &str {
    p.to_str().unwrap()
}

fn find<'a>(addrs: &'a [Address], street_fr_or_nl: &str, hn: &str) -> &'a Address {
    addrs
        .iter()
        .find(|a| {
            a.house_number == hn
                && [&a.street_name.fr, &a.street_name.nl, &a.street_name.de]
                    .iter()
                    .any(|n| n.as_deref() == Some(street_fr_or_nl))
        })
        .unwrap_or_else(|| panic!("no fixture {street_fr_or_nl} {hn}"))
}

/// An adversarial set over the `corpus()` fixtures: an address-level, a
/// building-level, a street-level and a locality-level entry, plus a
/// known failure.
fn adversarial_yaml(addrs: &[Address]) -> String {
    let loi = find(addrs, "Rue de la Loi", "16");
    let louise = find(addrs, "Avenue Louise", "32");
    let markt = addrs
        .iter()
        .find(|a| a.region == Region::Flanders)
        .expect("Antwerp Grote Markt");
    format!(
        r#"version: test
queries:
  - id: adv-cross_language-001
    category: cross_language
    subcategory: fr-nl
    query: "Wetstraat 16"
    expected_best_id: "{loi_id}"
    expected_building_group_id: "{loi_bgid}"
  - id: adv-notability-001
    category: notability
    subcategory: building
    match_level: building
    query: "Avenue Louise 32 1050"
    expected_building_group_id_set: ["{louise_bgid}"]
  - id: adv-special_chars-001
    category: special_chars
    subcategory: street
    match_level: street
    query: "grote markt antwerpen"
    expected_building_group_id_set: ["{markt_bgid}"]
  - id: adv-hamlet-001
    category: hamlet
    subcategory: han
    match_level: locality
    expected_point: [50.1260, 5.1880]
    query: "Han-sur-Lesse"
    expected_building_group_id_set: ["{markt_bgid}"]
  - id: adv-known_failure-001
    category: known_failure
    subcategory: nonsense
    query: "zzzz qqqq"
    expected_behavior: "nothing sensible"
    expected_to_fail: true
"#,
        loi_id = loi.id,
        loi_bgid = loi.building_group_id.0,
        louise_bgid = louise.building_group_id.0,
        markt_bgid = markt.building_group_id.0,
    )
}

fn write(dir: &Path, name: &str, body: &str) -> PathBuf {
    let p = dir.join(name);
    fs::write(&p, body).unwrap();
    p
}

fn generate(ti: &TestIndex, out: &Path, seed: &str) {
    run_with(&args(&[
        "--generate-eval-set",
        "--mvp",
        "--index",
        s(ti.path()),
        "--output",
        s(out),
        "--seed",
        seed,
        "--scale",
        "0.05",
        "--dump-date",
        "2026-10-15",
    ]))
    .expect("generate");
}

#[test]
fn generate_run_and_diff_end_to_end() {
    let addrs = corpus();
    // With a localities file, so the wrong_postal variant has postal
    // codes to pick from.
    let ti = TestIndex::build_with_places(
        &addrs,
        "osm_id,place,name,name_fr,name_nl,name_de,lat,lon\n",
    );
    let tmp = TempDir::new().unwrap();

    // Generate: reproducible by seed, all three regions sampled.
    let set_a = tmp.path().join("set-a.json");
    let set_b = tmp.path().join("set-b.json");
    generate(&ti, &set_a, "7");
    generate(&ti, &set_b, "7");
    assert_eq!(fs::read(&set_a).unwrap(), fs::read(&set_b).unwrap());
    let set = SyntheticEvalSet::load(&set_a).unwrap();
    assert_eq!(set.seed, 7);
    assert_eq!(set.corpus.dump_date, "2026-10-15");
    assert_eq!(set.corpus.doc_count, addrs.len() as u64);
    assert!(!set.queries.is_empty());
    for region in ["BE-BRU", "BE-VLG", "BE-WAL"] {
        assert!(
            set.queries.iter().any(|q| q.sample_region == region),
            "{region} not sampled"
        );
    }
    let wrong: Vec<_> = set
        .queries
        .iter()
        .filter(|q| q.variant_type == VariantType::WrongPostal)
        .collect();
    assert!(!wrong.is_empty());
    // Appended after every other variant.
    let first_wrong = set
        .queries
        .iter()
        .position(|q| q.variant_type == VariantType::WrongPostal);
    assert!(
        set.queries[first_wrong.unwrap()..]
            .iter()
            .all(|q| q.variant_type == VariantType::WrongPostal)
    );

    // Full run against the live router: synthetic + adversarial.
    let url = serve_index(&ti);
    let adv = write(tmp.path(), "adv.yaml", &adversarial_yaml(&addrs));
    let report_json = tmp.path().join("out/report.json");
    let report_md = tmp.path().join("out/report.md");
    run_with(&args(&[
        "--eval-set",
        s(&set_a),
        "--eval-set",
        s(&adv),
        "--service",
        &url,
        "--parallel",
        "4",
        "--output",
        s(&report_json),
        "--output-md",
        s(&report_md),
        "--alternatives",
    ]))
    .expect("eval run");

    let report: Report = serde_json::from_str(&fs::read_to_string(&report_json).unwrap()).unwrap();
    let m = &report.metrics;
    assert_eq!(report.service_url, url);
    assert_eq!(m.query_count, set.queries.len() + 5);
    assert_eq!(m.aggregated_query_count, m.query_count - 1);
    assert_eq!(m.error_count, 0);
    assert!(
        m.overall.recall_at_1 > 0.6,
        "recall@1 {}",
        m.overall.recall_at_1
    );
    for cat in ["cross_language", "notability", "special_chars", "hamlet"] {
        let b = &m.by_adversarial_category[cat];
        assert_eq!(b.recall_at_1, 1.0, "{cat}: {b:?}");
    }
    assert_eq!(m.known_failure_tracking.len(), 1);
    // --alternatives: the expected building is among the first groups.
    let alts = &m.alternatives["all"];
    assert_eq!(alts.n, set.queries.len());
    assert!(alts.recall_at_3 >= m.overall.recall_at_1, "{alts:?}");

    let md = fs::read_to_string(&report_md).unwrap();
    assert!(md.contains("## Alternatives by municipality"));
    assert!(md.contains("# crabfinder eval: single report"));
    assert!(md.contains("notability (building)"));
    assert!(md.contains("hamlet (locality)"));
    assert!(md.contains("adv-known_failure-001"));

    // The per-query detail sits next to the JSON, gzipped.
    let mut detail = String::new();
    flate2::read::GzDecoder::new(
        fs::File::open(tmp.path().join("out/report-detail.json.gz")).unwrap(),
    )
    .read_to_string(&mut detail)
    .unwrap();
    let rows: Vec<serde_json::Value> = serde_json::from_str(&detail).unwrap();
    assert_eq!(rows.len(), m.query_count);
    let loi = rows
        .iter()
        .find(|r| r["id"] == "adv-cross_language-001")
        .unwrap();
    assert_eq!(loi["level_rank"], 1);
    assert_eq!(loi["actual_top1_match_type"], "strict");

    // Diff: a report against itself has no deltas.
    let diff = tmp.path().join("diff.md");
    run_with(&args(&[
        "--baseline",
        s(&report_json),
        "--current",
        s(&report_json),
        "--diff-output",
        s(&diff),
    ]))
    .expect("diff");
    let d = fs::read_to_string(&diff).unwrap();
    assert!(d.contains("recall@1"), "{d}");
}

#[test]
fn dry_run_projects_without_writing_a_report() {
    let addrs = corpus();
    let ti = TestIndex::build(&addrs);
    let tmp = TempDir::new().unwrap();
    let url = serve_index(&ti);
    let adv = write(tmp.path(), "adv.yaml", &adversarial_yaml(&addrs));
    let out = tmp.path().join("never.json");
    run_with(&args(&[
        "--eval-set",
        s(&adv),
        "--service",
        &url,
        "--dry-run",
        "--output",
        s(&out),
    ]))
    .expect("dry run");
    assert!(!out.exists());
}

#[test]
fn validate_accepts_live_ids_and_rejects_stale_ones() {
    let addrs = corpus();
    let ti = TestIndex::build(&addrs);
    let tmp = TempDir::new().unwrap();
    let url = serve_index(&ti);

    let good = write(tmp.path(), "good.yaml", &adversarial_yaml(&addrs));
    run_with(&args(&[
        "--validate-eval-set",
        "--eval-set",
        s(&good),
        "--service",
        &url,
        "--max-missing-pct",
        "0",
    ]))
    .expect("every id is live");

    // A synthetic set validates the same way.
    let set = tmp.path().join("set.json");
    generate(&ti, &set, "1");
    run_with(&args(&[
        "--validate-eval-set",
        "--eval-set",
        s(&set),
        "--service",
        &url,
        "--max-missing-pct",
        "0",
    ]))
    .expect("synthetic ids are live");

    // A building that doesn't exist, and a real building with an address
    // id that isn't one of its units.
    let loi = find(&addrs, "Rue de la Loi", "16");
    let stale = write(
        tmp.path(),
        "stale.yaml",
        &format!(
            r#"version: test
queries:
  - id: adv-facilites-001
    category: facilites
    subcategory: gone
    query: "Rue Disparue 1"
    expected_building_group_id: "https://databrussels.be/id/streetname|999999|1"
  - id: adv-facilites-002
    category: facilites
    subcategory: renumbered
    query: "Rue de la Loi 16"
    expected_best_id: "https://databrussels.be/id/address/404/1"
    expected_building_group_id: "{bgid}"
  - id: adv-facilites-003
    category: facilites
    subcategory: ok
    query: "Rue de la Loi 16"
    expected_best_id: "{best}"
    expected_building_group_id: "{bgid}"
"#,
            bgid = loi.building_group_id.0,
            best = loi.id,
        ),
    );
    let err = run_with(&args(&[
        "--validate-eval-set",
        "--eval-set",
        s(&stale),
        "--service",
        &url,
    ]))
    .unwrap_err()
    .to_string();
    assert!(err.contains("2 eval set entries have stale"), "{err}");

    // Within tolerance it only warns.
    run_with(&args(&[
        "--validate-eval-set",
        "--eval-set",
        s(&stale),
        "--service",
        &url,
        "--max-missing-pct",
        "70",
    ]))
    .expect("2/3 missing is within 70%");
}

#[test]
fn batch_mode_writes_thresholds() {
    let addrs = corpus();
    let ti = TestIndex::build_with_mid_street(&addrs);
    let tmp = TempDir::new().unwrap();
    let set = tmp.path().join("set.json");
    generate(&ti, &set, "3");
    let adv = write(tmp.path(), "adv.yaml", &adversarial_yaml(&addrs));
    let out = tmp.path().join("thresholds.json");
    run_with(&args(&[
        "--batch-mode",
        "--index",
        s(ti.path()),
        "--eval-set",
        s(&set),
        "--eval-set",
        s(&adv),
        "--output",
        s(&out),
        "--no-mid-street",
        "--no-notability-boost",
    ]))
    .expect("batch mode");
    let t: serde_json::Value = serde_json::from_str(&fs::read_to_string(&out).unwrap()).unwrap();
    let n_set = SyntheticEvalSet::load(&set).unwrap().queries.len();
    assert_eq!(t["computed_from"]["n_queries"], n_set + 5);
    assert!(t["computed_from"]["n_resolved"].as_u64().unwrap() > 0);
    assert_eq!(
        t["computed_from"]["eval_set_paths"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
}

#[test]
fn missing_arguments_are_reported() {
    let tmp = TempDir::new().unwrap();
    let ti = TestIndex::build(&corpus());
    let idx = s(ti.path());
    let out = tmp.path().join("o.json");
    let o = s(&out);
    let adv = write(tmp.path(), "adv.yaml", "version: t\nqueries: []\n");
    let a = s(&adv);
    let cases: &[(&[&str], &str)] = &[
        (&[], "no mode selected"),
        (
            &["--generate-eval-set", "--mvp", "--output", o],
            "requires --index",
        ),
        (
            &["--generate-eval-set", "--mvp", "--index", idx],
            "requires --output",
        ),
        (
            &["--generate-eval-set", "--index", idx, "--output", o],
            "only --mvp",
        ),
        (&["--validate-eval-set"], "requires at least one --eval-set"),
        (
            &["--batch-mode", "--output", o, "--eval-set", a],
            "--batch-mode requires --index",
        ),
        (
            &["--batch-mode", "--index", idx, "--eval-set", a],
            "--batch-mode requires --output",
        ),
        (
            &["--batch-mode", "--index", idx, "--output", o],
            "requires at least one --eval-set",
        ),
        (
            &[
                "--batch-mode",
                "--index",
                idx,
                "--output",
                o,
                "--eval-set",
                a,
            ],
            "no eval queries",
        ),
        (&["--eval-set", a], "--output is required"),
        (&["--baseline", o, "--current", o], "--diff-output required"),
        (&["--eval-set", "set.txt"], "unknown eval set format"),
    ];
    for (argv, want) in cases {
        let err = format!("{:#}", run_with(&args(argv)).unwrap_err());
        assert!(err.contains(want), "{argv:?}: {err}");
    }
}

#[test]
fn unreachable_service_fails_the_health_check() {
    // Bind then drop: nothing listens on the port any more.
    let port = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let tmp = TempDir::new().unwrap();
    let adv = write(tmp.path(), "adv.yaml", &adversarial_yaml(&corpus()));
    let err = run_with(&args(&[
        "--eval-set",
        s(&adv),
        "--service",
        &format!("http://127.0.0.1:{port}"),
        "--output",
        s(&tmp.path().join("r.json")),
    ]))
    .unwrap_err();
    assert!(format!("{err:#}").contains("health check"), "{err:#}");
}

/// A fake service: healthy on the health-check query, then answers every
/// query according to `mode`.
fn fake_service(mode: &'static str) -> String {
    let router = Router::new().route(
        "/geocode",
        get(
            move |Query(p): Query<std::collections::HashMap<String, String>>| async move {
                use axum::http::StatusCode;
                let ok = (
                    StatusCode::OK,
                    r#"{"match_type":"strict","raw_hits":0,"features":[]}"#.to_string(),
                );
                if p.get("q").map(String::as_str) == Some("health-check") {
                    return ok;
                }
                match mode {
                    "5xx" => (StatusCode::INTERNAL_SERVER_ERROR, "boom".into()),
                    "4xx" => (StatusCode::BAD_REQUEST, "bad".into()),
                    "garbage" => (StatusCode::OK, "not json".into()),
                    _ => ok,
                }
            },
        ),
    );
    serve(router)
}

fn adversarial_queries() -> Vec<crabfinder_eval::eval_set::EvalQuery> {
    let tmp = TempDir::new().unwrap();
    let adv = write(tmp.path(), "adv.yaml", &adversarial_yaml(&corpus()));
    flatten(&[EvalSet::auto_load(&adv).unwrap()])
}

fn config(url: String) -> RunnerConfig {
    RunnerConfig {
        service_url: url,
        ..RunnerConfig::default()
    }
}

#[tokio::test]
async fn degraded_service_aborts_the_run() {
    let err = run_eval(&config(fake_service("5xx")), adversarial_queries())
        .await
        .unwrap_err();
    assert!(err.to_string().contains("service degraded"), "{err}");
}

#[tokio::test]
async fn client_errors_and_bad_json_are_recorded_per_query() {
    for (mode, want) in [("4xx", "http 400"), ("garbage", "json:")] {
        let results = run_eval(&config(fake_service(mode)), adversarial_queries())
            .await
            .unwrap();
        assert_eq!(results.len(), 5);
        for r in &results {
            let e = r.error.as_deref().unwrap();
            assert!(e.starts_with(want), "{mode}: {e}");
            assert!(r.features.is_empty());
        }
    }
}

#[tokio::test]
async fn empty_answers_are_misses_not_errors() {
    let results = run_eval(&config(fake_service("empty")), adversarial_queries())
        .await
        .unwrap();
    let m = crabfinder_eval::metrics::compute(&results);
    assert_eq!(m.error_count, 0);
    assert_eq!(m.overall.recall_at_1, 0.0);
    // Sorted by id for a deterministic report.
    let ids: Vec<_> = results.iter().map(|r| r.query.id.as_str()).collect();
    let mut sorted = ids.clone();
    sorted.sort();
    assert_eq!(ids, sorted);
}
