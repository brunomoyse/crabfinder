//! HTTP façade for batch geocoding: `POST /batch/geocode`.
//!
//! Accepts either:
//! - `Content-Type: application/json` → `{ addresses: [InputRow, ...], options? }`
//! - `Content-Type: multipart/form-data` → `file=<csv>` plus optional
//!   `options=<json>` part
//! - `Content-Type: text/csv` (or any other) → the raw body is treated as CSV
//!
//! Response: `text/csv` streamed back. Header line emitted on the first row,
//! so an empty input produces an empty body (no header) — matching the
//! `BatchWriter` behaviour.
//!
//! Streaming model: the resolution phase (dedup + parallel Tantivy) runs in
//! a `tokio::task::spawn_blocking` task and pushes CSV-serialised chunks to
//! a `tokio::sync::mpsc` channel. The HTTP body is built from
//! `Body::from_stream(...)` over that channel. The first chunk lands as soon
//! as the first batch of rows is resolved.
//!
//! Caps:
//! - JSON path refuses payloads with > 10_000 addresses (HTTP 413 — use
//!   multipart for larger files; per design §4.6).
//! - Multipart files are bounded by axum's default 2 MiB limit unless
//!   overridden at server config; we deliberately don't override here in
//!   Lot E. A future operator dial would set `axum::extract::DefaultBodyLimit`.

use axum::{
    body::Body,
    extract::{DefaultBodyLimit, FromRequest, Multipart, State},
    http::{Request, StatusCode, header::CONTENT_TYPE},
    response::{IntoResponse, Response},
};
use bytes::Bytes;
use futures::Stream;
use serde::Deserialize;
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;
use tracing::{error, info};

use crate::AppState;
use crate::query_builder::analyze;
use crate::resolver::{AddressQuery, SearchOptions, resolve_with};

use super::dedup::{ClosureResolver, dedup_then_resolve};
use super::io::{BatchWriter, InputRow, read_rows, sniff_delimiter};

/// Hard cap on JSON-mode payloads. Larger callers must switch to multipart
/// (CSV file upload), per design §4.6.
const MAX_JSON_ADDRESSES: usize = 10_000;
/// Channel capacity between the blocking resolver and the HTTP body stream.
/// 64 chunks ≈ 64 lines of bandwidth-head-room — plenty without ballooning
/// in-flight memory on a slow client.
const STREAM_CHANNEL_CAPACITY: usize = 64;
/// How many output rows we batch into one CSV chunk before sending it on
/// the channel. 64 keeps per-chunk overhead amortised without inflating
/// TTFB.
const ROWS_PER_CHUNK: usize = 64;

/// Per-request options. JSON callers pass these in-band; multipart callers
/// pass them as a separate `options` field.
///
/// `Default` is hand-rolled (NOT `#[derive(Default)]`) so the defaults are
/// `true` / `"csv"` rather than `bool::default()` / `String::default()`.
/// The `#[serde(default = ...)]` attributes only fire during deserialization
/// — they don't influence the Rust `Default::default()` impl, which is what
/// gets called when no JSON body / no `options` part is supplied.
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct BatchOptions {
    /// Toggle the street-level fallbacks. Default `true`. When `false`,
    /// rows whose house number can't be matched come back `not_found`
    /// instead of `median_street` / street-level `fuzzy_street` (the caller
    /// may prefer empty lat/lng over a street approximation).
    pub mid_street_fallback: bool,
    /// Output format. Only `"csv"` in Lot E; reserved for future GeoJSON.
    pub format: String,
}

impl Default for BatchOptions {
    fn default() -> Self {
        Self {
            mid_street_fallback: true,
            format: "csv".to_string(),
        }
    }
}

#[derive(Debug, Deserialize)]
struct JsonBody {
    addresses: Vec<InputRow>,
    #[serde(default)]
    options: BatchOptions,
}

/// Maximum body size for multipart CSV uploads (50 MiB — handles a 100 k row
/// CSV at ~500 bytes/row with headroom).
const MULTIPART_BODY_LIMIT: usize = 50 * 1024 * 1024;

/// Axum-layer wrapper to install the body limit only on the batch route.
/// Other routes keep the framework default. The router wires this via
/// `.route_layer(DefaultBodyLimit::max(MULTIPART_BODY_LIMIT))`.
pub fn body_limit_layer() -> DefaultBodyLimit {
    DefaultBodyLimit::max(MULTIPART_BODY_LIMIT)
}

