//! CLI entry. Dispatches between modes (generate / run / diff / validate).
//!
//! File 14 §4 describes the long-term CLI surface. MVP modes implemented
//! incrementally — generate-eval-set first (Step 3), then run + dry-run +
//! validate (Step 5), then diff (Step 8).

use std::path::PathBuf;

use anyhow::Result;
use clap::Parser;

#[derive(Debug, Parser)]
#[command(
    name = "crabfinder-eval",
    version,
    about = "crabfinder evaluation runner"
)]
pub struct Args {
    // --- Generate mode (Step 3) ---------------------------------------------
    /// Generate a synthetic eval set from an existing Tantivy index.
    /// Requires --index, --output, optionally --seed (default 1) and
    /// --dump-date (informational; embedded in the eval set).
    #[arg(long)]
    pub generate_eval_set: bool,

    /// Use the MVP sampling profile (100/250/150 random + 200 boosted
    /// cross_language). Only meaningful with --generate-eval-set.
    #[arg(long)]
    pub mvp: bool,

    /// Path to a Tantivy index directory (for --generate-eval-set or
    /// --validate-eval-set).
    #[arg(long)]
    pub index: Option<PathBuf>,

    /// Output path for the generated eval set (JSON) or run report (JSON).
    #[arg(long)]
    pub output: Option<PathBuf>,

    /// Output path for the run report's Markdown summary.
    #[arg(long)]
    pub output_md: Option<PathBuf>,

    /// Seeded RNG for reproducible sampling (default 1).
    #[arg(long, default_value_t = 1u64)]
    pub seed: u64,

    /// Uniform multiplier on the MVP per-region targets (default 1.0).
    /// Useful for tightening confidence intervals via larger samples.
    #[arg(long, default_value_t = 1.0f64)]
    pub scale: f64,

    /// BeSt dump date embedded in the eval set's `corpus.dump_date` field.
    #[arg(long, default_value = "2026-05-19")]
    pub dump_date: String,

    // --- Run mode (Step 5) --------------------------------------------------
    /// One or more eval-set files. JSON → synthetic, YAML/YML → adversarial.
    /// Repeatable. Auto-dispatched via EvalSet::auto_load.
    #[arg(long = "eval-set")]
    pub eval_sets: Vec<PathBuf>,

    /// Base URL of a running crabfinder-service (HTTP mode).
    #[arg(long, default_value = "http://127.0.0.1:8080")]
    pub service: String,

    /// Concurrent in-flight requests against the service.
    #[arg(long, default_value_t = 8usize)]
    pub parallel: usize,

    /// Project the full run time from the first 50 queries; don't write a
    /// final report.
    #[arg(long)]
    pub dry_run: bool,

    /// Check that every expected_best_id / expected_building_group_id in
    /// the eval set still exists in the live index. Fails if missing share
    /// exceeds --max-missing-pct.
    #[arg(long)]
    pub validate_eval_set: bool,

    #[arg(long, default_value_t = 5.0f64)]
    pub max_missing_pct: f64,

    // --- Batch mode (Lot G) -------------------------------------------------
    /// Run the resolver in-process against the eval set(s) and write a
    /// confidence-threshold calibration JSON. Requires --index, --output,
    /// and at least one --eval-set.
    ///
    /// Distinct from the HTTP runner — no service needed, no reqwest hop.
    /// Reads `street_match_score` directly from `ResolvedAddress` so the
    /// quartile bands match the resolver's internal numbers exactly.
    #[arg(long)]
    pub batch_mode: bool,

    /// Optional abbreviation dictionary (matches the HTTP service's
    /// `--abbreviation-dict` flag). Without it, batch mode runs with an
    /// empty (identity) map — same as the service started bare.
    #[arg(long)]
    pub abbreviation_dict: Option<PathBuf>,

    /// Disable the MedianStreet fallback during batch-mode resolution.
    /// Matches the HTTP service's `--no-mid-street` flag; useful to A/B
    /// the threshold quartiles with vs. without the fallback.
    #[arg(long, default_value_t = false)]
    pub no_mid_street: bool,

    /// Disable the notability tweak_score during batch-mode resolution.
    /// Matches the HTTP service's `--no-notability-boost`.
    #[arg(long, default_value_t = false)]
    pub no_notability_boost: bool,

