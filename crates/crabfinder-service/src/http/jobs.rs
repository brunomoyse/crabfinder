//! `/batch/jobs` (v3 Phase 11.6): asynchronous batch jobs over
//! `crate::jobs::Jobs`, mounted when the service has `--jobs-dir`
//! (otherwise every route answers 503).
//!
//! - `POST /batch/jobs`: the inputs and options of `POST /batch/geocode`
//!   (raw CSV, multipart `file` + `options`, JSON `{addresses, options}`,
//!   query-string options), streamed to disk up to `--jobs-max-body-mb`.
//!   No 10,000-row cap on JSON. `202` with the job and
//!   `Location: /batch/jobs/{id}`.
//! - `GET /batch/jobs/{id}`: status and progress.
//! - `GET /batch/jobs/{id}/result`: the output once `done` (409 before).
//! - `DELETE /batch/jobs/{id}`: cancels a queued or running job (202),
//!   deletes a finished one and its result (204).

use std::sync::Arc;

use axum::{
    Json,
    body::Body,
    extract::{FromRequest, Multipart, Path, State},
    http::{
        Request, StatusCode,
        header::{CONTENT_DISPOSITION, CONTENT_LENGTH, CONTENT_TYPE, LOCATION},
    },
    response::{IntoResponse, Response},
};
use bytes::Bytes;
use futures::{Stream, StreamExt};
use serde::Serialize;
use tokio::io::{AsyncReadExt, AsyncWrite, AsyncWriteExt};

use super::ServiceError;
use crate::batch::http::{BatchOptions, BatchQuery, HttpError, JsonBody};
use crate::batch::io::{BatchError, InputRow, read_rows, sniff_delimiter};
use crate::jobs::{Deleted, JobState, JobStatus, Jobs, Upload};

/// A job as the API shows it.
#[derive(Debug, Serialize)]
pub struct JobView<'a> {
    pub job_id: &'a str,
    pub status: JobStatus,
    pub format: &'a str,
    pub rows_total: Option<usize>,
    pub rows_done: usize,
    pub created_at: u64,
    pub started_at: Option<u64>,
    pub finished_at: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<&'a str>,
    pub status_url: String,
    /// Present once the job is `done`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result_url: Option<String>,
}

impl<'a> JobView<'a> {
    pub fn of(st: &'a JobState) -> Self {
        Self {
            job_id: &st.id,
            status: st.status,
            format: &st.options.format,
            rows_total: st.rows_total,
            rows_done: st.rows_done,
            created_at: st.created_at,
            started_at: st.started_at,
            finished_at: st.finished_at,
            error: st.error.as_deref(),
            status_url: status_url(&st.id),
            result_url: (st.status == JobStatus::Done)
                .then(|| format!("{}/result", status_url(&st.id))),
        }
    }
}

fn status_url(id: &str) -> String {
    format!("/batch/jobs/{id}")
}

fn unknown(id: &str) -> ServiceError {
    ServiceError::NotFound(format!("unknown job '{id}'"))
}

fn from_batch(e: HttpError) -> ServiceError {
    match e.status {
        StatusCode::PAYLOAD_TOO_LARGE => ServiceError::TooLarge(e.msg),
        _ => ServiceError::BadQuery(e.msg),
    }
}

/// Every `/batch/jobs` route when the service runs without `--jobs-dir`.
pub async fn disabled() -> ServiceError {
    ServiceError::Unavailable("batch jobs are off: start the service with --jobs-dir".to_string())
}

