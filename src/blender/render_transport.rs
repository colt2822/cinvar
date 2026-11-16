//! ssh transport for distributed Blender rendering.
//!
//! Everything that touches another machine goes through here: one BatchMode ssh invocation per
//! operation, a hard timeout on every one of them, and payloads carried as tar streams this crate
//! builds itself. No worker needs anything installed beyond a POSIX shell, tar, base64 and xz;
//! Blender itself arrives as a portable tarball cached per worker under
//! $HOME/.cache/cinvar/blender-dist/<version>/.
//!
//! Collected frames come back as base64 because a tar stream on stdout would be mangled by any
//! text conversion; the base64 decode is exact and a truncated stream fails its tar checksum.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use serde::Serialize;

use crate::error::{Error, ErrorCode, Result};

/// Directory under the worker home that holds every CINVAR artifact on that machine.
pub const REMOTE_CACHE: &str = ".cache/cinvar";

/// Host-side cache mirroring the worker layout (portable tarballs, staged libraries).
pub fn local_cache() -> PathBuf {
    crate::util::paths::home_dir().join(".cache/cinvar")
}

const SSH_BIN: &str = "ssh";
const BASE_URL: &str = "https://download.blender.org/release";

/// Guards every value that is pasted into a remote shell command line.
fn token<'a>(value: &'a str, what: &str) -> Result<&'a str> {
    let ok = !value.is_empty()
        && value
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-' | '/'));
    if !ok {
        return Err(Error::new(
            ErrorCode::InvalidInput,
            format!("{what} {value:?} contains characters that are unsafe in a remote command"),
        ));
    }
    Ok(value)
}

struct RawOut {
    code: Option<i32>,
    success: bool,
    stdout: Vec<u8>,
    stderr: String,
}

impl RawOut {
    fn text(&self) -> String {
        String::from_utf8_lossy(&self.stdout).to_string()
    }

    fn tail(&self, lines: usize) -> String {
        let collected: Vec<&str> = self.stderr.lines().collect();
        collected[collected.len().saturating_sub(lines)..].join("\n")
    }
}

/// Runs one command locally, optionally with stdin from bytes or a file, under a hard timeout.
fn spawn_capture(
    program: &str,
    args: &[String],
    stdin: Option<Vec<u8>>,
    stdin_file: Option<&Path>,
    timeout_secs: u64,
) -> Result<RawOut> {
    let mut cmd = Command::new("timeout");
    cmd.arg(timeout_secs.to_string()).arg(program).args(args);
    cmd.stdout(Stdio::piped()).stderr(Stdio::piped());
    if stdin.is_some() || stdin_file.is_some() {
        cmd.stdin(Stdio::piped());
    } else {
        cmd.stdin(Stdio::null());
    }
    let mut child = cmd
        .spawn()
        .map_err(|e| Error::new(ErrorCode::Io, format!("could not start {program}: {e}")))?;

    // Feed stdin on a thread so a slow remote reader cannot deadlock against our stdout read.
    let writer = if let Some(bytes) = stdin {
        let mut handle = child.stdin.take();
        Some(std::thread::spawn(move || {
            use std::io::Write;
            if let Some(mut pipe) = handle.take() {
                let _ = pipe.write_all(&bytes);
            }
        }))
    } else if let Some(path) = stdin_file {
        let mut handle = child.stdin.take();
        let file = std::fs::File::open(path).ok();
        Some(std::thread::spawn(move || {
            use std::io::copy;
            if let (Some(mut pipe), Some(mut file)) = (handle.take(), file) {
                let _ = copy(&mut file, &mut pipe);
            }
        }))
    } else {
        None
    };

    let out = child
        .wait_with_output()
        .map_err(|e| Error::new(ErrorCode::Io, format!("{program} could not be awaited: {e}")))?;
    if let Some(handle) = writer {
        let _ = handle.join();
    }
    let code = out.status.code();
    Ok(RawOut {
        success: out.status.success(),
        code,
        stdout: out.stdout,
        stderr: String::from_utf8_lossy(&out.stderr).to_string(),
    })
}

