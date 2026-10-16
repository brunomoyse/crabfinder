//! crabfinder-service — Axum HTTP API. See `crabfinder_service::http` for
//! the routes; this binary only parses arguments and serves.

use std::net::{IpAddr, SocketAddr};
use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Context, Result};
use clap::Parser;
use tracing::info;
use tracing_subscriber::EnvFilter;

use axum::http::HeaderValue;
use crabfinder_service::http::{self, RouterOptions, auth};
use crabfinder_service::jobs::{Jobs, JobsConfig};
use crabfinder_service::{AppState, LoadOptions, telemetry};

#[derive(Debug, Parser)]
#[command(
    name = "crabfinder-service",
    version,
    about = "Serve crabfinder over HTTP."
)]
struct Args {
    /// Path to the Tantivy index directory.
    #[arg(long, env = "CRABFINDER_INDEX")]
    index: PathBuf,

    /// IP address to bind. Defaults to IPv4 loopback; containers set
    /// `0.0.0.0` so the published port is reachable from outside.
    #[arg(long, env = "CRABFINDER_HOST", default_value = "127.0.0.1")]
    host: IpAddr,

    /// TCP port to listen on.
    #[arg(long, env = "CRABFINDER_PORT", default_value_t = 8080)]
    port: u16,

    /// Rank by pure BM25, bypassing the notability multiplier. Used to A/B
    /// isolate the boost on the *same* physical index.
    #[arg(long, env = "CRABFINDER_NO_NOTABILITY_BOOST")]
    no_notability_boost: bool,

    /// Path to `street-type-abbreviations-v1.yaml` (query-time `av` →
    /// `avenue` expansion). Optional.
    #[arg(long, env = "CRABFINDER_ABBREVIATION_DICT")]
    abbreviation_dict: Option<PathBuf>,

    /// Path to the OSM landmarks CSV (`data/osm-pois-be-v1.csv`): queries
    /// naming a landmark ("Manneken Pis") answer with the BeSt addresses
    /// around it. Optional, off by default; only helps interpret queries.
    #[arg(long, env = "CRABFINDER_POIS")]
    pois: Option<PathBuf>,

    /// Deadline of a request (queueing for a slot included), milliseconds;
    /// past it the service answers 503.
    #[arg(long, env = "CRABFINDER_REQUEST_TIMEOUT_MS", default_value_t = 10_000)]
    request_timeout_ms: u64,

    /// Deadline of `POST /batch/geocode`, seconds: resolution and
    /// streaming included (past it the response is cut off).
    #[arg(long, env = "CRABFINDER_BATCH_TIMEOUT_SECS", default_value_t = 600)]
    batch_timeout_secs: u64,

    /// Requests served at once; the rest wait for a slot.
    #[arg(long, env = "CRABFINDER_MAX_CONCURRENCY", default_value_t = 256)]
    max_concurrency: usize,

