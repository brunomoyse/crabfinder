//! Basic protection (v3 Phase 11.7), all off by default: API keys
//! (`--api-keys`), per-key and per-IP rate limits (`--rate-limit-per-ip`).
//!
//! With a keys file, every route but `/health` (and `/metrics`, unless
//! `--metrics-require-key`) needs a key, sent as `Authorization: Bearer
//! <key>` or `X-API-Key: <key>`: none or an unknown one is a 401, a key
//! not allowed on the route (`batch`, `jobs`) a 403. Over its rate a key,
//! or a client address, gets a 429 with `Retry-After`. The key's name
//! goes to the handler (`Caller`: batch jobs belong to the key that
//! submitted them) and to the request metrics.
//!
//! Keys file (YAML):
//!
//! ```yaml
//! keys:
//!   - name: frontend
//!     key: "2f0c9a…"          # at least 16 characters
//!     rate_per_minute: 600    # optional; none = no limit
//!   - name: etl
//!     key: "b81e44…"
//!     batch: true             # may use POST /batch/geocode
//!     jobs: true              # may use /batch/jobs
//! ```

use std::collections::HashMap;
use std::hash::Hash;
use std::net::{IpAddr, SocketAddr};
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use axum::{
    extract::{ConnectInfo, Request, State, connect_info::MockConnectInfo},
    http::{HeaderMap, header::AUTHORIZATION},
    middleware::Next,
    response::{IntoResponse, Response},
};
use serde::Deserialize;

use super::ServiceError;
use crate::telemetry;

/// Shortest key accepted.
const MIN_KEY_LEN: usize = 16;
/// Buckets kept before idle (full) ones are dropped.
const PRUNE_AT: usize = 10_000;

/// One API key.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ApiKey {
    /// Shown in logs and metrics; owns the batch jobs submitted with it.
    pub name: String,
    pub key: String,
    /// Requests per minute (bursts up to as many); none = no limit.
    #[serde(default)]
    pub rate_per_minute: Option<u32>,
    /// May use `POST /batch/geocode`.
    #[serde(default)]
    pub batch: bool,
    /// May use `/batch/jobs`.
    #[serde(default)]
    pub jobs: bool,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct KeysFile {
    keys: Vec<ApiKey>,
}

/// Read and check a keys file: names and keys unique, keys long enough.
pub fn load_keys(path: &Path) -> Result<Vec<ApiKey>> {
    let text = std::fs::read_to_string(path).with_context(|| format!("reading {path:?}"))?;
    let file: KeysFile =
        serde_saphyr::from_str(&text).with_context(|| format!("parsing {path:?}"))?;
    check_keys(&file.keys)?;
    Ok(file.keys)
}

fn check_keys(keys: &[ApiKey]) -> Result<()> {
    if keys.is_empty() {
        bail!("the keys file lists no key");
    }
    for (i, k) in keys.iter().enumerate() {
        if k.name.trim().is_empty() {
            bail!("key #{} has no name", i + 1);
        }
        if k.key.chars().count() < MIN_KEY_LEN {
            bail!("key '{}' is shorter than {MIN_KEY_LEN} characters", k.name);
        }
        if keys[..i].iter().any(|o| o.name == k.name) {
            bail!("two keys are named '{}'", k.name);
        }
        if keys[..i].iter().any(|o| o.key == k.key) {
            bail!("keys '{}' and another one are the same", k.name);
        }
    }
    Ok(())
}

/// Who made the request: the API key's name. A request extension (for
/// handlers) and a response extension (for `telemetry::track`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Caller(pub String);

/// What a route asks of the caller.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Need {
    /// Nothing, not even a rate limit.
    Open,
    /// Any valid key.
    Key,
    Batch,
    Jobs,
}

/// The settings and the rate-limit state.
pub struct Auth {
    keys: Vec<ApiKey>,
    per_ip: Option<u32>,
    metrics_require_key: bool,
    by_key: Limiter<String>,
    by_ip: Limiter<IpAddr>,
}

impl Auth {
    /// `None` when there's nothing to enforce (no keys, no per-IP limit):
    /// the router then skips the middleware altogether.
    pub fn new(
        keys: Vec<ApiKey>,
        per_ip: Option<u32>,
        metrics_require_key: bool,
    ) -> Option<Arc<Self>> {
        (!keys.is_empty() || per_ip.is_some()).then(|| {
            Arc::new(Self {
                keys,
                per_ip,
                metrics_require_key,
                by_key: Limiter::default(),
                by_ip: Limiter::default(),
            })
        })
    }

    fn need(&self, route: &str) -> Need {
        match route {
            "/health" => Need::Open,
            "/metrics" if !self.metrics_require_key => Need::Open,
            "/batch/geocode" => Need::Batch,
            r if r.starts_with("/batch/jobs") => Need::Jobs,
            _ => Need::Key,
        }
    }

    /// The key presented, compared with every key in constant time.
    fn find(&self, presented: &str) -> Option<&ApiKey> {
        let mut found = None;
        for k in &self.keys {
            if ct_eq(k.key.as_bytes(), presented.as_bytes()) {
                found = Some(k);
            }
        }
        found
    }

