//! Basic protection (v3 Phase 11.7): API keys, route permissions, rate
//! limits per key and per address, jobs scoped by key, the batch body
//! limit, and CORS with `Authorization`.

mod common;

use std::net::SocketAddr;
use std::sync::Arc;

use axum::{
    Router,
    body::Body,
    extract::connect_info::MockConnectInfo,
    http::{Method, Request, StatusCode, header},
};
use common::{App, enc, multipart};
use crabfinder_service::http::{
    self, RouterOptions,
    auth::{ApiKey, Auth},
};
use crabfinder_service::jobs::{Jobs, JobsConfig};
use crabfinder_service::telemetry;
use crabfinder_test_support::fixtures;
use http_body_util::BodyExt;
use serde_json::Value;
use tower::ServiceExt;

const WEB: &str = "web-key-0123456789";
const ETL: &str = "etl-key-0123456789";
const CSV: &str = "id,street,house_number,postal_code\n1,Avenue Louise,10,1050\n";

fn keys() -> Vec<ApiKey> {
    vec![
        ApiKey {
            name: "web".into(),
            key: WEB.into(),
            rate_per_minute: None,
            batch: false,
            jobs: false,
        },
        ApiKey {
            name: "etl".into(),
            key: ETL.into(),
            rate_per_minute: None,
            batch: true,
            jobs: true,
        },
    ]
}

fn router(app: &App, f: impl FnOnce(&mut RouterOptions)) -> Router {
    let mut opts = RouterOptions::default();
    f(&mut opts);
    http::router_with(app.state.clone(), &opts)
}

fn with_keys(app: &App, keys: Vec<ApiKey>) -> Router {
    router(app, |o| o.auth = Auth::new(keys, None, false))
}

async fn send(r: &Router, req: Request<Body>) -> (StatusCode, header::HeaderMap, String) {
    let resp = r.clone().oneshot(req).await.unwrap();
    let status = resp.status();
    let headers = resp.headers().clone();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    (status, headers, String::from_utf8(bytes.to_vec()).unwrap())
}

fn get(uri: &str, key: Option<&str>) -> Request<Body> {
    let mut b = Request::get(uri);
    if let Some(k) = key {
        b = b.header(header::AUTHORIZATION, format!("Bearer {k}"));
    }
    b.body(Body::empty()).unwrap()
}

fn post(uri: &str, key: &str, ct: &str, body: impl Into<Body>) -> Request<Body> {
    Request::post(uri)
        .header("x-api-key", key)
        .header(header::CONTENT_TYPE, ct)
        .body(body.into())
        .unwrap()
}

fn geocode_uri() -> String {
    format!("/geocode?q={}", enc("Avenue Louise 10 1050"))
}

fn error_of(body: &str) -> String {
    let v: Value = serde_json::from_str(body).unwrap();
    v["error"].as_str().unwrap().to_string()
}

