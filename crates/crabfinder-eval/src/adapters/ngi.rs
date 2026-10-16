//! NGI-IGN's national geocoder ("GeoSearchEngine", built with Smals; see
//! the BeGeo 2024 deck), public and keyless at `geosearch.ngi.be` (OpenAPI
//! at `/v3/api-docs`, found 2026-10-16). Free text through
//! `/search/text?q=&source=ALL_SOURCES&projection=4326`, retried in
//! `searchMode=FULL` when the fast mode finds nothing.
//!
//! Its addresses are BeSt's: `code` is the BeSt address id and
//! `details.streetId` (`1--BXLSTR--1679--1`) the BeSt street, so answers
//! are scored on BeSt ids like crabfinder's, not only on distance.
//! Streets, municipalities and NGI toponyms come back as lines or polygons;
//! their vertex mean stands in for a point.

use serde_json::Value;

use crabfinder_core::BestId;

use super::{Answer, HttpAdapter, Request};
use crate::eval_set::EvalQuery;

pub const DEFAULT_BASE: &str = "https://geosearch.ngi.be";

pub struct Ngi {
    base: String,
}

impl Ngi {
    pub fn new(base: &str) -> Self {
        Self {
            base: base.trim_end_matches('/').to_string(),
        }
    }

    fn text(&self, q: &EvalQuery, mode: &str) -> Request {
        Request::new(format!("{}/search/text", self.base))
            .param("q", &q.query_string)
            .param("projection", "4326")
            .param("source", "ALL_SOURCES")
            .param("responseFormat", "GEOJSON")
            .param("searchMode", mode)
    }
}

/// BeSt street namespace of an NGI street id's register tag.
fn street_namespace(tag: &str) -> Option<&'static str> {
    match tag {
        "BXLSTR" => Some("https://databrussels.be/id/streetname"),
        "VLDSTR" => Some("https://data.vlaanderen.be/id/straatnaam/"),
        "WALSTR" => Some("geodata.wallonie.be/id/Streetname"),
        _ => None,
    }
}

/// crabfinder's `building_group_id` for an NGI address:
/// `{street namespace}|{street object id}|{house number}`.
fn building_group_id(street_id: &str, house_number: &str) -> Option<String> {
    let mut parts = street_id.split("--");
    let (_, tag, oid) = (parts.next()?, parts.next()?, parts.next()?);
    Some(format!("{}|{oid}|{house_number}", street_namespace(tag)?))
}

/// A point for any GeoJSON geometry: itself, else the mean of its vertices.
fn point_of(g: &Value) -> Option<(f64, f64)> {
    fn walk(v: &Value, acc: &mut (f64, f64, usize)) {
        let Some(arr) = v.as_array() else { return };
        match (
            arr.first().and_then(Value::as_f64),
            arr.get(1).and_then(Value::as_f64),
        ) {
            (Some(lon), Some(lat)) => {
                acc.0 += lat;
                acc.1 += lon;
                acc.2 += 1;
            }
            _ => arr.iter().for_each(|c| walk(c, acc)),
        }
    }
    let mut acc = (0.0, 0.0, 0);
    walk(g.get("coordinates")?, &mut acc);
    (acc.2 > 0).then(|| (acc.0 / acc.2 as f64, acc.1 / acc.2 as f64))
}

impl HttpAdapter for Ngi {
    fn request(&self, q: &EvalQuery, _limit: usize) -> Request {
        self.text(q, "FAST")
    }

    fn fallback(&self, q: &EvalQuery, _limit: usize) -> Option<Request> {
        Some(self.text(q, "FULL"))
    }

