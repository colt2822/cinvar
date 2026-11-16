//! Order-key redemption against a local stub entitlement service.
//!
//! Every request in this file goes to a std `TcpListener` bound on 127.0.0.1, so no test can reach
//! the network: `DEFAULT_REDEEM_URL` is asserted to be `None` (nothing ships configured) and each
//! redemption names its URL explicitly. The licenses are minted with a throwaway Ed25519 key that
//! is passed as the trust root, because the production private key is held by the operator and
//! never in the repository.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use base64::Engine;
use cinvar::edition::license::{self, PublicKey};
use cinvar::edition::{self, Edition, Enforcement};
use cinvar::error::ErrorCode;
use ed25519_dalek::{Signer, SigningKey};

// --- a license this test can mint and trust -------------------------------------------------

struct TestKey {
    signing: SigningKey,
    public: PublicKey,
}

fn test_key() -> TestKey {
    let signing = SigningKey::from_bytes(&[7u8; 32]);
    TestKey {
        public: PublicKey {
            id: "redeem-test".to_string(),
            bytes: signing.verifying_key().to_bytes(),
        },
        signing,
    }
}

fn envelope(key: &TestKey, payload: &[u8]) -> String {
    envelope_as(key, &key.public.id, payload)
}

fn envelope_as(key: &TestKey, key_id: &str, payload: &[u8]) -> String {
    let b64 = base64::engine::general_purpose::STANDARD;
    let signature = key.signing.sign(payload);
    serde_json::json!({
        "key_id": key_id,
        "payload": b64.encode(payload),
        "signature": b64.encode(signature.to_bytes()),
    })
    .to_string()
}

fn payload(id: &str) -> Vec<u8> {
    serde_json::to_vec(&serde_json::json!({
        "v": 1,
        "id": id,
        "edition": "pro",
        "order_ref": "ord_stub",
        "issued_at": "2026-11-15",
        "grandfathered": false,
    }))
    .unwrap()
}

fn mint(key: &TestKey, id: &str) -> String {
    envelope(key, &payload(id))
}

// --- stub entitlement service ---------------------------------------------------------------

struct Stub {
    url: String,
    hits: Arc<Mutex<Vec<String>>>,
}

impl Stub {
    fn requests(&self) -> Vec<String> {
        self.hits.lock().unwrap().clone()
    }
}

/// Serves `status` with `body` for every request, recording each request it received.
fn stub(status: u16, body: impl Into<String>) -> Stub {
    let body = body.into();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(false).unwrap();
    let addr = listener.local_addr().unwrap();
    let hits = Arc::new(Mutex::new(Vec::new()));
    let recorded = Arc::clone(&hits);
    std::thread::spawn(move || {
        for mut stream in listener.incoming().flatten().take(8) {
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut request = String::new();
            let mut content_length = 0usize;
            loop {
                let mut line = String::new();
                if reader.read_line(&mut line).unwrap_or(0) == 0 {
                    break;
                }
                if line == "\r\n" || line == "\n" {
                    break;
                }
                if let Some(value) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                    content_length = value.trim().parse().unwrap_or(0);
                }
                request.push_str(&line);
            }
            let mut buf = vec![0u8; content_length];
            let mut read = 0;
            while read < content_length {
                match reader.read(&mut buf[read..]) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => read += n,
                }
            }
            request.push_str(&String::from_utf8_lossy(&buf[..read]));
            recorded.lock().unwrap().push(request);

            let reason = if status == 200 { "OK" } else { "Error" };
            let response = format!(
                "HTTP/1.1 {status} {reason}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = stream.write_all(response.as_bytes());
            let _ = stream.flush();
        }
    });
    Stub {
        url: format!("http://{addr}/redeem"),
        hits,
    }
}

fn install_dir() -> tempfile::TempDir {
    tempfile::tempdir().unwrap()
}

// --- redemption ------------------------------------------------------------------------------