/// One ssh invocation against a worker. The remote argument is a shell command line.
fn ssh(
    host: &str,
    remote: &str,
    stdin: Option<Vec<u8>>,
    stdin_file: Option<&Path>,
    timeout_secs: u64,
) -> Result<RawOut> {
    token(host, "worker host")?;
    let args: Vec<String> = vec![
        "-o".into(),
        "BatchMode=yes".into(),
        "-o".into(),
        "ConnectTimeout=10".into(),
        "-o".into(),
        "ServerAliveInterval=15".into(),
        "-o".into(),
        "ServerAliveCountMax=4".into(),
        host.into(),
        remote.into(),
    ];
    let out = spawn_capture(SSH_BIN, &args, stdin, stdin_file, timeout_secs)?;
    if out.code == Some(124) {
        return Err(Error::new(
            ErrorCode::Render,
            format!("ssh {host} timed out after {timeout_secs}s"),
        ));
    }
    Ok(out)
}

/// Fails if the worker cannot be reached at all (ssh auth, DNS, host down).
pub fn reachable(host: &str) -> Result<()> {
    let out = ssh(host, "true", None, None, 25)?;
    if out.success {
        return Ok(());
    }
    Err(Error::new(
        ErrorCode::DependencyMissing,
        format!(
            "worker {host} is not reachable over ssh: {}",
            if out.stderr.trim().is_empty() {
                format!("exit {:?}", out.code)
            } else {
                out.tail(4)
            }
        ),
    )
    .with_hint("make sure passwordless ssh works: ssh -o BatchMode=yes HOST true"))
}

/// Runs a remote command and returns its stdout, failing on a non-zero exit.
pub fn run_text(host: &str, remote: &str, timeout_secs: u64) -> Result<String> {
    let out = ssh(host, remote, None, None, timeout_secs)?;
    if !out.success {
        return Err(Error::new(
            ErrorCode::Render,
            format!(
                "remote command on {host} failed (exit {:?}): {}",
                out.code,
                out.tail(6)
            ),
        ));
    }
    Ok(out.text())
}

/// Runs a remote command whose failure is data, not an error: returns (succeeded, stdout).
pub fn run_allow_failure(host: &str, remote: &str, timeout_secs: u64) -> Result<(bool, String)> {
    let out = ssh(host, remote, None, None, timeout_secs)?;
    Ok((out.success, out.text()))
}

/// Facts a worker reports about itself; used for load balancing and disk checks.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct WorkerFacts {
    pub host: String,
    pub cores: u32,
    pub home: String,
    pub avail_kb: u64,
}

/// Shell one-liner the worker answers with key=value lines.
pub const FACTS_SCRIPT: &str = "printf 'cores=%s\\nhome=%s\\navail_kb=%s\\n' \"$(nproc)\" \"$HOME\" \"$(df -Pk \"$HOME\" | awk 'NR==2 {print $4}')\"";

pub fn facts(host: &str) -> Result<WorkerFacts> {
    let text = run_text(host, FACTS_SCRIPT, 40)?;
    parse_facts(host, &text)
}

/// Pure parser for the facts payload.
pub fn parse_facts(host: &str, text: &str) -> Result<WorkerFacts> {
    let mut cores = None;
    let mut home = None;
    let mut avail_kb = 0u64;
    for line in text.lines() {
        if let Some((key, value)) = line.trim().split_once('=') {
            match key {
                "cores" => cores = value.trim().parse::<u32>().ok(),
                "home" => home = Some(value.trim().to_string()),
                "avail_kb" => avail_kb = value.trim().parse().unwrap_or(0),
                _ => {}
            }
        }
    }
    let cores = cores.ok_or_else(|| {
        Error::new(
            ErrorCode::DependencyMissing,
            format!("worker {host} did not report its core count"),
        )
    })?;
    Ok(WorkerFacts {
        host: host.to_string(),
        cores: cores.max(1),
        home: home.unwrap_or_else(|| "~".to_string()),
        avail_kb,
    })
}

/// Uploads a tar stream and extracts it into a remote directory (created if needed).
pub fn push_tar(host: &str, tar: &[u8], remote_dir: &str, timeout_secs: u64) -> Result<()> {
    let remote = format!("mkdir -p {remote_dir} && tar -x -C {remote_dir}");
    let out = ssh(host, &remote, Some(tar.to_vec()), None, timeout_secs)?;
    if !out.success {
        return Err(Error::new(
            ErrorCode::Render,
            format!("pushing to {host}:{remote_dir} failed: {}", out.tail(6)),
        ));
    }
    Ok(())
}

