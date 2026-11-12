use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use crate::error::{Error, ErrorCode, Result};

#[derive(Debug, Clone)]
pub struct ProcOutput {
    pub code: Option<i32>,
    pub success: bool,
    pub stdout: String,
    pub stderr: String,
}

impl ProcOutput {
    pub fn tail_stderr(&self, lines: usize) -> String {
        let collected: Vec<&str> = self.stderr.lines().collect();
        let start = collected.len().saturating_sub(lines);
        collected[start..].join("\n")
    }
}

pub struct ProcOpts {
    pub cwd: Option<PathBuf>,
    pub timeout: Option<Duration>,
    pub stdin: Option<Vec<u8>>,
    pub env: Vec<(String, String)>,
    pub max_capture: usize,
}

impl Default for ProcOpts {
    fn default() -> Self {
        ProcOpts {
            cwd: None,
            timeout: Some(Duration::from_secs(600)),
            stdin: None,
            env: Vec::new(),
            max_capture: 8 * 1024 * 1024,
        }
    }
}

impl ProcOpts {
    pub fn with_timeout(mut self, secs: u64) -> Self {
        self.timeout = Some(Duration::from_secs(secs));
        self
    }
    pub fn in_dir(mut self, dir: impl Into<PathBuf>) -> Self {
        self.cwd = Some(dir.into());
        self
    }
    pub fn with_stdin(mut self, data: impl Into<Vec<u8>>) -> Self {
        self.stdin = Some(data.into());
        self
    }
}

/// Spawn, retrying briefly on ETXTBSY ("Text file busy"). Linux refuses to exec a file that some
/// other thread still holds open for writing, which happens when a script is written and run at once.
pub fn spawn_retrying(cmd: &mut Command) -> std::io::Result<std::process::Child> {
    let mut attempt = 0u64;
    loop {
        match cmd.spawn() {
            Err(e) if e.raw_os_error() == Some(26) && attempt < 10 => {
                attempt += 1;
                std::thread::sleep(Duration::from_millis(25 * attempt));
            }
            other => return other,
        }
    }
}

pub fn run(program: &Path, args: &[String], opts: &ProcOpts) -> Result<ProcOutput> {
    let mut cmd = Command::new(program);
    cmd.args(args);
    if let Some(cwd) = &opts.cwd {
        cmd.current_dir(cwd);
    }
    cmd.stdin(if opts.stdin.is_some() {
        Stdio::piped()
    } else {
        Stdio::null()
    });
    cmd.stdout(Stdio::piped());
    cmd.stderr(Stdio::piped());
    cmd.env_clear();
    for key in [
        "PATH",
        "HOME",
        "TMPDIR",
        "TEMP",
        "TMP",
        "LANG",
        "LC_ALL",
        "USERPROFILE",
        "SYSTEMROOT",
        "FONTCONFIG_PATH",
    ] {
        if let Ok(value) = std::env::var(key) {
            cmd.env(key, value);
        }
    }
    for (key, value) in &opts.env {
        cmd.env(key, value);
    }

    let mut child = spawn_retrying(&mut cmd).map_err(|e| {
        Error::new(
            ErrorCode::DependencyMissing,
            format!("could not start {}: {e}", program.display()),
        )
        .with_hint("run the cinvar doctor command to check installed dependencies")
    })?;

    if let Some(data) = &opts.stdin {
        if let Some(mut stdin) = child.stdin.take() {
            let payload = data.clone();
            std::thread::spawn(move || {
                let _ = stdin.write_all(&payload);
            });
        }
    } else {
        drop(child.stdin.take());
    }

    let out_pipe = child.stdout.take();
    let err_pipe = child.stderr.take();
    let out_handle = out_pipe.map(|mut pipe| {
        std::thread::spawn(move || {
            let mut buf = String::new();
            let _ = pipe.read_to_string(&mut buf);
            buf
        })
    });
    let err_handle = err_pipe.map(|mut pipe| {
        std::thread::spawn(move || {
            let mut buf = String::new();
            let _ = pipe.read_to_string(&mut buf);
            buf
        })
    });

    let started = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => {}
            Err(e) => return Err(Error::io(format!("waiting for process failed: {e}"))),
        }
        if let Some(limit) = opts.timeout {
            if started.elapsed() > limit {
                let _ = child.kill();
                let _ = child.wait();
                return Err(Error::new(
                    ErrorCode::Io,
                    format!("command timed out after {}s", limit.as_secs()),
                )
                .with_hint("increase the timeout or reduce the workload"));
            }
        }
        std::thread::sleep(Duration::from_millis(15));
    };

    let mut stdout = out_handle.and_then(|h| h.join().ok()).unwrap_or_default();
    let mut stderr = err_handle.and_then(|h| h.join().ok()).unwrap_or_default();
    if stdout.len() > opts.max_capture {
        stdout.truncate(opts.max_capture);
    }
    if stderr.len() > opts.max_capture {
        stderr.truncate(opts.max_capture);
    }

    Ok(ProcOutput {
        code: status.code(),
        success: status.success(),
        stdout,
        stderr,
    })
}

pub fn which(program: &str) -> Option<PathBuf> {
    let candidate = Path::new(program);
    if candidate.is_absolute() || program.contains('/') || program.contains('\\') {
        return if candidate.is_file() {
            Some(candidate.to_path_buf())
        } else {
            None
        };
    }
    let path_var = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path_var) {
        let full = dir.join(program);
        if full.is_file() {
            return Some(full);
        }
    }
    None
}

pub fn require(program: &str) -> Result<PathBuf> {
    which(program).ok_or_else(|| {
        Error::new(
            ErrorCode::DependencyMissing,
            format!("{program} is not installed"),
        )
        .with_hint("run cinvar setup for guided installation")
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn no_shell_interpretation() {
        let out = run(
            Path::new("echo"),
            &["a; rm -rf /".to_string()],
            &ProcOpts::default(),
        )
        .unwrap();
        assert_eq!(out.stdout.trim(), "a; rm -rf /");
    }

    #[cfg(unix)]
    #[test]
    fn times_out() {
        let opts = ProcOpts::default().with_timeout(1);
        let res = run(Path::new("sleep"), &["5".to_string()], &opts);
        assert!(res.is_err());
    }
}
