//! Competitor benchmark (v3 Phase 11.1): the same eval queries through
//! several geocoders (`adapters`), each scored with the usual metrics on
//! the queries it covers, plus distance to the reference point — the one
//! measure every geocoder can be held to. Writes `<output>.json` (per
//! geocoder metrics, head-to-head counts, the queries crabfinder loses),
//! `<output>-detail.json.gz` (per query, per geocoder) and a Markdown
//! report.
//!
//! The aim is an honest measurement, not a win: the losses list feeds
//! the ranking backlog.

use std::collections::{BTreeSet, HashMap};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result, bail};
use flate2::Compression;
use flate2::write::GzEncoder;
use serde::{Deserialize, Serialize};
use tracing::info;

use crate::adapters::{self, BuildOptions, Geocoder, GeocoderSpec};
use crate::eval_set::EvalQuery;
use crate::metrics::{self, Metrics};
use crate::report::{DISTANCE_HEADER, distance_row};
use crate::runner::{QueryResult, RunnerConfig};

/// A top-1 this close to the reference point is "right" for the
/// head-to-head and the losses list.
pub const HIT_M: f64 = 100.0;

pub struct BenchmarkConfig {
    pub geocoders: Vec<GeocoderSpec>,
    pub build: BuildOptions,
    /// Per-label requests per second, overriding `adapters::default_rate`.
    pub rates: HashMap<String, f64>,
    pub limit: usize,
    pub parallel: usize,
    /// Fill reference points from this crabfinder service (`--with-points`).
    pub points_from: Option<String>,
    pub output: PathBuf,
    pub output_md: Option<PathBuf>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GeocoderReport {
    pub name: String,
    pub kind: String,
    /// Queries it was asked (its coverage).
    pub covered: usize,
    pub errors: usize,
    /// Whether it returned BeSt ids at all: recall is meaningless
    /// otherwise.
    pub returns_best_ids: bool,
    pub metrics: Metrics,
}

/// crabfinder against one other geocoder, on the queries both were asked
/// that have a reference point.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct HeadToHead {
    pub other: String,
    pub common: usize,
    pub both: usize,
    pub crabfinder_only: usize,
    pub other_only: usize,
    pub neither: usize,
}

/// A query crabfinder misses (top-1 farther than `HIT_M`, or nothing)
/// that another geocoder gets.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Loss {
    pub id: String,
    pub query: String,
    pub crabfinder_error_m: Option<f64>,
    /// `(geocoder, error_m)` of every geocoder within `HIT_M`.
    pub winners: Vec<(String, f64)>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct BenchmarkReport {
    pub report_version: String,
    pub query_count: usize,
    pub with_reference_point: usize,
    pub geocoders: Vec<GeocoderReport>,
    /// Against the first `crabfinder` geocoder.
    pub reference: Option<String>,
    pub head_to_head: Vec<HeadToHead>,
    pub losses: Vec<Loss>,
}

#[derive(Serialize)]
struct DetailRow<'a> {
    geocoder: &'a str,
    id: &'a str,
    top1: Option<(f64, f64)>,
    error_m: Option<f64>,
    level_rank: usize,
    match_type: &'a str,
    latency_ms: f64,
    error: Option<&'a str>,
}

