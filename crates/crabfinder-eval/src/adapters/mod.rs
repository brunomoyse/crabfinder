//! Geocoder adapters for the benchmark (v3 Phase 11.1).
//!
//! The metrics only read `QueryResult`, so any geocoder that can turn an
//! `EvalQuery` into ranked points can be scored next to crabfinder: on
//! distance to the reference point always, on BeSt ids when it returns
//! them. Each adapter is a request builder + response parser
//! (`HttpAdapter`, pure and unit-tested on recorded answers) run by
//! `HttpGeocoder`, which adds the courtesy and reproducibility parts:
//! a per-geocoder rate limit, retries on transport errors, and an
//! on-disk response cache so reruns cost the remote service nothing.
//! PhacochR is an R batch library, not a service: `phacochr` runs it as a
//! file adapter.
//!
//! What a competitor returns is only ever compared, never served: BeSt
//! stays crabfinder's only source.

pub mod bepelias;
pub mod brussels;
pub mod cache;
pub mod crabfinder;
pub mod phacochr;
pub mod vlaanderen;
pub mod wallonie;

use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use futures::future::BoxFuture;
use futures::stream::{self, StreamExt};
use reqwest::Client;
use tracing::{info, warn};

use crabfinder_core::LambertCoord;
use crabfinder_indexer::reproject::Reprojector;

use crate::eval_set::{EvalQuery, EvalQuerySource};
use crate::runner::QueryResult;
use cache::{CachedResponse, ResponseCache};

/// What a geocoder answered for one query, best first.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Answer {
    /// `(building_group_id, best_id, match_type)` per result; the ids are
    /// empty when the geocoder doesn't return BeSt ids.
    pub features: Vec<(String, String, String)>,
    /// `(lat, lon)` per result, aligned with `features`.
    pub points: Vec<Option<(f64, f64)>>,
    pub top_match_type: String,
    pub raw_hits: usize,
}

impl Answer {
    /// One result with no BeSt ids.
    pub fn push_point(&mut self, point: Option<(f64, f64)>, match_type: &str) {
        self.features
            .push((String::new(), String::new(), match_type.to_string()));
        self.points.push(point);
    }

    fn finish(mut self) -> Self {
        if self.top_match_type.is_empty() {
            self.top_match_type = self
                .features
                .first()
                .map(|f| f.2.clone())
                .unwrap_or_default();
        }
        if self.raw_hits == 0 {
            self.raw_hits = self.features.len();
        }
        self
    }

    pub fn into_result(self, query: EvalQuery, latency_ms: f64) -> QueryResult {
        let a = self.finish();
        QueryResult {
            query,
            features: a.features,
            points: a.points,
            top_match_type: a.top_match_type,
            raw_hits: a.raw_hits,
            latency_ms,
            error: None,
        }
    }
}

/// A failed query: no answer, the reason in `error`.
pub fn failed(query: EvalQuery, latency_ms: f64, error: String) -> QueryResult {
    QueryResult {
        query,
        features: Vec::new(),
        points: Vec::new(),
        top_match_type: String::new(),
        raw_hits: 0,
        latency_ms,
        error: Some(error),
    }
}

/// A geocoder under benchmark.
pub trait Geocoder: Send + Sync {
    /// Label in the report (`crabfinder-osm`, `brussels`).
    fn name(&self) -> &str;
    /// Whether the geocoder is meant to answer `q` (a regional service is
    /// only scored on its region).
    fn covers(&self, _q: &EvalQuery) -> bool {
        true
    }
    /// Answer every query (any order).
    fn run(
        &self,
        queries: Vec<EvalQuery>,
        limit: usize,
        parallel: usize,
    ) -> BoxFuture<'_, Result<Vec<QueryResult>>>;
}

/// A GET request: URL and query parameters.
#[derive(Debug, Clone, PartialEq)]
pub struct Request {
    pub url: String,
    pub params: Vec<(&'static str, String)>,
}

impl Request {
    pub fn new(url: impl Into<String>) -> Self {
        Self {
            url: url.into(),
            params: Vec::new(),
        }
    }

    pub fn param(mut self, k: &'static str, v: impl Into<String>) -> Self {
        self.params.push((k, v.into()));
        self
    }

