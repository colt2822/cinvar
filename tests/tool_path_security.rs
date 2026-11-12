#![cfg(unix)]
//! Agent-facing tool path security through the real stdio server process: client-safe names, logical aliases,
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

fn setup() -> (tempfile::TempDir, std::path::PathBuf, Server) {
    let dir = tempfile::tempdir().unwrap();
    let cfg = dir.path().join("cfg");
    let p = project(dir.path(), &cfg);
    // Agents are project-only by default; these tests deliberately import from the temp dir.
    let root = dir.path().to_string_lossy().to_string();
    let s = Server::start(&p, &cfg, &[("CINVAR_MEDIA_ROOTS", root.as_str())]);
    (dir, p, s)
}

#[test]
fn media_import_copies_only_real_media() {
    let (dir, p, mut s) = setup();
    std::fs::write(dir.path().join("secret.txt"), "password=hunter2").unwrap();
    std::fs::write(dir.path().join("disguised.mp4"), "password=hunter2").unwrap();
    for f in ["secret.txt", "disguised.mp4"] {
        let r = s.call(
            "cinvar.media.import",
            json!({"path": dir.path().join(f).to_string_lossy()}),
        );
        assert_eq!(r["result"]["isError"], true, "{f}: {r}");
    }
    assert_eq!(std::fs::read_dir(p.join("assets")).unwrap().count(), 0);
    make_mp4(&dir.path().join("real.mp4"));
    let r = s.call(
        "cinvar.media.import",
        json!({"path": dir.path().join("real.mp4").to_string_lossy()}),
    );
    assert_ne!(r["result"]["isError"], true, "{r}");
}

#[test]
fn import_folder_skips_disguised_files_and_symlinks_to_secrets() {
    let (dir, p, mut s) = setup();
    let media = dir.path().join("footage");
    std::fs::create_dir_all(&media).unwrap();
    make_mp4(&media.join("a.mp4"));
    std::fs::write(media.join("fake.mp4"), "not video").unwrap();
    std::fs::write(dir.path().join("secret"), "token").unwrap();
    std::os::unix::fs::symlink(dir.path().join("secret"), media.join("link.mp4")).unwrap();
    let r = s.call(
        "cinvar.media.import_folder",
        json!({"path": media.to_string_lossy()}),
    );
    let body = text(&r);
    assert!(body.contains("skipped, not real media"), "{body}");
    assert_eq!(
        std::fs::read_dir(p.join("assets")).unwrap().count(),
        1,
        "{body}"
    );
}

#[test]
fn otio_export_cannot_overwrite_files_outside_the_project() {
    let (dir, p, mut s) = setup();
    let t = s.call("cinvar.timeline.create", json!({"name": "Main"}));
    assert_ne!(t["result"]["isError"], true, "{t}");
    let victim = dir.path().join("victim.txt");
    std::fs::write(&victim, "precious").unwrap();
    for target in [
        victim.to_string_lossy().to_string(),
        "../victim.otio".to_string(),
        "/tmp/cinvar-escape.otio".to_string(),
    ] {
        let r = s.call("cinvar.otio.export", json!({"output": target}));
        assert_eq!(r["result"]["isError"], true, "{target}: {r}");
    }
    assert_eq!(std::fs::read_to_string(&victim).unwrap(), "precious");
    assert!(!dir.path().join("victim.otio").exists());
    assert!(!Path::new("/tmp/cinvar-escape.otio").exists());
    // a non-.otio name inside the project is refused too; a proper one works
    let r = s.call(
        "cinvar.otio.export",
        json!({"output": p.join("notes.txt").to_string_lossy()}),
    );
    assert_eq!(r["result"]["isError"], true, "{r}");
    let r = s.call(
        "cinvar.otio.export",
        json!({"output": p.join("exports/cut.otio").to_string_lossy()}),
    );
    assert_ne!(r["result"]["isError"], true, "{r}");
}

