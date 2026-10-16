//! Shared harness for the HTTP integration tests: build a `TestIndex`, load
//! it through the production `AppState::load`, and drive `http::router`
//! in-process with `tower::ServiceExt::oneshot`.
#![allow(dead_code)] // each test binary uses a different subset

use axum::{
    Router,
    body::Body,
    http::{Request, StatusCode},
};
use crabfinder_core::Address;
use crabfinder_service::{AppState, LoadOptions, http};
use crabfinder_test_support::TestIndex;
use http_body_util::BodyExt;
use serde_json::Value;
use tower::ServiceExt;

/// A loaded app over a temp index. Keep it alive for the whole test — the
/// index directory is deleted when it drops.
pub struct App {
    pub index: TestIndex,
    pub state: AppState,
}

impl App {
    pub fn new(addrs: &[Address]) -> Self {
        Self::with_options(addrs, |o| o)
    }

    pub fn with_options(addrs: &[Address], f: impl FnOnce(LoadOptions) -> LoadOptions) -> Self {
        let index = TestIndex::build_with_mid_street(addrs);
        let state = AppState::load(&f(LoadOptions::new(index.path()))).expect("load state");
        Self { index, state }
    }

    /// An app whose index also has the localities file built from `addrs`
    /// and an OSM places CSV (`TestIndex::build_with_places`).
    pub fn with_places(addrs: &[Address], places_csv: &str) -> Self {
        let index = TestIndex::build_with_places(addrs, places_csv);
        let state = AppState::load(&LoadOptions::new(index.path())).expect("load state");
        Self { index, state }
    }

    /// `with_places`, plus an OSM landmarks CSV loaded through
    /// `LoadOptions::pois` (written next to the index).
    pub fn with_pois(addrs: &[Address], places_csv: &str, pois_csv: &str) -> Self {
        let index = TestIndex::build_with_places(addrs, places_csv);
        let pois = index.path().join("pois-test.csv");
        std::fs::write(&pois, pois_csv).unwrap();
        let state = AppState::load(&LoadOptions::new(index.path()).pois(pois)).expect("load state");
        Self { index, state }
    }

    /// An app over `addrs` whose history file holds `gone`
    /// (`crabfinder_test_support::write_history`).
    pub fn with_history(addrs: &[Address], gone: &[Address]) -> Self {
        let index = TestIndex::build_with_mid_street(addrs);
        crabfinder_test_support::write_history(index.path(), gone);
        let state = AppState::load(&LoadOptions::new(index.path())).expect("load state");
        Self { index, state }
    }

    pub fn router(&self) -> Router {
        http::router(self.state.clone())
    }

    /// GET `uri`, returning the status and the body parsed as JSON.
    pub async fn get(&self, uri: &str) -> (StatusCode, Value) {
        let resp = self
            .router()
            .oneshot(Request::get(uri).body(Body::empty()).unwrap())
            .await
            .unwrap();
        let status = resp.status();
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        let json = serde_json::from_slice(&bytes)
            .unwrap_or_else(|e| panic!("non-JSON body for {uri}: {e}: {bytes:?}"));
        (status, json)
    }

    /// GET `uri` and assert 200.
    pub async fn ok(&self, uri: &str) -> Value {
        let (status, body) = self.get(uri).await;
        assert_eq!(status, StatusCode::OK, "{uri} → {body}");
        body
    }
}

/// Percent-encode a query value (spaces, accents, `|`, `/`, …).
pub fn enc(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

pub fn features(body: &Value) -> &Vec<Value> {
    body["features"].as_array().expect("features array")
}

pub fn prop<'a>(f: &'a Value, key: &str) -> &'a Value {
    &f["properties"][key]
}

pub fn street_fr(f: &Value) -> String {
    prop(f, "street_name")["fr"]
        .as_str()
        .unwrap_or("")
        .to_lowercase()
}

pub fn street_nl(f: &Value) -> String {
    prop(f, "street_name")["nl"]
        .as_str()
        .unwrap_or("")
        .to_lowercase()
}

impl App {
    /// POST `body` to `uri` with `content_type`; returns status,
    /// response content type and the body as text.
    pub async fn post(
        &self,
        uri: &str,
        content_type: &str,
        body: impl Into<Body>,
    ) -> (StatusCode, String, String) {
        let resp = self
            .router()
            .oneshot(
                Request::post(uri)
                    .header(axum::http::header::CONTENT_TYPE, content_type)
                    .body(body.into())
                    .unwrap(),
            )
            .await
            .unwrap();
        let status = resp.status();
        let ct = resp
            .headers()
            .get(axum::http::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_string();
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        (status, ct, String::from_utf8(bytes.to_vec()).unwrap())
    }
}

/// Build a `multipart/form-data` body. Each part is `(name, filename, content)`.
pub fn multipart(parts: &[(&str, Option<&str>, &str)]) -> (String, String) {
    let boundary = "crabfinder-test-boundary";
    let mut body = String::new();
    for (name, filename, content) in parts {
        body.push_str(&format!("--{boundary}\r\n"));
        match filename {
            Some(f) => body.push_str(&format!(
                "Content-Disposition: form-data; name=\"{name}\"; filename=\"{f}\"\r\n\r\n"
            )),
            None => body.push_str(&format!(
                "Content-Disposition: form-data; name=\"{name}\"\r\n\r\n"
            )),
        }
        body.push_str(content);
        body.push_str("\r\n");
    }
    body.push_str(&format!("--{boundary}--\r\n"));
    (format!("multipart/form-data; boundary={boundary}"), body)
}
