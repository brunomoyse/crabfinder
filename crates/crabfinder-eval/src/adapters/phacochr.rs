//! PhacochR (an R batch library over BeSt + UrbIS, not a service) as a
//! file adapter: write the queries to `input.csv`, run `<command> input.csv
//! output.csv` (default `Rscript scripts/bench/phacochr.R`), read
//! `id,lat,lon,type_geocoding` back. No BeSt ids: scored on distance.
//!
//! The run is one batch, so the per-query latency is the wall clock
//! divided by the query count. With a cache dir the output is kept by a
//! hash of the input and an unchanged rerun skips R entirely.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Instant;

use anyhow::{Context, Result, bail};
use futures::future::BoxFuture;
use serde::Deserialize;
use tracing::info;

use super::cache::fnv64;
use super::{Answer, Geocoder, failed};
use crate::eval_set::EvalQuery;
use crate::runner::QueryResult;

pub const DEFAULT_COMMAND: &str = "Rscript scripts/bench/phacochr.R";

pub struct Phacochr {
    name: String,
    command: Vec<String>,
    work_dir: PathBuf,
    cache_dir: Option<PathBuf>,
}

#[derive(Deserialize)]
struct Row {
    id: String,
    #[serde(default)]
    lat: Option<f64>,
    #[serde(default)]
    lon: Option<f64>,
    #[serde(default)]
    type_geocoding: Option<String>,
}

impl Phacochr {
    pub fn new(
        name: &str,
        command: &str,
        work_dir: PathBuf,
        cache_dir: Option<PathBuf>,
    ) -> Result<Self> {
        let command: Vec<String> = command.split_whitespace().map(String::from).collect();
        if command.is_empty() {
            bail!("{name}: empty command");
        }
        Ok(Self {
            name: name.to_string(),
            command,
            work_dir,
            cache_dir,
        })
    }

    /// The input CSV: id, the free text, and the fields when known.
    fn input_csv(queries: &[EvalQuery]) -> Result<Vec<u8>> {
        let mut w = csv::Writer::from_writer(Vec::new());
        w.write_record([
            "id",
            "query",
            "street",
            "house_number",
            "postal_code",
            "city",
        ])?;
        for q in queries {
            let s = q.structured.clone().unwrap_or_default();
            w.write_record([
                q.id.as_str(),
                q.query_string.as_str(),
                s.street.as_deref().unwrap_or(""),
                s.house_number.as_deref().unwrap_or(""),
                s.postal_code.as_deref().unwrap_or(""),
                s.city.as_deref().unwrap_or(""),
            ])?;
        }
        Ok(w.into_inner()?)
    }

    fn read_output(path: &Path) -> Result<HashMap<String, Answer>> {
        let mut out = HashMap::new();
        let mut r = csv::Reader::from_path(path).with_context(|| format!("opening {path:?}"))?;
        for (i, row) in r.deserialize::<Row>().enumerate() {
            let row = row.with_context(|| format!("{path:?} record {}", i + 1))?;
            let mut a = Answer::default();
            if let (Some(lat), Some(lon)) = (row.lat, row.lon) {
                a.push_point(
                    Some((lat, lon)),
                    row.type_geocoding.as_deref().unwrap_or(""),
                );
            }
            out.insert(row.id, a);
        }
        Ok(out)
    }

    /// Run the command (or reuse a cached output); the output path and
    /// the wall clock in ms (the original run's for a cache hit).
    fn produce(&self, input: &[u8]) -> Result<(PathBuf, f64)> {
        fs::create_dir_all(&self.work_dir)
            .with_context(|| format!("creating {:?}", self.work_dir))?;
        let hash = format!("{:016x}", fnv64(&String::from_utf8_lossy(input)));
        let cached = self
            .cache_dir
            .as_ref()
            .map(|d| (d.join(format!("{hash}.csv")), d.join(format!("{hash}.ms"))));
        if let Some((csv, ms)) = &cached
            && csv.exists()
        {
            let ms = fs::read_to_string(ms)
                .ok()
                .and_then(|s| s.trim().parse().ok())
                .unwrap_or(0.0);
            return Ok((csv.clone(), ms));
        }
        let in_path = self.work_dir.join("input.csv");
        let out_path = self.work_dir.join("output.csv");
        fs::write(&in_path, input).with_context(|| format!("writing {in_path:?}"))?;
        let _ = fs::remove_file(&out_path);
        info!(geocoder = %self.name, command = ?self.command, "benchmark: running batch");
        let t0 = Instant::now();
        let status = std::process::Command::new(&self.command[0])
            .args(&self.command[1..])
            .arg(&in_path)
            .arg(&out_path)
            .status()
            .with_context(|| format!("{}: running {:?}", self.name, self.command))?;
        let ms = t0.elapsed().as_secs_f64() * 1000.0;
        if !status.success() {
            bail!("{}: {:?} exited with {status}", self.name, self.command);
        }
        if let Some((csv, ms_path)) = &cached
            && let Some(dir) = csv.parent()
        {
            fs::create_dir_all(dir).ok();
            fs::copy(&out_path, csv).ok();
            fs::write(ms_path, ms.to_string()).ok();
        }
        Ok((out_path, ms))
    }
}

