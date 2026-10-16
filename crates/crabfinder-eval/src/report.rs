//! Report writers (file 14 §4) + minimal diff (file 14 §5, MVP scope).
//!
//! Three artifacts written per full run:
//!   - {output}.json       — metrics-only, git-diffable (~10 KB)
//!   - {output}-detail.json.gz — per-query results, CI artifact only
//!   - {output}.md         — Markdown summary (--output-md)
//!
//! The split keeps repo size lean while preserving full per-query provenance
//! (file 14 §4 "Artifact split").

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use flate2::Compression;
use flate2::write::GzEncoder;
use serde::{Deserialize, Serialize};

use crate::metrics::{DistanceBucket, LatencyBucket, MetricBucket, Metrics};
use crate::runner::QueryResult;

#[derive(Debug, Serialize, Deserialize)]
pub struct Report {
    pub report_version: String,
    pub service_url: String,
    pub metrics: Metrics,
    pub wall_clock_seconds: f64,
}

#[derive(Debug, Serialize, Deserialize)]
struct DetailRow {
    id: String,
    expected_bgid_set_len: usize,
    expected_best_id_set_len: usize,
    expected_to_fail: bool,
    actual_top1_bgid: String,
    actual_top1_best_id: String,
    actual_top1_match_type: String,
    raw_hits: usize,
    latency_ms: f64,
    error: Option<String>,
    /// Rank at the query's match level (`metrics::level_ranks`), 1-based;
    /// 0 = not in the returned features.
    #[serde(default)]
    level_rank: usize,
}

pub fn write_reports(
    metrics: &Metrics,
    results: &[QueryResult],
    service_url: &str,
    output_json: &Path,
    output_md: Option<&Path>,
) -> Result<()> {
    let wall_clock = results.iter().map(|r| r.latency_ms).sum::<f64>() / 1000.0 / 8.0_f64.max(1.0); // approximate; better than 0
    let report = Report {
        report_version: "1".to_string(),
        service_url: service_url.to_string(),
        metrics: metrics.clone(),
        wall_clock_seconds: wall_clock,
    };

    if let Some(parent) = output_json.parent() {
        fs::create_dir_all(parent).ok();
    }

    // 1. Main JSON (metrics-only)
    let json = serde_json::to_string_pretty(&report).context("serialising report JSON")?;
    fs::write(output_json, json).context("writing report JSON")?;

    // 2. Detail JSON.gz alongside
    let detail_path = detail_path_for(output_json);
    let detail_rows: Vec<DetailRow> = results
        .iter()
        .map(|r| {
            let top = r.features.first().cloned().unwrap_or_default();
            DetailRow {
                id: r.query.id.clone(),
                expected_bgid_set_len: r.query.expected_bgid_set.len(),
                expected_best_id_set_len: r.query.expected_best_id_set.len(),
                expected_to_fail: r.query.expected_to_fail,
                actual_top1_bgid: top.0,
                actual_top1_best_id: top.1,
                actual_top1_match_type: top.2,
                raw_hits: r.raw_hits,
                latency_ms: r.latency_ms,
                error: r.error.clone(),
                level_rank: crate::metrics::level_ranks(r).0,
            }
        })
        .collect();
    let detail_json = serde_json::to_string(&detail_rows).context("serialising detail rows")?;
    let detail_file =
        fs::File::create(&detail_path).with_context(|| format!("creating {detail_path:?}"))?;
    let mut encoder = GzEncoder::new(detail_file, Compression::default());
    encoder
        .write_all(detail_json.as_bytes())
        .context("gzip write")?;
    encoder.finish().context("gzip finish")?;

    // 3. Markdown summary
    if let Some(md) = output_md {
        let body = render_report_md(metrics, service_url, results.len());
        if let Some(parent) = md.parent() {
            fs::create_dir_all(parent).ok();
        }
        fs::write(md, body).with_context(|| format!("writing {md:?}"))?;
    }

    Ok(())
}

