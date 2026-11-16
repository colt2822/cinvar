use super::*;
use std::io::{BufRead, BufReader, Read as _, Write as _};
use std::net::TcpListener;
use std::sync::{Arc, Mutex};

type Seen = Arc<Mutex<Vec<(String, String, String, String)>>>; // method, path, headers(lowercased), body
type Reply = (u16, &'static str, Vec<u8>);

/// One-request-per-connection HTTP mock. The handler gets (method, path, headers, body, base).
fn mock<F>(handler: F) -> (String, Seen)
where
    F: Fn(&str, &str, &str, &str, &str) -> Reply + Send + Sync + 'static,
{
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let seen: Seen = Arc::new(Mutex::new(Vec::new()));
    let (b, s, h) = (base.clone(), seen.clone(), Arc::new(handler));
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            let (b, s, h) = (b.clone(), s.clone(), h.clone());
            std::thread::spawn(move || {
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut first = String::new();
                if reader.read_line(&mut first).unwrap_or(0) == 0 {
                    return;
                }
                let mut parts = first.split_whitespace();
                let (method, path) = (parts.next().unwrap_or("").to_string(), parts.next().unwrap_or("").to_string());
                let (mut headers, mut len) = (String::new(), 0usize);
                loop {
                    let mut line = String::new();
                    reader.read_line(&mut line).unwrap();
                    if line == "\r\n" || line.is_empty() {
                        break;
                    }
                    let low = line.to_ascii_lowercase();
                    if let Some(v) = low.strip_prefix("content-length:") {
                        len = v.trim().parse().unwrap_or(0);
                    }
                    headers.push_str(&low);
                }
                let mut body = vec![0u8; len];
                reader.read_exact(&mut body).ok();
                let body = String::from_utf8_lossy(&body).to_string();
                s.lock().unwrap().push((method.clone(), path.clone(), headers.clone(), body.clone()));
                let (code, ct, bytes) = h(&method, &path, &headers, &body, &b);
                let head = format!("HTTP/1.1 {code} X\r\ncontent-type: {ct}\r\ncontent-length: {}\r\nconnection: close\r\n\r\n", bytes.len());
                stream.write_all(head.as_bytes()).ok();
                stream.write_all(&bytes).ok();
            });
        }
    });
    (base, seen)
}

fn js(v: Value) -> Reply {
    (200, "application/json", v.to_string().into_bytes())
}

fn req(model: &str, secs: u32, aspect: &str) -> GenRequest {
    GenRequest { prompt: "a fox".into(), seconds: secs, aspect: aspect.into(), model: model.into(), extra: Default::default() }
}

fn run(backend: Backend, base: &str, r: &GenRequest) -> (Result<Vec<PathBuf>>, tempfile_dir::Dir) {
    let dir = tempfile_dir::Dir::new();
    let ep = Endpoint { backend, base: base.to_string(), key: "KEY123".into() };
    let ctl = Control { cancelled: &|| false, progress: &|_, _| {}, poll_every: Duration::from_millis(1), timeout: Duration::from_secs(5) };
    let out = generate(&ep, r, dir.path(), &ctl);
    (out, dir)
}

mod tempfile_dir {
    pub struct Dir(std::path::PathBuf);
    impl Dir {
        pub fn new() -> Dir {
            let p = std::env::temp_dir().join(format!("cinvar-vg-test-{}", crate::util::ids::new_id("t")));
            std::fs::create_dir_all(&p).unwrap();
            Dir(p)
        }
        pub fn path(&self) -> &std::path::Path {
            &self.0
        }
    }
    impl Drop for Dir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
}

const MP4: &[u8] = b"\x00\x00\x00\x18ftypmp42FAKEVIDEO";

