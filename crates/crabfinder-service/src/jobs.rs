//! Asynchronous batch jobs (v3 Phase 11.6), on with `--jobs-dir`.
//!
//! `POST /batch/jobs` stores the upload as `<dir>/<id>/input.csv` and
//! queues the job (`http::jobs`). At most `max_running` jobs run at once,
//! on a rayon pool of their own, through the same chunked runner as
//! `POST /batch/geocode` (`batch::run`), so their output is the same.
//!
//! After each chunk a job checkpoints: its rows are flushed to
//! `result.part`, then `state.json` records `rows_done` and the length of
//! `result.part`. A restarted service picks interrupted jobs up from their
//! checkpoint: the part file is cut back to the recorded length and the
//! input skips the rows already done. When the part file is shorter than
//! recorded (lost writes), the job starts over. A finished job's output is
//! `result.csv` or `result.geojson`; finished jobs are deleted `ttl` after
//! they finish.
//!
//! Uploads land in `.upload-<id>` and are renamed into place once
//! complete, so a crash mid-upload leaves nothing to resume (startup
//! deletes such directories).

use std::collections::HashMap;
use std::fs::{self, File, OpenOptions};
use std::io::{self, BufWriter, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, Weak};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use tokio::sync::Semaphore;
use tracing::{info, warn};

use crate::AppState;
use crate::batch::http::BatchOptions;
use crate::batch::io::{OutputFormat, OutputSink, read_rows};
use crate::batch::run::{CHUNK_ROWS, resolve_chunks};

const STATE_FILE: &str = "state.json";
const INPUT_FILE: &str = "input.csv";
const PART_FILE: &str = "result.part";
const UPLOAD_PREFIX: &str = ".upload-";

/// How often finished jobs past their TTL are looked for.
const SWEEP_EVERY: Duration = Duration::from_secs(600);

/// `--jobs-*` settings.
#[derive(Debug, Clone)]
pub struct JobsConfig {
    /// Where jobs live (created if missing).
    pub dir: PathBuf,
    /// Jobs running at once; the others wait, `queued`.
    pub max_running: usize,
    /// Upload limit of `POST /batch/jobs`, bytes.
    pub max_body_bytes: u64,
    /// How long a finished job (and its result) is kept.
    pub ttl: Duration,
    /// Resolver threads shared by the running jobs; 0 = half the cores.
    pub threads: usize,
    /// Rows between two checkpoints.
    pub chunk_rows: usize,
    /// Test hook: a pause before each chunk.
    #[doc(hidden)]
    pub chunk_delay: Duration,
}

impl JobsConfig {
    /// Defaults for `dir`: 2 jobs at once, 1 GiB uploads, kept 24 h.
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self {
            dir: dir.into(),
            max_running: 2,
            max_body_bytes: 1024 * 1024 * 1024,
            ttl: Duration::from_secs(24 * 3600),
            threads: 0,
            chunk_rows: CHUNK_ROWS,
            chunk_delay: Duration::ZERO,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JobStatus {
    Queued,
    Running,
    Done,
    Failed,
    Cancelled,
}

impl JobStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Running => "running",
            Self::Done => "done",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
        }
    }

    pub fn is_finished(self) -> bool {
        matches!(self, Self::Done | Self::Failed | Self::Cancelled)
    }
}

/// A job, as `state.json` holds it. Times are Unix seconds.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JobState {
    pub id: String,
    pub status: JobStatus,
    pub options: BatchOptions,
    /// Input delimiter, sniffed at upload.
    pub delimiter: u8,
    /// Input rows, counted when the job starts.
    pub rows_total: Option<usize>,
    /// Rows written to the result (at the last checkpoint).
    pub rows_done: usize,
    /// Some input row has a `best_id` (the id columns are on).
    #[serde(default)]
    pub id_columns: bool,
    /// Length of `result.part` at the last checkpoint.
    #[serde(default)]
    pub result_bytes: u64,
    pub created_at: u64,
    pub started_at: Option<u64>,
    pub finished_at: Option<u64>,
    /// Why a `failed` job failed.
    pub error: Option<String>,
    /// The API key (name) that submitted the job (v3 Phase 11.7): only
    /// that key sees it. `None` without keys.
    #[serde(default)]
    pub owner: Option<String>,
}

