//! Index schema versioning sidecar.
//!
//! Tantivy doesn't know about crabfinder's tokenizer chain or query-time
//! assumptions, so we keep a separate `.crab-meta.json` next to the index
//! files. The service refuses to open an index whose schema version doesn't
//! match the binary, with an error pointing the user at the rebuild command.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Bump whenever a change makes existing indices unreadable by the new binary:
///   v1 — Phase 1 walking skeleton (default tokenizer, hyphen-split-OR).
///   v2 — Phase 2 "crab" tokenizer (SimpleTokenizer→Lower→AsciiFold) +
///        phrase-first query builder.
///   v3 — Phase 6 collapsed searchable_fr/nl/de into a single `searchable`
///        field. The per-language split had no ranking semantics and broke
///        cross-language strict matching (file 12 §3.2).
///   v4 — Phase 6 §3.1 added a FAST f64 `notability_score` field per doc.
///        Populated at index time by joining building_group_id against
///        `data/notability-whitelist-v1.yaml`. Read at query time via
///        `TopDocs::tweak_score`. File 13 §6 Option A.
///   v5 — Phase 7a added `street_prefix` (edge-ngram) + `street_id` (raw)
///        backing the /autocomplete handler. Removed in v6.
///   v6 — Phase 7a reverted: /autocomplete removed from the project. Both
///        `street_prefix` and `street_id` fields, plus the
///        `street_prefix_ngram` tokenizer, were dropped. The schema is back
///        to a single searchable field + notability + structured columns.
///   v7 — Batch geocoding (design-batch-geocoding §5.5): indexer emits a
///        side-file `mid-street-v1.bin` alongside the Tantivy directory. It
///        maps `(street_key, postal_code)` to the WGS84 coordinates of the
///        lower-discrete-median house number (R `quantile(p=0.5, type=3)`,
///        per PhacochR §2.1). The service reads it at startup to back
///        `Confidence::MedianStreet`. The Tantivy schema itself is unchanged
///        from v6 — this bump forces a rebuild so the side file is generated.
pub const SCHEMA_VERSION: u32 = 7;

/// Filename under the index directory.
pub const SIDECAR_FILENAME: &str = ".crab-meta.json";

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq, Eq)]
pub struct IndexMeta {
    pub schema_version: u32,
    pub tokenizer_name: String,
    pub built_at_unix_secs: i64,
}

#[derive(Debug, Error)]
pub enum MetaError {
    #[error(
        "index at {path} has schema_version={found}, but this binary expects \
         schema_version={expected}. Run `cargo run --release --bin crabfinder-indexer -- \
         --best-dir <best> --region <region> --output {path}` to rebuild."
    )]
    VersionMismatch {
        path: String,
        found: u32,
        expected: u32,
    },
    #[error(
        "index at {path} is missing {SIDECAR_FILENAME} (pre-Phase-2 index). \
         Rebuild with `cargo run --release --bin crabfinder-indexer -- \
         --best-dir <best> --region <region> --output {path}`."
    )]
    Missing { path: String },
    #[error("reading {path}: {source}")]
    Io {
        path: String,
        #[source]
        source: std::io::Error,
    },
    #[error("parsing {path}: {source}")]
    Parse {
        path: String,
        #[source]
        source: serde_json::Error,
    },
}

fn sidecar_path(index_dir: &Path) -> PathBuf {
    index_dir.join(SIDECAR_FILENAME)
}

pub fn write_sidecar(index_dir: &Path) -> std::io::Result<()> {
    let meta = IndexMeta {
        schema_version: SCHEMA_VERSION,
        tokenizer_name: super::index::TOKENIZER_NAME.to_string(),
        built_at_unix_secs: now_unix_secs(),
    };
    let path = sidecar_path(index_dir);
    let body =
        serde_json::to_string_pretty(&meta).expect("serializing IndexMeta to JSON cannot fail");
    std::fs::write(&path, body)
}

/// Read and validate the sidecar against the current binary's expectations.
/// Returns the parsed meta on success.
pub fn check_sidecar(index_dir: &Path) -> Result<IndexMeta, MetaError> {
    let path = sidecar_path(index_dir);
    let path_str = path.display().to_string();
    let body = match std::fs::read_to_string(&path) {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Err(MetaError::Missing {
                path: index_dir.display().to_string(),
            });
        }
        Err(source) => {
            return Err(MetaError::Io {
                path: path_str,
                source,
            });
        }
    };
    let meta: IndexMeta = serde_json::from_str(&body).map_err(|source| MetaError::Parse {
        path: path_str.clone(),
        source,
    })?;
    if meta.schema_version != SCHEMA_VERSION {
        return Err(MetaError::VersionMismatch {
            path: index_dir.display().to_string(),
            found: meta.schema_version,
            expected: SCHEMA_VERSION,
        });
    }
    Ok(meta)
}

fn now_unix_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn write_then_read_round_trip() {
        let tmp = tempdir();
        write_sidecar(tmp.path()).unwrap();
        let meta = check_sidecar(tmp.path()).unwrap();
        assert_eq!(meta.schema_version, SCHEMA_VERSION);
        assert_eq!(meta.tokenizer_name, super::super::index::TOKENIZER_NAME);
    }

    #[test]
    fn missing_sidecar_reports_specific_error() {
        let tmp = tempdir();
        match check_sidecar(tmp.path()) {
            Err(MetaError::Missing { .. }) => {}
            other => panic!("expected Missing, got {other:?}"),
        }
    }

    #[test]
    fn version_mismatch_reports_specific_error() {
        let tmp = tempdir();
        // Force a different version regardless of SCHEMA_VERSION value.
        let stale = IndexMeta {
            schema_version: SCHEMA_VERSION + 99,
            tokenizer_name: "default".into(),
            built_at_unix_secs: 0,
        };
        std::fs::write(
            tmp.path().join(SIDECAR_FILENAME),
            serde_json::to_string(&stale).unwrap(),
        )
        .unwrap();
        match check_sidecar(tmp.path()) {
            Err(MetaError::VersionMismatch {
                found, expected, ..
            }) => {
                assert_eq!(found, SCHEMA_VERSION + 99);
                assert_eq!(expected, SCHEMA_VERSION);
            }
            other => panic!("expected VersionMismatch, got {other:?}"),
        }
    }

    // Minimal tempdir helper — keeps `tempfile` out of the dep tree.
    struct TempDir(PathBuf);
    impl TempDir {
        fn path(&self) -> &Path {
            &self.0
        }
    }
    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    fn tempdir() -> TempDir {
        use std::sync::atomic::{AtomicU64, Ordering};
        static SEQ: AtomicU64 = AtomicU64::new(0);
        let n = SEQ.fetch_add(1, Ordering::Relaxed);
        let p = std::env::temp_dir().join(format!(
            "crab-meta-test-{}-{}-{}",
            std::process::id(),
            now_unix_secs(),
            n
        ));
        std::fs::create_dir_all(&p).unwrap();
        TempDir(p)
    }
}