    /// Stable cache key.
    pub fn key(&self) -> String {
        let mut k = self.url.clone();
        for (n, v) in &self.params {
            k.push_str(&format!("&{n}={v}"));
        }
        k
    }
}

/// One HTTP geocoder: how to ask, how to read the answer.
pub trait HttpAdapter: Send + Sync {
    fn covers(&self, _q: &EvalQuery) -> bool {
        true
    }
    fn request(&self, q: &EvalQuery, limit: usize) -> Request;
    /// A second request to try when the first answered nothing (the
    /// Brussels service only searches the language it is told).
    fn fallback(&self, _q: &EvalQuery, _limit: usize) -> Option<Request> {
        None
    }
    /// Parse a 2xx body; the `Err` text is recorded on the query.
    fn parse(&self, body: &str, limit: usize) -> Result<Answer, String>;
}

/// Runs an `HttpAdapter` with a rate limit, retries and a cache.
pub struct HttpGeocoder<A> {
    name: String,
    adapter: A,
    client: Client,
    cache: Option<ResponseCache>,
    limiter: Option<RateLimiter>,
    retries: u32,
}

impl<A: HttpAdapter> HttpGeocoder<A> {
    pub fn new(name: &str, adapter: A, timeout: Duration) -> Result<Self> {
        Ok(Self {
            name: name.to_string(),
            adapter,
            client: Client::builder()
                .timeout(timeout)
                .user_agent(concat!(
                    "crabfinder-eval/",
                    env!("CARGO_PKG_VERSION"),
                    " (benchmark)"
                ))
                .build()
                .context("constructing reqwest client")?,
            cache: None,
            limiter: None,
            retries: 3,
        })
    }

    /// Cache responses under `dir/<name>/`.
    pub fn with_cache(mut self, dir: &Path) -> Self {
        self.cache = Some(ResponseCache::new(dir.join(&self.name)));
        self
    }

    /// At most `per_second` requests per second (cache hits are free).
    pub fn with_rate(mut self, per_second: f64) -> Self {
        self.limiter = (per_second > 0.0).then(|| RateLimiter::per_second(per_second));
        self
    }

    pub fn with_retries(mut self, retries: u32) -> Self {
        self.retries = retries;
        self
    }

    /// Status, body and latency for `req`, from the cache or the network.
    async fn fetch(&self, req: &Request) -> Result<CachedResponse, String> {
        let key = req.key();
        if let Some(hit) = self.cache.as_ref().and_then(|c| c.get(&key)) {
            return Ok(hit);
        }
        let mut attempt = 0;
        loop {
            if let Some(l) = &self.limiter {
                l.acquire().await;
            }
            let t0 = Instant::now();
            let sent = self.client.get(&req.url).query(&req.params).send().await;
            let result = match sent {
                Ok(resp) => {
                    let status = resp.status().as_u16();
                    resp.text()
                        .await
                        .map(|body| CachedResponse {
                            status,
                            body,
                            latency_ms: t0.elapsed().as_secs_f64() * 1000.0,
                        })
                        .map_err(|e| format!("transport: {e}"))
                }
                Err(e) => Err(format!("transport: {e}")),
            };
            match result {
                // 5xx are retried like transport errors, and never cached.
                Ok(r) if r.status < 500 => {
                    if let Some(c) = &self.cache {
                        c.put(&key, &r);
                    }
                    return Ok(r);
                }
                Ok(r) if attempt >= self.retries => return Ok(r),
                Err(e) if attempt >= self.retries => return Err(e),
                _ => {}
            }
            attempt += 1;
            tokio::time::sleep(Duration::from_millis(500 << attempt)).await;
        }
    }

    /// Ask and parse one request; `Err` = (latency, error text).
    async fn ask(&self, req: &Request, limit: usize) -> Result<(Answer, f64), (f64, String)> {
        match self.fetch(req).await {
            Err(e) => Err((0.0, e)),
            Ok(r) if !(200..300).contains(&r.status) => {
                Err((r.latency_ms, format!("http {}", r.status)))
            }
            Ok(r) => match self.adapter.parse(&r.body, limit) {
                Ok(a) => Ok((a, r.latency_ms)),
                Err(e) => Err((r.latency_ms, format!("json: {e}"))),
            },
        }
    }

    async fn one(&self, q: EvalQuery, limit: usize) -> QueryResult {
        let first = self.ask(&self.adapter.request(&q, limit), limit).await;
        let first = match first {
            Ok((a, ms)) if a.features.is_empty() => match self.adapter.fallback(&q, limit) {
                Some(req) => self
                    .ask(&req, limit)
                    .await
                    .map(|(a, ms2)| (a, ms + ms2))
                    .map_err(|(ms2, e)| (ms + ms2, e)),
                None => Ok((a, ms)),
            },
            other => other,
        };
        match first {
            Ok((a, ms)) => a.into_result(q, ms),
            Err((ms, e)) => failed(q, ms, e),
        }
    }
}

impl<A: HttpAdapter> Geocoder for HttpGeocoder<A> {
    fn name(&self) -> &str {
        &self.name
    }

