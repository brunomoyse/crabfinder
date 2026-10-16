//! On-disk response cache (v3 Phase 11.1): one JSON file per request
//! under `eval/bench-cache/<geocoder>/` (gitignored), named by a stable
//! hash of the request and holding the request key, so a rerun asks a
//! public service nothing and gives the same answers.

use std::fs;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CachedResponse {
    pub status: u16,
    pub body: String,
    /// The latency of the original request.
    pub latency_ms: f64,
}

#[derive(Serialize, Deserialize)]
struct Entry {
    key: String,
    #[serde(flatten)]
    response: CachedResponse,
}

pub struct ResponseCache {
    dir: PathBuf,
}

/// FNV-1a 64: stable across Rust releases, unlike `DefaultHasher`.
pub fn fnv64(s: &str) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in s.bytes() {
        h ^= b as u64;
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    h
}

impl ResponseCache {
    pub fn new(dir: PathBuf) -> Self {
        Self { dir }
    }

    fn path(&self, key: &str) -> PathBuf {
        self.dir.join(format!("{:016x}.json", fnv64(key)))
    }

    /// The cached response for `key`; `None` when absent, unreadable or a
    /// hash collision.
    pub fn get(&self, key: &str) -> Option<CachedResponse> {
        let body = fs::read_to_string(self.path(key)).ok()?;
        let e: Entry = serde_json::from_str(&body).ok()?;
        (e.key == key).then_some(e.response)
    }

    /// Best effort: a cache that can't be written only costs a rerun.
    pub fn put(&self, key: &str, response: &CachedResponse) {
        let e = Entry {
            key: key.to_string(),
            response: response.clone(),
        };
        if fs::create_dir_all(&self.dir).is_ok()
            && let Ok(body) = serde_json::to_string(&e)
        {
            let _ = fs::write(self.path(key), body);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_and_rejects_collisions() {
        let tmp = tempfile::TempDir::new().unwrap();
        let c = ResponseCache::new(tmp.path().join("g"));
        assert_eq!(c.get("k"), None);
        let r = CachedResponse {
            status: 200,
            body: "{}".into(),
            latency_ms: 12.5,
        };
        c.put("k", &r);
        assert_eq!(c.get("k"), Some(r.clone()));
        // Same file, other key: treated as a miss.
        let other = Entry {
            key: "other".into(),
            response: r,
        };
        fs::write(c.path("k"), serde_json::to_string(&other).unwrap()).unwrap();
        assert_eq!(c.get("k"), None);
        fs::write(c.path("k"), "not json").unwrap();
        assert_eq!(c.get("k"), None);
        assert_eq!(fnv64(""), 0xcbf2_9ce4_8422_2325);
        assert_ne!(fnv64("a"), fnv64("b"));
    }
}