fn detail_path_for(output_json: &Path) -> PathBuf {
    let stem = output_json
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("report");
    let parent = output_json.parent().unwrap_or(Path::new("."));
    parent.join(format!("{stem}-detail.json.gz"))
}

// --- Markdown rendering -----------------------------------------------------

pub(crate) fn render_report_md(m: &Metrics, service_url: &str, n: usize) -> String {
    let mut s = String::new();
    s.push_str("# crabfinder eval: single report\n\n");
    s.push_str(&format!("- Service: `{service_url}`\n"));
    s.push_str(&format!("- Total queries: **{n}**\n"));
    s.push_str(&format!(
        "- Aggregated queries (excludes known_failure): {}\n",
        m.aggregated_query_count
    ));
    s.push_str(&format!("- Transport/HTTP errors: {}\n\n", m.error_count));

    s.push_str("## Overall (Tier A)\n\n");
    bucket_table(&mut s, &m.overall);

    s.push_str("\n## Latency (HTTP mode; under contention)\n\n");
    latency_table(&mut s, &m.latency);
    s.push_str(
        "\n_Methodology: HTTP loopback. Loopback adds ~0.5–1 ms/query; \
significant on sub-3 ms strict queries, negligible on fuzzy. Measures \
user-perceived end-to-end latency, not search-engine latency._\n",
    );

    s.push_str("\n## §3.2 contract: cross_language_recall@1\n\n");
    s.push_str(&format!(
        "- Random samples (n={}): **{:.3}**\n",
        m.cross_language_n, m.cross_language_recall_at_1
    ));
    s.push_str(&format!(
        "- Boosted samples (n={}): **{:.3}**\n",
        m.cross_language_boosted_n, m.cross_language_recall_at_1_boosted
    ));

    s.push_str("\n## By region\n\n");
    region_variant_table(&mut s, &m.by_region, "Region");

    s.push_str("\n## By query variant\n\n");
    region_variant_table(&mut s, &m.by_variant, "Variant");

    s.push_str("\n## By street primary language\n\n");
    region_variant_table(&mut s, &m.by_language, "Language");

    s.push_str("\n## Adversarial: by category\n\n");
    region_variant_table(&mut s, &m.by_adversarial_category, "Category");

    if !m.alternatives.is_empty() {
        s.push_str(
            "\n## Alternatives by municipality (`--alternatives`)\n\n\
             The expected building is the best answer of one of the first k municipality groups.\n\n\
             | Variant | n | recall@1 | recall@3 |\n|---|---|---|---|\n",
        );
        let mut keys: Vec<&String> = m.alternatives.keys().collect();
        keys.sort_by_key(|k| (k.as_str() == "all", k.as_str()));
        for k in keys {
            let b = &m.alternatives[k];
            s.push_str(&format!(
                "| {k} | {} | {:.3} | {:.3} |\n",
                b.n, b.recall_at_1, b.recall_at_3
            ));
        }
    }

    if !m.by_realworld_source.is_empty() {
        s.push_str("\n## Real-world: by dataset\n\n");
        region_variant_table(&mut s, &m.by_realworld_source, "Dataset");
    }

    if m.distance.n > 0 {
        s.push_str("\n## Distance to the reference point (top-1)\n\n");
        distance_table(&mut s, &m.distance, &m.distance_by);
    }

    s.push_str("\n## Known-failure tracking\n\n");
    if m.known_failure_tracking.is_empty() {
        s.push_str("_(no known_failure entries)_\n");
    } else {
        s.push_str("| ID | Subcategory | State | Top-1 (bgid prefix) | Top-1 match_type |\n");
        s.push_str("|---|---|---|---|---|\n");
        for kf in &m.known_failure_tracking {
            let bgid_short = if kf.current_top1_bgid.len() > 50 {
                format!("{}…", &kf.current_top1_bgid[..50])
            } else {
                kf.current_top1_bgid.clone()
            };
            s.push_str(&format!(
                "| `{}` | {} | **{}** | `{}` | {} |\n",
                kf.id, kf.subcategory, kf.state, bgid_short, kf.current_top1_match_type
            ));
        }
    }
    s
}

