//! crabfinder-service — Axum HTTP API.
//!
//! Routes:
//!   GET /health          → readiness + doc count
//!   GET /geocode?q=…     → structured-query geocode (phrase + AND tokens
//!                          with house-number / postal-code filters; OR
//!                          fallback marked `match_type: "fuzzy"`).
//!   GET /reverse         → reverse geocode (lat/lon → nearest features).
//!   GET /building/{id}/units → enumerate all units in a building group.
//!
//! Per file 07 §5.2, /geocode results are NOT yet deduplicated by
//! building_group_id (deferred to Phase 3). The response includes a top-level
//! `_note` field flagging this explicitly so callers can't accidentally
//! depend on the duplicated behaviour.

use std::net::{IpAddr, SocketAddr};
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result};
use axum::{
    Json, Router,
    extract::{Path, Query, State},
    http::StatusCode,
    response::IntoResponse,
    routing::get,
};
use clap::Parser;
use serde::{Deserialize, Serialize};
use tantivy::collector::TopDocs;
use tantivy::query::TermQuery;
use tantivy::schema::IndexRecordOption;
use tantivy::{Index, IndexReader, ReloadPolicy, TantivyDocument, Term, schema::Value};
use tower_http::cors::{Any, CorsLayer};
use tracing::{error, info, warn};
use tracing_subscriber::EnvFilter;

use crabfinder_indexer::index::{CrabSchema, register_tokenizers};
use crabfinder_indexer::meta::check_sidecar;
use crabfinder_parser::ParsedQuery;

// All shared logic now lives in the crate's library surface (`src/lib.rs`)
// so the future `crabfinder-batch` CLI can import the same modules without
// duplicating code. The binary keeps only HTTP-specific glue here.
use crabfinder_service::abbreviation::AbbreviationMap;
use crabfinder_service::addr_count::{AddressCountIndex, SharedAddressCountIndex};
use crabfinder_service::batch;
use crabfinder_service::dedup::{Group, MatchType, RawHit, group_by_building, sort_units_stable};
use crabfinder_service::focus::{apply_focus_decay, parse_focus_point};
use crabfinder_service::hydrate::{HydratedHit, hydrate as hydrate_hit};
use crabfinder_service::mid_street::MidStreetIndex;
use crabfinder_service::query_builder::{
    FilterParams, analyze, build_fuzzy_query, build_strict_query, search_score_only,
    search_with_notability, searchable_field,
};
use crabfinder_service::spatial::{SharedSpatialIndex, SpatialHit, SpatialIndex};

#[derive(Debug, Parser)]
#[command(
    name = "crabfinder-service",
    version,
    about = "Serve crabfinder over HTTP."
)]
struct Args {
    /// Path to the Tantivy index directory.
    #[arg(long, env = "CRABFINDER_INDEX")]
    index: PathBuf,

    /// IP address to bind. Defaults to IPv4 loopback; containers set
    /// `0.0.0.0` so the published port is reachable from outside.
    #[arg(long, env = "CRABFINDER_HOST", default_value = "127.0.0.1")]
    host: IpAddr,

    /// TCP port to listen on.
    #[arg(long, env = "CRABFINDER_PORT", default_value_t = 8080)]
    port: u16,

    /// Disable the schema-v4 notability boost at query time. The index keeps
    /// its `notability_score` column, but ranking falls back to pure BM25
    /// (`order_by_score`). Used to A/B isolate the boost's effect against an
    /// unboosted run on the *same* physical index — same segments, same
    /// tiebreak order — so any delta is purely the multiplier and not index
    /// rebuild noise.
    #[arg(long, env = "CRABFINDER_NO_NOTABILITY_BOOST")]
    no_notability_boost: bool,

    /// Path to `street-type-abbreviations-v1.yaml`. When supplied, query
    /// tokens are expanded at request time so that user-typed abbreviations
    /// ("Av.", "Stationsstr.", "Rathauspl.") match the indexed canonical
    /// forms ("Avenue", "Stationsstraat", "Rathausplatz"). Optional — when
    /// absent, the engine behaves identically to pre-Phase-6.6.
    #[arg(long, env = "CRABFINDER_ABBREVIATION_DICT")]
    abbreviation_dict: Option<PathBuf>,
}

// AppState and the `chrono_lite` clock wrapper now live in the library
// surface so the future batch CLI can construct an equivalent state.
use crabfinder_service::{AppState, chrono_lite};

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .with_target(false)
        .init();

    let args = Args::parse();
    info!(index = %args.index.display(), port = args.port, "starting crabfinder-service");

    // Refuse to open an index whose schema version doesn't match this binary.
    // The error type from `check_sidecar` already carries a rebuild-command
    // hint, so we just surface it as a startup failure.
    let meta = check_sidecar(&args.index).map_err(anyhow::Error::new)?;
    info!(
        schema_version = meta.schema_version,
        tokenizer = %meta.tokenizer_name,
        "index sidecar OK"
    );

    let schema = Arc::new(CrabSchema::build());
    let index = Arc::new(
        Index::open_in_dir(&args.index)
            .with_context(|| format!("opening Tantivy index at {:?}", args.index))?,
    );
    register_tokenizers(&index);
    let reader: IndexReader = index
        .reader_builder()
        .reload_policy(ReloadPolicy::Manual)
        .try_into()
        .context("constructing IndexReader")?;

    // Build the in-memory R-tree from the live Tantivy lat/lon columns.
    // Service binds to the listener AFTER this completes, so the first
    // /reverse request always sees a fully-built index.
    let t0 = std::time::Instant::now();
    let spatial =
        SpatialIndex::build_from_tantivy(&reader, &schema).context("building spatial index")?;
    info!(
        points = spatial.point_count(),
        elapsed_ms = t0.elapsed().as_millis(),
        "spatial index built"
    );
    let spatial: SharedSpatialIndex = Arc::new(spatial);

    if args.no_notability_boost {
        info!("--no-notability-boost set: ranking falls back to pure BM25 (tweak_score bypassed)");
    }
    let abbrev = match args.abbreviation_dict.as_ref() {
        Some(p) => AbbreviationMap::load(p, &index)
            .with_context(|| format!("loading abbreviation dict {p:?}"))?,
        None => {
            info!("no --abbreviation-dict supplied; query tokens are passed through unmodified");
            AbbreviationMap::empty()
        }
    };
    let abbrev = Arc::new(abbrev);

    // Build the per-segment address-count index used by /geocode's
    // combined tweak_score as a tiebreaker on ambiguous queries.
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
    let addr_counts: SharedAddressCountIndex = Arc::new(addr_counts);

    // Schema-v7: load the mid_street side-table. Missing file → empty index
    // (logged warning), so a v6-built index still boots — only the batch
    // resolver's `Confidence::MedianStreet` is disabled.
    let mid_street_path = args
        .index
        .join(crabfinder_indexer::mid_street::MID_STREET_FILENAME);
    let mid_street = MidStreetIndex::load(&mid_street_path)
        .with_context(|| format!("loading mid_street side-file {mid_street_path:?}"))?;
    let mid_street = Arc::new(mid_street);

    let state = AppState {
        schema,
        index,
        reader,
        started_at: chrono_lite::Instant::now(),
        index_path: Arc::new(args.index.clone()),
        spatial,
        notability_enabled: !args.no_notability_boost,
        abbrev,
        addr_counts,
        mid_street,
    };

    let app = Router::new()
        .route("/health", get(health))
        .route("/geocode", get(geocode))
        // Phase 3: dedicated endpoint for "show me all units in this
        // building." `building_group_id` is path-percent-encoded by the
        // client because the canonical form contains slashes and pipes.
        .route("/building/{building_group_id}/units", get(building_units))
        // Phase 4: reverse geocoding. Same response envelope as /geocode.
        .route("/reverse", get(reverse))
        // Lot E (v1 batch): structured-CSV / JSON batch geocoding. Streamed
        // CSV response. Body cap dialled up to 50 MiB on this route only.
        .route(
            "/batch/geocode",
            axum::routing::post(batch::batch_geocode).route_layer(batch::body_limit_layer()),
        )
        .layer(
            CorsLayer::new()
                .allow_origin(Any)
                .allow_methods(Any)
                .allow_headers(Any),
        )
        .with_state(state);

    let addr: SocketAddr = SocketAddr::new(args.host, args.port);
    let listener = tokio::net::TcpListener::bind(addr).await?;
    info!("listening at http://{addr}");
    axum::serve(listener, app)
        .await
        .context("Axum serve loop")?;
    Ok(())
}