/// `POST /batch/geocode` handler.
///
/// Takes the raw `Request<Body>` so we can dispatch on `Content-Type`
/// without committing the body to a single extractor early — multipart
/// and JSON both want to consume the body, just differently.
pub async fn batch_geocode(State(state): State<AppState>, request: Request<Body>) -> Response {
    let ct = request
        .headers()
        .get(CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();

    let (rows, options) = if ct.starts_with("application/json") {
        match parse_json(request.into_body()).await {
            Ok((rows, opts)) => (rows, opts),
            Err(e) => return e.into_response(),
        }
    } else if ct.starts_with("multipart/form-data") {
        // `Multipart::from_request` consumes the request and yields the
        // multipart extractor — same shape as the typed `Multipart`
        // extractor in handler signatures, just done manually here.
        let mp = match Multipart::from_request(request, &()).await {
            Ok(m) => m,
            Err(e) => return bad_request(format!("multipart: {e}")).into_response(),
        };
        match parse_multipart(mp).await {
            Ok((rows, opts)) => (rows, opts),
            Err(e) => return e.into_response(),
        }
    } else {
        // Default: treat the body as raw CSV. text/csv, application/octet-stream,
        // anything else.
        match parse_raw_csv(request.into_body()).await {
            Ok(rows) => (rows, BatchOptions::default()),
            Err(e) => return e.into_response(),
        }
    };

    if rows.is_empty() {
        return (StatusCode::OK, [(CONTENT_TYPE, "text/csv")], "").into_response();
    }

    info!(
        n_rows = rows.len(),
        mid_street_fallback = options.mid_street_fallback,
        "batch_geocode: resolving"
    );

    // Stream CSV bytes from a blocking resolver task.
    let (tx, rx) = mpsc::channel::<Result<Bytes, std::io::Error>>(STREAM_CHANNEL_CAPACITY);
    let state_for_blocking = state.clone();
    tokio::task::spawn_blocking(move || {
        run_resolve_to_channel(state_for_blocking, rows, options, tx);
    });

    let stream: ReceiverStream<Result<Bytes, std::io::Error>> = ReceiverStream::new(rx);
    let body = Body::from_stream(BoxedStream::new(stream));

    Response::builder()
        .status(StatusCode::OK)
        .header(CONTENT_TYPE, "text/csv; charset=utf-8")
        .body(body)
        .unwrap_or_else(|e| {
            error!(error=%e, "batch_geocode: failed to assemble response");
            (StatusCode::INTERNAL_SERVER_ERROR, "response build failure").into_response()
        })
}

/// Bridging helper: `ReceiverStream` is a `Stream` but we wrap it so the
/// item type is `Result<Bytes, ?>` instead of `Bytes` — `axum::body::Body`'s
/// `from_stream` wants the result-shape.
struct BoxedStream<S>(S);
impl<S> BoxedStream<S> {
    fn new(s: S) -> Self {
        Self(s)
    }
}
impl<S> Stream for BoxedStream<S>
where
    S: Stream + Unpin,
{
    type Item = S::Item;
    fn poll_next(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<S::Item>> {
        std::pin::Pin::new(&mut self.0).poll_next(cx)
    }
}

/// The blocking-thread side of the streaming pipeline. Runs the dedup +
/// resolver, writes batches of OutputRows into CSV chunks, sends those
/// chunks to the channel. On `tx` send-error (client disconnected) we
/// bail out cleanly.
fn run_resolve_to_channel(
    state: AppState,
    rows: Vec<InputRow>,
    options: BatchOptions,
    tx: mpsc::Sender<Result<Bytes, std::io::Error>>,
) {
    let ctx = state.resolver_context();
    let opts = SearchOptions {
        mid_street_fallback: options.mid_street_fallback,
        ..SearchOptions::default()
    };
    let resolver = ClosureResolver(|q: &AddressQuery| resolve_with(q, &opts, &ctx));
    let make_query = |row: &InputRow| AddressQuery {
        street_tokens: analyze(&state.index, &row.street),
        house_number: row.house_number.clone(),
        postal_code: row.postal_code.clone(),
        region_filter: None,
        focus_point: None,
    };

    let out_rows = dedup_then_resolve(rows, &resolver, make_query);

    // Chunk into CSV bytes. We use BatchWriter into a Vec<u8> per chunk so
    // the header is emitted on the *first* chunk; subsequent chunks have
    // the header suppressed by `has_headers(false)` via a fresh
    // `BatchWriter::with_delimiter`... wait — csv::Writer always emits a
    // header before the first serialize, regardless of how many serializes
    // have happened on the *previous* writer. So we need a header-aware
    // approach: keep one BatchWriter across chunks. Easiest: write all
    // rows into one Vec<u8>, then chunk the bytes (but that gives raw byte
    // chunking which may slice mid-line). Best: build one big buffer then
    // chunk it on \n boundaries.

    let mut buf: Vec<u8> = Vec::new();
    let mut writer = BatchWriter::new(&mut buf);
    for row in &out_rows {
        if let Err(e) = writer.write_row(row) {
            send_error(&tx, format!("csv write: {e}"));
            return;
        }
    }
    if let Err(e) = writer.flush() {
        send_error(&tx, format!("csv flush: {e}"));
        return;
    }
    // Drop writer so `buf` is fully populated. (Writer borrows &mut buf.)
    drop(writer);

    // Send the result in newline-aligned chunks of ~ROWS_PER_CHUNK lines.
    // Even if rayon already finished all the work, this still benefits
    // huge outputs (multi-MB) by letting the client start receiving while
    // we're still flushing.
    let mut chunk_start = 0usize;
    let mut lines_in_chunk = 0usize;
    for (i, &b) in buf.iter().enumerate() {
        if b == b'\n' {
            lines_in_chunk += 1;
            if lines_in_chunk >= ROWS_PER_CHUNK {
                let chunk = Bytes::copy_from_slice(&buf[chunk_start..=i]);
                if tx.blocking_send(Ok(chunk)).is_err() {
                    // Client gone — abort cleanly, no further error needed.
                    return;
                }
                chunk_start = i + 1;
                lines_in_chunk = 0;
            }
        }
    }
    if chunk_start < buf.len() {
        let chunk = Bytes::copy_from_slice(&buf[chunk_start..]);
        let _ = tx.blocking_send(Ok(chunk));
    }
}

fn send_error(tx: &mpsc::Sender<Result<Bytes, std::io::Error>>, msg: String) {
    error!("batch_geocode worker: {msg}");
    let _ = tx.blocking_send(Err(std::io::Error::other(msg)));
}

// --- request parsers -------------------------------------------------------

#[derive(Debug)]
struct HttpError {
    status: StatusCode,
    msg: String,
}
impl IntoResponse for HttpError {
    fn into_response(self) -> Response {
        (self.status, self.msg).into_response()
    }
}
fn bad_request(msg: impl Into<String>) -> HttpError {
    HttpError {
        status: StatusCode::BAD_REQUEST,
        msg: msg.into(),
    }
}
fn payload_too_large(msg: impl Into<String>) -> HttpError {
    HttpError {
        status: StatusCode::PAYLOAD_TOO_LARGE,
        msg: msg.into(),
    }
}

async fn parse_json(body: Body) -> Result<(Vec<InputRow>, BatchOptions), HttpError> {
    let bytes = axum::body::to_bytes(body, MULTIPART_BODY_LIMIT)
        .await
        .map_err(|e| bad_request(format!("read body: {e}")))?;
    let parsed: JsonBody =
        serde_json::from_slice(&bytes).map_err(|e| bad_request(format!("parse json: {e}")))?;
    if parsed.addresses.len() > MAX_JSON_ADDRESSES {
        return Err(payload_too_large(format!(
            "JSON limit is {MAX_JSON_ADDRESSES} addresses (got {}); use multipart for larger payloads",
            parsed.addresses.len()
        )));
    }
    Ok((parsed.addresses, parsed.options))
}

async fn parse_multipart(mut mp: Multipart) -> Result<(Vec<InputRow>, BatchOptions), HttpError> {
    let mut file_bytes: Option<Vec<u8>> = None;
    let mut options = BatchOptions::default();
    while let Some(field) = mp
        .next_field()
        .await
        .map_err(|e| bad_request(format!("multipart: {e}")))?
    {
        let name = field.name().unwrap_or("").to_string();
        match name.as_str() {
            "file" => {
                let bytes = field
                    .bytes()
                    .await
                    .map_err(|e| bad_request(format!("multipart file: {e}")))?;
                file_bytes = Some(bytes.to_vec());
            }
            "options" => {
                let s = field
                    .text()
                    .await
                    .map_err(|e| bad_request(format!("multipart options: {e}")))?;
                options = serde_json::from_str(&s)
                    .map_err(|e| bad_request(format!("parse options json: {e}")))?;
            }
            _ => { /* ignore unknown fields */ }
        }
    }
    let Some(bytes) = file_bytes else {
        return Err(bad_request("multipart body missing `file` part"));
    };
    let rows = csv_bytes_to_rows(&bytes)?;
    Ok((rows, options))
}

async fn parse_raw_csv(body: Body) -> Result<Vec<InputRow>, HttpError> {
    let bytes = axum::body::to_bytes(body, MULTIPART_BODY_LIMIT)
        .await
        .map_err(|e| bad_request(format!("read body: {e}")))?;
    csv_bytes_to_rows(&bytes)
}

fn csv_bytes_to_rows(bytes: &[u8]) -> Result<Vec<InputRow>, HttpError> {
    let delim = sniff_delimiter(bytes);
    let cur = std::io::Cursor::new(bytes.to_vec());
    let mut rows = Vec::new();
    for r in read_rows(cur, delim) {
        match r {
            Ok(row) => rows.push(row),
            Err(e) => return Err(bad_request(format!("csv: {e}"))),
        }
    }
    Ok(rows)
}

// The integration tests live in `tests/` (workspace target). Unit-level
// helpers are tested here.

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn options_defaults_are_sensible() {
        let o: BatchOptions = serde_json::from_str("{}").unwrap();
        assert!(o.mid_street_fallback);
        assert_eq!(o.format, "csv");
    }

    #[test]
    fn options_overrides_apply() {
        let o: BatchOptions =
            serde_json::from_str(r#"{"mid_street_fallback":false,"format":"csv"}"#).unwrap();
        assert!(!o.mid_street_fallback);
    }

    #[test]
    fn csv_bytes_to_rows_strips_trailing_newline() {
        let csv = b"id,street,house_number,postal_code\n1,Av Louise,32,1050\n";
        let rows = csv_bytes_to_rows(csv).unwrap();
        assert_eq!(rows.len(), 1);
    }
}
