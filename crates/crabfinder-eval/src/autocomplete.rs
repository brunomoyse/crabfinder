//! `--autocomplete-mode` (v3 Phase 11.5): how soon `/autocomplete` offers
//! the right street while it is being typed.
//!
//! Each `canonical` query of a synthetic set ("Benjamin Jansenlaan 6, 1160
//! Oudergem") becomes the typed text "street municipality" ("Benjamin
//! Jansenlaan Oudergem"). The text is cut after `PREFIX_LENGTHS`
//! characters, then sent whole. A cut scores when a street suggestion of
//! the expected street (the street object of the expected building) is
//! in the top 1 / top 10.
//!
//! Every text is sent twice: bare, and with `focus_point` at the
//! municipality typed (its `/geocode` answer), the way a map widget sends
//! the map's centre. Short prefixes ("Rue des ") match thousands of
//! streets, so without a place only the focused run can rank them.
//!
//! The removed v5 edge-ngram attempt (`reports/archive/phase7a-removed/`)
//! scored the *building* on street-name prefixes. Its numbers compare
//! only roughly.

use std::collections::HashMap;
use std::path::Path;
use std::time::Instant;

use anyhow::{Context, Result};
use futures::stream::{self, StreamExt};
use serde::{Deserialize, Serialize};

use crate::eval_set::{SyntheticEvalSet, VariantType};

/// Characters typed before each request; the whole text is sent last.
pub const PREFIX_LENGTHS: [usize; 4] = [2, 4, 6, 8];

/// Suggestions asked per request.
pub const LIMIT: usize = 10;

/// One street to type.
#[derive(Debug, Clone, PartialEq)]
pub struct Case {
    pub id: String,
    pub region: String,
    /// "street municipality".
    pub input: String,
    /// The municipality part (where the focused run centres).
    pub place: String,
    /// `{street namespace}|{street id}`: the expected street.
    pub street_key: String,
}

/// The `canonical` queries of `set` as cases.
pub fn cases(set: &SyntheticEvalSet) -> Vec<Case> {
    set.queries
        .iter()
        .filter(|q| q.variant_type == VariantType::Canonical)
        .filter_map(|q| {
            let (street, place) = q.query_string.split_once(", ")?;
            let street = match street.rsplit_once(' ') {
                Some((s, n)) if n.starts_with(|c: char| c.is_ascii_digit()) => s,
                _ => street,
            };
            let place = place
                .split_once(' ')
                .filter(|(pc, _)| pc.bytes().all(|b| b.is_ascii_digit()))
                .map_or(place, |(_, p)| p);
            Some(Case {
                id: q.id.clone(),
                region: q.sample_region.clone(),
                input: format!("{street} {place}"),
                place: place.to_string(),
                street_key: q.expected_building_group_id.rsplit_once('|')?.0.to_string(),
            })
        })
        .collect()
}

/// One request.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Outcome {
    pub id: String,
    pub region: String,
    /// Characters sent; `0` = the whole text.
    pub length: usize,
    pub q: String,
    /// 1-based rank of the expected street; `None` = not in the top
    /// `LIMIT`.
    pub rank: Option<usize>,
    pub latency_ms: f64,
    pub error: bool,
    /// Sent with `focus_point` at the municipality.
    #[serde(default)]
    pub focus: bool,
}

/// The texts sent for `case`.
pub fn prefixes(case: &Case) -> Vec<(usize, String)> {
    let n = case.input.chars().count();
    let mut out: Vec<(usize, String)> = PREFIX_LENGTHS
        .iter()
        .filter(|&&l| l < n)
        .map(|&l| (l, case.input.chars().take(l).collect()))
        .collect();
    out.push((0, case.input.clone()));
    out
}

/// Rank of `street_key` among the street suggestions of a response.
pub fn rank_of(body: &serde_json::Value, street_key: &str) -> Option<usize> {
    body["features"]
        .as_array()?
        .iter()
        .position(|f| f["properties"]["street"]["street_key"] == street_key)
        .map(|i| i + 1)
}

/// `lat,lng` of each place, from `/geocode` on `service`; places it can't
/// place are left out.
pub async fn places(service: &str, cases: &[Case], parallel: usize) -> HashMap<String, (f64, f64)> {
    let client = reqwest::Client::new();
    let url = format!("{}/geocode", service.trim_end_matches('/'));
    let mut names: Vec<&str> = cases.iter().map(|c| c.place.as_str()).collect();
    names.sort_unstable();
    names.dedup();
    stream::iter(names)
        .map(|name| {
            let (client, url) = (client.clone(), url.clone());
            async move {
                let body: serde_json::Value = client
                    .get(&url)
                    .query(&[("q", name), ("limit", "1")])
                    .send()
                    .await
                    .ok()?
                    .json()
                    .await
                    .ok()?;
                let c = &body["features"][0]["geometry"]["coordinates"];
                Some((name.to_string(), (c[1].as_f64()?, c[0].as_f64()?)))
            }
        })
        .buffer_unordered(parallel.max(1))
        .filter_map(|x| async move { x })
        .collect()
        .await
}