pub async fn run(config: &BenchmarkConfig, mut queries: Vec<EvalQuery>) -> Result<BenchmarkReport> {
    if config.geocoders.is_empty() {
        bail!("--benchmark needs at least one --geocoder");
    }
    if let Some(url) = &config.points_from {
        let rc = RunnerConfig {
            service_url: url.clone(),
            parallel: config.parallel,
            ..Default::default()
        };
        crate::runner::fill_reference_points(&rc, &mut queries).await?;
    }
    let mut geocoders: Vec<(GeocoderSpec, Arc<dyn Geocoder>)> = Vec::new();
    for spec in &config.geocoders {
        let mut opts = config.build.clone();
        if let Some(r) = config.rates.get(&spec.label) {
            opts.rate = Some(*r);
        }
        geocoders.push((spec.clone(), adapters::build(spec, &opts)?));
    }
    let one = |g: &Arc<dyn Geocoder>, label: String| {
        let covered: Vec<EvalQuery> = queries.iter().filter(|q| g.covers(q)).cloned().collect();
        let g = g.clone();
        let (limit, parallel) = (config.limit, config.parallel);
        async move {
            let mut results = g.run(covered, limit, parallel).await?;
            results.sort_by(|a, b| a.query.id.cmp(&b.query.id));
            info!(geocoder = %label, n = results.len(), "benchmark: done");
            Ok::<_, anyhow::Error>(results)
        }
    };
    // crabfinder alone first, so its latency isn't measured under the
    // load of a local competitor; the others side by side (independent
    // services, most of them rate-limited).
    let mut by_index: Vec<Option<Vec<QueryResult>>> = vec![None; geocoders.len()];
    for (i, (spec, g)) in geocoders.iter().enumerate() {
        if spec.kind == "crabfinder" {
            by_index[i] = Some(one(g, spec.label.clone()).await?);
        }
    }
    let others = geocoders
        .iter()
        .enumerate()
        .filter(|(_, (spec, _))| spec.kind != "crabfinder")
        .map(|(i, (spec, g))| {
            let f = one(g, spec.label.clone());
            async move { f.await.map(|r| (i, r)) }
        });
    for r in futures::future::join_all(others).await {
        let (i, results) = r?;
        by_index[i] = Some(results);
    }
    let runs: Vec<(GeocoderSpec, Vec<QueryResult>)> = geocoders
        .iter()
        .zip(by_index)
        .map(|((spec, _), r)| (spec.clone(), r.unwrap_or_default()))
        .collect();
    let report = assemble(&queries, &runs);
    write(&report, &runs, &config.output, config.output_md.as_deref())?;
    Ok(report)
}

/// Metrics, head-to-heads and losses from the per-geocoder results.
pub fn assemble(
    queries: &[EvalQuery],
    runs: &[(GeocoderSpec, Vec<QueryResult>)],
) -> BenchmarkReport {
    let geocoders = runs
        .iter()
        .map(|(spec, results)| GeocoderReport {
            name: spec.label.clone(),
            kind: spec.kind.clone(),
            covered: results.len(),
            errors: results.iter().filter(|r| r.error.is_some()).count(),
            returns_best_ids: results
                .iter()
                .any(|r| r.features.iter().any(|f| !f.1.is_empty())),
            metrics: metrics::compute(results),
        })
        .collect();
    let errors_by: Vec<(&str, HashMap<&str, Option<f64>>)> = runs
        .iter()
        .map(|(spec, results)| {
            let m = results
                .iter()
                .filter(|r| r.query.reference_point.is_some() && !r.query.expected_to_fail)
                .map(|r| (r.query.id.as_str(), metrics::top1_error_m(r)))
                .collect();
            (spec.label.as_str(), m)
        })
        .collect();
    let reference = runs.iter().position(|(s, _)| s.kind == "crabfinder");
    let mut head_to_head = Vec::new();
    let mut losses = Vec::new();
    if let Some(ri) = reference {
        let hit = |e: Option<f64>| e.is_some_and(|e| e <= HIT_M);
        let (_, crab) = &errors_by[ri];
        for (oi, (name, other)) in errors_by.iter().enumerate() {
            if oi == ri {
                continue;
            }
            let mut h = HeadToHead {
                other: name.to_string(),
                ..Default::default()
            };
            for (id, e_o) in other {
                let Some(e_c) = crab.get(id) else { continue };
                h.common += 1;
                match (hit(*e_c), hit(*e_o)) {
                    (true, true) => h.both += 1,
                    (true, false) => h.crabfinder_only += 1,
                    (false, true) => h.other_only += 1,
                    (false, false) => h.neither += 1,
                }
            }
            head_to_head.push(h);
        }
        let text: HashMap<&str, &str> = queries
            .iter()
            .map(|q| (q.id.as_str(), q.query_string.as_str()))
            .collect();
        let mut ids: Vec<&&str> = crab.keys().collect();
        ids.sort();
        for id in ids {
            let e_c = crab[*id];
            if hit(e_c) {
                continue;
            }
            let winners: Vec<(String, f64)> = errors_by
                .iter()
                .enumerate()
                .filter(|(oi, _)| *oi != ri)
                .filter_map(|(_, (name, m))| match m.get(*id) {
                    Some(Some(e)) if *e <= HIT_M => Some((name.to_string(), *e)),
                    _ => None,
                })
                .collect();
            if !winners.is_empty() {
                losses.push(Loss {
                    id: id.to_string(),
                    query: text.get(*id).unwrap_or(&"").to_string(),
                    crabfinder_error_m: e_c,
                    winners,
                });
            }
        }
    }
    BenchmarkReport {
        report_version: "bench-1".into(),
        query_count: queries.len(),
        with_reference_point: queries
            .iter()
            .filter(|q| q.reference_point.is_some())
            .count(),
        geocoders,
        reference: reference.map(|i| runs[i].0.label.clone()),
        head_to_head,
        losses,
    }
}

