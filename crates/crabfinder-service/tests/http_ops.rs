//! Operational behaviour of the router (v2 Phase 8): `/health` metadata,
//! `/metrics`, the request deadline, the concurrency guard and CORS.

mod common;

use std::sync::Arc;
use std::time::Duration;

use axum::{
    Router,
    body::Body,
    http::{Request, StatusCode, header},
    middleware,
    response::Response,
    routing::get,
};
use common::{App, enc};
use crabfinder_indexer::meta::SCHEMA_VERSION;
use crabfinder_service::http::{self, Guard, RouterOptions, guard};
use crabfinder_service::telemetry;
use crabfinder_test_support::fixtures;
use http_body_util::BodyExt;
use tokio::sync::Semaphore;
use tower::ServiceExt;

async fn send(router: Router, req: Request<Body>) -> (Response, String) {
    let resp = router.oneshot(req).await.unwrap();
    let (parts, body) = resp.into_parts();
    let bytes = body.collect().await.unwrap().to_bytes();
    (
        Response::from_parts(parts, Body::empty()),
        String::from_utf8(bytes.to_vec()).unwrap(),
    )
}

fn get_req(uri: &str) -> Request<Body> {
    Request::get(uri).body(Body::empty()).unwrap()
}

#[tokio::test]
async fn health_reports_index_metadata() {
    let app = App::new(&fixtures::louise_street());
    let h = app.ok("/health").await;
    assert_eq!(h["status"], "ok");
    assert_eq!(h["schema_version"], SCHEMA_VERSION);
    assert!(h["index_built_at_unix_secs"].as_i64().unwrap() > 1_700_000_000);
    assert_eq!(h["version"], env!("CARGO_PKG_VERSION"));
    assert_eq!(h["index_docs"], fixtures::louise_street().len());
}

#[tokio::test]
async fn metrics_expose_requests_latency_and_search_outcomes() {
    let app = App::new(&fixtures::louise_street());
    let opts = RouterOptions {
        metrics: Some(telemetry::install()),
        ..RouterOptions::default()
    };
    let router = http::router_with(app.state.clone(), &opts);
    let uri = format!("/geocode?q={}", enc("Avenue Louise 32 1050"));
    let (resp, _) = send(router.clone(), get_req(&uri)).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let (resp, _) = send(router.clone(), get_req("/building/nope/units")).await;
    assert_eq!(resp.status(), StatusCode::OK);

    let (resp, body) = send(router, get_req("/metrics")).await;
    assert_eq!(resp.status(), StatusCode::OK);
    assert!(
        resp.headers()[header::CONTENT_TYPE]
            .to_str()
            .unwrap()
            .starts_with("text/plain; version=0.0.4")
    );
    for series in [
        r#"crabfinder_http_requests_total{route="/geocode",method="GET",status="200"}"#,
        // Route templates, not concrete paths: bounded label cardinality.
        r#"crabfinder_http_requests_total{route="/building/{building_group_id}/units",method="GET",status="200"}"#,
        r#"crabfinder_http_request_duration_seconds_bucket{route="/geocode",le="0.1"}"#,
        r#"crabfinder_search_total{tier="exact",confidence="exact",match_type="strict"}"#,
    ] {
        assert!(body.contains(series), "missing {series} in:\n{body}");
    }
}

