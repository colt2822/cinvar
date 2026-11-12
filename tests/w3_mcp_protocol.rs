use std::io::{BufRead, BufReader, Read, Write};
use std::path::Path;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::Value;

const BIN: &str = env!("CARGO_BIN_EXE_cinvar");

struct Session {
    child: Child,
    stdin: Option<ChildStdin>,
    lines: Receiver<String>,
    stderr: Arc<Mutex<String>>,
}

impl Session {
    fn start(project: &Path, config: &Path) -> Session {
        let mut child = Command::new(BIN)
            .arg("mcp")
            .arg("serve")
            .arg("--project")
            .arg(project)
            .env("CINVAR_CONFIG_DIR", config)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("could not start cinvar mcp serve");
        let stdin = child.stdin.take().expect("stdin pipe");
        let stdout = child.stdout.take().expect("stdout pipe");
        let stderr = child.stderr.take().expect("stderr pipe");

        let (sender, lines) = mpsc::channel::<String>();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                match line {
                    Ok(line) => {
                        if sender.send(line).is_err() {
                            break;
                        }
                    }
                    Err(_) => break,
                }
            }
        });

        let capture = Arc::new(Mutex::new(String::new()));
        let sink = capture.clone();
        std::thread::spawn(move || {
            let mut buf = String::new();
            let _ = BufReader::new(stderr).read_to_string(&mut buf);
            if let Ok(mut guard) = sink.lock() {
                guard.push_str(&buf);
            }
        });

        Session {
            child,
            stdin: Some(stdin),
            lines,
            stderr: capture,
        }
    }

    fn send(&mut self, body: &str) {
        let stdin = self.stdin.as_mut().expect("server stdin already closed");
        writeln!(stdin, "{body}").expect("write request");
        stdin.flush().expect("flush request");
    }

    fn next(&mut self, what: &str) -> Value {
        match self.lines.recv_timeout(Duration::from_secs(20)) {
            Ok(line) => serde_json::from_str(&line).unwrap_or_else(|err| {
                panic!("{what}: the server wrote a non-JSON line {line:?}: {err}")
            }),
            Err(err) => panic!(
                "{what}: no reply from the MCP server ({err}); stderr: {}",
                self.stderr_text()
            ),
        }
    }

    fn stderr_text(&self) -> String {
        self.stderr
            .lock()
            .map(|guard| guard.clone())
            .unwrap_or_default()
    }

    fn finish(&mut self) -> Option<i32> {
        drop(self.stdin.take());
        let deadline = Instant::now() + Duration::from_secs(20);
        while Instant::now() < deadline {
            match self.child.try_wait() {
                Ok(Some(status)) => return status.code(),
                Ok(None) => std::thread::sleep(Duration::from_millis(25)),
                Err(err) => panic!("waiting for the MCP server failed: {err}"),
            }
        }
        let _ = self.child.kill();
        panic!(
            "the MCP server did not exit after stdin was closed; stderr: {}",
            self.stderr_text()
        );
    }
}