/// Collects a remote directory as a tar stream (base64 on the wire).
pub fn collect_dir(
    host: &str,
    remote_dir: &str,
    timeout_secs: u64,
) -> Result<Vec<super::render_tar::TarFile>> {
    let remote = format!("tar -c -C {remote_dir} . | base64 -w0");
    let out = ssh(host, &remote, None, None, timeout_secs)?;
    if !out.success {
        return Err(Error::new(
            ErrorCode::Render,
            format!("collecting {host}:{remote_dir} failed: {}", out.tail(6)),
        ));
    }
    decode_archive(out.stdout.as_slice())
}

fn decode_archive(base64_bytes: &[u8]) -> Result<Vec<super::render_tar::TarFile>> {
    use base64::Engine;
    let text: String = String::from_utf8_lossy(base64_bytes)
        .chars()
        .filter(|c| !c.is_whitespace())
        .collect();
    let decoded = base64::engine::general_purpose::STANDARD
        .decode(text.as_bytes())
        .map_err(|e| {
            Error::new(
                ErrorCode::Protocol,
                format!("collected archive was not valid base64: {e}"),
            )
        })?;
    super::render_tar::read(&decoded)
}

/// Parses ldconfig output into soname -> path. Pure, so library staging is testable.
pub fn parse_ldconfig(stdout: &str) -> HashMap<String, PathBuf> {
    let mut map = HashMap::new();
    for line in stdout.lines() {
        let line = line.trim();
        let Some(name) = line.split_whitespace().next() else {
            continue;
        };
        let Some(path) = line.split("=>").nth(1).map(str::trim) else {
            continue;
        };
        if path.contains('/') {
            map.entry(name.to_string())
                .or_insert_with(|| PathBuf::from(path));
        }
    }
    map
}

/// Parses ldd output for libraries the dynamic loader cannot resolve.
pub fn parse_ldd_missing(stdout: &str) -> Vec<String> {
    let mut missing: Vec<String> = stdout
        .lines()
        .filter(|l| l.contains("not found"))
        .filter_map(|l| l.split_whitespace().next().map(str::to_string))
        .filter(|l| l.contains(".so"))
        .collect();
    missing.sort();
    missing.dedup();
    missing
}

fn local_ldcache() -> Result<HashMap<String, PathBuf>> {
    let out = spawn_capture("ldconfig", &["-p".to_string()], None, None, 60)?;
    if !out.success {
        return Err(Error::new(
            ErrorCode::DependencyMissing,
            format!("ldconfig -p failed: {}", out.tail(3)),
        ));
    }
    Ok(parse_ldconfig(&out.text()))
}

/// Copies shared objects (resolved on this machine) into a staging directory.
pub fn stage_libs(
    libs: &[String],
    into: &Path,
    cache: &HashMap<String, PathBuf>,
) -> Result<Vec<String>> {
    crate::util::paths::ensure_dir(into)?;
    let mut staged = Vec::new();
    let mut unknown = Vec::new();
    for lib in libs {
        match cache.get(lib) {
            Some(source) => {
                let target = into.join(lib);
                std::fs::copy(source, &target).map_err(|e| {
                    Error::new(
                        ErrorCode::Io,
                        format!("staging {lib} from {}: {e}", source.display()),
                    )
                })?;
                staged.push(lib.clone());
            }
            None => unknown.push(lib.clone()),
        }
    }
    if !unknown.is_empty() {
        return Err(Error::new(
            ErrorCode::DependencyMissing,
            format!(
                "these libraries are needed by Blender on the worker but are not installed here either: {}",
                unknown.join(", ")
            ),
        )
        .with_hint("install them once on the coordinator (for example libxrender1 libxi6 libgl1) so they can be staged for workers"));
    }
    Ok(staged)
}

