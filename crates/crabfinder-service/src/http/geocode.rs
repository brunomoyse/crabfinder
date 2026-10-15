//! `GET /geocode` — free text → ranked address features.

use axum::{Json, extract::State};
use serde::{Deserialize, Serialize};
use tantivy::TantivyDocument;
use tracing::warn;

use crabfinder_parser::ParsedQuery;

use crate::AppState;
use crate::dedup::{MatchType, RAW_HIT_CAP, RawHit, group_by_building, raw_limit};
use crate::focus::{apply_focus_decay, parse_focus_point};
use crate::query_builder::{
    FilterParams, analyze, build_fuzzy_query, build_strict_query, search_score_only,
    search_with_notability, searchable_field,
};

use super::error::{ApiQuery, ServiceError};
use super::feature::{Feature, hydrate_feature};

#[derive(Debug, Deserialize)]
pub struct GeocodeParams {
    pub q: String,
    #[serde(default = "default_limit")]
    pub limit: usize,
    /// When true, return one feature per BeSt address row. When false
    /// (default), collapse rows sharing a `building_group_id` into one
    /// feature with `unit_count` ≥ 1.
    #[serde(default)]
    pub expand: bool,
    /// Hard filter on `region`: ISO codes (`BE-BRU`, `BE-VLG`, `BE-WAL`) or
    /// friendly aliases (`brussels`, `flanders`, `wallonia`, …).
    #[serde(default)]
    pub region: Option<String>,
    /// Hard filter on `postal_code`. Overrides any postal code parsed out
    /// of `q`.
    #[serde(default)]
    pub postal_code: Option<String>,
    /// Optional `"lat,lng"` hint; scores decay exponentially with distance.
    #[serde(default)]
    pub focus_point: Option<String>,
}

fn default_limit() -> usize {
    10
}

/// Normalise a user-supplied region string to the indexed ISO form
/// (`BE-BRU` / `BE-VLG` / `BE-WAL`). `None` if the input doesn't resolve.
pub fn normalize_region_filter(raw: &str) -> Option<&'static str> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "be-bru" | "brussels" | "brussel" | "bruxelles" | "bru" => Some("BE-BRU"),
        "be-vlg" | "flanders" | "vlaanderen" | "flandre" | "vlg" => Some("BE-VLG"),
        "be-wal" | "wallonia" | "wallonie" | "wal" => Some("BE-WAL"),
        _ => None,
    }
}

#[derive(Serialize)]
pub struct FeatureCollection {
    #[serde(rename = "type")]
    pub kind: &'static str,
    pub query: GeocodeEcho,
    pub count: usize,
    /// "strict" if any returned building had a strict-matching member,
    /// "fuzzy" if every one came only from the OR-token fallback.
    pub match_type: &'static str,
    /// False when `?expand=true` was set.
    pub deduplicated: bool,
    /// Raw BeSt rows considered before dedup.
    pub raw_hits: usize,
    #[serde(rename = "_note")]
    pub note: &'static str,
    pub features: Vec<Feature>,
}

#[derive(Serialize)]
pub struct GeocodeEcho {
    pub q: String,
    pub limit: usize,
    pub expand: bool,
    /// What the parser made of the query — useful for debugging.
    pub parsed: ParsedEcho,
}

