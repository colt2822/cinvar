//! The 14 Studio route+method pairs, exercised over real HTTP against a running server.
//!
//! These run in the Free edition on purpose: no license file exists in the isolated config
//! directory, so every Pro-gated action must answer 403 with the edition's own message, and the
//! ungated routes must answer their documented shapes. Nothing here touches the machine's real
//! configuration, project list, or license.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::time::Duration;

use cinvar::project::model::Cue;
use cinvar::project::store::ProjectStore;
use cinvar::util::time::Time;
use serde_json::Value;

/// Points the server's config and data directories at a throwaway tree, once per test binary.
fn isolate() -> PathBuf {
    static DIR: std::sync::OnceLock<tempfile::TempDir> = std::sync::OnceLock::new();
    DIR.get_or_init(|| {
        let dir = tempfile::tempdir().unwrap();
        let config = dir.path().join("config");
        std::fs::create_dir_all(&config).unwrap();
        // This package builds the library with edition-observe for `cargo test`, where limits are
        // reported but not enforced. `edition.enforce = true` can only tighten, so it turns the
        // enforcement back on and the 403 assertions below test the real gate.
        std::fs::write(
            config.join("config.toml"),
            "version = 1\n\n[edition]\nenforce = true\n",
        )
        .unwrap();
        std::env::set_var("CINVAR_CONFIG_DIR", &config);
        std::env::set_var("CINVAR_DATA_DIR", dir.path().join("data"));
        dir
    })
    .path()
    .to_path_buf()
}

fn free_port() -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.local_addr().unwrap().port()
}

/// Start the real server on a free port and wait until it answers.
fn start(project: Option<PathBuf>) -> u16 {
    isolate();
    let port = free_port();
    let dir = project.clone();
    std::thread::spawn(move || {
        let _ = cinvar::server::serve(&cinvar::config::Config::default(), port, dir);
    });
    for _ in 0..400 {
        if TcpStream::connect(("127.0.0.1", port)).is_ok() {
            return port;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    panic!("the server never started on port {port}");
}

fn new_project() -> PathBuf {
    let root = isolate().join(format!("project-{}", free_port()));
    let (store, _project) = ProjectStore::create_at(&root, "Studio Test").unwrap();
    store.root
}

fn call(port: u16, method: &str, path: &str, body: Option<&str>) -> (u16, Value) {
    let mut stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
    stream.set_read_timeout(Some(Duration::from_secs(120))).unwrap();
    let payload = body.unwrap_or("");
    let request = format!(
        "{method} {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{payload}",
        payload.len()
    );
    stream.write_all(request.as_bytes()).unwrap();
    let mut raw = Vec::new();
    stream.read_to_end(&mut raw).unwrap();
    let text = String::from_utf8_lossy(&raw).to_string();
    let status = text
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse::<u16>().ok())
        .unwrap_or(0);
    let body = text.split("\r\n\r\n").nth(1).unwrap_or("").to_string();
    let value = serde_json::from_str(&body).unwrap_or(Value::Null);
    (status, value)
}

fn get(port: u16, path: &str) -> (u16, Value) {
    call(port, "GET", path, None)
}

fn post(port: u16, path: &str, body: Value) -> (u16, Value) {
    call(port, "POST", path, Some(&body.to_string()))
}

fn assert_not_missing(status: u16, body: &Value, path: &str) {
    assert_eq!(
        body.get("error").and_then(Value::as_str),
        Some("unknown route"),
        "{path} is still unimplemented ({status} {body})"
    );
}

/// Every read-only probe the page uses to decide a panel is live.
#[test]
fn every_probe_answers_real_state() {
    let port = start(Some(new_project()));

    let (status, body) = get(port, "/api/shorts");
    assert_eq!(status, 200, "{body}");
    assert!(body["runs"].is_array(), "{body}");

    let (status, body) = get(port, "/api/captions/style");
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["mode"], "line");
    assert!(body["active"]["size"].is_number(), "{body}");
    let presets = body["presets"].as_array().expect("presets");
    assert!(presets.iter().any(|p| p["name"] == "Bold yellow"), "{body}");
    assert!(presets.iter().all(|p| p["builtin"] == true));

    let (status, body) = get(port, "/api/audio/enhance");
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["state"], "idle");
    assert!(body["stems"].is_array());

    let (status, body) = get(port, "/api/setup/pro-ai");
    assert_eq!(status, 200, "{body}");
    let components = body["components"].as_array().expect("components");
    assert_eq!(components.len(), 4);
    let ids: Vec<&str> = components
        .iter()
        .filter_map(|c| c["id"].as_str())
        .collect();
    assert_eq!(ids, vec!["whisperx", "faster-whisper", "realesrgan", "demucs"]);
    assert!(body["venv_dir"].is_string(), "{body}");
    for component in components {
        // A state the page colours with st-*; the detail says what was measured.
        assert!(
            ["queued", "running", "succeeded", "failed", "skipped"].contains(
                &component["state"].as_str().unwrap_or("")
            ),
            "unexpected state in {component}"
        );
        assert!(!component["detail"].as_str().unwrap_or("").is_empty());
    }

    let (status, body) = get(port, "/api/blender/preview");
    assert_eq!(status, 200, "{body}");
    assert!(body["blender"]["available"].is_boolean(), "{body}");
    assert!(body["gpus"].is_array());
    assert!(body["workers"].is_array());
    assert_eq!(body["workers"][0]["id"], "local");
}