#[test]
fn fal_submit_poll_download_and_wan_frame_mapping() {
    let polls = Arc::new(Mutex::new(0));
    let p = polls.clone();
    let (base, seen) = mock(move |m, path, _h, _b, base| match (m, path) {
        ("POST", "/fal-ai/wan/v2.2-a14b/text-to-video") => js(json!({"request_id": "r1", "status_url": format!("{base}/fal-ai/wan/requests/r1/status"), "response_url": format!("{base}/fal-ai/wan/requests/r1")})),
        ("GET", "/fal-ai/wan/requests/r1/status") => {
            let mut n = p.lock().unwrap();
            *n += 1;
            js(json!({"status": if *n < 3 { "IN_PROGRESS" } else { "COMPLETED" }}))
        }
        ("GET", "/fal-ai/wan/requests/r1") => js(json!({"video": {"url": format!("{base}/out.mp4")}})),
        ("GET", "/out.mp4") => (200, "video/mp4", MP4.to_vec()),
        _ => (404, "text/plain", vec![]),
    });
    let (out, _d) = run(Backend::Fal, &base, &req("fal-ai/wan/v2.2-a14b/text-to-video", 5, "9:16"));
    let files = out.unwrap();
    assert_eq!(std::fs::read(&files[0]).unwrap(), MP4);
    assert!(*polls.lock().unwrap() >= 3, "polled until COMPLETED");
    let s = seen.lock().unwrap();
    let submit = s.iter().find(|r| r.0 == "POST").unwrap();
    assert!(submit.2.contains("authorization: key key123"), "fal auth scheme is `Key <key>`");
    let body: Value = serde_json::from_str(&submit.3).unwrap();
    assert_eq!(body["num_frames"], 81); // 5s * 16fps + 1
    assert_eq!(body["aspect_ratio"], "9:16");
}

#[test]
fn fal_refuses_a_non_https_download_link() {
    let (base, _s) = mock(|m, path, _h, _b, base| match (m, path) {
        ("POST", _) => js(json!({"status_url": format!("{base}/s"), "response_url": format!("{base}/r")})),
        ("GET", "/s") => js(json!({"status": "COMPLETED"})),
        ("GET", "/r") => js(json!({"video": {"url": "http://evil.example/x.mp4"}})),
        _ => (404, "text/plain", vec![]),
    });
    let (out, _d) = run(Backend::Fal, &base, &req("fal-ai/other/model", 4, "16:9"));
    assert!(out.unwrap_err().message.contains("non-https"));
}

#[test]
fn runway_headers_ratio_throttle_then_success() {
    let n = Arc::new(Mutex::new(0));
    let c = n.clone();
    let (base, seen) = mock(move |m, path, _h, _b, base| match (m, path) {
        ("POST", "/v1/text_to_video") => js(json!({"id": "t-1"})),
        ("GET", "/v1/tasks/t-1") => {
            let mut k = c.lock().unwrap();
            *k += 1;
            match *k {
                1 => js(json!({"status": "THROTTLED"})),
                2 => js(json!({"status": "RUNNING"})),
                _ => js(json!({"status": "SUCCEEDED", "output": [format!("{base}/clip.mp4")]})),
            }
        }
        ("GET", "/clip.mp4") => (200, "video/mp4", MP4.to_vec()),
        _ => (404, "text/plain", vec![]),
    });
    let (out, _d) = run(Backend::Runway, &base, &req("gen4.5", 6, "9:16"));
    assert_eq!(std::fs::read(&out.unwrap()[0]).unwrap(), MP4);
    let s = seen.lock().unwrap();
    let submit = s.iter().find(|r| r.0 == "POST").unwrap();
    assert!(submit.2.contains("authorization: bearer key123"));
    assert!(submit.2.contains("x-runway-version: 2024-11-06"));
    let body: Value = serde_json::from_str(&submit.3).unwrap();
    assert_eq!((body["ratio"].as_str(), body["duration"].as_i64(), body["promptText"].as_str()), (Some("720:1280"), Some(6), Some("a fox")));
}

#[test]
fn runway_failure_reports_the_reason() {
    let (base, _s) = mock(|m, path, _h, _b, _| match (m, path) {
        ("POST", _) => js(json!({"id": "x"})),
        _ => js(json!({"status": "FAILED", "failure": "content moderation"})),
    });
    let (out, _d) = run(Backend::Runway, &base, &req("gen4.5", 5, "16:9"));
    assert!(out.unwrap_err().message.contains("content moderation"));
}

#[test]
fn luma_dreaming_then_completed() {
    let n = Arc::new(Mutex::new(0));
    let c = n.clone();
    let (base, seen) = mock(move |m, path, _h, _b, base| match (m, path) {
        ("POST", "/generations") => js(json!({"id": "g1", "state": "queued"})),
        ("GET", "/generations/g1") => {
            let mut k = c.lock().unwrap();
            *k += 1;
            if *k < 2 { js(json!({"state": "dreaming"})) } else { js(json!({"state": "completed", "assets": {"video": format!("{base}/v.mp4")}})) }
        }
        ("GET", "/v.mp4") => (200, "video/mp4", MP4.to_vec()),
        _ => (404, "text/plain", vec![]),
    });
    let (out, _d) = run(Backend::Luma, &base, &req("ray-2", 8, "16:9"));
    assert_eq!(std::fs::read(&out.unwrap()[0]).unwrap(), MP4);
    let body: Value = serde_json::from_str(&seen.lock().unwrap().iter().find(|r| r.0 == "POST").unwrap().3).unwrap();
    assert_eq!((body["duration"].as_str(), body["model"].as_str()), (Some("9s"), Some("ray-2")));
}