    /// The caller of a request to `route`, or why it is refused.
    fn admit(
        &self,
        route: &str,
        headers: &HeaderMap,
        ip: Option<IpAddr>,
        now: Instant,
    ) -> Result<Option<&ApiKey>, Refusal> {
        let need = self.need(route);
        if need == Need::Open {
            return Ok(None);
        }
        let key = if self.keys.is_empty() {
            None
        } else {
            let presented = presented_key(headers).ok_or(Refusal::NoKey)?;
            let key = self.find(presented).ok_or(Refusal::BadKey)?;
            let allowed = match need {
                Need::Batch => key.batch,
                Need::Jobs => key.jobs,
                Need::Key | Need::Open => true,
            };
            if !allowed {
                return Err(Refusal::Forbidden(key.name.clone()));
            }
            Some(key)
        };
        if let Some(k) = key
            && let Some(rate) = k.rate_per_minute
        {
            self.by_key
                .check(&k.name, rate, now)
                .map_err(|wait| Refusal::Limited(rate, wait))?;
        }
        if let (Some(rate), Some(ip)) = (self.per_ip, ip) {
            self.by_ip
                .check(&ip, rate, now)
                .map_err(|wait| Refusal::Limited(rate, wait))?;
        }
        Ok(key)
    }
}

#[derive(Debug)]
enum Refusal {
    NoKey,
    BadKey,
    /// The key's name.
    Forbidden(String),
    /// The rate (per minute), and how long until the next request fits.
    Limited(u32, Duration),
}

impl Refusal {
    fn reason(&self) -> &'static str {
        match self {
            Self::NoKey | Self::BadKey => "unauthorized",
            Self::Forbidden(_) => "forbidden",
            Self::Limited(..) => "rate_limited",
        }
    }

    fn into_error(self, route: &str) -> ServiceError {
        match self {
            Self::NoKey => ServiceError::Unauthorized(
                "missing API key (Authorization: Bearer <key> or X-API-Key: <key>)".to_string(),
            ),
            Self::BadKey => ServiceError::Unauthorized("invalid API key".to_string()),
            Self::Forbidden(name) => {
                ServiceError::Forbidden(format!("key '{name}' may not use {route}"))
            }
            Self::Limited(rate, wait) => ServiceError::RateLimited {
                message: format!("rate limit exceeded: {rate} requests per minute"),
                retry_after_secs: wait.as_secs_f64().ceil().max(1.0) as u64,
            },
        }
    }
}

/// The key in `Authorization: Bearer …` or `X-API-Key`.
fn presented_key(headers: &HeaderMap) -> Option<&str> {
    let bearer = headers
        .get(AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| {
            v.split_once(' ')
                .filter(|(scheme, _)| scheme.eq_ignore_ascii_case("bearer"))
                .map(|(_, k)| k.trim())
        });
    bearer
        .or_else(|| headers.get("x-api-key").and_then(|v| v.to_str().ok()))
        .filter(|k| !k.is_empty())
}

/// Equal bytes, in a time that depends on the lengths only.
fn ct_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// Route middleware: admit or refuse the request (see the module doc).
pub async fn check(State(auth): State<Arc<Auth>>, mut req: Request, next: Next) -> Response {
    let route = telemetry::route_of(&req);
    let ext = req.extensions();
    let ip = ext
        .get::<ConnectInfo<SocketAddr>>()
        .map(|c| c.0.ip())
        .or_else(|| ext.get::<MockConnectInfo<SocketAddr>>().map(|m| m.0.ip()));
    let caller = match auth.admit(&route, req.headers(), ip, Instant::now()) {
        Ok(key) => key.map(|k| Caller(k.name.clone())),
        Err(refusal) => {
            telemetry::record_rejected(route.clone(), refusal.reason());
            let name = match &refusal {
                Refusal::Forbidden(n) => Some(n.clone()),
                _ => None,
            };
            let mut resp = refusal.into_error(&route).into_response();
            if let Some(n) = name {
                resp.extensions_mut().insert(Caller(n));
            }
            return resp;
        }
    };
    if let Some(c) = &caller {
        req.extensions_mut().insert(c.clone());
    }
    let mut resp = next.run(req).await;
    if let Some(c) = caller {
        resp.extensions_mut().insert(c);
    }
    resp
}

/// Token buckets by key: `per_minute` requests a minute, in bursts of up
/// to as many.
pub struct Limiter<K> {
    buckets: Mutex<HashMap<K, Bucket>>,
}

impl<K> Default for Limiter<K> {
    fn default() -> Self {
        Self {
            buckets: Mutex::new(HashMap::new()),
        }
    }
}

struct Bucket {
    tokens: f64,
    at: Instant,
}

