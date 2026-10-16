//! crabfinder-batch — standalone CLI batch geocoder.
//!
//! Reads a CSV (stdin or `--input`), resolves every row against the same
//! Tantivy index the HTTP service uses, writes the enriched CSV or GeoJSON
//! (`--format`, stdout or `--output`). A CSV with an `address` column is
//! free text, parsed like `/geocode?q=`. Same resolver, same output schema, same fallbacks as the
//! HTTP `/batch/geocode` endpoint — this crate is a peer of
//! `crabfinder-service`, not a wrapper around it.
//!
//! Concurrency: the resolver runs through `rayon::par_iter` (see
//! `crabfinder_service::batch::dedup_then_resolve`) inside a pool sized by
//! `--parallel N` (default = all available cores). Rows are resolved and
//! written 5,000 at a time (`batch::run`, v3 Phase 11.6); `--progress`
//! reports rows/s and the time left on stderr after each chunk.
//!
//! Multi-process Tantivy reads (CLI alongside a running service on the same
//! index directory) are supported because Tantivy's `MmapDirectory` is
//! read-shared across processes. See `docs/v1-batch-plan.md` §4.5.

use std::fs::File;
use std::io::{BufWriter, Read, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use clap::Parser;
use tracing::{info, warn};

use crabfinder_core::Language;
use crabfinder_service::batch::dedup::RowExtras;
use crabfinder_service::batch::io::{
    InputRow, OutputFormat, OutputSink, read_rows, sniff_delimiter, write_geojson,
};
use crabfinder_service::batch::run::{CHUNK_ROWS, RunOptions, resolve_chunks};
use crabfinder_service::{AppState, LoadOptions};

#[derive(Debug, Clone, Parser)]
#[command(
    name = "crabfinder-batch",
    version,
    about = "Resolve a CSV of addresses against a crabfinder Tantivy index."
)]
pub struct Args {
    /// Tantivy index directory (must match the binary's schema version).
    #[arg(long, env = "CRABFINDER_INDEX")]
    pub index: PathBuf,

    /// Path to `street-type-abbreviations-v1.yaml`. Optional.
    #[arg(long, env = "CRABFINDER_ABBREVIATION_DICT")]
    pub abbreviation_dict: Option<PathBuf>,

    /// Path to the OSM landmarks CSV (`data/osm-pois-be-v1.csv`). Optional.
    #[arg(long, env = "CRABFINDER_POIS")]
    pub pois: Option<PathBuf>,

    /// Input CSV path. Use `-` (or omit) to read from stdin.
    #[arg(long, short)]
    pub input: Option<PathBuf>,

    /// Output CSV path. Use `-` (or omit) to write to stdout.
    #[arg(long, short)]
    pub output: Option<PathBuf>,

    /// Override the sniffed CSV delimiter for input (`,` or `;`). When
    /// unset, the first line decides.
    #[arg(long)]
    pub delimiter: Option<char>,

    /// Disable the street-level fallbacks: rows whose house number can't
    /// be matched come back `not_found`. Same toggle as the HTTP
    /// endpoint's `mid_street_fallback=false` option.
    #[arg(long, default_value_t = false)]
    pub no_mid_street: bool,

    /// Number of worker threads for resolution. Default = all cores. Set
    /// to 1 for deterministic profiling.
    #[arg(long)]
    pub parallel: Option<usize>,

    /// Output format: `csv` or `geojson`.
    #[arg(long, default_value = "csv", value_parser = parse_format)]
    pub format: OutputFormat,

    /// Language of the `label` column: `fr`, `nl` or `de`. Default: each
    /// address's own language.
    #[arg(long, value_parser = parse_lang)]
    pub lang: Option<Language>,

    /// Add the `/validate` columns `status` and `corrections` (v3 Phase
    /// 11.2): what differs between each row and the official address.
    #[arg(long, default_value_t = false)]
    pub corrections: bool,

    /// Add the `score_gap` and `alternatives` columns (v3 Phase 11.3): the
    /// municipalities of the best answers, for rows that don't say which.
    #[arg(long, default_value_t = false)]
    pub alternatives: bool,

    /// Report progress on stderr (rows done, rows/s, time left) after
    /// each chunk of 5,000 rows.
    #[arg(long, default_value_t = false)]
    pub progress: bool,
}

fn parse_format(s: &str) -> Result<OutputFormat, String> {
    s.parse()
}

fn parse_lang(s: &str) -> Result<Language, String> {
    s.trim()
        .to_ascii_lowercase()
        .parse()
        .map_err(|_| format!("unknown lang '{s}' (expected fr, nl or de)"))
}

