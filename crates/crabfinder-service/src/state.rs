//! Shared service state and the startup sequence that builds it.
//!
//! `AppState::load` is the one place an index directory is turned into a
//! ready-to-serve state (sidecar check → Tantivy reader → R-tree →
//! address-count index → abbreviation dict → mid-street side-file). The HTTP
//! binary, the batch CLI, eval batch mode and the tests all go through it,
//! so there is no second startup path to drift.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result};
use tantivy::{Index, IndexReader, ReloadPolicy};
use tracing::info;

use crabfinder_indexer::index::{CrabSchema, register_tokenizers};
use crabfinder_indexer::meta::{IndexMeta, check_sidecar};
use crabfinder_indexer::mid_street::MID_STREET_FILENAME;

use crate::abbreviation::AbbreviationMap;
use crate::addr_count::{AddressCountIndex, SharedAddressCountIndex};
use crate::history::{HistoryIndex, SharedHistoryIndex};
use crate::landmarks::{LandmarkIndex, SharedLandmarkIndex};
use crate::localities::{LocalityIndex, SharedLocalityIndex};
use crate::mid_street::MidStreetIndex;
use crate::resolver::ResolverContext;
use crate::spatial::{SharedSpatialIndex, SpatialIndex};

/// Inputs to `AppState::load`.
#[derive(Debug, Clone)]
pub struct LoadOptions {
    /// Tantivy index directory (must carry a current `.crab-meta.json`).
    pub index: PathBuf,
    /// Optional `street-type-abbreviations-v1.yaml`. `None` → identity
    /// expansion.
    pub abbreviation_dict: Option<PathBuf>,
    /// Optional OSM landmarks CSV (`--pois`, `data/osm-pois-be-v1.csv`).
    /// `None` → no landmark search.
    pub pois: Option<PathBuf>,
    /// `false` → rank by plain BM25 (`--no-notability-boost`).
    pub notability_enabled: bool,
    /// `false` → skip the mid-street side-file (`--no-mid-street`).
    pub mid_street: bool,
    /// `false` → skip the R-tree build. Only `/reverse` needs it; the batch
    /// CLI and eval batch mode save the startup cost.
    pub spatial: bool,
}

impl LoadOptions {
    /// Production defaults for `index`: notability on, mid-street on,
    /// spatial index on, no abbreviation dict.
    pub fn new(index: impl Into<PathBuf>) -> Self {
        Self {
            index: index.into(),
            abbreviation_dict: None,
            pois: None,
            notability_enabled: true,
            mid_street: true,
            spatial: true,
        }
    }

    pub fn abbreviation_dict(mut self, p: impl Into<PathBuf>) -> Self {
        self.abbreviation_dict = Some(p.into());
        self
    }

    pub fn pois(mut self, p: impl Into<PathBuf>) -> Self {
        self.pois = Some(p.into());
        self
    }
}

/// Shared service state, cloned cheaply via internal `Arc`-ing. Used as the
/// Axum `State<T>` for every route, and by the batch CLI / eval batch mode
/// through `resolver_context()`.
#[derive(Clone)]
pub struct AppState {
    pub schema: Arc<CrabSchema>,
    pub index: Arc<Index>,
    pub reader: IndexReader,
    pub meta: IndexMeta,
    pub started_at: chrono_lite::Instant,
    pub index_path: Arc<PathBuf>,
    /// Built once at startup. Re-indexing while the service is running has
    /// no effect on this — restart to pick up changes. Empty when
    /// `LoadOptions::spatial` is false.
    pub spatial: SharedSpatialIndex,
    /// When false, ranking falls back to plain BM25 (`order_by_score()`).
    /// Set by `--no-notability-boost` to support A/B isolation against a
    /// noisy rebuild baseline.
    pub notability_enabled: bool,
    /// Loaded from `--abbreviation-dict`, or an empty (identity) map.
    pub abbrev: Arc<AbbreviationMap>,
    /// Per-segment `DocId → street_address_count`, the ranking tiebreaker.
    pub addr_counts: SharedAddressCountIndex,
    /// Schema-v7 mid_street side-table. `MidStreetIndex::empty()` when the
    /// file is absent or disabled, in which case `Confidence::MedianStreet`
    /// falls back to the top hit's own coordinates.
    pub mid_street: Arc<MidStreetIndex>,
    /// Schema-v8 localities (`localities-v1.json`); empty when absent.
    pub localities: SharedLocalityIndex,
    /// OSM landmarks from `--pois`; empty when not supplied.
    pub landmarks: SharedLandmarkIndex,
    /// Schema-v12 retired / reserved / rejected addresses
    /// (`history-v1.json.gz`); empty when absent.
    pub history: SharedHistoryIndex,
}

