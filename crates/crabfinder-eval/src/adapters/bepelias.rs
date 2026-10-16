//! SmalsResearch's BePelias (Pelias over BeSt), run locally from its
//! Docker compose (`make build && make feed && make run`, API on :4001):
//! structured `/geocode?streetName=&houseNumber=&postCode=&postName=`
//! when the entry has fields, else `/geocode/unstructured?address=`.
//! Answers BeSt ids (`bestId`, a URI) and WGS84 points.

use serde_json::Value;

use super::{Answer, HttpAdapter, Request, structured};
use crate::eval_set::EvalQuery;

pub const DEFAULT_BASE: &str = "http://127.0.0.1:4001/REST/bepelias/v1";

pub struct BePelias {
    base: String,
}

impl BePelias {
    pub fn new(base: &str) -> Self {
        Self {
            base: base.trim_end_matches('/').to_string(),
        }
    }
}

impl HttpAdapter for BePelias {
    fn request(&self, q: &EvalQuery, _limit: usize) -> Request {
        match structured(q) {
            Some(s) => {
                let mut r = Request::new(format!("{}/geocode", self.base))
                    .param("streetName", s.street.clone().unwrap_or_default())
                    .param("houseNumber", s.house_number.clone().unwrap_or_default());
                if let Some(pc) = &s.postal_code {
                    r = r.param("postCode", pc);
                }
                if let Some(c) = &s.city {
                    r = r.param("postName", c);
                }
                r
            }
            None => Request::new(format!("{}/geocode/unstructured", self.base))
                .param("address", &q.query_string),
        }
    }

    fn parse(&self, body: &str, limit: usize) -> Result<Answer, String> {
        let v: Value = serde_json::from_str(body).map_err(|e| e.to_string())?;
        let mut a = Answer::default();
        for it in v
            .get("items")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .take(limit)
        {
            // (0, 0) is BePelias's "no position" (a BeSt address without
            // coordinates that its interpolation couldn't place).
            let point = (|| {
                Some((
                    it.pointer("/coordinates/lat")?.as_f64()?,
                    it.pointer("/coordinates/lon")?.as_f64()?,
                ))
            })()
            .filter(|&p| p != (0.0, 0.0));
            let best_id = it
                .get("bestId")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            let precision = it.get("precision").and_then(Value::as_str).unwrap_or("");
            a.features
                .push((String::new(), best_id, precision.to_string()));
            a.points.push(point);
        }
        Ok(a)
    }
}

#[cfg(test)]
mod tests {
    use super::super::test_util::{q, with_fields};
    use super::*;

    #[test]
    fn reads_best_ids_points_and_precision() {
        let body = r#"{"items":[
          {"bestId":"https://databrussels.be/id/address/219307/7","precision":"address","coordinates":{"lat":50.8355,"lon":4.3363}},
          {"bestId":"x","precision":"street"},
          {"bestId":"y","precision":"address","coordinates":{"lat":0.0,"lon":0.0}}],"total":3}"#;
        let b = BePelias::new(DEFAULT_BASE);
        let a = b.parse(body, 10).unwrap();
        assert_eq!(
            a.features[0].1,
            "https://databrussels.be/id/address/219307/7"
        );
        assert_eq!(a.features[0].2, "address");
        assert_eq!(a.points, [Some((50.8355, 4.3363)), None, None]);
        assert_eq!(b.parse(body, 1).unwrap().points.len(), 1);
        assert!(b.parse("x", 1).is_err());
    }

    #[test]
    fn structured_or_unstructured() {
        let b = BePelias::new("http://p/v1/");
        let st = with_fields(
            q("a", "x", "brussels"),
            "Avenue Fonsny",
            "20",
            "1060",
            "Saint-Gilles",
        );
        let r = b.request(&st, 1);
        assert_eq!(r.url, "http://p/v1/geocode");
        assert_eq!(r.params.len(), 4);
        assert!(r.params.contains(&("postName", "Saint-Gilles".into())));
        let r = b.request(&q("b", "Avenue Fonsny 20 1060", "brussels"), 1);
        assert_eq!(r.url, "http://p/v1/geocode/unstructured");
        assert_eq!(r.params, [("address", "Avenue Fonsny 20 1060".into())]);
    }
}
