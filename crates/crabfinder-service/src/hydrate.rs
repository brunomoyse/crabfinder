//! Materialise a Tantivy doc into a transport-agnostic struct.
//!
//! The HTTP handlers further wrap `HydratedHit` into a GeoJSON `Feature`; the
//! batch resolver consumes the same struct without a GeoJSON layer. Keeps the
//! hydration logic in one place.

use tantivy::{TantivyDocument, schema::Value};

use crabfinder_indexer::index::CrabSchema;

use crate::dedup::MatchType;

/// Result of materialising one Tantivy doc + dedup metadata into a flat
/// struct. No HTTP, no GeoJSON, no Feature.
///
/// `payload` is the raw `Address` JSON as stored in the index (see
/// `crabfinder-indexer/src/index.rs` — payload field of the Tantivy
/// schema). HTTP /geocode passes it through verbatim as
/// `Feature::properties`; batch deserializes it via serde into
/// `crabfinder_core::Address` for typed access.
#[derive(Debug, Clone)]
pub struct HydratedHit {
    pub lat: f64,
    pub lon: f64,
    pub score: f32,
    pub match_type: MatchType,
    pub unit_count: u32,
    pub building_group_id: String,
    pub payload: serde_json::Value,
    pub distance_m: Option<f64>,
}

/// Errors returned by `hydrate()`. Kept simple — caller maps to its own error
/// type (HTTP 500 for service, batch error row for CSV).
#[derive(Debug, thiserror::Error)]
pub enum HydrateError {
    #[error("doc missing required field: {0}")]
    MissingField(&'static str),
}

/// Pull lat/lon + payload JSON out of a Tantivy doc and assemble a
/// `HydratedHit`. The payload is parsed as JSON; on parse failure we fall
/// back to `{}` rather than erroring, matching the previous behaviour of the
/// inlined `hydrate()` in main.rs.
pub fn hydrate(
    doc: &TantivyDocument,
    schema: &CrabSchema,
    score: f32,
    match_type: MatchType,
    unit_count: u32,
    building_group_id: String,
    distance_m: Option<f64>,
) -> Result<HydratedHit, HydrateError> {
    let lat = doc
        .get_first(schema.lat)
        .and_then(|v| v.as_f64())
        .ok_or(HydrateError::MissingField("lat"))?;
    let lon = doc
        .get_first(schema.lon)
        .and_then(|v| v.as_f64())
        .ok_or(HydrateError::MissingField("lon"))?;

    let payload_str = doc
        .get_first(schema.payload)
        .and_then(|v| v.as_str())
        .unwrap_or("{}");
    let payload: serde_json::Value =
        serde_json::from_str(payload_str).unwrap_or(serde_json::json!({}));

    Ok(HydratedHit {
        lat,
        lon,
        score,
        match_type,
        unit_count,
        building_group_id,
        payload,
        distance_m,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crabfinder_test_support::{AddressBuilder, TestIndex};
    use tantivy::TantivyDocument;

    fn louise_32() -> TestIndex {
        TestIndex::build(&[AddressBuilder::new("100", "3919", "32")
            .street(Some("Avenue Louise"), Some("Louizalaan"))
            .box_no("1")
            .postal("1050")
            .at(50.8345, 4.3567)
            .build()])
    }

    fn first_doc(ti: &TestIndex) -> TantivyDocument {
        ti.reader
            .searcher()
            .doc(tantivy::DocAddress::new(0, 0))
            .unwrap()
    }

    #[test]
    fn extracts_lat_lon_and_metadata() {
        let ti = louise_32();
        let hit = hydrate(
            &first_doc(&ti),
            &ti.schema,
            42.0,
            MatchType::Strict,
            3,
            "bgid".into(),
            None,
        )
        .unwrap();
        assert!((hit.lat - 50.8345).abs() < 1e-9);
        assert!((hit.lon - 4.3567).abs() < 1e-9);
        assert_eq!(hit.score, 42.0);
        assert_eq!(hit.match_type, MatchType::Strict);
        assert_eq!(hit.unit_count, 3);
        assert_eq!(hit.building_group_id, "bgid");
        assert_eq!(hit.distance_m, None);
    }

    #[test]
    fn parses_payload_json() {
        let ti = louise_32();
        let hit = hydrate(
            &first_doc(&ti),
            &ti.schema,
            1.0,
            MatchType::Strict,
            1,
            "b".into(),
            None,
        )
        .unwrap();
        assert_eq!(hit.payload["house_number"], "32");
        assert_eq!(hit.payload["street_name"]["fr"], "Avenue Louise");
    }

    #[test]
    fn propagates_distance() {
        let ti = louise_32();
        let hit = hydrate(
            &first_doc(&ti),
            &ti.schema,
            1.0,
            MatchType::Spatial,
            1,
            "b".into(),
            Some(123.45),
        )
        .unwrap();
        assert_eq!(hit.distance_m, Some(123.45));
        assert_eq!(hit.match_type, MatchType::Spatial);
    }

    #[test]
    fn missing_coordinates_is_an_error() {
        let ti = louise_32();
        let err = hydrate(
            &TantivyDocument::default(),
            &ti.schema,
            1.0,
            MatchType::Strict,
            1,
            "b".into(),
            None,
        )
        .unwrap_err();
        assert_eq!(err.to_string(), "doc missing required field: lat");
        let mut doc = TantivyDocument::default();
        doc.add_f64(ti.schema.lat, 50.0);
        let err = hydrate(
            &doc,
            &ti.schema,
            1.0,
            MatchType::Strict,
            1,
            "b".into(),
            None,
        )
        .unwrap_err();
        assert_eq!(err.to_string(), "doc missing required field: lon");
    }

    #[test]
    fn unparseable_payload_falls_back_to_empty_object() {
        let ti = louise_32();
        let mut doc = TantivyDocument::default();
        doc.add_f64(ti.schema.lat, 50.0);
        doc.add_f64(ti.schema.lon, 4.0);
        doc.add_text(ti.schema.payload, "{not json");
        let hit = hydrate(
            &doc,
            &ti.schema,
            1.0,
            MatchType::Strict,
            1,
            "b".into(),
            None,
        )
        .unwrap();
        assert_eq!(hit.payload, serde_json::json!({}));
    }
}