/// A wrong path must still be a 404 that the page reads as "not implemented".
#[test]
fn an_unknown_route_is_still_a_404() {
    let port = start(Some(new_project()));
    let (status, body) = get(port, "/api/not-a-route");
    assert_eq!(status, 404);
    assert_not_missing(404, &body, "/api/not-a-route");
}

/// With no project open every action is a clean 404, never a panic.
#[test]
fn without_a_project_actions_answer_404() {
    let port = start(None);

    let (status, body) = get(port, "/api/shorts");
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["runs"].as_array().map(Vec::len), Some(0));

    let (status, body) = post(port, "/api/shorts", serde_json::json!({"source": "x"}));
    assert_eq!(status, 404, "{body}");
    assert!(body["error"].as_str().unwrap_or("").contains("no project"));

    let (status, body) = post(port, "/api/captions/style", serde_json::json!({"size": 50}));
    assert_eq!(status, 404, "{body}");

    let (status, body) = post(port, "/api/blender/scene", serde_json::json!({}));
    assert_eq!(status, 404, "{body}");

    let (status, body) = post(port, "/api/audio/enhance", serde_json::json!({"source": "x"}));
    assert_eq!(status, 404, "{body}");

    // The probes stay useful with no project: they report what exists, not an error.
    for path in ["/api/captions/style", "/api/audio/enhance", "/api/blender/preview", "/api/setup/pro-ai"] {
        let (status, body) = get(port, path);
        assert_eq!(status, 200, "{path}: {body}");
    }
}

/// A Free edition must refuse every Pro action with its own message, not run it quietly.
#[test]
fn free_edition_refuses_the_pro_actions() {
    let port = start(Some(new_project()));
    let cases = [
        ("/api/shorts", serde_json::json!({"source": "asset_x"})),
        ("/api/audio/enhance", serde_json::json!({"source": "asset_x"})),
        ("/api/blender/render", serde_json::json!({})),
        ("/api/blender/recover", serde_json::json!({"frames": [1]})),
        ("/api/captions/word_level", serde_json::json!({"clip_id": "clip_x"})),
        ("/api/captions/style", serde_json::json!({"mode": "karaoke"})),
        ("/api/captions/style/preset", serde_json::json!({"name": "Mine", "style": {}})),
    ];
    for (path, body) in cases {
        let (status, reply) = post(port, path, body);
        assert_eq!(status, 403, "{path} should be refused on Free: {reply}");
        assert_eq!(reply["code"], "permission_denied", "{path}: {reply}");
        assert!(
            reply["error"].as_str().unwrap_or("").contains("Pro feature"),
            "{path}: {reply}"
        );
        assert!(reply["hint"].is_string(), "{path} needs a way forward: {reply}");
    }
}

/// Bad input on an ungated route is a 400 with a message the page shows verbatim.
#[test]
fn invalid_input_is_a_400_before_any_work() {
    let port = start(Some(new_project()));
    let (status, body) = post(port, "/api/captions/style", serde_json::json!({"size": 50}));
    assert_eq!(status, 400, "{body}");
    assert!(
        body["error"]
            .as_str()
            .unwrap_or("")
            .contains("no caption track"),
        "{body}"
    );

    let (status, body) = post(port, "/api/blender/scene", serde_json::json!({"template": "nope"}));
    assert_eq!(status, 400, "{body}");
    assert!(body["error"].as_str().unwrap_or("").contains("unknown template"));

    let (status, body) = post(
        port,
        "/api/captions/style/preset",
        serde_json::json!({"name": ""}),
    );
    // Free refuses the feature first; on Pro the same request is a 400 for the empty name.
    assert!(status == 403 || status == 400, "{body}");
}

