//! `GET /geocode` — free text → ranked address features.

use axum::{Json, extract::State};
use serde::{Deserialize, Serialize};
use tracing::warn;

use crabfinder_parser::ParsedQuery;

use crate::AppState;
use crate::dedup::{MatchType, RAW_HIT_CAP};
use crate::focus::parse_focus_point;
use crate::resolver::{AddressQuery, SearchOptions, Tier, search};

use super::error::{ApiQuery, ServiceError};
use super::feature::Feature;

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
    /// "fuzzy" if every one came from the typo or OR-token tiers.
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
    /// Which reading produced the features: 0 = `parsed`, `n` =
    /// `parsed.alternatives[n - 1]`.
    pub reading: usize,
}

#[derive(Serialize)]
pub struct ParsedEcho {
    pub street_tokens: Vec<String>,
    pub house_number: Option<String>,
    pub box_number: Option<String>,
    pub postal_code: Option<String>,
    /// Other readings the parser considered (Phase 2), best first.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub alternatives: Vec<ParsedEcho>,
}

impl From<&ParsedQuery> for ParsedEcho {
    fn from(p: &ParsedQuery) -> Self {
        Self {
            street_tokens: p.street_tokens.clone(),
            house_number: p.house_number.clone(),
            box_number: p.box_number.clone(),
            postal_code: p.postal_code.clone(),
            alternatives: p.alternatives.iter().map(ParsedEcho::from).collect(),
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
    let focus_point = match params.focus_point.as_deref() {
        Some(raw) => Some(parse_focus_point(raw).ok_or_else(|| {
            ServiceError::BadQuery(format!(
                "invalid focus_point '{raw}' (expected 'lat,lng' inside Belgium)"
            ))
        })?),
        None => None,
    };

    let mut parsed = crabfinder_parser::parse(q);
    // An explicit `postal_code` param overrides the one parsed from q. A
    // 4-digit token the parser took as the postal code is then more
    // likely the house number: lead with a reading that kept it as one.
    let postal_override = params.postal_code.as_deref().map(|p| p.trim().to_string());
    if postal_override.is_some()
        && parsed.postal_code.is_some()
        && let Some(i) = parsed
            .alternatives
            .iter()
            .position(|a| a.postal_code.is_none())
    {
        parsed.promote(i);
    }
    let mut query = AddressQuery::from_parsed(&s.index, &parsed);
    query.for_each_reading(|r| {
        if postal_override.is_some() {
            r.postal_code.clone_from(&postal_override);
        }
        r.region_filter.clone_from(&region_filter);
        r.focus_point = focus_point;
    });
    let opts = SearchOptions {
        limit,
        expand,
        mid_street_fallback: true,
    };
    let set = search(&query, &opts, &s.resolver_context())
        .map_err(|e| ServiceError::internal("Tantivy search", e))?;
    if set.capped {
        warn!(
            raw_hits = set.raw_hits,
            cap = RAW_HIT_CAP,
            q = %q_to_debug(&parsed),
            "oversearch cap reached; some buildings may be missing from the tail"
        );
    }
    if set.tier == Tier::OrStreet || set.tier == Tier::OrWithNumber {
        warn!(q = %q_to_debug(&parsed), "strict tiers empty; fell back to fuzzy");
    }
    let raw_hit_count = set.raw_hits;
    let features: Vec<Feature> = set.candidates.into_iter().map(Feature::from).collect();

    // An empty result reports "strict": nothing fell back to fuzzy.
    let match_type = if features.is_empty()
        || features
            .iter()
            .any(|f| f.match_type == MatchType::Strict.as_str())
    {
        "strict"
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
            reading: set.reading,
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