    fn parse(&self, body: &str, limit: usize) -> Result<Answer, String> {
        let v: Value = serde_json::from_str(body).map_err(|e| e.to_string())?;
        let mut a = Answer::default();
        for f in v
            .get("features")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .take(limit)
        {
            let p = &f["properties"];
            let s = |ptr: &str| p.pointer(ptr).and_then(Value::as_str);
            let best_id = match (
                s("/code/namespace"),
                s("/code/objectIdentifier"),
                s("/code/versionIdentifier"),
            ) {
                (Some(ns), Some(oid), Some(ver)) => BestId::new(ns, oid, ver).to_string(),
                _ => String::new(),
            };
            let bgid = s("/details/streetId")
                .zip(s("/houseNumber"))
                .and_then(|(st, hn)| building_group_id(st, hn))
                .unwrap_or_default();
            let kind = s("/_type").unwrap_or("").to_ascii_lowercase();
            a.features.push((bgid, best_id, kind));
            a.points.push(f.get("geometry").and_then(point_of));
        }
        Ok(a)
    }
}

#[cfg(test)]
mod tests {
    use super::super::test_util::q;
    use super::*;

    // Recorded 2026-10-16 (trimmed): an address, a street, a municipality.
    const BODY: &str = r#"{"type":"FeatureCollection","features":[
      {"type":"Feature","geometry":{"type":"Point","coordinates":[4.36652848,50.84616244]},
       "properties":{"_type":"ADDRESS","id":"1--BXLADR--210225--1",
         "code":{"namespace":"https://databrussels.be/id/address","objectIdentifier":"210225","versionIdentifier":"1"},
         "houseNumber":"16","details":{"streetId":"1--BXLSTR--1679--1","source":{"sourceOwner":"BOSA"}}}},
      {"type":"Feature","geometry":{"type":"MultiLineString","coordinates":[[[4.0,50.0],[4.2,50.2]],[[4.4,50.4]]]},
       "properties":{"_type":"STREET","hasStreetName":{}}},
      {"type":"Feature","geometry":null,"properties":{"_type":"MUNICIPALITY"}}],
      "responseHeader":{"searchType":"ADDRESS","numFound":3}}"#;

    #[test]
    fn reads_best_ids_building_groups_and_points() {
        let n = Ngi::new(DEFAULT_BASE);
        let a = n.parse(BODY, 10).unwrap();
        assert_eq!(
            a.features[0],
            (
                "https://databrussels.be/id/streetname|1679|16".into(),
                "https://databrussels.be/id/address/210225/1".into(),
                "address".into()
            )
        );
        assert_eq!(a.points[0], Some((50.84616244, 4.36652848)));
        // A line: the mean of its vertices.
        let (lat, lon) = a.points[1].unwrap();
        assert!((lat - 50.2).abs() < 1e-9 && (lon - 4.2).abs() < 1e-9);
        assert_eq!(a.features[1], ("".into(), "".into(), "street".into()));
        assert_eq!(a.points[2], None);
        assert_eq!(n.parse(BODY, 1).unwrap().points.len(), 1);
        assert!(n.parse("x", 1).is_err());
        assert!(n.parse("{}", 1).unwrap().features.is_empty());
    }

    #[test]
    fn building_groups_follow_each_register() {
        assert_eq!(
            building_group_id("1--VLDSTR--36523--2023-11-01T08:29:06+01:00", "387").as_deref(),
            Some("https://data.vlaanderen.be/id/straatnaam/|36523|387")
        );
        assert_eq!(
            building_group_id("1--WALSTR--7753589--2", "16A").as_deref(),
            Some("geodata.wallonie.be/id/Streetname|7753589|16A")
        );
        assert_eq!(building_group_id("1--XXSTR--1--1", "1"), None);
        assert_eq!(building_group_id("garbage", "1"), None);
    }

    #[test]
    fn asks_free_text_fast_then_full() {
        let n = Ngi::new("https://g/");
        let query = q("a", "Wetstraat 16 Brussel", "brussels");
        let r = n.request(&query, 5);
        assert_eq!(r.url, "https://g/search/text");
        assert!(r.params.contains(&("q", "Wetstraat 16 Brussel".into())));
        assert!(r.params.contains(&("source", "ALL_SOURCES".into())));
        assert!(r.params.contains(&("searchMode", "FAST".into())));
        let f = n.fallback(&query, 5).unwrap();
        assert!(f.params.contains(&("searchMode", "FULL".into())));
    }
}
