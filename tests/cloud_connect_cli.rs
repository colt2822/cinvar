//! `cinvar cloud connect|status|disconnect` against a local fake RunPod endpoint.
use std::io::Write;
use std::process::{Command, Stdio};
use tiny_http::{Header, Response, Server, StatusCode};

const BIN: &str = env!("CARGO_BIN_EXE_cinvar");

fn fake_runpod() -> (String, std::sync::Arc<std::sync::atomic::AtomicBool>) {
    let server = Server::http("127.0.0.1:0").unwrap();
    let url = format!("http://{}", server.server_addr());
    let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let s2 = stop.clone();
    std::thread::spawn(move || {
        while !s2.load(std::sync::atomic::Ordering::SeqCst) {
            if let Ok(Some(req)) = server.recv_timeout(std::time::Duration::from_millis(100)) {
                let ok = req.headers().iter().any(|h| {
                    h.field.equiv("Authorization")
                        && h.value.as_str() == "Bearer rpa_GOODKEY_123456789"
                });
                let mut r = Response::from_string(if ok {
                    "[]"
                } else {
                    "{\"error\":\"unauthorized\"}"
                })
                .with_status_code(StatusCode(if ok { 200 } else { 401 }));
                r.add_header(Header::from_bytes("Content-Type", "application/json").unwrap());
                let _ = req.respond(r);
            }
        }
    });
    (url, stop)
}

fn run(cfg: &std::path::Path, base: &str, args: &[&str], stdin: Option<&str>) -> (bool, String) {
    let mut c = Command::new(BIN);
    c.args(args)
        .env("CINVAR_CONFIG_DIR", cfg)
        .env("CINVAR_RUNPOD_BASE", base)
        .env_remove("CINVAR_RUNPOD_API_KEY")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = c.spawn().unwrap();
    if let Some(s) = stdin {
        child
            .stdin
            .as_mut()
            .unwrap()
            .write_all(s.as_bytes())
            .unwrap();
    }
    drop(child.stdin.take());
    let out = child.wait_with_output().unwrap();
    (
        out.status.success(),
        format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        ),
    )
}

#[test]
fn connect_validates_before_storing_and_never_echoes_the_key() {
    let (base, stop) = fake_runpod();
    let d = tempfile::tempdir().unwrap();
    let (ok, text) = run(
        d.path(),
        &base,
        &["cloud", "connect"],
        Some("rpa_WRONGKEY_000000000\n"),
    );
    assert!(!ok);
    assert!(!text.contains("WRONGKEY"), "key leaked: {text}");
    assert!(
        !d.path().join("credentials.toml").exists(),
        "a rejected key must not be stored"
    );

    let (ok, text) = run(
        d.path(),
        &base,
        &["cloud", "connect"],
        Some("rpa_GOODKEY_123456789\n"),
    );
    assert!(ok, "{text}");
    assert!(!text.contains("GOODKEY"), "key leaked: {text}");
    let creds = d.path().join("credentials.toml");
    assert!(creds.exists());
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(&creds).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
    let (ok, text) = run(d.path(), &base, &["--json", "cloud", "status"], None);
    assert!(
        ok && text.contains("\"accepted_by_runpod\":true")
            || text.contains("\"accepted_by_runpod\": true"),
        "{text}"
    );
    assert!(!text.contains("GOODKEY"));

    let (ok, _) = run(d.path(), &base, &["cloud", "disconnect"], None);
    assert!(ok);
    let (_, text) = run(d.path(), &base, &["--json", "cloud", "status"], None);
    assert!(
        text.contains("\"configured\":false") || text.contains("\"configured\": false"),
        "{text}"
    );
    stop.store(true, std::sync::atomic::Ordering::SeqCst);
}
