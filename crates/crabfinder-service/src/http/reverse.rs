//! `GET /reverse` — coordinates → nearest features by great-circle distance.

use axum::{Json, extract::State};
use serde::{Deserialize, Serialize};
use tantivy::TantivyDocument;

use crate::AppState;
use crate::dedup::{MatchType, RawHit, group_by_building, raw_limit};
use crate::spatial::SpatialHit;

use super::error::{ApiQuery, ServiceError};
use super::feature::{Feature, hydrate_feature};

#[derive(Debug, Deserialize)]
pub struct ReverseParams {
    pub lat: f64,
    pub lng: f64,
    #[serde(default = "default_reverse_limit")]
    pub limit: usize,
    #[serde(default)]
    pub radius_m: Option<f64>,
}

fn default_reverse_limit() -> usize {
    1
}

#[derive(Serialize)]
pub struct ReverseEcho {
    pub lat: f64,
    pub lng: f64,
    pub limit: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub radius_m: Option<f64>,
}

#[derive(Serialize)]
pub struct ReverseCollection {
    #[serde(rename = "type")]
    pub kind: &'static str,
    pub query: ReverseEcho,
    pub count: usize,
    pub deduplicated: bool,
    pub raw_hits: usize,
    #[serde(rename = "_note")]
    pub note: &'static str,
    pub features: Vec<Feature>,
}

pub async fn reverse(
    State(s): State<AppState>,
    ApiQuery(params): ApiQuery<ReverseParams>,
) -> Result<Json<ReverseCollection>, ServiceError> {
    validate(&params)?;
    let limit = params.limit.clamp(1, 100);

    // Oversearch so dedup can find `limit` distinct buildings; a dense city
    // block can have hundreds of units sharing very close coordinates.
    let spatial_hits = s.spatial.nearest(
        params.lat,
        params.lng,
        raw_limit(limit, false),
        params.radius_m,
    );
    let raw_hit_count = spatial_hits.len();

    // Score = -distance_m so dedup's max-score semantics resolve to
    // nearest-wins.
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
            .map_err(|e| ServiceError::internal("Tantivy doc", e))?;
        raw_hits.push(RawHit::from_doc(
            &doc,
            &s.schema,
            (-distance_m) as f32,
            MatchType::Spatial,
            i,
        ));
        docs.push(doc);
        distances.push(*distance_m);
    }

    // max(-distance) = min(distance), so groups come out nearest-first.
    let features = group_by_building(&raw_hits)
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
                Some(distances[g.representative_index]),
            )
        })
        .collect::<Result<Vec<_>, _>>()?;

    Ok(Json(ReverseCollection {
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
    }))
}

fn validate(p: &ReverseParams) -> Result<(), ServiceError> {
    if !p.lat.is_finite() || !(-90.0..=90.0).contains(&p.lat) {
        return Err(ServiceError::BadQuery(format!(
            "lat out of range [-90, 90]: {}",
            p.lat
        )));
    }
    if !p.lng.is_finite() || !(-180.0..=180.0).contains(&p.lng) {
        return Err(ServiceError::BadQuery(format!(
            "lng out of range [-180, 180]: {}",
            p.lng
        )));
    }
    if let Some(r) = p.radius_m
        && (!r.is_finite() || r < 0.0)
    {
        return Err(ServiceError::BadQuery(format!(
            "radius_m must be non-negative finite, got {r}"
        )));
    }
    Ok(())
}