/// Header of a distance table (`distance_row` fills it).
pub(crate) const DISTANCE_HEADER: &str = "| n | found | ≤25 m | ≤100 m | ≤250 m | median | p90 |";

/// One distance table row's cells after the label.
pub(crate) fn distance_row(d: &DistanceBucket) -> String {
    let m = |x: Option<f64>| x.map_or("–".to_string(), |x| format!("{x:.0} m"));
    format!(
        "{} | {:.3} | {:.3} | {:.3} | {:.3} | {} | {} |",
        d.n,
        d.found_rate,
        d.within_25m,
        d.within_100m,
        d.within_250m,
        m(d.median_error_m),
        m(d.p90_error_m)
    )
}

fn distance_table(
    s: &mut String,
    overall: &DistanceBucket,
    by: &std::collections::HashMap<String, DistanceBucket>,
) {
    s.push_str(&format!("| Slice {DISTANCE_HEADER}\n"));
    s.push_str("|---|---|---|---|---|---|---|---|\n");
    s.push_str(&format!("| **all** | {}\n", distance_row(overall)));
    let mut entries: Vec<_> = by.iter().collect();
    entries.sort_by(|a, b| a.0.cmp(b.0));
    for (k, d) in entries {
        s.push_str(&format!("| {k} | {}\n", distance_row(d)));
    }
}

fn bucket_table(s: &mut String, b: &MetricBucket) {
    s.push_str("| Metric | Value |\n|---|---|\n");
    s.push_str(&format!("| n | {} |\n", b.n));
    s.push_str(&format!("| recall@1 | {:.3} |\n", b.recall_at_1));
    s.push_str(&format!("| recall@5 | {:.3} |\n", b.recall_at_5));
    s.push_str(&format!("| recall@10 | {:.3} |\n", b.recall_at_10));
    s.push_str(&format!(
        "| building_group_recall@1 | {:.3} |\n",
        b.building_group_recall_at_1
    ));
    s.push_str(&format!("| MRR | {:.3} |\n", b.mrr));
    s.push_str(&format!(
        "| strict_match_rate | {:.3} |\n",
        b.strict_match_rate
    ));
}

fn latency_table(s: &mut String, l: &LatencyBucket) {
    s.push_str("| | n | p50 | p95 | p99 | mean | max |\n|---|---|---|---|---|---|---|\n");
    s.push_str(&format!(
        "| ms | {} | {:.1} | {:.1} | {:.1} | {:.1} | {:.1} |\n",
        l.n, l.p50_ms, l.p95_ms, l.p99_ms, l.mean_ms, l.max_ms
    ));
}

fn region_variant_table(
    s: &mut String,
    map: &std::collections::HashMap<String, MetricBucket>,
    header: &str,
) {
    s.push_str(&format!(
        "| {header} | n | recall@1 | recall@5 | bgid_recall@1 | MRR | strict |\n"
    ));
    s.push_str("|---|---|---|---|---|---|---|\n");
    let mut entries: Vec<_> = map.iter().collect();
    entries.sort_by(|a, b| a.0.cmp(b.0));
    for (k, b) in entries {
        // Adversarial categories show which match levels they are scored at.
        let k = if b.match_levels.is_empty() {
            k.clone()
        } else {
            format!("{k} ({})", b.match_levels.join(", "))
        };
        s.push_str(&format!(
            "| {k} | {} | {:.3} | {:.3} | {:.3} | {:.3} | {:.3} |\n",
            b.n,
            b.recall_at_1,
            b.recall_at_5,
            b.building_group_recall_at_1,
            b.mrr,
            b.strict_match_rate
        ));
    }
}

