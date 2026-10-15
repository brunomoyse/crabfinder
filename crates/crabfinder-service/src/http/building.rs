//! `GET /building/{building_group_id}/units` — every BeSt row in a building.

use axum::{
    Json,
    extract::{Path, State},
};
use tantivy::collector::TopDocs;
use tantivy::query::TermQuery;
use tantivy::schema::IndexRecordOption;
use tantivy::{TantivyDocument, Term};

use crate::AppState;
use crate::dedup::{MatchType, RawHit, sort_units_stable};

use super::error::ServiceError;
use super::feature::hydrate_feature;

/// Safety belt: a single Brussels building maxes out around 100 units.
const MAX_UNITS: usize = 1000;

pub async fn building_units(
    State(s): State<AppState>,
    Path(building_group_id): Path<String>,
) -> Result<Json<serde_json::Value>, ServiceError> {
    if building_group_id.is_empty() {
        return Err(ServiceError::BadQuery("empty building_group_id".into()));
    }
    let term = Term::from_field_text(s.schema.building_group_id, &building_group_id);
    let term_q = TermQuery::new(term, IndexRecordOption::Basic);

    let searcher = s.reader.searcher();
    let collector = TopDocs::with_limit(MAX_UNITS).order_by_score();
    let top_docs = searcher
        .search(&term_q, &collector)
        .map_err(|e| ServiceError::internal("Tantivy search", e))?;

    let mut docs: Vec<TantivyDocument> = Vec::with_capacity(top_docs.len());
    let mut hits: Vec<RawHit> = Vec::with_capacity(top_docs.len());
    for (i, (score, doc_addr)) in top_docs.iter().enumerate() {
        let doc = searcher
            .doc::<TantivyDocument>(*doc_addr)
            .map_err(|e| ServiceError::internal("Tantivy doc", e))?;
        hits.push(RawHit::from_doc(
            &doc,
            &s.schema,
            *score,
            MatchType::Strict,
            i,
        ));
        docs.push(doc);
    }

    sort_units_stable(&mut hits);

    let features = hits
        .into_iter()
        .map(|h| {
            hydrate_feature(
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

    Ok(Json(serde_json::json!({
        "type": "FeatureCollection",
        "building_group_id": building_group_id,
        "count": features.len(),
        "_note": "Phase 3: stable order by (box_number asc, None first) then address_id asc.",
        "features": features,
    })))
}
