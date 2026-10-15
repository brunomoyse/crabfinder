//! HTTP runner with bounded parallelism (file 14 §4).
//!
//! Default 8-way concurrency via `futures::stream::iter().buffer_unordered(N)`.
//! Health-check before run; differentiates connection_refused (abort) from
//! 5xx (count as failure, abort if >5% of queries error).
//!
//! Validation mode checks that every expected_* ID in an eval set still
//! resolves in the live index — for post-BeSt-refresh sanity.

use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use futures::stream::{self, StreamExt};
use reqwest::Client;
use serde::Deserialize;
use tracing::{info, warn};

use crate::eval_set::EvalQuery;

/// One per-query result captured by the runner. The metric layer consumes
/// these in Step 6.
#[derive(Debug, Clone)]
pub struct QueryResult {
    pub query: EvalQuery,
    /// Returned features in order. Each tuple is
    /// `(building_group_id, best_id_canonical, match_type)`.
    pub features: Vec<(String, String, String)>,
    /// `(lat, lon)` of each feature, aligned with `features` (`None` when
    /// the feature has no point geometry).
    pub points: Vec<Option<(f64, f64)>>,
    pub top_match_type: String,
    pub raw_hits: usize,
    pub latency_ms: f64,
    /// Non-empty on transport-level failure (network error, 5xx, JSON
    /// parse failure).
    pub error: Option<String>,
}

/// What the service returns at /geocode (only the fields we consume).
#[derive(Debug, Deserialize)]
struct GeocodeResponse {
    #[serde(default)]
    match_type: String,
    #[serde(default)]
    raw_hits: usize,
    #[serde(default)]
    features: Vec<GeocodeFeature>,
}

#[derive(Debug, Deserialize)]
struct GeocodeFeature {
    #[serde(default)]
    building_group_id: String,
    #[serde(default)]
    match_type: String,
    properties: serde_json::Value,
    #[serde(default)]
    geometry: Option<serde_json::Value>,
}

/// `(lat, lon)` of a GeoJSON Point geometry.
fn point_of(geometry: Option<&serde_json::Value>) -> Option<(f64, f64)> {
    let c = geometry?.get("coordinates")?.as_array()?;
    Some((c.get(1)?.as_f64()?, c.first()?.as_f64()?))
}

/// Best-id object as carried inside `feature.properties.id`. We reassemble
/// the canonical "namespace/object_id/version_id" form (matching BestId's
/// Display impl) for string comparison against the eval set's expected.
fn extract_best_id_canonical(props: &serde_json::Value) -> String {
    let id = match props.get("id") {
        Some(v) => v,
        None => return String::new(),
    };
    let ns = id.get("namespace").and_then(|v| v.as_str()).unwrap_or("");
    let ns = ns.trim_end_matches('/');
    let oid = id
        .get("object_identifier")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let vid = id
        .get("version_identifier")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    if ns.is_empty() {
        String::new()
    } else {
        format!("{ns}/{oid}/{vid}")
    }
}

pub struct RunnerConfig {
    pub service_url: String,
    pub parallel: usize,
    pub timeout: Duration,
    pub limit: usize,
}

impl Default for RunnerConfig {
    fn default() -> Self {
        Self {
            service_url: "http://127.0.0.1:8080".to_string(),
            parallel: 8,
            timeout: Duration::from_secs(30),
            limit: 10,
        }
    }
}

/// Health-check before the eval starts.
pub async fn health_check(client: &Client, base: &str) -> Result<()> {
    let url = format!("{base}/geocode?q=health-check&limit=1");
    let resp = client.get(&url).send().await.context("health check GET")?;
    if !resp.status().is_success() {
        bail!("service health check failed: status={}", resp.status());
    }
    let _ = resp
        .json::<GeocodeResponse>()
        .await
        .context("health check JSON parse")?;
    info!(base, "runner: health-check OK");
    Ok(())
}