// --- Minimal --diff (Step 8) -----------------------------------------------

pub fn write_diff(baseline_path: &Path, current_path: &Path, output_md: &Path) -> Result<()> {
    let baseline: Report = serde_json::from_str(&fs::read_to_string(baseline_path)?)
        .with_context(|| format!("loading baseline {baseline_path:?}"))?;
    let current: Report = serde_json::from_str(&fs::read_to_string(current_path)?)
        .with_context(|| format!("loading current {current_path:?}"))?;

    let mut s = String::new();
    s.push_str(&format!(
        "# Eval diff: `{}` → `{}`\n\n",
        baseline_path.display(),
        current_path.display()
    ));
    s.push_str("_Minimal MVP diff. Per-query regression list deferred to Phase 6.5b._\n\n");

    s.push_str("## Overall (Tier A): Δ\n\n");
    bucket_diff_table(&mut s, &baseline.metrics.overall, &current.metrics.overall);

    s.push_str("\n## §3.2 contract: cross_language_recall@1: Δ\n\n");
    s.push_str(&format!(
        "| Pool | Baseline | Current | Δ |\n|---|---|---|---|\n\
| random   (n={base_n}/{cur_n}) | {:.3} | {:.3} | {:+.3} |\n\
| boosted  (n={base_bn}/{cur_bn}) | {:.3} | {:.3} | {:+.3} |\n",
        baseline.metrics.cross_language_recall_at_1,
        current.metrics.cross_language_recall_at_1,
        current.metrics.cross_language_recall_at_1 - baseline.metrics.cross_language_recall_at_1,
        baseline.metrics.cross_language_recall_at_1_boosted,
        current.metrics.cross_language_recall_at_1_boosted,
        current.metrics.cross_language_recall_at_1_boosted
            - baseline.metrics.cross_language_recall_at_1_boosted,
        base_n = baseline.metrics.cross_language_n,
        cur_n = current.metrics.cross_language_n,
        base_bn = baseline.metrics.cross_language_boosted_n,
        cur_bn = current.metrics.cross_language_boosted_n,
    ));

    s.push_str("\n## Latency: Δ\n\n");
    s.push_str("| | baseline | current | Δ |\n|---|---|---|---|\n");
    s.push_str(&format!(
        "| p50  | {:.1} ms | {:.1} ms | {:+.1} ms |\n",
        baseline.metrics.latency.p50_ms,
        current.metrics.latency.p50_ms,
        current.metrics.latency.p50_ms - baseline.metrics.latency.p50_ms
    ));
    s.push_str(&format!(
        "| p95  | {:.1} ms | {:.1} ms | {:+.1} ms |\n",
        baseline.metrics.latency.p95_ms,
        current.metrics.latency.p95_ms,
        current.metrics.latency.p95_ms - baseline.metrics.latency.p95_ms
    ));
    s.push_str(&format!(
        "| p99  | {:.1} ms | {:.1} ms | {:+.1} ms |\n",
        baseline.metrics.latency.p99_ms,
        current.metrics.latency.p99_ms,
        current.metrics.latency.p99_ms - baseline.metrics.latency.p99_ms
    ));

    s.push_str("\n## By region: Δ\n\n");
    map_diff_table(
        &mut s,
        &baseline.metrics.by_region,
        &current.metrics.by_region,
        "Region",
    );

    s.push_str("\n## By variant: Δ\n\n");
    map_diff_table(
        &mut s,
        &baseline.metrics.by_variant,
        &current.metrics.by_variant,
        "Variant",
    );

    s.push_str("\n## Adversarial: by category: Δ\n\n");
    map_diff_table(
        &mut s,
        &baseline.metrics.by_adversarial_category,
        &current.metrics.by_adversarial_category,
        "Category",
    );

    if !base_or_cur_empty(
        &baseline.metrics.by_realworld_source,
        &current.metrics.by_realworld_source,
    ) {
        s.push_str("\n## Real-world: by dataset: Δ\n\n");
        map_diff_table(
            &mut s,
            &baseline.metrics.by_realworld_source,
            &current.metrics.by_realworld_source,
            "Dataset",
        );
    }

    if baseline.metrics.distance.n + current.metrics.distance.n > 0 {
        s.push_str("\n## Distance to the reference point: Δ\n\n");
        s.push_str(&format!("| Report {DISTANCE_HEADER}\n"));
        s.push_str("|---|---|---|---|---|---|---|---|\n");
        s.push_str(&format!(
            "| baseline | {}\n",
            distance_row(&baseline.metrics.distance)
        ));
        s.push_str(&format!(
            "| current | {}\n",
            distance_row(&current.metrics.distance)
        ));
    }

    s.push_str("\n## Known-failure state changes\n\n");
    let mut base_state: std::collections::HashMap<&str, &str> = std::collections::HashMap::new();
    for kf in &baseline.metrics.known_failure_tracking {
        base_state.insert(&kf.id, &kf.state);
    }
    let mut changes = 0usize;
    for kf in &current.metrics.known_failure_tracking {
        let was = base_state.get(kf.id.as_str()).copied().unwrap_or("(new)");
        if was != kf.state {
            s.push_str(&format!(
                "- `{}` ({}): **{} → {}**\n",
                kf.id, kf.subcategory, was, kf.state
            ));
            changes += 1;
        }
    }
    if changes == 0 {
        s.push_str(
            "_No state changes; all known_failure entries continue to behave as documented._\n",
        );
    }

    if let Some(parent) = output_md.parent() {
        fs::create_dir_all(parent).ok();
    }
    fs::write(output_md, s).with_context(|| format!("writing {output_md:?}"))?;
    Ok(())
}