impl Geocoder for Phacochr {
    fn name(&self) -> &str {
        &self.name
    }

    fn run(
        &self,
        queries: Vec<EvalQuery>,
        _limit: usize,
        _parallel: usize,
    ) -> BoxFuture<'_, Result<Vec<QueryResult>>> {
        Box::pin(async move {
            let input = Self::input_csv(&queries)?;
            let (out, ms) = tokio::task::block_in_place(|| self.produce(&input))?;
            let mut answers = Self::read_output(&out)?;
            let per_query = ms / queries.len().max(1) as f64;
            Ok(queries
                .into_iter()
                .map(|q| match answers.remove(&q.id) {
                    Some(a) => a.into_result(q, per_query),
                    None => failed(q, per_query, "missing from the batch output".into()),
                })
                .collect())
        })
    }
}

#[cfg(test)]
mod tests {
    use super::super::test_util::{q, with_fields};
    use super::*;

    /// A stand-in for R: answers q1, leaves q2 unplaced, forgets q3, and
    /// counts its runs.
    fn fake(dir: &Path) -> String {
        let script = dir.join("fake.sh");
        fs::write(
            &script,
            format!(
                "echo run >> {}/runs\nhead -1 \"$1\" > {}/header\n\
                 printf 'id,lat,lon,type_geocoding\\nq1,50.1,4.2,exact\\nq2,,,\\n' > \"$2\"\n",
                dir.display(),
                dir.display()
            ),
        )
        .unwrap();
        format!("sh {}", script.display())
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn runs_the_batch_reads_points_and_caches_by_input() {
        let tmp = tempfile::TempDir::new().unwrap();
        let p = Phacochr::new(
            "phacochr",
            &fake(tmp.path()),
            tmp.path().join("work"),
            Some(tmp.path().join("cache")),
        )
        .unwrap();
        let qs = vec![
            with_fields(
                q("q1", "Rue de Fer 25 5000 Namur", "wallonia"),
                "Rue de Fer",
                "25",
                "5000",
                "Namur",
            ),
            q("q2", "x", "wallonia"),
            q("q3", "y", "wallonia"),
        ];
        let r = p.run(qs.clone(), 10, 1).await.unwrap();
        assert_eq!(r[0].points, [Some((50.1, 4.2))]);
        assert_eq!(r[0].top_match_type, "exact");
        assert!(r[1].points.is_empty() && r[1].error.is_none());
        assert_eq!(r[2].error.as_deref(), Some("missing from the batch output"));
        assert_eq!(
            fs::read_to_string(tmp.path().join("header"))
                .unwrap()
                .trim(),
            "id,query,street,house_number,postal_code,city"
        );
        // Same input: served from the cache, R not run again.
        let again = p.run(qs, 10, 1).await.unwrap();
        assert_eq!(again[0].points, [Some((50.1, 4.2))]);
        assert_eq!(
            fs::read_to_string(tmp.path().join("runs"))
                .unwrap()
                .lines()
                .count(),
            1
        );
        assert_eq!(p.name(), "phacochr");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn failures_are_errors() {
        let tmp = tempfile::TempDir::new().unwrap();
        assert!(Phacochr::new("p", "  ", tmp.path().into(), None).is_err());
        let p = Phacochr::new("p", "false", tmp.path().join("w"), None).unwrap();
        let err = p.run(vec![q("a", "x", "x")], 1, 1).await.unwrap_err();
        assert!(err.to_string().contains("exited"), "{err}");
        let p = Phacochr::new("p", "/nonexistent/bin", tmp.path().join("w"), None).unwrap();
        assert!(p.run(vec![q("a", "x", "x")], 1, 1).await.is_err());
        // Exits fine without writing an output.
        let p = Phacochr::new("p", "true", tmp.path().join("w"), None).unwrap();
        assert!(p.run(vec![q("a", "x", "x")], 1, 1).await.is_err());
        fs::write(tmp.path().join("bad.csv"), "id,lat\nq,notanumber\n").unwrap();
        assert!(Phacochr::read_output(&tmp.path().join("bad.csv")).is_err());
    }
}
