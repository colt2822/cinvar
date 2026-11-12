//! Advisory cross-process project lock.
//!
//! Every tool call is load -> edit -> save. Two processes (an MCP session and the CLI, or two
//! assistants) doing that at once would silently lose one edit. The lock is a file created with
//! `create_new`, which is atomic on every supported platform and needs no extra dependency.
//! A lock whose owner process is gone, or that is older than `STALE_AFTER`, is taken over.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

use crate::error::{Error, ErrorCode, Result};

const STALE_AFTER: Duration = Duration::from_secs(300);
const POLL: Duration = Duration::from_millis(25);

#[derive(Debug)]
pub struct ProjectLock {
    path: PathBuf,
}

fn pid_alive(pid: u32) -> bool {
    #[cfg(target_os = "linux")]
    {
        Path::new(&format!("/proc/{pid}")).exists()
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = pid;
        true // cannot tell cheaply; rely on the age check
    }
}

fn lock_is_stale(path: &Path) -> bool {
    let owner: Option<u32> = std::fs::read_to_string(path)
        .ok()
        .and_then(|t| t.lines().next().and_then(|l| l.trim().parse().ok()));
    if let Some(pid) = owner {
        if pid != std::process::id() && !pid_alive(pid) {
            return true;
        }
        // On Linux a live owner is authoritative: never steal a lock from a running process.
        if cfg!(target_os = "linux") && pid_alive(pid) {
            return false;
        }
    }
    std::fs::metadata(path)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|m| SystemTime::now().duration_since(m).ok())
        .map(|age| age > STALE_AFTER)
        .unwrap_or(false)
}

impl ProjectLock {
    pub fn acquire(project_dir: &Path, wait: Duration) -> Result<ProjectLock> {
        let path = project_dir.join(".cinvar.lock");
        let started = Instant::now();
        loop {
            match std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&path)
            {
                Ok(mut file) => {
                    use std::io::Write;
                    let _ = writeln!(file, "{}", std::process::id());
                    return Ok(ProjectLock { path });
                }
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                    if lock_is_stale(&path) {
                        // Take over: remove and retry; if another process wins the race we loop again.
                        let _ = std::fs::remove_file(&path);
                        continue;
                    }
                    if started.elapsed() >= wait {
                        return Err(Error::new(
                            ErrorCode::Io,
                            "the project is being edited by another CINVAR process",
                        )
                        .with_hint("wait a moment and retry; if no other CINVAR is running, delete .cinvar.lock in the project folder"));
                    }
                    std::thread::sleep(POLL);
                }
                Err(e) => return Err(Error::io(format!("cannot create project lock: {e}"))),
            }
        }
    }
}

impl Drop for ProjectLock {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn second_holder_waits_then_fails_and_release_frees_it() {
        let dir = tempfile::tempdir().unwrap();
        let first = ProjectLock::acquire(dir.path(), Duration::from_millis(50)).unwrap();
        let err = ProjectLock::acquire(dir.path(), Duration::from_millis(120)).unwrap_err();
        assert!(err.message.contains("another CINVAR process"), "{err}");
        drop(first);
        assert!(ProjectLock::acquire(dir.path(), Duration::from_millis(50)).is_ok());
    }

    #[test]
    fn a_dead_owner_is_taken_over() {
        let dir = tempfile::tempdir().unwrap();
        // pid 4294967 is far above pid_max on Linux, so nothing owns it.
        std::fs::write(dir.path().join(".cinvar.lock"), "4294967\n").unwrap();
        if cfg!(target_os = "linux") {
            assert!(ProjectLock::acquire(dir.path(), Duration::from_millis(50)).is_ok());
        }
    }

    #[test]
    fn waiters_get_the_lock_when_it_is_released() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().to_path_buf();
        let held = ProjectLock::acquire(&path, Duration::from_millis(10)).unwrap();
        let p2 = path.clone();
        let t =
            std::thread::spawn(move || ProjectLock::acquire(&p2, Duration::from_secs(3)).is_ok());
        std::thread::sleep(Duration::from_millis(150));
        drop(held);
        assert!(t.join().unwrap());
    }
}
