//! Project Inspector (`cinvar mcp inspector`): the fixed three-tool allowlist, annotations,
//! refusal of everything else (even with broad env permissions), and no writes into the project.

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
        cmd.args(["mcp", "inspector", "--project"])
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

    fn names(&mut self) -> Vec<String> {
        let v = self.rpc("tools/list", json!({}));
        v["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["name"].as_str().unwrap().to_string())
            .collect()
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

const ALLOWED: [&str; 3] = [
    "cinvar.project.status",
    "cinvar.qa.project_check",
    "cinvar.render.preflight",
];

#[test]
fn lists_exactly_the_three_tools_with_annotations_even_when_env_asks_for_full() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = dir.path().join("cfg");
    let p = project(dir.path(), &cfg);
    let mut s = Server::start(&p, &cfg, &[("CINVAR_MCP_PERMISSIONS", "full"), ("CINVAR_MCP_NAMES", "safe")]);
    let v = s.rpc("tools/list", json!({}));
    let tools = v["result"]["tools"].as_array().unwrap();
    let mut names: Vec<_> = tools.iter().map(|t| t["name"].as_str().unwrap().to_string()).collect();
    names.sort();
    let mut want: Vec<_> = ALLOWED.iter().map(|s| s.to_string()).collect();
    want.sort();
    assert_eq!(names, want);
    for t in tools {
        let a = &t["annotations"];
        assert!(a["title"].as_str().is_some_and(|x| !x.is_empty()), "{t}");
        assert_eq!(a["readOnlyHint"], true);
        assert_eq!(a["destructiveHint"], false);
        assert_eq!(a["openWorldHint"], false);
        // no tool takes a path: the project folder is fixed at launch
        assert!(t["inputSchema"]["properties"].get("path").is_none(), "{t}");
    }
}

#[test]
fn excluded_tools_and_their_aliases_are_refused() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = dir.path().join("cfg");
    let p = project(dir.path(), &cfg);
    let mut s = Server::start(&p, &cfg, &[("CINVAR_MCP_PERMISSIONS", "full")]);
    for name in [
        "cinvar.ai.generate_video",
        "cinvar.ai.auto_edit",
        "cinvar.ai.run_workflow",
        "cinvar.run_ai_workflow", // alias
        "cinvar_ai_run_workflow", // safe form
        "cinvar.media.inspect",
        "cinvar.inspect_media", // alias
        "cinvar.timeline.create",
        "cinvar.render.export",
        "cinvar.render.cloud",
        "cinvar.qa.render_check",
    ] {
        let r = s.call(name, json!({"path": "/etc/passwd"}));
        assert_eq!(r["result"]["isError"], true, "{name}: {r}");
        assert!(text(&r).contains("permission_denied"), "{name}: {r}");
    }
    let r = s.call("nope", json!({}));
    assert!(r["error"].is_object(), "{r}");
}

#[test]
fn the_three_tools_work_and_leave_the_project_folder_untouched() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = dir.path().join("cfg");
    let p = project(dir.path(), &cfg);
    let mut s = Server::start(&p, &cfg, &[]);
    for name in ["cinvar.project.status", "cinvar.qa.project_check"] {
        let r = s.call(name, json!({}));
        assert_ne!(r["result"]["isError"], true, "{name}: {r}");
    }
    // An empty project cannot export: preflight must say so with an actionable hint.
    let pf = s.call("cinvar.render.preflight", json!({"size_preset": "720p"}));
    assert_eq!(pf["result"]["isError"], true, "{pf}");
    assert!(text(&pf).contains("hint"), "{pf}");
    assert!(!p.join("sessions/mcp-calls.jsonl").exists(), "inspector must not log into the project");
    assert!(!p.join(".cinvar.lock").exists(), "inspector must not leave a lock file");
    assert!(!p.join("renders/preflight.mp4").exists());
}

#[test]
fn instructions_describe_the_inspector_not_the_editor_and_raw_project_json_is_not_exposed() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = dir.path().join("cfg");
    let p = project(dir.path(), &cfg);
    let mut s = Server::start(&p, &cfg, &[]);
    let init = s.rpc("initialize", json!({}));
    let ins = init["result"]["instructions"].as_str().unwrap();
    assert!(ins.contains("Project Inspector"));
    for banned in ["timeline.create", "render.export", "skills.load", "generate_video"] {
        assert!(!ins.contains(banned), "instructions mention {banned}");
    }
    let list = s.rpc("resources/list", json!({}));
    let uris: Vec<_> = list["result"]["resources"].as_array().unwrap().iter().map(|r| r["uri"].as_str().unwrap().to_string()).collect();
    assert_eq!(uris, vec!["cinvar://project/status".to_string()]);
    let read = s.rpc("resources/read", json!({"uri": "cinvar://project/json"}));
    assert!(read["error"].is_object(), "{read}");
}

#[test]
fn a_missing_project_gives_an_actionable_error() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = dir.path().join("cfg");
    let missing = dir.path().join("does-not-exist");
    let mut s = Server::start(&missing, &cfg, &[]);
    let r = s.call("cinvar.project.status", json!({}));
    assert_eq!(r["result"]["isError"], true, "{r}");
    assert!(text(&r).contains("no CINVAR project"), "{r}");
}

#[test]
fn sample_project_with_a_deleted_asset_is_reported_live_without_writing() {
    let Ok(py) = Command::new("python3").arg("--version").output() else { return };
    if !py.status.success() || Command::new("ffmpeg").arg("-version").output().is_err() {
        return; // sample builder needs python3 + ffmpeg; skip where absent
    }
    let dir = tempfile::tempdir().unwrap();
    let script = concat!(env!("CARGO_MANIFEST_DIR"), "/packaging/mcpb/sample/make_sample.py");
    let built = Command::new("python3").args([script, BIN]).arg(dir.path()).output().unwrap();
    assert!(built.status.success(), "{}", String::from_utf8_lossy(&built.stderr));
    let p = dir.path().join("inspector-sample");
    let before = std::fs::read(p.join("project.json")).unwrap();
    let cfg = dir.path().join("cfg");
    let mut s = Server::start(&p, &cfg, &[]);
    let st = text(&s.call("cinvar.project.status", json!({})));
    assert!(st.contains("\"missing\": true"), "{st}");
    assert!(st.contains("missing asset"), "{st}");
    let qa = text(&s.call("cinvar.qa.project_check", json!({})));
    assert!(qa.contains("missing asset: 0bcb6085-lost.mp4") || qa.contains("missing asset"), "{qa}");
    assert!(qa.contains("\"passed\": false"), "{qa}");
    assert_eq!(std::fs::read(p.join("project.json")).unwrap(), before, "project.json must not change");
}