fn write(
    report: &BenchmarkReport,
    runs: &[(GeocoderSpec, Vec<QueryResult>)],
    output: &Path,
    output_md: Option<&Path>,
) -> Result<()> {
    if let Some(parent) = output.parent() {
        fs::create_dir_all(parent).ok();
    }
    fs::write(output, serde_json::to_string_pretty(report)?)
        .with_context(|| format!("writing {output:?}"))?;
    let rows: Vec<DetailRow> = runs
        .iter()
        .flat_map(|(spec, results)| {
            results.iter().map(move |r| DetailRow {
                geocoder: &spec.label,
                id: &r.query.id,
                top1: r.points.first().copied().flatten(),
                error_m: metrics::top1_error_m(r),
                level_rank: metrics::level_ranks(r).0,
                match_type: &r.top_match_type,
                latency_ms: r.latency_ms,
                error: r.error.as_deref(),
            })
        })
        .collect();
    let stem = output
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("benchmark");
    let detail = output
        .parent()
        .unwrap_or(Path::new("."))
        .join(format!("{stem}-detail.json.gz"));
    let mut gz = GzEncoder::new(
        fs::File::create(&detail).with_context(|| format!("creating {detail:?}"))?,
        Compression::default(),
    );
    gz.write_all(serde_json::to_string(&rows)?.as_bytes())?;
    gz.finish()?;
    if let Some(md) = output_md {
        if let Some(parent) = md.parent() {
            fs::create_dir_all(parent).ok();
        }
        fs::write(md, render_md(report)).with_context(|| format!("writing {md:?}"))?;
    }
    Ok(())
}

