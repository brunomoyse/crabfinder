//! crabfinder-indexer binary.
//!
//! Phase 5: full Belgium coverage. Accepts a single `--region` for testing
//! (`brussels`, `flanders`, `wallonia`) or the default `all`, which
//! auto-detects every region with an `*Address*.zip` present in `--best-dir`
//! and indexes them into one Tantivy directory.

use std::path::{Path, PathBuf};
use std::time::Instant;

use anyhow::{Context, Result, anyhow};
use clap::Parser;
use tracing::{info, warn};
use tracing_subscriber::EnvFilter;

use crabfinder_indexer::bundle::detect_regions;
use crabfinder_indexer::index::CrabIndexWriter;
use crabfinder_indexer::meta::write_sidecar;
use crabfinder_indexer::mid_street::{MID_STREET_FILENAME, MidStreetBuilder};
use crabfinder_indexer::notability::NotabilityMap;
use crabfinder_indexer::pipeline::{IngestRegion, load_dictionaries, stream_resolved_addresses, StreamReport};

/// Expected per-region status-filter rejection rate, derived from the
/// Phase 5 Step 4 full-corpus indexing run (see `11-best-data-verified.md`
/// §9.3 — these are the *production-verified* figures, not the original
/// 500k-sample estimates).
///
/// When the indexer runs against a future BeSt dump and a region's actual
/// rejection rate drifts beyond `DRIFT_THRESHOLD_PCT` from these baselines,
/// we emit a warning. Rationale (post §9.3.1): the rejection rate is the
/// most sensitive indicator that BeSt's status mix has shifted across
/// years. We don't fail the build — drift is informational, not blocking —
/// but we want it visible without requiring anyone to remember §9.3 exists.
// TODO: calibrate against multiple historical drift observations once we
// have them. Today's 2.0 pp threshold is anchored to a single observed
// drift (Flanders retired 2.8% → 6.46%, 3.66 pp gap). If small drifts
// (0.5–1.5 pp) become operationally significant, this constant needs a
// real distribution behind it, not "round number above the single example."
const DRIFT_THRESHOLD_PCT: f64 = 2.0;
fn expected_status_skip_pct(region: IngestRegion) -> f64 {
    match region {
        IngestRegion::Brussels => 0.30,
        IngestRegion::Flanders => 6.46,
        IngestRegion::Wallonia => 0.00,
    }
}

/// CLI-only region selector. `All` resolves to whatever regions are present
/// in `--best-dir`; the named variants force a single region (useful for
/// testing / CI / smoke runs).
#[derive(Debug, Clone, Copy, clap::ValueEnum)]
enum RegionArg {
    All,
    Brussels,
    Flanders,
    Wallonia,
}

impl RegionArg {
    fn resolve(self, best_dir: &Path) -> Result<Vec<IngestRegion>> {
        match self {
            RegionArg::All => detect_regions(best_dir),
            RegionArg::Brussels => Ok(vec![IngestRegion::Brussels]),
            RegionArg::Flanders => Ok(vec![IngestRegion::Flanders]),
            RegionArg::Wallonia => Ok(vec![IngestRegion::Wallonia]),
        }
    }
}

#[derive(Debug, Parser)]
#[command(name = "crabfinder-indexer", version, about = "Index BeSt addresses into Tantivy.")]
struct Args {
    /// Directory containing the BeSt zip bundle (e.g. ./best-full-latest).
    #[arg(long)]
    best_dir: PathBuf,

    /// Which region(s) to ingest. `all` (default) auto-detects every region
    /// whose `*Address*.zip` is present in `--best-dir`. Single-region
    /// values (`brussels`, `flanders`, `wallonia`) are retained for testing.
    #[arg(long, value_enum, default_value_t = RegionArg::All)]
    region: RegionArg,

    /// Maximum number of records to emit *per region*. Useful for smoke
    /// runs; omitted in production. Applied independently to each region
    /// when `--region all` is in effect, so `--limit 1000 --region all`
    /// yields up to 3000 docs total.
    #[arg(long)]
    limit: Option<usize>,

    /// Output directory for the Tantivy index. Created if missing. One
    /// directory holds all regions when `--region all` is in effect.
    #[arg(long)]
    output: PathBuf,

    /// Print N sample addresses to stdout before indexing (debug aid).
    /// Applied per region.
    #[arg(long, default_value_t = 0)]
    print_samples: usize,

