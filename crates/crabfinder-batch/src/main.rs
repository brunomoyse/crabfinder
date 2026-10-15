//! crabfinder-batch — standalone CLI batch geocoder.
//!
//! Reads a CSV (stdin or `--input`), resolves every row against the same
//! Tantivy index the HTTP service uses, writes the enriched CSV (stdout or
//! `--output`). Same resolver, same output schema, same fallbacks as the
//! HTTP `/batch/geocode` endpoint — this binary is a peer of
//! `crabfinder-service`, not a wrapper around it.
//!
//! Concurrency: the resolver runs through `rayon::par_iter` (see
//! `crabfinder_service::batch::dedup_then_resolve`). `--parallel N` sets the
//! global rayon pool size; default = all available cores.
//!
//! Multi-process Tantivy reads (CLI alongside a running service on the same
//! index directory) are supported because Tantivy's `MmapDirectory` is
//! read-shared across processes. See `docs/v1-batch-plan.md` §4.5.

use std::fs::File;
use std::io::{BufReader, BufWriter, Read, Write, stdin, stdout};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result};
use clap::Parser;
use tantivy::{Index, IndexReader, ReloadPolicy};
use tracing::{info, warn};
use tracing_subscriber::EnvFilter;

use crabfinder_indexer::index::{CrabSchema, register_tokenizers};
use crabfinder_indexer::meta::check_sidecar;
use crabfinder_indexer::mid_street::MID_STREET_FILENAME;

use crabfinder_service::abbreviation::AbbreviationMap;
use crabfinder_service::addr_count::AddressCountIndex;
use crabfinder_service::batch::dedup::{ClosureResolver, dedup_then_resolve};
use crabfinder_service::batch::io::{BatchWriter, InputRow, read_rows, sniff_delimiter};
use crabfinder_service::mid_street::MidStreetIndex;
use crabfinder_service::query_builder::analyze;
use crabfinder_service::resolver::{AddressQuery, ResolverContext, resolve};

#[derive(Debug, Parser)]
#[command(
    name = "crabfinder-batch",
    version,
    about = "Resolve a CSV of addresses against a crabfinder Tantivy index."
)]
struct Args {
    /// Tantivy index directory (must match the binary's schema version).
    #[arg(long, env = "CRABFINDER_INDEX")]
    index: PathBuf,

    /// Path to `street-type-abbreviations-v1.yaml`. Optional; without it the
    /// CLI behaves exactly as if `--no-abbreviation-dict` had been passed.
    #[arg(long, env = "CRABFINDER_ABBREVIATION_DICT")]
    abbreviation_dict: Option<PathBuf>,

    /// Input CSV path. Use `-` (or omit) to read from stdin.
    #[arg(long, short)]
    input: Option<PathBuf>,

    /// Output CSV path. Use `-` (or omit) to write to stdout.
    #[arg(long, short)]
    output: Option<PathBuf>,

    /// Override the sniffed CSV delimiter for input (`,` or `;`). When
    /// unset, the CLI peeks the first line and picks whichever separator
    /// dominates.
    #[arg(long)]
    delimiter: Option<char>,

    /// Disable the `Confidence::MedianStreet` fallback. Same toggle as the
    /// HTTP endpoint's `mid_street_fallback=false` option.
    #[arg(long, default_value_t = false)]
    no_mid_street: bool,

