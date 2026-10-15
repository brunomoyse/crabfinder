//! In-process batch evaluation + confidence-threshold calibration (Lot G).
//!
//! Re-runs the resolver against an existing eval set (no HTTP hop), tabulates
//! the `Confidence` distribution and `street_match_score` quartiles, and
//! writes `confidence-thresholds-v1.json` for downstream calibration.
//!
//! Distinct from `runner.rs`, which exercises the live HTTP service over
//! reqwest. This mode talks straight to `crabfinder_service::resolver::resolve`
//! so the eval is deterministic and isolated from network / serialisation
//! noise — and so we can read `street_match_score` directly (the HTTP path
//! exposes the same number under a different field name, but going through
//! reqwest serialises and re-parses it as f32 for nothing).
//!
//! See `docs/v1-batch-plan.md` §G.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Instant;

use anyhow::{Context, Result};
use serde::Serialize;
use tracing::{info, warn};

use crabfinder_parser::parse;
use crabfinder_service::resolver::{AddressQuery, Confidence, SearchOptions, resolve_with};
use crabfinder_service::{AppState, LoadOptions};

use crate::eval_set::EvalQuery;

/// CLI-facing config for the batch mode. Mirrors the resolver knobs the HTTP
/// service exposes at startup so calibration matches production conditions.
#[derive(Debug, Clone)]
pub struct BatchModeConfig {
    pub index: PathBuf,
    pub abbreviation_dict: Option<PathBuf>,
    pub output: PathBuf,
    pub no_mid_street: bool,
    pub notability_enabled: bool,
}

/// One row in the per-query trace we keep for distribution stats.
#[derive(Debug, Clone)]
struct EvalDecision {
    confidence: Confidence,
    street_match_score: f32,
}

/// On-disk shape of `confidence-thresholds-v1.json`.
///
/// Consumers (none yet — the resolver does not read this back at runtime in
/// v1) get distribution stats *and* the quartile bands that bucket the BM25
/// score within the `Exact` confidence band. The quartiles are recomputed
/// every time this is run, so the file is reproducible from the eval set.
#[derive(Debug, Clone, Serialize)]
pub struct ConfidenceThresholdsV1 {
    pub version: u32,
    pub computed_from: ComputedFrom,
    pub summary: BTreeMap<String, ConfidenceSummary>,
    /// BM25 quartiles among queries that landed in `Confidence::Exact` —
    /// the band where the score is informative. For non-Exact bands the
    /// score is either zero (NotFound) or carries a different meaning
    /// (fuzzy similarity), so mixing them in one distribution would muddy
    /// the calibration.
    pub exact_score_quantiles: ScoreQuantiles,
}

#[derive(Debug, Clone, Serialize)]
pub struct ComputedFrom {
    pub eval_set_paths: Vec<String>,
    pub n_queries: usize,
    pub n_resolved: usize,
    pub elapsed_ms: u128,
}

#[derive(Debug, Clone, Serialize)]
pub struct ConfidenceSummary {
    pub count: usize,
    pub pct: f64,
}

#[derive(Debug, Clone, Serialize)]
pub struct ScoreQuantiles {
    pub n: usize,
    pub q25: f32,
    pub q50: f32,
    pub q75: f32,
    pub q95: f32,
}

pub fn run(
    config: &BatchModeConfig,
    queries: Vec<EvalQuery>,
    eval_set_paths: &[PathBuf],
) -> Result<()> {
    if queries.is_empty() {
        anyhow::bail!("no eval queries supplied; pass --eval-set");
    }

    let mut opts = LoadOptions::new(&config.index);
    opts.abbreviation_dict = config.abbreviation_dict.clone();
    opts.notability_enabled = config.notability_enabled;
    opts.spatial = false;
    let state = AppState::load(&opts)?;
    info!(
        index = %config.index.display(),
        schema_version = state.meta.schema_version,
        "batch-mode: index loaded"
    );
    let ctx = state.resolver_context();
    let search_opts = SearchOptions {
        mid_street_fallback: !config.no_mid_street,
        ..SearchOptions::default()
    };

    let total = queries.len();
    info!(total, "batch-mode: resolving in-process");
    let t0 = Instant::now();

    let mut decisions: Vec<EvalDecision> = Vec::with_capacity(total);
    for q in &queries {
        let parsed = parse(&q.query_string);
        if parsed.street_tokens.is_empty() {
            // Falls through to NotFound naturally — but it's worth a warn
            // since a synthetic eval set should never have a row with zero
            // street tokens.
            warn!(id = %q.id, raw = %q.query_string, "batch-mode: parser produced no street tokens");
        }
        let aq = AddressQuery::from_parsed(&state.index, &parsed);
        let r = resolve_with(&aq, &search_opts, &ctx);
        decisions.push(EvalDecision {
            confidence: r.confidence,
            street_match_score: r.street_match_score,
        });
    }
    let elapsed = t0.elapsed();

    let n_resolved = decisions
        .iter()
        .filter(|d| d.confidence != Confidence::NotFound)
        .count();
    info!(
        n_resolved,
        n_total = total,
        elapsed_ms = elapsed.as_millis(),
        "batch-mode: resolution complete"
    );

    let mut paths: Vec<String> = eval_set_paths
        .iter()
        .map(|p| p.display().to_string())
        .collect();
    paths.sort();
    let thresholds = build_thresholds(
        &decisions,
        ComputedFrom {
            eval_set_paths: paths,
            n_queries: total,
            n_resolved,
            elapsed_ms: elapsed.as_millis(),
        },
    );

    write_thresholds(&config.output, &thresholds)?;

    // Pretty-print a quick summary so an operator running this from the
    // shell gets a usable answer without opening the JSON.
    print_summary(&thresholds);

    Ok(())
}

