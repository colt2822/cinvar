//! Director's Room: alpha reliability checks - atomic renders, concurrent edits, path boundaries,
//! malformed tool calls, and the MCP surface a first-time user's assistant sees.

use std::path::Path;
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

use cinvar::project::ops;
use cinvar::project::store::ProjectStore;
use cinvar::render::{self, RenderRequest};
use cinvar::util::time::parse_fps;

fn have_ffmpeg() -> bool {
    Command::new("ffmpeg")
        .arg("-version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

fn mk_project(root: &Path) -> Option<(ProjectStore, cinvar::project::model::Project)> {
    let src = root.join("src.mp4");
    if !Command::new("ffmpeg")
        .args([
            "-v",
            "error",
            "-y",
            "-f",
            "lavfi",
            "-i",
            "testsrc2=s=320x180:r=30:d=6",
            "-pix_fmt",
            "yuv420p",
            src.to_str().unwrap(),
        ])
        .status()
        .ok()?
        .success()
    {
        return None;
    }
    let projects = root.join("p");
    std::fs::create_dir_all(&projects).unwrap();
    let (store, mut project) = ProjectStore::create(&projects, "rel").unwrap();
    let asset = store.import_asset(&mut project, &src, "t").unwrap();
    ops::create_timeline(&mut project, "Main", 320, 180, parse_fps("30").unwrap()).unwrap();
    ops::append_clip(&mut project, None, None, &asset.id).unwrap();
    store.save(&mut project).unwrap();
    Some((store, project))
}

fn partials(dir: &Path) -> Vec<String> {
    std::fs::read_dir(dir)
        .map(|rd| {
            rd.flatten()
                .map(|e| e.file_name().to_string_lossy().to_string())
                .filter(|n| n.contains(".partial."))
                .collect()
        })
        .unwrap_or_default()
}

#[test]
fn finished_render_leaves_no_partial_file() {
    if !have_ffmpeg() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let Some((store, project)) = mk_project(dir.path()) else {
        return;
    };
    let out = dir.path().join("out").join("a.mp4");
    let mut req = RenderRequest::for_output(&out);
    req.width = Some(320);
    req.height = Some(180);
    render::render(&project, &store, &req, &|_, _| {}, &|| false).unwrap();
    assert!(out.is_file());
    assert!(
        partials(out.parent().unwrap()).is_empty(),
        "{:?}",
        partials(out.parent().unwrap())
    );
}

#[test]
fn cancelled_render_leaves_nothing_behind() {
    if !have_ffmpeg() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let Some((store, project)) = mk_project(dir.path()) else {
        return;
    };
    let out = dir.path().join("out").join("b.mp4");
    let mut req = RenderRequest::for_output(&out);
    req.width = Some(1280);
    req.height = Some(720);
    req.crf = Some(10);
    let polls = AtomicUsize::new(0);
    let cancel = || polls.fetch_add(1, Ordering::Relaxed) > 2;
    let err = render::render(&project, &store, &req, &|_, _| {}, &cancel);
    // Either it was cancelled in time, or it finished first; in both cases no half-written file may exist.
    if err.is_err() {
        assert!(
            !out.exists(),
            "a cancelled render must not leave the final file"
        );
    }
    assert!(partials(out.parent().unwrap()).is_empty());
}

#[test]
fn failed_render_leaves_no_file_at_the_final_name() {
    if !have_ffmpeg() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let Some((store, mut project)) = mk_project(dir.path()) else {
        return;
    };
    // Make the only asset unreadable garbage so ffmpeg fails mid-way.
    let asset = project.assets[0].clone();
    let path = store.asset_path(&asset).unwrap();
    std::fs::write(&path, b"not a video").unwrap();
    store.save(&mut project).unwrap();
    let out = dir.path().join("out").join("c.mp4");
    let req = RenderRequest::for_output(&out);
    let result = render::render(&project, &store, &req, &|_, _| {}, &|| false);
    assert!(
        !out.exists(),
        "no output at the final name after a failure: {result:?}"
    );
    assert!(partials(out.parent().unwrap()).is_empty());
}

#[test]
fn a_stale_save_is_refused_instead_of_overwriting_newer_work() {
    let dir = tempfile::tempdir().unwrap();
    let projects = dir.path().join("p");
    std::fs::create_dir_all(&projects).unwrap();
    let (store, mut a) = ProjectStore::create(&projects, "conc").unwrap();
    let mut b = store.load().unwrap();
    ops::create_timeline(&mut a, "A", 320, 180, parse_fps("30").unwrap()).unwrap();
    store.save(&mut a).unwrap();
    ops::create_timeline(&mut b, "B", 320, 180, parse_fps("30").unwrap()).unwrap();
    let err = store.save(&mut b).unwrap_err();
    assert!(err.message.contains("changed on disk"), "{err}");
    let on_disk = store.load().unwrap();
    assert!(
        on_disk.timelines.iter().any(|t| t.name == "A"),
        "A's edit survived"
    );
}

#[cfg(unix)]
#[test]
fn symlinks_cannot_smuggle_paths_out_of_the_project() {
    use cinvar::util::paths::ensure_within;
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("proj");
    let outside = dir.path().join("outside");
    std::fs::create_dir_all(root.join("renders")).unwrap();
    std::fs::create_dir_all(&outside).unwrap();
    std::os::unix::fs::symlink(&outside, root.join("renders").join("link")).unwrap();
    assert!(ensure_within(&root, Path::new("renders/ok.mp4")).is_ok());
    assert!(
        ensure_within(&root, Path::new("renders/link/evil.mp4")).is_err(),
        "symlink to outside is refused"
    );
    assert!(ensure_within(&root, Path::new("../outside/x.mp4")).is_err());
    assert!(ensure_within(&root, Path::new("/etc/passwd")).is_err());
}

#[test]
fn mcp_guides_the_model_and_logs_calls() {
    use serde_json::{json, Value};
    let dir = tempfile::tempdir().unwrap();
    let projects = dir.path().join("p");
    std::fs::create_dir_all(&projects).unwrap();
    let (store, _project) = ProjectStore::create(&projects, "mcp").unwrap();
    let bin = env!("CARGO_BIN_EXE_cinvar");
    let mut child = Command::new(bin)
        .args(["mcp", "serve", "--project", store.root.to_str().unwrap()])
        .env("CINVAR_CONFIG_DIR", dir.path().join("cfg"))
        .env("CINVAR_DATA_DIR", dir.path().join("data"))
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    use std::io::{BufRead, BufReader, Write};
    let mut stdin = child.stdin.take().unwrap();
    let mut reader = BufReader::new(child.stdout.take().unwrap());
    let mut ask = |req: Value| -> Value {
        writeln!(stdin, "{}", req).unwrap();
        let mut line = String::new();
        reader.read_line(&mut line).unwrap();
        serde_json::from_str(&line).unwrap()
    };
    let init = ask(json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}));
    let instructions = init["result"]["instructions"].as_str().unwrap_or("");
    assert!(
        instructions.contains("Workflow"),
        "instructions explain the workflow"
    );
    assert!(instructions.contains("rollback"));
    // malformed calls must answer with errors, never crash the server
    let bad = ask(
        json!({"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"cinvar.timeline.trim","arguments":{"clip_id": 5, "new_in": [1]}}}),
    );
    assert_eq!(bad["result"]["isError"], json!(true), "{bad}");
    let nothing = ask(
        json!({"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"cinvar.timeline.keyframe","arguments":{}}}),
    );
    assert_eq!(nothing["result"]["isError"], json!(true));
    let weird = ask(
        json!({"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"cinvar.timeline.keyframe","arguments":{"clip_id":"x","property":"blur","at":"abc","value":"high"}}}),
    );
    assert_eq!(weird["result"]["isError"], json!(true));
    let ok = ask(
        json!({"jsonrpc":"2.0","id":5,"method":"tools/call","params":{"name":"cinvar.project.status","arguments":{}}}),
    );
    assert_eq!(ok["result"]["isError"], json!(false), "{ok}");
    drop(stdin);
    let _ = child.wait();
    let log = std::fs::read_to_string(store.sessions_dir().join("mcp-calls.jsonl"))
        .expect("call log written");
    assert_eq!(log.lines().count(), 4);
    assert!(log.contains("\"tool\":\"cinvar.project.status\""));
    assert!(!log.contains("high"), "arguments are not logged");
}