fn base_or_cur_empty(
    base: &std::collections::HashMap<String, MetricBucket>,
    cur: &std::collections::HashMap<String, MetricBucket>,
) -> bool {
    base.is_empty() && cur.is_empty()
}

fn bucket_diff_table(s: &mut String, base: &MetricBucket, cur: &MetricBucket) {
    s.push_str("| Metric | baseline | current | Δ |\n|---|---|---|---|\n");
    let pairs: &[(&str, f64, f64)] = &[
        ("recall@1", base.recall_at_1, cur.recall_at_1),
        ("recall@5", base.recall_at_5, cur.recall_at_5),
        ("recall@10", base.recall_at_10, cur.recall_at_10),
        (
            "bgid_recall@1",
            base.building_group_recall_at_1,
            cur.building_group_recall_at_1,
        ),
        ("MRR", base.mrr, cur.mrr),
        (
            "strict_match_rate",
            base.strict_match_rate,
            cur.strict_match_rate,
        ),
    ];
    for (k, b, c) in pairs {
        s.push_str(&format!("| {k} | {:.3} | {:.3} | {:+.3} |\n", b, c, c - b));
    }
    s.push_str(&format!(
        "| n | {} | {} | {} |\n",
        base.n,
        cur.n,
        cur.n as i64 - base.n as i64
    ));
}