/// The caption style round trip: apply, persist, read back, and the preset store.
#[test]
fn caption_style_applies_and_reads_back() {
    let root = new_project();
    let store = ProjectStore::open(&root).unwrap();
    let mut project = store.load().unwrap();
    cinvar::project::ops::set_captions(
        &mut project,
        vec![Cue {
            start: Time::from_secs_f64(0.0),
            end: Time::from_secs_f64(2.0),
            text: "hello studio".into(),
        }],
        "en",
        None,
        "test",
    )
    .unwrap();
    store.save(&mut project).unwrap();
    let port = start(Some(root.clone()));

    let (status, body) = post(
        port,
        "/api/captions/style",
        serde_json::json!({"size": 64, "color": "#ff0000", "uppercase": true, "position": "top"}),
    );
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["ok"], true);

    let (status, body) = get(port, "/api/captions/style");
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["active"]["size"], 64);
    assert_eq!(body["active"]["color"], "#ff0000");
    assert_eq!(body["active"]["uppercase"], true);
    assert_eq!(body["active"]["position"], "top");
    assert_eq!(body["tracks"], 1);

    // The style is in the project, not just in the response.
    let project = ProjectStore::open(&root).unwrap().load().unwrap();
    assert_eq!(project.captions[0].style.size, 64);
    assert_eq!(
        serde_json::to_value(project.captions[0].style.position).unwrap(),
        serde_json::json!("top")
    );
}

/// Rendering state and the four views come from what the project recorded, not from a template.
#[test]
fn blender_preview_reports_the_state_file() {
    let root = new_project();
    let blender = root.join("blender");
    std::fs::create_dir_all(&blender).unwrap();
    std::fs::create_dir_all(root.join("renders").join("blender").join("preview")).unwrap();
    let view = root
        .join("renders")
        .join("blender")
        .join("preview")
        .join("view_camera.png");
    std::fs::write(&view, b"\x89PNG").unwrap();
    let state = serde_json::json!({
        "scene": {"id": "scene-1", "template": "logo_reveal", "text": "CINVAR", "blend": "/tmp/scene.blend",
                  "width": 1080, "height": 1920, "fps": 30, "frame_start": 1, "frame_end": 150,
                  "objects": 1, "cameras": 1, "lights": 3, "engine": "BLENDER_EEVEE"},
        "preview": {"views": [{"name": "front", "path": view.to_string_lossy()}],
                    "warnings": [{"severity": "warning", "message": "Camera clips the floor", "object": "Camera"}],
                    "passed": false, "elapsed_ms": 10, "blender_version": "4.0.2", "ran_at": 1},
        "render": {"job_id": "job_missing", "scene_id": "scene-1", "template": "logo_reveal", "text": "CINVAR",
                   "base": "/tmp", "job_dir": "/tmp/x", "engine": "eevee", "samples": 64,
                   "width": 1080, "height": 1920, "fps": 30, "frame_start": 1, "frame_end": 4,
                   "started_unix": 1, "failed_frames": [2, 3], "state": "failed", "error": "Blender produced 2 of 4 frames"}
    });
    std::fs::write(blender.join("state.json"), state.to_string()).unwrap();
    let port = start(Some(root));

    let (status, body) = get(port, "/api/blender/preview");
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["scene"]["id"], "scene-1");
    assert_eq!(body["scene"]["views"][0]["name"], "front");
    assert_eq!(
        body["scene"]["views"][0]["url"],
        "/api/media/blender/preview/view_camera.png"
    );
    assert_eq!(body["warnings"][0]["severity"], "warning");
    assert_eq!(body["render"]["failed_frames"], serde_json::json!([2, 3]));
    assert_eq!(body["render"]["total_frames"], 4);
    // The job record is gone (as after a restart): the recorded state still answers honestly.
    assert_eq!(body["render"]["state"], "failed");
    assert!(body["render"]["error"]
        .as_str()
        .unwrap_or("")
        .contains("2 of 4"));
}

/// Worker list and GPU ids are what the render form round-trips.
#[test]
fn blender_preview_offers_local_worker_and_gpus() {
    let port = start(Some(new_project()));
    let (status, body) = get(port, "/api/blender/preview");
    assert_eq!(status, 200, "{body}");
    let workers = body["workers"].as_array().unwrap();
    assert_eq!(workers[0]["id"], "local");
    assert!(workers[0]["label"].is_string());
    for gpu in body["gpus"].as_array().unwrap() {
        assert!(gpu["id"].is_number(), "{gpu}");
        assert!(gpu["name"].is_string(), "{gpu}");
    }
}

/// The media route is what makes a produced file playable, so it must serve what the state names.
#[test]
fn a_preview_view_is_served_by_the_media_route() {
    let root = new_project();
    let dir = root.join("renders").join("blender").join("preview");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("view_top.png"), b"\x89PNG\r\n\x1a\n").unwrap();
    let port = start(Some(root));
    let (status, _body) = get(port, "/api/media/blender/preview/view_top.png");
    // 200 with image bytes; the JSON client cannot parse it, so only the status is asserted.
    assert!(status == 200 || status == 206, "media route refused the file");
}