#[test]
fn successful_redeem_installs_the_license_and_resolves_to_pro() {
    let key = test_key();
    let service = stub(200, mint(&key, "L-REDEEM-1"));
    let dir = install_dir();
    let dest = dir.path().join("license.json");

    let verified =
        edition::redeem_order_key_from("ORD-KEY-42", &service.url, &dest, &[key.public.clone()])
            .expect("stub returned a good license");
    assert_eq!(verified.id, "L-REDEEM-1");
    assert!(dest.is_file(), "license installed at {}", dest.display());

    let entitlements = edition::resolve_from(Enforcement::Enforce, &dest, &[key.public.clone()]);
    assert_eq!(entitlements.edition, Edition::Pro);
    assert_eq!(license::load(&dest, &[key.public.clone()]).unwrap().unwrap().id, "L-REDEEM-1");

    let requests = service.requests();
    assert_eq!(requests.len(), 1, "exactly one request");
    assert!(requests[0].starts_with("POST /redeem "), "{}", requests[0]);
    assert!(requests[0].contains("ORD-KEY-42"), "the order key is what is exchanged");
    assert!(!dir.path().join("license.json.tmp").exists(), "no partial file left");
}

#[test]
fn tampered_signature_is_rejected_and_installs_nothing() {
    let key = test_key();
    let signed = payload("L-TAMPER");
    let mut served: serde_json::Value = serde_json::from_str(&envelope(&key, &signed)).unwrap();
    let mut corrupted = signed;
    corrupted[0] ^= 0x01;
    served["payload"] = serde_json::json!(base64::engine::general_purpose::STANDARD.encode(&corrupted));
    let service = stub(200, served.to_string());
    let dir = install_dir();
    let dest = dir.path().join("license.json");

    let err = edition::redeem_order_key_from("ORD-T", &service.url, &dest, &[key.public.clone()])
        .expect_err("a tampered envelope must not install");
    assert!(err.message.contains("signature"), "{}", err.message);
    assert!(!dest.exists(), "nothing installed");
}

#[test]
fn server_error_leaves_an_existing_working_license_untouched() {
    let key = test_key();
    let dir = install_dir();
    let dest = dir.path().join("license.json");
    license::install_text(&mint(&key, "L-OLD"), &dest, &[key.public.clone()]).unwrap();
    let before = std::fs::read(&dest).unwrap();

    let service = stub(500, "{\"error\":\"boom\"}");
    let err = edition::redeem_order_key_from("ORD-500", &service.url, &dest, &[key.public.clone()])
        .expect_err("a 500 must not install");
    assert_eq!(err.code, ErrorCode::Provider);
    assert!(err.message.contains("HTTP 500"), "{}", err.message);
    assert!(err.message.contains("unchanged"), "{}", err.message);
    assert_eq!(std::fs::read(&dest).unwrap(), before, "installed license byte-identical");
    assert_eq!(
        license::load(&dest, &[key.public.clone()]).unwrap().unwrap().id,
        "L-OLD"
    );
    assert!(!dir.path().join("license.json.tmp").exists());
}

#[test]
fn not_found_reports_an_unknown_order_key() {
    let key = test_key();
    let service = stub(404, "{\"error\":\"no such order\"}");
    let dir = install_dir();
    let dest = dir.path().join("license.json");

    let err = edition::redeem_order_key_from("ORD-NOPE", &service.url, &dest, &[key.public.clone()])
        .expect_err("404 is an error");
    assert_eq!(err.code, ErrorCode::NotFound);
    assert!(err.message.contains("unknown order key"), "{}", err.message);
    assert!(err.message.contains("HTTP 404"), "{}", err.message);
    assert!(!dest.exists());
}