/// Execute one /geocode query against the service.
async fn execute_one(client: &Client, base: &str, limit: usize, query: EvalQuery) -> QueryResult {
    let t0 = Instant::now();
    let url = format!("{base}/geocode");
    let resp_result = client
        .get(&url)
        .query(&[
            ("q", query.query_string.as_str()),
            ("limit", &limit.to_string()),
        ])
        .send()
        .await;
    match resp_result {
        Err(e) => QueryResult {
            query,
            features: Vec::new(),
            points: Vec::new(),
            top_match_type: String::new(),
            raw_hits: 0,
            latency_ms: (t0.elapsed().as_secs_f64() * 1000.0),
            error: Some(format!("transport: {e}")),
        },
        Ok(resp) => {
            let status = resp.status();
            if !status.is_success() {
                return QueryResult {
                    query,
                    features: Vec::new(),
                    points: Vec::new(),
                    top_match_type: String::new(),
                    raw_hits: 0,
                    latency_ms: (t0.elapsed().as_secs_f64() * 1000.0),
                    error: Some(format!("http {status}")),
                };
            }
            match resp.json::<GeocodeResponse>().await {
                Err(e) => QueryResult {
                    query,
                    features: Vec::new(),
                    points: Vec::new(),
                    top_match_type: String::new(),
                    raw_hits: 0,
                    latency_ms: (t0.elapsed().as_secs_f64() * 1000.0),
                    error: Some(format!("json: {e}")),
                },
                Ok(body) => {
                    let points = body
                        .features
                        .iter()
                        .map(|f| point_of(f.geometry.as_ref()))
                        .collect();
                    let features: Vec<(String, String, String)> = body
                        .features
                        .into_iter()
                        .map(|f| {
                            let best_id = extract_best_id_canonical(&f.properties);
                            (f.building_group_id, best_id, f.match_type)
                        })
                        .collect();
                    QueryResult {
                        query,
                        features,
                        points,
                        top_match_type: body.match_type,
                        raw_hits: body.raw_hits,
                        latency_ms: (t0.elapsed().as_secs_f64() * 1000.0),
                        error: None,
                    }
                }
            }
        }
    }
}

/// Run an entire eval set with bounded parallelism.
///
/// Aborts mid-run if connection-level errors exceed 5% of queries executed
/// (the "connection_refused → service-degraded" case from file 14 §4).
pub async fn run_eval(config: &RunnerConfig, queries: Vec<EvalQuery>) -> Result<Vec<QueryResult>> {
    let client = Client::builder()
        .timeout(config.timeout)
        .build()
        .context("constructing reqwest client")?;
    health_check(&client, &config.service_url).await?;

    let total = queries.len();
    info!(total, parallel = config.parallel, "runner: starting eval");

    let base = config.service_url.clone();
    let limit = config.limit;
    let client_arc = client.clone();

    let mut results: Vec<QueryResult> = stream::iter(queries)
        .map(|q| {
            let client = client_arc.clone();
            let base = base.clone();
            async move { execute_one(&client, &base, limit, q).await }
        })
        .buffer_unordered(config.parallel)
        .collect()
        .await;

    // Sort by query id for deterministic report order
    results.sort_by(|a, b| a.query.id.cmp(&b.query.id));

    let transport_errs = results
        .iter()
        .filter(|r| {
            r.error
                .as_deref()
                .map(|e| e.starts_with("transport:"))
                .unwrap_or(false)
        })
        .count();
    let http_5xx_errs = results
        .iter()
        .filter(|r| {
            r.error
                .as_deref()
                .map(|e| e.starts_with("http 5"))
                .unwrap_or(false)
        })
        .count();
    let pct_5xx = 100.0 * http_5xx_errs as f64 / total as f64;

    if transport_errs > 0 {
        bail!(
            "{transport_errs} queries failed at transport level (likely \
             connection_refused) — report invalid; check service is up"
        );
    }
    if pct_5xx > 5.0 {
        bail!(
            "service degraded: {http_5xx_errs}/{total} queries returned 5xx ({pct_5xx:.1}%); \
             threshold 5.0%"
        );
    }

    info!(
        total = results.len(),
        transport_errs, http_5xx_errs, "runner: eval complete"
    );
    Ok(results)
}