// --- /health ---------------------------------------------------------------

#[derive(Serialize)]
struct HealthBody {
    status: &'static str,
    index_path: String,
    index_docs: u64,
    uptime_secs: u64,
}

async fn health(State(s): State<AppState>) -> impl IntoResponse {
    let searcher = s.reader.searcher();
    let docs = searcher.num_docs();
    let body = HealthBody {
        status: "ok",
        index_path: s.index_path.to_string_lossy().into_owned(),
        index_docs: docs,
        uptime_secs: s.started_at.elapsed_secs(),
    };
    (StatusCode::OK, Json(body))
}

// --- /geocode --------------------------------------------------------------

#[derive(Deserialize)]
struct GeocodeParams {
    q: String,
    #[serde(default = "default_limit")]
    limit: usize,
    /// When true, return one feature per BeSt address row (Phase 2 shape).
    /// When false (default), collapse rows sharing a `building_group_id` into
    /// one feature with `unit_count` ≥ 1.
    #[serde(default)]
    expand: bool,
    /// Hard filter on `region`. Accepts ISO codes (`BE-BRU`, `BE-VLG`,
    /// `BE-WAL`) or friendly aliases (`brussels`, `flanders`, `wallonia`).
    /// When set, only addresses in the given region are scored.
    #[serde(default)]
    region: Option<String>,
    /// Hard filter on `postal_code` (4-digit Belgian postal). When set,
    /// overrides any postal code parsed out of `q` itself.
    #[serde(default)]
    postal_code: Option<String>,
    /// Optional geographic hint as `"lat,lng"`. When set, results are
    /// re-scored after retrieval by multiplying BM25 with an exponential
    /// distance decay. Closer candidates outrank further ones on
    /// ambiguous queries without filtering distant matches out entirely.
    #[serde(default)]
    focus_point: Option<String>,
}

/// Normalise a user-supplied region string to the indexed ISO form
/// (`BE-BRU` / `BE-VLG` / `BE-WAL`). Returns `None` if the input doesn't
/// resolve.
fn normalize_region_filter(raw: &str) -> Option<&'static str> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "be-bru" | "brussels" | "brussel" | "bruxelles" | "bru" => Some("BE-BRU"),
        "be-vlg" | "flanders" | "vlaanderen" | "flandre" | "vlg" => Some("BE-VLG"),
        "be-wal" | "wallonia" | "wallonie" | "wal" => Some("BE-WAL"),
        _ => None,
    }
}

fn default_limit() -> usize {
    10
}

/// Internal oversearch multiplier: dedup happens after Tantivy ranking, so we
/// fetch `limit * RAW_HIT_MULTIPLIER` raw hits, then collapse. With Brussels'
/// per-unit modeling a single building can have 20+ units; 20× gives enough
/// headroom for limit ≤ 100 while staying well inside Tantivy's cheap range.
const RAW_HIT_MULTIPLIER: usize = 20;
const RAW_HIT_CAP: usize = 2000;

#[derive(Serialize)]
struct FeatureCollection {
    #[serde(rename = "type")]
    kind: &'static str,
    query: GeocodeEcho,
    count: usize,
    /// "strict" if any building in the response had a strict-matching member.
    /// "fuzzy"  if every returned building came only from the OR-token
    /// fallback. Per-feature `match_type` carries the same field for
    /// downstream filtering.
    match_type: &'static str,
    /// True when results are collapsed to one feature per building. False
    /// when `?expand=true` was set — matches the Phase 2 shape.
    deduplicated: bool,
    /// Convenience flag for clients: how many raw BeSt rows we considered
    /// before dedup. Useful when `unit_count` sums exceed this — that means
    /// the oversearch was the bottleneck, not the data.
    raw_hits: usize,
    #[serde(rename = "_note")]
    note: &'static str,
    features: Vec<Feature>,
}

#[derive(Serialize)]
struct GeocodeEcho {
    q: String,
    limit: usize,
    expand: bool,
    /// Echo what the parser made of the query — useful for debugging from a
    /// browser.
    parsed: ParsedEcho,
}

#[derive(Serialize)]
struct ParsedEcho {
    street_tokens: Vec<String>,
    house_number: Option<String>,
    postal_code: Option<String>,
}

impl From<&ParsedQuery> for ParsedEcho {
    fn from(p: &ParsedQuery) -> Self {
        Self {
            street_tokens: p.street_tokens.clone(),
            house_number: p.house_number.clone(),
            postal_code: p.postal_code.clone(),
        }
    }
}

#[derive(Serialize)]
struct Feature {
    #[serde(rename = "type")]
    kind: &'static str,
    geometry: Geometry,
    properties: serde_json::Value,
    score: f32,
    match_type: &'static str,
    /// How many BeSt address rows belong to the same `building_group_id`.
    /// Default 1 (a singleton or an expanded row). > 1 means the response
    /// is collapsed.
    unit_count: u32,
    /// Echoed at the top level of the feature so a UI can call
    /// `/building/{building_group_id}/units` without parsing the properties
    /// blob.
    building_group_id: String,
    /// Great-circle distance in metres from the /reverse query point.
    /// `Some` for /reverse features, `None` for /geocode and /building features.
    #[serde(skip_serializing_if = "Option::is_none")]
    distance_m: Option<f64>,
}

#[derive(Serialize)]
struct Geometry {
    #[serde(rename = "type")]
    kind: &'static str,
    /// [lon, lat] per GeoJSON RFC 7946.
    coordinates: [f64; 2],
}

#[derive(Debug, thiserror::Error)]
enum ServiceError {
    #[error("bad query: {0}")]
    BadQuery(String),
    #[error("internal: {0}")]
    Internal(anyhow::Error),
}

impl IntoResponse for ServiceError {
    fn into_response(self) -> axum::response::Response {
        let (code, msg) = match &self {
            ServiceError::BadQuery(s) => (StatusCode::BAD_REQUEST, s.clone()),
            ServiceError::Internal(e) => {
                error!(error=%e, "internal");
                (StatusCode::INTERNAL_SERVER_ERROR, e.to_string())
            }
        };
        (code, Json(serde_json::json!({ "error": msg }))).into_response()
    }
}

impl From<anyhow::Error> for ServiceError {
    fn from(e: anyhow::Error) -> Self {
        ServiceError::Internal(e)
    }
}

