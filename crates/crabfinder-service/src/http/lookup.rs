//! `GET /lookup?id=<best_id>[,<best_id>…]` — addresses by BeSt id, in the
//! order asked (v2 Phase 10). Ids are the canonical
//! `{namespace}/{objectIdentifier}/{versionIdentifier}` form found in
//! `properties.id` and the batch `best_id` column.

use axum::{Json, extract::State};
use serde::Deserialize;
use tantivy::collector::TopDocs;
use tantivy::query::TermQuery;
use tantivy::schema::IndexRecordOption;
use tantivy::{TantivyDocument, Term};

use crabfinder_core::Language;

use crate::AppState;
use crate::dedup::{MatchType, RawHit};

use super::blocking;
use super::error::{ApiQuery, ServiceError};
use super::feature::{Feature, hydrate_feature, parse_lang};

/// Ids per request.
pub const MAX_IDS: usize = 50;

#[derive(Debug, Deserialize)]
pub struct LookupParams {
    /// One id, or several separated by commas.
    pub id: String,
    /// Language of `properties.label`: `fr`, `nl` or `de`.
    #[serde(default)]
    pub lang: Option<String>,
}

pub async fn lookup(
    State(s): State<AppState>,
    ApiQuery(p): ApiQuery<LookupParams>,
) -> Result<Json<serde_json::Value>, ServiceError> {
    let lang = parse_lang(p.lang.as_deref())?;
    let ids: Vec<String> =
        p.id.split(',')
            .map(str::trim)
            .filter(|i| !i.is_empty())
            .map(str::to_string)
            .collect();
    if ids.is_empty() {
        return Err(ServiceError::BadQuery("id is empty".into()));
    }
    if ids.len() > MAX_IDS {
        return Err(ServiceError::BadQuery(format!(
            "too many ids: {} (max {MAX_IDS})",
            ids.len()
        )));
    }
    let (features, missing) = blocking(s, move |s| find_all(&s, ids, lang)).await??;
    Ok(Json(serde_json::json!({
        "type": "FeatureCollection",
        "count": features.len(),
        "missing": missing,
        "features": features,
    })))
}

/// The feature of every id found, and the ids not found.
fn find_all(
    s: &AppState,
    ids: Vec<String>,
    lang: Option<Language>,
) -> Result<(Vec<Feature>, Vec<String>), ServiceError> {
    let searcher = s.reader.searcher();
    let mut features = Vec::with_capacity(ids.len());
    let mut missing = Vec::new();
    for id in ids {
        let q = TermQuery::new(
            Term::from_field_text(s.schema.best_id, &id),
            IndexRecordOption::Basic,
        );
        let top = searcher
            .search(&q, &TopDocs::with_limit(1).order_by_score())
            .map_err(|e| ServiceError::internal("Tantivy search", e))?;
        let Some((_, addr)) = top.first() else {
            missing.push(id);
            continue;
        };
        let doc = searcher
            .doc::<TantivyDocument>(*addr)
            .map_err(|e| ServiceError::internal("Tantivy doc", e))?;
        let hit = RawHit::from_doc(&doc, &s.schema, 1.0, MatchType::Id, 0);
        features.push(
            hydrate_feature(
                &doc,
                &s.schema,
                1.0,
                MatchType::Id,
                1,
                hit.building_group_id,
                None,
            )?
            .labelled(lang),
        );
    }
    Ok((features, missing))
}
