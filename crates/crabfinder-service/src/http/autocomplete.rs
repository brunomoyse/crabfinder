//! `GET /autocomplete?q=…` (v3 Phase 11.5): suggestions while typing, from
//! the in-memory name index (`suggest.rs`). Streets, BeSt localities and,
//! once a number is typed, the street's buildings; every label, id and
//! position is BeSt's.

use axum::{Json, extract::State};
use serde::Deserialize;

use crate::AppState;
use crate::focus::parse_focus_point;
use crate::suggest::{MAX_LIMIT, SuggestContext, SuggestQuery, suggest};

use super::blocking;
use super::error::{ApiQuery, ServiceError};
use super::feature::parse_lang;

/// Suggestions when `limit` is not given.
pub const DEFAULT_LIMIT: usize = 10;

#[derive(Debug, Deserialize)]
pub struct AutocompleteParams {
    pub q: String,
    #[serde(default)]
    pub limit: Option<usize>,
    #[serde(default)]
    pub lang: Option<String>,
    /// `lat,lng`: nearer suggestions first.
    #[serde(default)]
    pub focus_point: Option<String>,
    /// Hard filter.
    #[serde(default)]
    pub postal_code: Option<String>,
}

pub async fn autocomplete(
    State(s): State<AppState>,
    ApiQuery(p): ApiQuery<AutocompleteParams>,
) -> Result<Json<serde_json::Value>, ServiceError> {
    if s.suggest.is_empty() {
        return Err(ServiceError::Unavailable(
            "autocomplete is disabled (--no-autocomplete)".into(),
        ));
    }
    let lang = parse_lang(p.lang.as_deref())?;
    let limit = p.limit.unwrap_or(DEFAULT_LIMIT);
    if limit == 0 || limit > MAX_LIMIT {
        return Err(ServiceError::BadQuery(format!(
            "limit must be 1..={MAX_LIMIT}"
        )));
    }
    let focus = p
        .focus_point
        .as_deref()
        .map(|raw| {
            parse_focus_point(raw).ok_or_else(|| {
                ServiceError::BadQuery(format!(
                    "invalid focus_point '{raw}' (expected 'lat,lng' inside Belgium)"
                ))
            })
        })
        .transpose()?;
    if let Some(pc) = &p.postal_code
        && !(pc.len() == 4 && pc.bytes().all(|b| b.is_ascii_digit()))
    {
        return Err(ServiceError::BadQuery(format!(
            "invalid postal_code '{pc}' (expected 4 digits)"
        )));
    }
    let query = SuggestQuery {
        q: p.q.clone(),
        limit,
        lang,
        focus,
        postal_code: p.postal_code,
    };
    let suggestions = blocking(s, move |s| {
        let searcher = s.reader.searcher();
        let ctx = SuggestContext {
            index: &s.index,
            searcher: &searcher,
            schema: &s.schema,
            suggest: &s.suggest,
            abbrev: &s.abbrev,
        };
        suggest(&ctx, &query).map_err(|e| ServiceError::internal("autocomplete", e))
    })
    .await??;
    let features: Vec<serde_json::Value> = suggestions
        .into_iter()
        .map(|sg| {
            let mut props = serde_json::to_value(&sg).unwrap_or_default();
            if let Some(o) = props.as_object_mut() {
                o.remove("coordinates");
            }
            serde_json::json!({
                "type": "Feature",
                "geometry": {"type": "Point", "coordinates": sg.coordinates},
                "properties": props,
            })
        })
        .collect();
    Ok(Json(serde_json::json!({
        "type": "FeatureCollection",
        "query": p.q,
        "count": features.len(),
        "features": features,
    })))
}