fn create_project(dir: &Path, config: &Path) -> std::path::PathBuf {
    let output = Command::new(BIN)
        .arg("project")
        .arg("create")
        .arg("--name")
        .arg("mcp-test")
        .arg("--dir")
        .arg(dir)
        .env("CINVAR_CONFIG_DIR", config)
        .output()
        .expect("could not run cinvar project create");
    assert!(
        output.status.success(),
        "project create failed: {}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let project = dir.join("mcp-test");
    assert!(
        project.join("project.json").is_file(),
        "no project.json at {}",
        project.display()
    );
    project
}

#[test]
fn mcp_stdio_session_serves_a_real_project() {
    let dir = tempfile::tempdir().unwrap();
    let config = dir.path().join("config");
    let project = create_project(dir.path(), &config);
    let mut session = Session::start(&project, &config);

    // --- initialize -------------------------------------------------------
    let init_request = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"w3-mcp-test","version":"1.0"}}}"#;
    session.send(init_request);
    let init_response = session.next("initialize");
    println!("EVIDENCE initialize request:  {init_request}");
    println!("EVIDENCE initialize response: {init_response}");
    assert_eq!(init_response["jsonrpc"], "2.0");
    assert_eq!(init_response["id"], 1);
    assert_eq!(init_response["result"]["protocolVersion"], "2024-11-05");
    assert_eq!(init_response["result"]["serverInfo"]["name"], "cinvar");
    assert_eq!(
        init_response["result"]["serverInfo"]["version"],
        cinvar::VERSION
    );
    assert_eq!(
        init_response["result"]["capabilities"]["tools"]["listChanged"],
        false
    );
    assert_eq!(
        init_response["result"]["capabilities"]["resources"]["subscribe"],
        false
    );

    // A notification must not produce a reply.
    session.send(r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#);

    // --- ping -------------------------------------------------------------
    session.send(r#"{"jsonrpc":"2.0","id":2,"method":"ping"}"#);
    let pong = session.next("ping");
    assert_eq!(pong["id"], 2);
    assert_eq!(pong["result"], serde_json::json!({}));

    // --- tools/list -------------------------------------------------------
    session.send(r#"{"jsonrpc":"2.0","id":3,"method":"tools/list"}"#);
    let listed = session.next("tools/list");
    let tools = listed["result"]["tools"]
        .as_array()
        .expect("tools must be an array");
    assert!(
        tools.len() > 20,
        "expected more than 20 tools, got {}",
        tools.len()
    );
    let append = tools
        .iter()
        .find(|tool| tool["name"] == "cinvar.timeline.append_clip")
        .expect("cinvar.timeline.append_clip must be listed");
    let required = append["inputSchema"]["required"]
        .as_array()
        .expect("append_clip must declare required inputs");
    assert!(
        required.iter().any(|value| value == "asset_id"),
        "append_clip must require asset_id: {append}"
    );
    assert!(
        append["description"].as_str().unwrap_or("").len() > 10,
        "append_clip needs a description"
    );

    // --- tools/call -------------------------------------------------------
    session.send(r#"{"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"cinvar.project.status","arguments":{}}}"#);
    let called = session.next("tools/call cinvar.project.status");
    println!("EVIDENCE tools/call response: {called}");
    assert_eq!(
        called["result"]["isError"], false,
        "status call failed: {called}"
    );
    let text = called["result"]["content"][0]["text"]
        .as_str()
        .expect("text content block");
    assert!(
        text.contains("mcp-test"),
        "project name missing from the status payload: {text}"
    );
    let status: Value = serde_json::from_str(text).expect("status payload must be JSON");
    assert!(status["project"]["assets"].is_array());
    assert!(status["project"]["timelines"].is_array());
    assert!(status["project"]["issues"].is_array());

    // --- malformed JSON must not kill the server --------------------------
    session.send("{ this is not json");
    let malformed = session.next("malformed request");
    assert_eq!(
        malformed["error"]["code"], -32700,
        "malformed JSON must be -32700: {malformed}"
    );
    session.send(r#"{"jsonrpc":"2.0","id":5,"method":"ping"}"#);
    let still_alive = session.next("ping after malformed input");
    assert_eq!(
        still_alive["id"], 5,
        "the server stopped answering after malformed input"
    );

    // An empty line is ignored, not answered.
    session.send("");

    // --- unknown method ---------------------------------------------------
    session.send(r#"{"jsonrpc":"2.0","id":6,"method":"does/not/exist","params":{}}"#);
    let unknown_method = session.next("unknown method");
    assert_eq!(
        unknown_method["error"]["code"], -32601,
        "unknown method must be -32601: {unknown_method}"
    );

    // --- unknown tool -----------------------------------------------------
    session.send(r#"{"jsonrpc":"2.0","id":7,"method":"tools/call","params":{"name":"cinvar.nope.nope","arguments":{}}}"#);
    let unknown_tool = session.next("unknown tool");
    let code = unknown_tool["error"]["code"].as_i64();
    let is_error = unknown_tool["result"]["isError"].as_bool().unwrap_or(false);
    assert!(
        code == Some(-32602) || is_error,
        "an unknown tool must be rejected: {unknown_tool}"
    );

    // --- resources --------------------------------------------------------
    session.send(r#"{"jsonrpc":"2.0","id":8,"method":"resources/list"}"#);
    let resources = session.next("resources/list");
    let uris: Vec<String> = resources["result"]["resources"]
        .as_array()
        .expect("resources must be an array")
        .iter()
        .filter_map(|resource| resource["uri"].as_str().map(|uri| uri.to_string()))
        .collect();
    assert!(
        uris.iter().any(|uri| uri == "cinvar://project/status"),
        "missing status resource: {uris:?}"
    );
    assert!(
        uris.iter().any(|uri| uri == "cinvar://project/json"),
        "missing project.json resource: {uris:?}"
    );

    session.send(r#"{"jsonrpc":"2.0","id":9,"method":"resources/read","params":{"uri":"cinvar://project/json"}}"#);
    let raw = session.next("resources/read");
    let raw_text = raw["result"]["contents"][0]["text"]
        .as_str()
        .expect("resource text");
    assert!(
        raw_text.contains("\"mcp-test\""),
        "raw project.json does not carry the project name"
    );

    // --- EOF exits cleanly ------------------------------------------------
    let exit = session.finish();
    let diagnostics = session.stderr_text();
    assert_eq!(
        exit,
        Some(0),
        "the MCP server should exit 0 on EOF; stderr: {diagnostics}"
    );
}

#[cfg(unix)]
#[test]
fn plugin_tools_are_discovered_registered_and_executed() {
    use std::os::unix::fs::PermissionsExt;

    let dir = tempfile::tempdir().unwrap();
    let plugins = dir.path().join("plugins");
    let root = plugins.join("demo");
    std::fs::create_dir_all(&root).unwrap();
    let script = root.join("run.sh");
    std::fs::write(&script, "#!/bin/sh\ncat\n").unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    let manifest = serde_json::json!({
        "name": "demo",
        "version": "1.0.0",
        "description": "Integration-test plugin that echoes its stdin",
        "cinvar_min": "0.1.0",
        "permissions": {"spawn_subprocess": true},
        "tools": [{
            "name": "echo",
            "description": "Echo the arguments JSON back on stdout",
            "input": {"type": "object", "properties": {"text": {"type": "string"}}, "required": ["text"]},
            "command": ["./run.sh"],
            "timeout_secs": 15
        }]
    });
    std::fs::write(
        root.join("plugin.json"),
        serde_json::to_string_pretty(&manifest).unwrap(),
    )
    .unwrap();

    let manifests = cinvar::plugins::discover(&plugins).expect("discover must succeed");
    assert_eq!(manifests.len(), 1, "expected exactly one plugin");
    assert_eq!(manifests[0].name, "demo");
    assert_eq!(manifests[0].tools.len(), 1);
    assert!(cinvar::plugins::discover(&dir.path().join("missing"))
        .unwrap()
        .is_empty());

    // Refused unless subprocess execution is enabled.
    let mut refused = cinvar::tools::Registry::new();
    let err = cinvar::plugins::register(&mut refused, &manifests, false)
        .expect_err("register must refuse");
    assert_eq!(
        err.code,
        cinvar::error::ErrorCode::PermissionDenied,
        "wrong error code: {err}"
    );
    assert!(
        err.message.contains("subprocess"),
        "the refusal must mention subprocesses: {err}"
    );
    assert_eq!(refused.len(), 0, "a refused plugin must not register tools");

    // Enabled: the tool is registered and runs.
    let mut registry = cinvar::tools::Registry::new();
    let count =
        cinvar::plugins::register(&mut registry, &manifests, true).expect("register must succeed");
    assert_eq!(count, 1);
    assert!(registry.contains("plugin.demo.echo"));
    let spec = registry.get("plugin.demo.echo").expect("plugin tool spec");
    assert_eq!(spec.input["required"][0], "text");

    let (store, mut project) =
        cinvar::project::store::ProjectStore::create(dir.path(), "plugin host").unwrap();
    let mut config = cinvar::config::Config::default();
    // Plugin commands are gated at call time by the host switch, not only at registration.
    config.security.allow_plugin_subprocess = true;
    config.security.allow_unsandboxed_plugins = true;
    let jobs = cinvar::jobs::Jobs::new();
    let mut history = cinvar::project::history::History::default();
    let mut ctx = cinvar::tools::ToolCtx {
        project: &mut project,
        store: &store,
        config: &config,
        jobs: &jobs,
        history: &mut history,
        dry_run: false,
    };
    let output = registry
        .call(
            "plugin.demo.echo",
            &mut ctx,
            serde_json::json!({"text": "hello plugin"}),
        )
        .expect("the plugin tool must run");
    println!("EVIDENCE plugin call result: {output}");
    assert_eq!(output["exit_code"], 0, "plugin exit code: {output}");
    let stdout = output["stdout"].as_str().expect("stdout string");
    let echoed: Value =
        serde_json::from_str(stdout.trim()).expect("stdout must be the arguments JSON");
    assert_eq!(
        echoed["text"], "hello plugin",
        "stdout did not round-trip the arguments: {stdout:?}"
    );
    assert!(output["stderr"].is_string());
}
