//! GeoJSON response shapes shared by `/geocode`, `/reverse` and `/building`.

use serde::Serialize;
use tantivy::TantivyDocument;

use crabfinder_indexer::index::CrabSchema;

use crate::dedup::MatchType;
use crate::hydrate::{HydratedHit, hydrate as hydrate_hit};

use super::error::ServiceError;

#[derive(Debug, Serialize)]
pub struct Feature {
    #[serde(rename = "type")]
    pub kind: &'static str,
    pub geometry: Geometry,
    pub properties: serde_json::Value,
    pub score: f32,
    pub match_type: &'static str,
    /// How many BeSt address rows belong to the same `building_group_id`.
    /// Default 1 (a singleton or an expanded row). > 1 means the response
    /// is collapsed.
    pub unit_count: u32,
    /// Echoed at the top level of the feature so a UI can call
    /// `/building/{building_group_id}/units` without parsing the properties
    /// blob.
    pub building_group_id: String,
    /// Great-circle distance in metres from the /reverse query point.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub distance_m: Option<f64>,
}

#[derive(Debug, Serialize)]
pub struct Geometry {
    #[serde(rename = "type")]
    pub kind: &'static str,
    /// [lon, lat] per GeoJSON RFC 7946.
    pub coordinates: [f64; 2],
}

impl From<HydratedHit> for Feature {
    fn from(h: HydratedHit) -> Self {
        Feature {
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
        }
    }
}

/// Hydrate a Tantivy doc straight into a GeoJSON `Feature`.
pub fn hydrate_feature(
    doc: &TantivyDocument,
    schema: &CrabSchema,
    score: f32,
    match_type: MatchType,
    unit_count: u32,
    building_group_id: String,
    distance_m: Option<f64>,
) -> Result<Feature, ServiceError> {
    hydrate_hit(
        doc,
        schema,
        score,
        match_type,
        unit_count,
        building_group_id,
        distance_m,
    )
    .map(Feature::from)
    .map_err(|e| ServiceError::internal("hydrate", e))
}
