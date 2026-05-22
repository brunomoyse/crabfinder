//! crabfinder-eval — MVP entry point.
//!
//! File 14 §4 describes the full CLI surface; the MVP implements:
//!   --generate-eval-set
//!   --eval-set (multi)
//!   --service
//!   --output / --output-md
//!   --dry-run
//!   --validate-eval-set
//!   --baseline / --current / --diff-output (minimal diff)
//!
//! Deferred to Phase 6.5b (NOT in this binary yet):
//!   --latency-pass, --fail-on-redline, --mode=in-process, --repeat N

use anyhow::Result;

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .with_target(false)
        .init();

    crabfinder_eval::cli::run()
}
