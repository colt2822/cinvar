//! MCP behaviour through the real stdio server process: client-safe names, logical aliases,
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

#[test]
fn read_only_session_hides_and_refuses_mutating_tools() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = dir.path().join("cfg");
    let p = project(dir.path(), &cfg);
    let mut s = Server::start(&p, &cfg, &[("CINVAR_MCP_PERMISSIONS", "read_only")]);
    let names = s.names();
    assert!(names.contains(&"cinvar.project.status".to_string()));
    assert!(
        !names.contains(&"cinvar.timeline.trim".to_string()),
        "edit tools must not be advertised read-only"
    );
    assert!(!names.contains(&"cinvar.render.cloud".to_string()));
    let denied = s.call("cinvar.timeline.create", json!({"name": "x"}));
    assert_eq!(denied["result"]["isError"], true);
    assert!(text(&denied).contains("permission_denied"), "{denied}");
    let ok = s.call("cinvar.project.status", json!({}));
    assert_ne!(ok["result"]["isError"], true, "{ok}");
}

#[test]
fn unknown_permission_value_fails_closed() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = dir.path().join("cfg");
    let p = project(dir.path(), &cfg);
    let mut s = Server::start(&p, &cfg, &[("CINVAR_MCP_PERMISSIONS", "yolo")]);
    assert!(!s.names().contains(&"cinvar.timeline.create".to_string()));
}

#[test]
fn edit_mode_excludes_spend_and_admin_tools() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = dir.path().join("cfg");
    let p = project(dir.path(), &cfg);
    let mut s = Server::start(&p, &cfg, &[("CINVAR_MCP_PERMISSIONS", "edit")]);
    let names = s.names();
    assert!(names.contains(&"cinvar.timeline.create".to_string()));
    assert!(!names.contains(&"cinvar.render.cloud".to_string()));
    assert!(!names.contains(&"cinvar.plugins.set_enabled".to_string()));
}

#[test]
fn safe_names_mode_advertises_only_strict_names_and_accepts_every_form() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = dir.path().join("cfg");
    let p = project(dir.path(), &cfg);
    let mut s = Server::start(&p, &cfg, &[("CINVAR_MCP_NAMES", "safe")]);
    let names = s.names();
    assert!(names.len() > 50);
    for n in &names {
        assert!(
            n.len() <= 64
                && n.chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-'),
            "unsafe name {n}"
        );
    }
    // safe form, native form and the logical alias all reach the same tool
    for form in ["cinvar_project_status", "cinvar.project.status"] {
        let r = s.call(form, json!({}));
        assert_ne!(r["result"]["isError"], true, "{form}: {r}");
    }
    let alias = s.call("cinvar.inspect_media", json!({}));
    assert!(
        !text(&alias).contains("unknown tool"),
        "alias must resolve: {alias}"
    );
    assert!(alias["error"].is_null());
}

#[test]
fn aliases_are_documented_by_a_tool_and_unknown_names_are_protocol_errors() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = dir.path().join("cfg");
    let p = project(dir.path(), &cfg);
    let mut s = Server::start(&p, &cfg, &[]);
    let a: Value =
        serde_json::from_str(&text(&s.call("cinvar.system.aliases", json!({})))).unwrap();
    assert!(a["aliases"]
        .as_array()
        .unwrap()
        .iter()
        .any(|x| x["alias"] == "cinvar.render_cloud" && x["tool"] == "cinvar.render.cloud"));
    assert!(a["not_a_single_tool"]
        .as_array()
        .unwrap()
        .iter()
        .any(|x| x["name"] == "cinvar.create_project"));
    let bad = s.call("cinvar.no_such_tool", json!({}));
    assert!(
        bad["error"]["code"].is_i64(),
        "unknown tool must be a JSON-RPC error: {bad}"
    );
}

#[test]
fn filesystem_boundary_and_untrusted_arguments_are_refused() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = dir.path().join("cfg");
    let p = project(dir.path(), &cfg);
    let mut s = Server::start(&p, &cfg, &[("CINVAR_MCP_PERMISSIONS", "full")]);
    // Render output outside the project is refused.
    let r = s.call(
        "cinvar.render.export",
        json!({"output": "/tmp/escape-cinvar-test.mp4"}),
    );
    assert_eq!(r["result"]["isError"], true);
    assert!(
        text(&r).contains("outside the project") || text(&r).contains("permission"),
        "{r}"
    );
    assert!(!Path::new("/tmp/escape-cinvar-test.mp4").exists());
    // Non-.otio import is refused; wrong-typed arguments produce errors, not panics.
    let r = s.call("cinvar.otio.import", json!({"input": "/etc/passwd"}));
    assert_eq!(r["result"]["isError"], true);
    let r = s.call(
        "cinvar.timeline.create",
        json!({"width": "not a number", "name": 12}),
    );
    // The server must survive hostile argument types and keep answering.
    assert!(r["error"].is_object() || r["result"].is_object(), "{r}");
    let alive = s.call("cinvar.project.status", json!({}));
    assert_ne!(alive["result"]["isError"], true, "{alive}");
    // Cloud rendering is off by default even in full mode.
    let r = s.call(
        "cinvar.render.cloud",
        json!({"max_price_per_hr": 1.0, "max_runtime_minutes": 10, "image": "x"}),
    );
    assert_eq!(r["result"]["isError"], true);
    assert!(text(&r).contains("disabled"), "{r}");
}

#[test]
fn default_session_cannot_spend_or_administer_plugins() {
    // No CINVAR_MCP_PERMISSIONS set: the default is `edit`, which excludes cloud spend and
    // plugin administration. Those need an explicit `full` from whoever starts the server.
    let dir = tempfile::tempdir().unwrap();
    let cfg = dir.path().join("cfg");
    let p = project(dir.path(), &cfg);
    let mut s = Server::start(&p, &cfg, &[]);
    for (tool, args) in [
        (
            "cinvar.render.cloud",
            json!({"max_price_per_hr": 1.0, "max_runtime_minutes": 10, "image": "x"}),
        ),
        (
            "cinvar.plugins.set_enabled",
            json!({"id": "anything", "enabled": true}),
        ),
    ] {
        let r = s.call(tool, args);
        assert_eq!(r["result"]["isError"], true, "{tool}: {r}");
        assert!(text(&r).contains("permission"), "{tool}: {r}");
    }
    let ok = s.call("cinvar.project.status", json!({}));
    assert_ne!(ok["result"]["isError"], true, "{ok}");
}
