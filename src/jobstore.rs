//! Durable job records.
//!
//! One JSON file per job under `<project>/jobs/`, written atomically. A record carries the
//! portable manifest, its hash (the idempotency key), state, progress, checkpoints, attempt
//! count and the owning process, so a crash or restart never loses what was in flight and
//! never silently re-executes it.
//!
//! Rules:
//! * Submitting an identical manifest while a live/queued/succeeded job exists returns that job
//!   (no duplicate execution). A failed/cancelled/interrupted job needs an explicit `retry`.
//! * [`JobStore::recover`] marks jobs whose owner process is gone as `Interrupted` and does
//!   NOT restart them. Provider calls that timed out are `Uncertain` until verified.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::error::{Error, ErrorCode, Result};
use crate::util::{clock, ids, paths};

pub const MAX_LOG_LINES: usize = 200;
pub const DEFAULT_MAX_ATTEMPTS: u32 = 3;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JobState {
    Queued,
    Running,
    Succeeded,
    Failed,
    Cancelled,
    /// The owner process disappeared mid-run. Never auto-restarted.
    Interrupted,
    /// A remote/provider outcome is unknown (e.g. a request timed out). Must be reconciled.
    Uncertain,
}

impl JobState {
    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            JobState::Succeeded | JobState::Failed | JobState::Cancelled
        )
    }
    /// States that block an identical submission from starting a second execution.
    pub fn blocks_duplicates(self) -> bool {
        matches!(
            self,
            JobState::Queued | JobState::Running | JobState::Succeeded | JobState::Uncertain
        )
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JobRecord {
    pub id: String,
    pub kind: String,
    pub state: JobState,
    /// sha256 of the canonical manifest: the idempotency key.
    pub key: String,
    pub manifest: Value,
    pub progress: f32,
    pub message: String,
    pub attempts: u32,
    /// Set by an explicit `retry`: re-queued on purpose, so the next identical request may adopt
    /// it even if it never ran.
    #[serde(default)]
    pub retried: bool,
    pub max_attempts: u32,
    pub cancel_requested: bool,
    pub owner_pid: Option<u32>,
    pub created_at: String,
    pub updated_at: String,
    /// Opaque provider handle (e.g. a RunPod pod/job id) needed to reconcile or clean up.
    pub remote_ref: Option<String>,
    pub checkpoints: Vec<String>,
    pub output: Option<Value>,
    pub error: Option<String>,
    pub log: Vec<String>,
}

pub fn manifest_key(kind: &str, manifest: &Value) -> String {
    // serde_json maps are ordered (BTreeMap) unless preserve_order is on, so this is canonical.
    let text = format!(
        "{kind}\n{}",
        serde_json::to_string(manifest).unwrap_or_default()
    );
    Sha256::digest(text.as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

pub fn process_alive(pid: u32) -> bool {
    if pid == std::process::id() {
        return true;
    }
    #[cfg(target_os = "linux")]
    {
        Path::new(&format!("/proc/{pid}")).exists()
    }
    #[cfg(all(unix, not(target_os = "linux")))]
    {
        // macOS/BSD: no /proc. `kill -0` only checks that the process exists and is signalable.
        std::process::Command::new("kill")
            .args(["-0", &pid.to_string()])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(true)
    }
    #[cfg(windows)]
    {
        // `tasklist` prints the PID when the process exists. On any failure assume alive: we would
        // rather leave a stale "running" record than mark live work as interrupted.
        std::process::Command::new("tasklist")
            .args(["/FI", &format!("PID eq {pid}"), "/NH", "/FO", "CSV"])
            .output()
            .map(|o| String::from_utf8_lossy(&o.stdout).contains(&format!("\"{pid}\"")))
            .unwrap_or(true)
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = pid;
        true
    }
}

/// One lock for every handle in the process: records are shared files, and several handles
/// (the submitting tool call and the worker thread) legitimately write the same job.
static WRITE_GUARD: Mutex<()> = Mutex::new(());

pub struct JobStore {
    dir: PathBuf,
}

pub enum Submission {
    /// A new record was created and is `Queued`.
    Created(JobRecord),
    /// An identical job already exists in a state that forbids a second execution.
    Existing(JobRecord),
}

impl Submission {
    pub fn record(&self) -> &JobRecord {
        match self {
            Submission::Created(r) | Submission::Existing(r) => r,
        }
    }
    pub fn is_new(&self) -> bool {
        matches!(self, Submission::Created(_))
    }
}

impl JobStore {
    pub fn open(dir: &Path) -> Result<JobStore> {
        paths::ensure_dir(dir)?;
        Ok(JobStore {
            dir: dir.to_path_buf(),
        })
    }

    fn path(&self, id: &str) -> Result<PathBuf> {
        if id.is_empty()
            || !id
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
        {
            return Err(Error::invalid("invalid job id"));
        }
        Ok(self.dir.join(format!("{id}.json")))
    }

    fn write(&self, record: &JobRecord) -> Result<()> {
        let text = serde_json::to_vec_pretty(record)?;
        paths::write_atomic(&self.path(&record.id)?, &text)
    }

    pub fn get(&self, id: &str) -> Result<JobRecord> {
        let path = self.path(id)?;
        let text = std::fs::read_to_string(&path)
            .map_err(|_| Error::not_found(format!("job {id} not found")))?;
        Ok(serde_json::from_str(&text)?)
    }

    pub fn list(&self) -> Result<Vec<JobRecord>> {
        let mut out = Vec::new();
        for entry in std::fs::read_dir(&self.dir)? {
            let path = entry?.path();
            if path.extension().and_then(|e| e.to_str()) != Some("json") {
                continue;
            }
            if let Ok(text) = std::fs::read_to_string(&path) {
                if let Ok(record) = serde_json::from_str::<JobRecord>(&text) {
                    out.push(record);
                }
            }
        }
        out.sort_by(|a, b| a.created_at.cmp(&b.created_at).then(a.id.cmp(&b.id)));
        Ok(out)
    }

    /// Idempotent submit. Returns the existing job instead of creating a duplicate.
    pub fn submit(&self, kind: &str, manifest: Value) -> Result<Submission> {
        let _g = WRITE_GUARD.lock().unwrap_or_else(|e| e.into_inner());
        let key = manifest_key(kind, &manifest);
        if let Some(existing) = self
            .list()?
            .into_iter()
            .rev()
            .find(|r| r.key == key && r.state.blocks_duplicates())
        {
            return Ok(Submission::Existing(existing));
        }
        let now = clock::now_rfc3339();
        let record = JobRecord {
            id: ids::new_id("job"),
            kind: kind.to_string(),
            state: JobState::Queued,
            key,
            manifest,
            progress: 0.0,
            message: "queued".into(),
            attempts: 0,
            retried: false,
            max_attempts: DEFAULT_MAX_ATTEMPTS,
            cancel_requested: false,
            owner_pid: Some(std::process::id()),
            created_at: now.clone(),
            updated_at: now,
            remote_ref: None,
            checkpoints: Vec::new(),
            output: None,
            error: None,
            log: Vec::new(),
        };
        self.write(&record)?;
        Ok(Submission::Created(record))
    }

    pub fn update<F: FnOnce(&mut JobRecord)>(&self, id: &str, f: F) -> Result<JobRecord> {
        let _g = WRITE_GUARD.lock().unwrap_or_else(|e| e.into_inner());
        let mut record = self.get(id)?;
        f(&mut record);
        if record.log.len() > MAX_LOG_LINES {
            let drop = record.log.len() - MAX_LOG_LINES;
            record.log.drain(..drop);
        }
        record.updated_at = clock::now_rfc3339();
        self.write(&record)?;
        Ok(record)
    }

    pub fn start(&self, id: &str) -> Result<JobRecord> {
        let current = self.get(id)?;
        if current.state.is_terminal() || current.state == JobState::Running {
            return Err(Error::new(
                ErrorCode::Conflict,
                format!("job {id} is {:?} and cannot be started", current.state),
            ));
        }
        self.update(id, |r| {
            r.state = JobState::Running;
            r.attempts += 1;
            r.owner_pid = Some(std::process::id());
            r.message = "running".into();
        })
    }

    pub fn progress(&self, id: &str, fraction: f32, message: &str) -> Result<JobRecord> {
        self.update(id, |r| {
            r.progress = fraction.clamp(0.0, 1.0);
            r.message = message.to_string();
        })
    }

    pub fn checkpoint(&self, id: &str, label: &str) -> Result<JobRecord> {
        self.update(id, |r| r.checkpoints.push(label.to_string()))
    }

    pub fn succeed(&self, id: &str, output: Value) -> Result<JobRecord> {
        self.update(id, |r| {
            r.state = JobState::Succeeded;
            r.progress = 1.0;
            r.message = "done".into();
            r.output = Some(output);
            r.error = None;
        })
    }

    pub fn fail(&self, id: &str, error: &str) -> Result<JobRecord> {
        self.update(id, |r| {
            r.state = if r.cancel_requested {
                JobState::Cancelled
            } else {
                JobState::Failed
            };
            r.message = error.to_string();
            r.error = Some(error.to_string());
        })
    }

    pub fn mark_uncertain(&self, id: &str, why: &str) -> Result<JobRecord> {
        self.update(id, |r| {
            r.state = JobState::Uncertain;
            r.message = format!("outcome unknown: {why}");
            r.log.push(format!("uncertain: {why}"));
        })
    }

    pub fn request_cancel(&self, id: &str) -> Result<JobRecord> {
        self.update(id, |r| {
            if !r.state.is_terminal() {
                r.cancel_requested = true;
                if r.state == JobState::Queued {
                    r.state = JobState::Cancelled;
                    r.message = "cancelled before start".into();
                }
            }
        })
    }

    /// Explicit re-run of a failed/interrupted/cancelled job, bounded by `max_attempts`.
    pub fn retry(&self, id: &str) -> Result<JobRecord> {
        let current = self.get(id)?;
        match current.state {
            JobState::Succeeded | JobState::Running | JobState::Queued => {
                return Err(Error::new(
                    ErrorCode::Conflict,
                    format!("job {id} is {:?}; nothing to retry", current.state),
                ))
            }
            JobState::Uncertain => {
                return Err(Error::new(
                    ErrorCode::Conflict,
                    format!("job {id} has an unknown remote outcome; reconcile it before retrying"),
                ))
            }
            _ => {}
        }
        if current.attempts >= current.max_attempts {
            return Err(Error::new(
                ErrorCode::Budget,
                format!("job {id} used all {} attempts", current.max_attempts),
            ));
        }
        self.update(id, |r| {
            r.state = JobState::Queued;
            r.retried = true;
            r.cancel_requested = false;
            r.error = None;
            r.message = "queued for retry".into();
        })
    }

    /// Run at startup. Marks jobs whose owner process is gone as `Interrupted`; never restarts.
    pub fn recover(&self) -> Result<Vec<JobRecord>> {
        let mut changed = Vec::new();
        for record in self.list()? {
            if !matches!(record.state, JobState::Running | JobState::Queued) {
                continue;
            }
            let alive = record.owner_pid.map(process_alive).unwrap_or(false);
            if !alive {
                let updated = self.update(&record.id, |r| {
                    r.state = JobState::Interrupted;
                    r.message =
                        "interrupted: the owning process ended before the job finished".into();
                    r.log.push(
                        "recovered as interrupted on startup; not restarted automatically".into(),
                    );
                })?;
                changed.push(updated);
            }
        }
        Ok(changed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn store() -> (tempfile::TempDir, JobStore) {
        let dir = tempfile::tempdir().unwrap();
        let s = JobStore::open(&dir.path().join("jobs")).unwrap();
        (dir, s)
    }

    #[test]
    fn identical_manifest_is_deduplicated_but_different_one_is_not() {
        let (_d, s) = store();
        let a = s.submit("render", json!({"w": 7680, "h": 4320})).unwrap();
        assert!(a.is_new());
        let b = s.submit("render", json!({"h": 4320, "w": 7680})).unwrap();
        assert!(
            !b.is_new(),
            "same manifest (key order irrelevant) must not start twice"
        );
        assert_eq!(a.record().id, b.record().id);
        let c = s.submit("render", json!({"w": 3840, "h": 2160})).unwrap();
        assert!(c.is_new());
    }

    #[test]
    fn lifecycle_progress_checkpoints_and_output_persist() {
        let (d, s) = store();
        let id = s
            .submit("render", json!({"x": 1}))
            .unwrap()
            .record()
            .id
            .clone();
        s.start(&id).unwrap();
        s.progress(&id, 0.4, "encoding").unwrap();
        s.checkpoint(&id, "frames:0-99").unwrap();
        s.succeed(&id, json!({"file": "a.mp4"})).unwrap();
        // A new handle (as after a restart) sees everything.
        let again = JobStore::open(&d.path().join("jobs"))
            .unwrap()
            .get(&id)
            .unwrap();
        assert_eq!(again.state, JobState::Succeeded);
        assert_eq!(again.checkpoints, vec!["frames:0-99"]);
        assert_eq!(again.attempts, 1);
        assert!(
            s.start(&id).is_err(),
            "a finished job cannot be started again"
        );
    }

    #[test]
    fn dead_owner_becomes_interrupted_and_is_not_restarted() {
        let (_d, s) = store();
        let id = s
            .submit("render", json!({"x": 2}))
            .unwrap()
            .record()
            .id
            .clone();
        s.start(&id).unwrap();
        // Simulate a crashed owner: a pid that cannot exist.
        s.update(&id, |r| r.owner_pid = Some(4_000_000_000))
            .unwrap();
        let changed = s.recover().unwrap();
        assert_eq!(changed.len(), 1);
        let r = s.get(&id).unwrap();
        assert_eq!(r.state, JobState::Interrupted);
        // Re-submitting the same manifest is allowed (interrupted does not block) but only via a
        // NEW record when asked; the old one is retried explicitly and bounded.
        let retried = s.retry(&id).unwrap();
        assert_eq!(retried.state, JobState::Queued);
    }

    #[test]
    fn live_owner_is_left_alone() {
        let (_d, s) = store();
        let id = s
            .submit("render", json!({"x": 3}))
            .unwrap()
            .record()
            .id
            .clone();
        s.start(&id).unwrap();
        assert!(s.recover().unwrap().is_empty());
        assert_eq!(s.get(&id).unwrap().state, JobState::Running);
    }

    #[test]
    fn retries_are_bounded_and_uncertain_jobs_need_reconciliation() {
        let (_d, s) = store();
        let id = s
            .submit("cloud", json!({"x": 4}))
            .unwrap()
            .record()
            .id
            .clone();
        for _ in 0..DEFAULT_MAX_ATTEMPTS {
            s.start(&id).unwrap();
            s.fail(&id, "boom").unwrap();
            let _ = s.retry(&id);
        }
        assert_eq!(s.retry(&id).unwrap_err().code, ErrorCode::Budget);

        let u = s
            .submit("cloud", json!({"x": 5}))
            .unwrap()
            .record()
            .id
            .clone();
        s.start(&u).unwrap();
        s.mark_uncertain(&u, "request timed out").unwrap();
        assert_eq!(s.retry(&u).unwrap_err().code, ErrorCode::Conflict);
        let dup = s.submit("cloud", json!({"x": 5})).unwrap();
        assert!(
            !dup.is_new(),
            "an uncertain job must block a duplicate submission"
        );
    }

    #[test]
    fn cancel_before_start_and_path_traversal_ids() {
        let (_d, s) = store();
        let id = s
            .submit("render", json!({"x": 6}))
            .unwrap()
            .record()
            .id
            .clone();
        assert_eq!(s.request_cancel(&id).unwrap().state, JobState::Cancelled);
        assert!(s.get("../etc/passwd").is_err());
        assert!(s.get("").is_err());
    }

    #[test]
    fn retry_marks_a_never_started_job_so_the_next_request_may_adopt_it() {
        let dir = tempfile::tempdir().unwrap();
        let store = JobStore::open(dir.path()).unwrap();
        let rec = store
            .submit("k", serde_json::json!({"a": 1}))
            .unwrap()
            .record()
            .clone();
        assert!(!rec.retried);
        store
            .update(&rec.id, |r| r.state = JobState::Interrupted)
            .unwrap();
        let again = store.retry(&rec.id).unwrap();
        assert_eq!(again.attempts, 0, "retry must not consume an attempt");
        assert!(again.retried && again.state == JobState::Queued);
    }
}