fn build_thresholds(
    decisions: &[EvalDecision],
    provenance: ComputedFrom,
) -> ConfidenceThresholdsV1 {
    let mut by_confidence: BTreeMap<&'static str, usize> = BTreeMap::new();
    for d in decisions {
        *by_confidence.entry(d.confidence.as_str()).or_insert(0) += 1;
    }
    let total = decisions.len() as f64;
    let summary: BTreeMap<String, ConfidenceSummary> = by_confidence
        .into_iter()
        .map(|(k, count)| {
            (
                k.to_string(),
                ConfidenceSummary {
                    count,
                    pct: 100.0 * count as f64 / total.max(1.0),
                },
            )
        })
        .collect();

    let mut exact_scores: Vec<f32> = decisions
        .iter()
        .filter(|d| d.confidence == Confidence::Exact)
        .map(|d| d.street_match_score)
        .collect();
    // sort_by because f32 is not Ord.
    exact_scores.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));

    let quantiles = ScoreQuantiles {
        n: exact_scores.len(),
        q25: pick_quantile(&exact_scores, 0.25),
        q50: pick_quantile(&exact_scores, 0.50),
        q75: pick_quantile(&exact_scores, 0.75),
        q95: pick_quantile(&exact_scores, 0.95),
    };

    ConfidenceThresholdsV1 {
        version: 1,
        computed_from: provenance,
        summary,
        exact_score_quantiles: quantiles,
    }
}

/// Lower-discrete quantile (same convention as `quantile(p, type=3)` in R,
/// matching the mid_street builder). `idx = round_down(p * (n - 1))`.
fn pick_quantile(sorted: &[f32], p: f64) -> f32 {
    if sorted.is_empty() {
        return 0.0;
    }
    let n = sorted.len();
    let idx = (p * (n as f64 - 1.0)).floor() as usize;
    sorted[idx.min(n - 1)]
}

fn write_thresholds(path: &Path, t: &ConfidenceThresholdsV1) -> Result<()> {
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        std::fs::create_dir_all(parent).with_context(|| format!("creating {parent:?}"))?;
    }
    let body = serde_json::to_string_pretty(t).context("serialising thresholds")?;
    std::fs::write(path, body).with_context(|| format!("writing {path:?}"))?;
    info!(path = %path.display(), "batch-mode: thresholds written");
    Ok(())
}

fn print_summary(t: &ConfidenceThresholdsV1) {
    println!();
    println!("====== --mode=batch summary ======");
    println!(
        "n_queries={} n_resolved={} elapsed_ms={}",
        t.computed_from.n_queries, t.computed_from.n_resolved, t.computed_from.elapsed_ms,
    );
    println!("Confidence distribution:");
    for (k, v) in &t.summary {
        println!("  {:<20}: {:>6}  ({:5.2}%)", k, v.count, v.pct);
    }
    let q = &t.exact_score_quantiles;
    println!(
        "Exact BM25 quartiles (n={}): q25={:.2} q50={:.2} q75={:.2} q95={:.2}",
        q.n, q.q25, q.q50, q.q75, q.q95
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quantile_empty_is_zero() {
        let v: Vec<f32> = Vec::new();
        assert_eq!(pick_quantile(&v, 0.5), 0.0);
    }

    #[test]
    fn quantile_single_value_is_that_value() {
        let v = vec![2.75_f32];
        assert!((pick_quantile(&v, 0.25) - 2.75).abs() < 1e-6);
        assert!((pick_quantile(&v, 0.95) - 2.75).abs() < 1e-6);
    }

    #[test]
    fn quantile_lower_discrete_matches_mid_street_convention() {
        // 4 values: indices 0..3. q25 = floor(0.25*3) = 0 → first.
        // q50 = floor(0.50*3) = 1 → second. q75 = floor(0.75*3) = 2 →
        // third. q95 = floor(0.95*3) = 2 → still third (95% of 3 = 2.85).
        let v = vec![1.0_f32, 2.0, 3.0, 4.0];
        assert_eq!(pick_quantile(&v, 0.25), 1.0);
        assert_eq!(pick_quantile(&v, 0.50), 2.0);
        assert_eq!(pick_quantile(&v, 0.75), 3.0);
        assert_eq!(pick_quantile(&v, 0.95), 3.0);
    }

    #[test]
    fn build_thresholds_tabulates_confidence_and_score() {
        let decisions = vec![
            EvalDecision {
                confidence: Confidence::Exact,
                street_match_score: 10.0,
            },
            EvalDecision {
                confidence: Confidence::Exact,
                street_match_score: 20.0,
            },
            EvalDecision {
                confidence: Confidence::MedianStreet,
                street_match_score: 5.0,
            },
            EvalDecision {
                confidence: Confidence::NotFound,
                street_match_score: 0.0,
            },
        ];
        let prov = ComputedFrom {
            eval_set_paths: vec![],
            n_queries: 4,
            n_resolved: 3,
            elapsed_ms: 1,
        };
        let t = build_thresholds(&decisions, prov);
        assert_eq!(t.summary["exact"].count, 2);
        assert!((t.summary["exact"].pct - 50.0).abs() < 1e-9);
        assert_eq!(t.summary["median_street"].count, 1);
        assert_eq!(t.summary["not_found"].count, 1);
        assert_eq!(t.exact_score_quantiles.n, 2);
        // 2 exact scores sorted [10, 20]. q50 = floor(0.5*1)=0 → 10.0.
        assert!((t.exact_score_quantiles.q50 - 10.0).abs() < 1e-6);
        assert!((t.exact_score_quantiles.q95 - 10.0).abs() < 1e-6);
    }
}