    fn covers(&self, q: &EvalQuery) -> bool {
        self.adapter.covers(q)
    }

    fn run(
        &self,
        queries: Vec<EvalQuery>,
        limit: usize,
        parallel: usize,
    ) -> BoxFuture<'_, Result<Vec<QueryResult>>> {
        Box::pin(async move {
            let total = queries.len();
            info!(geocoder = %self.name, total, "benchmark: querying");
            let results: Vec<QueryResult> = stream::iter(queries)
                .map(|q| self.one(q, limit))
                .buffer_unordered(parallel.max(1))
                .collect()
                .await;
            let errors = results.iter().filter(|r| r.error.is_some()).count();
            if total > 0 && errors == total {
                bail!(
                    "{}: every query failed (first: {})",
                    self.name,
                    results[0].error.as_deref().unwrap_or("?")
                );
            }
            if errors > 0 {
                warn!(geocoder = %self.name, errors, "benchmark: some queries failed");
            }
            Ok(results)
        })
    }
}

/// Spaces requests at least `interval` apart across all tasks.
pub struct RateLimiter {
    interval: Duration,
    next: tokio::sync::Mutex<tokio::time::Instant>,
}

impl RateLimiter {
    pub fn per_second(n: f64) -> Self {
        Self {
            interval: Duration::from_secs_f64(1.0 / n),
            next: tokio::sync::Mutex::new(tokio::time::Instant::now()),
        }
    }

    pub async fn acquire(&self) {
        let mut next = self.next.lock().await;
        let now = tokio::time::Instant::now();
        if *next > now {
            tokio::time::sleep_until(*next).await;
        }
        *next = (*next).max(now) + self.interval;
    }
}

/// Lambert 72 metres → `(lat, lon)`.
pub fn lambert72(x: f64, y: f64) -> Option<(f64, f64)> {
    static R: OnceLock<Option<Reprojector>> = OnceLock::new();
    let r = R.get_or_init(|| Reprojector::new().ok()).as_ref()?;
    let w = r
        .lambert72_to_wgs84(LambertCoord {
            easting: x,
            northing: y,
        })
        .ok()?;
    Some((w.lat, w.lon))
}

/// Region of a query: its set's, else read off its expected BeSt ids'
/// namespaces (adversarial entries carry no region).
pub fn region_of(q: &EvalQuery) -> Option<&'static str> {
    if let Some(r) = q.source.region() {
        return Some(r);
    }
    let id = q
        .expected_best_id_set
        .first()
        .or(q.expected_bgid_set.first())?;
    if id.contains("databrussels") {
        Some("brussels")
    } else if id.contains("vlaanderen") {
        Some("flanders")
    } else if id.contains("wallonie") {
        Some("wallonia")
    } else {
        None
    }
}

/// The address as fields when the set has them (street and number at
/// least), for structured APIs.
pub fn structured(q: &EvalQuery) -> Option<&crate::eval_set::StructuredAddress> {
    q.structured
        .as_ref()
        .filter(|s| s.street.is_some() && s.house_number.is_some())
}

/// `true` for a source whose queries are real-world.
pub fn is_realworld(q: &EvalQuery) -> bool {
    matches!(q.source, EvalQuerySource::RealWorld { .. })
}

/// Default requests per second for a geocoder kind: unlimited for the
/// local ones, polite for public services.
pub fn default_rate(kind: &str) -> f64 {
    match kind {
        "crabfinder" | "bepelias" | "phacochr" => 0.0,
        _ => 2.0,
    }
}

/// `--geocoder label[:kind][=target]`: `target` is a base URL (HTTP
/// kinds) or a command line (`phacochr`).
#[derive(Debug, Clone, PartialEq)]
pub struct GeocoderSpec {
    pub label: String,
    pub kind: String,
    pub target: Option<String>,
}

impl GeocoderSpec {
    pub fn parse(s: &str) -> Result<Self> {
        let (head, target) = match s.split_once('=') {
            Some((h, t)) => (h, Some(t.to_string())),
            None => (s, None),
        };
        let (label, kind) = match head.split_once(':') {
            Some((l, k)) => (l, k),
            None => (head, head),
        };
        if label.is_empty() || kind.is_empty() {
            bail!("bad --geocoder {s:?}: expected label[:kind][=target]");
        }
        Ok(Self {
            label: label.to_string(),
            kind: kind.to_string(),
            target,
        })
    }
}

