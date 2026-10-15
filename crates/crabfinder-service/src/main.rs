//! crabfinder-service — Axum HTTP API. See `crabfinder_service::http` for
//! the routes; this binary only parses arguments and serves.

use std::net::{IpAddr, SocketAddr};
use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::Parser;
use tracing::info;
use tracing_subscriber::EnvFilter;

use crabfinder_service::{AppState, LoadOptions, http};

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
    opts.notability_enabled = !args.no_notability_boost;
    // Bind only after the state (incl. the R-tree) is fully built, so the
    // first request always sees a ready index.
    let state = AppState::load(&opts)?;

    let addr = SocketAddr::new(args.host, args.port);
    let listener = tokio::net::TcpListener::bind(addr).await?;
    info!("listening at http://{addr}");
    axum::serve(listener, http::router(state))
        .await
        .context("Axum serve loop")
}
