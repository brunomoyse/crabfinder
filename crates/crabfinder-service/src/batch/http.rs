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
//! `BatchWriter` behaviour. With `format=geojson`, one
//! `application/geo+json` FeatureCollection instead (v2 Phase 6).
//!
//! Options (`BatchOptions`) come in the JSON body / `options` part, or as
//! query-string parameters (`?format=geojson&lang=nl`), which win. Every
//! input path accepts free-text rows (`id,address`, see `io::InputRow`).
//!
//! Streaming model (v3 Phase 11.6): the body is read and parsed, then a
//! blocking task resolves the rows `batch_chunk_rows` at a time
//! (`run::resolve_chunks`) and sends each chunk's output down a bounded
//! channel feeding `Body::from_stream`. The handler answers once the first
//! chunk is out, so a request whose first chunk misses the deadline still
//! gets the guard's 503. The task holds the request's concurrency slot
//! until it ends, and checks the deadline (`--batch-timeout-secs`, counted
//! from the request's arrival) between chunks: past it, the body ends with
//! an error (the client sees a cut-off response, never a short one that
//! looks complete). A client that goes away stops the work at its next
//! write. Larger or longer batches belong in `/batch/jobs` (`jobs`).
//!
//! Caps:
//! - JSON path refuses payloads with > 10_000 addresses (HTTP 413 — use
//!   multipart for larger files; per design §4.6).
//! - Request bodies are bounded at 50 MiB (`MULTIPART_BODY_LIMIT`, applied
//!   by `body_limit_layer`).

use std::io::Write;
use std::time::{Duration, Instant};

use axum::{
    body::Body,
    extract::{DefaultBodyLimit, FromRequest, Multipart, State},
    http::{Request, StatusCode, header::CONTENT_TYPE},
    response::{IntoResponse, Response},
};
use bytes::Bytes;
use futures::StreamExt;
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;
use tracing::{debug, error, info, warn};

use crabfinder_core::Language;

use crate::AppState;
use crate::http::{Deadline, Permit};
use crate::telemetry;

use super::dedup::RowExtras;
use super::io::{InputRow, OutputFormat, OutputSink, read_rows, sniff_delimiter};
use super::run::{CHUNK_ROWS, RunOptions, resolve_chunks};

/// Hard cap on JSON-mode payloads. Larger callers must switch to multipart
/// (CSV file upload), per design §4.6.
const MAX_JSON_ADDRESSES: usize = 10_000;
/// Chunks in flight between the blocking resolver and the HTTP body
/// stream: a slow client holds the resolver back (backpressure) instead
/// of piling the output up in memory.
const STREAM_CHANNEL_CAPACITY: usize = 16;
/// Bytes collected before a send on the channel.
const SEND_BYTES: usize = 64 * 1024;

/// How the route cuts its work (`RouterOptions::batch_chunk_rows`),
/// installed as a request extension by the router.
#[derive(Debug, Clone, Copy)]
pub struct BatchTuning {
    /// Rows resolved between two deadline checks.
    pub chunk_rows: usize,
    /// Test hook: a pause before each chunk, to exercise the deadline.
    pub chunk_delay: Duration,
}

impl Default for BatchTuning {
    fn default() -> Self {
        Self {
            chunk_rows: CHUNK_ROWS,
            chunk_delay: Duration::ZERO,
        }
    }
}

/// Per-request options. JSON callers pass these in-band; multipart callers
/// pass them as a separate `options` field.
///
/// `Default` is hand-rolled (NOT `#[derive(Default)]`) so the defaults are
/// `true` / `"csv"` rather than `bool::default()` / `String::default()`.
/// The `#[serde(default = ...)]` attributes only fire during deserialization
/// — they don't influence the Rust `Default::default()` impl, which is what
/// gets called when no JSON body / no `options` part is supplied.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct BatchOptions {
    /// Toggle the street-level fallbacks. Default `true`. When `false`,
    /// rows whose house number can't be matched come back `not_found`
    /// instead of `median_street` / street-level `fuzzy_street` (the caller
    /// may prefer empty lat/lng over a street approximation).
    pub mid_street_fallback: bool,
    /// Output format: `"csv"` (default) or `"geojson"`. Anything else is
    /// a 400.
    pub format: String,
    /// Language of the `label` column: `fr`, `nl` or `de` (default: each
    /// address's own). Anything else is a 400.
    pub lang: Option<String>,
    /// Add the `/validate` columns `status` and `corrections` (v3 Phase
    /// 11.2). Default `false`: the columns stay as they were.
    pub corrections: bool,
    /// Add the `score_gap` and `alternatives` columns (v3 Phase 11.3).
    /// Default `false`.
    pub alternatives: bool,
}

