//! crabfinder-indexer binary. All logic lives in `crabfinder_indexer::cli`.

use anyhow::Result;
use clap::Parser;
use tracing_subscriber::EnvFilter;

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .with_target(false)
        .init();
    let report = crabfinder_indexer::cli::run(&crabfinder_indexer::cli::Args::parse())?;
    println!();
    print!("{}", report.summary());
    Ok(())
}