impl Args {
    /// Minimal args for `index`; everything else at its CLI default.
    pub fn for_index(index: impl Into<PathBuf>) -> Self {
        Self {
            index: index.into(),
            abbreviation_dict: None,
            pois: None,
            input: None,
            output: None,
            delimiter: None,
            no_mid_street: false,
            parallel: None,
            format: OutputFormat::Csv,
            lang: None,
            corrections: false,
            alternatives: false,
            progress: false,
        }
    }
}

/// What a run did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Summary {
    pub rows: usize,
}

/// Run the CLI: read `--input` (or stdin), write `--output` (or stdout).
pub fn run(args: &Args) -> Result<Summary> {
    let input: Box<dyn Read> = match non_dash(args.input.as_deref()) {
        Some(p) => Box::new(File::open(p).with_context(|| format!("reading input {p:?}"))?),
        None => Box::new(std::io::stdin().lock()),
    };
    let output: Box<dyn Write + Send> = match non_dash(args.output.as_deref()) {
        Some(p) => Box::new(BufWriter::new(
            File::create(p).with_context(|| format!("creating {p:?}"))?,
        )),
        None => Box::new(BufWriter::new(std::io::stdout())),
    };
    run_with_io(args, input, output)
}

/// `Some(p)` unless the path is absent or `-`.
fn non_dash(p: Option<&Path>) -> Option<&Path> {
    p.filter(|p| p.as_os_str() != "-")
}

/// Same as `run`, with explicit I/O streams (`--input` / `--output` are
/// ignored).
pub fn run_with_io(
    args: &Args,
    mut input: impl Read,
    output: impl Write + Send,
) -> Result<Summary> {
    let mut opts = LoadOptions::new(&args.index);
    opts.abbreviation_dict = args.abbreviation_dict.clone();
    opts.pois = args.pois.clone();
    opts.spatial = false;
    opts.autocomplete = false;
    let state = AppState::load(&opts)?;

    let mut raw = Vec::new();
    input.read_to_end(&mut raw).context("reading input")?;
    let rows = parse_rows(raw, args.delimiter)?;
    info!(n_rows = rows.len(), "batch input loaded");
    if rows.is_empty() {
        warn!("input is empty; no work to do");
        if args.format == OutputFormat::GeoJson {
            write_geojson(&[], output).context("writing geojson")?;
        }
        return Ok(Summary { rows: 0 });
    }

    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(args.parallel.unwrap_or(0))
        .build()
        .context("configuring rayon thread pool")?;
    let options = RunOptions {
        mid_street_fallback: !args.no_mid_street,
        format: args.format,
        extras: RowExtras {
            corrections: args.corrections,
            alternatives: args.alternatives,
            lang: args.lang,
        },
    };
    let total = rows.len();
    let id_columns = rows.iter().any(|r| r.best_id.is_some());
    let mut sink = OutputSink::new(args.format, output, 0);
    let t0 = Instant::now();
    let report = |done: usize| {
        if args.progress {
            eprintln!("{}", progress_line(done, total, t0.elapsed()));
        }
        true
    };
    let end = pool.install(|| {
        resolve_chunks(
            &state,
            rows.into_iter().map(Ok),
            &options,
            id_columns,
            CHUNK_ROWS,
            &mut sink,
            &report,
        )
    })?;
    report(end.rows);
    sink.finish().context("writing output")?;
    info!(
        out_rows = end.rows,
        elapsed_ms = t0.elapsed().as_millis(),
        "resolution complete"
    );
    Ok(Summary { rows: end.rows })
}

/// `rows 5000/20000 (25.0%), 3120 rows/s, 4.8 s left`.
pub fn progress_line(done: usize, total: usize, elapsed: Duration) -> String {
    let secs = elapsed.as_secs_f64();
    let rate = if secs > 0.0 { done as f64 / secs } else { 0.0 };
    let pct = if total > 0 {
        100.0 * done as f64 / total as f64
    } else {
        100.0
    };
    let left = if done == 0 {
        "time left unknown".to_string()
    } else if done >= total {
        format!("done in {secs:.1} s")
    } else {
        format!("{:.1} s left", (total - done) as f64 / rate)
    };
    format!("rows {done}/{total} ({pct:.1}%), {rate:.0} rows/s, {left}")
}

fn parse_rows(raw: Vec<u8>, delimiter: Option<char>) -> Result<Vec<InputRow>> {
    let delim = match delimiter {
        Some(c) if c.is_ascii() => c as u8,
        Some(c) => anyhow::bail!("non-ASCII delimiter not supported: {c:?}"),
        None => sniff_delimiter(&raw),
    };
    read_rows(std::io::Cursor::new(raw), delim)
        .map(|r| r.map_err(|e| anyhow::anyhow!("csv: {e}")))
        .collect()
}