    /// Allowed CORS origin (repeatable). Default: any (`*`).
    #[arg(
        long = "cors-origin",
        env = "CRABFINDER_CORS_ORIGIN",
        value_delimiter = ','
    )]
    cors_origins: Vec<String>,

    /// Don't serve Prometheus metrics at `/metrics`.
    #[arg(long, env = "CRABFINDER_NO_METRICS")]
    no_metrics: bool,

    /// Don't build the `/autocomplete` name index (saves ~1 s and its
    /// memory at startup; `/autocomplete` then answers 503).
    #[arg(long, env = "CRABFINDER_NO_AUTOCOMPLETE")]
    no_autocomplete: bool,

    /// Directory of the asynchronous batch jobs (`/batch/jobs`). Without
    /// it, those routes answer 503.
    #[arg(long, env = "CRABFINDER_JOBS_DIR")]
    jobs_dir: Option<PathBuf>,

    /// Batch jobs running at once; the others wait, queued.
    #[arg(long, env = "CRABFINDER_JOBS_MAX_RUNNING", default_value_t = 2)]
    jobs_max_running: usize,

    /// Upload limit of `POST /batch/jobs`, MiB.
    #[arg(long, env = "CRABFINDER_JOBS_MAX_BODY_MB", default_value_t = 1024)]
    jobs_max_body_mb: u64,

    /// Hours a finished job (and its result) is kept.
    #[arg(long, env = "CRABFINDER_JOBS_TTL_HOURS", default_value_t = 24)]
    jobs_ttl_hours: u64,

    /// Resolver threads of the batch jobs; 0 = half the cores.
    #[arg(long, env = "CRABFINDER_JOBS_THREADS", default_value_t = 0)]
    jobs_threads: usize,

    /// API keys file (YAML, see `http::auth`): every route but `/health`
    /// (and `/metrics`) then needs a key. Default: no keys.
    #[arg(long, env = "CRABFINDER_API_KEYS")]
    api_keys: Option<PathBuf>,

    /// Requests per minute per client address (bursts up to as many).
    #[arg(long, env = "CRABFINDER_RATE_LIMIT_PER_IP")]
    rate_limit_per_ip: Option<u32>,

    /// With `--api-keys`, `/metrics` needs a key too.
    #[arg(long, env = "CRABFINDER_METRICS_REQUIRE_KEY")]
    metrics_require_key: bool,

    /// Body limit of `POST /batch/geocode`, MiB.
    #[arg(long, env = "CRABFINDER_BATCH_MAX_BODY_MB", default_value_t = 50)]
    batch_max_body_mb: usize,
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .with_target(false)
        .init();

    let args = Args::parse();
    info!(index = %args.index.display(), port = args.port, "starting crabfinder-service");

    let mut opts = LoadOptions::new(&args.index);
    opts.abbreviation_dict = args.abbreviation_dict;
    opts.pois = args.pois;
    opts.notability_enabled = !args.no_notability_boost;
    opts.autocomplete = !args.no_autocomplete;
    // Bind only after the state (incl. the R-tree) is fully built, so the
    // first request always sees a ready index.
    let state = AppState::load(&opts)?;

    let cors_origins = args
        .cors_origins
        .iter()
        .filter(|o| o.as_str() != "*")
        .map(|o| HeaderValue::from_str(o).with_context(|| format!("invalid --cors-origin {o:?}")))
        .collect::<Result<Vec<_>>>()?;
    let metrics = (!args.no_metrics).then(|| {
        let h = telemetry::install();
        telemetry::spawn_upkeep(h.clone(), Duration::from_secs(5));
        h
    });
    let jobs = match &args.jobs_dir {
        Some(dir) => {
            let cfg = JobsConfig {
                max_running: args.jobs_max_running,
                max_body_bytes: args.jobs_max_body_mb * 1024 * 1024,
                ttl: Duration::from_secs(args.jobs_ttl_hours * 3600),
                threads: args.jobs_threads,
                ..JobsConfig::new(dir)
            };
            info!(dir = %dir.display(), "batch jobs on");
            Some(Jobs::open(state.clone(), cfg)?)
        }
        None => None,
    };
    let keys = match &args.api_keys {
        Some(p) => {
            let keys = auth::load_keys(p)?;
            info!(keys = keys.len(), "API keys on");
            keys
        }
        None => Vec::new(),
    };
    let auth = auth::Auth::new(keys, args.rate_limit_per_ip, args.metrics_require_key);
    let router_opts = RouterOptions {
        request_timeout: Duration::from_millis(args.request_timeout_ms),
        batch_timeout: Duration::from_secs(args.batch_timeout_secs),
        max_concurrency: args.max_concurrency,
        cors_origins,
        metrics,
        jobs,
        auth,
        batch_max_body_bytes: args.batch_max_body_mb * 1024 * 1024,
        ..RouterOptions::default()
    };

    let addr = SocketAddr::new(args.host, args.port);
    let listener = tokio::net::TcpListener::bind(addr).await?;
    info!("listening at http://{addr}");
    // Client addresses for `--rate-limit-per-ip`.
    let app =
        http::router_with(state, &router_opts).into_make_service_with_connect_info::<SocketAddr>();
    axum::serve(listener, app).await.context("Axum serve loop")
}
