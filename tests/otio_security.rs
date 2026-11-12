#![cfg(unix)]
//! OTIO import security through the real stdio server process: client-safe names, logical aliases,
//! permission modes (fail closed), filesystem boundary, and provider-independent invocation
//! (plain JSON-RPC, no vendor SDK anywhere).

use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::time::Duration;

use serde_json::{json, Value};

const BIN: &str = env!("CARGO_BIN_EXE_cinvar");

struct Server {
    child: Child,
    stdin: ChildStdin,
    out: Receiver<String>,
    id: u64,
}

impl Server {
    fn start(project: &Path, config: &Path, env: &[(&str, &str)]) -> Server {
        let mut cmd = Command::new(BIN);
        cmd.args(["mcp", "serve", "--project"])
            .arg(project)
            .env("CINVAR_CONFIG_DIR", config);
        for (k, v) in env {
            cmd.env(k, v);
        }
        let mut child = cmd
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let stdin = child.stdin.take().unwrap();
        let stdout = child.stdout.take().unwrap();
        let (tx, out) = mpsc::channel();
        std::thread::spawn(move || {
            for l in BufReader::new(stdout).lines().map_while(|l| l.ok()) {
                if tx.send(l).is_err() {
                    break;
                }
            }
        });
        Server {
            child,
            stdin,
            out,
            id: 0,
        }
    }

    fn rpc(&mut self, method: &str, params: Value) -> Value {
        self.id += 1;
        let req = json!({"jsonrpc": "2.0", "id": self.id, "method": method, "params": params});
        writeln!(self.stdin, "{req}").unwrap();
        self.stdin.flush().unwrap();
        let line = self
            .out
            .recv_timeout(Duration::from_secs(20))
            .expect("server reply");
        let v: Value = serde_json::from_str(&line).unwrap();
        assert_eq!(v["id"], self.id);
        v
    }