impl Default for BatchOptions {
    fn default() -> Self {
        Self {
            mid_street_fallback: true,
            format: "csv".to_string(),
            lang: None,
            corrections: false,
            alternatives: false,
        }
    }
}

/// Query-string overrides of `BatchOptions`.
#[derive(Debug, Default, Deserialize)]
pub(crate) struct BatchQuery {
    mid_street_fallback: Option<bool>,
    format: Option<String>,
    lang: Option<String>,
    corrections: Option<bool>,
    alternatives: Option<bool>,
}

impl BatchOptions {
    pub(crate) fn with_query(mut self, q: BatchQuery) -> Self {
        if let Some(m) = q.mid_street_fallback {
            self.mid_street_fallback = m;
        }
        if let Some(f) = q.format {
            self.format = f;
        }
        if q.lang.is_some() {
            self.lang = q.lang;
        }
        if let Some(c) = q.corrections {
            self.corrections = c;
        }
        if let Some(a) = q.alternatives {
            self.alternatives = a;
        }
        self
    }

    /// The options checked: a bad `format` or `lang` is a 400.
    pub(crate) fn validate(&self) -> Result<RunOptions, HttpError> {
        let format = self.format.parse::<OutputFormat>().map_err(bad_request)?;
        let lang = self
            .lang
            .as_deref()
            .map(|l| {
                l.trim()
                    .to_ascii_lowercase()
                    .parse::<Language>()
                    .map_err(|_| bad_request(format!("unknown lang '{l}' (expected fr, nl or de)")))
            })
            .transpose()?;
        Ok(RunOptions {
            mid_street_fallback: self.mid_street_fallback,
            format,
            extras: RowExtras {
                corrections: self.corrections,
                alternatives: self.alternatives,
                lang,
            },
        })
    }
}

#[derive(Debug, Deserialize)]
pub(crate) struct JsonBody {
    pub(crate) addresses: Vec<InputRow>,
    #[serde(default)]
    pub(crate) options: BatchOptions,
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
    let deadline = request.extensions().get::<Deadline>().copied();
    let permit = request.extensions().get::<Permit>().cloned();
    let tuning = request.extensions().get::<BatchTuning>().copied();
    let ct = request
        .headers()
        .get(CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();
    let query: BatchQuery = match axum::extract::Query::try_from_uri(request.uri()) {
        Ok(axum::extract::Query(q)) => q,
        Err(e) => return bad_request(format!("query string: {e}")).into_response(),
    };

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

    let options = match options.with_query(query).validate() {
        Ok(o) => o,
        Err(e) => return e.into_response(),
    };

    if rows.is_empty() {
        let body = match options.format {
            OutputFormat::Csv => String::new(),
            OutputFormat::GeoJson => r#"{"type":"FeatureCollection","features":[]}"#.to_string(),
        };
        return (
            StatusCode::OK,
            [(CONTENT_TYPE, options.format.content_type())],
            body,
        )
            .into_response();
    }

    info!(
        n_rows = rows.len(),
        mid_street_fallback = options.mid_street_fallback,
        format = ?options.format,
        "batch_geocode: resolving"
    );

    let (tx, mut rx) = mpsc::channel::<Result<Bytes, std::io::Error>>(STREAM_CHANNEL_CAPACITY);
    let job = Worker {
        state,
        rows,
        options,
        tuning: tuning.unwrap_or_default(),
        deadline: deadline.map_or_else(|| Instant::now() + Duration::from_secs(600), |d| d.0),
        _permit: permit,
    };
    tokio::task::spawn_blocking(move || job.run(tx));

    // Answer once the first chunk is out (see the module doc).
    let first = rx.recv().await;
    let stream = futures::stream::iter(first).chain(ReceiverStream::new(rx));
    Response::builder()
        .status(StatusCode::OK)
        .header(CONTENT_TYPE, options.format.content_type())
        .body(Body::from_stream(stream))
        .unwrap_or_else(|e| {
            error!(error=%e, "batch_geocode: failed to assemble response");
            (StatusCode::INTERNAL_SERVER_ERROR, "response build failure").into_response()
        })
}

/// The blocking side of `batch_geocode`: resolves the rows chunk by chunk
/// into the channel, holding the request's concurrency slot.
struct Worker {
    state: AppState,
    rows: Vec<InputRow>,
    options: RunOptions,
    tuning: BatchTuning,
    deadline: Instant,
    _permit: Option<Permit>,
}

impl Worker {
    fn run(self, tx: mpsc::Sender<Result<Bytes, std::io::Error>>) {
        let id_columns = self.rows.iter().any(|r| r.best_id.is_some());
        let mut sink = OutputSink::new(self.options.format, ChannelWriter::new(tx.clone()), 0);
        let mut late = false;
        let end = resolve_chunks(
            &self.state,
            self.rows.into_iter().map(Ok),
            &self.options,
            id_columns,
            self.tuning.chunk_rows,
            &mut sink,
            |_| {
                if !self.tuning.chunk_delay.is_zero() {
                    std::thread::sleep(self.tuning.chunk_delay);
                }
                late = Instant::now() >= self.deadline;
                !late
            },
        );
        match end {
            Ok(end) if end.stopped => {
                warn!(
                    rows = end.rows,
                    "batch_geocode: deadline reached, response cut off"
                );
                telemetry::record_rejected("/batch/geocode".to_owned(), "timeout");
                let _ = tx.blocking_send(Err(std::io::Error::other(
                    "batch deadline reached; use /batch/jobs for long batches",
                )));
            }
            Ok(_) => {
                if let Err(e) = sink.finish() {
                    debug!(error = %e, "batch_geocode: client gone");
                }
            }
            // The only write error is a closed channel: the client left.
            Err(e) => debug!(error = %e, "batch_geocode: stopped"),
        }
    }
}

/// `Write` into the response channel, in `SEND_BYTES` pieces. A closed
/// channel (client gone) is a write error, which stops the run.
struct ChannelWriter {
    tx: mpsc::Sender<Result<Bytes, std::io::Error>>,
    buf: Vec<u8>,
}

impl ChannelWriter {
    fn new(tx: mpsc::Sender<Result<Bytes, std::io::Error>>) -> Self {
        Self {
            tx,
            buf: Vec::with_capacity(SEND_BYTES),
        }
    }