#[test]
fn already_redeemed_and_rate_limited_are_distinct_failures() {
    let key = test_key();
    let dir = install_dir();
    let dest = dir.path().join("license.json");

    let conflict = stub(409, "{}");
    let err = edition::redeem_order_key_from("ORD-DUP", &conflict.url, &dest, &[key.public.clone()])
        .expect_err("409 is an error");
    assert_eq!(err.code, ErrorCode::Conflict);
    assert!(err.message.contains("already been redeemed"), "{}", err.message);

    let limited = stub(429, "{}");
    let err = edition::redeem_order_key_from("ORD-RL", &limited.url, &dest, &[key.public.clone()])
        .expect_err("429 is an error");
    assert_eq!(err.code, ErrorCode::Provider);
    assert!(err.message.contains("rate limiting"), "{}", err.message);

    assert!(!dest.exists());
}

#[test]
fn malformed_and_empty_bodies_are_rejected_without_installing() {
    let key = test_key();
    let dir = install_dir();
    let dest = dir.path().join("license.json");

    let html = stub(200, "<html>not a license</html>");
    let err = edition::redeem_order_key_from("ORD-H", &html.url, &dest, &[key.public.clone()])
        .expect_err("garbage is not a license");
    assert!(err.message.contains("no usable license"), "{}", err.message);
    assert!(err.message.contains("not valid JSON"), "{}", err.message);

    let empty = stub(200, "");
    let err = edition::redeem_order_key_from("ORD-E", &empty.url, &dest, &[key.public.clone()])
        .expect_err("an empty body is not a license");
    assert_eq!(err.code, ErrorCode::Protocol);
    assert!(err.message.contains("empty response"), "{}", err.message);

    assert!(!dest.exists());
}

#[test]
fn network_failure_is_reported_and_installs_nothing() {
    let key = test_key();
    let dead = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}/redeem", dead.local_addr().unwrap());
    drop(dead);
    let dir = install_dir();
    let dest = dir.path().join("license.json");

    let err = edition::redeem_order_key_from("ORD-NET", &url, &dest, &[key.public.clone()])
        .expect_err("a dead endpoint is an error");
    assert_eq!(err.code, ErrorCode::Provider);
    assert!(err.message.contains("could not reach"), "{}", err.message);
    assert!(err.message.contains("127.0.0.1"), "{}", err.message);
    assert!(!err.message.contains("ORD-NET"), "the order key must never appear in an error");
    assert!(!dest.exists());
}

#[test]
fn the_order_key_is_never_echoed_in_a_failure() {
    let key = test_key();
    let service = stub(500, "{\"echoed\":\"ORD-SECRET-9\"}");
    let dir = install_dir();
    let dest = dir.path().join("license.json");
    let err = edition::redeem_order_key_from("ORD-SECRET-9", &service.url, &dest, &[key.public.clone()])
        .expect_err("500 is an error");
    assert!(!err.message.contains("ORD-SECRET-9"), "{}", err.message);
    assert!(!format!("{err}").contains("ORD-SECRET-9"));
}

// --- configuration precedence ---------------------------------------------------------------

#[test]
fn no_service_ships_configured_and_precedence_is_explicit_then_config_then_env() {
    assert!(
        edition::DEFAULT_REDEEM_URL.is_none(),
        "nothing ships configured: the ticket asked for no invented endpoint"
    );
    assert_eq!(edition::REDEEM_URL_ENV, "CINVAR_LICENSE_REDEEM_URL");

    let pick = |a: Option<&str>, b: Option<&str>, c: Option<&str>| edition::pick_redeem_url(a, b, c);
    assert_eq!(pick(None, None, None), None);
    assert_eq!(pick(None, Some("  "), None), None, "blank config is unset");
    assert_eq!(pick(None, None, Some("")), None, "empty env is unset");
    assert_eq!(pick(Some("http://a/1"), Some("http://c/2"), Some("http://e/3")), Some("http://a/1".into()));
    assert_eq!(pick(None, Some("http://c/2"), Some("http://e/3")), Some("http://c/2".into()));
    assert_eq!(pick(None, None, Some("http://e/3")), Some("http://e/3".into()));
    assert_eq!(pick(Some(" "), Some("http://c/2"), None), Some("http://c/2".into()));
}