async fn geocode(
    State(s): State<AppState>,
    Query(params): Query<GeocodeParams>,
) -> Result<impl IntoResponse, ServiceError> {
    let q = params.q.trim();
    if q.is_empty() {
        return Err(ServiceError::BadQuery("empty q".into()));
    }
    let limit = params.limit.clamp(1, 100);
    let expand = params.expand;
    // Oversearch so that after collapsing by building_group_id we still
    // have ≥ `limit` distinct buildings in typical cases. See design note.
    let raw_limit = if expand {
        limit
    } else {
        (limit * RAW_HIT_MULTIPLIER).min(RAW_HIT_CAP)
    };

    // Resolve filter params up-front; reject unknown region values rather
    // than silently ignoring them — saves debug time downstream.
    let region_filter = match params.region.as_deref() {
        Some(raw) => match normalize_region_filter(raw) {
            Some(iso) => Some(iso.to_string()),
            None => {
                return Err(ServiceError::BadQuery(format!(
                    "unknown region '{raw}' (expected brussels/flanders/wallonia or BE-BRU/BE-VLG/BE-WAL)"
                )));
            }
        },
        None => None,
    };
    let postal_filter = params.postal_code.as_deref().map(|s| s.trim().to_string());
    let filters = FilterParams {
        region: region_filter,
        postal_code: postal_filter,
    };
    let focus_point = match params.focus_point.as_deref() {
        Some(raw) => match parse_focus_point(raw) {
            Some(p) => Some(p),
            None => {
                return Err(ServiceError::BadQuery(format!(
                    "invalid focus_point '{raw}' (expected 'lat,lng' inside Belgium)"
                )));
            }
        },
        None => None,
    };

    let parsed = crabfinder_parser::parse(q);
    let street_token_stream: Vec<String> = parsed
        .street_tokens
        .iter()
        .flat_map(|t| analyze(&s.index, t))
        .collect();
    // Per-slot abbreviation expansion: each token becomes a slot whose first
    // form is the user-typed token; expansions follow. With no
    // `--abbreviation-dict` flag, expand() is identity and every slot is a
    // singleton — query semantics match the pre-Phase-6.6 path exactly.
    let slots: Vec<Vec<String>> = street_token_stream
        .iter()
        .map(|t| s.abbrev.expand(t))
        .collect();

    let field = searchable_field(&s.schema);
    let searcher = s.reader.searcher();

    // --- Strict pass.
    let strict_query = build_strict_query(field, &slots, &parsed, &s.schema, &filters);
    let (mut top_docs, mut top_level_mt) = if let Some(q) = strict_query {
        let hits = if s.notability_enabled {
            search_with_notability(&searcher, &q, raw_limit, s.addr_counts.clone())
        } else {
            search_score_only(&searcher, &q, raw_limit)
        }
        .map_err(|e| ServiceError::Internal(anyhow::anyhow!("Tantivy search (strict): {e}")))?;
        (hits, MatchType::Strict)
    } else {
        (Vec::new(), MatchType::Strict)
    };

    // --- Fuzzy fallback (only when strict returned nothing).
    if top_docs.is_empty() {
        if let Some(q) = build_fuzzy_query(field, &slots, &parsed, &s.schema, &filters) {
            top_docs = if s.notability_enabled {
                search_with_notability(&searcher, &q, raw_limit, s.addr_counts.clone())
            } else {
                search_score_only(&searcher, &q, raw_limit)
            }
            .map_err(|e| ServiceError::Internal(anyhow::anyhow!("Tantivy search (fuzzy): {e}")))?;
            if !top_docs.is_empty() {
                top_level_mt = MatchType::Fuzzy;
                warn!(q = %q_to_debug(&parsed), "strict pass empty; fell back to fuzzy");
            }
        }
    }

    // Materialise the docs once — we may re-index them after grouping.
    let mut docs: Vec<TantivyDocument> = Vec::with_capacity(top_docs.len());
    let mut raw_hits: Vec<RawHit> = Vec::with_capacity(top_docs.len());
    for (i, (score, doc_addr)) in top_docs.iter().enumerate() {
        let doc = searcher
            .doc::<TantivyDocument>(*doc_addr)
            .map_err(|e| ServiceError::Internal(anyhow::anyhow!("Tantivy doc: {e}")))?;
        let bgid = doc
            .get_first(s.schema.building_group_id)
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let box_no = doc
            .get_first(s.schema.box_number)
            .and_then(|v| v.as_str())
            .map(|x| x.to_string());
        let addr_id = doc
            .get_first(s.schema.best_id)
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        raw_hits.push(RawHit {
            score: *score,
            building_group_id: bgid,
            box_number: box_no,
            address_id: addr_id,
            match_type: top_level_mt,
            raw_index: i,
        });
        docs.push(doc);
    }

    // Apply focus_point decay (mutates scores + re-sorts). No-op when
    // focus_point is None, preserving the pre-decay Tantivy order.
    if let Some(focus) = focus_point {
        apply_focus_decay(&mut raw_hits, focus, &s.schema, &docs);
    }

    let raw_hit_count = raw_hits.len();
    // Saturation signal: when the oversearch hits the cap, dedup may be
    // truncating low-ranked buildings off the end. Cheap observability —
    // surface in logs without changing the algorithm. If this fires
    // routinely on a representative query workload, raise RAW_HIT_CAP or
    // make the oversearch adaptive.
    if !expand && raw_hit_count >= RAW_HIT_CAP {
        warn!(
            raw_hits = raw_hit_count,
            cap = RAW_HIT_CAP,
            q = %q_to_debug(&parsed),
            "oversearch cap reached; some buildings may be missing from the tail"
        );
    }
    let features: Vec<Feature> = if expand {
        // Phase 2 shape: one feature per BeSt row, unit_count = 1.
        raw_hits
            .iter()
            .take(limit)
            .map(|h| {
                hydrate(
                    &docs[h.raw_index],
                    &s.schema,
                    h.score,
                    h.match_type,
                    1,
                    h.building_group_id.clone(),
                    None,
                )
            })
            .collect::<Result<Vec<_>, _>>()?
    } else {
        let groups = group_by_building(&raw_hits);
        groups
            .into_iter()
            .take(limit)
            .map(|g: Group| {
                hydrate(
                    &docs[g.representative_index],
                    &s.schema,
                    g.max_score,
                    g.match_type,
                    g.unit_count,
                    g.building_group_id,
                    None,
                )
            })
            .collect::<Result<Vec<_>, _>>()?
    };

    // Top-level summary: strict if any returned feature is strict, else fuzzy.
    let top_level_mt_str = if features
        .iter()
        .any(|f| f.match_type == MatchType::Strict.as_str())
    {
        "strict"
    } else if features.is_empty() {
        top_level_mt.as_str()
    } else {
        "fuzzy"
    };

    let body = FeatureCollection {
        kind: "FeatureCollection",
        query: GeocodeEcho {
            q: q.to_string(),
            limit,
            expand,
            parsed: ParsedEcho::from(&parsed),
        },
        count: features.len(),
        match_type: top_level_mt_str,
        deduplicated: !expand,
        raw_hits: raw_hit_count,
        note: if expand {
            "Phase 3: expand=true returns one feature per BeSt row (no dedup)."
        } else {
            "Phase 3: results collapsed by building_group_id; score = max(member.score). Use ?expand=true for raw rows."
        },
        features,
    };
    Ok(Json(body))
}

// --- /building/:building_group_id/units ------------------------------------

async fn building_units(
    State(s): State<AppState>,
    Path(building_group_id): Path<String>,
) -> Result<impl IntoResponse, ServiceError> {
    if building_group_id.is_empty() {
        return Err(ServiceError::BadQuery("empty building_group_id".into()));
    }
    let term = Term::from_field_text(s.schema.building_group_id, &building_group_id);
    let term_q = TermQuery::new(term, IndexRecordOption::Basic);

    let searcher = s.reader.searcher();
    // Building units are bounded (a single Brussels building maxes out around
    // 100 units in practice). Cap at 1000 as a safety belt.
    let collector = TopDocs::with_limit(1000).order_by_score();
    let top_docs = searcher
        .search(&term_q, &collector)
        .map_err(|e| ServiceError::Internal(anyhow::anyhow!("Tantivy search: {e}")))?;

    let mut docs: Vec<TantivyDocument> = Vec::with_capacity(top_docs.len());
    let mut hits: Vec<RawHit> = Vec::with_capacity(top_docs.len());
    for (i, (score, doc_addr)) in top_docs.iter().enumerate() {
        let doc = searcher
            .doc::<TantivyDocument>(*doc_addr)
            .map_err(|e| ServiceError::Internal(anyhow::anyhow!("Tantivy doc: {e}")))?;
        let bgid = doc
            .get_first(s.schema.building_group_id)
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let box_no = doc
            .get_first(s.schema.box_number)
            .and_then(|v| v.as_str())
            .map(|x| x.to_string());
        let addr_id = doc
            .get_first(s.schema.best_id)
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        hits.push(RawHit {
            score: *score,
            building_group_id: bgid,
            box_number: box_no,
            address_id: addr_id,
            match_type: MatchType::Strict,
            raw_index: i,
        });
        docs.push(doc);
    }

    sort_units_stable(&mut hits);

    let features: Vec<Feature> = hits
        .into_iter()
        .map(|h| {
            hydrate(
                &docs[h.raw_index],
                &s.schema,
                h.score,
                h.match_type,
                1,
                h.building_group_id,
                None,
            )
        })
        .collect::<Result<Vec<_>, _>>()?;

    let body = serde_json::json!({
        "type": "FeatureCollection",
        "building_group_id": building_group_id,
        "count": features.len(),
        "_note": "Phase 3: stable order by (box_number asc, None first) then address_id asc.",
        "features": features,
    });
    Ok(Json(body))
}

// --- /reverse --------------------------------------------------------------

#[derive(Deserialize)]
struct ReverseParams {
    lat: f64,
    lng: f64,
    #[serde(default = "default_reverse_limit")]
    limit: usize,
    #[serde(default)]
    radius_m: Option<f64>,
}

fn default_reverse_limit() -> usize {
    1
}

#[derive(Serialize)]
struct ReverseEcho {
    lat: f64,
    lng: f64,
    limit: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    radius_m: Option<f64>,
}

#[derive(Serialize)]
struct ReverseCollection {
    #[serde(rename = "type")]
    kind: &'static str,
    query: ReverseEcho,
    count: usize,
    deduplicated: bool,
    raw_hits: usize,
    #[serde(rename = "_note")]
    note: &'static str,
    features: Vec<Feature>,
}