    fn call(&mut self, name: &str, args: Value) -> Value {
        self.rpc("tools/call", json!({"name": name, "arguments": args}))
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn project(dir: &Path, config: &Path) -> std::path::PathBuf {
    let out = Command::new(BIN)
        .args(["project", "create", "--name", "compat", "--dir"])
        .arg(dir)
        .env("CINVAR_CONFIG_DIR", config)
        .output()
        .unwrap();
    assert!(out.status.success());
    dir.join("compat")
}

fn text(reply: &Value) -> String {
    reply["result"]["content"][0]["text"]
        .as_str()
        .unwrap_or("")
        .to_string()
}

fn make_mp4(path: &Path) {
    let st = Command::new("ffmpeg")
        .args([
            "-v",
            "error",
            "-y",
            "-f",
            "lavfi",
            "-i",
            "testsrc=size=160x120:rate=12:duration=1",
            "-pix_fmt",
            "yuv420p",
        ])
        .arg(path)
        .status()
        .unwrap();
    assert!(st.success());
}

fn otio_for(target: &str) -> String {
    json!({"OTIO_SCHEMA": "Timeline.1", "name": "t", "tracks": {"OTIO_SCHEMA": "Stack.1", "children": [
        {"OTIO_SCHEMA": "Track.1", "kind": "Video", "name": "V", "children": [
            {"OTIO_SCHEMA": "Clip.2", "name": "c", "source_range": {"OTIO_SCHEMA": "TimeRange.1",
              "start_time": {"OTIO_SCHEMA": "RationalTime.1", "value": 0.0, "rate": 12.0},
              "duration": {"OTIO_SCHEMA": "RationalTime.1", "value": 12.0, "rate": 12.0}},
             "media_references": {"DEFAULT_MEDIA": {"OTIO_SCHEMA": "ExternalReference.1", "target_url": target}},
             "active_media_reference_key": "DEFAULT_MEDIA"}]}]}})
    .to_string()
}

struct Env {
    _dir: tempfile::TempDir,
    root: std::path::PathBuf,
    project: std::path::PathBuf,
    outside: std::path::PathBuf,
    server: Server,
}

fn env() -> Env {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().to_path_buf();
    let cfg = root.join("cfg");
    let project = project(&root, &cfg);
    let outside = root.join("outside");
    std::fs::create_dir_all(&outside).unwrap();
    let server = Server::start(&project, &cfg, &[]);
    Env {
        _dir: dir,
        root,
        project,
        outside,
        server,
    }
}

/// Imports `otio_target` from an .otio placed inside the project; returns (warnings text, assets copied).
fn import(e: &mut Env, target: &str) -> (String, usize, Value) {
    let otio = e.project.join("t.otio");
    std::fs::write(&otio, otio_for(target)).unwrap();
    let r = e.server.call(
        "cinvar.otio.import",
        json!({"input": otio.to_string_lossy()}),
    );
    let n = std::fs::read_dir(e.project.join("assets"))
        .map(|d| d.count())
        .unwrap_or(0);
    (text(&r), n, r)
}

fn assert_gap(e: &mut Env, target: &str, why: &str) {
    let (body, assets, _) = import(e, target);
    assert!(body.contains(why), "expected '{why}' in: {body}");
    assert_eq!(assets, 0, "nothing may be copied into the project: {body}");
    let leftovers = std::fs::read_dir(e.project.join("tmp"))
        .map(|d| d.count())
        .unwrap_or(0);
    assert_eq!(leftovers, 0, "staged files must be cleaned up");
}

#[test]
fn valid_media_beside_the_timeline_imports() {
    let mut e = env();
    make_mp4(&e.project.join("ok.mp4"));
    let (body, assets, r) = import(&mut e, "ok.mp4");
    assert_ne!(r["result"]["isError"], true, "{body}");
    assert_eq!(assets, 1, "{body}");
    assert!(
        body.contains("\"clips\": 1") || body.contains("\"clips\":1"),
        "{body}"
    );
}

#[test]
fn renamed_text_file_is_rejected_by_content() {
    let mut e = env();
    std::fs::write(
        e.project.join("notes.mp4"),
        "this is not video, it is a secret",
    )
    .unwrap();
    assert_gap(&mut e, "notes.mp4", "rejected");
}

#[test]
fn renamed_executable_is_rejected_by_content() {
    let mut e = env();
    std::fs::copy("/bin/ls", e.project.join("tool.mp4")).unwrap();
    assert_gap(&mut e, "tool.mp4", "rejected");
}

#[test]
fn malformed_media_is_rejected() {
    let mut e = env();
    let good = e.project.join("good.mp4");
    make_mp4(&good);
    let bytes = std::fs::read(&good).unwrap();
    std::fs::remove_file(&good).unwrap();
    std::fs::write(e.project.join("trunc.mp4"), &bytes[..40]).unwrap();
    assert_gap(&mut e, "trunc.mp4", "rejected");
}

#[test]
fn ffconcat_playlist_cannot_smuggle_other_files() {
    let mut e = env();
    std::fs::write(
        e.project.join("list.mp4"),
        "ffconcat version 1.0\nfile '/etc/passwd'\n",
    )
    .unwrap();
    assert_gap(&mut e, "list.mp4", "rejected");
}

#[test]
fn symlink_to_file_outside_project_is_refused() {
    let mut e = env();
    make_mp4(&e.outside.join("real.mp4"));
    std::os::unix::fs::symlink(e.outside.join("real.mp4"), e.project.join("link.mp4")).unwrap();
    assert_gap(&mut e, "link.mp4", "outside the authorised folders");
}

#[test]
fn symlink_to_restricted_directory_is_refused() {
    let mut e = env();
    std::os::unix::fs::symlink("/etc", e.project.join("etc")).unwrap();
    assert_gap(&mut e, "etc/passwd", "outside the authorised folders");
    std::os::unix::fs::symlink("/etc/passwd", e.project.join("pw.mp4")).unwrap();
    assert_gap(&mut e, "pw.mp4", "outside the authorised folders");
}

#[test]
fn relative_traversal_is_refused() {
    let mut e = env();
    make_mp4(&e.outside.join("real.mp4"));
    assert_gap(
        &mut e,
        "../outside/real.mp4",
        "outside the authorised folders",
    );
}

#[test]
fn absolute_path_outside_roots_is_refused() {
    let mut e = env();
    make_mp4(&e.outside.join("real.mp4"));
    let abs = e.outside.join("real.mp4");
    assert_gap(
        &mut e,
        &abs.to_string_lossy(),
        "outside the authorised folders",
    );
    assert_gap(
        &mut e,
        "file:///etc/passwd",
        "outside the authorised folders",
    );
}

#[test]
fn absolute_path_inside_project_is_allowed() {
    let mut e = env();
    let abs = e.project.join("inside.mp4");
    make_mp4(&abs);
    let (body, assets, _) = import(&mut e, &abs.to_string_lossy());
    assert_eq!(assets, 1, "{body}");
}

#[test]
fn missing_media_becomes_a_gap_with_a_warning() {
    let mut e = env();
    assert_gap(&mut e, "nope.mp4", "not found");
}

#[test]
fn otio_file_outside_project_is_refused_for_agents() {
    let mut e = env();
    let otio = e.outside.join("x.otio");
    std::fs::write(&otio, otio_for("a.mp4")).unwrap();
    let r = e.server.call(
        "cinvar.otio.import",
        json!({"input": otio.to_string_lossy()}),
    );
    assert_eq!(r["result"]["isError"], true);
    assert!(text(&r).contains("outside the project"), "{r}");
    // and a symlinked .otio pointing out is refused too
    std::os::unix::fs::symlink(&otio, e.project.join("l.otio")).unwrap();
    let r = e.server.call(
        "cinvar.otio.import",
        json!({"input": e.project.join("l.otio").to_string_lossy()}),
    );
    assert_eq!(r["result"]["isError"], true);
    let _ = &e.root;
}