/// Options shared by every geocoder of a benchmark.
#[derive(Debug, Clone)]
pub struct BuildOptions {
    pub timeout: Duration,
    /// Response cache root (competitors only: crabfinder is the system
    /// under test and is always asked afresh).
    pub cache_dir: Option<PathBuf>,
    /// Requests per second; `None` = `default_rate(kind)`.
    pub rate: Option<f64>,
    /// Scratch directory for file adapters.
    pub work_dir: PathBuf,
}

/// Instantiate the adapter `spec` names.
pub fn build(spec: &GeocoderSpec, opts: &BuildOptions) -> Result<Arc<dyn Geocoder>> {
    let target = spec.target.as_deref();
    let rate = opts.rate.unwrap_or_else(|| default_rate(&spec.kind));
    fn http<A: HttpAdapter + 'static>(
        spec: &GeocoderSpec,
        opts: &BuildOptions,
        rate: f64,
        cached: bool,
        a: A,
    ) -> Result<Arc<dyn Geocoder>> {
        let mut g = HttpGeocoder::new(&spec.label, a, opts.timeout)?.with_rate(rate);
        if let (true, Some(dir)) = (cached, &opts.cache_dir) {
            g = g.with_cache(dir);
        }
        Ok(Arc::new(g))
    }
    match spec.kind.as_str() {
        "crabfinder" => http(
            spec,
            opts,
            rate,
            false,
            crabfinder::Crabfinder::new(target.unwrap_or("http://127.0.0.1:8080")),
        ),
        "bepelias" => http(
            spec,
            opts,
            rate,
            true,
            bepelias::BePelias::new(target.unwrap_or(bepelias::DEFAULT_BASE)),
        ),
        "brussels" => http(
            spec,
            opts,
            rate,
            true,
            brussels::Brussels::new(target.unwrap_or(brussels::DEFAULT_BASE)),
        ),
        "vlaanderen" => http(
            spec,
            opts,
            rate,
            true,
            vlaanderen::Vlaanderen::new(target.unwrap_or(vlaanderen::DEFAULT_BASE)),
        ),
        "wallonie" => http(
            spec,
            opts,
            rate,
            true,
            wallonie::Wallonie::new(target.unwrap_or(wallonie::DEFAULT_BASE)),
        ),
        "phacochr" => Ok(Arc::new(phacochr::Phacochr::new(
            &spec.label,
            target.unwrap_or(phacochr::DEFAULT_COMMAND),
            opts.work_dir.join(&spec.label),
            opts.cache_dir.as_ref().map(|d| d.join(&spec.label)),
        )?)),
        "ngi" => bail!(
            "the NGI geocoder has no public endpoint yet (search.geo.be does not resolve, \
             2026-10-16); see docs/eval.md"
        ),
        k => bail!(
            "unknown geocoder kind {k:?}; expected crabfinder, bepelias, brussels, \
             vlaanderen, wallonie or phacochr"
        ),
    }
}

#[cfg(test)]
pub(crate) mod test_util {
    use crate::eval_set::*;

    pub fn q(id: &str, query: &str, region: &str) -> EvalQuery {
        EvalQuery {
            id: id.into(),
            query_string: query.into(),
            expected_bgid_set: Vec::new(),
            expected_best_id_set: Vec::new(),
            expected_to_fail: false,
            match_level: MatchLevel::Locality,
            expected_point: Some(((50.0, 4.0), 100.0)),
            reference_point: Some((50.0, 4.0)),
            structured: None,
            source: EvalQuerySource::RealWorld {
                source: "s".into(),
                region: region.into(),
            },
        }
    }

    pub fn with_fields(
        mut q: EvalQuery,
        street: &str,
        hn: &str,
        pc: &str,
        city: &str,
    ) -> EvalQuery {
        q.structured = Some(StructuredAddress {
            street: Some(street.into()),
            house_number: Some(hn.into()),
            postal_code: Some(pc.into()),
            city: Some(city.into()),
        });
        q
    }
}

#[cfg(test)]
mod tests {
    use super::test_util::*;
    use super::*;
    use crate::eval_set::{AdversarialCategory, StructuredAddress};

    #[test]
    fn specs_parse_label_kind_and_target() {
        assert_eq!(
            GeocoderSpec::parse("crabfinder-osm:crabfinder=http://h:1").unwrap(),
            GeocoderSpec {
                label: "crabfinder-osm".into(),
                kind: "crabfinder".into(),
                target: Some("http://h:1".into())
            }
        );
        let b = GeocoderSpec::parse("brussels").unwrap();
        assert_eq!(
            (b.label.as_str(), b.kind.as_str(), b.target),
            ("brussels", "brussels", None)
        );
        // A command line may itself contain '=' and ':'.
        let p = GeocoderSpec::parse("phacochr=Rscript x.R --a=b:c").unwrap();
        assert_eq!(p.target.as_deref(), Some("Rscript x.R --a=b:c"));
        assert!(GeocoderSpec::parse(":x").is_err());
        assert!(GeocoderSpec::parse("=x").is_err());
    }

