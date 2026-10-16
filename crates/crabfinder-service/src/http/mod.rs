//! HTTP surface of the service: handlers, response shapes and the router.
//!
//! Lives in the library (not `main.rs`) so integration tests drive the real
//! handlers through `router()` with `tower::ServiceExt::oneshot`, and the
//! binary stays a thin `Args` → `AppState::load` → `serve` wrapper.

pub mod autocomplete;
pub mod building;
pub mod error;
pub mod feature;
pub mod geocode;
pub mod health;
pub mod lookup;
pub mod reverse;
pub mod validate;

use std::sync::Arc;
use std::time::Duration;

use axum::{
    Router,
    extract::{Request, State},
    http::HeaderValue,
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::get,
};
use metrics_exporter_prometheus::PrometheusHandle;
use tokio::sync::Semaphore;
use tower_http::cors::{AllowOrigin, Any, CorsLayer};
use tower_http::trace::{DefaultMakeSpan, DefaultOnResponse, TraceLayer};
use tracing::Level;

use crate::AppState;
use crate::batch;
use crate::telemetry;

pub use error::ServiceError;

/// Operational knobs of the router (v2 Phase 8). `Default` is what the
/// binary uses without flags.
#[derive(Clone)]
pub struct RouterOptions {
    /// Deadline of every route but the batch one, queueing included.
    pub request_timeout: Duration,
    /// Deadline of `POST /batch/geocode` until its response starts (the
    /// CSV body then streams without one).
    pub batch_timeout: Duration,
    /// Requests served at once across all routes; the rest queue (and
    /// time out) until a slot frees up.
    pub max_concurrency: usize,
    /// Allowed CORS origins; empty = any (`*`).
    pub cors_origins: Vec<HeaderValue>,
    /// Serve `GET /metrics` from this handle (`telemetry::install`).
    pub metrics: Option<PrometheusHandle>,
}

impl Default for RouterOptions {
    fn default() -> Self {
        Self {
            request_timeout: Duration::from_secs(10),
            batch_timeout: Duration::from_secs(600),
            max_concurrency: 256,
            cors_origins: Vec::new(),
            metrics: None,
        }
    }
}

/// Build the production router over `state` with default options.
pub fn router(state: AppState) -> Router {
    router_with(state, &RouterOptions::default())
}

/// Build the router over `state` with `opts`.
pub fn router_with(state: AppState, opts: &RouterOptions) -> Router {
    let permits = Arc::new(Semaphore::new(opts.max_concurrency.max(1)));
    let guard_for = |timeout| Guard {
        timeout,
        permits: permits.clone(),
    };
    let api = Router::new()
        .route("/health", get(health::health))
        .route("/geocode", get(geocode::geocode))
        // `building_group_id` is path-percent-encoded by the client because
        // the canonical form contains slashes and pipes.
        .route(
            "/building/{building_group_id}/units",
            get(building::building_units),
        )
        .route("/reverse", get(reverse::reverse))
        .route("/lookup", get(lookup::lookup))
        .route("/validate", get(validate::validate))
        .route("/autocomplete", get(autocomplete::autocomplete))
        .route_layer(middleware::from_fn_with_state(
            guard_for(opts.request_timeout),
            guard,
        ));
    // Lot E (v1 batch): body cap dialled up to 50 MiB on this route only.
    let batch = Router::new()
        .route(
            "/batch/geocode",
            axum::routing::post(batch::batch_geocode).route_layer(batch::body_limit_layer()),
        )
        .route_layer(middleware::from_fn_with_state(
            guard_for(opts.batch_timeout),
            guard,
        ));
    let mut app = api.merge(batch);
    if let Some(handle) = opts.metrics.clone() {
        app = app.route(
            "/metrics",
            get(move || std::future::ready(metrics_body(&handle))),
        );
    }
    app.route_layer(middleware::from_fn(telemetry::track))
        .layer(
            TraceLayer::new_for_http()
                .make_span_with(DefaultMakeSpan::new().level(Level::INFO))
                .on_response(DefaultOnResponse::new().level(Level::INFO)),
        )
        .layer(cors_layer(&opts.cors_origins))
        .with_state(state)
}

fn cors_layer(origins: &[HeaderValue]) -> CorsLayer {
    let origin = if origins.is_empty() {
        AllowOrigin::any()
    } else {
        AllowOrigin::list(origins.iter().cloned())
    };
    CorsLayer::new()
        .allow_origin(origin)
        .allow_methods(Any)
        .allow_headers(Any)
}

/// Prometheus text exposition.
fn metrics_body(handle: &PrometheusHandle) -> Response {
    (
        [(
            axum::http::header::CONTENT_TYPE,
            "text/plain; version=0.0.4; charset=utf-8",
        )],
        handle.render(),
    )
        .into_response()
}

/// Run `f` on the blocking pool (Tantivy work is CPU-bound and
/// synchronous), so it neither stalls the runtime nor outlives `guard`'s
/// deadline unnoticed.
pub async fn blocking<T, F>(state: AppState, f: F) -> Result<T, ServiceError>
where
    F: FnOnce(AppState) -> T + Send + 'static,
    T: Send + 'static,
{
    tokio::task::spawn_blocking(move || f(state))
        .await
        .map_err(|e| ServiceError::internal("blocking task", e))
}

/// State of `guard`: one route group's deadline and the shared slots.
#[derive(Clone)]
pub struct Guard {
    pub timeout: Duration,
    pub permits: Arc<Semaphore>,
}

/// Bound a request's wait for a concurrency slot plus its handling by
/// `timeout`; past it, answer 503 (JSON). Handlers run their Tantivy work
/// on the blocking pool, so the deadline fires even mid-search (the
/// search itself finishes in the background).
pub async fn guard(State(g): State<Guard>, req: Request, next: Next) -> Response {
    let route = telemetry::route_of(&req);
    let run = async {
        // The semaphore is never closed.
        let _permit = g.permits.acquire().await.ok();
        next.run(req).await
    };
    match tokio::time::timeout(g.timeout, run).await {
        Ok(resp) => resp,
        Err(_) => {
            telemetry::record_rejected(route, "timeout");
            ServiceError::Unavailable(format!(
                "request timed out after {} ms",
                g.timeout.as_millis()
            ))
            .into_response()
        }
    }
}