/// Blender portable tarball URL for a version like 4.0.2.
pub fn tarball_url(version: &str) -> Result<String> {
    let mut parts = version.split('.');
    let major = parts.next().unwrap_or_default();
    let minor = parts.next().unwrap_or_default();
    if major.is_empty() || minor.is_empty() || !minor.chars().all(|c| c.is_ascii_digit()) {
        return Err(Error::new(
            ErrorCode::InvalidInput,
            format!("cannot derive a Blender download for version {version:?}"),
        ));
    }
    token(version, "blender version")?;
    Ok(format!(
        "{BASE_URL}/Blender{major}.{minor}/blender-{version}-linux-x64.tar.xz"
    ))
}

/// Downloads and caches the portable tarball once per Blender version on the coordinator.
pub fn ensure_tarball(version: &str) -> Result<PathBuf> {
    let url = tarball_url(version)?;
    let dir = local_cache().join("blender-dist").join(version);
    crate::util::paths::ensure_dir(&dir)?;
    let target = dir.join(format!("blender-{version}-linux-x64.tar.xz"));
    if std::fs::metadata(&target).map(|m| m.len()).unwrap_or(0) > 10_000_000 {
        return Ok(target);
    }
    let partial = dir.join("download.part");
    let out = spawn_capture(
        "curl",
        &[
            "-sSL".to_string(),
            "--max-time".to_string(),
            "1800".to_string(),
            "-o".to_string(),
            partial.to_string_lossy().to_string(),
            url.clone(),
        ],
        None,
        None,
        1800,
    )?;
    if !out.success {
        return Err(Error::new(
            ErrorCode::DependencyMissing,
            format!("could not download {url}: {}", out.tail(3)),
        ));
    }
    std::fs::rename(&partial, &target)
        .map_err(|e| Error::new(ErrorCode::Io, format!("caching the Blender tarball: {e}")))?;
    Ok(target)
}

/// A worker with a working, cached portable Blender.
#[derive(Debug, Clone, Serialize)]
pub struct RemoteBlender {
    pub host: String,
    pub version: String,
    /// Remote directory holding the blender executable (HOME-relative shell expression).
    pub dir: String,
    /// Libraries staged next to it to satisfy the worker dynamic loader.
    pub libs: Vec<String>,
    /// True when nothing had to be transferred (the worker already had it).
    pub cached: bool,
}

pub fn remote_dist_dir(version: &str) -> String {
    format!("$HOME/{REMOTE_CACHE}/blender-dist/{version}")
}

fn remote_version(host: &str, version: &str) -> Result<Option<String>> {
    let dir = remote_dist_dir(version);
    let probe = format!(
        "if [ -x {dir}/blender ]; then LD_LIBRARY_PATH={dir}/libs {dir}/blender --version 2>&1 | head -1; fi"
    );
    let out = ssh(host, &probe, None, None, 120)?;
    Ok(crate::blender::parse_version(&out.text()).map(|(_, _, v)| v))
}

/// Lists the shared objects the portable Blender cannot resolve on the worker.
fn remote_missing_libs(host: &str, version: &str) -> Result<Vec<String>> {
    let dir = remote_dist_dir(version);
    let sweep = format!(
        "D={dir}; LD_LIBRARY_PATH=$D/libs ldd $D/blender 2>&1; for f in $D/libs/*.so*; do [ -e \"$f\" ] && LD_LIBRARY_PATH=$D/libs ldd \"$f\" 2>&1; done"
    );
    let out = ssh(host, &sweep, None, None, 180)?;
    Ok(parse_ldd_missing(&out.text()))
}