async fn reverse(
    State(s): State<AppState>,
    Query(params): Query<ReverseParams>,
) -> Result<impl IntoResponse, ServiceError> {
    if !params.lat.is_finite() || !(-90.0..=90.0).contains(&params.lat) {
        return Err(ServiceError::BadQuery(format!(
            "lat out of range [-90, 90]: {}",
            params.lat
        )));
    }
    if !params.lng.is_finite() || !(-180.0..=180.0).contains(&params.lng) {
        return Err(ServiceError::BadQuery(format!(
            "lng out of range [-180, 180]: {}",
            params.lng
        )));
    }
    if let Some(r) = params.radius_m {
        if !r.is_finite() || r < 0.0 {
            return Err(ServiceError::BadQuery(format!(
                "radius_m must be non-negative finite, got {r}"
            )));
        }
    }
    let limit = params.limit.clamp(1, 100);

    // Oversearch so dedup can find `limit` distinct buildings. Same
    // multiplier as /geocode for symmetry; capped because a dense city
    // block can have hundreds of units sharing very close coordinates.
    let raw_limit = (limit * RAW_HIT_MULTIPLIER).min(RAW_HIT_CAP);

    let spatial_hits = s
        .spatial
        .nearest(params.lat, params.lng, raw_limit, params.radius_m);
    let raw_hit_count = spatial_hits.len();

    // Materialise docs + RawHits. Score = -distance_m so dedup's max-score
    // semantics resolve to nearest-wins.
    let searcher = s.reader.searcher();
    let mut docs: Vec<TantivyDocument> = Vec::with_capacity(spatial_hits.len());
    let mut raw_hits: Vec<RawHit> = Vec::with_capacity(spatial_hits.len());
    let mut distances: Vec<f64> = Vec::with_capacity(spatial_hits.len());
    for (
        i,
        SpatialHit {
            doc_addr,
            distance_m,
            ..
        },
    ) in spatial_hits.iter().enumerate()
    {
        let doc = searcher
            .doc::<TantivyDocument>(*doc_addr)
            .map_err(|e| ServiceError::Internal(anyhow::anyhow!("Tantivy doc: {e}")))?;
        let bgid = doc
            .get_first(s.schema.building_group_id)
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let box_no = doc
            .get_first(s.schema.box_number)
            .and_then(|v| v.as_str())
            .map(|x| x.to_string());
        let addr_id = doc
            .get_first(s.schema.best_id)
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        raw_hits.push(RawHit {
            score: (-distance_m) as f32,
            building_group_id: bgid,
            box_number: box_no,
            address_id: addr_id,
            match_type: MatchType::Spatial,
            raw_index: i,
        });
        docs.push(doc);
        distances.push(*distance_m);
    }

    // Reuse dedup's grouping logic. max(-distance) = min(distance), so the
    // resulting group order is nearest-first. Representative selection
    // (lowest box number on tie) is unchanged and still the right answer.
    let groups = group_by_building(&raw_hits);

    let features: Vec<Feature> = groups
        .into_iter()
        .take(limit)
        .map(|g| {
            // The representative's distance is the group's min distance
            // (already by max-score-as-negative-distance).
            let dist = distances[g.representative_index];
            hydrate(
                &docs[g.representative_index],
                &s.schema,
                g.max_score,
                g.match_type,
                g.unit_count,
                g.building_group_id,
                Some(dist),
            )
        })
        .collect::<Result<Vec<_>, _>>()?;

    let body = ReverseCollection {
        kind: "FeatureCollection",
        query: ReverseEcho {
            lat: params.lat,
            lng: params.lng,
            limit,
            radius_m: params.radius_m,
        },
        count: features.len(),
        deduplicated: true,
        raw_hits: raw_hit_count,
        note: "Phase 4: nearest-neighbour by great-circle distance; collapsed by building_group_id.",
        features,
    };
    Ok(Json(body))
}

/// Compact debug form of the parsed query for log lines.
fn q_to_debug(p: &ParsedQuery) -> String {
    format!(
        "{:?} hn={:?} pc={:?}",
        p.street_tokens, p.house_number, p.postal_code
    )
}

/// Thin wrapper turning a `HydratedHit` (transport-agnostic) into the
/// HTTP-layer `Feature`. Hydration of the doc itself lives in
/// `crate::hydrate`; this wrapper only paints on the GeoJSON shape.
fn hydrate(
    doc: &TantivyDocument,
    schema: &CrabSchema,
    score: f32,
    match_type: MatchType,
    unit_count: u32,
    building_group_id: String,
    distance_m: Option<f64>,
) -> Result<Feature, ServiceError> {
    let h: HydratedHit = hydrate_hit(
        doc,
        schema,
        score,
        match_type,
        unit_count,
        building_group_id,
        distance_m,
    )
    .map_err(|e| ServiceError::Internal(anyhow::anyhow!("{e}")))?;
    Ok(Feature {
        kind: "Feature",
        geometry: Geometry {
            kind: "Point",
            coordinates: [h.lon, h.lat],
        },
        properties: h.payload,
        score: h.score,
        match_type: h.match_type.as_str(),
        unit_count: h.unit_count,
        building_group_id: h.building_group_id,
        distance_m: h.distance_m,
    })
}

