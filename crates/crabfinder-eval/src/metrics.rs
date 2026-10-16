//! Metric computation (file 14 §1 — Tier A + partial Tier B for MVP).
//!
//! Aggregation granularities for MVP: overall, per-region, per-variant,
//! per-adversarial-category. Class-2 ambiguity metrics + region_leakage +
//! recall_by_match_type deferred to Phase 6.5b.
//!
//! known_failure semantics: tracked in a dedicated panel, NOT included in
//! aggregate metrics (file 14 §4).

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::eval_set::{AdversarialCategory, EvalQuerySource, MatchLevel, VariantType};
use crate::runner::QueryResult;

/// One bucket's aggregate metrics.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct MetricBucket {
    pub n: usize,
    pub recall_at_1: f64,
    pub recall_at_5: f64,
    pub recall_at_10: f64,
    pub mrr: f64,
    pub building_group_recall_at_1: f64,
    pub strict_match_rate: f64,
    /// Adversarial buckets: the `match_level`s scored in this bucket.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub match_levels: Vec<String>,
}

/// Distance from the top-1 point to the query's reference point (v3
/// Phase 11.1): what every geocoder can be scored on, BeSt ids or not.
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
pub struct DistanceBucket {
    /// Queries with a reference point.
    pub n: usize,
    /// Share of them answered with a point.
    pub found_rate: f64,
    /// Share of `n` whose top-1 lies within 25 / 100 / 250 m (no answer
    /// counts as a miss).
    pub within_25m: f64,
    pub within_100m: f64,
    pub within_250m: f64,
    /// Over the answered ones.
    pub median_error_m: Option<f64>,
    pub p90_error_m: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct LatencyBucket {
    pub n: usize,
    pub p50_ms: f64,
    pub p95_ms: f64,
    pub p99_ms: f64,
    pub mean_ms: f64,
    pub max_ms: f64,
}

/// Per-known-failure entry tracking — see file 14 §4 four states.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KnownFailureEntry {
    pub id: String,
    pub subcategory: String,
    /// "still_failing" / "now_passing" / "failing_differently".
    /// (Phase 6.5b adds "unverifiable" for malformed-payload detection.)
    pub state: String,
    /// Top-1 returned today, for context.
    pub current_top1_bgid: String,
    pub current_top1_match_type: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Metrics {
    pub query_count: usize,
    pub aggregated_query_count: usize, // excludes known_failure entries
    pub error_count: usize,
    pub overall: MetricBucket,
    pub latency: LatencyBucket,
    pub by_region: HashMap<String, MetricBucket>,
    pub by_variant: HashMap<String, MetricBucket>,
    pub by_language: HashMap<String, MetricBucket>,
    pub by_adversarial_category: HashMap<String, MetricBucket>,
    /// Real-world queries by dataset (v3 Phase 11.1).
    #[serde(default)]
    pub by_realworld_source: HashMap<String, MetricBucket>,
    /// Top-1 distance to the reference point, over every query that has
    /// one (v3 Phase 11.1).
    #[serde(default)]
    pub distance: DistanceBucket,
    /// The same by set (`synthetic`, `adversarial`, `realworld`), by
    /// real-world dataset (`realworld:<source>`) and by region
    /// (`region:<region>`).
    #[serde(default)]
    pub distance_by: HashMap<String, DistanceBucket>,
    /// Phase 6.5a partial Tier B — file 14 §1 metric #9.
    pub cross_language_recall_at_1: f64,
    pub cross_language_n: usize,
    pub cross_language_recall_at_1_boosted: f64,
    pub cross_language_boosted_n: usize,
    pub known_failure_tracking: Vec<KnownFailureEntry>,
}

/// Compute all MVP metrics over the runner's per-query results.
pub fn compute(results: &[QueryResult]) -> Metrics {
    let mut m = Metrics {
        query_count: results.len(),
        ..Default::default()
    };

    let mut overall_acc = Acc::default();
    let mut latency_samples: Vec<f64> = Vec::with_capacity(results.len());

    let mut by_region: HashMap<String, Acc> = HashMap::new();
    let mut by_variant: HashMap<String, Acc> = HashMap::new();
    let mut by_language: HashMap<String, Acc> = HashMap::new();
    let mut by_adv: HashMap<String, Acc> = HashMap::new();
    let mut by_rw: HashMap<String, Acc> = HashMap::new();
    let mut distances: Vec<Option<f64>> = Vec::new();
    let mut distances_by: HashMap<String, Vec<Option<f64>>> = HashMap::new();

    let mut cl_acc = Acc::default();
    let mut cl_boost_acc = Acc::default();

    for r in results {
        if r.error.is_some() {
            m.error_count += 1;
            continue;
        }
        latency_samples.push(r.latency_ms);

        // Known_failure → tracking panel, NOT aggregates
        if r.query.expected_to_fail {
            let (subcategory, _category) = match &r.query.source {
                EvalQuerySource::Adversarial {
                    subcategory,
                    category,
                } => (subcategory.clone(), category.as_str()),
                _ => ("?".to_string(), "?"),
            };
            let (top_bgid, top_mt) = r
                .features
                .first()
                .map(|(bgid, _, mt)| (bgid.clone(), mt.clone()))
                .unwrap_or_default();
            let still_failing = !r
                .features
                .iter()
                .take(1)
                .any(|(bgid, _, _)| r.query.expected_bgid_set.iter().any(|b| b == bgid));
            let state = if still_failing {
                // For known_failure with empty expected_bgid_set (the typical
                // case), "still_failing" means the query continues to behave
                // as documented — that's the expected steady state.
                "still_failing".to_string()
            } else {
                "now_passing".to_string()
            };
            m.known_failure_tracking.push(KnownFailureEntry {
                id: r.query.id.clone(),
                subcategory,
                state,
                current_top1_bgid: top_bgid,
                current_top1_match_type: top_mt,
            });
            continue;
        }

        m.aggregated_query_count += 1;
        if r.query.reference_point.is_some() {
            let d = top1_error_m(r);
            distances.push(d);
            let mut keys = vec![r.query.source.set_kind().to_string()];
            if let EvalQuerySource::RealWorld { source, .. } = &r.query.source {
                keys.push(format!("realworld:{source}"));
            }
            if let Some(region) = r.query.source.region() {
                keys.push(format!("region:{region}"));
            }
            for k in keys {
                distances_by.entry(k).or_default().push(d);
            }
        }
        let (rank, bid_rank) = level_ranks(r);
        let strict = r.top_match_type == "strict";
        overall_acc.add(rank, bid_rank, strict);

        // by_region / by_variant / by_language come from the source variant
        match &r.query.source {
            EvalQuerySource::Synthetic {
                variant_type,
                region,
                street_primary_lang,
                boosted,
            } => {
                by_region
                    .entry(region.clone())
                    .or_default()
                    .add(rank, bid_rank, strict);
                by_variant
                    .entry(variant_type.as_str().to_string())
                    .or_default()
                    .add(rank, bid_rank, strict);
                by_language
                    .entry(street_primary_lang.clone())
                    .or_default()
                    .add(rank, bid_rank, strict);
                if *variant_type == VariantType::CrossLanguage {
                    if *boosted {
                        cl_boost_acc.add(rank, bid_rank, strict);
                    } else {
                        cl_acc.add(rank, bid_rank, strict);
                    }
                }
            }
            EvalQuerySource::Adversarial { category, .. } => {
                let acc = by_adv.entry(category.as_str().to_string()).or_default();
                acc.add(rank, bid_rank, strict);
                acc.levels.insert(r.query.match_level.as_str());
                if *category == AdversarialCategory::CrossLanguage {
                    cl_acc.add(rank, bid_rank, strict);
                }
            }
            EvalQuerySource::RealWorld { source, .. } => {
                let acc = by_rw.entry(source.clone()).or_default();
                acc.add(rank, bid_rank, strict);
                acc.levels.insert(r.query.match_level.as_str());
            }
        }
    }

    m.overall = overall_acc.into_bucket();
    m.latency = compute_latency(&mut latency_samples);
    m.by_region = by_region
        .into_iter()
        .map(|(k, v)| (k, v.into_bucket()))
        .collect();
    m.by_variant = by_variant
        .into_iter()
        .map(|(k, v)| (k, v.into_bucket()))
        .collect();
    m.by_language = by_language
        .into_iter()
        .map(|(k, v)| (k, v.into_bucket()))
        .collect();
    m.by_adversarial_category = by_adv
        .into_iter()
        .map(|(k, v)| (k, v.into_bucket()))
        .collect();
    m.by_realworld_source = by_rw
        .into_iter()
        .map(|(k, v)| (k, v.into_bucket()))
        .collect();
    m.distance = distance_bucket(&distances);
    m.distance_by = distances_by
        .into_iter()
        .map(|(k, v)| (k, distance_bucket(&v)))
        .collect();
    m.cross_language_recall_at_1 = if cl_acc.n > 0 {
        cl_acc.recall_at_1 as f64 / cl_acc.n as f64
    } else {
        0.0
    };
    m.cross_language_n = cl_acc.n;
    m.cross_language_recall_at_1_boosted = if cl_boost_acc.n > 0 {
        cl_boost_acc.recall_at_1 as f64 / cl_boost_acc.n as f64
    } else {
        0.0
    };
    m.cross_language_boosted_n = cl_boost_acc.n;

    m
}

/// 1-indexed rank of the first feature whose extracted key is in `expected`.
/// 0 means not found.
/// `(rank, best_id rank)` of `r` at its query's match level, 1-based; 0
/// when not found. Only the address level ranks best ids.
pub fn level_ranks(r: &QueryResult) -> (usize, usize) {
    let q = &r.query;
    match q.match_level {
        MatchLevel::Address => (
            find_rank(&q.expected_bgid_set, &r.features, |t| &t.0),
            find_rank(&q.expected_best_id_set, &r.features, |t| &t.1),
        ),
        MatchLevel::Building => (find_rank(&q.expected_bgid_set, &r.features, |t| &t.0), 0),
        MatchLevel::Street => (street_rank(&q.expected_bgid_set, &r.features), 0),
        MatchLevel::Locality => (
            q.expected_point
                .map_or(0, |(p, within)| within_rank(p, within, &r.points)),
            0,
        ),
    }
}

/// Metres from the top-1 point to the query's reference point; `None`
/// without either.
pub fn top1_error_m(r: &QueryResult) -> Option<f64> {
    let reference = r.query.reference_point?;
    let top = r.points.first().copied().flatten()?;
    Some(haversine_m(reference, top))
}

/// Aggregate per-query top-1 errors (`None` = no answer).
pub fn distance_bucket(errors: &[Option<f64>]) -> DistanceBucket {
    let n = errors.len();
    if n == 0 {
        return DistanceBucket::default();
    }
    let mut found: Vec<f64> = errors.iter().flatten().copied().collect();
    found.sort_by(f64::total_cmp);
    let share = |max: f64| found.iter().filter(|d| **d <= max).count() as f64 / n as f64;
    let pick = |q: f64| {
        (!found.is_empty()).then(|| {
            let idx = ((q * found.len() as f64).ceil() as usize)
                .saturating_sub(1)
                .min(found.len() - 1);
            found[idx]
        })
    };
    DistanceBucket {
        n,
        found_rate: found.len() as f64 / n as f64,
        within_25m: share(25.0),
        within_100m: share(100.0),
        within_250m: share(250.0),
        median_error_m: pick(0.5),
        p90_error_m: pick(0.9),
    }
}

/// 1-based rank of the first point within `within_m` of `p`; 0 when none.
fn within_rank(p: (f64, f64), within_m: f64, points: &[Option<(f64, f64)>]) -> usize {
    points
        .iter()
        .position(|x| x.is_some_and(|x| haversine_m(p, x) <= within_m))
        .map_or(0, |i| i + 1)
}

/// Great-circle distance in metres between two `(lat, lon)` points.
pub fn haversine_m(a: (f64, f64), b: (f64, f64)) -> f64 {
    let (la1, lo1, la2, lo2) = (
        a.0.to_radians(),
        a.1.to_radians(),
        b.0.to_radians(),
        b.1.to_radians(),
    );
    let h = ((la2 - la1) / 2.0).sin().powi(2)
        + la1.cos() * la2.cos() * ((lo2 - lo1) / 2.0).sin().powi(2);
    2.0 * 6_371_008.8 * h.sqrt().asin()
}

/// Street part of a building group id (`{ns}|{oid}|{hn}` → `{ns}|{oid}`).
fn street_of(bgid: &str) -> &str {
    bgid.rsplit_once('|').map_or(bgid, |(s, _)| s)
}

/// 1-based rank of the first feature on one of the expected buildings'
/// streets; 0 when none is.
fn street_rank(expected_bgids: &[String], features: &[(String, String, String)]) -> usize {
    let streets: std::collections::HashSet<&str> =
        expected_bgids.iter().map(|b| street_of(b)).collect();
    features
        .iter()
        .position(|f| !f.0.is_empty() && streets.contains(street_of(&f.0)))
        .map_or(0, |i| i + 1)
}

fn find_rank<F>(expected: &[String], features: &[(String, String, String)], key: F) -> usize
where
    F: Fn(&(String, String, String)) -> &String,
{
    if expected.is_empty() {
        return 0;
    }
    let exp_set: std::collections::HashSet<&str> = expected.iter().map(String::as_str).collect();
    for (i, f) in features.iter().enumerate() {
        let k = key(f);
        if !k.is_empty() && exp_set.contains(k.as_str()) {
            return i + 1;
        }
    }
    0
}

#[derive(Default, Debug)]
struct Acc {
    n: usize,
    recall_at_1: usize,
    recall_at_5: usize,
    recall_at_10: usize,
    rr_sum: f64, // sum of 1/rank for queries that found at any rank
    bgid_recall_at_1: usize,
    strict_count: usize,
    levels: std::collections::BTreeSet<&'static str>,
}

impl Acc {
    /// `bgid_rank` is rank w.r.t. expected_bgid_set; `best_id_rank` w.r.t.
    /// expected_best_id_set. For Phase 6.5a MVP we treat "recall@1" as the
    /// best_id rank (literal best_id match) AND track building_group_recall@1
    /// separately, per file 14 §1 Tier A definitions.
    fn add(&mut self, bgid_rank: usize, best_id_rank: usize, strict: bool) {
        self.n += 1;
        if strict {
            self.strict_count += 1;
        }
        if best_id_rank > 0 {
            self.rr_sum += 1.0 / best_id_rank as f64;
            if best_id_rank == 1 {
                self.recall_at_1 += 1;
            }
            if best_id_rank <= 5 {
                self.recall_at_5 += 1;
            }
            if best_id_rank <= 10 {
                self.recall_at_10 += 1;
            }
        } else if bgid_rank > 0 {
            // For adversarial entries with no best_id (e.g. notability sets),
            // fall back to bgid for MRR + recall@K so the bucket still earns
            // signal. The literal-best_id metric is reported separately as
            // bgid_recall_at_1 below; both shapes coexist in the bucket.
            self.rr_sum += 1.0 / bgid_rank as f64;
            if bgid_rank == 1 {
                self.recall_at_1 += 1;
            }
            if bgid_rank <= 5 {
                self.recall_at_5 += 1;
            }
            if bgid_rank <= 10 {
                self.recall_at_10 += 1;
            }
        }
        if bgid_rank == 1 {
            self.bgid_recall_at_1 += 1;
        }
    }

