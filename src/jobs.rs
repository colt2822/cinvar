use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::error::Result;
use crate::util::clock;
use crate::util::ids;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JobStatus {
    Queued,
    Running,
    Succeeded,
    Failed,
    Cancelled,
}

impl JobStatus {
    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            JobStatus::Succeeded | JobStatus::Failed | JobStatus::Cancelled
        )
    }
    pub fn as_str(self) -> &'static str {
        match self {
            JobStatus::Queued => "queued",
            JobStatus::Running => "running",
            JobStatus::Succeeded => "succeeded",
            JobStatus::Failed => "failed",
            JobStatus::Cancelled => "cancelled",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JobSnapshot {
    pub id: String,
    pub kind: String,
    pub status: JobStatus,
    pub progress: f32,
    pub message: String,
    pub output: Option<serde_json::Value>,
    pub error: Option<crate::error::Error>,
    pub started_at: String,
    pub finished_at: Option<String>,
}

struct Entry {
    snapshot: JobSnapshot,
    cancel: Arc<AtomicBool>,
}

#[derive(Clone)]
pub struct Jobs {
    entries: Arc<Mutex<BTreeMap<String, Entry>>>,
}

impl Default for Jobs {
    fn default() -> Self {
        Self::new()
    }
}

impl Jobs {
    pub fn new() -> Jobs {
        Jobs {
            entries: Arc::new(Mutex::new(BTreeMap::new())),
        }
    }

    pub fn spawn<F>(&self, kind: &str, work: F) -> String
    where
        F: FnOnce(JobCtx) -> Result<serde_json::Value> + Send + 'static,
    {
        let id = ids::new_id("job");
        let cancel = Arc::new(AtomicBool::new(false));
        let snapshot = JobSnapshot {
            id: id.clone(),
            kind: kind.to_string(),
            status: JobStatus::Queued,
            progress: 0.0,
            message: "queued".into(),
            output: None,
            error: None,
            started_at: clock::now_rfc3339(),
            finished_at: None,
        };
        {
            let mut guard = self.entries.lock().unwrap();
            guard.insert(
                id.clone(),
                Entry {
                    snapshot,
                    cancel: cancel.clone(),
                },
            );
        }
        let ctx = JobCtx {
            id: id.clone(),
            cancel,
            entries: self.entries.clone(),
        };
        std::thread::spawn(move || {
            ctx.update(|s| {
                s.status = JobStatus::Running;
                s.message = "running".into();
            });
            let result = work(ctx.clone());
            match result {
                Ok(value) => ctx.update(|s| {
                    s.status = JobStatus::Succeeded;
                    s.progress = 1.0;
                    s.message = "done".into();
                    s.output = Some(value);
                    s.finished_at = Some(clock::now_rfc3339());
                }),
                Err(err) => {
                    let cancelled = ctx.cancelled();
                    ctx.update(|s| {
                        s.status = if cancelled {
                            JobStatus::Cancelled
                        } else {
                            JobStatus::Failed
                        };
                        s.message = err.message.clone();
                        s.error = Some(err);
                        s.finished_at = Some(clock::now_rfc3339());
                    });
                }
            }
        });
        id
    }

    pub fn get(&self, id: &str) -> Option<JobSnapshot> {
        self.entries
            .lock()
            .unwrap()
            .get(id)
            .map(|e| e.snapshot.clone())
    }

    pub fn list(&self) -> Vec<JobSnapshot> {
        self.entries
            .lock()
            .unwrap()
            .values()
            .map(|e| e.snapshot.clone())
            .collect()
    }

    pub fn cancel(&self, id: &str) -> bool {
        if let Some(entry) = self.entries.lock().unwrap().get(id) {
            entry.cancel.store(true, Ordering::SeqCst);
            return true;
        }
        false
    }

    pub fn wait(&self, id: &str, timeout: Duration) -> Option<JobSnapshot> {
        let start = Instant::now();
        loop {
            {
                let snap = self.get(id)?;
                if snap.status.is_terminal() || start.elapsed() > timeout {
                    return Some(snap);
                }
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    }
}

#[derive(Clone)]
pub struct JobCtx {
    pub id: String,
    cancel: Arc<AtomicBool>,
    entries: Arc<Mutex<BTreeMap<String, Entry>>>,
}

impl JobCtx {
    pub fn cancelled(&self) -> bool {
        self.cancel.load(Ordering::SeqCst)
    }

    pub fn check_cancel(&self) -> Result<()> {
        if self.cancelled() {
            Err(crate::error::Error::cancelled("job cancelled by user"))
        } else {
            Ok(())
        }
    }

    fn update<F: FnOnce(&mut JobSnapshot)>(&self, f: F) {
        if let Some(entry) = self.entries.lock().unwrap().get_mut(&self.id) {
            f(&mut entry.snapshot);
        }
    }

    pub fn progress(&self, percent: f32, message: impl Into<String>) {
        let pct = percent.clamp(0.0, 1.0);
        let msg = message.into();
        self.update(|s| {
            s.progress = pct;
            s.message = msg;
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn runs_job_to_completion() {
        let jobs = Jobs::new();
        let id = jobs.spawn("test", |ctx| {
            ctx.progress(0.5, "halfway");
            Ok(serde_json::json!({"ok": true}))
        });
        let snap = jobs.wait(&id, Duration::from_secs(10)).unwrap();
        assert_eq!(snap.status, JobStatus::Succeeded);
        assert!(snap.output.is_some());
    }

    #[test]
    fn reports_failure() {
        let jobs = Jobs::new();
        let id = jobs.spawn("test", |_ctx| Err(crate::error::Error::invalid("boom")));
        let snap = jobs.wait(&id, Duration::from_secs(10)).unwrap();
        assert_eq!(snap.status, JobStatus::Failed);
        assert_eq!(snap.error.unwrap().message, "boom");
    }
}
