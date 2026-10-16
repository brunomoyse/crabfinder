//! HTTP surface of the service: handlers, response shapes and the router.
//!
//! Lives in the library (not `main.rs`) so integration tests drive the real
//! handlers through `router()` with `tower::ServiceExt::oneshot`, and the
//! binary stays a thin `Args` → `AppState::load` → `serve` wrapper.

pub mod auth;
pub mod autocomplete;
pub mod building;
pub mod error;
pub mod feature;
pub mod geocode;
pub mod health;
pub mod jobs;
pub mod lookup;
pub mod reverse;
pub mod validate;

use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::{
    Extension, Router,
    extract::{DefaultBodyLimit, Request, State},
    http::HeaderValue,
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use metrics_exporter_prometheus::PrometheusHandle;
use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use tower_http::cors::{AllowHeaders, AllowOrigin, Any, CorsLayer};
use tower_http::trace::{DefaultMakeSpan, DefaultOnResponse, TraceLayer};
use tracing::Level;

use crate::AppState;
use crate::batch;
use crate::jobs::Jobs;
use crate::telemetry;

pub use error::ServiceError;

/// Operational knobs of the router (v2 Phase 8). `Default` is what the
/// binary uses without flags.
#[derive(Clone)]
pub struct RouterOptions {
    /// Deadline of every route but the batch one, queueing included.
    pub request_timeout: Duration,
    /// Deadline of `POST /batch/geocode`, resolution and streaming
    /// included (v3 Phase 11.6): past it the response is cut off.
    pub batch_timeout: Duration,
    /// Rows `POST /batch/geocode` resolves between two deadline checks
    /// (`batch::run::CHUNK_ROWS`).
    pub batch_chunk_rows: usize,
    /// Test hook: a pause before each batch chunk.
    #[doc(hidden)]
    pub batch_chunk_delay: Duration,
    /// Requests served at once across all routes; the rest queue (and
    /// time out) until a slot frees up.
    pub max_concurrency: usize,
    /// Allowed CORS origins; empty = any (`*`).
    pub cors_origins: Vec<HeaderValue>,
    /// Serve `GET /metrics` from this handle (`telemetry::install`).
    pub metrics: Option<PrometheusHandle>,
    /// Serve `/batch/jobs` from these jobs (`--jobs-dir`, v3 Phase 11.6);
    /// `None`: the routes answer 503.
    pub jobs: Option<Arc<Jobs>>,
    /// API keys and rate limits (`auth::Auth::new`, v3 Phase 11.7);
    /// `None`: every route is open.
    pub auth: Option<Arc<auth::Auth>>,
    /// Body limit of `POST /batch/geocode` (`--batch-max-body-mb`).
    pub batch_max_body_bytes: usize,
}

impl Default for RouterOptions {
    fn default() -> Self {
        Self {
            request_timeout: Duration::from_secs(10),
            batch_timeout: Duration::from_secs(600),
            batch_chunk_rows: batch::run::CHUNK_ROWS,
            batch_chunk_delay: Duration::ZERO,
            max_concurrency: 256,
            cors_origins: Vec::new(),
            metrics: None,
            jobs: None,
            auth: None,
            batch_max_body_bytes: batch::http::DEFAULT_BODY_LIMIT,
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
    let tuning = batch::BatchTuning {
        chunk_rows: opts.batch_chunk_rows,
        chunk_delay: opts.batch_chunk_delay,
        max_body_bytes: opts.batch_max_body_bytes,
    };
    let batch = Router::new()
        .route(
            "/batch/geocode",
            axum::routing::post(batch::batch_geocode)
                .route_layer(DefaultBodyLimit::max(opts.batch_max_body_bytes))
                .route_layer(Extension(tuning)),
        )
        .route_layer(middleware::from_fn_with_state(
            guard_for(opts.batch_timeout),
            guard,
        ));
    let jobs = match &opts.jobs {
        Some(j) => {
            // The upload has the batch deadline; its size is checked
            // while it streams to disk (`http::jobs::receive`).
            let submit = Router::new()
                .route(
                    "/batch/jobs",
                    post(jobs::submit).route_layer(DefaultBodyLimit::disable()),
                )
                .route_layer(middleware::from_fn_with_state(
                    guard_for(opts.batch_timeout),
                    guard,
                ))
                .with_state(j.clone());
            let manage = Router::new()
                .route("/batch/jobs/{id}", get(jobs::status).delete(jobs::delete))
                .route("/batch/jobs/{id}/result", get(jobs::result))
                .route_layer(middleware::from_fn_with_state(
                    guard_for(opts.request_timeout),
                    guard,
                ))
                .with_state(j.clone());
            submit.merge(manage)
        }
        None => Router::new()
            .route("/batch/jobs", post(jobs::disabled))
            .route(
                "/batch/jobs/{id}",
                get(jobs::disabled).delete(jobs::disabled),
            )
            .route("/batch/jobs/{id}/result", get(jobs::disabled)),
    };
    let mut app = api.merge(batch).merge(jobs);
    if let Some(handle) = opts.metrics.clone() {
        app = app.route(
            "/metrics",
            get(move || std::future::ready(metrics_body(&handle))),
        );
    }
    // Outermost first: count (every status, refusals included), then
    // admit, then the guard's slot and deadline.
    if let Some(a) = opts.auth.clone() {
        app = app.route_layer(middleware::from_fn_with_state(a, auth::check));
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
    // Mirrored rather than `*`, which doesn't cover `Authorization`.
    CorsLayer::new()
        .allow_origin(origin)
        .allow_methods(Any)
        .allow_headers(AllowHeaders::mirror_request())
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

/// The request's concurrency slot, as a request extension: a handler
/// whose work outlives its response (`POST /batch/geocode` streaming)
/// keeps a clone so the slot stays taken until the work ends.
#[derive(Clone)]
pub struct Permit(pub Arc<OwnedSemaphorePermit>);

/// When the request's deadline falls, as a request extension (the batch
/// route checks it between chunks, after its response has started).
#[derive(Debug, Clone, Copy)]
pub struct Deadline(pub Instant);

/// Bound a request's wait for a concurrency slot plus its handling by
/// `timeout`; past it, answer 503 (JSON). Handlers run their Tantivy work
/// on the blocking pool, so the deadline fires even mid-search (the
/// search itself finishes in the background).
///
/// The slot (`Permit`) and the deadline (`Deadline`) go to the handler as
/// request extensions.
pub async fn guard(State(g): State<Guard>, mut req: Request, next: Next) -> Response {
    let route = telemetry::route_of(&req);
    let timeout = g.timeout;
    req.extensions_mut()
        .insert(Deadline(Instant::now() + timeout));
    let run = async move {
        // The semaphore is never closed. The guard holds the slot until
        // the response (handlers may drop the request, and its
        // extensions, before they finish).
        let permit = g.permits.acquire_owned().await.ok().map(Arc::new);
        if let Some(p) = &permit {
            req.extensions_mut().insert(Permit(p.clone()));
        }
        let resp = next.run(req).await;
        drop(permit);
        resp
    };
    match tokio::time::timeout(timeout, run).await {
        Ok(resp) => resp,
        Err(_) => {
            telemetry::record_rejected(route, "timeout");
            ServiceError::Unavailable(format!(
                "request timed out after {} ms",
                timeout.as_millis()
            ))
            .into_response()
        }
    }
}