    fn send(&mut self) -> std::io::Result<()> {
        if self.buf.is_empty() {
            return Ok(());
        }
        let chunk = Bytes::from(std::mem::replace(
            &mut self.buf,
            Vec::with_capacity(SEND_BYTES),
        ));
        self.tx
            .blocking_send(Ok(chunk))
            .map_err(|_| std::io::Error::new(std::io::ErrorKind::BrokenPipe, "client gone"))
    }
}

impl Write for ChannelWriter {
    fn write(&mut self, data: &[u8]) -> std::io::Result<usize> {
        self.buf.extend_from_slice(data);
        if self.buf.len() >= SEND_BYTES {
            self.send()?;
        }
        Ok(data.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.send()
    }
}

// --- request parsers -------------------------------------------------------

#[derive(Debug)]
pub(crate) struct HttpError {
    pub(crate) status: StatusCode,
    pub(crate) msg: String,
}
impl IntoResponse for HttpError {
    fn into_response(self) -> Response {
        (self.status, self.msg).into_response()
    }
}
pub(crate) fn bad_request(msg: impl Into<String>) -> HttpError {
    HttpError {
        status: StatusCode::BAD_REQUEST,
        msg: msg.into(),
    }
}
pub(crate) fn payload_too_large(msg: impl Into<String>) -> HttpError {
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
    fn query_overrides_and_validation() {
        let o = BatchOptions::default().with_query(BatchQuery {
            mid_street_fallback: Some(false),
            format: Some("GeoJSON".into()),
            lang: Some("NL".into()),
            corrections: Some(true),
            alternatives: Some(true),
        });
        let r = o.validate().unwrap();
        assert!(r.extras.corrections && r.extras.alternatives);
        assert_eq!(r.extras.lang, Some(Language::Nl));
        assert!(!r.mid_street_fallback);
        assert_eq!(r.format, OutputFormat::GeoJson);
        // Absent query values keep the body's.
        let o = BatchOptions {
            lang: Some("de".into()),
            ..BatchOptions::default()
        }
        .with_query(BatchQuery::default());
        assert_eq!(o.validate().unwrap().extras.lang, Some(Language::De));

        let bad = |o: BatchOptions| o.validate().unwrap_err().msg;
        assert_eq!(
            bad(BatchOptions {
                format: "xml".into(),
                ..BatchOptions::default()
            }),
            "unknown format 'xml' (expected csv or geojson)"
        );
        assert_eq!(
            bad(BatchOptions {
                lang: Some("en".into()),
                ..BatchOptions::default()
            }),
            "unknown lang 'en' (expected fr, nl or de)"
        );
    }

    #[test]
    fn csv_bytes_to_rows_strips_trailing_newline() {
        let csv = b"id,street,house_number,postal_code\n1,Av Louise,32,1050\n";
        let rows = csv_bytes_to_rows(csv).unwrap();
        assert_eq!(rows.len(), 1);
    }
}
