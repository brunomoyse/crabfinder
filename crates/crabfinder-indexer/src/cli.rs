//! The `crabfinder-indexer` command: BeSt bundle → Tantivy index +
//! side-files. Lives in the library so tests can drive a full indexing run
//! in-process; `src/main.rs` only parses arguments and prints the report.

use std::path::{Path, PathBuf};
use std::time::Instant;

use anyhow::{Context, Result, anyhow};
use clap::Parser;
use tracing::{info, warn};

use crate::bundle::detect_regions;
use crate::index::CrabIndexWriter;
use crate::meta::write_sidecar;
use crate::mid_street::{MID_STREET_FILENAME, MidStreetBuilder};
use crate::notability::NotabilityMap;
use crate::pipeline::{IngestRegion, StreamReport, load_dictionaries, stream_resolved_addresses};

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
pub enum RegionArg {
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

#[derive(Debug, Clone, Parser)]
#[command(
    name = "crabfinder-indexer",
    version,
    about = "Index BeSt addresses into Tantivy."
)]
pub struct Args {
    /// Directory containing the BeSt zip bundle (e.g. ./best-full-latest).
    #[arg(long)]
    pub best_dir: PathBuf,

    /// Which region(s) to ingest. `all` (default) auto-detects every region
    /// whose `*Address*.zip` is present in `--best-dir`. Single-region
    /// values (`brussels`, `flanders`, `wallonia`) are retained for testing.
    #[arg(long, value_enum, default_value_t = RegionArg::All)]
    pub region: RegionArg,

    /// Maximum number of records to emit *per region*. Useful for smoke
    /// runs; omitted in production. Applied independently to each region
    /// when `--region all` is in effect, so `--limit 1000 --region all`
    /// yields up to 3000 docs total.
    #[arg(long)]
    pub limit: Option<usize>,

    /// Output directory for the Tantivy index. Created if missing. One
    /// directory holds all regions when `--region all` is in effect.
    #[arg(long)]
    pub output: PathBuf,

    /// Print N sample addresses to stdout before indexing (debug aid).
    /// Applied per region.
    #[arg(long, default_value_t = 0)]
    pub print_samples: usize,

    /// Path to the notability whitelist YAML (file 13 §4 / schema v4).
    /// Optional — when absent, every doc gets `notability_score = 1.0`
    /// (no boost) and the resulting index behaves like v3 ranking.
    #[arg(long)]
    pub notability_whitelist: Option<PathBuf>,
}

impl Args {
    /// Minimal args: index everything in `best_dir` into `output`.
    pub fn new(best_dir: impl Into<PathBuf>, output: impl Into<PathBuf>) -> Self {
        Self {
            best_dir: best_dir.into(),
            region: RegionArg::All,
            limit: None,
            output: output.into(),
            print_samples: 0,
            notability_whitelist: None,
        }
    }
}

/// Per-region outcome of a run.
#[derive(Debug, Clone)]
pub struct RegionReport {
    pub region: IngestRegion,
    pub pass1_ms: u128,
    pub pass2_ms: u128,
    pub stream: StreamReport,
    pub notability_stamped: u64,
    /// `Some(actual_pct)` when the status-skip share drifted beyond
    /// `DRIFT_THRESHOLD_PCT` from the expected baseline.
    pub status_drift_pct: Option<f64>,
}

/// Outcome of a whole indexing run.
#[derive(Debug, Clone)]
pub struct IndexReport {
    pub regions: Vec<RegionReport>,
    pub commit_ms: u128,
    pub total_ms: u128,
    pub mid_street_entries: usize,
    pub whitelist_entries: usize,
    pub whitelist_unique_bgids: usize,
    pub output: PathBuf,
}

impl IndexReport {
    /// Sum of every region's stream report.
    pub fn total(&self) -> StreamReport {
        let mut t = StreamReport::default();
        for r in &self.regions {
            t.parsed += r.stream.parsed;
            t.emitted += r.stream.emitted;
            t.skipped_status += r.stream.skipped_status;
            t.skipped_unresolved += r.stream.skipped_unresolved;
        }
        t
    }