#[tokio::test]
async fn metrics_route_is_off_by_default() {
    let app = App::new(&fixtures::louise_street());
    let (resp, _) = send(app.router(), get_req("/metrics")).await;
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn the_deadline_fires_during_blocking_work() {
    // 200 ms of synchronous work on the blocking pool (as /geocode and
    // /reverse do with Tantivy) against a 50 ms deadline.
    let app = App::new(&fixtures::louise_street());
    let metrics = telemetry::install();
    let g = Guard {
        timeout: Duration::from_millis(50),
        permits: Arc::new(Semaphore::new(4)),
    };
    let state = app.state.clone();
    let router = Router::new()
        .route(
            "/sleepy",
            get(move || {
                let state = state.clone();
                async move {
                    http::blocking(state, |_| std::thread::sleep(Duration::from_millis(200)))
                        .await
                        .map(|()| "late")
                }
            }),
        )
        .route_layer(middleware::from_fn_with_state(g, guard));
    let t0 = std::time::Instant::now();
    let (resp, body) = send(router, get_req("/sleepy")).await;
    assert!(
        t0.elapsed() < Duration::from_millis(190),
        "{:?}",
        t0.elapsed()
    );
    assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
    let json: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(json["error"], "request timed out after 50 ms");
    assert!(
        metrics
            .render()
            .contains(r#"crabfinder_http_rejected_total{route="/sleepy",reason="timeout"}"#)
    );
}

/// A router with one `/slow` route (sleeps 1 s) behind `guard`.
fn slow_router(permits: usize, timeout: Duration) -> Router {
    let g = Guard {
        timeout,
        permits: Arc::new(Semaphore::new(permits)),
    };
    Router::new()
        .route(
            "/slow",
            get(|| async {
                tokio::time::sleep(Duration::from_secs(1)).await;
                "done"
            }),
        )
        .route_layer(middleware::from_fn_with_state(g, guard))
}

#[tokio::test(start_paused = true)]
async fn the_deadline_covers_waiting_for_a_slot() {
    // One slot, 1.5 s deadline: the first request takes 1 s; the second
    // waits 1 s for the slot, then needs 1 s more and times out.
    let router = slow_router(1, Duration::from_millis(1500));
    let (a, b) = tokio::join!(
        send(router.clone(), get_req("/slow")),
        send(router.clone(), get_req("/slow")),
    );
    let mut codes = [a.0.status(), b.0.status()];
    codes.sort();
    assert_eq!(codes, [StatusCode::OK, StatusCode::SERVICE_UNAVAILABLE]);

    // With two slots both fit.
    let router = slow_router(2, Duration::from_millis(1500));
    let (a, b) = tokio::join!(
        send(router.clone(), get_req("/slow")),
        send(router, get_req("/slow")),
    );
    assert_eq!(a.0.status(), StatusCode::OK);
    assert_eq!(b.1, "done");
}

fn with_origin(uri: &str, origin: &str) -> Request<Body> {
    Request::get(uri)
        .header(header::ORIGIN, origin)
        .body(Body::empty())
        .unwrap()
}

#[tokio::test]
async fn cors_allows_any_origin_unless_configured() {
    let app = App::new(&fixtures::louise_street());
    let (resp, _) = send(app.router(), with_origin("/health", "https://a.example")).await;
    assert_eq!(resp.headers()[header::ACCESS_CONTROL_ALLOW_ORIGIN], "*");

    let opts = RouterOptions {
        cors_origins: vec!["https://app.example".parse().unwrap()],
        ..RouterOptions::default()
    };
    let router = http::router_with(app.state.clone(), &opts);
    let (resp, _) = send(
        router.clone(),
        with_origin("/health", "https://app.example"),
    )
    .await;
    assert_eq!(
        resp.headers()[header::ACCESS_CONTROL_ALLOW_ORIGIN],
        "https://app.example"
    );
    let (resp, _) = send(router, with_origin("/health", "https://evil.example")).await;
    assert!(
        resp.headers()
            .get(header::ACCESS_CONTROL_ALLOW_ORIGIN)
            .is_none()
    );
}

#[tokio::test]
async fn a_zero_concurrency_limit_still_serves_one_at_a_time() {
    let app = App::new(&fixtures::louise_street());
    let opts = RouterOptions {
        max_concurrency: 0,
        ..RouterOptions::default()
    };
    let (resp, _) = send(
        http::router_with(app.state.clone(), &opts),
        get_req("/health"),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::OK);
}
