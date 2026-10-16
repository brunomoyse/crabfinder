//! crabfinder itself, over HTTP (`/geocode?q=&limit=`): the same request
//! and parsing as the eval runner.

use super::{Answer, HttpAdapter, Request};
use crate::eval_set::EvalQuery;

pub struct Crabfinder {
    base: String,
}

impl Crabfinder {
    pub fn new(base: &str) -> Self {
        Self {
            base: base.trim_end_matches('/').to_string(),
        }
    }
}

impl HttpAdapter for Crabfinder {
    fn request(&self, q: &EvalQuery, limit: usize) -> Request {
        Request::new(format!("{}/geocode", self.base))
            .param("q", &q.query_string)
            .param("limit", limit.to_string())
    }

    fn parse(&self, body: &str, _limit: usize) -> Result<Answer, String> {
        crate::runner::parse_geocode_body(body)
    }
}

#[cfg(test)]
mod tests {
    use super::super::test_util::q;
    use super::*;

    #[test]
    fn asks_geocode_and_reads_ids_and_points() {
        let a = Crabfinder::new("http://h:1/");
        let r = a.request(&q("a", "Rue de la Loi 16", "brussels"), 5);
        assert_eq!(r.url, "http://h:1/geocode");
        assert_eq!(
            r.params,
            [("q", "Rue de la Loi 16".to_string()), ("limit", "5".into())]
        );
        let body = r#"{"match_type":"strict","raw_hits":3,"features":[{"building_group_id":"ns|1|16",
            "match_type":"strict","properties":{"id":{"namespace":"https://databrussels.be/id/address/",
            "object_identifier":"210225","version_identifier":"1"}},
            "geometry":{"type":"Point","coordinates":[4.3669,50.8466]}}]}"#;
        let ans = a.parse(body, 5).unwrap();
        assert_eq!(ans.raw_hits, 3);
        assert_eq!(
            ans.features[0],
            (
                "ns|1|16".into(),
                "https://databrussels.be/id/address/210225/1".into(),
                "strict".into()
            )
        );
        assert_eq!(ans.points, [Some((50.8466, 4.3669))]);
        assert!(a.parse("nope", 5).is_err());
    }
}
