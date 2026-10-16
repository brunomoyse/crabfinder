//! Asynchronous batch jobs (v3 Phase 11.6): `/batch/jobs` through the
//! production router, and `Jobs` restarts (resume, cleanup, TTL).

mod common;

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use axum::{
    Router,
    body::Body,
    http::{Request, StatusCode, header},
};
use common::{App, multipart};
use crabfinder_service::http::{self, RouterOptions};
use crabfinder_service::jobs::{JobState, JobStatus, Jobs, JobsConfig};
use crabfinder_test_support::{fixtures, parse_csv};
use http_body_util::BodyExt;
use serde_json::Value;
use tower::ServiceExt;

const HEADER: &str = "id,street,house_number,postal_code\n";

fn louise_rows(n: usize) -> String {
    let mut csv = String::from(HEADER);
    for i in 0..n {
        csv.push_str(&format!("{i},Avenue Louise,{},1050\n", [10, 20, 30][i % 3]));
    }
    csv
}

struct JobsApp {
    app: App,
    jobs: Arc<Jobs>,
    router: Router,
    _dir: tempfile::TempDir,
}

fn jobs_app(f: impl FnOnce(&mut JobsConfig)) -> JobsApp {
    let app = App::new(&fixtures::batch_small());
    let dir = tempfile::tempdir().unwrap();
    let mut cfg = JobsConfig {
        chunk_rows: 2,
        threads: 2,
        ..JobsConfig::new(dir.path().join("jobs"))
    };
    f(&mut cfg);
    let jobs = Jobs::open(app.state.clone(), cfg).unwrap();
    let router = router_for(&app, &jobs);
    JobsApp {
        app,
        jobs,
        router,
        _dir: dir,
    }
}

fn router_for(app: &App, jobs: &Arc<Jobs>) -> Router {
    let opts = RouterOptions {
        jobs: Some(jobs.clone()),
        ..RouterOptions::default()
    };
    http::router_with(app.state.clone(), &opts)
}

/// Send `req`: status, headers and the body as text.
async fn send(router: &Router, req: Request<Body>) -> (StatusCode, header::HeaderMap, String) {
    let resp = router.clone().oneshot(req).await.unwrap();
    let status = resp.status();
    let headers = resp.headers().clone();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    (status, headers, String::from_utf8(bytes.to_vec()).unwrap())
}

fn post(uri: &str, content_type: &str, body: impl Into<Body>) -> Request<Body> {
    Request::post(uri)
        .header(header::CONTENT_TYPE, content_type)
        .body(body.into())
        .unwrap()
}

fn get(uri: &str) -> Request<Body> {
    Request::get(uri).body(Body::empty()).unwrap()
}

fn delete(uri: &str) -> Request<Body> {
    Request::delete(uri).body(Body::empty()).unwrap()
}

/// Submit a job; returns its id.
async fn submit(router: &Router, uri: &str, content_type: &str, body: impl Into<Body>) -> String {
    let (status, headers, body) = send(router, post(uri, content_type, body)).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{body}");
    let v: Value = serde_json::from_str(&body).unwrap();
    let id = v["job_id"].as_str().unwrap().to_string();
    assert_eq!(headers[header::LOCATION], format!("/batch/jobs/{id}"));
    assert_eq!(v["status_url"], format!("/batch/jobs/{id}"));
    id
}