#[tokio::test]
async fn keys_are_required_except_on_health_and_metrics() {
    let app = App::new(&fixtures::batch_small());
    let r = router(&app, |o| {
        o.auth = Auth::new(keys(), None, false);
        o.metrics = Some(telemetry::install());
    });
    let (status, headers, body) = send(&r, get(&geocode_uri(), None)).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(headers[header::WWW_AUTHENTICATE], "Bearer");
    assert_eq!(
        error_of(&body),
        "missing API key (Authorization: Bearer <key> or X-API-Key: <key>)"
    );
    let (status, _, body) = send(&r, get(&geocode_uri(), Some("nope-0123456789abc"))).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(error_of(&body), "invalid API key");

    let (status, _, _) = send(&r, get(&geocode_uri(), Some(WEB))).await;
    assert_eq!(status, StatusCode::OK);
    let req = Request::get(geocode_uri())
        .header("x-api-key", WEB)
        .body(Body::empty())
        .unwrap();
    assert_eq!(send(&r, req).await.0, StatusCode::OK);

    assert_eq!(send(&r, get("/health", None)).await.0, StatusCode::OK);
    let (status, _, metrics) = send(&r, get("/metrics", None)).await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        metrics.contains(r#"route="/geocode",method="GET",status="200",key="web""#),
        "{metrics}"
    );
    assert!(
        metrics
            .contains(r#"crabfinder_http_rejected_total{route="/geocode",reason="unauthorized"}"#),
        "{metrics}"
    );

    // --metrics-require-key closes /metrics too.
    let r = router(&app, |o| {
        o.auth = Auth::new(keys(), None, true);
        o.metrics = Some(telemetry::install());
    });
    assert_eq!(
        send(&r, get("/metrics", None)).await.0,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(send(&r, get("/metrics", Some(WEB))).await.0, StatusCode::OK);
    assert_eq!(send(&r, get("/health", None)).await.0, StatusCode::OK);
}

#[tokio::test]
async fn batch_and_jobs_need_their_permission() {
    let app = App::new(&fixtures::batch_small());
    let dir = tempfile::tempdir().unwrap();
    let jobs = Jobs::open(app.state.clone(), JobsConfig::new(dir.path())).unwrap();
    let r = router(&app, |o| {
        o.auth = Auth::new(keys(), None, false);
        o.jobs = Some(jobs);
    });
    let (status, _, body) = send(&r, post("/batch/geocode", WEB, "text/csv", CSV)).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(error_of(&body), "key 'web' may not use /batch/geocode");
    let (status, _, _) = send(&r, post("/batch/jobs", WEB, "text/csv", CSV)).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, _, _) = send(
        &r,
        get("/batch/jobs/0123456789abcdef0123456789abcdef", Some(WEB)),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    let (status, _, body) = send(&r, post("/batch/geocode", ETL, "text/csv", CSV)).await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("exact"), "{body}");
    let (status, _, _) = send(&r, post("/batch/jobs", ETL, "text/csv", CSV)).await;
    assert_eq!(status, StatusCode::ACCEPTED);
}

#[tokio::test]
async fn jobs_belong_to_the_key_that_submitted_them() {
    let app = App::new(&fixtures::batch_small());
    let dir = tempfile::tempdir().unwrap();
    let jobs = Jobs::open(app.state.clone(), JobsConfig::new(dir.path())).unwrap();
    let mut other = keys();
    other[0].jobs = true;
    let r = router(&app, |o| {
        o.auth = Auth::new(other, None, false);
        o.jobs = Some(jobs.clone());
    });
    let (_, _, body) = send(&r, post("/batch/jobs", ETL, "text/csv", CSV)).await;
    let id = serde_json::from_str::<Value>(&body).unwrap()["job_id"]
        .as_str()
        .unwrap()
        .to_string();
    assert_eq!(jobs.get(&id, None).unwrap().owner.as_deref(), Some("etl"));
    let uri = format!("/batch/jobs/{id}");
    assert_eq!(send(&r, get(&uri, Some(ETL))).await.0, StatusCode::OK);
    assert_eq!(
        send(&r, get(&uri, Some(WEB))).await.0,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        send(&r, get(&format!("{uri}/result"), Some(WEB))).await.0,
        StatusCode::NOT_FOUND
    );
    let del = |k: &str| {
        Request::delete(uri.as_str())
            .header("x-api-key", k)
            .body(Body::empty())
            .unwrap()
    };
    assert_eq!(send(&r, del(WEB)).await.0, StatusCode::NOT_FOUND);
    assert!(jobs.get(&id, Some("etl")).is_some());
    assert!(jobs.get(&id, Some("web")).is_none());
}

#[tokio::test]
async fn keys_are_rate_limited() {
    let app = App::new(&fixtures::batch_small());
    let mut ks = keys();
    ks[0].rate_per_minute = Some(2);
    let r = with_keys(&app, ks);
    for _ in 0..2 {
        assert_eq!(
            send(&r, get(&geocode_uri(), Some(WEB))).await.0,
            StatusCode::OK
        );
    }
    let (status, headers, body) = send(&r, get(&geocode_uri(), Some(WEB))).await;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(headers[header::RETRY_AFTER], "30");
    assert_eq!(
        error_of(&body),
        "rate limit exceeded: 2 requests per minute"
    );
    // Other keys and /health are not affected.
    assert_eq!(
        send(&r, get(&geocode_uri(), Some(ETL))).await.0,
        StatusCode::OK
    );
    assert_eq!(send(&r, get("/health", Some(WEB))).await.0, StatusCode::OK);
}

#[tokio::test]
async fn addresses_are_rate_limited_without_keys() {
    let app = App::new(&fixtures::batch_small());
    let auth = Auth::new(Vec::new(), Some(2), false);
    let base = router(&app, |o| o.auth = auth.clone());
    let from = |ip: &str| {
        base.clone()
            .layer(MockConnectInfo(ip.parse::<SocketAddr>().unwrap()))
    };
    let a = from("10.0.0.1:5000");
    for _ in 0..2 {
        assert_eq!(send(&a, get(&geocode_uri(), None)).await.0, StatusCode::OK);
    }
    assert_eq!(
        send(&a, get(&geocode_uri(), None)).await.0,
        StatusCode::TOO_MANY_REQUESTS
    );
    // Another address has its own budget; no key is needed.
    let b = from("10.0.0.2:5000");
    assert_eq!(send(&b, get(&geocode_uri(), None)).await.0, StatusCode::OK);
    // Without a peer address (in-process), nothing to limit by.
    for _ in 0..3 {
        assert_eq!(
            send(&base, get(&geocode_uri(), None)).await.0,
            StatusCode::OK
        );
    }
    let _: Option<Arc<Auth>> = auth;
}

#[tokio::test]
async fn the_batch_body_limit_is_a_413() {
    let app = App::new(&fixtures::batch_small());
    let r = router(&app, |o| o.batch_max_body_bytes = 64);
    let big = format!("{CSV}{}", "2,Avenue Louise,20,1050\n".repeat(5));
    let req = |ct: &str, body: String| {
        Request::post("/batch/geocode")
            .header(header::CONTENT_TYPE, ct)
            .body(Body::from(body))
            .unwrap()
    };
    let (status, _, body) = send(&r, req("text/csv", big.clone())).await;
    assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE);
    assert_eq!(
        body,
        "request body over the 1 MiB limit; use /batch/jobs for larger files"
    );
    let json = serde_json::json!({"addresses": [{"id": "1", "street": "Avenue Louise Avenue Louise Avenue Louise"}]});
    let (status, _, _) = send(&r, req("application/json", json.to_string())).await;
    assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE);
    let (ct, body) = multipart(&[("file", Some("in.csv"), &big)]);
    let (status, _, _) = send(&r, req(&ct, body)).await;
    assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE);
    // Under the limit it works.
    let (status, _, _) = send(&r, req("text/csv", CSV.to_string())).await;
    assert_eq!(status, StatusCode::OK);
}

#[tokio::test]
async fn cors_preflight_allows_the_authorization_header() {
    let app = App::new(&fixtures::batch_small());
    let r = with_keys(&app, keys());
    let req = Request::builder()
        .method(Method::OPTIONS)
        .uri("/geocode")
        .header(header::ORIGIN, "https://app.example")
        .header(header::ACCESS_CONTROL_REQUEST_METHOD, "GET")
        .header(header::ACCESS_CONTROL_REQUEST_HEADERS, "authorization")
        .body(Body::empty())
        .unwrap();
    let (status, headers, _) = send(&r, req).await;
    assert!(status.is_success(), "{status}");
    assert_eq!(
        headers[header::ACCESS_CONTROL_ALLOW_HEADERS],
        "authorization"
    );
}