    /// Human-readable summary, as printed by the binary.
    pub fn summary(&self) -> String {
        use std::fmt::Write;
        let mut s = String::from("====== indexing summary ======\n");
        for r in &self.regions {
            let _ = writeln!(
                s,
                "  {:>9?}: pass1={:>5}ms  pass2={:>7}ms  {}  notability_stamped={}",
                r.region, r.pass1_ms, r.pass2_ms, r.stream, r.notability_stamped
            );
        }
        let stamped: u64 = self.regions.iter().map(|r| r.notability_stamped).sum();
        let _ = writeln!(
            s,
            "     TOTAL: {}  notability_stamped={} (whitelist entries={}, unique bgids={})",
            self.total(),
            stamped,
            self.whitelist_entries,
            self.whitelist_unique_bgids
        );
        let _ = writeln!(s, "  mid_street entries: {}", self.mid_street_entries);
        let _ = writeln!(s, "  commit: {}ms", self.commit_ms);
        let _ = writeln!(
            s,
            "  wall-clock total: {}ms ({:.1} min)",
            self.total_ms,
            self.total_ms as f64 / 60_000.0
        );
        if let Ok(size) = total_dir_size(&self.output) {
            let mb = size as f64 / 1024.0 / 1024.0;
            let _ = writeln!(
                s,
                "  index size on disk: {size} bytes ({mb:.1} MB / {:.2} GB)",
                mb / 1024.0
            );
        }
        s
    }
}

/// Status-skip share in percent, or `None` when it stays within
/// `DRIFT_THRESHOLD_PCT` of the region's baseline.
fn status_drift(region: IngestRegion, report: &StreamReport) -> Option<f64> {
    if report.parsed == 0 {
        return None;
    }
    let actual = 100.0 * report.skipped_status as f64 / report.parsed as f64;
    let expected = expected_status_skip_pct(region);
    ((actual - expected).abs() > DRIFT_THRESHOLD_PCT).then_some(actual)
}

/// Index every selected region of `args.best_dir` into `args.output`.
pub fn run(args: &Args) -> Result<IndexReport> {
    info!("crabfinder-indexer starting; output={:?}", args.output);
    if args.output.as_os_str().is_empty() {
        return Err(anyhow!("--output is empty"));
    }

    let regions = args.region.resolve(&args.best_dir)?;
    info!(regions = ?regions, "regions selected");

    let notability_map = match args.notability_whitelist.as_ref() {
        Some(p) => {
            NotabilityMap::load(p).with_context(|| format!("loading notability whitelist {p:?}"))?
        }
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
    // Schema v7: (street_id, postal_code) → median-number coords, shared
    // across regions and flushed after commit.
    let mut mid_street_builder = MidStreetBuilder::new();

    let mut reports = Vec::new();
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
        let stream =
            stream_resolved_addresses(&args.best_dir, *region, &dicts, args.limit, |addr| {
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
                mid_street_builder.record(&addr);
                Ok(true)
            })
            .with_context(|| format!("Pass 2: streaming {region:?}"))?;
        let pass2_ms = t_pass2.elapsed().as_millis();
        info!(?region, elapsed_ms = pass2_ms, %stream, notability_stamped = stamped, "Pass 2 done");

        let status_drift_pct = status_drift(*region, &stream);
        if let Some(actual) = status_drift_pct {
            warn!(
                ?region,
                actual_skip_pct = format!("{actual:.2}%"),
                expected_skip_pct = format!("{:.2}%", expected_status_skip_pct(*region)),
                threshold_pct = DRIFT_THRESHOLD_PCT,
                "status-skip share drifted from the file 11 §9.3 baseline — may indicate BeSt has shifted its status mix in this dump"
            );
        }
        reports.push(RegionReport {
            region: *region,
            pass1_ms,
            pass2_ms,
            stream,
            notability_stamped: stamped,
            status_drift_pct,
        });
    }

    let t_commit = Instant::now();
    writer.commit().context("committing Tantivy writer")?;
    write_sidecar(&args.output).context("writing .crab-meta.json sidecar")?;
    let commit_ms = t_commit.elapsed().as_millis();

    // Flushed AFTER the Tantivy commit so a commit failure doesn't leave an
    // orphan side-file.
    let mid_path = args.output.join(MID_STREET_FILENAME);
    let mid_street_entries = mid_street_builder
        .finish(&mid_path)
        .with_context(|| format!("writing {MID_STREET_FILENAME}"))?;
    info!(path = %mid_path.display(), entries = mid_street_entries, "mid_street side-table written");

    Ok(IndexReport {
        regions: reports,
        commit_ms,
        total_ms: t_overall.elapsed().as_millis(),
        mid_street_entries,
        whitelist_entries: notability_map.n_entries,
        whitelist_unique_bgids: notability_map.n_unique_bgids,
        output: args.output.clone(),
    })
}

fn total_dir_size(p: &Path) -> Result<u64> {
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