/// Send every prefix of every case to `service`, `parallel` at a time,
/// with `focus_point` at the case's place when `focus` has it.
pub async fn run(
    service: &str,
    cases: &[Case],
    parallel: usize,
    focus: Option<&HashMap<String, (f64, f64)>>,
) -> Result<Vec<Outcome>> {
    let client = reqwest::Client::new();
    let url = format!("{}/autocomplete", service.trim_end_matches('/'));
    // (case, characters sent, text, focus point).
    type Job<'a> = (&'a Case, usize, String, Option<(f64, f64)>);
    let jobs: Vec<Job> = cases
        .iter()
        .filter_map(|c| match focus {
            Some(f) => Some((c, Some(*f.get(&c.place)?))),
            None => Some((c, None)),
        })
        .flat_map(|(c, f)| prefixes(c).into_iter().map(move |(l, q)| (c, l, q, f)))
        .collect();
    let out = stream::iter(jobs)
        .map(|(c, length, q, f)| {
            let client = client.clone();
            let url = url.clone();
            async move {
                let mut params = vec![("q", q.clone()), ("limit", LIMIT.to_string())];
                if let Some((lat, lon)) = f {
                    params.push(("focus_point", format!("{lat},{lon}")));
                }
                let t0 = Instant::now();
                let resp = client.get(&url).query(&params).send().await;
                let body = match resp {
                    Ok(r) if r.status().is_success() => r.json::<serde_json::Value>().await.ok(),
                    _ => None,
                };
                Outcome {
                    id: c.id.clone(),
                    region: c.region.clone(),
                    length,
                    rank: body.as_ref().and_then(|b| rank_of(b, &c.street_key)),
                    error: body.is_none(),
                    q,
                    latency_ms: t0.elapsed().as_secs_f64() * 1000.0,
                    focus: f.is_some(),
                }
            }
        })
        .buffer_unordered(parallel.max(1))
        .collect::<Vec<_>>()
        .await;
    Ok(out)
}

/// Recall and latency of one group of outcomes.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Row {
    pub label: String,
    pub n: usize,
    pub recall_at_1: f64,
    pub recall_at_10: f64,
    pub p50_ms: f64,
    pub p95_ms: f64,
    pub p99_ms: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Summary {
    pub service: String,
    pub cases: usize,
    pub errors: usize,
    pub overall: Row,
    pub by_length: Vec<Row>,
    pub by_region: Vec<Row>,
    /// The same texts with `focus_point` at the municipality.
    pub focus_overall: Row,
    pub focus_by_length: Vec<Row>,
}

fn row(label: &str, outs: &[&Outcome]) -> Row {
    let n = outs.len();
    let share = |k: usize| {
        if n == 0 {
            0.0
        } else {
            outs.iter()
                .filter(|o| o.rank.is_some_and(|r| r <= k))
                .count() as f64
                / n as f64
        }
    };
    let mut lat: Vec<f64> = outs.iter().map(|o| o.latency_ms).collect();
    lat.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let pct = |p: f64| {
        if lat.is_empty() {
            0.0
        } else {
            lat[((lat.len() - 1) as f64 * p).round() as usize]
        }
    };
    Row {
        label: label.to_string(),
        n,
        recall_at_1: share(1),
        recall_at_10: share(LIMIT),
        p50_ms: pct(0.50),
        p95_ms: pct(0.95),
        p99_ms: pct(0.99),
    }
}

fn by_length(outs: &[&Outcome]) -> Vec<Row> {
    PREFIX_LENGTHS
        .iter()
        .copied()
        .chain([0])
        .map(|l| {
            let g: Vec<&Outcome> = outs.iter().copied().filter(|o| o.length == l).collect();
            let label = if l == 0 {
                "full".to_string()
            } else {
                l.to_string()
            };
            row(&label, &g)
        })
        .collect()
}

pub fn summarize(service: &str, n_cases: usize, all_outs: &[Outcome]) -> Summary {
    let (focused, bare): (Vec<&Outcome>, Vec<&Outcome>) = all_outs.iter().partition(|o| o.focus);
    let outs = &bare;
    let all: Vec<&Outcome> = outs.clone();
    let by_len = by_length(outs);
    let mut regions: Vec<&str> = outs.iter().map(|o| o.region.as_str()).collect();
    regions.sort_unstable();
    regions.dedup();
    let by_region = regions
        .into_iter()
        .map(|r| {
            let g: Vec<&Outcome> = outs.iter().copied().filter(|o| o.region == r).collect();
            row(r, &g)
        })
        .collect();
    Summary {
        service: service.to_string(),
        cases: n_cases,
        errors: all_outs.iter().filter(|o| o.error).count(),
        overall: row("all", &all),
        by_length: by_len,
        by_region,
        focus_overall: row("all", &focused),
        focus_by_length: by_length(&focused),
    }
}