#[test]
fn media_roots_confine_agent_file_tools_when_configured() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = dir.path().join("cfg");
    let p = project(dir.path(), &cfg);
    let allowed = dir.path().join("allowed");
    let other = dir.path().join("other");
    std::fs::create_dir_all(&allowed).unwrap();
    std::fs::create_dir_all(&other).unwrap();
    std::fs::create_dir_all(&cfg).unwrap();
    make_mp4(&allowed.join("ok.mp4"));
    make_mp4(&other.join("no.mp4"));
    std::fs::write(
        cfg.join("config.toml"),
        format!("[security]\nmedia_roots = [\"{}\"]\n", allowed.display()),
    )
    .unwrap();
    let mut s = Server::start(&p, &cfg, &[]);
    for tool in [
        "cinvar.media.import",
        "cinvar.media.inspect",
        "cinvar.qa.render_check",
    ] {
        let denied = s.call(
            tool,
            json!({"path": other.join("no.mp4").to_string_lossy()}),
        );
        assert_eq!(denied["result"]["isError"], true, "{tool}: {denied}");
        assert!(text(&denied).contains("media_roots"), "{tool}: {denied}");
    }
    let r = s.call(
        "cinvar.media.import_folder",
        json!({"path": other.to_string_lossy()}),
    );
    assert_eq!(r["result"]["isError"], true, "{r}");
    let ok = s.call(
        "cinvar.media.import",
        json!({"path": allowed.join("ok.mp4").to_string_lossy()}),
    );
    assert_ne!(ok["result"]["isError"], true, "{ok}");
    // a symlink inside the allowed root that points elsewhere does not launder the path
    std::os::unix::fs::symlink(other.join("no.mp4"), allowed.join("sneaky.mp4")).unwrap();
    let r = s.call(
        "cinvar.media.import",
        json!({"path": allowed.join("sneaky.mp4").to_string_lossy()}),
    );
    assert_eq!(r["result"]["isError"], true, "{r}");
}

#[test]
fn relink_cannot_point_an_asset_at_a_non_media_file_or_outside_media_roots() {
    // 1. Content validation: a text-ish file that ffprobe can mis-detect as "video" (ANSI art
    //    reader) must not become an asset that render later reads and rasterises.
    let (dir, _p, mut s) = setup();
    let clip = dir.path().join("real.mp4");
    make_mp4(&clip);
    let imported = s.call(
        "cinvar.media.import",
        json!({"path": clip.to_string_lossy()}),
    );
    assert_ne!(imported["result"]["isError"], true, "{imported}");
    let id: Value = serde_json::from_str(&text(&imported)).unwrap();
    let asset_id = id["asset_id"]
        .as_str()
        .unwrap_or_else(|| panic!("no asset id in {id}"))
        .to_string();
    std::fs::write(dir.path().join("key.asc"), "-----BEGIN SECRET-----\nabc\n").unwrap();
    let r = s.call(
        "cinvar.project.relink",
        json!({"asset_id": asset_id, "path": dir.path().join("key.asc").to_string_lossy()}),
    );
    assert_eq!(r["result"]["isError"], true, "relink to a text file: {r}");
    // 2. A genuine media file is still accepted when no media_roots are configured.
    let other = dir.path().join("other.mp4");
    make_mp4(&other);
    let r = s.call(
        "cinvar.project.relink",
        json!({"asset_id": asset_id, "path": other.to_string_lossy()}),
    );
    assert_ne!(r["result"]["isError"], true, "{r}");

    // 3. With media_roots configured, relink obeys them.
    let dir2 = tempfile::tempdir().unwrap();
    let cfg = dir2.path().join("cfg");
    let p = project(dir2.path(), &cfg);
    let allowed = dir2.path().join("allowed");
    std::fs::create_dir_all(&allowed).unwrap();
    std::fs::create_dir_all(&cfg).unwrap();
    make_mp4(&allowed.join("ok.mp4"));
    make_mp4(&dir2.path().join("outside.mp4"));
    std::fs::write(
        cfg.join("config.toml"),
        format!("[security]\nmedia_roots = [\"{}\"]\n", allowed.display()),
    )
    .unwrap();
    let mut s2 = Server::start(&p, &cfg, &[]);
    let imp = s2.call(
        "cinvar.media.import",
        json!({"path": allowed.join("ok.mp4").to_string_lossy()}),
    );
    let v: Value = serde_json::from_str(&text(&imp)).unwrap();
    let aid = v["asset_id"]
        .as_str()
        .unwrap_or_else(|| panic!("no asset id in {v}"))
        .to_string();
    let r = s2.call(
        "cinvar.project.relink",
        json!({"asset_id": aid, "path": dir2.path().join("outside.mp4").to_string_lossy()}),
    );
    assert_eq!(r["result"]["isError"], true, "{r}");
    assert!(text(&r).contains("media_roots"), "{r}");
}

