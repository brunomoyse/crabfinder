//! GeoJSON response shapes shared by `/geocode`, `/reverse` and `/building`.

use serde::Serialize;
use tantivy::TantivyDocument;

use crabfinder_core::{Address, Language};
use crabfinder_indexer::index::CrabSchema;

use crate::dedup::MatchType;
use crate::hydrate::{HydratedHit, hydrate as hydrate_hit};
use crate::resolver::Candidate;

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
    /// How well the feature matches the query (`/geocode` only). Same
    /// vocabulary as the batch `confidence` column.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub confidence: Option<&'static str>,
    /// Why the match isn't `exact` (`/geocode` only; absent when exact).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fallback_reason: Option<&'static str>,
    /// Matched minus requested house number when the number was honoured
    /// (`0` for an exact number); absent otherwise.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub house_number_delta: Option<i32>,
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
            confidence: None,
            fallback_reason: None,
            house_number_delta: None,
        }
    }
}

impl From<Candidate> for Feature {
    fn from(c: Candidate) -> Self {
        Feature {
            confidence: Some(c.confidence.as_str()),
            fallback_reason: c.fallback_reason.map(|r| r.as_str()),
            house_number_delta: c.house_number_delta,
            ..Feature::from(c.hit)
        }
    }
}

impl Feature {
    /// Add `properties.label`: the address on one line in `lang`
    /// (`Address::display_name`; `None` = the address's own language).
    pub fn labelled(mut self, lang: Option<Language>) -> Self {
        if let Ok(a) = serde_json::from_value::<Address>(self.properties.clone())
            && let Some(props) = self.properties.as_object_mut()
        {
            props.insert("label".into(), a.display_name(lang).into());
        }
        self
    }
}

/// Parse the `lang` query parameter (`fr`, `nl` or `de`, any case).
pub fn parse_lang(raw: Option<&str>) -> Result<Option<Language>, ServiceError> {
    raw.map(|r| {
        r.trim()
            .to_ascii_lowercase()
            .parse::<Language>()
            .map_err(|_| {
                ServiceError::BadQuery(format!("unknown lang '{r}' (expected fr, nl or de)"))
            })
    })
    .transpose()
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
