//! Billable cloud operations need approval that only a human can create. An agent can request a
//! quote and echo every argument, but cannot start a render without the approval record.
use std::io::{BufRead, BufReader, Write};
use std::process::{Command, Stdio};

use serde_json::{json, Value};

const BIN: &str = env!("CARGO_BIN_EXE_cinvar");

struct Srv {
    child: std::process::Child,
    stdin: std::process::ChildStdin,
    out: BufReader<std::process::ChildStdout>,
    id: u64,
}
impl Srv {
    fn call(&mut self, name: &str, args: Value) -> Value {
        self.id += 1;
        let req = json!({"jsonrpc":"2.0","id":self.id,"method":"tools/call","params":{"name":name,"arguments":args}});
        writeln!(self.stdin, "{req}").unwrap();
        let mut line = String::new();
        self.out.read_line(&mut line).unwrap();
        serde_json::from_str(&line).unwrap()
    }
}
impl Drop for Srv {
    fn drop(&mut self) {
        let _ = self.child.kill();
    }
}
fn text(r: &Value) -> String {
    r["result"]["content"][0]["text"]
        .as_str()
        .unwrap_or("")
        .to_string()
}

#[test]
fn agent_cannot_self_approve_a_billable_render() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = dir.path().join("cfg");
    std::fs::create_dir_all(&cfg).unwrap();
    std::fs::write(
        cfg.join("config.toml"),
        "[security]\nallow_cloud_render = true\n",
    )
    .unwrap();
    std::env::set_var("CINVAR_CONFIG_DIR", &cfg);
    let ok = Command::new(BIN)
        .args(["project", "create", "--name", "cloud", "--dir"])
        .arg(dir.path())
        .output()
        .unwrap();
    assert!(ok.status.success());
    let project = dir.path().join("cloud");
    let mut child = Command::new(BIN)
        .args(["mcp", "serve", "--project"])
        .arg(&project)
        .env("CINVAR_CONFIG_DIR", &cfg)
        .env("CINVAR_MCP_PERMISSIONS", "full")
        .env("CINVAR_MEDIA_ROOTS", dir.path())
        .env("CINVAR_RUNPOD_API_KEY", "rpa_FAKEKEYFORTESTS_0123456789")
        .env("CINVAR_RUNPOD_BASE", "http://127.0.0.1:9") // nothing listens: no network is ever reached
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let stdin = child.stdin.take().unwrap();
    let out = BufReader::new(child.stdout.take().unwrap());
    let mut s = Srv {
        child,
        stdin,
        out,
        id: 0,
    };
    // A real (tiny) clip on a timeline, so there is something to package.
    let clip = dir.path().join("c.mp4");
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
        .arg(&clip)
        .status()
        .unwrap();
    assert!(st.success());
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
    let base = json!({"max_price_per_hr": 1.0, "max_runtime_minutes": 10, "image": "attacker/image:latest"});

    // 1. The quote step only records a pending quote and says a human must approve it.
    let q = s.call("cinvar.render.cloud", base.clone());
    assert_ne!(q["result"]["isError"], true, "{q}");
    let quote: Value = serde_json::from_str(&text(&q)).unwrap();
    assert_eq!(quote["needs_human_approval"], true);
    let id = quote["quote"]["id"].as_str().unwrap().to_string();

    // 2. Echoing the id and acknowledging the risk is not an authorization.
    let mut confirm = base.clone();
    confirm["confirm_quote"] = json!(id);
    confirm["acknowledge_billing_risk"] = json!(true);
    let r = s.call("cinvar.render.cloud", confirm.clone());
    assert_eq!(r["result"]["isError"], true, "{r}");
    assert!(text(&r).contains("approval from a human"), "{r}");
    assert!(text(&r).contains("cinvar cloud approve"), "{r}");

    // 3. The CLI refuses to approve without a terminal (stdin here is a pipe).
    let cli = Command::new(BIN)
        .args(["cloud", "approve", &id])
        .env("CINVAR_CONFIG_DIR", &cfg)
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert!(!cli.status.success());
    assert!(String::from_utf8_lossy(&cli.stderr).contains("interactive terminal"));
    assert!(!cfg.join("cloud").join("approved").join(&id).exists());

    // 4. A human approval (what the interactive CLI writes) is accepted exactly once.
    cinvar::cloud::approval::approve(&id).unwrap();
    let r = s.call("cinvar.render.cloud", confirm.clone());
    assert_ne!(
        r["result"]["isError"], true,
        "approved quote must start: {r}"
    );
    let r = s.call("cinvar.render.cloud", confirm);
    assert_eq!(r["result"]["isError"], true, "approval is single-use: {r}");
}