impl JobState {
    /// `caller` may see this job: no keys in use (`None`, the id is the
    /// access), or the key that submitted it.
    pub fn visible_to(&self, caller: Option<&str>) -> bool {
        caller.is_none() || self.owner.as_deref() == caller
    }
}

impl JobState {
    pub fn format(&self) -> OutputFormat {
        self.options.format.parse().unwrap_or_default()
    }

    /// File name of the finished result.
    pub fn result_file(&self) -> &'static str {
        match self.format() {
            OutputFormat::Csv => "result.csv",
            OutputFormat::GeoJson => "result.geojson",
        }
    }
}

/// An upload in progress: `input.csv` goes in `dir`, then `Jobs::submit`.
#[derive(Debug)]
pub struct Upload {
    pub id: String,
    pub dir: PathBuf,
}

impl Upload {
    pub fn input(&self) -> PathBuf {
        self.dir.join(INPUT_FILE)
    }
}

/// What `Jobs::delete` did.
#[derive(Debug)]
pub enum Deleted {
    /// The job was queued or running: it is now `cancelled` (its files
    /// stay until the TTL, or until it is deleted again).
    Cancelled(Box<JobState>),
    /// The job had finished: its files are gone.
    Removed,
}

struct Job {
    dir: PathBuf,
    state: Mutex<JobState>,
    cancel: AtomicBool,
}

impl Job {
    fn lock(&self) -> MutexGuard<'_, JobState> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Write `state.json` (atomically: temp file, then rename).
    fn persist(&self, st: &JobState) {
        let tmp = self.dir.join(format!("{STATE_FILE}.tmp"));
        let res = serde_json::to_vec_pretty(st)
            .map_err(io::Error::other)
            .and_then(|b| fs::write(&tmp, b))
            .and_then(|()| fs::rename(&tmp, self.dir.join(STATE_FILE)));
        if let Err(e) = res {
            warn!(job = %st.id, error = %e, "job state not saved");
        }
    }

    fn update(&self, f: impl FnOnce(&mut JobState)) {
        let mut st = self.lock();
        f(&mut st);
        self.persist(&st);
    }
}

/// The job table and its workers.
pub struct Jobs {
    cfg: JobsConfig,
    app: AppState,
    table: Mutex<HashMap<String, Arc<Job>>>,
    slots: Arc<Semaphore>,
    pool: rayon::ThreadPool,
}