pub fn render_md(r: &BenchmarkReport) -> String {
    let mut s = String::new();
    s.push_str("# crabfinder benchmark\n\n");
    s.push_str(&format!(
        "- Queries: **{}** ({} with a reference point)\n",
        r.query_count, r.with_reference_point
    ));
    s.push_str(&format!(
        "- A top-1 within {HIT_M:.0} m of the reference point counts as a hit in the \
         head-to-head and the losses list.\n"
    ));
    s.push_str(
        "- Each geocoder is only asked the queries it covers (a regional service, its \
         region): compare them within a region row.\n- recall@1 needs BeSt ids; it is \
         `–` for geocoders that return none.\n\n",
    );

    s.push_str("## Overall\n\n");
    s.push_str(&format!(
        "| Geocoder | asked | errors | recall@1 {DISTANCE_HEADER} p50 | p95 |\n"
    ));
    s.push_str("|---|---|---|---|---|---|---|---|---|---|---|---|---|\n");
    for g in &r.geocoders {
        let recall = if g.returns_best_ids {
            format!("{:.3}", g.metrics.overall.recall_at_1)
        } else {
            "–".into()
        };
        s.push_str(&format!(
            "| {} | {} | {} | {recall} | {} {:.1} ms | {:.1} ms |\n",
            g.name,
            g.covered,
            g.errors,
            distance_row(&g.metrics.distance),
            g.metrics.latency.p50_ms,
            g.metrics.latency.p95_ms
        ));
    }

    let slices: BTreeSet<&str> = r
        .geocoders
        .iter()
        .flat_map(|g| g.metrics.distance_by.keys().map(String::as_str))
        .collect();
    if !slices.is_empty() {
        s.push_str("\n## By slice (distance to the reference point)\n");
        for slice in slices {
            s.push_str(&format!("\n### {slice}\n\n| Geocoder {DISTANCE_HEADER}\n"));
            s.push_str("|---|---|---|---|---|---|---|---|\n");
            for g in &r.geocoders {
                if let Some(d) = g.metrics.distance_by.get(slice) {
                    s.push_str(&format!("| {} | {}\n", g.name, distance_row(d)));
                }
            }
        }
    }

    if let Some(reference) = &r.reference {
        s.push_str(&format!(
            "\n## Head to head ({reference}, ≤ {HIT_M:.0} m)\n\n| vs | common | both | \
             {reference} only | other only | neither |\n|---|---|---|---|---|---|\n"
        ));
        for h in &r.head_to_head {
            s.push_str(&format!(
                "| {} | {} | {} | {} | {} | {} |\n",
                h.other, h.common, h.both, h.crabfinder_only, h.other_only, h.neither
            ));
        }
        s.push_str(&format!(
            "\n## Queries {reference} loses ({})\n\n",
            r.losses.len()
        ));
        if r.losses.is_empty() {
            s.push_str("_None._\n");
        } else {
            s.push_str(&format!(
                "| id | query | {reference} error | got it |\n|---|---|---|---|\n"
            ));
            for l in r.losses.iter().take(100) {
                let e = l
                    .crabfinder_error_m
                    .map_or("no answer".to_string(), |e| format!("{e:.0} m"));
                let w: Vec<String> = l
                    .winners
                    .iter()
                    .map(|(n, e)| format!("{n} ({e:.0} m)"))
                    .collect();
                s.push_str(&format!(
                    "| `{}` | {} | {e} | {} |\n",
                    l.id,
                    l.query.replace('|', "\\|"),
                    w.join(", ")
                ));
            }
            if r.losses.len() > 100 {
                s.push_str(&format!(
                    "\n_… and {} more in the JSON report._\n",
                    r.losses.len() - 100
                ));
            }
        }
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapters::Answer;
    use crate::adapters::test_util::q;

    fn res(id: &str, point: Option<(f64, f64)>, best: &str) -> QueryResult {
        let mut a = Answer::default();
        if let Some(p) = point {
            a.features.push(("".into(), best.into(), "x".into()));
            a.points.push(Some(p));
        }
        a.into_result(q(id, &format!("query {id}"), "brussels"), 1.0)
    }

    fn spec(label: &str, kind: &str) -> GeocoderSpec {
        GeocoderSpec {
            label: label.into(),
            kind: kind.into(),
            target: None,
        }
    }

    /// Reference points are (50.0, 4.0); 0.002° of latitude ≈ 222 m.
    fn runs() -> Vec<(GeocoderSpec, Vec<QueryResult>)> {
        let near = Some((50.0, 4.0));
        let far = Some((50.002, 4.0));
        vec![
            (
                spec("crabfinder", "crabfinder"),
                vec![
                    res("a", near, "id"),
                    res("b", far, "id"),
                    res("c", None, ""),
                ],
            ),
            (
                spec("brussels", "brussels"),
                vec![res("a", far, ""), res("b", near, ""), res("c", near, "")],
            ),
            (spec("vlaanderen", "vlaanderen"), vec![res("b", near, "")]),
        ]
    }

    #[test]
    fn head_to_head_and_losses() {
        let queries: Vec<EvalQuery> = ["a", "b", "c"]
            .iter()
            .map(|id| q(id, &format!("query {id}"), "brussels"))
            .collect();
        let r = assemble(&queries, &runs());
        assert_eq!(r.reference.as_deref(), Some("crabfinder"));
        assert_eq!(r.with_reference_point, 3);
        assert_eq!(
            r.head_to_head[0],
            HeadToHead {
                other: "brussels".into(),
                common: 3,
                both: 0,
                crabfinder_only: 1,
                other_only: 2,
                neither: 0
            }
        );
        assert_eq!(r.head_to_head[1].common, 1);
        assert_eq!(r.losses.len(), 2);
        assert_eq!(r.losses[0].id, "b");
        assert_eq!(r.losses[0].winners.len(), 2);
        assert!((r.losses[0].crabfinder_error_m.unwrap() - 222.4).abs() < 1.0);
        assert_eq!(
            (r.losses[1].id.as_str(), r.losses[1].crabfinder_error_m),
            ("c", None)
        );
        assert!(r.geocoders[0].returns_best_ids);
        assert!(!r.geocoders[1].returns_best_ids);
        assert_eq!(r.geocoders[2].covered, 1);
        assert!((r.geocoders[0].metrics.distance.within_100m - 1.0 / 3.0).abs() < 1e-9);

        let md = render_md(&r);
        assert!(md.contains("| crabfinder | 3 | 0 | 0.333 |"), "{md}");
        assert!(md.contains("| brussels | 3 | 0 | – |"), "{md}");
        assert!(md.contains("### region:brussels"), "{md}");
        assert!(md.contains("| brussels | 3 | 0 | 1 | 2 | 0 |"), "{md}");
        assert!(
            md.contains("| `c` | query c | no answer | brussels (0 m) |"),
            "{md}"
        );
        assert!(md.contains("## Queries crabfinder loses (2)"), "{md}");
    }

    #[test]
    fn without_crabfinder_there_is_no_head_to_head() {
        let runs = vec![(spec("brussels", "brussels"), vec![res("a", None, "")])];
        let r = assemble(&[q("a", "x", "brussels")], &runs);
        assert!(r.reference.is_none() && r.losses.is_empty());
        let md = render_md(&r);
        assert!(!md.contains("Head to head"));
    }

    #[test]
    fn writes_json_detail_and_markdown() {
        let tmp = tempfile::TempDir::new().unwrap();
        let queries: Vec<EvalQuery> = ["a", "b", "c"]
            .iter()
            .map(|id| q(id, "x", "brussels"))
            .collect();
        let runs = runs();
        let r = assemble(&queries, &runs);
        let out = tmp.path().join("nested/bench.json");
        write(&r, &runs, &out, Some(&tmp.path().join("md/bench.md"))).unwrap();
        let back: BenchmarkReport =
            serde_json::from_str(&fs::read_to_string(&out).unwrap()).unwrap();
        assert_eq!(back.geocoders.len(), 3);
        assert!(tmp.path().join("nested/bench-detail.json.gz").exists());
        assert!(tmp.path().join("md/bench.md").exists());
    }

    #[test]
    fn long_loss_lists_are_truncated() {
        let mut r = assemble(&[], &[]);
        r.reference = Some("crabfinder".into());
        r.losses = (0..101)
            .map(|i| Loss {
                id: format!("q{i}"),
                query: "a|b".into(),
                crabfinder_error_m: Some(500.0),
                winners: vec![("x".into(), 1.0)],
            })
            .collect();
        let md = render_md(&r);
        assert!(md.contains("… and 1 more"), "{md}");
        assert!(md.contains("a\\|b"));
        r.losses.clear();
        assert!(render_md(&r).contains("_None._"));
    }
}