fn map_diff_table(
    s: &mut String,
    base: &std::collections::HashMap<String, MetricBucket>,
    cur: &std::collections::HashMap<String, MetricBucket>,
    header: &str,
) {
    let mut keys: std::collections::BTreeSet<&str> = std::collections::BTreeSet::new();
    for k in base.keys() {
        keys.insert(k);
    }
    for k in cur.keys() {
        keys.insert(k);
    }
    s.push_str(&format!(
        "| {header} | n base/cur | recall@1 base/cur (Δ) | MRR base/cur (Δ) | strict base/cur (Δ) |\n"
    ));
    s.push_str("|---|---|---|---|---|\n");
    for k in keys {
        let b = base.get(k).cloned().unwrap_or_default();
        let c = cur.get(k).cloned().unwrap_or_default();
        s.push_str(&format!(
            "| {k} | {}/{} | {:.3}/{:.3} ({:+.3}) | {:.3}/{:.3} ({:+.3}) | {:.3}/{:.3} ({:+.3}) |\n",
            b.n,
            c.n,
            b.recall_at_1,
            c.recall_at_1,
            c.recall_at_1 - b.recall_at_1,
            b.mrr,
            c.mrr,
            c.mrr - b.mrr,
            b.strict_match_rate,
            c.strict_match_rate,
            c.strict_match_rate - b.strict_match_rate,
        ));
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;
    use crate::metrics::KnownFailureEntry;

    fn bucket(n: usize, r1: f64, levels: &[&str]) -> MetricBucket {
        MetricBucket {
            n,
            recall_at_1: r1,
            recall_at_5: (r1 + 0.05).min(1.0),
            recall_at_10: (r1 + 0.1).min(1.0),
            mrr: r1 * 0.98,
            building_group_recall_at_1: r1,
            strict_match_rate: 0.75,
            match_levels: levels.iter().map(|l| l.to_string()).collect(),
        }
    }

    fn kf(id: &str, state: &str, bgid: &str) -> KnownFailureEntry {
        KnownFailureEntry {
            id: id.into(),
            subcategory: "sub".into(),
            state: state.into(),
            current_top1_bgid: bgid.into(),
            current_top1_match_type: "fuzzy".into(),
        }
    }

    fn metrics(r1: f64, kfs: Vec<KnownFailureEntry>, extra_variant: bool) -> Metrics {
        let mut by_variant = HashMap::from([("canonical".to_string(), bucket(40, r1, &[]))]);
        if extra_variant {
            by_variant.insert("typo".into(), bucket(10, 0.5, &[]));
        }
        Metrics {
            query_count: 61,
            aggregated_query_count: 60,
            error_count: 0,
            overall: bucket(60, r1, &[]),
            latency: LatencyBucket {
                n: 61,
                p50_ms: 1.25,
                p95_ms: 4.5,
                p99_ms: 9.0,
                mean_ms: 2.0,
                max_ms: 12.0,
            },
            by_region: HashMap::from([
                ("BE-BRU".to_string(), bucket(20, r1, &[])),
                ("BE-VLG".to_string(), bucket(20, 1.0, &[])),
            ]),
            by_variant,
            by_language: HashMap::from([("nl".to_string(), bucket(30, r1, &[]))]),
            by_adversarial_category: HashMap::from([
                (
                    "hamlet".to_string(),
                    bucket(5, 0.8, &["locality", "street"]),
                ),
                ("notability".to_string(), bucket(5, 0.6, &["building"])),
            ]),
            by_realworld_source: if extra_variant {
                HashMap::from([(
                    "brussels-schools".to_string(),
                    bucket(10, 0.7, &["locality"]),
                )])
            } else {
                HashMap::new()
            },
            distance: if extra_variant {
                DistanceBucket {
                    n: 10,
                    found_rate: 1.0,
                    within_25m: 0.6,
                    within_100m: 0.9,
                    within_250m: 1.0,
                    median_error_m: Some(12.0),
                    p90_error_m: None,
                }
            } else {
                DistanceBucket::default()
            },
            distance_by: if extra_variant {
                HashMap::from([("realworld".to_string(), DistanceBucket::default())])
            } else {
                HashMap::new()
            },
            cross_language_recall_at_1: r1,
            cross_language_n: 12,
            cross_language_recall_at_1_boosted: 0.9,
            cross_language_boosted_n: 8,
            known_failure_tracking: kfs,
            alternatives: HashMap::new(),
        }
    }

    #[test]
    fn report_markdown_snapshot() {
        let long_bgid = format!("https://databrussels.be/id/streetname|{}|1", "9".repeat(40));
        let m = metrics(
            0.9,
            vec![
                kf("adv-known_failure-001", "still_failing", "short|1|1"),
                kf("adv-known_failure-002", "now_passing", &long_bgid),
            ],
            false,
        );
        insta::assert_snapshot!(render_report_md(&m, "http://127.0.0.1:8080", 61));
    }

    #[test]
    fn report_markdown_without_known_failures() {
        let md = render_report_md(&metrics(0.9, vec![], false), "u", 60);
        assert!(md.contains("_(no known_failure entries)_"));
        assert!(!md.contains("Real-world"));
        assert!(!md.contains("Distance"));
    }

    #[test]
    fn report_markdown_with_realworld_and_distances() {
        let md = render_report_md(&metrics(0.9, vec![], true), "u", 70);
        assert!(
            md.contains("| brussels-schools (locality) | 10 | 0.700 |"),
            "{md}"
        );
        assert!(
            md.contains("| **all** | 10 | 1.000 | 0.600 | 0.900 | 1.000 | 12 m | – |"),
            "{md}"
        );
        assert!(md.contains("| realworld | 0 |"), "{md}");
    }

    fn write_report(dir: &Path, name: &str, m: Metrics) -> PathBuf {
        let p = dir.join(name);
        let r = Report {
            report_version: "1".into(),
            service_url: "u".into(),
            metrics: m,
            wall_clock_seconds: 1.0,
        };
        fs::write(&p, serde_json::to_string(&r).unwrap()).unwrap();
        p
    }

    #[test]
    fn diff_shows_deltas_new_buckets_and_state_changes() {
        let tmp = tempfile::TempDir::new().unwrap();
        let base = write_report(
            tmp.path(),
            "base.json",
            metrics(
                0.8,
                vec![kf("adv-known_failure-001", "still_failing", "x")],
                false,
            ),
        );
        let cur = write_report(
            tmp.path(),
            "cur.json",
            metrics(
                0.9,
                vec![
                    kf("adv-known_failure-001", "now_passing", "x"),
                    kf("adv-known_failure-009", "still_failing", "y"),
                ],
                true,
            ),
        );
        let out = tmp.path().join("nested/diff.md");
        write_diff(&base, &cur, &out).unwrap();
        let d = fs::read_to_string(&out).unwrap();
        assert!(d.contains("| recall@1 | 0.800 | 0.900 | +0.100 |"), "{d}");
        assert!(d.contains("| typo | 0/10 |"), "{d}");
        assert!(d.contains("**still_failing → now_passing**"), "{d}");
        assert!(d.contains("**(new) → still_failing**"), "{d}");
        assert!(d.contains("| brussels-schools | 0/10 |"), "{d}");
        assert!(d.contains("| current | 10 | 1.000 |"), "{d}");

        write_diff(&base, &base, &out).unwrap();
        assert!(
            fs::read_to_string(&out)
                .unwrap()
                .contains("_No state changes")
        );
    }

    #[test]
    fn diff_rejects_unreadable_reports() {
        let tmp = tempfile::TempDir::new().unwrap();
        let bad = tmp.path().join("bad.json");
        fs::write(&bad, "{}").unwrap();
        let good = write_report(tmp.path(), "good.json", metrics(0.8, vec![], false));
        let out = tmp.path().join("d.md");
        let err = write_diff(&bad, &good, &out).unwrap_err();
        assert!(format!("{err:#}").contains("loading baseline"), "{err:#}");
        let err = write_diff(&good, &bad, &out).unwrap_err();
        assert!(format!("{err:#}").contains("loading current"), "{err:#}");
        assert!(write_diff(&tmp.path().join("missing.json"), &good, &out).is_err());
    }

    #[test]
    fn detail_path_sits_next_to_the_report() {
        assert_eq!(
            detail_path_for(Path::new("reports/x.json")),
            Path::new("reports/x-detail.json.gz")
        );
        assert_eq!(
            detail_path_for(Path::new("r")),
            Path::new("r-detail.json.gz")
        );
    }
}