#[test]
fn a_redemption_without_a_configured_url_fails_before_any_request() {
    let dir = install_dir();
    let dest = dir.path().join("license.json");
    // No config file, no environment variable, and no shipped default: the resolver is empty, so
    // redemption cannot reach any host at all.
    let cfg = cinvar::config::Config::default();
    assert_eq!(cfg.edition.redeem_url, "", "default config carries no endpoint");
    assert_eq!(edition::pick_redeem_url(None, Some(&cfg.edition.redeem_url), None), None);
    assert!(!dest.exists());
}

// --- the real CLI against the stub -----------------------------------------------------------

fn cinvar_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_cinvar"))
}

fn run_cli(dir: &Path, args: &[&str], env: &[(&str, &str)], clear: &[&str]) -> (bool, String) {
    let mut cmd = std::process::Command::new(cinvar_bin());
    cmd.args(args)
        .env("CINVAR_CONFIG_DIR", dir)
        .env_remove("CINVAR_LICENSE_REDEEM_URL")
        .env_remove("XDG_CONFIG_HOME")
        .env_remove("HOME")
        .current_dir(dir)
        .stdin(std::process::Stdio::null());
    for (k, v) in env {
        cmd.env(k, v);
    }
    for k in clear {
        cmd.env_remove(k);
    }
    let out = cmd.output().expect("run cinvar");
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    (out.status.success(), text)
}

#[test]
fn cli_redeems_through_the_service_url_from_the_environment() {
    let key = test_key();
    let service = stub(200, envelope_as(&key, "k1", &payload("L-CLI")));
    let dir = install_dir();

    let (ok, text) = run_cli(
        dir.path(),
        &["activate", "ORD-CLI-42"],
        &[(edition::REDEEM_URL_ENV, &service.url)],
        &[],
    );

    // The stub is trusted only by this test, so the shipped verifier must refuse its license: the
    // CLI reached the service, exchanged the key, verified, and installed nothing.
    assert!(!ok, "production keys do not trust the test key: {text}");
    assert!(text.contains("no usable license"), "{text}");
    assert!(text.contains("signature"), "{text}");
    assert!(!text.contains("ORD-CLI-42"), "the CLI must never echo the order key: {text}");
    assert!(!dir.path().join("license.json").exists(), "nothing installed");

    let requests = service.requests();
    assert_eq!(requests.len(), 1, "the CLI made exactly one request: {requests:?}");
    assert!(requests[0].starts_with("POST /redeem "), "{}", requests[0]);
    assert!(requests[0].contains("ORD-CLI-42"), "{}", requests[0]);
}

#[test]
fn cli_config_file_is_used_and_beats_the_environment() {
    let key = test_key();
    let from_config = stub(200, mint(&key, "L-CFG"));
    let from_env = stub(200, mint(&key, "L-ENV"));
    let dir = install_dir();
    std::fs::write(
        dir.path().join("config.toml"),
        format!("[edition]\nredeem_url = \"{}\"\n", from_config.url),
    )
    .unwrap();

    let (ok, text) = run_cli(
        dir.path(),
        &["activate", "ORD-CFG"],
        &[(edition::REDEEM_URL_ENV, &from_env.url)],
        &[],
    );
    assert!(!ok, "{text}");
    assert_eq!(from_config.requests().len(), 1, "config endpoint served the request");
    assert!(from_env.requests().is_empty(), "the environment did not win");
}

#[test]
fn cli_without_a_service_url_contacts_nothing_and_says_so() {
    let dir = install_dir();
    let (ok, text) = run_cli(dir.path(), &["activate", "ORD-NOURL"], &[], &[]);
    assert!(!ok);
    assert!(text.contains("none is deployed or configured"), "{text}");
    assert!(!text.contains("ORD-NOURL"), "{text}");
    assert!(!dir.path().join("license.json").exists());
}
