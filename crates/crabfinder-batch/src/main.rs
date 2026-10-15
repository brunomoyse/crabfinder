//! crabfinder-batch binary. All logic lives in the library (`src/lib.rs`).

use anyhow::Result;
use clap::Parser;
use tracing_subscriber::EnvFilter;

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .with_target(false)
        .with_writer(std::io::stderr) // keep stdout clean for the CSV output
        .init();
    crabfinder_batch::run(&crabfinder_batch::Args::parse()).map(|_| ())
}