#[test]
fn a_symlink_planted_in_cache_is_replaced_not_followed() {
    let (dir, p, mut s) = setup();
    let clip = dir.path().join("real.mp4");
    make_mp4(&clip);
    let imp = s.call(
        "cinvar.media.import",
        json!({"path": clip.to_string_lossy()}),
    );
    let v: Value = serde_json::from_str(&text(&imp)).unwrap();
    let aid = v["asset_id"].as_str().unwrap().to_string();
    let victim = dir.path().join("victim.txt");
    std::fs::write(&victim, "precious").unwrap();
    std::fs::create_dir_all(p.join("cache")).unwrap();
    std::os::unix::fs::symlink(&victim, p.join("cache").join(format!("sheet-{aid}.jpg"))).unwrap();
    let r = s.call("cinvar.media.contact_sheet", json!({"asset_id": aid}));
    assert_ne!(r["result"]["isError"], true, "{r}");
    assert_eq!(std::fs::read_to_string(&victim).unwrap(), "precious");
}

#[test]
fn agents_are_project_scoped_by_default_and_roots_are_human_chosen() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = dir.path().join("cfg");
    let p = project(dir.path(), &cfg);
    let outside = dir.path().join("footage");
    std::fs::create_dir_all(&outside).unwrap();
    make_mp4(&outside.join("a.mp4"));
    make_mp4(&p.join("inside.mp4"));
    let outside_file = outside.join("a.mp4").to_string_lossy().to_string();

    // Default: no media roots -> only the project is readable by agent tools.
    let mut s = Server::start(&p, &cfg, &[]);
    for (tool, args) in [
        ("cinvar.media.import", json!({"path": outside_file})),
        ("cinvar.media.inspect", json!({"path": outside_file})),
        (
            "cinvar.media.import_folder",
            json!({"path": outside.to_string_lossy()}),
        ),
        ("cinvar.qa.render_check", json!({"path": outside_file})),
        (
            "cinvar.project.relink",
            json!({"asset_id": "x", "path": outside_file}),
        ),
    ] {
        let r = s.call(tool, args);
        assert_eq!(r["result"]["isError"], true, "{tool}: {r}");
        assert!(text(&r).contains("outside the project"), "{tool}: {r}");
    }
    // Reading inside the project works, and so does /etc/passwd being refused.
    let ok = s.call(
        "cinvar.media.inspect",
        json!({"path": p.join("inside.mp4").to_string_lossy()}),
    );
    assert_ne!(ok["result"]["isError"], true, "{ok}");
    let r = s.call("cinvar.media.inspect", json!({"path": "/etc/passwd"}));
    assert_eq!(r["result"]["isError"], true, "{r}");
    drop(s);

    // The human widens it with CINVAR_MEDIA_ROOTS (or --media-root / config); then it works.
    let roots = outside.to_string_lossy().to_string();
    let mut s = Server::start(&p, &cfg, &[("CINVAR_MEDIA_ROOTS", roots.as_str())]);
    let ok = s.call("cinvar.media.import", json!({"path": outside_file}));
    assert_ne!(ok["result"]["isError"], true, "{ok}");
}

#[test]
fn render_partial_file_symlink_is_not_followed() {
    let (dir, p, mut s) = setup();
    let clip = dir.path().join("real.mp4");
    make_mp4(&clip);
    let imp = s.call(
        "cinvar.media.import",
        json!({"path": clip.to_string_lossy()}),
    );
    let aid = serde_json::from_str::<Value>(&text(&imp)).unwrap()["asset_id"]
        .as_str()
        .unwrap()
        .to_string();
    s.call("cinvar.timeline.create", json!({"name": "T"}));
    let ap = s.call("cinvar.timeline.append_clip", json!({"asset_id": aid}));
    assert_ne!(ap["result"]["isError"], true, "{ap}");
    let victim = dir.path().join("victim.txt");
    std::fs::write(&victim, "precious").unwrap();
    std::fs::create_dir_all(p.join("renders")).unwrap();
    std::os::unix::fs::symlink(&victim, p.join("renders").join(".out.partial.mp4")).unwrap();
    let r = s.call("cinvar.render.export", json!({"output": "renders/out.mp4"}));
    assert_ne!(r["result"]["isError"], true, "{r}");
    let job = serde_json::from_str::<Value>(&text(&r)).unwrap()["job_id"]
        .as_str()
        .unwrap()
        .to_string();
    for _ in 0..40 {
        let st = s.call(
            "cinvar.render.status",
            json!({"job_id": job, "wait_seconds": 2}),
        );
        let t = text(&st);
        if t.contains("succeeded") || t.contains("failed") {
            break;
        }
    }
    assert_eq!(std::fs::read_to_string(&victim).unwrap(), "precious");
}