pub fn markdown(s: &Summary) -> String {
    use std::fmt::Write;
    let mut md = String::from("# Autocomplete eval (v3 Phase 11.5)\n\n");
    let _ = writeln!(
        md,
        "Service `{}`; {} streets typed as \"street municipality\" (canonical \
         queries), {} requests, {} errors. A request scores when the expected \
         street is among the street suggestions (top 1 / top {LIMIT}).\n",
        s.service, s.cases, s.overall.n, s.errors
    );
    let table = |md: &mut String, title: &str, head: &str, rows: &[Row]| {
        let _ = writeln!(md, "## {title}\n");
        let _ = writeln!(
            md,
            "| {head} | n | recall@1 | recall@10 | p50 ms | p95 ms | p99 ms |\n|---|---|---|---|---|---|---|"
        );
        for r in rows {
            let _ = writeln!(
                md,
                "| {} | {} | {:.3} | {:.3} | {:.1} | {:.1} | {:.1} |",
                r.label, r.n, r.recall_at_1, r.recall_at_10, r.p50_ms, r.p95_ms, r.p99_ms
            );
        }
        md.push('\n');
    };
    table(&mut md, "By characters typed", "typed", &s.by_length);
    table(&mut md, "By region", "region", &s.by_region);
    table(&mut md, "Overall", "", std::slice::from_ref(&s.overall));
    if s.focus_overall.n > 0 {
        table(
            &mut md,
            "With focus_point at the municipality, by characters typed",
            "typed",
            &s.focus_by_length,
        );
        table(
            &mut md,
            "With focus_point at the municipality, overall",
            "",
            std::slice::from_ref(&s.focus_overall),
        );
    }
    md
}

/// Run the mode end to end and write `output` (JSON), its
/// `-detail.json` and, if given, `output_md`.
pub async fn run_mode(
    service: &str,
    eval_set: &Path,
    parallel: usize,
    output: &Path,
    output_md: Option<&Path>,
) -> Result<Summary> {
    let set = SyntheticEvalSet::load(eval_set)?;
    let cases = cases(&set);
    anyhow::ensure!(
        !cases.is_empty(),
        "{eval_set:?} has no canonical query to type"
    );
    let mut outs = run(service, &cases, parallel, None).await?;
    let focus = places(service, &cases, parallel).await;
    outs.extend(run(service, &cases, parallel, Some(&focus)).await?);
    let summary = summarize(service, cases.len(), &outs);
    std::fs::write(output, serde_json::to_string_pretty(&summary)?)
        .with_context(|| format!("writing {output:?}"))?;
    let detail = output.with_extension("detail.json");
    std::fs::write(&detail, serde_json::to_string(&outs)?)
        .with_context(|| format!("writing {detail:?}"))?;
    if let Some(p) = output_md {
        std::fs::write(p, markdown(&summary)).with_context(|| format!("writing {p:?}"))?;
    }
    Ok(summary)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn case(input: &str) -> Case {
        Case {
            id: "1".into(),
            region: "BE-BRU".into(),
            input: input.into(),
            place: "Ixelles".into(),
            street_key: "ns|1".into(),
        }
    }

    #[test]
    fn prefixes_stop_at_the_whole_text() {
        let p = prefixes(&case("Rue X Ixelles"));
        let lens: Vec<usize> = p.iter().map(|x| x.0).collect();
        assert_eq!(lens, [2, 4, 6, 8, 0]);
        assert_eq!(p[1].1, "Rue ");
        assert_eq!(prefixes(&case("Rue")).len(), 2);
    }

    #[test]
    fn rank_finds_the_street() {
        let body = serde_json::json!({"features": [
            {"properties": {"kind": "locality"}},
            {"properties": {"street": {"street_key": "ns|1"}}},
        ]});
        assert_eq!(rank_of(&body, "ns|1"), Some(2));
        assert_eq!(rank_of(&body, "ns|2"), None);
    }

    #[test]
    fn rows_count_recall_and_latency() {
        let o = |length, rank, ms| Outcome {
            id: "1".into(),
            region: "BE-BRU".into(),
            length,
            q: String::new(),
            rank,
            latency_ms: ms,
            error: false,
            focus: false,
        };
        let mut focused = o(8, Some(1), 9.0);
        focused.focus = true;
        let outs = [
            o(2, None, 1.0),
            o(2, Some(3), 2.0),
            o(0, Some(1), 3.0),
            focused,
        ];
        let s = summarize("http://x", 1, &outs);
        assert_eq!(s.by_length[0].n, 2);
        assert_eq!(s.by_length[0].recall_at_1, 0.0);
        assert_eq!(s.by_length[0].recall_at_10, 0.5);
        assert_eq!(s.by_length[4].label, "full");
        assert_eq!(s.by_length[4].recall_at_1, 1.0);
        assert_eq!(s.overall.p99_ms, 3.0);
        assert!(markdown(&s).contains("| full | 1 | 1.000 | 1.000 |"));
        // An empty group stays at zero.
        assert_eq!(s.by_length[1].n, 0);
        assert_eq!(s.by_length[1].p50_ms, 0.0);
        // The focused run is reported apart.
        assert_eq!(s.overall.n, 3);
        assert_eq!(s.focus_overall.n, 1);
        assert_eq!(s.focus_by_length[3].recall_at_1, 1.0);
        assert!(markdown(&s).contains("With focus_point at the municipality"));
    }
}