impl AppState {
    /// Open `opts.index` and build every in-memory structure the service
    /// needs. Fails on a schema-version mismatch (the error carries the
    /// rebuild hint) or on any unreadable input.
    pub fn load(opts: &LoadOptions) -> Result<Self> {
        let meta = check_sidecar(&opts.index).map_err(anyhow::Error::new)?;
        info!(
            schema_version = meta.schema_version,
            tokenizer = %meta.tokenizer_name,
            "index sidecar OK"
        );

        let schema = Arc::new(CrabSchema::build());
        let index = Arc::new(
            Index::open_in_dir(&opts.index)
                .with_context(|| format!("opening Tantivy index at {:?}", opts.index))?,
        );
        register_tokenizers(&index);
        let reader: IndexReader = index
            .reader_builder()
            .reload_policy(ReloadPolicy::Manual)
            .try_into()
            .context("constructing IndexReader")?;

        let spatial = if opts.spatial {
            let t0 = std::time::Instant::now();
            let s = SpatialIndex::build_from_tantivy(&reader, &schema)
                .context("building spatial index")?;
            info!(
                points = s.point_count(),
                elapsed_ms = t0.elapsed().as_millis(),
                "spatial index built"
            );
            s
        } else {
            SpatialIndex::from_points(Vec::new())
        };

        let abbrev = match opts.abbreviation_dict.as_ref() {
            Some(p) => AbbreviationMap::load(p, &index)
                .with_context(|| format!("loading abbreviation dict {p:?}"))?,
            None => {
                info!("no abbreviation dict supplied; query tokens pass through unmodified");
                AbbreviationMap::empty()
            }
        };

        let t_ac = std::time::Instant::now();
        let addr_counts =
            AddressCountIndex::build(&reader, &schema).context("building AddressCountIndex")?;
        info!(
            elapsed_ms = t_ac.elapsed().as_millis(),
            n_docs = addr_counts.n_docs,
            n_streets = addr_counts.n_streets,
            max_count = addr_counts.max_count,
            "address-count index built"
        );

        let mid_street = if opts.mid_street {
            load_mid_street(&opts.index)?
        } else {
            info!("mid-street side-file disabled; MedianStreet uses the top hit's coordinates");
            MidStreetIndex::empty()
        };

        let localities = LocalityIndex::load(&index, &opts.index)?;
        info!(entries = localities.len(), "localities loaded");

        let landmarks = match opts.pois.as_ref() {
            Some(p) => {
                let l = LandmarkIndex::load(&index, p)?;
                info!(landmarks = l.len(), "landmarks loaded");
                l
            }
            None => LandmarkIndex::empty(),
        };

        let history = HistoryIndex::load(&opts.index)?;
        info!(records = history.len(), "address history loaded");

        if !opts.notability_enabled {
            info!("notability boost disabled: ranking falls back to pure BM25");
        }

        Ok(Self {
            schema,
            index,
            reader,
            meta,
            started_at: chrono_lite::Instant::now(),
            index_path: Arc::new(opts.index.clone()),
            spatial: Arc::new(spatial),
            notability_enabled: opts.notability_enabled,
            abbrev: Arc::new(abbrev),
            addr_counts: Arc::new(addr_counts),
            mid_street: Arc::new(mid_street),
            localities: Arc::new(localities),
            landmarks: Arc::new(landmarks),
            history: Arc::new(history),
        })
    }

    /// Borrow this state as a resolver context.
    pub fn resolver_context(&self) -> ResolverContext<'_> {
        ResolverContext {
            index: &self.index,
            reader: &self.reader,
            schema: &self.schema,
            abbrev: self.abbrev.clone(),
            addr_counts: self.addr_counts.clone(),
            notability_enabled: self.notability_enabled,
            mid_street: self.mid_street.clone(),
            localities: self.localities.clone(),
            landmarks: self.landmarks.clone(),
            history: self.history.clone(),
            spatial: self.spatial.clone(),
        }
    }
}

/// Load `<index>/mid-street-v1.bin`. A missing file yields an empty index
/// (logged by the loader), so a pre-v7 index still boots.
fn load_mid_street(index_dir: &Path) -> Result<MidStreetIndex> {
    let path = index_dir.join(MID_STREET_FILENAME);
    MidStreetIndex::load(&path).with_context(|| format!("loading mid_street side-file {path:?}"))
}

/// Trivial monotonic-clock wrapper to keep tokio / chrono out of
/// `crabfinder-core`. The `/health` handler reads `elapsed_secs()`.
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
