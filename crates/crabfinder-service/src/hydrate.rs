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
    use crabfinder_indexer::index::{CrabIndexWriter, register_tokenizers};
    use tantivy::{Index, IndexReader, ReloadPolicy};

    /// Build a tiny in-RAM index with one stored doc carrying lat/lon +
    /// a payload, so hydrate() has something realistic to chew on. We
    /// reuse the production CrabIndexWriter to be sure the doc layout
    /// matches what the service sees in production.
    fn one_doc_index() -> (Index, IndexReader, CrabSchema) {
        use crabfinder_core::{
            Address, AddressId, BestId, BuildingGroupId, Coordinates, MultilingualName,
            MunicipalityId, PostalInfoId, Region, Status, StreetId, StreetType, WgsCoord,
        };
        let dir = tempfile::tempdir().unwrap();
        let mut writer = CrabIndexWriter::create(dir.path()).unwrap();

        let ns_addr = "https://databrussels.be/id/address".to_string();
        let ns_street = "https://databrussels.be/id/streetname".to_string();
        let ns_muni = "https://databrussels.be/id/municipality".to_string();
        let ns_postal = "https://databrussels.be/id/postalinfo".to_string();
        let street_id = StreetId::new(BestId::new(ns_street, "3919", "1"));
        let building_group_id = BuildingGroupId::from_parts(&street_id, "32");

        let addr = Address {
            id: AddressId::new(BestId::new(ns_addr, "100", "1")),
            street_id,
            municipality_id: MunicipalityId::new(BestId::new(ns_muni, "21001", "1")),
            postal_info_id: PostalInfoId::new(BestId::new(ns_postal, "1050", "1")),
            part_of_municipality_id: None,
            region: Region::Brussels,
            status: Status::Current,
            house_number: "32".into(),
            box_number: Some("1".into()),
            officially_assigned: true,
            street_name: MultilingualName {
                fr: Some("Avenue Louise".into()),
                nl: Some("Louizalaan".into()),
                de: None,
            },
            street_type: StreetType::Streetname,
            municipality_name: MultilingualName {
                fr: Some("Bruxelles".into()),
                nl: Some("Brussel".into()),
                de: None,
            },
            postal_code: "1050".into(),
            postal_name: MultilingualName::default(),
            part_of_municipality_name: None,
            coordinates: Coordinates::Wgs84(WgsCoord {
                lat: 50.8345,
                lon: 4.3567,
            }),
            building_group_id: building_group_id.clone(),
        };
        writer.add_address(&addr).unwrap();
        writer.commit().unwrap();

        let schema = CrabSchema::build();
        let index = Index::open_in_dir(dir.path()).unwrap();
        register_tokenizers(&index);
        let reader: IndexReader = index
            .reader_builder()
            .reload_policy(ReloadPolicy::Manual)
            .try_into()
            .unwrap();
        // Keep tempdir alive by leaking it — these are tests, the dir is
        // cleaned up by the OS later. Avoid Drop-order issues with the
        // Tantivy Index file handles on macOS.
        std::mem::forget(dir);
        (index, reader, schema)
    }

    #[test]
    fn hydrate_extracts_lat_lon_from_real_doc() {
        let (_index, reader, schema) = one_doc_index();
        let searcher = reader.searcher();
        let doc_addr = tantivy::DocAddress::new(0, 0);
        let doc: tantivy::TantivyDocument = searcher.doc(doc_addr).unwrap();

        let hit = hydrate(
            &doc,
            &schema,
            42.0,
            MatchType::Strict,
            1,
            "bgid".into(),
            None,
        )
        .unwrap();

        assert!((hit.lat - 50.8345).abs() < 1e-9);
        assert!((hit.lon - 4.3567).abs() < 1e-9);
        assert_eq!(hit.score, 42.0);
        assert_eq!(hit.match_type, MatchType::Strict);
        assert_eq!(hit.unit_count, 1);
        assert_eq!(hit.distance_m, None);
    }

    #[test]
    fn hydrate_parses_payload_json() {
        let (_index, reader, schema) = one_doc_index();
        let searcher = reader.searcher();
        let doc_addr = tantivy::DocAddress::new(0, 0);
        let doc: tantivy::TantivyDocument = searcher.doc(doc_addr).unwrap();

        let hit = hydrate(
            &doc,
            &schema,
            1.0,
            MatchType::Strict,
            1,
            "bgid".into(),
            None,
        )
        .unwrap();

        // The payload is the serialized Address — verify a couple of fields
        // came through.
        assert_eq!(
            hit.payload
                .get("house_number")
                .and_then(|v| v.as_str()),
            Some("32")
        );
        assert_eq!(
            hit.payload
                .get("street_name")
                .and_then(|v| v.get("fr"))
                .and_then(|v| v.as_str()),
            Some("Avenue Louise")
        );
    }

    #[test]
    fn hydrate_propagates_distance_when_provided() {
        // /reverse uses this: distance from the query point is computed
        // upstream, hydrate just propagates it through.
        let (_index, reader, schema) = one_doc_index();
        let searcher = reader.searcher();
        let doc_addr = tantivy::DocAddress::new(0, 0);
        let doc: tantivy::TantivyDocument = searcher.doc(doc_addr).unwrap();

        let hit = hydrate(
            &doc,
            &schema,
            1.0,
            MatchType::Spatial,
            1,
            "bgid".into(),
            Some(123.45),
        )
        .unwrap();
        assert_eq!(hit.distance_m, Some(123.45));
        assert_eq!(hit.match_type, MatchType::Spatial);
    }
}
