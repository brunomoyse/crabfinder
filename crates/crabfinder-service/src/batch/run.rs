//! Chunked batch resolution (v3 Phase 11.6): rows are resolved
//! `chunk_rows` at a time (dedup within the chunk, rayon fan-out) and
//! written to an `OutputSink` as each chunk finishes. Every batch path
//! goes through it: `POST /batch/geocode` streams the chunks to the
//! client, batch jobs append them to a file and checkpoint, the
//! `crabfinder-batch` CLI writes them to its output and reports progress.
//!
//! Between chunks, `keep_going` decides whether to continue: the HTTP
//! route stops at its deadline, a job when it is cancelled. So a stop
//! takes effect within one chunk, and memory stays bounded by a chunk's
//! rows, not the input's.

use std::io::Write;

use crate::AppState;
use crate::resolver::{AddressQuery, SearchOptions};

use super::dedup::{ClosureResolver, RowExtras, dedup_then_resolve_chunk, resolve_row, row_query};
use super::io::{BatchError, InputRow, OutputFormat, OutputSink};

/// Rows resolved between two checks of `keep_going`.
pub const CHUNK_ROWS: usize = 5_000;

/// What a batch asks for, once checked.
#[derive(Debug, Clone, Copy, Default)]
pub struct RunOptions {
    /// The street-level fallbacks (`mid_street_fallback`).
    pub mid_street_fallback: bool,
    pub format: OutputFormat,
    /// Optional columns, and the language of `label`.
    pub extras: RowExtras,
}

/// How a run ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RunEnd {
    /// Rows written by this run.
    pub rows: usize,
    /// `keep_going` said stop before the input ended.
    pub stopped: bool,
}

/// Resolve `rows` into `sink`, `chunk_rows` at a time (at least 1).
/// `keep_going(rows_done)` runs before each chunk (after the previous one
/// was written and flushed); `false` stops the run. It doesn't run once
/// the input is exhausted.
/// `id_columns` adds the id columns to every row (decided over the whole
/// input, see `dedup_then_resolve_chunk`).
///
/// A read error ends the run with that error; rows of earlier chunks are
/// already in the sink.
pub fn resolve_chunks<W: Write>(
    state: &AppState,
    rows: impl Iterator<Item = Result<InputRow, BatchError>>,
    options: &RunOptions,
    id_columns: bool,
    chunk_rows: usize,
    sink: &mut OutputSink<W>,
    mut keep_going: impl FnMut(usize) -> bool,
) -> Result<RunEnd, BatchError> {
    let ctx = state.resolver_context();
    let opts = SearchOptions {
        mid_street_fallback: options.mid_street_fallback,
        ..SearchOptions::default()
    };
    let extras = options.extras;
    let resolver = ClosureResolver(|q: &AddressQuery| resolve_row(q, &opts, &ctx, extras));
    let make_query = |row: &InputRow| row_query(&state.index, row);

    let chunk_rows = chunk_rows.max(1);
    let mut rows = rows.peekable();
    let mut done = 0;
    while rows.peek().is_some() {
        if !keep_going(done) {
            return Ok(RunEnd {
                rows: done,
                stopped: true,
            });
        }
        let chunk = rows
            .by_ref()
            .take(chunk_rows)
            .collect::<Result<Vec<_>, _>>()?;
        let n = chunk.len();
        let out = dedup_then_resolve_chunk(chunk, &resolver, make_query, extras.lang, id_columns);
        for row in &out {
            sink.write_row(row)?;
        }
        sink.flush()?;
        done += n;
    }
    Ok(RunEnd {
        rows: done,
        stopped: false,
    })
}
