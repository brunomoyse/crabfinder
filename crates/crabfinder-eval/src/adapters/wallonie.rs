//! The SPW geocoder (`geoservices.wallonie.be/geocodeWS/geocode`, ICAR
//! based), no key: structured `street`/`house`/`city` when the entry has
//! fields, else free text `address=`. Lambert 72 points, ICAR ids (not
//! BeSt ones): scored on distance only. Wallonia only.

use serde_json::Value;

use super::{Answer, HttpAdapter, Request, lambert72, region_of, structured};
use crate::eval_set::EvalQuery;

pub const DEFAULT_BASE: &str = "https://geoservices.wallonie.be/geocodeWS/geocode";

pub struct Wallonie {
    base: String,
}

impl Wallonie {
    pub fn new(base: &str) -> Self {
        Self {
            base: base.to_string(),
        }
    }
}

impl HttpAdapter for Wallonie {
    fn covers(&self, q: &EvalQuery) -> bool {
        region_of(q) == Some("wallonia")
    }

    fn request(&self, q: &EvalQuery, _limit: usize) -> Request {
        match structured(q).filter(|s| s.city.is_some()) {
            Some(s) => Request::new(&self.base)
                .param("street", s.street.clone().unwrap_or_default())
                .param("house", s.house_number.clone().unwrap_or_default())
                .param("city", s.city.clone().unwrap_or_default()),
            None => Request::new(&self.base).param("address", &q.query_string),
        }
    }

    fn parse(&self, body: &str, limit: usize) -> Result<Answer, String> {
        let v: Value = serde_json::from_str(body).map_err(|e| e.to_string())?;
        let mut cands: Vec<&Value> = v
            .get("candidates")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .collect();
        cands.sort_by(|a, b| {
            let s = |c: &Value| c.get("score").and_then(Value::as_f64).unwrap_or(0.0);
            s(b).total_cmp(&s(a))
        });
        let mut a = Answer::default();
        for c in cands.into_iter().take(limit) {
            // A house when one matched, else whatever geometry the street has.
            let (geom, kind) = match c.pointer("/house/geometry/coordinates") {
                Some(g) => (Some(g), "house"),
                None => (c.pointer("/street/geometry/coordinates"), "street"),
            };
            let point = geom
                .and_then(Value::as_array)
                .and_then(|p| lambert72(p.first()?.as_f64()?, p.get(1)?.as_f64()?));
            a.push_point(point, kind);
        }
        Ok(a)
    }
}

#[cfg(test)]
mod tests {
    use super::super::test_util::{q, with_fields};
    use super::*;

    #[test]
    fn reads_house_points_best_score_first() {
        // Recorded 2026-10-16, "Rue de Fer 25, 5000 Namur" (+ a weaker street hit).
        let body = r#"{"candidates":[
          {"score":40,"street":{"ident":1,"name":"Rue de Fer"}},
          {"score":111,"house":{"ident":367956,"name":"25","geometry":{"type":"Point","coordinates":[185233.057099998,128403.8979]}}}]}"#;
        let w = Wallonie::new(DEFAULT_BASE);
        let a = w.parse(body, 10).unwrap();
        assert_eq!(a.features[0].2, "house");
        let (lat, lon) = a.points[0].unwrap();
        assert!(
            (lat - 50.466).abs() < 0.01 && (lon - 4.866).abs() < 0.01,
            "{lat} {lon}"
        );
        assert_eq!((a.features[1].2.as_str(), a.points[1]), ("street", None));
        assert!(w.parse("x", 1).is_err());
        assert!(w.parse("{}", 1).unwrap().points.is_empty());
    }

    #[test]
    fn structured_needs_a_city_else_free_text() {
        let w = Wallonie::new("http://w");
        let st = with_fields(q("a", "x", "wallonia"), "Rue de Fer", "25", "5000", "Namur");
        assert_eq!(
            w.request(&st, 1).params,
            [
                ("street", "Rue de Fer".into()),
                ("house", "25".into()),
                ("city", "Namur".into())
            ]
        );
        let free = q("b", "Rue de Fer 25, 5000 Namur", "wallonia");
        assert_eq!(
            w.request(&free, 1).params,
            [("address", "Rue de Fer 25, 5000 Namur".into())]
        );
        assert!(w.covers(&free));
        assert!(!w.covers(&q("c", "x", "brussels")));
    }
}