    // --- Diff mode (Step 8) -------------------------------------------------
    /// Path to the baseline report.json for --diff.
    #[arg(long)]
    pub baseline: Option<PathBuf>,

    /// Path to the current report.json for --diff.
    #[arg(long)]
    pub current: Option<PathBuf>,

    /// Output path for the diff Markdown.
    #[arg(long)]
    pub diff_output: Option<PathBuf>,
}

pub fn run() -> Result<()> {
    let args = Args::parse();

    if args.generate_eval_set {
        return run_generate(&args);
    }
    if args.validate_eval_set {
        return run_validate(&args);
    }
    if args.baseline.is_some() && args.current.is_some() {
        return run_diff(&args);
    }
    if args.batch_mode {
        return run_batch_mode(&args);
    }
    if !args.eval_sets.is_empty() {
        return run_eval(&args);
    }
    anyhow::bail!(
        "no mode selected — pass one of: --generate-eval-set, --validate-eval-set, \
         --batch-mode, --baseline+--current, or --eval-set"
    )
}

fn run_generate(args: &Args) -> Result<()> {
    let index = args
        .index
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("--generate-eval-set requires --index"))?;
    let output = args
        .output
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("--generate-eval-set requires --output"))?;
    if !args.mvp {
        anyhow::bail!("MVP scaffold supports only --mvp profile (Phase 6.5b will add others)");
    }
    crate::generator::generate_mvp(index, output, args.seed, &args.dump_date, args.scale)
}

fn run_validate(args: &Args) -> Result<()> {
    if args.eval_sets.is_empty() {
        anyhow::bail!("--validate-eval-set requires at least one --eval-set");
    }
    let queries = load_and_flatten(&args.eval_sets)?;
    let config = build_runner_config(args);
    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(crate::runner::validate_eval_set(
        &config,
        queries,
        args.max_missing_pct,
    ))
}

fn run_eval(args: &Args) -> Result<()> {
    let queries = load_and_flatten(&args.eval_sets)?;
    let config = build_runner_config(args);
    let rt = tokio::runtime::Runtime::new()?;

    if args.dry_run {
        return rt.block_on(crate::runner::dry_run(&config, queries));
    }

    // Full run: execute, compute metrics, write reports.
    let output = args
        .output
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("--output is required for a full eval run"))?;
    let results = rt.block_on(crate::runner::run_eval(&config, queries))?;

    let metrics = crate::metrics::compute(&results);
    crate::report::write_reports(
        &metrics,
        &results,
        &args.service,
        output,
        args.output_md.as_deref(),
    )?;
    Ok(())
}

fn run_batch_mode(args: &Args) -> Result<()> {
    let index = args
        .index
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("--batch-mode requires --index"))?;
    let output = args
        .output
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("--batch-mode requires --output"))?;
    if args.eval_sets.is_empty() {
        anyhow::bail!("--batch-mode requires at least one --eval-set");
    }
    let queries = load_and_flatten(&args.eval_sets)?;
    let config = crate::batch_mode::BatchModeConfig {
        index: index.clone(),
        abbreviation_dict: args.abbreviation_dict.clone(),
        output: output.clone(),
        no_mid_street: args.no_mid_street,
        notability_enabled: !args.no_notability_boost,
    };
    crate::batch_mode::run(&config, queries, &args.eval_sets)
}

fn run_diff(args: &Args) -> Result<()> {
    let baseline = args
        .baseline
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("--baseline required for diff"))?;
    let current = args
        .current
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("--current required for diff"))?;
    let output = args
        .diff_output
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("--diff-output required for diff"))?;
    crate::report::write_diff(baseline, current, output)
}

fn load_and_flatten(paths: &[PathBuf]) -> Result<Vec<crate::eval_set::EvalQuery>> {
    let mut sets = Vec::with_capacity(paths.len());
    for p in paths {
        sets.push(crate::eval_set::EvalSet::auto_load(p)?);
    }
    Ok(crate::eval_set::flatten(&sets))
}

fn build_runner_config(args: &Args) -> crate::runner::RunnerConfig {
    crate::runner::RunnerConfig {
        service_url: args.service.clone(),
        parallel: args.parallel,
        ..Default::default()
    }
}