    fn into_bucket(self) -> MetricBucket {
        let n = self.n.max(1) as f64;
        MetricBucket {
            n: self.n,
            recall_at_1: self.recall_at_1 as f64 / n,
            recall_at_5: self.recall_at_5 as f64 / n,
            recall_at_10: self.recall_at_10 as f64 / n,
            mrr: self.rr_sum / n,
            building_group_recall_at_1: self.bgid_recall_at_1 as f64 / n,
            strict_match_rate: self.strict_count as f64 / n,
            match_levels: self.levels.into_iter().map(String::from).collect(),
        }
    }
}

fn compute_latency(samples: &mut [f64]) -> LatencyBucket {
    if samples.is_empty() {
        return LatencyBucket::default();
    }
    samples.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let n = samples.len();
    let mean = samples.iter().sum::<f64>() / n as f64;
    let max = *samples.last().unwrap();
    let pick = |q: f64| -> f64 {
        // Nearest-rank percentile (1-indexed). Matches Tantivy/Pelias eval
        // conventions where p99 of 100 samples is the 99th-largest.
        let idx = ((q * n as f64).ceil() as usize)
            .saturating_sub(1)
            .min(n - 1);
        samples[idx]
    };
    LatencyBucket {
        n,
        p50_ms: pick(0.50),
        p95_ms: pick(0.95),
        p99_ms: pick(0.99),
        mean_ms: mean,
        max_ms: max,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::eval_set::{EvalQuery, EvalQuerySource, MatchLevel, VariantType};

    fn synth_q(id: &str, expected_bgid: &str, expected_best: &str, region: &str) -> EvalQuery {
        EvalQuery {
            id: id.into(),
            query_string: "q".into(),
            expected_bgid_set: vec![expected_bgid.into()],
            expected_best_id_set: vec![expected_best.into()],
            expected_to_fail: false,
            match_level: MatchLevel::Address,
            expected_point: None,
            reference_point: None,
            structured: None,
            source: EvalQuerySource::Synthetic {
                variant_type: VariantType::Canonical,
                region: region.into(),
                street_primary_lang: "nl".into(),
                boosted: false,
            },
        }
    }

    fn result_with_top(
        q: EvalQuery,
        top_bgid: &str,
        top_best: &str,
        mt: &str,
        latency: f64,
    ) -> QueryResult {
        QueryResult {
            query: q,
            features: vec![(top_bgid.into(), top_best.into(), mt.into())],
            points: vec![Some((50.0, 4.0))],
            top_match_type: mt.into(),
            raw_hits: 1,
            latency_ms: latency,
            error: None,
        }
    }

    #[test]
    fn recall_at_1_seven_of_ten_correct_top1() {
        let results: Vec<QueryResult> = (0..10)
            .map(|i| {
                let q = synth_q(
                    &format!("q{i}"),
                    &format!("bgid-{i}"),
                    &format!("best-{i}"),
                    "flanders",
                );
                let (top_bgid, top_best) = if i < 7 {
                    (format!("bgid-{i}"), format!("best-{i}")) // hit
                } else {
                    (format!("other-{i}"), format!("other-{i}")) // miss
                };
                result_with_top(q, &top_bgid, &top_best, "strict", 20.0)
            })
            .collect();
        let m = compute(&results);
        assert!((m.overall.recall_at_1 - 0.7).abs() < 1e-9);
        assert!((m.overall.building_group_recall_at_1 - 0.7).abs() < 1e-9);
        assert_eq!(m.overall.n, 10);
    }

    #[test]
    fn known_failure_excluded_from_aggregate() {
        let mut q_normal = synth_q("a", "bgid-1", "best-1", "flanders");
        let mut q_kf = synth_q("b", "bgid-99", "best-99", "flanders");
        q_normal.expected_to_fail = false;
        q_kf.expected_to_fail = true;
        // Mark the failing one as adversarial known_failure for the panel
        q_kf.source = EvalQuerySource::Adversarial {
            category: AdversarialCategory::KnownFailure,
            subcategory: "x".into(),
        };
        let results = vec![
            result_with_top(q_normal.clone(), "bgid-1", "best-1", "strict", 10.0),
            result_with_top(q_kf.clone(), "bgid-other", "best-other", "fuzzy", 10.0),
        ];
        let m = compute(&results);
        assert_eq!(m.aggregated_query_count, 1, "kf excluded");
        assert!((m.overall.recall_at_1 - 1.0).abs() < 1e-9);
        assert_eq!(m.known_failure_tracking.len(), 1);
        assert_eq!(m.known_failure_tracking[0].state, "still_failing");
    }

    #[test]
    fn known_failure_now_passing_when_expected_bgid_matches() {
        let mut q_kf = synth_q("kf", "bgid-X", "best-X", "wallonia");
        q_kf.expected_to_fail = true;
        q_kf.source = EvalQuerySource::Adversarial {
            category: AdversarialCategory::KnownFailure,
            subcategory: "x".into(),
        };
        // The expected bgid IS in top-1 → now_passing
        let results = vec![result_with_top(q_kf, "bgid-X", "best-X", "strict", 10.0)];
        let m = compute(&results);
        assert_eq!(m.known_failure_tracking[0].state, "now_passing");
    }

    #[test]
    fn mrr_uses_reciprocal_rank() {
        let q1 = synth_q("q1", "bgid-a", "best-a", "flanders");
        let q2 = synth_q("q2", "bgid-b", "best-b", "flanders");
        let r1 = QueryResult {
            query: q1,
            features: vec![("bgid-a".into(), "best-a".into(), "strict".into())], // rank 1
            points: Vec::new(),
            top_match_type: "strict".into(),
            raw_hits: 1,
            latency_ms: 10.0,
            error: None,
        };
        let r2 = QueryResult {
            query: q2,
            features: vec![
                ("other".into(), "other".into(), "strict".into()),
                ("other2".into(), "other2".into(), "strict".into()),
                ("bgid-b".into(), "best-b".into(), "strict".into()), // rank 3
            ],
            points: Vec::new(),
            top_match_type: "strict".into(),
            raw_hits: 3,
            latency_ms: 10.0,
            error: None,
        };
        let m = compute(&[r1, r2]);
        // MRR = (1/1 + 1/3) / 2 = 0.666...
        assert!((m.overall.mrr - (1.0 + 1.0 / 3.0) / 2.0).abs() < 1e-9);
        assert!((m.overall.recall_at_5 - 1.0).abs() < 1e-9);
        assert!((m.overall.recall_at_1 - 0.5).abs() < 1e-9);
    }

    #[test]
    fn percentile_picks_nearest_rank() {
        let mut samples = vec![10.0, 20.0, 30.0, 40.0, 50.0, 60.0, 70.0, 80.0, 90.0, 100.0];
        let b = compute_latency(&mut samples);
        assert_eq!(b.n, 10);
        // p50: index = ceil(0.5*10) - 1 = 4 → 50.0
        assert!((b.p50_ms - 50.0).abs() < 1e-9);
        // p95: index = ceil(0.95*10) - 1 = 9 → 100.0
        assert!((b.p95_ms - 100.0).abs() < 1e-9);
    }

    fn adv_q(id: &str, level: MatchLevel, bgid: &str, best: Option<&str>) -> EvalQuery {
        EvalQuery {
            id: id.into(),
            query_string: "q".into(),
            expected_bgid_set: vec![bgid.into()],
            expected_best_id_set: best.into_iter().map(String::from).collect(),
            expected_to_fail: false,
            match_level: level,
            expected_point: None,
            reference_point: None,
            structured: None,
            source: EvalQuerySource::Adversarial {
                category: crate::eval_set::AdversarialCategory::Facilites,
                subcategory: "s".into(),
            },
        }
    }

    #[test]
    fn match_levels_score_address_building_and_street() {
        let results = vec![
            // Address: neighbouring building on the same street → miss.
            result_with_top(
                adv_q("a", MatchLevel::Address, "ns|1|5", Some("unit-2")),
                "ns|1|7",
                "unit-1",
                "strict",
                1.0,
            ),
            // Building: same situation → hit.
            result_with_top(
                adv_q("b", MatchLevel::Building, "ns|1|5", Some("unit-2")),
                "ns|1|5",
                "unit-1",
                "strict",
                1.0,
            ),
            // Street: another number on the same street → hit.
            result_with_top(
                adv_q("c", MatchLevel::Street, "ns|1|5", None),
                "ns|1|99",
                "x",
                "strict",
                1.0,
            ),
            // Street: another street → miss.
            result_with_top(
                adv_q("d", MatchLevel::Street, "ns|1|5", None),
                "ns|2|5",
                "x",
                "strict",
                1.0,
            ),
        ];
        let m = compute(&results);
        let b = &m.by_adversarial_category["facilites"];
        assert_eq!(b.n, 4);
        assert!((b.recall_at_1 - 0.5).abs() < 1e-9, "{b:?}");
        assert_eq!(b.match_levels, ["address", "building", "street"]);
        assert_eq!(
            street_rank(&["ns|1|5".into()], &[("".into(), "".into(), "".into())]),
            0
        );
        assert_eq!(street_of("nopipe"), "nopipe");
    }

    #[test]
    fn distance_metrics_over_reference_points() {
        // Four queries with a reference point at (50.0, 4.0): answers at
        // ~0, ~55, ~222 m and none; one more without a reference point.
        let mut results = Vec::new();
        for (i, lat) in [Some(50.0), Some(50.0005), Some(50.002), None]
            .into_iter()
            .enumerate()
        {
            let mut q = synth_q(&format!("d{i}"), "b", "x", "brussels");
            q.reference_point = Some((50.0, 4.0));
            let mut r = result_with_top(q, "b", "x", "strict", 1.0);
            r.points = vec![lat.map(|lat| (lat, 4.0))];
            results.push(r);
        }
        results.push(result_with_top(
            synth_q("none", "b", "x", "brussels"),
            "b",
            "x",
            "strict",
            1.0,
        ));
        let m = compute(&results);
        let d = &m.distance;
        assert_eq!(d.n, 4);
        assert!((d.found_rate - 0.75).abs() < 1e-9);
        assert!((d.within_25m - 0.25).abs() < 1e-9);
        assert!((d.within_100m - 0.5).abs() < 1e-9);
        assert!((d.within_250m - 0.75).abs() < 1e-9);
        assert!((d.median_error_m.unwrap() - 55.6).abs() < 1.0, "{d:?}");
        assert!((d.p90_error_m.unwrap() - 222.4).abs() < 1.0, "{d:?}");
        assert_eq!(m.distance_by["synthetic"], *d);
        assert_eq!(m.distance_by["region:brussels"].n, 4);
        assert_eq!(distance_bucket(&[]), DistanceBucket::default());
        let none = distance_bucket(&[None]);
        assert_eq!((none.found_rate, none.median_error_m), (0.0, None));
    }

    #[test]
    fn realworld_queries_get_their_own_buckets() {
        let mut q = adv_q("rw", MatchLevel::Locality, "ns|1|5", None);
        q.source = EvalQuerySource::RealWorld {
            source: "brussels-schools".into(),
            region: "brussels".into(),
        };
        q.expected_point = Some(((50.0, 4.0), 100.0));
        q.reference_point = Some((50.0, 4.0));
        let m = compute(&[result_with_top(q, "ns|9|1", "x", "strict", 1.0)]);
        let b = &m.by_realworld_source["brussels-schools"];
        assert_eq!((b.n, b.recall_at_1), (1, 1.0));
        assert_eq!(b.match_levels, ["locality"]);
        assert_eq!(m.distance_by["realworld:brussels-schools"].within_25m, 1.0);
        assert_eq!(m.distance_by["realworld"].n, 1);
        assert!(m.by_region.is_empty(), "real-world regions stay apart");
    }

    #[test]
    fn locality_level_scores_by_distance() {
        let mut q = adv_q("l", MatchLevel::Locality, "ns|1|5", None);
        // `result_with_top` puts the feature at (50.0, 4.0).
        q.expected_point = Some(((50.01, 4.0), 1_500.0)); // ~1.1 km away
        let near = result_with_top(q.clone(), "ns|9|1", "x", "strict", 1.0);
        assert_eq!(level_ranks(&near), (1, 0));
        q.expected_point = Some(((50.02, 4.0), 1_500.0)); // ~2.2 km away
        let far = result_with_top(q.clone(), "ns|9|1", "x", "strict", 1.0);
        assert_eq!(level_ranks(&far), (0, 0));
        q.expected_point = None;
        let unset = result_with_top(q, "ns|9|1", "x", "strict", 1.0);
        assert_eq!(level_ranks(&unset).0, 0);
        assert!((haversine_m((50.0, 4.0), (50.01, 4.0)) - 1_112.0).abs() < 2.0);
        assert_eq!(
            within_rank((50.0, 4.0), 10.0, &[None, Some((50.0, 4.0))]),
            2
        );
    }
}
