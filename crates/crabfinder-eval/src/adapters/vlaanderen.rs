//! Digitaal Vlaanderen's Geolocation API (`geo.api.vlaanderen.be/
//! geolocation/v4/Location?q=&c=`), no key: free text over Flanders and
//! Brussels, WGS84 points, no BeSt ids (scored on distance only). A
//! stand-in national-ish reference while the NGI geocoder has no public
//! endpoint.

use serde_json::Value;

use super::{Answer, HttpAdapter, Request, region_of};
use crate::eval_set::EvalQuery;

pub const DEFAULT_BASE: &str = "https://geo.api.vlaanderen.be/geolocation/v4/Location";

pub struct Vlaanderen {
    base: String,
}

impl Vlaanderen {
    pub fn new(base: &str) -> Self {
        Self {
            base: base.to_string(),
        }
    }
}

impl HttpAdapter for Vlaanderen {
    fn covers(&self, q: &EvalQuery) -> bool {
        matches!(region_of(q), Some("flanders" | "brussels"))
    }

    fn request(&self, q: &EvalQuery, limit: usize) -> Request {
        Request::new(&self.base)
            .param("q", &q.query_string)
            .param("c", limit.min(20).to_string())
    }

    fn parse(&self, body: &str, limit: usize) -> Result<Answer, String> {
        let v: Value = serde_json::from_str(body).map_err(|e| e.to_string())?;
        let mut a = Answer::default();
        for r in v
            .get("LocationResult")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .take(limit)
        {
            let point = (|| {
                Some((
                    r.pointer("/Location/Lat_WGS84")?.as_f64()?,
                    r.pointer("/Location/Lon_WGS84")?.as_f64()?,
                ))
            })();
            let kind = r.get("LocationType").and_then(Value::as_str).unwrap_or("");
            a.push_point(point, kind);
        }
        Ok(a)
    }
}

#[cfg(test)]
mod tests {
    use super::super::test_util::q;
    use super::*;

    #[test]
    fn reads_wgs84_points_in_order() {
        // Recorded 2026-10-16, "Korenmarkt 1, 9000 Gent" (trimmed).
        let body = r#"{"LocationResult":[
          {"Housenumber":"10","Location":{"Lat_WGS84":51.05458,"Lon_WGS84":3.72231},"LocationType":"basisregisters_huisnummer_aangeduidDoorBeheerder"},
          {"Housenumber":"11","Location":{"Lat_WGS84":51.05451,"Lon_WGS84":3.72229}},
          {"Housenumber":"12"}]}"#;
        let v = Vlaanderen::new(DEFAULT_BASE);
        let a = v.parse(body, 10).unwrap();
        assert_eq!(a.points.len(), 3);
        assert_eq!(a.points[0], Some((51.05458, 3.72231)));
        assert_eq!(a.points[2], None);
        assert_eq!(
            a.features[0].2,
            "basisregisters_huisnummer_aangeduidDoorBeheerder"
        );
        assert_eq!(a.features[0].0, "", "no BeSt ids");
        assert_eq!(v.parse(body, 1).unwrap().points.len(), 1);
        assert!(v.parse("x", 1).is_err());
        let r = v.request(&q("a", "Korenmarkt 1 Gent", "flanders"), 50);
        assert_eq!(
            r.params,
            [("q", "Korenmarkt 1 Gent".into()), ("c", "20".into())]
        );
        assert!(v.covers(&q("a", "x", "brussels")));
        assert!(!v.covers(&q("a", "x", "wallonia")));
    }
}