    /// Path to the notability whitelist YAML (file 13 §4 / schema v4).
    /// Optional — when absent, every doc gets `notability_score = 1.0`
    /// (no boost) and the resulting index behaves like v3 ranking.
    #[arg(long)]
    notability_whitelist: Option<PathBuf>,
}

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")))
        .with_target(false)
        .init();

    let args = Args::parse();
    info!("crabfinder-indexer starting; output={:?}", args.output);

    if args.output.as_os_str().is_empty() {
        return Err(anyhow!("--output is empty"));
    }

    let regions = args.region.resolve(&args.best_dir)?;
    info!(regions = ?regions, "regions selected");

    let notability_map = match args.notability_whitelist.as_ref() {
        Some(p) => NotabilityMap::load(p)
            .with_context(|| format!("loading notability whitelist {p:?}"))?,
        None => {
            info!("no --notability-whitelist supplied; all docs get notability_score = 1.0");
            NotabilityMap::empty()
        }
    };

    // One writer for the whole run. Tantivy commits at the end so a partial
    // failure leaves no half-built index.
    let t_overall = Instant::now();
    let mut writer =
        CrabIndexWriter::create(&args.output).context("opening Tantivy index for write")?;
    // Schema v7: collects (street_id, postal_code) → median-number coords
    // alongside Tantivy. Flushed after commit (see below). One instance is
    // shared across regions so the side-table covers the whole indexed corpus.
    let mut mid_street_builder = MidStreetBuilder::new();

    let mut per_region_timing: Vec<(IngestRegion, u128, u128, StreamReport, u64)> = Vec::new();
    let mut samples_printed = 0usize;

    for region in &regions {
        info!(?region, "=== ingesting region ===");
        let t_pass1 = Instant::now();
        let dicts = load_dictionaries(&args.best_dir, *region)
            .with_context(|| format!("Pass 1: loading dictionaries for {region:?}"))?;
        let pass1_ms = t_pass1.elapsed().as_millis();
        info!(?region, elapsed_ms = pass1_ms, "Pass 1 done");

        let t_pass2 = Instant::now();
        let mut stamped: u64 = 0;
        let report = stream_resolved_addresses(
            &args.best_dir,
            *region,
            &dicts,
            args.limit,
            |addr| {
                if samples_printed < args.print_samples {
                    samples_printed += 1;
                    println!(
                        "--- sample #{samples_printed} ({region:?}) ---\n{}",
                        serde_json::to_string_pretty(&addr)?
                    );
                }
                let boost = notability_map.boost_for(addr.building_group_id.as_str());
                if boost > 1.0 {
                    stamped += 1;
                }
                writer.add_address_with_notability(&addr, boost)?;
                // Also feed the mid_street side-table. Skipped silently for
                // unparseable house numbers / non-WGS84 coordinates — see
                // mid_street::record() for the exact semantics.
                mid_street_builder.record(&addr);
                Ok(true)
            },
        )
        .with_context(|| format!("Pass 2: streaming {region:?}"))?;
        let pass2_ms = t_pass2.elapsed().as_millis();
        info!(?region, elapsed_ms = pass2_ms, %report, notability_stamped = stamped, "Pass 2 done");

        // Drift alarm — see DRIFT_THRESHOLD_PCT comment for rationale.
        if report.parsed > 0 {
            let actual = 100.0 * report.skipped_status as f64 / report.parsed as f64;
            let expected = expected_status_skip_pct(*region);
            let drift = (actual - expected).abs();
            if drift > DRIFT_THRESHOLD_PCT {
                warn!(
                    ?region,
                    actual_skip_pct = format!("{actual:.2}%"),
                    expected_skip_pct = format!("{expected:.2}%"),
                    drift_pct = format!("{drift:.2}"),
                    threshold_pct = DRIFT_THRESHOLD_PCT,
                    "status-skip share drifted from the file 11 §9.3 baseline — investigate if this is the first time you've seen it. May indicate BeSt has shifted its status mix in this dump."
                );
            }
        }

        per_region_timing.push((*region, pass1_ms, pass2_ms, report, stamped));
    }

    // Commit + sidecar once for the whole index.
    let t_commit = Instant::now();
    writer.commit().context("committing Tantivy writer")?;
    write_sidecar(&args.output).context("writing .crab-meta.json sidecar")?;
    let commit_ms = t_commit.elapsed().as_millis();

    // Flush the v7 mid_street side-table. Done AFTER Tantivy commit so a
    // partial commit failure doesn't leave an orphan side-file.
    let t_mid = Instant::now();
    let mid_path = args.output.join(MID_STREET_FILENAME);
    let n_mid = mid_street_builder
        .finish(&mid_path)
        .with_context(|| format!("writing {MID_STREET_FILENAME}"))?;
    let mid_ms = t_mid.elapsed().as_millis();
    info!(
        path = %mid_path.display(),
        entries = n_mid,
        elapsed_ms = mid_ms,
        "mid_street side-table written"
    );

    let total_ms = t_overall.elapsed().as_millis();

    // --- Summary ---
    println!();
    println!("====== indexing summary ======");
    let mut total = StreamReport::default();
    let mut total_stamped: u64 = 0;
    for (region, p1, p2, rep, stamped) in &per_region_timing {
        println!(
            "  {:>9?}: pass1={:>5}ms  pass2={:>7}ms  {}  notability_stamped={}",
            region, p1, p2, rep, stamped
        );
        total.parsed += rep.parsed;
        total.emitted += rep.emitted;
        total.skipped_status += rep.skipped_status;
        total.skipped_unresolved += rep.skipped_unresolved;
        total_stamped += stamped;
    }
    println!(
        "     TOTAL: {}  notability_stamped={} (whitelist entries={}, unique bgids={})",
        total, total_stamped, notability_map.n_entries, notability_map.n_unique_bgids
    );
    println!("  commit: {commit_ms}ms");
    println!("  wall-clock total: {total_ms}ms ({:.1} min)", total_ms as f64 / 60_000.0);

    if let Ok(size) = total_dir_size(&args.output) {
        let mb = size as f64 / 1024.0 / 1024.0;
        let gb = mb / 1024.0;
        println!("  index size on disk: {size} bytes ({mb:.1} MB / {gb:.2} GB)");
    }
    Ok(())
}

fn total_dir_size(p: &std::path::Path) -> Result<u64> {
    let mut total = 0u64;
    for entry in std::fs::read_dir(p)? {
        let entry = entry?;
        let m = entry.metadata()?;
        if m.is_file() {
            total += m.len();
        } else if m.is_dir() {
            total += total_dir_size(&entry.path())?;
        }
    }
    Ok(total)
}