/// Dry-run: execute the first 50 queries, project full-run wall-clock.
pub async fn dry_run(config: &RunnerConfig, mut queries: Vec<EvalQuery>) -> Result<()> {
    let total_planned = queries.len();
    let dry_n = 50.min(total_planned);
    queries.truncate(dry_n);

    let t0 = Instant::now();
    let results = run_eval(config, queries).await?;
    let elapsed = t0.elapsed().as_secs_f64();

    // Per-variant sanity check: did the eval set generate enough variants?
    use std::collections::HashMap;
    let mut variant_seen: HashMap<String, usize> = HashMap::new();
    for r in &results {
        let variant = match &r.query.source {
            crate::eval_set::EvalQuerySource::Synthetic { variant_type, .. } => {
                variant_type.as_str().to_string()
            }
            crate::eval_set::EvalQuerySource::Adversarial { category, .. } => {
                format!("adv:{}", category.as_str())
            }
        };
        *variant_seen.entry(variant).or_default() += 1;
    }

    let latencies: Vec<f64> = results.iter().map(|r| r.latency_ms).collect();
    let mean_ms = latencies.iter().sum::<f64>() / latencies.len().max(1) as f64;
    let projection_s = elapsed * (total_planned as f64 / dry_n as f64);

    println!();
    println!("====== --dry-run summary ======");
    println!("Ran {dry_n} queries in {elapsed:.1}s (mean {mean_ms:.1} ms/query)");
    println!("Projected full run time for {total_planned} queries: {projection_s:.1}s");
    println!("Variant breakdown in the first {dry_n}:");
    let mut entries: Vec<_> = variant_seen.into_iter().collect();
    entries.sort();
    for (v, n) in entries {
        println!("  {v:<24}: {n}");
    }

    Ok(())
}

/// Validate that every expected_* ID in the eval set still resolves in the
/// current index. For MVP: query the service for each, check whether the
/// expected bgid (or any of the bgid set) appears in the top-50.
pub async fn validate_eval_set(
    config: &RunnerConfig,
    queries: Vec<EvalQuery>,
    max_missing_pct: f64,
) -> Result<()> {
    let client = Client::builder()
        .timeout(config.timeout)
        .build()
        .context("constructing reqwest client")?;
    health_check(&client, &config.service_url).await?;

    let total = queries.len();
    info!(total, "validator: starting");

    let base = config.service_url.clone();
    let client_arc = client.clone();
    let results: Vec<(String, bool)> = stream::iter(queries)
        .map(|q| {
            let client = client_arc.clone();
            let base = base.clone();
            async move {
                if q.expected_bgid_set.is_empty() {
                    // Nothing to validate (e.g., known_failure entries).
                    return (q.id, true);
                }
                let r = execute_one(&client, &base, 50, q.clone()).await;
                let expected_bgids: std::collections::HashSet<&str> =
                    q.expected_bgid_set.iter().map(String::as_str).collect();
                let found = r
                    .features
                    .iter()
                    .any(|(bgid, _, _)| expected_bgids.contains(bgid.as_str()));
                (q.id, found)
            }
        })
        .buffer_unordered(config.parallel)
        .collect()
        .await;

    let missing: Vec<String> = results
        .iter()
        .filter(|(_, ok)| !ok)
        .map(|(id, _)| id.clone())
        .collect();
    let missing_pct = 100.0 * missing.len() as f64 / total as f64;
    println!();
    println!("====== --validate-eval-set summary ======");
    println!("Queries validated: {total}");
    println!(
        "Missing expected_* IDs in top-50: {} ({:.2}%)",
        missing.len(),
        missing_pct
    );
    if missing.len() <= 20 {
        for id in &missing {
            println!("  MISSING: {id}");
        }
    } else {
        for id in missing.iter().take(20) {
            println!("  MISSING: {id}");
        }
        println!("  ... and {} more (truncated)", missing.len() - 20);
    }

    if missing_pct > max_missing_pct {
        bail!(
            "{} eval set entries have stale expected_* IDs ({:.2}% > threshold {:.2}%); \
             regenerate eval set after BeSt refresh",
            missing.len(),
            missing_pct,
            max_missing_pct
        );
    }
    if missing_pct > 0.0 {
        warn!(
            missing = missing.len(),
            pct = format!("{missing_pct:.2}"),
            "validator: some entries missing but within tolerance"
        );
    }
    Ok(())
}
