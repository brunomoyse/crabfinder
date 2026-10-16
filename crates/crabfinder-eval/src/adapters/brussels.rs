//! The Brussels regional geocoder (Paradigm / UrbIS, BeSt model):
//! `https://geoservices.irisnet.be/geocoding/api/v1/brussels/geocoding/…`,
//! no key. Structured `searchAddress` when the entry has fields, else
//! `freeSearch/searchAnyObject`. It answers BeSt Brussels ids and Lambert
//! 72 points, so it can be scored at address level too.
//!
//! The service only searches the language it is given (`searchLanguage=fr`
//! finds no "Wetstraat"): the language is guessed from the street words
//! and the other one is tried when the first finds nothing.

use serde_json::Value;

use super::{Answer, HttpAdapter, Request, lambert72, region_of, structured};
use crate::eval_set::EvalQuery;

pub const DEFAULT_BASE: &str = "https://geoservices.irisnet.be/geocoding/api/v1/brussels/geocoding";

pub struct Brussels {
    base: String,
}

impl Brussels {
    pub fn new(base: &str) -> Self {
        Self {
            base: base.trim_end_matches('/').to_string(),
        }
    }

    fn ask(&self, q: &EvalQuery, lang: &str) -> Request {
        match structured(q) {
            Some(s) => {
                let mut r = Request::new(format!("{}/searchAddress", self.base))
                    .param("searchLanguage", lang)
                    .param("streetName", s.street.clone().unwrap_or_default())
                    .param("policeNumber", s.house_number.clone().unwrap_or_default())
                    .param("model", "BeSt");
                if let Some(pc) = &s.postal_code {
                    r = r.param("postalCode", pc);
                }
                if let Some(c) = &s.city {
                    r = r.param("municipality", c);
                }
                r
            }
            None => Request::new(format!("{}/freeSearch/searchAnyObject", self.base))
                .param("searchLanguage", lang)
                .param("freeText", &q.query_string)
                .param("model", "BeSt"),
        }
    }
}

/// `nl` when the street words look Dutch, else `fr`.
pub fn guess_language(q: &EvalQuery) -> &'static str {
    let text = structured(q)
        .and_then(|s| s.street.as_deref())
        .unwrap_or(&q.query_string)
        .to_lowercase();
    const FR: &[&str] = &[
        "rue",
        "avenue",
        "av.",
        "chaussée",
        "boulevard",
        "place",
        "quai",
        "square",
        "chemin",
        "drève",
        "clos",
        "impasse",
        "allée",
        "parvis",
        "galerie",
        "passage",
        "sentier",
        "cité",
    ];
    const NL: &[&str] = &[
        "straat",
        "laan",
        "plein",
        "steenweg",
        "dreef",
        "lei",
        "kaai",
        "weg",
        "markt",
        "hof",
        "gaanderij",
        "pad",
        "park",
        "wijk",
        "dijk",
    ];
    let words: Vec<&str> = text
        .split(|c: char| !c.is_alphanumeric() && c != '.')
        .filter(|w| !w.is_empty())
        .collect();
    if words.iter().any(|w| FR.contains(w)) {
        "fr"
    } else if words.iter().any(|w| NL.iter().any(|s| w.ends_with(s))) {
        "nl"
    } else {
        "fr"
    }
}

impl HttpAdapter for Brussels {
    fn covers(&self, q: &EvalQuery) -> bool {
        region_of(q) == Some("brussels")
    }

    fn request(&self, q: &EvalQuery, _limit: usize) -> Request {
        self.ask(q, guess_language(q))
    }

    fn fallback(&self, q: &EvalQuery, _limit: usize) -> Option<Request> {
        let other = if guess_language(q) == "fr" {
            "nl"
        } else {
            "fr"
        };
        Some(self.ask(q, other))
    }

    fn parse(&self, body: &str, limit: usize) -> Result<Answer, String> {
        let v: Value = serde_json::from_str(body).map_err(|e| e.to_string())?;
        let mut hits: Vec<(f64, &Value)> = v
            .get("resultsList")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .flat_map(|rl| {
                rl.get("result")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
            })
            .filter_map(|r| Some((r.get("score")?.as_f64().unwrap_or(0.0), r.get("address")?)))
            .collect();
        // Highest score first, the service's order among equals.
        hits.sort_by(|a, b| b.0.total_cmp(&a.0));
        let mut a = Answer::default();
        for (_, addr) in hits.into_iter().take(limit) {
            let pos = addr
                .pointer("/position/pointGeometry/point/pos/0/value")
                .and_then(Value::as_array);
            let point = pos.and_then(|p| lambert72(p.first()?.as_f64()?, p.get(1)?.as_f64()?));
            let code = |p: &str| -> Option<(String, String, String)> {
                let c = addr.get(p)?;
                Some((
                    c.get("namespace")?
                        .as_str()?
                        .trim_end_matches('/')
                        .to_string(),
                    c.get("objectIdentifier")?.as_str()?.to_string(),
                    c.get("versionIdentifier")?
                        .as_str()
                        .unwrap_or("")
                        .to_string(),
                ))
            };
            let best_id = code("code")
                .map(|(ns, oid, vid)| format!("{ns}/{oid}/{vid}"))
                .unwrap_or_default();
            // crabfinder's building id: street namespace | street id | number.
            let hn = addr.get("houseNumber").and_then(Value::as_str);
            let bgid = match (code("hasStreetName"), hn) {
                (Some((ns, oid, _)), Some(hn)) => format!("{ns}|{oid}|{}", hn.to_uppercase()),
                _ => String::new(),
            };
            a.features.push((bgid, best_id, "address".into()));
            a.points.push(point);
        }
        Ok(a)
    }
}