/// Poll the job until `done(status)` (10 s at most).
async fn wait(router: &Router, id: &str, done: impl Fn(&str) -> bool) -> Value {
    for _ in 0..500 {
        let (status, _, body) = send(router, get(&format!("/batch/jobs/{id}"))).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let v: Value = serde_json::from_str(&body).unwrap();
        if done(v["status"].as_str().unwrap()) {
            return v;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("job {id} never got there");
}

fn finished(s: &str) -> bool {
    matches!(s, "done" | "failed" | "cancelled")
}

#[tokio::test]
async fn a_job_runs_to_the_same_output_as_the_sync_route() {
    let j = jobs_app(|_| {});
    let csv = louise_rows(9);
    let id = submit(&j.router, "/batch/jobs", "text/csv", csv.clone()).await;
    let v = wait(&j.router, &id, finished).await;
    assert_eq!(v["status"], "done", "{v}");
    assert_eq!(v["rows_total"], 9);
    assert_eq!(v["rows_done"], 9);
    assert_eq!(v["format"], "csv");
    assert!(v["started_at"].as_u64().unwrap() >= v["created_at"].as_u64().unwrap());
    assert!(v["finished_at"].is_u64());
    assert_eq!(v["result_url"], format!("/batch/jobs/{id}/result"));

    let (status, headers, body) = send(&j.router, get(&format!("/batch/jobs/{id}/result"))).await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        headers[header::CONTENT_TYPE]
            .to_str()
            .unwrap()
            .starts_with("text/csv")
    );
    assert_eq!(
        headers[header::CONTENT_DISPOSITION],
        format!("attachment; filename=\"{id}.csv\"")
    );
    let (_, _, sync) = j.app.post("/batch/geocode", "text/csv", csv).await;
    assert_eq!(body, sync);
    assert_eq!(parse_csv(&body).1.len(), 9);

    // Deleting a finished job removes it.
    let (status, _, _) = send(&j.router, delete(&format!("/batch/jobs/{id}"))).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (status, _, body) = send(&j.router, get(&format!("/batch/jobs/{id}"))).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let v: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(v["error"], format!("unknown job '{id}'"));
    assert!(!j.jobs.config().dir.join(&id).exists());
}

#[tokio::test]
async fn json_and_multipart_jobs_take_their_options() {
    let j = jobs_app(|c| c.chunk_rows = 5_000);
    // JSON: no 10,000-row cap here.
    let addresses: Vec<_> = (0..10_001)
        .map(|i| serde_json::json!({"id": i.to_string(), "street": "Avenue Louise", "house_number": "10", "postal_code": "1050"}))
        .collect();
    let body = serde_json::json!({ "addresses": addresses, "options": {"format": "geojson"} });
    let id = submit(
        &j.router,
        "/batch/jobs",
        "application/json",
        body.to_string(),
    )
    .await;
    let v = wait(&j.router, &id, finished).await;
    assert_eq!(v["status"], "done", "{v}");
    assert_eq!(v["format"], "geojson");
    let (_, headers, out) = send(&j.router, get(&format!("/batch/jobs/{id}/result"))).await;
    assert_eq!(headers[header::CONTENT_TYPE], "application/geo+json");
    let fc: Value = serde_json::from_str(&out).unwrap();
    assert_eq!(fc["features"].as_array().unwrap().len(), 10_001);
    assert_eq!(fc["features"][10_000]["properties"]["confidence"], "exact");

    // Multipart with options; the query string wins.
    let (ct, body) = multipart(&[
        ("file", Some("in.csv"), &louise_rows(3)),
        ("options", None, r#"{"format":"geojson","lang":"nl"}"#),
    ]);
    let id = submit(&j.router, "/batch/jobs?format=csv", &ct, body).await;
    let v = wait(&j.router, &id, finished).await;
    assert_eq!(v["status"], "done", "{v}");
    assert_eq!(v["format"], "csv");
    let (_, _, out) = send(&j.router, get(&format!("/batch/jobs/{id}/result"))).await;
    assert_eq!(parse_csv(&out).1.len(), 3);

    // An empty input is a job with no rows.
    let id = submit(&j.router, "/batch/jobs", "text/csv", HEADER).await;
    let v = wait(&j.router, &id, finished).await;
    assert_eq!(v["status"], "done");
    assert_eq!(v["rows_total"], 0);
    let (_, _, out) = send(&j.router, get(&format!("/batch/jobs/{id}/result"))).await;
    assert_eq!(out, "");
}

#[tokio::test]
async fn bad_submissions_are_refused_and_leave_nothing() {
    let j = jobs_app(|c| c.max_body_bytes = 200);
    let cases = [
        (
            post("/batch/jobs?format=xml", "text/csv", louise_rows(1)),
            StatusCode::BAD_REQUEST,
            "unknown format 'xml' (expected csv or geojson)",
        ),
        (
            post("/batch/jobs", "text/csv", "id,nothing\n1,x\n"),
            StatusCode::BAD_REQUEST,
            "csv header has none of the `street`, `address` and `best_id` columns",
        ),
        (
            post("/batch/jobs", "text/csv", louise_rows(20)),
            StatusCode::PAYLOAD_TOO_LARGE,
            "batch job uploads are limited to 1 MiB",
        ),
        (
            post("/batch/jobs", "application/json", "{not json"),
            StatusCode::BAD_REQUEST,
            "",
        ),
        (
            post(
                "/batch/jobs",
                "multipart/form-data; boundary=x",
                "--x\r\nContent-Disposition: form-data; name=\"options\"\r\n\r\n{}\r\n--x--\r\n",
            ),
            StatusCode::BAD_REQUEST,
            "multipart body missing `file` part",
        ),
        (
            post("/batch/jobs?mid_street_fallback=maybe", "text/csv", ""),
            StatusCode::BAD_REQUEST,
            "",
        ),
    ];
    for (req, code, msg) in cases {
        let uri = req.uri().to_string();
        let (status, _, body) = send(&j.router, req).await;
        assert_eq!(status, code, "{uri}: {body}");
        let v: Value = serde_json::from_str(&body).unwrap();
        assert!(v["error"].as_str().unwrap().contains(msg), "{uri}: {body}");
    }
    // No job and no upload left behind.
    let left: Vec<_> = std::fs::read_dir(&j.jobs.config().dir)
        .unwrap()
        .map(|e| e.unwrap().file_name())
        .collect();
    assert!(left.is_empty(), "{left:?}");

    let unknown = "0123456789abcdef0123456789abcdef";
    for req in [
        get(&format!("/batch/jobs/{unknown}")),
        get(&format!("/batch/jobs/{unknown}/result")),
        delete(&format!("/batch/jobs/{unknown}")),
        get("/batch/jobs/..%2F..%2Fetc"),
    ] {
        let (status, _, _) = send(&j.router, req).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
    }
}

#[tokio::test]
async fn a_malformed_row_fails_the_job() {
    let j = jobs_app(|_| {});
    // Not UTF-8 on line 3.
    let csv = b"id,street\n1,Avenue Louise\n2,\xffbad\n".to_vec();
    let id = submit(&j.router, "/batch/jobs", "text/csv", csv).await;
    let v = wait(&j.router, &id, finished).await;
    assert_eq!(v["status"], "failed", "{v}");
    assert!(v["error"].as_str().unwrap().contains("line 3"), "{v}");
    assert!(v.get("result_url").is_none());
    let (status, _, body) = send(&j.router, get(&format!("/batch/jobs/{id}/result"))).await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert!(body.contains("is failed, not done"), "{body}");
}

#[tokio::test]
async fn jobs_queue_and_cancel() {
    // One job at a time, 50 ms per one-row chunk: A runs, B waits.
    let j = jobs_app(|c| {
        c.max_running = 1;
        c.chunk_rows = 1;
        c.chunk_delay = Duration::from_millis(50);
    });
    let a = submit(&j.router, "/batch/jobs", "text/csv", louise_rows(100)).await;
    wait(&j.router, &a, |s| s == "running").await;
    let b = submit(&j.router, "/batch/jobs", "text/csv", louise_rows(3)).await;
    let (_, _, body) = send(&j.router, get(&format!("/batch/jobs/{b}"))).await;
    assert!(body.contains(r#""status":"queued""#), "{body}");
    let (status, _, _) = send(&j.router, get(&format!("/batch/jobs/{a}/result"))).await;
    assert_eq!(status, StatusCode::CONFLICT);

    for id in [&b, &a] {
        let (status, _, body) = send(&j.router, delete(&format!("/batch/jobs/{id}"))).await;
        assert_eq!(status, StatusCode::ACCEPTED, "{body}");
        assert!(body.contains(r#""status":"cancelled""#), "{body}");
    }
    // B never ran; A stops within a chunk and leaves no result.
    let dir = j.jobs.config().dir.clone();
    for _ in 0..100 {
        if !dir.join(&a).join("result.part").exists() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(!dir.join(&a).join("result.part").exists());
    let st = j.jobs.get(&b).unwrap();
    assert_eq!(
        (st.status, st.rows_done, st.started_at),
        (JobStatus::Cancelled, 0, None)
    );
    let st = j.jobs.get(&a).unwrap();
    assert!(st.rows_done < 100, "{}", st.rows_done);

    // Cancelled jobs are finished: deleting again removes them.
    let (status, _, _) = send(&j.router, delete(&format!("/batch/jobs/{a}"))).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
}

/// Write a job directory as a service killed mid-job would leave it:
/// `rows_done` rows of `csv` in `result.part` (plus `junk` past the
/// checkpoint), status `running`.
fn interrupted_job(
    dir: &Path,
    id: &str,
    csv: &str,
    format: &str,
    partial: &[u8],
    rows_done: usize,
    junk: &[u8],
) {
    let job = dir.join(id);
    std::fs::create_dir_all(&job).unwrap();
    std::fs::write(job.join("input.csv"), csv).unwrap();
    let mut part = partial.to_vec();
    part.extend_from_slice(junk);
    std::fs::write(job.join("result.part"), part).unwrap();
    let st = serde_json::json!({
        "id": id, "status": "running",
        "options": {"mid_street_fallback": true, "format": format, "lang": null, "corrections": false, "alternatives": false},
        "delimiter": b',', "rows_total": csv.lines().count() - 1, "rows_done": rows_done,
        "id_columns": false, "result_bytes": partial.len(),
        "created_at": 1, "started_at": 2, "finished_at": null, "error": null
    });
    std::fs::write(job.join("state.json"), st.to_string()).unwrap();
}

#[tokio::test]
async fn a_restarted_service_resumes_interrupted_jobs() {
    let app = App::new(&fixtures::batch_small());
    let dir = tempfile::tempdir().unwrap();
    let jobs_dir = dir.path().join("jobs");
    let csv = louise_rows(11);
    let (_, _, whole_csv) = app.post("/batch/geocode", "text/csv", csv.clone()).await;
    let (_, _, whole_geo) = app
        .post("/batch/geocode?format=geojson", "text/csv", csv.clone())
        .await;

    // CSV, 4 rows done: header + 4 lines, then a half-written line.
    let head: String = whole_csv
        .lines()
        .take(5)
        .map(|l| format!("{l}\n"))
        .collect();
    let csv_id = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    interrupted_job(
        &jobs_dir,
        csv_id,
        &csv,
        "csv",
        head.as_bytes(),
        4,
        b"7,Avenue Lou",
    );
    // GeoJSON, 3 features done.
    let (_, _, three) = app
        .post("/batch/geocode?format=geojson", "text/csv", louise_rows(3))
        .await;
    let geo_head = three.strip_suffix("]}").unwrap();
    let geo_id = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
    interrupted_job(
        &jobs_dir,
        geo_id,
        &csv,
        "geojson",
        geo_head.as_bytes(),
        3,
        b",{\"type",
    );
    // A checkpoint past the end of its file: starts over.
    let lost_id = "cccccccccccccccccccccccccccccccc";
    interrupted_job(&jobs_dir, lost_id, &csv, "csv", head.as_bytes(), 4, b"");
    let st_path = jobs_dir.join(lost_id).join("state.json");
    let st = std::fs::read_to_string(&st_path).unwrap().replace(
        &format!("\"result_bytes\":{}", head.len()),
        "\"result_bytes\":99999",
    );
    std::fs::write(&st_path, st).unwrap();
    // Leftovers: an unfinished upload, an expired job, a foreign file.
    std::fs::create_dir_all(jobs_dir.join(".upload-dddddddddddddddddddddddddddddddd")).unwrap();
    let old = "eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee";
    interrupted_job(&jobs_dir, old, &csv, "csv", b"", 0, b"");
    let st = std::fs::read_to_string(jobs_dir.join(old).join("state.json"))
        .unwrap()
        .replace("\"running\"", "\"done\"")
        .replace("\"finished_at\":null", "\"finished_at\":5");
    std::fs::write(jobs_dir.join(old).join("state.json"), st).unwrap();
    std::fs::write(jobs_dir.join("README"), "not a job").unwrap();
    std::fs::create_dir_all(jobs_dir.join("ffffffffffffffffffffffffffffffff")).unwrap();

    let jobs = Jobs::open(
        app.state.clone(),
        JobsConfig {
            chunk_rows: 2,
            ..JobsConfig::new(&jobs_dir)
        },
    )
    .unwrap();
    let router = router_for(&app, &jobs);
    for (id, whole) in [
        (csv_id, &whole_csv),
        (geo_id, &whole_geo),
        (lost_id, &whole_csv),
    ] {
        let v = wait(&router, id, finished).await;
        assert_eq!(v["status"], "done", "{id}: {v}");
        assert_eq!(v["rows_done"], 11);
        let (_, _, out) = send(&router, get(&format!("/batch/jobs/{id}/result"))).await;
        assert_eq!(&out, whole, "{id}");
    }
    assert!(
        !jobs_dir
            .join(".upload-dddddddddddddddddddddddddddddddd")
            .exists()
    );
    assert!(jobs.get(old).is_none() && !jobs_dir.join(old).exists());
    assert!(jobs_dir.join("README").exists());
    assert!(jobs.get("ffffffffffffffffffffffffffffffff").is_none());
}

#[tokio::test]
async fn finished_jobs_expire_after_the_ttl() {
    let j = jobs_app(|c| c.ttl = Duration::from_secs(3600));
    let id = submit(&j.router, "/batch/jobs", "text/csv", louise_rows(2)).await;
    let v = wait(&j.router, &id, finished).await;
    let finished_at = v["finished_at"].as_u64().unwrap();
    assert_eq!(j.jobs.sweep(finished_at + 3599), 0);
    assert!(j.jobs.get(&id).is_some());
    assert_eq!(j.jobs.sweep(finished_at + 3600), 1);
    assert!(j.jobs.get(&id).is_none());
    assert!(!j.jobs.config().dir.join(&id).exists());
    let st: Option<JobState> = j.jobs.get(&id);
    assert!(st.is_none());
}

#[tokio::test]
async fn without_a_jobs_dir_the_routes_answer_503() {
    let app = App::new(&fixtures::batch_small());
    let router = app.router();
    for req in [
        post("/batch/jobs", "text/csv", louise_rows(1)),
        get("/batch/jobs/0123456789abcdef0123456789abcdef"),
        get("/batch/jobs/0123456789abcdef0123456789abcdef/result"),
        delete("/batch/jobs/0123456789abcdef0123456789abcdef"),
    ] {
        let (status, _, body) = send(&router, req).await;
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
        assert!(body.contains("--jobs-dir"), "{body}");
    }
}