impl Jobs {
    /// Open (or create) `cfg.dir`: delete unfinished uploads and expired
    /// jobs, queue again the jobs a previous run left queued or running,
    /// and start the TTL sweep. Needs a Tokio runtime.
    pub fn open(app: AppState, cfg: JobsConfig) -> Result<Arc<Self>> {
        fs::create_dir_all(&cfg.dir).with_context(|| format!("creating {:?}", cfg.dir))?;
        let threads = match cfg.threads {
            0 => std::thread::available_parallelism().map_or(1, |n| (n.get() / 2).max(1)),
            n => n,
        };
        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(threads)
            .thread_name(|i| format!("crabfinder-job-{i}"))
            .build()
            .context("building the jobs thread pool")?;
        let jobs = Arc::new(Self {
            slots: Arc::new(Semaphore::new(cfg.max_running.max(1))),
            cfg,
            app,
            table: Mutex::new(HashMap::new()),
            pool,
        });

        let mut resume = Vec::new();
        for entry in fs::read_dir(&jobs.cfg.dir)? {
            let entry = entry?;
            let name = entry.file_name().to_string_lossy().into_owned();
            let path = entry.path();
            if name.starts_with(UPLOAD_PREFIX) {
                let _ = fs::remove_dir_all(&path);
                continue;
            }
            if !is_job_id(&name) || !path.is_dir() {
                continue;
            }
            let st: JobState = match fs::read(path.join(STATE_FILE))
                .map_err(anyhow::Error::from)
                .and_then(|b| Ok(serde_json::from_slice(&b)?))
            {
                Ok(st) => st,
                Err(e) => {
                    warn!(job = %name, error = %e, "unreadable job state, job ignored");
                    continue;
                }
            };
            let interrupted = !st.status.is_finished();
            let job = Arc::new(Job {
                dir: path,
                state: Mutex::new(st),
                cancel: AtomicBool::new(false),
            });
            if interrupted {
                job.update(|st| st.status = JobStatus::Queued);
                resume.push(job.clone());
            }
            jobs.table().insert(name, job);
        }
        resume.sort_by_key(|j| j.lock().created_at);
        if !resume.is_empty() {
            info!(jobs = resume.len(), "resuming batch jobs");
        }
        for job in resume {
            jobs.enqueue(job);
        }
        jobs.sweep(now());

        let weak: Weak<Self> = Arc::downgrade(&jobs);
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(SWEEP_EVERY);
            loop {
                tick.tick().await;
                match weak.upgrade() {
                    Some(jobs) => {
                        jobs.sweep(now());
                    }
                    None => return,
                }
            }
        });
        Ok(jobs)
    }

    pub fn config(&self) -> &JobsConfig {
        &self.cfg
    }

    fn table(&self) -> MutexGuard<'_, HashMap<String, Arc<Job>>> {
        self.table.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// The job, if `caller` may see it (`JobState::visible_to`).
    fn job(&self, id: &str, caller: Option<&str>) -> Option<Arc<Job>> {
        self.table()
            .get(id)
            .filter(|j| j.lock().visible_to(caller))
            .cloned()
    }

    /// A fresh upload directory under a new id.
    pub fn new_upload(&self) -> io::Result<Upload> {
        let id = new_id()?;
        let dir = self.cfg.dir.join(format!("{UPLOAD_PREFIX}{id}"));
        fs::create_dir_all(&dir)?;
        Ok(Upload { id, dir })
    }

    /// Drop an upload that won't become a job.
    pub fn discard(&self, upload: Upload) {
        let _ = fs::remove_dir_all(upload.dir);
    }

    /// Turn a complete upload into a queued job, owned by `owner`.
    pub fn submit(
        self: &Arc<Self>,
        upload: Upload,
        options: BatchOptions,
        delimiter: u8,
        owner: Option<String>,
    ) -> io::Result<JobState> {
        let st = JobState {
            id: upload.id.clone(),
            status: JobStatus::Queued,
            options,
            delimiter,
            rows_total: None,
            rows_done: 0,
            id_columns: false,
            result_bytes: 0,
            created_at: now(),
            started_at: None,
            finished_at: None,
            error: None,
            owner,
        };
        let staged = Job {
            dir: upload.dir.clone(),
            state: Mutex::new(st.clone()),
            cancel: AtomicBool::new(false),
        };
        staged.persist(&st);
        let dir = self.cfg.dir.join(&upload.id);
        fs::rename(&upload.dir, &dir)?;
        let job = Arc::new(Job { dir, ..staged });
        self.table().insert(upload.id, job.clone());
        self.enqueue(job);
        Ok(st)
    }

    /// The job, as `caller` may see it.
    pub fn get(&self, id: &str, caller: Option<&str>) -> Option<JobState> {
        self.job(id, caller).map(|j| j.lock().clone())
    }

    /// The job and the path of its result (whether or not it is done).
    pub fn result(&self, id: &str, caller: Option<&str>) -> Option<(JobState, PathBuf)> {
        let job = self.job(id, caller)?;
        let st = job.lock().clone();
        let path = job.dir.join(st.result_file());
        Some((st, path))
    }

    /// Cancel a queued or running job; remove a finished one.
    pub fn delete(&self, id: &str, caller: Option<&str>) -> Option<Deleted> {
        let job = self.job(id, caller)?;
        let mut st = job.lock();
        if st.status.is_finished() {
            drop(st);
            self.table().remove(id);
            let _ = fs::remove_dir_all(&job.dir);
            return Some(Deleted::Removed);
        }
        job.cancel.store(true, Ordering::SeqCst);
        st.status = JobStatus::Cancelled;
        st.finished_at = Some(now());
        job.persist(&st);
        Some(Deleted::Cancelled(Box::new(st.clone())))
    }

    /// Delete the jobs that finished `ttl` or more before `now` (Unix
    /// seconds). Returns how many.
    pub fn sweep(&self, now: u64) -> usize {
        let ttl = self.cfg.ttl.as_secs();
        let expired: Vec<(String, Arc<Job>)> = self
            .table()
            .iter()
            .filter(|(_, j)| {
                let st = j.lock();
                st.status.is_finished() && st.finished_at.is_some_and(|t| t + ttl <= now)
            })
            .map(|(id, j)| (id.clone(), j.clone()))
            .collect();
        for (id, job) in &expired {
            self.table().remove(id);
            let _ = fs::remove_dir_all(&job.dir);
        }
        if !expired.is_empty() {
            info!(jobs = expired.len(), "expired batch jobs deleted");
        }
        expired.len()
    }

    /// Run `job` once a slot frees up.
    fn enqueue(self: &Arc<Self>, job: Arc<Job>) {
        let this = self.clone();
        tokio::spawn(async move {
            let Ok(_slot) = this.slots.clone().acquire_owned().await else {
                return;
            };
            if job.cancel.load(Ordering::SeqCst) {
                return;
            }
            let worker = this.clone();
            let _ = tokio::task::spawn_blocking(move || worker.run(&job)).await;
        });
    }

    /// Run `job` to its end (blocking).
    fn run(&self, job: &Job) {
        {
            let mut st = job.lock();
            if job.cancel.load(Ordering::SeqCst) {
                return;
            }
            st.status = JobStatus::Running;
            st.started_at.get_or_insert_with(now);
            job.persist(&st);
        }
        info!(job = %job.lock().id, "batch job running");
        let outcome = self.pool.install(|| self.work(job));
        let mut st = job.lock();
        st.finished_at = Some(now());
        if job.cancel.load(Ordering::SeqCst) {
            st.status = JobStatus::Cancelled;
            let _ = fs::remove_file(job.dir.join(PART_FILE));
            let _ = fs::remove_file(job.dir.join(st.result_file()));
        } else {
            match outcome {
                Ok(()) => st.status = JobStatus::Done,
                Err(e) => {
                    warn!(job = %st.id, error = %e, "batch job failed");
                    st.status = JobStatus::Failed;
                    st.error = Some(e);
                    let _ = fs::remove_file(job.dir.join(PART_FILE));
                }
            }
        }
        info!(job = %st.id, status = ?st.status, rows = st.rows_done, "batch job ended");
        job.persist(&st);
    }

    /// Count the rows (first run), then resolve them from the checkpoint
    /// on into `result.part`, and rename it once complete.
    fn work(&self, job: &Job) -> Result<(), String> {
        let st = job.lock().clone();
        let options = st.options.validate().map_err(|e| e.msg)?;
        let input = job.dir.join(INPUT_FILE);
        let (id_columns, rows_total) = match st.rows_total {
            Some(n) => (st.id_columns, n),
            None => {
                let (n, ids) = count_rows(&input, st.delimiter)?;
                job.update(|s| {
                    s.rows_total = Some(n);
                    s.id_columns = ids;
                });
                (ids, n)
            }
        };

        let part = job.dir.join(PART_FILE);
        let mut file = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(false)
            .open(&part)
            .map_err(|e| format!("result file: {e}"))?;
        let (mut base_rows, mut base_bytes) = (st.rows_done, st.result_bytes);
        let len = file.metadata().map_err(|e| e.to_string())?.len();
        if len < base_bytes {
            warn!(job = %st.id, "result shorter than its checkpoint, starting over");
            (base_rows, base_bytes) = (0, 0);
        }
        file.set_len(base_bytes).map_err(|e| e.to_string())?;
        file.seek(SeekFrom::End(0)).map_err(|e| e.to_string())?;

        let written = AtomicU64::new(base_bytes);
        let writer = Counting {
            inner: BufWriter::new(file),
            written: &written,
        };
        let mut sink = OutputSink::new(options.format, writer, base_rows);
        let reader = File::open(&input).map_err(|e| format!("input: {e}"))?;
        let rows = read_rows(reader, st.delimiter).skip(base_rows);
        let end = resolve_chunks(
            &self.app,
            rows,
            &options,
            id_columns,
            self.cfg.chunk_rows,
            &mut sink,
            |done| {
                if !self.cfg.chunk_delay.is_zero() {
                    std::thread::sleep(self.cfg.chunk_delay);
                }
                if done > 0 {
                    job.update(|s| {
                        s.rows_done = base_rows + done;
                        s.result_bytes = written.load(Ordering::SeqCst);
                    });
                }
                !job.cancel.load(Ordering::SeqCst)
            },
        )
        .map_err(|e| e.to_string())?;
        if end.stopped {
            return Ok(());
        }
        let writer = sink.finish().map_err(|e| e.to_string())?;
        let file = writer.inner.into_inner().map_err(|e| e.to_string())?;
        file.sync_all().map_err(|e| e.to_string())?;
        fs::rename(&part, job.dir.join(st.result_file())).map_err(|e| e.to_string())?;
        job.update(|s| {
            s.rows_done = base_rows + end.rows;
            s.rows_total = Some(rows_total);
            s.result_bytes = written.load(Ordering::SeqCst);
        });
        Ok(())
    }
}