    fn opts() -> BuildOptions {
        BuildOptions {
            timeout: Duration::from_secs(1),
            cache_dir: None,
            rate: None,
            work_dir: std::env::temp_dir(),
        }
    }

    #[test]
    fn build_knows_every_kind_and_explains_ngi() {
        for (spec, name) in [
            ("crabfinder", "crabfinder"),
            ("x:bepelias", "x"),
            ("brussels", "brussels"),
            ("vlaanderen", "vlaanderen"),
            ("wallonie", "wallonie"),
            ("phacochr=true", "phacochr"),
        ] {
            let g = build(&GeocoderSpec::parse(spec).unwrap(), &opts()).unwrap();
            assert_eq!(g.name(), name);
        }
        let err = build(&GeocoderSpec::parse("ngi").unwrap(), &opts())
            .err()
            .unwrap();
        assert!(err.to_string().contains("no public endpoint"), "{err}");
        assert!(build(&GeocoderSpec::parse("nope").unwrap(), &opts()).is_err());
        assert_eq!(default_rate("crabfinder"), 0.0);
        assert_eq!(default_rate("brussels"), 2.0);
    }

    #[test]
    fn regions_come_from_the_set_or_the_ids() {
        assert_eq!(region_of(&q("a", "x", "flanders")), Some("flanders"));
        assert_eq!(region_of(&q("a", "x", "BE-WAL")), Some("wallonia"));
        assert_eq!(region_of(&q("a", "x", "mars")), None);
        let mut adv = q("a", "x", "x");
        adv.source = EvalQuerySource::Adversarial {
            category: AdversarialCategory::Hamlet,
            subcategory: "s".into(),
        };
        assert_eq!(region_of(&adv), None);
        for (id, r) in [
            ("https://databrussels.be/id/streetname|1|1", "brussels"),
            ("https://data.vlaanderen.be/id/straatnaam/|1|1", "flanders"),
            ("geodata.wallonie.be/id/Streetname|1|1", "wallonia"),
        ] {
            adv.expected_bgid_set = vec![id.into()];
            assert_eq!(region_of(&adv), Some(r));
        }
        adv.expected_bgid_set = vec!["other".into()];
        assert_eq!(region_of(&adv), None);
        assert!(!is_realworld(&adv));
        assert!(is_realworld(&q("a", "x", "x")));
    }

    #[test]
    fn structured_needs_street_and_number() {
        let full = with_fields(q("a", "x", "x"), "Rue", "1", "1000", "B");
        assert!(structured(&full).is_some());
        let mut part = q("a", "x", "x");
        part.structured = Some(StructuredAddress {
            street: Some("Rue".into()),
            ..Default::default()
        });
        assert!(structured(&part).is_none());
        assert!(structured(&q("a", "x", "x")).is_none());
    }

    #[test]
    fn lambert72_reprojects_brussels() {
        // Rue de la Loi 16 (BeSt): ~(50.8466, 4.3669).
        let (lat, lon) = lambert72(149843.389, 170639.759).unwrap();
        assert!(
            (lat - 50.8466).abs() < 1e-3 && (lon - 4.3669).abs() < 1e-3,
            "{lat} {lon}"
        );
    }

    #[test]
    fn answers_fill_their_top_match_type_and_hits() {
        let mut a = Answer::default();
        a.push_point(Some((1.0, 2.0)), "street");
        let r = a.into_result(q("a", "x", "x"), 3.0);
        assert_eq!((r.top_match_type.as_str(), r.raw_hits), ("street", 1));
        assert_eq!(r.points, [Some((1.0, 2.0))]);
        let f = failed(q("a", "x", "x"), 1.0, "boom".into());
        assert_eq!(f.error.as_deref(), Some("boom"));
        assert_eq!(Request::new("u").param("a", "1").key(), "u&a=1");
    }

    #[tokio::test]
    async fn the_rate_limiter_spaces_requests() {
        let l = RateLimiter::per_second(50.0);
        let t0 = Instant::now();
        for _ in 0..4 {
            l.acquire().await;
        }
        // 3 intervals of 20 ms after the first, free, one.
        assert!(
            t0.elapsed() >= Duration::from_millis(55),
            "{:?}",
            t0.elapsed()
        );
    }
}