#[test]
fn gemini_inline_base64_video() {
    let b64 = base64::engine::general_purpose::STANDARD.encode(MP4);
    let (base, seen) = mock(move |m, path, _h, _b, _| match (m, path) {
        ("POST", "/v1beta/interactions") => js(json!({"steps": [{"type": "model_output", "content": [{"type": "text", "text": "ok"}, {"type": "video", "mime_type": "video/mp4", "data": b64}]}]})),
        _ => (404, "text/plain", vec![]),
    });
    let (out, _d) = run(Backend::Gemini, &base, &req("gemini-omni-1.2-flash", 20, "9:16"));
    assert_eq!(std::fs::read(&out.unwrap()[0]).unwrap(), MP4);
    let s = seen.lock().unwrap();
    assert!(s[0].2.contains("x-goog-api-key: key123"));
    let body: Value = serde_json::from_str(&s[0].3).unwrap();
    assert_eq!(body["response_format"]["duration"], "15s"); // clamped to the model's maximum
    assert_eq!(body["response_format"]["aspect_ratio"], "9:16");
}

#[test]
fn gemini_uri_delivery_waits_for_file_active() {
    let n = Arc::new(Mutex::new(0));
    let c = n.clone();
    let (base, _s) = mock(move |m, path, headers, _b, base| match (m, path) {
        ("POST", "/v1beta/interactions") => js(json!({"steps": [{"content": [{"type": "video", "mime_type": "video/mp4", "uri": format!("{base}/v1beta/files/abc123")}]}]})),
        ("GET", "/v1beta/files/abc123") => {
            let mut k = c.lock().unwrap();
            *k += 1;
            js(json!({"state": if *k < 2 { "PROCESSING" } else { "ACTIVE" }}))
        }
        ("GET", "/v1beta/files/abc123:download?alt=media") => {
            assert!(headers.contains("x-goog-api-key: key123"), "download must carry the key");
            (200, "video/mp4", MP4.to_vec())
        }
        _ => (404, "text/plain", vec![]),
    });
    let (out, _d) = run(Backend::Gemini, &base, &req("gemini-omni-1.2-flash", 5, "16:9"));
    assert_eq!(std::fs::read(&out.unwrap()[0]).unwrap(), MP4);
}

#[test]
fn gemini_without_a_video_explains_why() {
    let (base, _s) = mock(|_, _, _, _, _| js(json!({"steps": [{"content": [{"type": "text", "text": "I can't make that"}]}]})));
    let (out, _d) = run(Backend::Gemini, &base, &req("gemini-omni-1.2-flash", 5, "16:9"));
    assert!(out.unwrap_err().message.contains("no video"));
}

#[test]
fn provider_errors_never_leak_the_key_and_cancel_works() {
    let (base, _s) = mock(|_, _, _, _, _| (401, "application/json", br#"{"error":"bad key KEY123"}"#.to_vec()));
    let (out, _d) = run(Backend::Luma, &base, &req("ray-2", 5, "16:9"));
    assert!(out.unwrap_err().message.contains("HTTP 401"));

    let (base, _s) = mock(|m, _, _, _, _| if m == "POST" { js(json!({"id": "x"})) } else { js(json!({"state": "dreaming"})) });
    let dir = tempfile_dir::Dir::new();
    let ep = Endpoint { backend: Backend::Luma, base, key: "k".into() };
    let ctl = Control { cancelled: &|| true, progress: &|_, _| {}, poll_every: Duration::from_millis(1), timeout: Duration::from_secs(5) };
    assert_eq!(generate(&ep, &req("ray-2", 5, "16:9"), dir.path(), &ctl).unwrap_err().code, ErrorCode::Cancelled);
}

#[test]
fn budget_requires_opt_in_and_respects_per_job_cap() {
    let off = VideoGenSettings::default();
    assert_eq!(check_budget(&off, 5).unwrap_err().code, ErrorCode::PermissionDenied);
    let on = VideoGenSettings { enabled: true, max_job_seconds: 8, max_day_seconds: 100_000 };
    assert!(check_budget(&on, 8).is_ok());
    assert_eq!(check_budget(&on, 9).unwrap_err().code, ErrorCode::Budget);
    assert_eq!(check_budget(&on, 0).unwrap_err().code, ErrorCode::Budget);
}
