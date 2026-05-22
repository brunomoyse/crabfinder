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

use crate::metrics::{Metrics, MetricBucket, LatencyBucket};
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
}

pub fn write_reports(
    metrics: &Metrics,
    results: &[QueryResult],
    service_url: &str,
    output_json: &Path,
    output_md: Option<&Path>,
) -> Result<()> {
    let wall_clock = results.iter().map(|r| r.latency_ms).sum::<f64>() / 1000.0
        / 8.0_f64.max(1.0); // approximate; better than 0
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
            }
        })
        .collect();
    let detail_json = serde_json::to_string(&detail_rows).context("serialising detail rows")?;
    let detail_file = fs::File::create(&detail_path)
        .with_context(|| format!("creating {detail_path:?}"))?;
    let mut encoder = GzEncoder::new(detail_file, Compression::default());
    encoder.write_all(detail_json.as_bytes()).context("gzip write")?;
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
    let stem = output_json.file_stem().and_then(|s| s.to_str()).unwrap_or("report");
    let parent = output_json.parent().unwrap_or(Path::new("."));
    parent.join(format!("{stem}-detail.json.gz"))
}

// --- Markdown rendering -----------------------------------------------------

fn render_report_md(m: &Metrics, service_url: &str, n: usize) -> String {
    let mut s = String::new();
    s.push_str(&format!("# crabfinder eval — single report\n\n"));
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
        "\n_Methodology: HTTP loopback. Loopback adds ~0.5–1 ms/query — \
significant on sub-3 ms strict queries, negligible on fuzzy. Measures \
user-perceived end-to-end latency, not search-engine latency._\n",
    );

    s.push_str("\n## §3.2 contract — cross_language_recall@1\n\n");
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

    s.push_str("\n## Adversarial — by category\n\n");
    region_variant_table(&mut s, &m.by_adversarial_category, "Category");

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

fn bucket_table(s: &mut String, b: &MetricBucket) {
    s.push_str("| Metric | Value |\n|---|---|\n");
    s.push_str(&format!("| n | {} |\n", b.n));
    s.push_str(&format!("| recall@1 | {:.3} |\n", b.recall_at_1));
    s.push_str(&format!("| recall@5 | {:.3} |\n", b.recall_at_5));
    s.push_str(&format!("| recall@10 | {:.3} |\n", b.recall_at_10));
    s.push_str(&format!("| building_group_recall@1 | {:.3} |\n", b.building_group_recall_at_1));
    s.push_str(&format!("| MRR | {:.3} |\n", b.mrr));
    s.push_str(&format!("| strict_match_rate | {:.3} |\n", b.strict_match_rate));
}

fn latency_table(s: &mut String, l: &LatencyBucket) {
    s.push_str("| | n | p50 | p95 | p99 | mean | max |\n|---|---|---|---|---|---|---|\n");
    s.push_str(&format!(
        "| ms | {} | {:.1} | {:.1} | {:.1} | {:.1} | {:.1} |\n",
        l.n, l.p50_ms, l.p95_ms, l.p99_ms, l.mean_ms, l.max_ms
    ));
}

fn region_variant_table(s: &mut String, map: &std::collections::HashMap<String, MetricBucket>, header: &str) {
    s.push_str(&format!(
        "| {header} | n | recall@1 | recall@5 | bgid_recall@1 | MRR | strict |\n"
    ));
    s.push_str("|---|---|---|---|---|---|---|\n");
    let mut entries: Vec<_> = map.iter().collect();
    entries.sort_by(|a, b| a.0.cmp(b.0));
    for (k, b) in entries {
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
        "# Eval diff — `{}` → `{}`\n\n",
        baseline_path.display(),
        current_path.display()
    ));
    s.push_str(&format!("_Minimal MVP diff. Per-query regression list deferred to Phase 6.5b._\n\n"));

    s.push_str("## Overall (Tier A) — Δ\n\n");
    bucket_diff_table(&mut s, &baseline.metrics.overall, &current.metrics.overall);

    s.push_str("\n## §3.2 contract — cross_language_recall@1 — Δ\n\n");
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

    s.push_str("\n## Latency — Δ\n\n");
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

    s.push_str("\n## By region — Δ\n\n");
    map_diff_table(&mut s, &baseline.metrics.by_region, &current.metrics.by_region, "Region");

    s.push_str("\n## By variant — Δ\n\n");
    map_diff_table(&mut s, &baseline.metrics.by_variant, &current.metrics.by_variant, "Variant");

    s.push_str("\n## Adversarial — by category — Δ\n\n");
    map_diff_table(
        &mut s,
        &baseline.metrics.by_adversarial_category,
        &current.metrics.by_adversarial_category,
        "Category",
    );

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
        s.push_str("_No state changes — all known_failure entries continue to behave as documented._\n");
    }

    if let Some(parent) = output_md.parent() {
        fs::create_dir_all(parent).ok();
    }
    fs::write(output_md, s).with_context(|| format!("writing {output_md:?}"))?;
    Ok(())
}

fn bucket_diff_table(s: &mut String, base: &MetricBucket, cur: &MetricBucket) {
    s.push_str("| Metric | baseline | current | Δ |\n|---|---|---|---|\n");
    let pairs: &[(&str, f64, f64)] = &[
        ("recall@1", base.recall_at_1, cur.recall_at_1),
        ("recall@5", base.recall_at_5, cur.recall_at_5),
        ("recall@10", base.recall_at_10, cur.recall_at_10),
        ("bgid_recall@1", base.building_group_recall_at_1, cur.building_group_recall_at_1),
        ("MRR", base.mrr, cur.mrr),
        ("strict_match_rate", base.strict_match_rate, cur.strict_match_rate),
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

// Helper for handling missing buckets in diff
impl MetricBucket {
    // (cloned for diff fill — using Default already; nothing extra needed)
}

// Used in the diff aggregator
#[allow(dead_code)]
fn safe_get<'a>(map: &'a std::collections::HashMap<String, MetricBucket>, k: &str) -> Option<&'a MetricBucket> {
    map.get(k)
}
