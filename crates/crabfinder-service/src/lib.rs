//! crabfinder-service library surface.
//!
//! Until Lot F this crate was bin-only. Lot F adds the `crabfinder-batch`
//! CLI, which lives in its own workspace crate and depends on this one as
//! a library to reuse the resolver, the batch I/O, the abbreviation/
//! mid-street loaders, etc.
//!
//! The crate also still produces the `crabfinder-service` HTTP binary
//! (`src/main.rs`); that binary now uses these same modules through
//! the library surface.
//!
//! HTTP-only types (`AppState`, `ServiceError`, `Feature`, …) deliberately
//! stay in `main.rs` and are not re-exported — they are not useful to the
//! batch CLI.

pub mod abbreviation;
pub mod addr_count;
pub mod batch;
pub mod dedup;
pub mod focus;
pub mod hydrate;
pub mod mid_street;
pub mod query_builder;
pub mod resolver;
pub mod spatial;

use std::path::PathBuf;
use std::sync::Arc;

use tantivy::{Index, IndexReader};

use crabfinder_indexer::index::CrabSchema;

use crate::abbreviation::AbbreviationMap;
use crate::addr_count::SharedAddressCountIndex;
use crate::mid_street::MidStreetIndex;
use crate::spatial::SharedSpatialIndex;

/// Shared service state, cloned cheaply via internal `Arc`-ing. Used as
/// the Axum `State<T>` for both the HTTP routes (in `crabfinder-service`)
/// and the eventual batch HTTP route. The batch CLI builds an equivalent
/// state by hand from the same loaders — there's no second `AppState`
/// shape.
#[derive(Clone)]
pub struct AppState {
    pub schema: Arc<CrabSchema>,
    pub index: Arc<Index>,
    pub reader: IndexReader,
    pub started_at: chrono_lite::Instant,
    pub index_path: Arc<PathBuf>,
    /// Built once at startup. Re-indexing while the service is running has
    /// no effect on this — restart to pick up changes.
    pub spatial: SharedSpatialIndex,
    /// When true, `/geocode` bypasses the notability tweak_score and uses
    /// `order_by_score()` instead. Set by `--no-notability-boost` at startup
    /// to support A/B isolation against a noisy rebuild baseline.
    pub notability_enabled: bool,
    /// Loaded from `--abbreviation-dict`, or an empty (identity) map when the
    /// flag is absent. Used by `/geocode` to expand abbreviated query tokens.
    pub abbrev: Arc<AbbreviationMap>,
    /// Per-segment `DocId → street_address_count`. Built once at startup
    /// from the doc store. Used by `/geocode`'s tweak_score to break ties
    /// on ambiguous queries (busy streets > obscure variants).
    pub addr_counts: SharedAddressCountIndex,
    /// Schema-v7 mid_street side-table. Loaded once at startup from
    /// `<index>/mid-street-v1.bin`. When the file is absent (v6 index, or
    /// v7 index built by a pre-v7 indexer), this is `MidStreetIndex::empty()`
    /// and `Confidence::MedianStreet` never fires.
    pub mid_street: Arc<MidStreetIndex>,
}

/// Trivial monotonic-clock wrapper to keep tokio / chrono out of
/// `crabfinder-core` (and the rest of the library). The HTTP `/health`
/// handler reads `elapsed_secs()` to compute uptime.
pub mod chrono_lite {
    #[derive(Clone, Copy)]
    pub struct Instant(std::time::Instant);
    impl Instant {
        pub fn now() -> Self {
            Self(std::time::Instant::now())
        }
        pub fn elapsed_secs(&self) -> u64 {
            self.0.elapsed().as_secs()
        }
    }
}
