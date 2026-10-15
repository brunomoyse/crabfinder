//! Prometheus metrics (v2 Phase 8), served at `GET /metrics`.
//!
//! Series:
//! - `crabfinder_http_requests_total{route,method,status}` and
//!   `crabfinder_http_request_duration_seconds{route}` (histogram), per
//!   matched route (`track`);
//! - `crabfinder_http_rejected_total{route,reason}`: requests answered 503
//!   by the timeout / concurrency guard (`http::guard`);
//! - `crabfinder_search_total{tier,confidence,match_type}`: one per
//!   resolver search (`/geocode`, every batch row), labelled by the top
//!   candidate;
//! - `crabfinder_search_capped_total`: searches whose over-fetch hit
//!   `RAW_HIT_CAP` (the tail may miss buildings).
//!
//! The `metrics` macros are no-ops until `install` sets the global
//! recorder, so the CLI and eval batch mode pay nothing.

use std::sync::OnceLock;
use std::time::{Duration, Instant};

use axum::extract::{MatchedPath, Request};
use axum::middleware::Next;
use axum::response::Response;
use metrics::{counter, histogram};
use metrics_exporter_prometheus::{Matcher, PrometheusBuilder, PrometheusHandle};
use tracing::warn;

use crate::resolver::CandidateSet;

/// Request latency buckets, seconds: sub-millisecond strict hits up to a
/// pathological 10 s.
const LATENCY_BUCKETS: &[f64] = &[
    0.001, 0.0025, 0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 10.0,
];

/// Install the process-wide Prometheus recorder (once) and return a handle
/// that renders it. Idempotent: later calls return the same handle.
pub fn install() -> PrometheusHandle {
    static HANDLE: OnceLock<PrometheusHandle> = OnceLock::new();
    HANDLE
        .get_or_init(|| {
            let recorder = PrometheusBuilder::new()
                .set_buckets_for_metric(
                    Matcher::Suffix("_duration_seconds".into()),
                    LATENCY_BUCKETS,
                )
                .expect("LATENCY_BUCKETS is non-empty")
                .build_recorder();
            let handle = recorder.handle();
            if metrics::set_global_recorder(recorder).is_err() {
                warn!("another metrics recorder is installed; /metrics will stay empty");
            }
            handle
        })
        .clone()
}

/// Drain histogram samples into their buckets every `every`. Without it
/// samples pile up between scrapes. Runs until the runtime shuts down.
pub fn spawn_upkeep(handle: PrometheusHandle, every: Duration) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(every);
        loop {
            tick.tick().await;
            handle.run_upkeep();
        }
    })
}

/// Route middleware: count and time every request by its matched route.
pub async fn track(req: Request, next: Next) -> Response {
    let route = route_of(&req);
    let method = req.method().as_str().to_owned();
    let t0 = Instant::now();
    let resp = next.run(req).await;
    let status = resp.status().as_u16().to_string();
    counter!(
        "crabfinder_http_requests_total",
        "route" => route.clone(),
        "method" => method,
        "status" => status,
    )
    .increment(1);
    histogram!("crabfinder_http_request_duration_seconds", "route" => route)
        .record(t0.elapsed().as_secs_f64());
    resp
}

/// The request's matched route template (`/building/{building_group_id}/units`,
/// not the concrete path, to keep label cardinality bounded).
pub fn route_of(req: &Request) -> String {
    req.extensions()
        .get::<MatchedPath>()
        .map_or_else(|| "unmatched".to_owned(), |p| p.as_str().to_owned())
}

/// Count a request the guard turned away (`reason`: `timeout`).
pub fn record_rejected(route: String, reason: &'static str) {
    counter!("crabfinder_http_rejected_total", "route" => route, "reason" => reason).increment(1);
}

/// Count one resolver search by its winning tier and top candidate.
pub fn record_search(set: &CandidateSet) {
    let (confidence, match_type) = set.candidates.first().map_or(("not_found", "none"), |c| {
        (c.confidence.as_str(), c.hit.match_type.as_str())
    });
    counter!(
        "crabfinder_search_total",
        "tier" => set.tier.as_str(),
        "confidence" => confidence,
        "match_type" => match_type,
    )
    .increment(1);
    if set.capped {
        counter!("crabfinder_search_capped_total").increment(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn install_is_idempotent_and_renders_recorded_series() {
        let a = install();
        let b = install();
        counter!("crabfinder_test_total").increment(2);
        record_search(&CandidateSet::empty());
        let out = b.render();
        assert!(out.contains("crabfinder_test_total 2"), "{out}");
        assert!(
            out.contains(
                r#"crabfinder_search_total{tier="none",confidence="not_found",match_type="none"}"#
            ),
            "{out}"
        );
        assert!(a.render().contains("crabfinder_test_total"));
    }

    #[test]
    fn capped_searches_are_counted() {
        let h = install();
        let set = CandidateSet {
            capped: true,
            ..CandidateSet::empty()
        };
        record_search(&set);
        assert!(h.render().contains("crabfinder_search_capped_total"));
    }

    #[tokio::test(start_paused = true)]
    async fn upkeep_ticks_until_aborted() {
        let task = spawn_upkeep(install(), Duration::from_secs(1));
        tokio::time::sleep(Duration::from_secs(3)).await;
        assert!(!task.is_finished());
        task.abort();
    }
}