/// Provisions (or verifies) the portable Blender on one worker, converging its library closure.
pub fn provision(host: &str, version: &str, tarball: &Path) -> Result<RemoteBlender> {
    token(version, "blender version")?;
    let dir = remote_dist_dir(version);
    let libs_dir = local_cache()
        .join("blender-dist")
        .join(version)
        .join("libs");

    if let Some(found) = remote_version(host, version)? {
        if found == version {
            return Ok(RemoteBlender {
                host: host.to_string(),
                version: version.to_string(),
                dir,
                libs: Vec::new(),
                cached: true,
            });
        }
    }

    // Push the tarball unless the executable is already unpacked (an earlier run may have only
    // failed on libraries, and unpacking 600 MB again would be waste).
    let present = ssh(host, &format!("test -x {dir}/blender"), None, None, 40)?;
    if !present.success {
        let extract = format!("mkdir -p {dir} && tar -xJ -C {dir} --strip-components=1");
        let out = ssh(host, &extract, None, Some(tarball), 1800)?;
        if !out.success {
            return Err(Error::new(
                ErrorCode::Render,
                format!("unpacking Blender on {host} failed: {}", out.tail(6)),
            ));
        }
    }

    // Converge the dynamic-loader closure: each round pushes what the last sweep found missing.
    let cache = local_ldcache()?;
    let mut staged: Vec<String> = Vec::new();
    for _round in 0..5 {
        let missing = remote_missing_libs(host, version)?;
        let fresh: Vec<String> = missing
            .into_iter()
            .filter(|lib| !staged.contains(lib))
            .collect();
        if fresh.is_empty() {
            break;
        }
        stage_libs(&fresh, &libs_dir, &cache)?;
        let files: Vec<super::render_tar::TarFile> = fresh
            .iter()
            .map(|lib| {
                let bytes = std::fs::read(libs_dir.join(lib)).unwrap_or_default();
                super::render_tar::TarFile::new(format!("libs/{lib}"), bytes)
            })
            .collect();
        let archive = super::render_tar::build(&files)?;
        push_tar(host, &archive, &dir, 600)?;
        staged.extend(fresh);
    }

    match remote_version(host, version)? {
        Some(found) if found == version => Ok(RemoteBlender {
            host: host.to_string(),
            version: version.to_string(),
            dir,
            libs: staged,
            cached: false,
        }),
        other => Err(Error::new(
            ErrorCode::DependencyMissing,
            format!(
                "portable Blender {version} still does not run on {host} (reported {:?}); libraries staged: {}",
                other,
                if staged.is_empty() {
                    "none".to_string()
                } else {
                    staged.join(", ")
                }
            ),
        )),
    }
}

/// Where a worker keeps one job's scratch directory (HOME-relative shell expression).
pub fn remote_job_dir(hash: &str) -> String {
    format!("$HOME/{REMOTE_CACHE}/jobs/{hash}")
}