/// `POST /batch/jobs`.
pub async fn submit(
    State(jobs): State<Arc<Jobs>>,
    request: Request<Body>,
) -> Result<Response, ServiceError> {
    let ct = request
        .headers()
        .get(CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();
    let query: BatchQuery = axum::extract::Query::try_from_uri(request.uri())
        .map(|q| q.0)
        .map_err(|e| ServiceError::BadQuery(format!("query string: {e}")))?;
    let upload = jobs
        .new_upload()
        .map_err(|e| ServiceError::internal("job upload", e))?;
    let limit = jobs.config().max_body_bytes;
    let checked = match receive(&upload, &ct, request, limit).await {
        Ok(options) => check(options.with_query(query), &upload),
        Err(e) => Err(e),
    };
    let (options, delimiter) = match checked {
        Ok(c) => c,
        Err(e) => {
            jobs.discard(upload);
            return Err(e);
        }
    };
    let st = jobs
        .submit(upload, options, delimiter)
        .map_err(|e| ServiceError::internal("job submit", e))?;
    Ok((
        StatusCode::ACCEPTED,
        [(LOCATION, status_url(&st.id))],
        Json(JobView::of(&st)),
    )
        .into_response())
}

/// Options and input checked before queueing: a bad option or a header
/// without an address column is a 400 now, not a failed job later.
fn check(options: BatchOptions, upload: &Upload) -> Result<(BatchOptions, u8), ServiceError> {
    options.validate().map_err(from_batch)?;
    let input = upload.input();
    let mut head = Vec::with_capacity(1024);
    std::fs::File::open(&input)
        .and_then(|f| std::io::Read::read_to_end(&mut std::io::Read::take(f, 1024), &mut head))
        .map_err(|e| ServiceError::internal("job input", e))?;
    let delimiter = sniff_delimiter(&head);
    let file = std::fs::File::open(&input).map_err(|e| ServiceError::internal("job input", e))?;
    if let Some(Err(e @ BatchError::MissingColumn)) = read_rows(file, delimiter).next() {
        return Err(ServiceError::BadQuery(e.to_string()));
    }
    Ok((options, delimiter))
}

/// Write the request's rows to `upload`'s input file; return the body's
/// options.
async fn receive(
    upload: &Upload,
    ct: &str,
    request: Request<Body>,
    limit: u64,
) -> Result<BatchOptions, ServiceError> {
    let io_err = |e: std::io::Error| ServiceError::internal("job upload", e);
    let mut out = tokio::fs::File::create(upload.input())
        .await
        .map_err(io_err)?;
    let mut written = 0;
    let options = if ct.starts_with("application/json") {
        let mut buf = Vec::new();
        copy_limited(
            request.into_body().into_data_stream(),
            &mut buf,
            limit,
            &mut written,
        )
        .await?;
        let body: JsonBody = serde_json::from_slice(&buf)
            .map_err(|e| ServiceError::BadQuery(format!("parse json: {e}")))?;
        out.write_all(&rows_csv(&body.addresses)?)
            .await
            .map_err(io_err)?;
        body.options
    } else if ct.starts_with("multipart/form-data") {
        let mut mp = Multipart::from_request(request, &())
            .await
            .map_err(|e| ServiceError::BadQuery(format!("multipart: {e}")))?;
        let mut options = BatchOptions::default();
        let mut has_file = false;
        while let Some(field) = mp
            .next_field()
            .await
            .map_err(|e| ServiceError::BadQuery(format!("multipart: {e}")))?
        {
            match field.name() {
                Some("file") => {
                    copy_limited(field, &mut out, limit, &mut written).await?;
                    has_file = true;
                }
                Some("options") => {
                    let s = field
                        .text()
                        .await
                        .map_err(|e| ServiceError::BadQuery(format!("multipart options: {e}")))?;
                    options = serde_json::from_str(&s)
                        .map_err(|e| ServiceError::BadQuery(format!("parse options json: {e}")))?;
                }
                _ => {}
            }
        }
        if !has_file {
            return Err(ServiceError::BadQuery(
                "multipart body missing `file` part".to_string(),
            ));
        }
        options
    } else {
        copy_limited(
            request.into_body().into_data_stream(),
            &mut out,
            limit,
            &mut written,
        )
        .await?;
        BatchOptions::default()
    };
    out.flush().await.map_err(io_err)?;
    Ok(options)
}

/// Copy `stream` into `out`, failing with 413 past `limit` bytes in all
/// (`written` counts across calls).
async fn copy_limited<S, E>(
    stream: S,
    out: &mut (impl AsyncWrite + Unpin),
    limit: u64,
    written: &mut u64,
) -> Result<(), ServiceError>
where
    S: Stream<Item = Result<Bytes, E>>,
    E: std::fmt::Display,
{
    let mut stream = std::pin::pin!(stream);
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|e| ServiceError::BadQuery(format!("read body: {e}")))?;
        *written += chunk.len() as u64;
        if *written > limit {
            return Err(ServiceError::TooLarge(format!(
                "batch job uploads are limited to {} MiB",
                limit.div_ceil(1024 * 1024)
            )));
        }
        out.write_all(&chunk)
            .await
            .map_err(|e| ServiceError::internal("job upload", e))?;
    }
    Ok(())
}