/// `Write` that counts the bytes passed through.
struct Counting<'a, W: Write> {
    inner: W,
    written: &'a AtomicU64,
}

impl<W: Write> Write for Counting<'_, W> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let n = self.inner.write(buf)?;
        self.written.fetch_add(n as u64, Ordering::SeqCst);
        Ok(n)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

/// Rows in `input`, and whether one has a `best_id`. A malformed row
/// fails the job before any output.
fn count_rows(input: &Path, delimiter: u8) -> Result<(usize, bool), String> {
    let file = File::open(input).map_err(|e| format!("input: {e}"))?;
    let mut n = 0;
    let mut ids = false;
    for row in read_rows(file, delimiter) {
        let row = row.map_err(|e| e.to_string())?;
        n += 1;
        ids |= row.best_id.is_some();
    }
    Ok((n, ids))
}

/// 128 random bits, hex.
fn new_id() -> io::Result<String> {
    let mut b = [0u8; 16];
    getrandom::fill(&mut b).map_err(io::Error::other)?;
    Ok(b.iter().map(|x| format!("{x:02x}")).collect())
}

/// A job id: 32 lowercase hex digits.
pub fn is_job_id(s: &str) -> bool {
    s.len() == 32 && s.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_are_random_hex() {
        let a = new_id().unwrap();
        assert!(is_job_id(&a), "{a}");
        assert_ne!(a, new_id().unwrap());
        assert!(!is_job_id("../etc"));
        assert!(!is_job_id(&a.to_uppercase()));
    }

    #[test]
    fn finished_statuses() {
        assert!(!JobStatus::Queued.is_finished());
        assert!(!JobStatus::Running.is_finished());
        assert!(JobStatus::Done.is_finished());
        assert!(JobStatus::Failed.is_finished());
        assert!(JobStatus::Cancelled.is_finished());
        assert_eq!(
            serde_json::to_string(&JobStatus::Cancelled).unwrap(),
            r#""cancelled""#
        );
    }

    #[test]
    fn counting_rows_reports_ids_and_bad_rows() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("in.csv");
        fs::write(&p, "id,street,best_id\n1,A,\n2,B,x/1/2\n").unwrap();
        assert_eq!(count_rows(&p, b',').unwrap(), (2, true));
        fs::write(&p, "id,street\n1,A\n").unwrap();
        assert_eq!(count_rows(&p, b',').unwrap(), (1, false));
        fs::write(&p, "id,nothing\n1,A\n").unwrap();
        assert!(count_rows(&p, b',').is_err());
        assert!(count_rows(&dir.path().join("missing"), b',').is_err());
    }
}