#[derive(Serialize)]
pub struct ParsedEcho {
    pub street_tokens: Vec<String>,
    pub house_number: Option<String>,
    pub postal_code: Option<String>,
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

pub async fn geocode(
    State(s): State<AppState>,
    ApiQuery(params): ApiQuery<GeocodeParams>,
) -> Result<Json<FeatureCollection>, ServiceError> {
    let q = params.q.trim();
    if q.is_empty() {
        return Err(ServiceError::BadQuery("empty q".into()));
    }
    let limit = params.limit.clamp(1, 100);
    let expand = params.expand;
    let raw_limit = raw_limit(limit, expand);

    let region_filter = match params.region.as_deref() {
        Some(raw) => Some(
            normalize_region_filter(raw)
                .ok_or_else(|| {
                    ServiceError::BadQuery(format!(
                        "unknown region '{raw}' (expected brussels/flanders/wallonia or BE-BRU/BE-VLG/BE-WAL)"
                    ))
                })?
                .to_string(),
        ),
        None => None,
    };
    let filters = FilterParams {
        region: region_filter,
        postal_code: params.postal_code.as_deref().map(|s| s.trim().to_string()),
    };
    let focus_point = match params.focus_point.as_deref() {
        Some(raw) => Some(parse_focus_point(raw).ok_or_else(|| {
            ServiceError::BadQuery(format!(
                "invalid focus_point '{raw}' (expected 'lat,lng' inside Belgium)"
            ))
        })?),
        None => None,
    };

    let parsed = crabfinder_parser::parse(q);
    let slots: Vec<Vec<String>> = parsed
        .street_tokens
        .iter()
        .flat_map(|t| analyze(&s.index, t))
        .map(|t| s.abbrev.expand(&t))
        .collect();

    let field = searchable_field(&s.schema);
    let searcher = s.reader.searcher();
    let run = |q: &dyn tantivy::query::Query| {
        if s.notability_enabled {
            search_with_notability(&searcher, q, raw_limit, s.addr_counts.clone())
        } else {
            search_score_only(&searcher, q, raw_limit)
        }
    };

    // Strict pass, then the OR fallback only when strict returned nothing.
    let mut top_level_mt = MatchType::Strict;
    let mut top_docs = match build_strict_query(field, &slots, &parsed, &s.schema, &filters) {
        Some(q) => {
            run(q.as_ref()).map_err(|e| ServiceError::internal("Tantivy search (strict)", e))?
        }
        None => Vec::new(),
    };
    if top_docs.is_empty()
        && let Some(q) = build_fuzzy_query(field, &slots, &parsed, &s.schema, &filters)
    {
        top_docs =
            run(q.as_ref()).map_err(|e| ServiceError::internal("Tantivy search (fuzzy)", e))?;
        if !top_docs.is_empty() {
            top_level_mt = MatchType::Fuzzy;
            warn!(q = %q_to_debug(&parsed), "strict pass empty; fell back to fuzzy");
        }
    }

    let mut docs: Vec<TantivyDocument> = Vec::with_capacity(top_docs.len());
    let mut raw_hits: Vec<RawHit> = Vec::with_capacity(top_docs.len());
    for (i, (score, doc_addr)) in top_docs.iter().enumerate() {
        let doc = searcher
            .doc::<TantivyDocument>(*doc_addr)
            .map_err(|e| ServiceError::internal("Tantivy doc", e))?;
        raw_hits.push(RawHit::from_doc(&doc, &s.schema, *score, top_level_mt, i));
        docs.push(doc);
    }

    if let Some(focus) = focus_point {
        apply_focus_decay(&mut raw_hits, focus, &s.schema, &docs);
    }

    let raw_hit_count = raw_hits.len();
    if !expand && raw_hit_count >= RAW_HIT_CAP {
        warn!(
            raw_hits = raw_hit_count,
            cap = RAW_HIT_CAP,
            q = %q_to_debug(&parsed),
            "oversearch cap reached; some buildings may be missing from the tail"
        );
    }
    let features: Vec<Feature> = if expand {
        raw_hits
            .iter()
            .take(limit)
            .map(|h| {
                hydrate_feature(
                    &docs[h.raw_index],
                    &s.schema,
                    h.score,
                    h.match_type,
                    1,
                    h.building_group_id.clone(),
                    None,
                )
            })
            .collect::<Result<_, _>>()?
    } else {
        group_by_building(&raw_hits)
            .into_iter()
            .take(limit)
            .map(|g| {
                hydrate_feature(
                    &docs[g.representative_index],
                    &s.schema,
                    g.max_score,
                    g.match_type,
                    g.unit_count,
                    g.building_group_id,
                    None,
                )
            })
            .collect::<Result<_, _>>()?
    };

    let match_type = if features
        .iter()
        .any(|f| f.match_type == MatchType::Strict.as_str())
    {
        "strict"
    } else if features.is_empty() {
        top_level_mt.as_str()
    } else {
        "fuzzy"
    };

    Ok(Json(FeatureCollection {
        kind: "FeatureCollection",
        query: GeocodeEcho {
            q: q.to_string(),
            limit,
            expand,
            parsed: ParsedEcho::from(&parsed),
        },
        count: features.len(),
        match_type,
        deduplicated: !expand,
        raw_hits: raw_hit_count,
        note: if expand {
            "Phase 3: expand=true returns one feature per BeSt row (no dedup)."
        } else {
            "Phase 3: results collapsed by building_group_id; score = max(member.score). Use ?expand=true for raw rows."
        },
        features,
    }))
}

/// Compact debug form of the parsed query for log lines.
fn q_to_debug(p: &ParsedQuery) -> String {
    format!(
        "{:?} hn={:?} pc={:?}",
        p.street_tokens, p.house_number, p.postal_code
    )
}