#[cfg(test)]
mod tests {
    use super::super::test_util::{q, with_fields};
    use super::*;

    /// Recorded 2026-10-16: "Rue de la Loi 16, 1000 Bruxelles".
    const LOI: &str = r#"{"resultsList":[{"objectType":"Address","result":[
      {"score":14,"address":{"code":{"namespace":"https://databrussels.be/id/address","objectIdentifier":"1","versionIdentifier":"1"},"houseNumber":"18"}},
      {"score":29,"address":{"code":{"namespace":"https://databrussels.be/id/address","objectIdentifier":"210225","versionIdentifier":"1"},"houseNumber":"16a",
        "position":{"pointGeometry":{"point":{"pos":[{"value":[149843.38900005203,170639.75900060777],"srsName":"http://www.opengis.net/def/crs/EPSG/0/31370"}]}}},
        "hasStreetName":{"namespace":"https://databrussels.be/id/streetname","objectIdentifier":"1679","versionIdentifier":"1"}}}]},
      {"objectType":"StreetName","result":[{"score":9,"streetname":{}}]}]}"#;

    #[test]
    fn reads_ids_and_reprojects_best_score_first() {
        let a = Brussels::new(DEFAULT_BASE).parse(LOI, 10).unwrap();
        assert_eq!(a.features.len(), 2, "street names carry no address");
        assert_eq!(
            a.features[0],
            (
                "https://databrussels.be/id/streetname|1679|16A".into(),
                "https://databrussels.be/id/address/210225/1".into(),
                "address".into()
            )
        );
        let (lat, lon) = a.points[0].unwrap();
        assert!((lat - 50.8466).abs() < 1e-3 && (lon - 4.3669).abs() < 1e-3);
        // No position, no street: kept, without point or building id.
        assert_eq!(a.points[1], None);
        assert_eq!(a.features[1].0, "");
        assert_eq!(Brussels::new("x").parse(LOI, 1).unwrap().features.len(), 1);
        assert!(Brussels::new("x").parse("[", 1).is_err());
        assert!(
            Brussels::new("x")
                .parse("{}", 1)
                .unwrap()
                .features
                .is_empty()
        );
    }

    #[test]
    fn structured_or_free_text_in_the_guessed_language_then_the_other() {
        let b = Brussels::new("http://b/");
        let free = q("a", "Wetstraat 16 Brussel", "brussels");
        let r = b.request(&free, 5);
        assert_eq!(r.url, "http://b/freeSearch/searchAnyObject");
        assert_eq!(r.params[0], ("searchLanguage", "nl".into()));
        assert_eq!(b.fallback(&free, 5).unwrap().params[0].1, "fr");
        let st = with_fields(
            q("b", "x", "brussels"),
            "Rue de la Loi",
            "16",
            "1000",
            "Bruxelles",
        );
        let r = b.request(&st, 5);
        assert_eq!(r.url, "http://b/searchAddress");
        assert!(r.params.contains(&("policeNumber", "16".into())));
        assert!(r.params.contains(&("postalCode", "1000".into())));
        assert!(r.params.contains(&("municipality", "Bruxelles".into())));
        assert_eq!(r.params[0].1, "fr");
        assert!(b.covers(&st));
        assert!(!b.covers(&q("c", "x", "flanders")));
    }

    #[test]
    fn language_guess() {
        for (s, l) in [
            ("Rue de la Loi 16", "fr"),
            ("Avenue Louise 54, 1050", "fr"),
            ("Louizalaan 54", "nl"),
            ("Grote Markt 1", "nl"),
            ("Chaussée de Waterloo, Waterloosesteenweg", "fr"),
            ("Bisschoppenhoflaan 1", "nl"),
            ("Square Ambiorix 1", "fr"),
            ("Atomium", "fr"),
        ] {
            assert_eq!(guess_language(&q("a", s, "brussels")), l, "{s}");
        }
    }
}