// --- Tests -----------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crabfinder_core::{
        Address, AddressId, BestId, BuildingGroupId, Coordinates, MultilingualName, MunicipalityId,
        PostalInfoId, Region, Status, StreetId, StreetType, WgsCoord,
    };
    use crabfinder_indexer::index::CrabIndexWriter;
    use crabfinder_indexer::meta::{
        MetaError, SCHEMA_VERSION, SIDECAR_FILENAME, check_sidecar, write_sidecar,
    };
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

    static SEQ: AtomicU64 = AtomicU64::new(0);

    fn unique_tempdir() -> PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let p = std::env::temp_dir().join(format!(
            "crab-svc-test-{}-{}-{}",
            std::process::id(),
            nanos,
            SEQ.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    fn make_addr(
        addr_oid: &str,
        street_oid: &str,
        street_fr: Option<&str>,
        street_nl: Option<&str>,
        house: &str,
        box_no: Option<&str>,
        postal: &str,
        wgs_lat: f64,
        wgs_lon: f64,
    ) -> Address {
        let ns_addr = "https://databrussels.be/id/address".to_string();
        let ns_street = "https://databrussels.be/id/streetname".to_string();
        let ns_muni = "https://databrussels.be/id/municipality".to_string();
        let ns_postal = "https://databrussels.be/id/postalinfo".to_string();
        let street_id = StreetId::new(BestId::new(ns_street, street_oid, "1"));
        let building_group_id = BuildingGroupId::from_parts(&street_id, house);
        Address {
            id: AddressId::new(BestId::new(ns_addr, addr_oid, "1")),
            street_id,
            municipality_id: MunicipalityId::new(BestId::new(ns_muni, "21001", "1")),
            postal_info_id: PostalInfoId::new(BestId::new(ns_postal, postal, "1")),
            part_of_municipality_id: None,
            region: Region::Brussels,
            status: Status::Current,
            house_number: house.into(),
            box_number: box_no.map(|s| s.into()),
            officially_assigned: true,
            street_name: MultilingualName {
                fr: street_fr.map(|s| s.into()),
                nl: street_nl.map(|s| s.into()),
                de: None,
            },
            street_type: StreetType::Streetname,
            municipality_name: MultilingualName {
                fr: Some("Bruxelles".into()),
                nl: Some("Brussel".into()),
                de: None,
            },
            postal_code: postal.into(),
            postal_name: MultilingualName::default(),
            part_of_municipality_name: None,
            coordinates: Coordinates::Wgs84(WgsCoord {
                lat: wgs_lat,
                lon: wgs_lon,
            }),
            building_group_id,
        }
    }

    fn fixtures() -> Vec<Address> {
        let mut out = vec![];
        // Avenue Louise #32 — five boxes in the same building. Same
        // `building_group_id` (street_oid=3919, house=32) → dedup target.
        for (i, box_no) in ["1", "2", "3", "4", "5"].iter().enumerate() {
            out.push(make_addr(
                &format!("10{i}"),
                "3919",
                Some("Avenue Louise"),
                Some("Louizalaan"),
                "32",
                Some(box_no),
                "1050",
                50.8345,
                4.3567,
            ));
        }
        // Avenue Louise #32A — single unit, distinct building_group_id.
        out.push(make_addr(
            "120",
            "3919",
            Some("Avenue Louise"),
            Some("Louizalaan"),
            "32A",
            Some("1"),
            "1050",
            50.8345,
            4.3568,
        ));
        // Avenue Louise #34 — single unit.
        out.push(make_addr(
            "121",
            "3919",
            Some("Avenue Louise"),
            Some("Louizalaan"),
            "34",
            Some("1"),
            "1050",
            50.8346,
            4.3569,
        ));
        let extra: Vec<Address> = vec![
            // Grote-Ringlaan (NL) / Boulevard de la Grande Ceinture (FR).
            make_addr(
                "200",
                "4962",
                Some("Boulevard de la Grande Ceinture"),
                Some("Grote-Ringlaan"),
                "1",
                Some("0001"),
                "1070",
                50.8466,
                4.2981,
            ),
            // Grand-Place (FR) / Grote Markt (NL) — the actual one.
            make_addr(
                "300",
                "7",
                Some("Grand-Place"),
                Some("Grote Markt"),
                "1",
                None,
                "1000",
                50.8466,
                4.3525,
            ),
            make_addr(
                "301",
                "7",
                Some("Grand-Place"),
                Some("Grote Markt"),
                "5",
                None,
                "1000",
                50.8467,
                4.3526,
            ),
            // Rue de la Loi (FR) / Wetstraat (NL).
            make_addr(
                "400",
                "100",
                Some("Rue de la Loi"),
                Some("Wetstraat"),
                "16",
                None,
                "1000",
                50.8453,
                4.3658,
            ),
            make_addr(
                "401",
                "100",
                Some("Rue de la Loi"),
                Some("Wetstraat"),
                "200",
                None,
                "1000",
                50.8457,
                4.3712,
            ),
            // Synthetic distractor that contains "loi" as a token.
            make_addr(
                "500",
                "101",
                Some("Rue Loi-Belliard"),
                Some("Loi-Belliardstraat"),
                "1",
                None,
                "1040",
                50.8400,
                4.3700,
            ),
            // Accent fixture: Place Saint-Étienne / Sint-Étiennestraat.
            make_addr(
                "600",
                "102",
                Some("Place Saint-Étienne"),
                Some("Sint-Étiennestraat"),
                "1",
                None,
                "1040",
                50.8350,
                4.3800,
            ),
        ];
        out.extend(extra);
        out
    }

    /// Build a temp index populated with the fixtures and a current sidecar.
    fn build_test_index() -> (PathBuf, Arc<CrabSchema>, Arc<Index>) {
        let dir = unique_tempdir();
        let mut writer = CrabIndexWriter::create(&dir).unwrap();
        for f in fixtures() {
            writer.add_address(&f).unwrap();
        }
        writer.commit().unwrap();
        write_sidecar(&dir).unwrap();

        let schema = Arc::new(CrabSchema::build());
        let index = Arc::new(Index::open_in_dir(&dir).unwrap());
        register_tokenizers(&index);
        (dir, schema, index)
    }

    /// Phase 3 test harness — mirrors the production /geocode handler:
    /// search → dedup (or expand) → hydrate.
    fn run_geocode(
        index: &Index,
        schema: &CrabSchema,
        q: &str,
        limit: usize,
        expand: bool,
    ) -> (Vec<Feature>, &'static str, usize) {
        let parsed = crabfinder_parser::parse(q);
        let tokens: Vec<String> = parsed
            .street_tokens
            .iter()
            .flat_map(|t| analyze(index, t))
            .collect();
        // Tests use the identity (no-expansion) path — wrap each token in a
        // singleton slot.
        let slots: Vec<Vec<String>> = tokens.iter().map(|t| vec![t.clone()]).collect();
        let reader = index
            .reader_builder()
            .reload_policy(ReloadPolicy::Manual)
            .try_into()
            .unwrap();
        let searcher: tantivy::Searcher = reader.searcher();
        let addr_counts: SharedAddressCountIndex = Arc::new(
            AddressCountIndex::build(&reader, schema).expect("build addr_counts (test fixture)"),
        );

        let raw_limit = if expand {
            limit
        } else {
            (limit * RAW_HIT_MULTIPLIER).min(RAW_HIT_CAP)
        };

        let field = searchable_field(schema);
        let mut top_level_mt = MatchType::Strict;
        let mut top_docs: Vec<(f32, tantivy::DocAddress)> = Vec::new();

        if let Some(q) =
            build_strict_query(field, &slots, &parsed, schema, &FilterParams::default())
        {
            top_docs =
                search_with_notability(&searcher, &q, raw_limit, addr_counts.clone()).unwrap();
        }
        if top_docs.is_empty() {
            if let Some(q) =
                build_fuzzy_query(field, &slots, &parsed, schema, &FilterParams::default())
            {
                top_docs =
                    search_with_notability(&searcher, &q, raw_limit, addr_counts.clone()).unwrap();
                if !top_docs.is_empty() {
                    top_level_mt = MatchType::Fuzzy;
                }
            }
        }

        let mut docs: Vec<TantivyDocument> = Vec::with_capacity(top_docs.len());
        let mut raw_hits: Vec<RawHit> = Vec::with_capacity(top_docs.len());
        for (i, (score, doc_addr)) in top_docs.iter().enumerate() {
            let doc: TantivyDocument = searcher.doc(*doc_addr).unwrap();
            let bgid = doc
                .get_first(schema.building_group_id)
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let box_no = doc
                .get_first(schema.box_number)
                .and_then(|v| v.as_str())
                .map(|x| x.to_string());
            let addr_id = doc
                .get_first(schema.best_id)
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            raw_hits.push(RawHit {
                score: *score,
                building_group_id: bgid,
                box_number: box_no,
                address_id: addr_id,
                match_type: top_level_mt,
                raw_index: i,
            });
            docs.push(doc);
        }

        let raw_hit_count = raw_hits.len();
        let features: Vec<Feature> = if expand {
            raw_hits
                .iter()
                .take(limit)
                .map(|h| {
                    hydrate(
                        &docs[h.raw_index],
                        schema,
                        h.score,
                        h.match_type,
                        1,
                        h.building_group_id.clone(),
                        None,
                    )
                    .unwrap()
                })
                .collect()
        } else {
            let groups = group_by_building(&raw_hits);
            groups
                .into_iter()
                .take(limit)
                .map(|g| {
                    hydrate(
                        &docs[g.representative_index],
                        schema,
                        g.max_score,
                        g.match_type,
                        g.unit_count,
                        g.building_group_id,
                        None,
                    )
                    .unwrap()
                })
                .collect()
        };

        (features, top_level_mt.as_str(), raw_hit_count)
    }

    fn run_building_units(
        index: &Index,
        schema: &CrabSchema,
        building_group_id: &str,
    ) -> Vec<Feature> {
        let reader = index
            .reader_builder()
            .reload_policy(ReloadPolicy::Manual)
            .try_into()
            .unwrap();
        let searcher: tantivy::Searcher = reader.searcher();
        let term = Term::from_field_text(schema.building_group_id, building_group_id);
        let term_q = TermQuery::new(term, IndexRecordOption::Basic);
        let collector = TopDocs::with_limit(1000).order_by_score();
        let top_docs = searcher.search(&term_q, &collector).unwrap();

        let mut docs: Vec<TantivyDocument> = Vec::with_capacity(top_docs.len());
        let mut hits: Vec<RawHit> = Vec::with_capacity(top_docs.len());
        for (i, (score, doc_addr)) in top_docs.iter().enumerate() {
            let doc: TantivyDocument = searcher.doc(*doc_addr).unwrap();
            let bgid = doc
                .get_first(schema.building_group_id)
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let box_no = doc
                .get_first(schema.box_number)
                .and_then(|v| v.as_str())
                .map(|x| x.to_string());
            let addr_id = doc
                .get_first(schema.best_id)
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            hits.push(RawHit {
                score: *score,
                building_group_id: bgid,
                box_number: box_no,
                address_id: addr_id,
                match_type: MatchType::Strict,
                raw_index: i,
            });
            docs.push(doc);
        }
        sort_units_stable(&mut hits);
        hits.into_iter()
            .map(|h| {
                hydrate(
                    &docs[h.raw_index],
                    schema,
                    h.score,
                    h.match_type,
                    1,
                    h.building_group_id,
                    None,
                )
                .unwrap()
            })
            .collect()
    }

    /// Phase 4 test harness — mirrors the production /reverse handler:
    /// spatial.nearest → hydrate raw hits → dedup → hydrate representatives.
    fn run_reverse(
        index: &Index,
        schema: &CrabSchema,
        lat: f64,
        lng: f64,
        limit: usize,
        radius_m: Option<f64>,
    ) -> (Vec<Feature>, usize) {
        let reader: IndexReader = index
            .reader_builder()
            .reload_policy(ReloadPolicy::Manual)
            .try_into()
            .unwrap();
        let spatial = SpatialIndex::build_from_tantivy(&reader, schema).unwrap();
        let raw_limit = (limit * RAW_HIT_MULTIPLIER).min(RAW_HIT_CAP);
        let spatial_hits = spatial.nearest(lat, lng, raw_limit, radius_m);
        let raw_count = spatial_hits.len();
        let searcher: tantivy::Searcher = reader.searcher();

        let mut docs: Vec<TantivyDocument> = Vec::with_capacity(spatial_hits.len());
        let mut raw_hits: Vec<RawHit> = Vec::with_capacity(spatial_hits.len());
        let mut distances: Vec<f64> = Vec::with_capacity(spatial_hits.len());
        for (i, sh) in spatial_hits.iter().enumerate() {
            let doc: TantivyDocument = searcher.doc(sh.doc_addr).unwrap();
            let bgid = doc
                .get_first(schema.building_group_id)
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let box_no = doc
                .get_first(schema.box_number)
                .and_then(|v| v.as_str())
                .map(|x| x.to_string());
            let addr_id = doc
                .get_first(schema.best_id)
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            raw_hits.push(RawHit {
                score: (-sh.distance_m) as f32,
                building_group_id: bgid,
                box_number: box_no,
                address_id: addr_id,
                match_type: MatchType::Spatial,
                raw_index: i,
            });
            docs.push(doc);
            distances.push(sh.distance_m);
        }
        let groups = group_by_building(&raw_hits);
        let features: Vec<Feature> = groups
            .into_iter()
            .take(limit)
            .map(|g| {
                let dist = distances[g.representative_index];
                hydrate(
                    &docs[g.representative_index],
                    schema,
                    g.max_score,
                    g.match_type,
                    g.unit_count,
                    g.building_group_id,
                    Some(dist),
                )
                .unwrap()
            })
            .collect();
        (features, raw_count)
    }

    fn run_search(
        index: &Index,
        schema: &CrabSchema,
        q: &str,
        limit: usize,
    ) -> (Vec<(f32, serde_json::Value)>, &'static str) {
        let parsed = crabfinder_parser::parse(q);
        let tokens: Vec<String> = parsed
            .street_tokens
            .iter()
            .flat_map(|t| analyze(index, t))
            .collect();
        let slots: Vec<Vec<String>> = tokens.iter().map(|t| vec![t.clone()]).collect();
        let reader = index
            .reader_builder()
            .reload_policy(ReloadPolicy::Manual)
            .try_into()
            .unwrap();
        let searcher: tantivy::Searcher = reader.searcher();
        let addr_counts: SharedAddressCountIndex = Arc::new(
            AddressCountIndex::build(&reader, schema).expect("build addr_counts (test fixture)"),
        );

        let field = searchable_field(schema);
        let mut match_type = "strict";
        let mut top_docs: Vec<(f32, tantivy::DocAddress)> = Vec::new();

        if let Some(q) =
            build_strict_query(field, &slots, &parsed, schema, &FilterParams::default())
        {
            top_docs = search_with_notability(&searcher, &q, limit, addr_counts.clone()).unwrap();
        }
        if top_docs.is_empty() {
            if let Some(q) =
                build_fuzzy_query(field, &slots, &parsed, schema, &FilterParams::default())
            {
                top_docs =
                    search_with_notability(&searcher, &q, limit, addr_counts.clone()).unwrap();
                if !top_docs.is_empty() {
                    match_type = "fuzzy";
                }
            }
        }

        let mut out = Vec::new();
        for (score, doc_addr) in top_docs {
            let doc: TantivyDocument = searcher.doc(doc_addr).unwrap();
            let payload_str = doc
                .get_first(schema.payload)
                .and_then(|v| v.as_str())
                .unwrap_or("{}");
            let payload: serde_json::Value =
                serde_json::from_str(payload_str).unwrap_or(serde_json::json!({}));
            out.push((score, payload));
        }
        (out, match_type)
    }

    fn nl_name(p: &serde_json::Value) -> String {
        p.get("street_name")
            .and_then(|s| s.get("nl"))
            .and_then(|s| s.as_str())
            .unwrap_or("")
            .to_lowercase()
    }
    fn fr_name(p: &serde_json::Value) -> String {
        p.get("street_name")
            .and_then(|s| s.get("fr"))
            .and_then(|s| s.as_str())
            .unwrap_or("")
            .to_lowercase()
    }

    #[test]
    fn tokenizer_folds_accents_and_case() {
        let (_dir, _schema, index) = build_test_index();
        let with_accent = analyze(&index, "Étterbeek");
        let without = analyze(&index, "etterbeek");
        assert_eq!(with_accent, without, "accent folding broken");
        assert_eq!(with_accent, vec!["etterbeek"]);
    }

    #[test]
    fn tokenizer_splits_on_hyphen() {
        let (_dir, _schema, index) = build_test_index();
        assert_eq!(analyze(&index, "Grote-Ringlaan"), vec!["grote", "ringlaan"]);
        assert_eq!(analyze(&index, "Grand-Place"), vec!["grand", "place"]);
    }

    #[test]
    fn grand_place_hyphen_equals_space() {
        let (_dir, schema, index) = build_test_index();
        let (with_hyphen, _) = run_search(&index, &schema, "Grand-Place", 5);
        let (with_space, _) = run_search(&index, &schema, "Grand Place", 5);
        assert!(!with_hyphen.is_empty(), "Grand-Place returned 0 hits");
        assert!(!with_space.is_empty(), "Grand Place returned 0 hits");
        assert_eq!(
            with_hyphen[0].1.get("building_group_id"),
            with_space[0].1.get("building_group_id"),
            "Grand-Place and Grand Place produced different top results"
        );
    }

    #[test]
    fn grote_markt_does_not_return_grote_ringlaan_top() {
        let (_dir, schema, index) = build_test_index();
        let (results, _) = run_search(&index, &schema, "Grote Markt", 10);
        assert!(!results.is_empty(), "Grote Markt should match the fixture");
        let top_nl = nl_name(&results[0].1);
        assert!(
            top_nl.contains("grote markt"),
            "top hit for 'Grote Markt' was '{top_nl}', expected Grote Markt"
        );
        // Grote-Ringlaan must not appear in the top 5.
        for (rank, (_score, payload)) in results.iter().take(5).enumerate() {
            let nl = nl_name(payload);
            assert!(
                !nl.contains("ringlaan"),
                "Grote-Ringlaan appeared at rank {rank}: {nl}"
            );
        }
    }

    #[test]
    fn avenue_louise_32_filters_by_house_number() {
        let (_dir, schema, index) = build_test_index();
        let (results, _) = run_search(&index, &schema, "Avenue Louise 32", 10);
        assert!(!results.is_empty(), "Avenue Louise 32 returned 0 hits");
        for (_score, payload) in &results {
            let hn = payload
                .get("house_number")
                .and_then(|s| s.as_str())
                .unwrap_or("");
            assert_eq!(hn, "32", "house-number filter failed (got '{hn}')");
        }
    }

    #[test]
    fn rue_de_la_loi_ranks_above_loi_distractor() {
        let (_dir, schema, index) = build_test_index();
        let (results, _) = run_search(&index, &schema, "Rue de la Loi", 10);
        assert!(!results.is_empty());
        let top = fr_name(&results[0].1);
        assert!(
            top.contains("rue de la loi"),
            "top hit for 'Rue de la Loi' was '{top}'"
        );
        assert!(
            !top.contains("loi-belliard"),
            "distractor 'Rue Loi-Belliard' took the top rank"
        );
    }

    #[test]
    fn accent_insensitive_search() {
        let (_dir, schema, index) = build_test_index();
        let (with_accent, _) = run_search(&index, &schema, "Étienne", 5);
        let (without, _) = run_search(&index, &schema, "Etienne", 5);
        assert_eq!(
            with_accent.len(),
            without.len(),
            "accent and ascii forms must return identical counts"
        );
        if !with_accent.is_empty() {
            assert_eq!(
                with_accent[0].1.get("building_group_id"),
                without[0].1.get("building_group_id"),
                "top result differs between accented and ascii forms"
            );
        }
    }

    #[test]
    fn fuzzy_fallback_marks_match_type() {
        let (_dir, schema, index) = build_test_index();
        // Two-token query where only one token appears in the corpus. Strict
        // (AND-of-tokens) fails; fuzzy (OR) should still find the Louise hits.
        let (results, mt) = run_search(&index, &schema, "Louise quetzal", 10);
        assert!(!results.is_empty(), "fuzzy fallback returned 0");
        assert_eq!(mt, "fuzzy", "expected match_type=fuzzy, got {mt}");
    }

    #[test]
    fn postal_code_filter_narrows_results() {
        let (_dir, schema, index) = build_test_index();
        // Avenue Louise lives in 1050; Grand-Place in 1000. Restrict to 1050.
        let (results, _) = run_search(&index, &schema, "Louise 1050", 10);
        assert!(!results.is_empty());
        for (_, payload) in &results {
            let pc = payload
                .get("postal_code")
                .and_then(|s| s.as_str())
                .unwrap_or("");
            assert_eq!(pc, "1050");
        }
    }

    // --- Phase 3 tests ----------------------------------------------------

    fn feat_unit_count(f: &Feature) -> u32 {
        f.unit_count
    }

    fn feat_hn(f: &Feature) -> String {
        f.properties
            .get("house_number")
            .and_then(|s| s.as_str())
            .unwrap_or("")
            .to_string()
    }

    /// Building 32 has 5 fixture boxes; dedup must collapse them to one feature.
    #[test]
    fn dedup_collapses_louise_32_to_one_feature() {
        let (_dir, schema, index) = build_test_index();
        let (features, _, raw) = run_geocode(
            &index,
            &schema,
            "Avenue Louise 32",
            10,
            /* expand */ false,
        );
        assert_eq!(features.len(), 1, "expected one collapsed building");
        assert_eq!(
            feat_unit_count(&features[0]),
            5,
            "Louise 32 has 5 fixture boxes"
        );
        assert_eq!(feat_hn(&features[0]), "32");
        assert_eq!(raw, 5, "5 raw hits should have been fetched");
    }

    #[test]
    fn expand_true_returns_phase_2_shape() {
        let (_dir, schema, index) = build_test_index();
        let (features, _, raw) = run_geocode(
            &index,
            &schema,
            "Avenue Louise 32",
            10,
            /* expand */ true,
        );
        assert_eq!(features.len(), 5, "expand=true returns one feature per row");
        for f in &features {
            assert_eq!(feat_unit_count(f), 1, "expanded features have unit_count=1");
            assert_eq!(feat_hn(f), "32");
        }
        assert_eq!(raw, 5);
    }

    /// Fixture singleton buildings (Grote-Ringlaan #1, Étienne #1) must still
    /// produce one feature each with unit_count=1.
    #[test]
    fn singleton_building_unit_count_is_one() {
        let (_dir, schema, index) = build_test_index();
        let (features, _, _) = run_geocode(
            &index,
            &schema,
            "Grote-Ringlaan",
            5,
            /* expand */ false,
        );
        assert!(!features.is_empty());
        assert_eq!(feat_unit_count(&features[0]), 1);
    }

    /// Grand-Place has 2 fixture units (#1, #5) on the same street_oid but
    /// different `house_number` → different `building_group_id`. So they
    /// should NOT collapse; expect 2 distinct features.
    #[test]
    fn distinct_house_numbers_do_not_collapse() {
        let (_dir, schema, index) = build_test_index();
        let (features, _, _) =
            run_geocode(&index, &schema, "Grand-Place", 5, /* expand */ false);
        assert_eq!(
            features.len(),
            2,
            "two different house numbers, two buildings"
        );
        for f in &features {
            assert_eq!(feat_unit_count(f), 1);
        }
    }

    /// Limit semantics: limit=N counts buildings, not raw rows. Searching for
    /// "Avenue Louise" matches 7 raw rows across 3 buildings (#32, #32A, #34).
    /// limit=2 must return 2 features.
    #[test]
    fn limit_counts_buildings_not_rows() {
        let (_dir, schema, index) = build_test_index();
        let (features, _, raw) =
            run_geocode(&index, &schema, "Avenue Louise", 2, /* expand */ false);
        assert_eq!(features.len(), 2, "limit=2 → exactly 2 buildings");
        // The oversearch should have fetched all 7 Louise rows so dedup is
        // well-formed; the truncation happens AFTER grouping.
        assert!(
            raw >= 7,
            "oversearch should fetch all Louise rows (got {raw})"
        );
    }

    /// Score aggregation must be max, not sum. We rely on the pure-function
    /// dedup test for the algorithmic guarantee; this test pins the
    /// production wiring: a singleton-fixture (Étienne) that matches strict
    /// should outrank a multi-unit Louise that matches only fuzzy on a query
    /// hitting both.
    ///
    /// Strategy: query "Étienne", which only Place Saint-Étienne matches
    /// strict. Confirm the singleton ranks first. Adding a multi-unit fuzzy
    /// hit shouldn't displace it — that's the property of max.
    #[test]
    fn one_strict_singleton_beats_many_fuzzy_units_via_max() {
        let (_dir, schema, index) = build_test_index();
        // "Étienne" → only the Étienne fixture matches; everything else is
        // missing the token. The (singleton, strict) feature must rank first.
        let (features, _, _) = run_geocode(&index, &schema, "Étienne", 10, /* expand */ false);
        assert!(!features.is_empty());
        let top_fr = features[0]
            .properties
            .get("street_name")
            .and_then(|s| s.get("fr"))
            .and_then(|s| s.as_str())
            .unwrap_or("")
            .to_lowercase();
        assert!(
            top_fr.contains("étienne") || top_fr.contains("etienne"),
            "expected Étienne building on top, got '{top_fr}'"
        );
        assert_eq!(features[0].match_type, "strict");
    }

    #[test]
    fn match_type_top_level_strict_when_any_member_strict() {
        // Pure-function check using the dedup module directly. Pipeline-level
        // mixed members are exercised in `mixed_strict_and_fuzzy_aggregator`.
        use crabfinder_service::dedup::{MatchType as Mt, RawHit};
        let hits = vec![
            RawHit {
                score: 5.0,
                building_group_id: "B|1".into(),
                box_number: Some("1".into()),
                address_id: "a/1".into(),
                match_type: Mt::Fuzzy,
                raw_index: 0,
            },
            RawHit {
                score: 9.0,
                building_group_id: "B|1".into(),
                box_number: Some("2".into()),
                address_id: "a/2".into(),
                match_type: Mt::Strict,
                raw_index: 1,
            },
        ];
        let g = group_by_building(&hits);
        assert_eq!(g[0].match_type, Mt::Strict);
    }

    #[test]
    fn match_type_top_level_fuzzy_when_all_fuzzy() {
        use crabfinder_service::dedup::{MatchType as Mt, RawHit};
        let hits = vec![
            RawHit {
                score: 3.0,
                building_group_id: "B|1".into(),
                box_number: Some("1".into()),
                address_id: "a/1".into(),
                match_type: Mt::Fuzzy,
                raw_index: 0,
            },
            RawHit {
                score: 4.0,
                building_group_id: "B|1".into(),
                box_number: Some("2".into()),
                address_id: "a/2".into(),
                match_type: Mt::Fuzzy,
                raw_index: 1,
            },
        ];
        let g = group_by_building(&hits);
        assert_eq!(g[0].match_type, Mt::Fuzzy);
    }

    /// /building/.../units must return every unit of the requested building
    /// in stable order. Same call → same order.
    #[test]
    fn building_units_returns_all_five_in_stable_order() {
        let (_dir, schema, index) = build_test_index();
        // First find the building_group_id by searching.
        let (features, _, _) = run_geocode(
            &index,
            &schema,
            "Avenue Louise 32",
            5,
            /* expand */ false,
        );
        assert_eq!(features.len(), 1);
        let bgid = features[0].building_group_id.clone();
        assert!(bgid.contains("|3919|32"), "unexpected bgid: {bgid}");

        let first = run_building_units(&index, &schema, &bgid);
        let second = run_building_units(&index, &schema, &bgid);
        assert_eq!(first.len(), 5, "Louise 32 has 5 fixture boxes");
        assert_eq!(second.len(), 5);

        // Order is stable across calls.
        let first_ids: Vec<String> = first
            .iter()
            .map(|f| {
                f.properties
                    .get("id")
                    .and_then(|i| i.get("object_identifier"))
                    .and_then(|s| s.as_str())
                    .unwrap_or("")
                    .to_string()
            })
            .collect();
        let second_ids: Vec<String> = second
            .iter()
            .map(|f| {
                f.properties
                    .get("id")
                    .and_then(|i| i.get("object_identifier"))
                    .and_then(|s| s.as_str())
                    .unwrap_or("")
                    .to_string()
            })
            .collect();
        assert_eq!(first_ids, second_ids, "unit order must be stable");

        // Box numbers sorted ascending ("1","2","3","4","5").
        let boxes: Vec<String> = first
            .iter()
            .map(|f| {
                f.properties
                    .get("box_number")
                    .and_then(|s| s.as_str())
                    .unwrap_or("")
                    .to_string()
            })
            .collect();
        assert_eq!(boxes, vec!["1", "2", "3", "4", "5"]);
    }

    /// Querying an unknown building_group_id returns 0 features rather than
    /// panicking.
    #[test]
    fn building_units_unknown_id_returns_empty() {
        let (_dir, schema, index) = build_test_index();
        let feats = run_building_units(&index, &schema, "https://example.invalid|0|0");
        assert!(feats.is_empty());
    }

    #[test]
    fn schema_version_mismatch_is_reported() {
        let dir = unique_tempdir();
        // Write a stale sidecar whose version is one ahead of the binary —
        // this guarantees a mismatch regardless of the SCHEMA_VERSION value.
        let stale = serde_json::json!({
            "schema_version": SCHEMA_VERSION + 1,
            "tokenizer_name": "default",
            "built_at_unix_secs": 0i64,
        });
        std::fs::write(
            dir.join(SIDECAR_FILENAME),
            serde_json::to_string(&stale).unwrap(),
        )
        .unwrap();
        match check_sidecar(&dir) {
            Err(MetaError::VersionMismatch {
                found, expected, ..
            }) => {
                assert_eq!(found, SCHEMA_VERSION + 1);
                assert_eq!(expected, SCHEMA_VERSION);
            }
            other => panic!("expected VersionMismatch, got {other:?}"),
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    // --- Phase 4 tests ----------------------------------------------------

    /// Query at the exact Grand-Place fixture coordinate → Grand-Place is
    /// the nearest building. Distance should be < 1m.
    #[test]
    fn reverse_at_grand_place_returns_grand_place() {
        let (_dir, schema, index) = build_test_index();
        // Fixture coord for Grand-Place: 50.8466, 4.3525.
        let (features, _) = run_reverse(&index, &schema, 50.8466, 4.3525, 1, None);
        assert_eq!(features.len(), 1);
        let fr = features[0]
            .properties
            .get("street_name")
            .and_then(|s| s.get("fr"))
            .and_then(|s| s.as_str())
            .unwrap_or("")
            .to_lowercase();
        assert!(fr.contains("grand-place"), "expected Grand-Place, got {fr}");
        assert!(
            features[0].distance_m.unwrap() < 5.0,
            "distance should be ~0m, got {:?}",
            features[0].distance_m
        );
        assert_eq!(features[0].match_type, "spatial");
    }

    /// Coordinate next to Avenue Louise 32 → 1 deduplicated feature with
    /// unit_count = 5 (the 5 fixture boxes), not 5 features at the same point.
    #[test]
    fn reverse_dedupes_louise_32_units() {
        let (_dir, schema, index) = build_test_index();
        // Fixture coord for Avenue Louise 32: 50.8345, 4.3567 (all 5 boxes
        // share this point).
        let (features, raw) = run_reverse(&index, &schema, 50.8345, 4.3567, 5, None);
        assert!(!features.is_empty());
        // Top hit should be Avenue Louise 32 (distance ~0).
        let top = &features[0];
        assert!(top.distance_m.unwrap() < 5.0);
        assert_eq!(top.unit_count, 5, "Louise 32 has 5 fixture boxes");
        // The 5 raw rows must have been fetched even though dedup returns 1.
        assert!(raw >= 5);
        // building_group_id is the BGid the UI would use to fetch units.
        assert!(top.building_group_id.contains("|3919|32"));
    }

    /// distance_m must be monotonically non-decreasing across results.
    #[test]
    fn reverse_distances_monotonic_non_decreasing() {
        let (_dir, schema, index) = build_test_index();
        // Query from somewhere central; ask for several nearest buildings.
        let (features, _) = run_reverse(&index, &schema, 50.8400, 4.3600, 10, None);
        assert!(features.len() >= 3);
        for w in features.windows(2) {
            let a = w[0].distance_m.unwrap();
            let b = w[1].distance_m.unwrap();
            assert!(a <= b, "distance regression: {a} > {b}");
        }
    }

    /// radius_m=10 at a point ~150m from every fixture → empty FC, not 404.
    #[test]
    fn reverse_tight_radius_returns_empty() {
        let (_dir, schema, index) = build_test_index();
        // Pick a coord that's far (>100m) from every fixture coordinate.
        // Fixtures live around (50.83–50.85, 4.30–4.38). Choose (50.86, 4.40),
        // which is ~1.7km from the nearest fixture.
        let (features, _) = run_reverse(&index, &schema, 50.8600, 4.4000, 5, Some(10.0));
        assert!(features.is_empty(), "tiny radius should yield empty");
    }

    /// radius_m=2000 from the same point → multiple buildings, sorted by
    /// distance.
    #[test]
    fn reverse_wide_radius_returns_multiple() {
        let (_dir, schema, index) = build_test_index();
        let (features, _) = run_reverse(&index, &schema, 50.8467, 4.3600, 10, Some(2000.0));
        assert!(features.len() >= 3, "expected ≥3 fixtures within 2km");
        for f in &features {
            assert!(
                f.distance_m.unwrap() <= 2000.0,
                "feature beyond radius: {:?}",
                f.distance_m
            );
        }
    }

    /// Out-of-region query without radius: Antwerp (51.22, 4.40), the
    /// nearest Brussels fixture should still come back, far away.
    #[test]
    fn reverse_out_of_region_unbounded_returns_nearest() {
        let (_dir, schema, index) = build_test_index();
        let (features, _) = run_reverse(&index, &schema, 51.2194, 4.4025, 1, None);
        assert_eq!(features.len(), 1);
        let d = features[0].distance_m.unwrap();
        assert!(d > 30_000.0, "Antwerp→Brussels should be >30km, got {d}m");
    }

    /// Same query with a 5km radius → empty.
    #[test]
    fn reverse_out_of_region_small_radius_empty() {
        let (_dir, schema, index) = build_test_index();
        let (features, _) = run_reverse(&index, &schema, 51.2194, 4.4025, 5, Some(5_000.0));
        assert!(features.is_empty());
    }

    /// limit=N counts buildings after dedup, not raw rows.
    #[test]
    fn reverse_limit_counts_buildings_after_dedup() {
        let (_dir, schema, index) = build_test_index();
        // limit=2 around Avenue Louise — multiple fixture buildings nearby.
        let (features, _) = run_reverse(&index, &schema, 50.8345, 4.3567, 2, None);
        assert_eq!(features.len(), 2);
        // The first must be the building with all 5 units (the query point).
        assert_eq!(features[0].unit_count, 5);
    }

    /// MatchType::Spatial flows through to the per-feature field.
    #[test]
    fn reverse_features_carry_spatial_match_type() {
        let (_dir, schema, index) = build_test_index();
        let (features, _) = run_reverse(&index, &schema, 50.8467, 4.3525, 5, None);
        for f in &features {
            assert_eq!(f.match_type, "spatial");
        }
    }

    /// distance_m=0 query at an exact fixture point returns the fixture
    /// even with radius=0 (Haversine of identical points is 0).
    #[test]
    fn reverse_radius_zero_at_exact_point_returns_the_point() {
        let (_dir, schema, index) = build_test_index();
        let (features, _) = run_reverse(&index, &schema, 50.8466, 4.3525, 5, Some(0.0));
        // Note: there are two Grand-Place fixtures (house #1 and #5) that
        // share lat/lon (50.8466, 4.3525) and (50.8467, 4.3526) — but
        // dedup collapses the same building, and the *exact* match is
        // building "Grand-Place #1" at the query point.
        assert!(!features.is_empty());
        assert!(features[0].distance_m.unwrap() < 1e-6);
    }
}