/// Removes a worker scratch directory. The path is built only from validated tokens.
pub fn cleanup(host: &str, hash: &str) -> Result<()> {
    token(hash, "job hash")?;
    let dir = remote_job_dir(hash);
    let out = ssh(host, &format!("rm -rf {dir}"), None, None, 180)?;
    if !out.success {
        return Err(Error::new(
            ErrorCode::Render,
            format!("cleaning {host}:{dir} failed: {}", out.tail(3)),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tarball_urls_follow_blender_layout() {
        assert_eq!(
            tarball_url("4.0.2").unwrap(),
            "https://download.blender.org/release/Blender4.0/blender-4.0.2-linux-x64.tar.xz"
        );
        assert_eq!(
            tarball_url("3.6.23").unwrap(),
            "https://download.blender.org/release/Blender3.6/blender-3.6.23-linux-x64.tar.xz"
        );
        assert!(tarball_url("not-a-version").is_err());
        assert!(tarball_url("4.0.2; rm -rf /").is_err());
    }

    #[test]
    fn unsafe_tokens_are_refused() {
        assert!(token("n1", "host").is_ok());
        assert!(token("n1; reboot", "host").is_err());
        assert!(token("", "host").is_err());
        assert!(token("n1 && curl evil", "host").is_err());
    }

    #[test]
    fn facts_are_parsed_from_the_worker_answer() {
        // Captured from n4: echo of FACTS_SCRIPT output.
        let text = "cores=12\nhome=/home/colt2822\navail_kb=36155220\n";
        let facts = parse_facts("n4", text).unwrap();
        assert_eq!(facts.cores, 12);
        assert_eq!(facts.home, "/home/colt2822");
        assert_eq!(facts.avail_kb, 36_155_220);
        assert_eq!(parse_facts("n1", "garbage").unwrap_err().code, ErrorCode::DependencyMissing);
        assert_eq!(parse_facts("n1", "cores=0").unwrap().cores, 1, "a worker never weighs zero");
    }

    /// Captured from ldconfig -p on the coordinator (Ubuntu 24.04).
    const CAPTURED_LDCONFIG: &str = "\tlibXrender.so.1 (libc6,x86-64) => /lib/x86_64-linux-gnu/libXrender.so.1
\tlibGL.so.1 (libc6,x86-64) => /lib/x86_64-linux-gnu/libGL.so.1
\tlibGLX.so.0 (libc6,x86-64) => /lib/x86_64-linux-gnu/libGLX.so.0
\tlibc.so.6 (libc6,x86-64) => /lib/x86_64-linux-gnu/libc.so.6
\tlibXrender.so.1 (libc6) => /lib/i386-linux-gnu/libXrender.so.1
";

    #[test]
    fn ldconfig_map_keeps_the_first_resolution() {
        let map = parse_ldconfig(CAPTURED_LDCONFIG);
        assert_eq!(
            map.get("libXrender.so.1").unwrap(),
            Path::new("/lib/x86_64-linux-gnu/libXrender.so.1")
        );
        assert!(map.get("libc.so.6").is_some());
        assert_eq!(map.len(), 4);
        assert!(parse_ldconfig("").is_empty());
    }

    /// Captured from ldd on a worker that had no X client libraries (Ubuntu 26.04 server).
    const CAPTURED_LDD: &str = "\tlinux-vdso.so.1 (0x00007ffce1b9c000)
\tlibGL.so.1 => not found
\tlibICE.so.6 => not found
\tlibSM.so.6 => not found
\tlibXfixes.so.3 => not found
\tlibXi.so.6 => not found
\tlibXrender.so.1 => not found
\tlibXxf86vm.so.1 => not found
\tlibm.so.6 => /lib/x86_64-linux-gnu/libm.so.6 (0x00007f0f0e3e2000)
";

    #[test]
    fn ldd_missing_lists_only_unresolved_libraries() {
        assert_eq!(
            parse_ldd_missing(CAPTURED_LDD),
            vec![
                "libGL.so.1",
                "libICE.so.6",
                "libSM.so.6",
                "libXfixes.so.3",
                "libXi.so.6",
                "libXrender.so.1",
                "libXxf86vm.so.1"
            ]
        );
        assert!(parse_ldd_missing("all resolved").is_empty());
    }

    #[test]
    fn staged_libraries_are_copied_and_unknowns_are_named() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("source.so.1");
        std::fs::write(&source, b"ELF-ish").unwrap();
        let cache: HashMap<String, PathBuf> =
            [("libfake.so.1".to_string(), source)].into_iter().collect();
        let into = dir.path().join("libs");
        let staged = stage_libs(&["libfake.so.1".to_string()], &into, &cache).unwrap();
        assert_eq!(staged, vec!["libfake.so.1"]);
        assert_eq!(std::fs::read(into.join("libfake.so.1")).unwrap(), b"ELF-ish");

        let err = stage_libs(&["libnope.so.9".to_string()], &into, &cache).unwrap_err();
        assert_eq!(err.code, ErrorCode::DependencyMissing);
        assert!(err.message.contains("libnope.so.9"));
    }

    #[test]
    fn base64_archive_roundtrip_survives_line_wrapping() {
        use base64::Engine;
        let files = vec![
            super::super::render_tar::TarFile::new("frames/frame_0001.png", vec![1u8, 2, 3]),
            super::super::render_tar::TarFile::new("frames/frame_0002.png", vec![9u8; 900]),
        ];
        let archive = super::super::render_tar::build(&files).unwrap();
        let encoded = base64::engine::general_purpose::STANDARD.encode(&archive);
        let wrapped = format!("{}\n{}\n", &encoded[..40], &encoded[40..]);
        assert_eq!(decode_archive(wrapped.as_bytes()).unwrap(), files);
        let err = decode_archive(b"not base64!!").unwrap_err();
        assert_eq!(err.code, ErrorCode::Protocol);
    }

    #[test]
    fn remote_paths_are_home_relative_and_version_scoped() {
        assert_eq!(
            remote_dist_dir("4.0.2"),
            "$HOME/.cache/cinvar/blender-dist/4.0.2"
        );
        assert_eq!(remote_job_dir("deadbeef"), "$HOME/.cache/cinvar/jobs/deadbeef");
    }
}
