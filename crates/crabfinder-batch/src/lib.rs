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
//! `--parallel N` (default = all available cores).
//!
//! Multi-process Tantivy reads (CLI alongside a running service on the same
//! index directory) are supported because Tantivy's `MmapDirectory` is
//! read-shared across processes. See `docs/v1-batch-plan.md` §4.5.

use std::fs::File;
use std::io::{BufWriter, Read, Write};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use clap::Parser;
use tracing::{info, warn};

use crabfinder_core::Language;
use crabfinder_service::batch::dedup::{ClosureResolver, dedup_then_resolve, row_query};
use crabfinder_service::batch::io::{
    BatchWriter, InputRow, OutputFormat, read_rows, sniff_delimiter, write_geojson,
};
use crabfinder_service::resolver::{AddressQuery, SearchOptions, resolve_with};
use crabfinder_service::validate::resolve_and_assess;
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
    let output: Box<dyn Write> = match non_dash(args.output.as_deref()) {
        Some(p) => Box::new(BufWriter::new(
            File::create(p).with_context(|| format!("creating {p:?}"))?,
        )),
        None => Box::new(BufWriter::new(std::io::stdout().lock())),
    };
    run_with_io(args, input, output)
}

/// `Some(p)` unless the path is absent or `-`.
fn non_dash(p: Option<&Path>) -> Option<&Path> {
    p.filter(|p| p.as_os_str() != "-")
}

/// Same as `run`, with explicit I/O streams (`--input` / `--output` are
/// ignored).
pub fn run_with_io(args: &Args, mut input: impl Read, output: impl Write) -> Result<Summary> {
    let mut opts = LoadOptions::new(&args.index);
    opts.abbreviation_dict = args.abbreviation_dict.clone();
    opts.pois = args.pois.clone();
    opts.spatial = false;
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
    let ctx = state.resolver_context();
    let search_opts = SearchOptions {
        mid_street_fallback: !args.no_mid_street,
        ..SearchOptions::default()
    };
    let resolver = ClosureResolver(|q: &AddressQuery| {
        if args.corrections {
            resolve_and_assess(q, &search_opts, &ctx, args.lang)
        } else {
            resolve_with(q, &search_opts, &ctx)
        }
    });
    let make_query = |row: &InputRow| row_query(&state.index, row);
    let t0 = std::time::Instant::now();
    let out = pool.install(|| dedup_then_resolve(rows, &resolver, make_query, args.lang));
    info!(
        out_rows = out.len(),
        elapsed_ms = t0.elapsed().as_millis(),
        "resolution complete"
    );

    match args.format {
        OutputFormat::Csv => {
            let mut w = BatchWriter::new(output);
            for row in &out {
                w.write_row(row).context("writing csv row")?;
            }
            w.flush().context("flushing csv")?;
        }
        OutputFormat::GeoJson => {
            let mut output = output;
            write_geojson(&out, &mut output).context("writing geojson")?;
            output.flush().context("flushing geojson")?;
        }
    }
    Ok(Summary { rows: out.len() })
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