impl<K: Hash + Eq + Clone> Limiter<K> {
    /// Take one token from `key`'s bucket at `now`, or say how long until
    /// one is there.
    pub fn check(&self, key: &K, per_minute: u32, now: Instant) -> Result<(), Duration> {
        let cap = f64::from(per_minute.max(1));
        let rate = cap / 60.0;
        let refill = |b: &Bucket| {
            (b.tokens + rate * now.saturating_duration_since(b.at).as_secs_f64()).min(cap)
        };
        let mut buckets = self.buckets.lock().unwrap_or_else(|e| e.into_inner());
        if buckets.len() >= PRUNE_AT {
            buckets.retain(|_, b| refill(b) < cap);
        }
        let b = buckets.entry(key.clone()).or_insert(Bucket {
            tokens: cap,
            at: now,
        });
        b.tokens = refill(b);
        b.at = now;
        if b.tokens >= 1.0 {
            b.tokens -= 1.0;
            Ok(())
        } else {
            Err(Duration::from_secs_f64((1.0 - b.tokens) / rate))
        }
    }

    pub fn len(&self) -> usize {
        self.buckets.lock().map_or(0, |b| b.len())
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(name: &str, key: &str) -> ApiKey {
        ApiKey {
            name: name.into(),
            key: key.into(),
            rate_per_minute: None,
            batch: false,
            jobs: false,
        }
    }

    #[test]
    fn buckets_refill_over_the_minute() {
        let l = Limiter::default();
        let t0 = Instant::now();
        for _ in 0..3 {
            assert!(l.check(&"a", 3, t0).is_ok());
        }
        let wait = l.check(&"a", 3, t0).unwrap_err();
        assert!((19.9..=20.0).contains(&wait.as_secs_f64()), "{wait:?}");
        // Another key has its own bucket.
        assert!(l.check(&"b", 3, t0).is_ok());
        // 20 s later one token is back, and only one.
        let t1 = t0 + Duration::from_secs(20);
        assert!(l.check(&"a", 3, t1).is_ok());
        assert!(l.check(&"a", 3, t1).is_err());
        // Never more than the burst.
        let t2 = t0 + Duration::from_secs(3600);
        for _ in 0..3 {
            assert!(l.check(&"a", 3, t2).is_ok());
        }
        assert!(l.check(&"a", 3, t2).is_err());
        assert!(
            l.check(&"z", 0, t2).is_ok(),
            "a 0 rate still lets one through"
        );
    }

    #[test]
    fn idle_buckets_are_pruned() {
        let l = Limiter::default();
        let t0 = Instant::now();
        for i in 0..PRUNE_AT {
            l.check(&i, 60, t0).unwrap();
        }
        assert_eq!(l.len(), PRUNE_AT);
        // A minute later every bucket is full again: all dropped but the new one.
        l.check(&usize::MAX, 60, t0 + Duration::from_secs(60))
            .unwrap();
        assert_eq!(l.len(), 1);
        assert!(!l.is_empty());
    }

    #[test]
    fn keys_files_are_checked() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("keys.yaml");
        std::fs::write(
            &p,
            "keys:\n  - name: web\n    key: 0123456789abcdef\n    rate_per_minute: 60\n  - name: etl\n    key: fedcba9876543210\n    batch: true\n    jobs: true\n",
        )
        .unwrap();
        let keys = load_keys(&p).unwrap();
        assert_eq!(keys.len(), 2);
        assert_eq!(keys[0].rate_per_minute, Some(60));
        assert!(!keys[0].batch && keys[1].batch && keys[1].jobs);

        let err = |keys: Vec<ApiKey>| check_keys(&keys).unwrap_err().to_string();
        assert_eq!(err(vec![]), "the keys file lists no key");
        assert_eq!(
            err(vec![key(" ", "0123456789abcdef")]),
            "key #1 has no name"
        );
        assert_eq!(
            err(vec![key("web", "short")]),
            "key 'web' is shorter than 16 characters"
        );
        assert_eq!(
            err(vec![
                key("a", "0123456789abcdef"),
                key("a", "fedcba9876543210")
            ]),
            "two keys are named 'a'"
        );
        assert_eq!(
            err(vec![
                key("a", "0123456789abcdef"),
                key("b", "0123456789abcdef")
            ]),
            "keys 'b' and another one are the same"
        );
        std::fs::write(
            &p,
            "keys:\n  - name: x\n    key: 0123456789abcdef\n    color: red\n",
        )
        .unwrap();
        assert!(load_keys(&p).is_err());
        assert!(load_keys(&dir.path().join("missing.yaml")).is_err());
    }

    #[test]
    fn keys_come_from_either_header() {
        let mut h = HeaderMap::new();
        assert_eq!(presented_key(&h), None);
        h.insert("x-api-key", "abc".parse().unwrap());
        assert_eq!(presented_key(&h), Some("abc"));
        h.insert(AUTHORIZATION, "Bearer  xyz ".parse().unwrap());
        assert_eq!(presented_key(&h), Some("xyz"));
        h.insert(AUTHORIZATION, "Basic xyz".parse().unwrap());
        assert_eq!(presented_key(&h), Some("abc"));
        assert!(ct_eq(b"abc", b"abc"));
        assert!(!ct_eq(b"abc", b"abd"));
        assert!(!ct_eq(b"abc", b"abcd"));
    }

    #[test]
    fn nothing_to_enforce_is_no_auth() {
        assert!(Auth::new(vec![], None, false).is_none());
        assert!(Auth::new(vec![], Some(10), false).is_some());
    }
}