/// JSON rows as an input CSV.
fn rows_csv(rows: &[InputRow]) -> Result<Vec<u8>, ServiceError> {
    let mut w = csv::Writer::from_writer(Vec::new());
    let csv_err = |e: csv::Error| ServiceError::internal("job input", e);
    w.write_record([
        "id",
        "street",
        "house_number",
        "postal_code",
        "box_number",
        "address",
        "city",
        "best_id",
    ])
    .map_err(csv_err)?;
    for r in rows {
        let o = |v: &Option<String>| v.clone().unwrap_or_default();
        w.write_record([
            r.id.clone(),
            r.street.clone(),
            o(&r.house_number),
            o(&r.postal_code),
            o(&r.box_number),
            o(&r.address),
            o(&r.city),
            o(&r.best_id),
        ])
        .map_err(csv_err)?;
    }
    w.into_inner()
        .map_err(|e| ServiceError::internal("job input", e))
}

/// `GET /batch/jobs/{id}`.
pub async fn status(
    State(jobs): State<Arc<Jobs>>,
    Path(id): Path<String>,
) -> Result<Response, ServiceError> {
    let st = jobs.get(&id).ok_or_else(|| unknown(&id))?;
    Ok(Json(JobView::of(&st)).into_response())
}

/// `GET /batch/jobs/{id}/result`.
pub async fn result(
    State(jobs): State<Arc<Jobs>>,
    Path(id): Path<String>,
) -> Result<Response, ServiceError> {
    let (st, path) = jobs.result(&id).ok_or_else(|| unknown(&id))?;
    if st.status != JobStatus::Done {
        return Err(ServiceError::Conflict(format!(
            "job '{id}' is {}, not done",
            st.status.as_str()
        )));
    }
    let io_err = |e: std::io::Error| ServiceError::internal("job result", e);
    let file = tokio::fs::File::open(&path).await.map_err(io_err)?;
    let len = file.metadata().await.map_err(io_err)?.len();
    let stream = futures::stream::try_unfold(file, |mut f| async move {
        let mut buf = vec![0u8; 64 * 1024];
        let n = f.read(&mut buf).await?;
        if n == 0 {
            return Ok::<_, std::io::Error>(None);
        }
        buf.truncate(n);
        Ok(Some((Bytes::from(buf), f)))
    });
    let name = st.result_file().replace("result", &st.id);
    Response::builder()
        .header(CONTENT_TYPE, st.format().content_type())
        .header(CONTENT_LENGTH, len)
        .header(
            CONTENT_DISPOSITION,
            format!("attachment; filename=\"{name}\""),
        )
        .body(Body::from_stream(stream))
        .map_err(|e| ServiceError::internal("job result", e))
}

/// `DELETE /batch/jobs/{id}`.
pub async fn delete(
    State(jobs): State<Arc<Jobs>>,
    Path(id): Path<String>,
) -> Result<Response, ServiceError> {
    let key = id.clone();
    let outcome = tokio::task::spawn_blocking(move || jobs.delete(&key))
        .await
        .map_err(|e| ServiceError::internal("job delete", e))?;
    match outcome.ok_or_else(|| unknown(&id))? {
        Deleted::Removed => Ok(StatusCode::NO_CONTENT.into_response()),
        Deleted::Cancelled(st) => {
            Ok((StatusCode::ACCEPTED, Json(JobView::of(&st))).into_response())
        }
    }
}
