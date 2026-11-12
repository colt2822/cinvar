//! The local web UI is unauthenticated, so it must refuse browser-driven abuse:
//! DNS rebinding (wrong Host), cross-site requests (foreign Origin) and no-preflight posts.
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

const BIN: &str = env!("CARGO_BIN_EXE_cinvar");

struct Ui(Child, u16);
impl Drop for Ui {
    fn drop(&mut self) {
        let _ = self.0.kill();
    }
}

fn start() -> Ui {
    let port = TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let cfg = tempfile::tempdir().unwrap().keep();
    let child = Command::new(BIN)
        .args(["ui", "--port", &port.to_string()])
        .env("CINVAR_CONFIG_DIR", cfg)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let t = Instant::now();
    while TcpStream::connect(("127.0.0.1", port)).is_err() {
        assert!(t.elapsed() < Duration::from_secs(10), "ui did not start");
        std::thread::sleep(Duration::from_millis(50));
    }
    Ui(child, port)
}

fn http(port: u16, method: &str, path: &str, headers: &[(&str, &str)], body: &str) -> u16 {
    let mut s = TcpStream::connect(("127.0.0.1", port)).unwrap();
    let mut req = format!("{method} {path} HTTP/1.1\r\nConnection: close\r\n");
    for (k, v) in headers {
        req.push_str(&format!("{k}: {v}\r\n"));
    }
    req.push_str(&format!("Content-Length: {}\r\n\r\n{body}", body.len()));
    s.write_all(req.as_bytes()).unwrap();
    let mut out = String::new();
    let _ = s.read_to_string(&mut out);
    out.split_whitespace().nth(1).unwrap().parse().unwrap()
}

#[test]
fn rebinding_csrf_and_simple_posts_are_refused() {
    let ui = start();
    let p = ui.1;
    let host = format!("127.0.0.1:{p}");
    assert_eq!(http(p, "GET", "/", &[("Host", &host)], ""), 200);
    // DNS rebinding: the page's hostname resolves to 127.0.0.1 but the Host header is the attacker's.
    assert_eq!(
        http(
            p,
            "GET",
            "/api/status",
            &[("Host", &format!("evil.example:{p}"))],
            ""
        ),
        403
    );
    // Cross-site fetch carries a foreign Origin.
    assert_eq!(
        http(
            p,
            "GET",
            "/",
            &[("Host", &host), ("Origin", "https://evil.example")],
            ""
        ),
        403
    );
    // A no-preflight "simple" POST (text/plain) must not reach the API.
    assert_eq!(
        http(
            p,
            "POST",
            "/api/chat",
            &[("Host", &host), ("Content-Type", "text/plain")],
            r#"{"message":"x"}"#
        ),
        403
    );
    // Same-origin JSON passes the guard (the handler may still reject the content).
    assert_ne!(
        http(
            p,
            "POST",
            "/api/chat",
            &[
                ("Host", &host),
                ("Origin", &format!("http://{host}")),
                ("Content-Type", "application/json")
            ],
            "{}"
        ),
        403
    );
}

#[test]
fn upload_endpoint_is_same_origin_only_and_octet_stream_only() {
    let ui = start();
    let p = ui.1;
    let host = format!("127.0.0.1:{p}");
    let origin = format!("http://127.0.0.1:{p}");
    // Foreign origin: refused before any bytes are read.
    assert_eq!(
        http(
            p,
            "POST",
            "/api/upload?name=a.mp4",
            &[
                ("Host", &host),
                ("Origin", "http://evil.example"),
                ("Content-Type", "application/octet-stream")
            ],
            "x"
        ),
        403
    );
    // A browser form post (no preflight) cannot use this endpoint.
    assert_eq!(
        http(
            p,
            "POST",
            "/api/upload?name=a.mp4",
            &[
                ("Host", &host),
                ("Origin", &origin),
                ("Content-Type", "text/plain")
            ],
            "x"
        ),
        403
    );
    // The octet-stream exemption does not extend to other routes.
    assert_eq!(
        http(
            p,
            "POST",
            "/api/chat",
            &[
                ("Host", &host),
                ("Origin", &origin),
                ("Content-Type", "application/octet-stream")
            ],
            "{}"
        ),
        403
    );
    // Same-origin upload with no project open is a clean error, not a crash.
    let code = http(
        p,
        "POST",
        "/api/upload?name=a.mp4",
        &[
            ("Host", &host),
            ("Origin", &origin),
            ("Content-Type", "application/octet-stream"),
        ],
        "x",
    );
    assert!(code == 404 || code == 400 || code == 500, "got {code}");
}
