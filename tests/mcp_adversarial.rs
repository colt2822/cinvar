#![cfg(unix)]
//! Adversarial verification of the agent-facing boundaries that docs/SECURITY_REVIEW_PACKAGE.md
//! claims are closed. Every test here attacks a guard through the real stdio MCP server process
//! (or the real local web UI / the real ComfyUI client): aliases and case variants against the
//! permission mode, spend approval records, request-origin defences, secret redaction, and a
//! hostile server response that carries a path.
//!
//! Each test fails if its guard is removed; two of them were proven by breaking the guard
//! (see docs/SECURITY_REVIEW_PACKAGE.md, "Adversarial verification").

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, Instant};

use serde_json::{json, Value};

const BIN: &str = env!("CARGO_BIN_EXE_cinvar");

// ---------------------------------------------------------------- stdio harness

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
            .recv_timeout(Duration::from_secs(30))
            .expect("server reply");
        let v: Value = serde_json::from_str(&line).unwrap();
        assert_eq!(v["id"], self.id);
        v
    }

    fn call(&mut self, name: &str, args: Value) -> Value {
        self.rpc("tools/call", json!({"name": name, "arguments": args}))
    }

    fn listed_tools(&mut self) -> Vec<String> {
        self.rpc("tools/list", json!({}))["result"]["tools"]
            .as_array()
            .expect("tools array")
            .iter()
            .filter_map(|t| t["name"].as_str().map(String::from))
            .collect()
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn text(reply: &Value) -> String {
    reply["result"]["content"][0]["text"]
        .as_str()
        .unwrap_or("")
        .to_string()
}

/// The tool's own JSON answer (the MCP content envelope carries it as text).
fn payload(reply: &Value) -> Value {
    serde_json::from_str(&text(reply)).unwrap_or(Value::Null)
}

fn denied(reply: &Value) -> bool {
    let t = text(reply);
    t.contains("permission_denied")
        || t.contains("needs Edit permission")
        || t.contains("needs Spend")
}

/// A name the server does not know: refused before any permission decision, never executed.
fn unknown_tool(reply: &Value) -> bool {
    reply
        .get("error")
        .and_then(|e| e["message"].as_str())
        .map(|m| m.contains("unknown tool"))
        .unwrap_or(false)
}

// ---------------------------------------------------------------- fixtures

struct Fixture {
    _dir: tempfile::TempDir,
    cfg: PathBuf,
    project: PathBuf,
}

fn fixture(name: &str) -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let cfg = dir.path().join("cfg");
    std::fs::create_dir_all(&cfg).unwrap();
    let out = Command::new(BIN)
        .args(["project", "create", "--name", name, "--dir"])
        .arg(dir.path())
        .env("CINVAR_CONFIG_DIR", &cfg)
        .output()
        .unwrap();
    assert!(out.status.success(), "project create failed");
    let project = dir.path().join(name);
    Fixture {
        _dir: dir,
        cfg,
        project,
    }
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

// ---------------------------------------------------------------- 1. permission mode

/// No write/spend/admin tool may run in a read-only session, under any accepted spelling of its
/// name: native, dotted-safe, alias, alias-safe, upper case, client-prefixed or space-padded.
#[test]
fn read_only_denies_every_non_read_tool_under_every_name_form() {
    use cinvar::mcp::compat::{level_of, safe_name, Level, Mode, ALIASES};

    let f = fixture("perm");
    // Everything a full session can reach is the attack surface; classify it with the same
    // function the server uses.
    let mut full = Server::start(&f.project, &f.cfg, &[("CINVAR_MCP_PERMISSIONS", "full")]);
    let listed = full.listed_tools();
    let mut edit = Server::start(&f.project, &f.cfg, &[("CINVAR_MCP_PERMISSIONS", "edit")]);
    assert!(
        listed.len() > 20,
        "unexpectedly small tool surface: {listed:?}"
    );

    let guarded: Vec<(String, Level)> = listed
        .iter()
        .map(|n| (n.clone(), level_of(n)))
        .filter(|(_, l)| *l != Level::Read)
        .collect();
    assert!(
        guarded.iter().any(|(_, l)| *l == Level::Edit)
            && guarded.iter().any(|(_, l)| *l == Level::Spend),
        "expected edit and spend tools in {guarded:?}"
    );

    let mut ro = Server::start(
        &f.project,
        &f.cfg,
        &[("CINVAR_MCP_PERMISSIONS", "read_only")],
    );
    let mut attacks = 0usize;
    for (native, _) in &guarded {
        let mut forms = vec![
            native.clone(),
            safe_name(native),
            native.to_uppercase(),
            format!("{native} "),
            format!("mcp__cinvar__{}", safe_name(native)),
        ];
        for (alias, target) in ALIASES {
            if target == native {
                forms.push((*alias).to_string());
                forms.push(safe_name(alias));
            }
        }
        for name in forms {
            let r = ro.call(&name, json!({"confirm_quote": "q_x"}));
            attacks += 1;
            assert!(
                denied(&r) || unknown_tool(&r),
                "read-only session ran or mis-answered {name:?} (native {native}): {r}"
            );
        }
    }
    assert!(attacks >= 100, "attack matrix too small: {attacks}");

    // The same matrix must be honest about what a read-only session can still do.
    let ok = ro.call("cinvar.project.status", json!({}));
    assert_ne!(ok["result"]["isError"], true, "{}", text(&ok));
    assert!(
        !ro.listed_tools()
            .contains(&"cinvar.render.export".to_string()),
        "tools/list leaked a write tool to a read-only session"
    );

    // "edit" is not an escalation path either: spending and plugin administration stay closed.
    for (name, level) in &guarded {
        if *level == Level::Edit {
            continue;
        }
        let r = edit.call(
            name,
            json!({"max_price_per_hr": 0.01, "max_runtime_minutes": 1, "image": "i"}),
        );
        assert!(
            denied(&r),
            "edit session reached {level:?} tool {name}: {}",
            text(&r)
        );
    }
    assert!(Mode::Edit.allows(Level::Edit));
}

/// A malformed or empty CINVAR_MCP_PERMISSIONS must never widen the session.
#[test]
fn permission_env_fails_closed_and_empty_means_edit() {
    let f = fixture("env");
    let cases: &[(&str, &str)] = &[
        ("", "edit"),
        ("garbage", "read_only"),
        ("FULL_MODE", "read_only"),
        ("full ", "full"),
        ("Read_Only", "read_only"),
        ("read-only", "read_only"),
    ];
    for (value, expected) in cases {
        let mut s = Server::start(&f.project, &f.cfg, &[("CINVAR_MCP_PERMISSIONS", value)]);
        let write = s.call("cinvar.timeline.create", json!({"name": "T"}));
        let spend = s.call(
            "cinvar.render.cloud",
            json!({"max_price_per_hr": 0.01, "max_runtime_minutes": 1, "image": "i"}),
        );
        let admin = s.call(
            "cinvar.plugins.set_enabled",
            json!({"id": "x", "enabled": true}),
        );
        match *expected {
            "edit" => {
                assert_ne!(
                    write["result"]["isError"],
                    true,
                    "empty mode must mean edit: {}",
                    text(&write)
                );
                assert!(denied(&spend), "{}", text(&spend));
                assert!(denied(&admin), "{}", text(&admin));
            }
            "full" => {
                assert_ne!(write["result"]["isError"], true, "{}", text(&write));
                // Reached the tool (it then fails on the unknown plugin), so the mode gate opened.
                assert!(
                    !denied(&admin),
                    "full mode must reach the tool: {}",
                    text(&admin)
                );
            }
            _ => {
                assert!(
                    denied(&write),
                    "unknown value {value:?} must be read-only: {}",
                    text(&write)
                );
                assert!(denied(&spend), "{}", text(&spend));
            }
        }
    }
}

// ---------------------------------------------------------------- 2. cloud approval

/// The approval record is the human's alone: the agent cannot mint, move or replay it through any
/// other tool, cannot spend without it, and is told no synchronously.
#[test]
fn an_agent_cannot_self_approve_spend_through_any_other_tool() {
    let f = fixture("spend");
    std::fs::write(
        f.cfg.join("config.toml"),
        "[security]\nallow_cloud_render = true\n",
    )
    .unwrap();
    let clip = f.project.join("clip.mp4");
    make_mp4(&clip);
    let mut s = Server::start(
        &f.project,
        &f.cfg,
        &[
            ("CINVAR_MCP_PERMISSIONS", "full"),
            ("CINVAR_RUNPOD_API_KEY", "rpa_FAKEKEYFORTESTS_0123456789"),
            ("CINVAR_RUNPOD_BASE", "http://127.0.0.1:9"),
        ],
    );
    let imp = s.call(
        "cinvar.media.import",
        json!({"path": clip.to_string_lossy()}),
    );
    let aid = payload(&imp)["asset_id"]
        .as_str()
        .expect("asset id")
        .to_string();
    s.call("cinvar.timeline.create", json!({"name": "T"}));
    let ap = s.call("cinvar.timeline.append_clip", json!({"asset_id": aid}));
    assert_ne!(ap["result"]["isError"], true, "{}", text(&ap));
    // A tool that takes a free-form name cannot move or aim anything at the approval directory.
    s.call(
        "cinvar.project.rename",
        json!({"name": "../../cloud/approved/x"}),
    );

    let base = json!({"max_price_per_hr": 1.0, "max_runtime_minutes": 10,
                      "image": "attacker/image:latest"});
    let q = s.call("cinvar.render.cloud", base.clone());
    let quote = payload(&q);
    assert_eq!(
        quote["billed"],
        json!(false),
        "step 1 spends nothing: {quote}"
    );
    assert_eq!(quote["needs_human_approval"], json!(true));
    let id = quote["quote"]["id"].as_str().expect("quote id").to_string();
    let approved = f.cfg.join("cloud").join("approved").join(&id);
    assert!(!approved.exists(), "asking for a quote must not approve it");

    // Every tool that takes a path or an id is aimed at the approval directory.
    let attempts = [
        (
            "cinvar.otio.export",
            json!({"output": approved.to_string_lossy()}),
        ),
        (
            "cinvar.otio.export",
            json!({"output": format!("../../cfg/cloud/approved/{id}")}),
        ),
        (
            "cinvar.captions.export",
            json!({"output": format!("../../cfg/cloud/approved/{id}")}),
        ),
        (
            "cinvar.render.export",
            json!({"output": format!("../../cfg/cloud/approved/{id}")}),
        ),
        (
            "cinvar.media.import",
            json!({"path": f.cfg.join("cloud").join("pending")
            .join(format!("{id}.json")).to_string_lossy()}),
        ),
        (
            "cinvar.plugins.set_enabled",
            json!({"id": format!("../../cloud/approved/{id}"),
                                             "enabled": true}),
        ),
        (
            "cinvar.plugins.pin",
            json!({"id": format!("../../cloud/approved/{id}"),
                                     "version": "1.0.0"}),
        ),
        (
            "cinvar.jobs.retry",
            json!({"job_id": format!("../approved/{id}")}),
        ),
    ];
    for (tool, args) in attempts {
        s.call(tool, args);
        assert!(
            !approved.exists(),
            "{tool} minted an approval record for {id}"
        );
    }

    // Echoing the id and acknowledging the risk is not an authorization.
    let mut confirm = base.clone();
    confirm["confirm_quote"] = json!(id);
    confirm["acknowledge_billing_risk"] = json!(true);
    let refused = s.call("cinvar.render.cloud", confirm.clone());
    assert_eq!(refused["result"]["isError"], true, "{}", text(&refused));
    let rp = payload(&refused);
    assert!(
        text(&refused).contains("approval from a human"),
        "{}",
        text(&refused)
    );
    assert_ne!(
        rp["billed"].as_bool(),
        Some(true),
        "refusal must not claim spend: {rp}"
    );
    assert!(
        rp.get("job_id").is_none(),
        "refusal must not create a job: {rp}"
    );
    assert!(!approved.exists());

    // What the human's CLI writes works exactly once.
    std::fs::create_dir_all(approved.parent().unwrap()).unwrap();
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    std::fs::write(&approved, now.to_string()).unwrap();
    let first = s.call("cinvar.render.cloud", confirm.clone());
    assert_ne!(
        first["result"]["isError"],
        true,
        "approved quote must start: {}",
        text(&first)
    );
    let fp = payload(&first);
    assert_ne!(
        fp["billed"].as_bool(),
        Some(true),
        "must not report a boolean billed=true: {fp}"
    );
    assert!(fp["job_id"].is_string(), "{fp}");
    let replay = s.call("cinvar.render.cloud", confirm);
    assert_eq!(
        replay["result"]["isError"],
        true,
        "an approval is single-use: {}",
        text(&replay)
    );

    // An approval older than the TTL is refused, and refused for good (consumed, not replayed).
    let q2 = s.call("cinvar.render.cloud", base.clone());
    let id2 = payload(&q2)["quote"]["id"].as_str().unwrap().to_string();
    let stale = f.cfg.join("cloud").join("approved").join(&id2);
    std::fs::write(&stale, (now - 3600).to_string()).unwrap();
    let mut confirm2 = base;
    confirm2["confirm_quote"] = json!(id2);
    confirm2["acknowledge_billing_risk"] = json!(true);
    let expired = s.call("cinvar.render.cloud", confirm2.clone());
    assert_eq!(expired["result"]["isError"], true, "{}", text(&expired));
    assert!(text(&expired).contains("expired"), "{}", text(&expired));
    assert!(!stale.exists(), "an expired approval must not stay usable");
    let again = s.call("cinvar.render.cloud", confirm2);
    assert_eq!(again["result"]["isError"], true, "{}", text(&again));
}

// ---------------------------------------------------------------- 3. secrets

/// A configured key must not come back through an error surface, whatever env name it is held in.
#[test]
fn configured_secrets_never_appear_in_mcp_error_surfaces() {
    let f = fixture("secret");
    let custom = "CINVAR_TEST_CUSTOM_PROVIDER_KEY";
    let custom_value = "pk_live_0123456789abcdefghijklmnop";
    let runpod_value = "rpa_FAKEKEYFORTESTS_0123456789";
    std::fs::write(
        f.cfg.join("config.toml"),
        format!(
            "[models.custom]\nprovider = \"openai\"\nmodel = \"gpt-4o\"\napi_key_env = \"{custom}\"\n"
        ),
    )
    .unwrap();
    let mut s = Server::start(
        &f.project,
        &f.cfg,
        &[
            ("CINVAR_MCP_PERMISSIONS", "full"),
            (custom, custom_value),
            ("CINVAR_RUNPOD_API_KEY", runpod_value),
        ],
    );
    // A tool error is echoed verbatim, so drive a secret through one.
    for (secret, label) in [
        (custom_value, "profile api_key_env"),
        (runpod_value, "runpod key"),
    ] {
        let r = s.call(
            "cinvar.otio.export",
            json!({"output": format!("/tmp/{secret}/leak.otio")}),
        );
        let t = text(&r);
        assert!(
            !t.contains(secret),
            "{label} leaked through an MCP error: {t}"
        );
        assert!(
            t.contains("[redacted]"),
            "{label}: not echoed and not redacted: {t}"
        );
    }
    // The same text reaches the project.json resource through redaction.
    let res = s.rpc("resources/read", json!({"uri": "cinvar://project/json"}));
    let body = res["result"]["contents"][0]["text"].as_str().unwrap_or("");
    for secret in [custom_value, runpod_value] {
        assert!(!body.contains(secret), "secret in project.json resource");
    }
}

// ---------------------------------------------------------------- 4. local web UI

struct Ui {
    child: Child,
    port: u16,
}

impl Drop for Ui {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn start_ui(project: &Path, config: &Path) -> Ui {
    let probe = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = probe.local_addr().unwrap().port();
    drop(probe);
    let child = Command::new(BIN)
        .args(["ui", "--port", &port.to_string()])
        .current_dir(project)
        .env("CINVAR_CONFIG_DIR", config)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let started = Instant::now();
    while TcpStream::connect(("127.0.0.1", port)).is_err() {
        assert!(
            started.elapsed() < Duration::from_secs(15),
            "ui did not start"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
    Ui { child, port }
}

/// One raw HTTP/1.1 exchange. Write errors are ignored: a guard that refuses early closes the
/// connection while the body is still being sent.
fn http(
    port: u16,
    method: &str,
    path: &str,
    headers: &[(&str, &str)],
    body: &[u8],
) -> (u16, String) {
    let mut s = TcpStream::connect(("127.0.0.1", port)).unwrap();
    s.set_read_timeout(Some(Duration::from_secs(20))).unwrap();
    let mut req = format!("{method} {path} HTTP/1.1\r\nConnection: close\r\n");
    for (k, v) in headers {
        req.push_str(&format!("{k}: {v}\r\n"));
    }
    req.push_str(&format!("Content-Length: {}\r\n\r\n", body.len()));
    let _ = s.write_all(req.as_bytes());
    let _ = s.write_all(body);
    let _ = s.flush();
    let mut out = Vec::new();
    let _ = s.read_to_end(&mut out);
    let text = String::from_utf8_lossy(&out).to_string();
    let status = text
        .split_whitespace()
        .nth(1)
        .and_then(|c| c.parse().ok())
        .unwrap_or(0);
    let body = text
        .split_once("\r\n\r\n")
        .map(|(_, b)| b.to_string())
        .unwrap_or_default();
    (status, body)
}

/// The local UI is unauthenticated: it must refuse browser-driven abuse and bound what it will
/// read, while a same-origin client still works.
#[test]
fn ui_guard_refuses_rebinding_csrf_traversal_and_oversized_bodies() {
    let f = fixture("uiguard");
    let ui = start_ui(&f.project, &f.cfg);
    let p = ui.port;
    let host = format!("127.0.0.1:{p}");

    assert_eq!(
        http(p, "GET", "/", &[("Host", &host)], b"").0,
        200,
        "same-origin GET"
    );

    // DNS rebinding: the browser's Host is the attacker's name, not ours.
    assert_eq!(
        http(
            p,
            "GET",
            "/api/status",
            &[("Host", &format!("evil.example:{p}"))],
            b""
        )
        .0,
        403
    );
    assert_eq!(
        http(p, "GET", "/api/status", &[("Host", "127.0.0.1")], b"").0,
        403
    );

    // A foreign page may not drive the API, with or without a CORS preflight.
    assert_eq!(
        http(
            p,
            "GET",
            "/",
            &[("Host", &host), ("Origin", "https://evil.example")],
            b""
        )
        .0,
        403
    );
    for ctype in [
        "text/plain",
        "application/x-www-form-urlencoded",
        "multipart/form-data",
    ] {
        assert_eq!(
            http(
                p,
                "POST",
                "/api/chat",
                &[
                    ("Host", &host),
                    ("Origin", &format!("http://{host}")),
                    ("Content-Type", ctype)
                ],
                br#"{"message":"x"}"#
            )
            .0,
            403,
            "a simple cross-site POST with {ctype} must not reach the API"
        );
    }

    // Path traversal in the media route: refused, and nothing of the project leaks.
    let project_json = std::fs::read_to_string(f.project.join("project.json")).unwrap();
    let marker = project_json
        .lines()
        .find(|l| l.contains("revision"))
        .map(|l| l.trim().to_string())
        .unwrap_or_else(|| "\"revision\"".to_string());
    for path in [
        "/api/media/../../project.json",
        "/api/media/%2e%2e%2f%2e%2e%2fproject.json",
        "/api/media/..%2f..%2fproject.json",
        "/api/media/%2Fetc%2Fpasswd",
    ] {
        let (status, body) = http(p, "GET", path, &[("Host", &host)], b"");
        assert!(
            (400..500).contains(&status),
            "traversal {path} answered {status}, expected an explicit refusal"
        );
        assert!(
            body.contains("error"),
            "traversal {path} answered without a JSON error: {body:?}"
        );
        assert!(
            !body.contains(&marker),
            "traversal {path} disclosed project.json"
        );
        assert!(
            !body.contains("root:"),
            "traversal {path} disclosed /etc/passwd"
        );
    }

    // An oversized JSON body is refused before it is buffered.
    let huge = vec![b'A'; 2 * 1024 * 1024];
    let mut body = Vec::with_capacity(huge.len() + 16);
    body.extend_from_slice(br#"{"message":""#);
    body.extend_from_slice(&huge);
    body.extend_from_slice(br#""}"#);
    let (status, _) = http(
        p,
        "POST",
        "/api/chat",
        &[
            ("Host", &host),
            ("Origin", &format!("http://{host}")),
            ("Content-Type", "application/json"),
        ],
        &body,
    );
    assert_eq!(
        status, 413,
        "a 2 MiB JSON body must be refused, not buffered"
    );

    // A same-origin JSON request still reaches the handler (the guard is not a blanket denial).
    let (status, _) = http(
        p,
        "POST",
        "/api/model",
        &[
            ("Host", &host),
            ("Origin", &format!("http://{host}")),
            ("Content-Type", "application/json"),
        ],
        br#"{"name":"definitely-not-a-model"}"#,
    );
    assert_eq!(
        status, 400,
        "unknown model reaches the handler and is rejected there"
    );
}

// ---------------------------------------------------------------- 5. external data

struct MockComfy {
    url: String,
    stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl MockComfy {
    /// A ComfyUI server that answers every prompt with one output whose filename is hostile.
    fn with_hostile_filename(filename: &str) -> MockComfy {
        use tiny_http::{Header, Response, StatusCode};
        let server = tiny_http::Server::http("127.0.0.1:0").unwrap();
        let url = format!("http://{}", server.server_addr());
        let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let flag = stop.clone();
        let name = filename.to_string();
        let thread = std::thread::spawn(move || {
            while !flag.load(std::sync::atomic::Ordering::SeqCst) {
                let Ok(Some(req)) = server.recv_timeout(Duration::from_millis(50)) else {
                    continue;
                };
                let json = |code: u16, v: Value| {
                    let mut r =
                        Response::from_string(v.to_string()).with_status_code(StatusCode(code));
                    r.add_header(Header::from_bytes("Content-Type", "application/json").unwrap());
                    r
                };
                let path = req.url().split('?').next().unwrap_or("/").to_string();
                let resp = match (req.method().as_str(), path.as_str()) {
                    ("GET", "/system_stats") => {
                        json(200, json!({"system": {"comfyui_version": "mock"}}))
                    }
                    ("GET", "/object_info") => json(
                        200,
                        json!({
                            "VideoUpscale": {"input": {"required": {"video": ["STRING"],
                                                                  "scale": ["INT"]}}},
                            "SaveVideo": {"input": {"required": {"video": ["VIDEO"]}}}}),
                    ),
                    ("POST", "/prompt") => json(200, json!({"prompt_id": "pid-1", "number": 1})),
                    ("GET", "/history/pid-1") => json(
                        200,
                        json!({"pid-1": {"status": {"status_str": "success", "completed": true},
                            "outputs": {"9": {"images": [
                                {"filename": name, "subfolder": "", "type": "output"}]}}}}),
                    ),
                    ("GET", "/view") => Response::from_data(b"hostile output bytes".to_vec()),
                    _ => json(404, json!({})),
                };
                let _ = req.respond(resp);
            }
        });
        MockComfy {
            url,
            stop,
            thread: Some(thread),
        }
    }
}

impl Drop for MockComfy {
    fn drop(&mut self) {
        self.stop.store(true, std::sync::atomic::Ordering::SeqCst);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

/// A hostile ComfyUI response must not choose where CINVAR writes: only the final path component
/// of a server-supplied filename is used.
#[test]
fn a_hostile_comfy_output_filename_cannot_choose_a_local_path() {
    use cinvar::comfy::{self, Binding, ComfyClient, WorkflowDef};

    let dir = tempfile::tempdir().unwrap();
    let mock = MockComfy::with_hostile_filename("../../../../cinvar-adversarial-escape.mp4");
    let client = ComfyClient::new(&mock.url);
    let def = WorkflowDef {
        name: "hostile".into(),
        task: "upscale".into(),
        description: "mock".into(),
        workflow: json!({"3": {"class_type": "VideoUpscale", "inputs": {"video": "x", "scale": 2}},
                          "9": {"class_type": "SaveVideo", "inputs": {"video": ["3", 0]}}}),
        params: [(
            "scale".to_string(),
            Binding {
                node: "3".into(),
                input: "scale".into(),
                description: String::new(),
            },
        )]
        .into(),
        file_params: vec![],
    };
    let wf = comfy::bind(&def, json!({"scale": 4}).as_object().unwrap()).unwrap();
    let dest = dir.path().join("cache").join("comfy");
    std::fs::create_dir_all(&dest).unwrap();
    let out = comfy::run(
        &client,
        &wf,
        &dest,
        Duration::from_secs(10),
        Duration::from_millis(20),
        &|| false,
        &|_, _| {},
    )
    .unwrap();
    assert_eq!(out.files.len(), 1, "{:?}", out.files);
    let written = &out.files[0];
    assert!(
        written.starts_with(&dest),
        "server-chosen filename escaped the output directory: {}",
        written.display()
    );
    assert!(
        written
            .file_name()
            .unwrap()
            .to_string_lossy()
            .contains("cinvar-adversarial-escape.mp4"),
        "unexpected output name: {}",
        written.display()
    );
    assert!(
        !dir.path().join("cinvar-adversarial-escape.mp4").exists()
            && !Path::new("/cinvar-adversarial-escape.mp4").exists(),
        "a hostile filename wrote outside the cache directory"
    );
}