    /// Number of rayon worker threads to use for resolution. Default = all
    /// cores. Set to 1 for deterministic profiling.
    #[arg(long)]
    parallel: Option<usize>,
}

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .with_target(false)
        .with_writer(std::io::stderr) // keep stdout clean for the CSV output
        .init();

    let args = Args::parse();

    if let Some(n) = args.parallel {
        rayon::ThreadPoolBuilder::new()
            .num_threads(n)
            .build_global()
            .context("configuring rayon thread pool")?;
    }

    // --- Index + sidecars ------------------------------------------------
    let meta = check_sidecar(&args.index).map_err(anyhow::Error::new)?;
    info!(
        index = %args.index.display(),
        schema_version = meta.schema_version,
        "index sidecar OK"
    );

    let schema = CrabSchema::build();
    let index = Index::open_in_dir(&args.index)
        .with_context(|| format!("opening Tantivy index at {:?}", args.index))?;
    register_tokenizers(&index);
    let reader: IndexReader = index
        .reader_builder()
        .reload_policy(ReloadPolicy::Manual)
        .try_into()
        .context("constructing IndexReader")?;

    let abbrev = match args.abbreviation_dict.as_ref() {
        Some(p) => AbbreviationMap::load(p, &index)
            .with_context(|| format!("loading abbreviation dict {p:?}"))?,
        None => AbbreviationMap::empty(),
    };
    let abbrev = Arc::new(abbrev);

    let addr_counts = Arc::new(
        AddressCountIndex::build(&reader, &schema).context("building address-count index")?,
    );

    let mid_street_path = args.index.join(MID_STREET_FILENAME);
    let mid_street = if args.no_mid_street {
        info!("--no-mid-street: MedianStreet fallback disabled");
        Arc::new(MidStreetIndex::empty())
    } else {
        let m = MidStreetIndex::load(&mid_street_path)
            .with_context(|| format!("loading mid_street side-file {mid_street_path:?}"))?;
        Arc::new(m)
    };

    // --- Build the resolver context once. -------------------------------
    let ctx = ResolverContext {
        index: &index,
        reader: &reader,
        schema: &schema,
        abbrev: abbrev.clone(),
        addr_counts: addr_counts.clone(),
        notability_enabled: true,
        mid_street: mid_street.clone(),
    };

    // --- Read input -----------------------------------------------------
    let rows = read_input(&args)?;
    let n_rows = rows.len();
    info!(n_rows, "batch input loaded");
    if n_rows == 0 {
        warn!("input is empty; no work to do");
        return Ok(());
    }

    // --- Resolve in parallel -------------------------------------------
    let resolver = ClosureResolver(|q: &AddressQuery| resolve(q, &ctx));
    let make_query = |row: &InputRow| AddressQuery {
        street_tokens: analyze(&index, &row.street),
        house_number: row.house_number.clone(),
        postal_code: row.postal_code.clone(),
        region_filter: None,
        focus_point: None,
    };
    let t0 = std::time::Instant::now();
    let out = dedup_then_resolve(rows, &resolver, make_query);
    info!(
        out_rows = out.len(),
        elapsed_ms = t0.elapsed().as_millis(),
        "resolution complete"
    );

    // --- Write output ---------------------------------------------------
    write_output(&args, &out)?;
    Ok(())
}

fn read_input(args: &Args) -> Result<Vec<InputRow>> {
    let raw: Vec<u8> = match args.input.as_deref() {
        Some(p) if p.as_os_str() != "-" => {
            std::fs::read(p).with_context(|| format!("reading input {p:?}"))?
        }
        _ => read_all_stdin().context("reading stdin")?,
    };
    let delim = match args.delimiter {
        Some(c) if c.is_ascii() => c as u8,
        Some(c) => anyhow::bail!("non-ASCII delimiter not supported: {c:?}"),
        None => sniff_delimiter(&raw),
    };
    let mut rows = Vec::new();
    let cur = std::io::Cursor::new(raw);
    for r in read_rows(cur, delim) {
        match r {
            Ok(row) => rows.push(row),
            Err(e) => anyhow::bail!("csv: {e}"),
        }
    }
    Ok(rows)
}

fn read_all_stdin() -> std::io::Result<Vec<u8>> {
    let mut buf = Vec::new();
    BufReader::new(stdin()).read_to_end(&mut buf)?;
    Ok(buf)
}

fn write_output(args: &Args, out: &[crabfinder_service::batch::io::OutputRow]) -> Result<()> {
    let mut writer: Box<dyn Write> = match args.output.as_deref() {
        Some(p) if p.as_os_str() != "-" => {
            let f = File::create(p).with_context(|| format!("creating {p:?}"))?;
            Box::new(BufWriter::new(f))
        }
        _ => Box::new(BufWriter::new(stdout().lock())),
    };
    let mut w = BatchWriter::new(&mut writer);
    for row in out {
        w.write_row(row).context("writing csv row")?;
    }
    w.flush().context("flushing csv")?;
    Ok(())
}

// Silence: `Path` unused on some builds.
const _: fn(&Path) = |_| {};
