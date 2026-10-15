//! `GET /health` — readiness probe.

use axum::{Json, extract::State};
use serde::Serialize;

use crate::AppState;

#[derive(Serialize)]
pub struct HealthBody {
    pub status: &'static str,
    pub index_path: String,
    pub index_docs: u64,
    pub uptime_secs: u64,
    /// Index schema version (`.crab-meta.json`), = this binary's.
    pub schema_version: u32,
    /// When the index was built (Unix seconds, from the sidecar).
    pub index_built_at_unix_secs: i64,
    /// Version of the running binary.
    pub version: &'static str,
}

pub async fn health(State(s): State<AppState>) -> Json<HealthBody> {
    Json(HealthBody {
        status: "ok",
        index_path: s.index_path.to_string_lossy().into_owned(),
        index_docs: s.reader.searcher().num_docs(),
        uptime_secs: s.started_at.elapsed_secs(),
        schema_version: s.meta.schema_version,
        index_built_at_unix_secs: s.meta.built_at_unix_secs,
        version: env!("CARGO_PKG_VERSION"),
    })
}
