//! `GET /lookup?id=<best_id>[,<best_id>…]` — addresses by BeSt id, in the
//! order asked (v2 Phase 10). Ids are the canonical
//! `{namespace}/{objectIdentifier}/{versionIdentifier}` form found in
//! `properties.id` and the batch `best_id` column, with or without the
//! version. Since v3 Phase 11.4 an id from an older dump resolves too
//! (`history::resolve_id`): `results` says per id whether it is
//! `current`, an `outdated_version`, `retired` / `reserved` / `rejected`
//! (with the successor BeSt address, if any) or `unknown`.

use axum::{Json, extract::State};
use serde::{Deserialize, Serialize};
use tantivy::{DocAddress, TantivyDocument};

use crabfinder_core::Language;
use crabfinder_indexer::history::HistoryRecord;

use crate::AppState;
use crate::dedup::{MatchType, RawHit};
use crate::history::{IdStatus, SuccessorRule, resolve_id};

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
    let (features, missing, results) = blocking(s, move |s| find_all(&s, ids, lang)).await??;
    Ok(Json(serde_json::json!({
        "type": "FeatureCollection",
        "count": features.len(),
        "missing": missing,
        "features": features,
        "results": results,
    })))
}

/// One id's answer in `results`.
#[derive(Debug, Serialize)]
struct LookupResult {
    id: String,
    status: IdStatus,
    /// Today's id of the address (`current`, `outdated_version`).
    #[serde(skip_serializing_if = "Option::is_none")]
    current_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    feature: Option<Feature>,
    /// The BeSt record of a retired / reserved / rejected id.
    #[serde(skip_serializing_if = "Option::is_none")]
    record: Option<HistoryRecord>,
    #[serde(skip_serializing_if = "Option::is_none")]
    successor: Option<SuccessorOut>,
}

#[derive(Debug, Serialize)]
struct SuccessorOut {
    rule: SuccessorRule,
    #[serde(skip_serializing_if = "Option::is_none")]
    distance_m: Option<f64>,
    feature: Feature,
}

/// `find_all`'s features, missing ids and per-id results.
type Found = (Vec<Feature>, Vec<String>, Vec<LookupResult>);

/// Per id (`history::resolve_id`): the features of the ids that are
/// addresses today (any version), the ids that are not, and every id's
/// status with its successor, if any.
fn find_all(s: &AppState, ids: Vec<String>, lang: Option<Language>) -> Result<Found, ServiceError> {
    let searcher = s.reader.searcher();
    // The feature, and the address's id.
    let feature_of = |addr: DocAddress| -> Result<(Feature, String), ServiceError> {
        let doc = searcher
            .doc::<TantivyDocument>(addr)
            .map_err(|e| ServiceError::internal("Tantivy doc", e))?;
        let hit = RawHit::from_doc(&doc, &s.schema, 1.0, MatchType::Id, 0);
        let feature = hydrate_feature(
            &doc,
            &s.schema,
            1.0,
            MatchType::Id,
            1,
            hit.building_group_id,
            None,
        )?
        .labelled(lang);
        Ok((feature, hit.address_id))
    };
    let mut features = Vec::with_capacity(ids.len());
    let mut missing = Vec::new();
    let mut results = Vec::with_capacity(ids.len());
    for id in ids {
        let r = resolve_id(&searcher, &s.schema, &s.history, &s.spatial, &id)
            .map_err(|e| ServiceError::internal("Tantivy search", e))?;
        let (feature, current_id) = r.doc.map(feature_of).transpose()?.unzip();
        match &feature {
            Some(f) => features.push(f.clone()),
            None => missing.push(id.clone()),
        }
        let successor = r
            .successor
            .map(|sc| -> Result<SuccessorOut, ServiceError> {
                Ok(SuccessorOut {
                    rule: sc.rule,
                    distance_m: sc.distance_m.map(|d| (d * 10.0).round() / 10.0),
                    feature: feature_of(sc.doc)?.0,
                })
            })
            .transpose()?;
        results.push(LookupResult {
            id,
            status: r.status,
            current_id,
            feature,
            record: r.record,
            successor,
        });
    }
    Ok((features, missing, results))
}
