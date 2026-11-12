//! ComfyUI integration against a MOCK server speaking the documented HTTP API
//! (/system_stats, /object_info, /prompt, /history/{id}, /view, /interrupt). The "generated"
//! media is a real FFmpeg-made clip, so import, probing and timeline placement are real.
//! No live ComfyUI/model was used; see the final report.

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use cinvar::comfy::{self, Binding, ComfyClient, WorkflowDef};
use cinvar::project::store::ProjectStore;
use serde_json::{json, Value};
use tiny_http::{Header, Response, Server, StatusCode};

struct MockComfy {
    url: String,
    stop: Arc<AtomicBool>,
    interrupted: Arc<AtomicBool>,
    queued: Arc<AtomicUsize>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl MockComfy {
    fn start(clip: std::path::PathBuf, polls_until_done: usize, fail: bool) -> MockComfy {
        let server = Server::http("127.0.0.1:0").unwrap();
        let url = format!("http://{}", server.server_addr());
        let stop = Arc::new(AtomicBool::new(false));
        let interrupted = Arc::new(AtomicBool::new(false));
        let queued = Arc::new(AtomicUsize::new(0));
        let (sp, it, q) = (stop.clone(), interrupted.clone(), queued.clone());
        let polls = AtomicUsize::new(0);
        let thread = std::thread::spawn(move || {
            while !sp.load(Ordering::SeqCst) {
                let Ok(Some(mut req)) = server.recv_timeout(Duration::from_millis(50)) else {
                    continue;
                };
                let url = req.url().to_string();
                let json = |code: u16, v: Value| {
                    let mut r =
                        Response::from_string(v.to_string()).with_status_code(StatusCode(code));
                    r.add_header(Header::from_bytes("Content-Type", "application/json").unwrap());
                    r
                };
                let path = url.split('?').next().unwrap().to_string();
                let resp = match (req.method().as_str(), path.as_str()) {
                    ("GET", "/system_stats") => {
                        json(200, json!({"system": {"comfyui_version": "mock"}}))
                    }
                    ("GET", "/object_info") => json(
                        200,
                        json!({
                        "CheckpointLoaderSimple": {"input": {"required": {"ckpt_name": [["good.safetensors"]]}}},
                        "VideoUpscale": {"input": {"required": {"video": ["STRING"], "scale": ["INT"]}}},
                        "SaveVideo": {"input": {"required": {"video": ["VIDEO"]}}}}),
                    ),
                    ("POST", "/prompt") => {
                        let mut body = String::new();
                        std::io::Read::read_to_string(req.as_reader(), &mut body).unwrap();
                        let v: Value = serde_json::from_str(&body).unwrap();
                        if v["prompt"]
                            .as_object()
                            .map(|o| o.is_empty())
                            .unwrap_or(true)
                        {
                            json(400, json!({"error": "empty prompt"}))
                        } else {
                            q.fetch_add(1, Ordering::SeqCst);
                            if v["prompt"].get("hostile").is_some() {
                                json(200, json!({"prompt_id": "../../escaped", "number": 1}))
                            } else {
                                json(200, json!({"prompt_id": "pid-1", "number": 1}))
                            }
                        }
                    }
                    ("GET", "/history/pid-1") => {
                        let n = polls.fetch_add(1, Ordering::SeqCst);
                        if it.load(Ordering::SeqCst) || n < polls_until_done {
                            json(200, json!({}))
                        } else if fail {
                            json(
                                200,
                                json!({"pid-1": {"status": {"status_str": "error", "completed": false, "messages": [["execution_error", {"exception_message": "CUDA out of memory"}]]}, "outputs": {}}}),
                            )
                        } else {
                            json(
                                200,
                                json!({"pid-1": {"status": {"status_str": "success", "completed": true}, "outputs": {"9": {"videos": [{"filename": "out.mp4", "subfolder": "cinvar", "type": "output"}]}}}}),
                            )
                        }
                    }
                    ("GET", "/view") => {
                        assert!(
                            url.contains("filename=out.mp4") && url.contains("subfolder=cinvar")
                        );
                        let data = std::fs::read(&clip).unwrap();
                        let mut r = Response::from_data(data);
                        r.add_header(Header::from_bytes("Content-Type", "video/mp4").unwrap());
                        r
                    }
                    ("POST", "/interrupt") => {
                        it.store(true, Ordering::SeqCst);
                        json(200, json!({}))
                    }
                    _ => json(404, json!({})),
                };
                let _ = req.respond(resp);
            }
        });
        MockComfy {
            url,
            stop,
            interrupted,
            queued,
            thread: Some(thread),
        }
    }
}

impl Drop for MockComfy {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

fn clip(dir: &std::path::Path) -> std::path::PathBuf {
    let p = dir.join("generated.mp4");
    assert!(std::process::Command::new("ffmpeg")
        .args([
            "-y",
            "-v",
            "error",
            "-f",
            "lavfi",
            "-i",
            "testsrc2=size=320x180:rate=24:duration=1",
            "-c:v",
            "libx264",
            "-pix_fmt",
            "yuv420p"
        ])
        .arg(&p)
        .status()
        .unwrap()
        .success());
    p
}

fn def() -> WorkflowDef {
    WorkflowDef {
        name: "upscale-x2".into(),
        task: "upscale".into(),
        description: "mock".into(),
        workflow: json!({"3": {"class_type": "VideoUpscale", "inputs": {"video": "x", "scale": 2}}, "9": {"class_type": "SaveVideo", "inputs": {"video": ["3", 0]}}}),
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
    }
}

#[test]
fn workflow_runs_downloads_and_the_result_lands_on_the_timeline() {
    let dir = tempfile::tempdir().unwrap();
    let mock = MockComfy::start(clip(dir.path()), 2, false);
    let client = ComfyClient::new(&mock.url);
    assert!(client.system_stats().is_ok());
    let wf = comfy::bind(&def(), json!({"scale": 4}).as_object().unwrap()).unwrap();
    let out = dir.path().join("out");
    let r = comfy::run(
        &client,
        &wf,
        &out,
        Duration::from_secs(10),
        Duration::from_millis(20),
        &|| false,
        &|_, _| {},
    )
    .unwrap();
    assert_eq!(r.files.len(), 1);
    // Import as a real asset, probe it, place it on a real timeline.
    let (store, mut project) = ProjectStore::create(dir.path(), "ai").unwrap();
    let asset = store
        .import_asset(&mut project, &r.files[0], "comfyui")
        .unwrap();
    assert!(asset.info.as_ref().unwrap().has_video);
    let tl = cinvar::project::ops::create_timeline(
        &mut project,
        "Main",
        320,
        180,
        cinvar::util::time::Rational::new(24, 1),
    )
    .unwrap();
    let clip_id = cinvar::project::ops::insert_clip(
        &mut project,
        Some(&tl),
        None,
        cinvar::project::ops::ClipRequest {
            asset_id: &asset.id,
            source_in: None,
            source_out: None,
            at: None,
            name: Some("AI"),
        },
    )
    .unwrap();
    assert!(project
        .timeline_by_id(&tl)
        .unwrap()
        .tracks
        .iter()
        .any(|t| t.clips.iter().any(|c| c.id == clip_id)));
}

#[test]
fn missing_model_or_node_fails_before_anything_is_queued() {
    let dir = tempfile::tempdir().unwrap();
    let mock = MockComfy::start(clip(dir.path()), 0, false);
    let client = ComfyClient::new(&mock.url);
    let bad_model = json!({"1": {"class_type": "CheckpointLoaderSimple", "inputs": {"ckpt_name": "absent.safetensors"}}});
    let e = comfy::run(
        &client,
        &bad_model,
        dir.path(),
        Duration::from_secs(5),
        Duration::from_millis(10),
        &|| false,
        &|_, _| {},
    )
    .unwrap_err();
    assert!(e.message.contains("absent.safetensors"), "{e}");
    let bad_node = json!({"1": {"class_type": "NoSuchNode", "inputs": {}}});
    let e = comfy::run(
        &client,
        &bad_node,
        dir.path(),
        Duration::from_secs(5),
        Duration::from_millis(10),
        &|| false,
        &|_, _| {},
    )
    .unwrap_err();
    assert!(e.message.contains("not installed"));
    assert_eq!(
        mock.queued.load(Ordering::SeqCst),
        0,
        "nothing may be queued for an invalid workflow"
    );
}

#[test]
fn server_side_failure_cancellation_and_timeout_are_distinct_errors() {
    let dir = tempfile::tempdir().unwrap();
    let wf = comfy::bind(&def(), &serde_json::Map::new()).unwrap();

    let failing = MockComfy::start(clip(dir.path()), 0, true);
    let e = comfy::run(
        &ComfyClient::new(&failing.url),
        &wf,
        dir.path(),
        Duration::from_secs(5),
        Duration::from_millis(10),
        &|| false,
        &|_, _| {},
    )
    .unwrap_err();
    assert_eq!(e.code, cinvar::error::ErrorCode::Render);
    assert!(e.message.contains("CUDA out of memory"));

    let slow = MockComfy::start(clip(dir.path()), 10_000, false);
    let polls = AtomicUsize::new(0);
    let e = comfy::run(
        &ComfyClient::new(&slow.url),
        &wf,
        dir.path(),
        Duration::from_secs(5),
        Duration::from_millis(10),
        &|| polls.fetch_add(1, Ordering::SeqCst) > 3,
        &|_, _| {},
    )
    .unwrap_err();
    assert_eq!(e.code, cinvar::error::ErrorCode::Cancelled);
    assert!(
        slow.interrupted.load(Ordering::SeqCst),
        "cancel must interrupt the server"
    );

    let stuck = MockComfy::start(clip(dir.path()), 10_000, false);
    let e = comfy::run(
        &ComfyClient::new(&stuck.url),
        &wf,
        dir.path(),
        Duration::from_millis(150),
        Duration::from_millis(10),
        &|| false,
        &|_, _| {},
    )
    .unwrap_err();
    assert!(e.message.contains("exceeded"), "{e}");
    assert!(stuck.interrupted.load(Ordering::SeqCst));
}

#[test]
fn unreachable_server_is_a_clear_provider_error_and_registry_loads_only_valid_descriptors() {
    let e = ComfyClient::new("http://127.0.0.1:1")
        .system_stats()
        .unwrap_err();
    assert_eq!(e.code, cinvar::error::ErrorCode::Provider);
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("a.json"),
        serde_json::to_string(&def()).unwrap(),
    )
    .unwrap();
    let mut bad_task = def();
    bad_task.task = "make_coffee".into();
    bad_task.name = "coffee".into();
    std::fs::write(
        dir.path().join("b.json"),
        serde_json::to_string(&bad_task).unwrap(),
    )
    .unwrap();
    std::fs::write(dir.path().join("c.json"), "not json").unwrap();
    let loaded = comfy::load_registry(dir.path());
    assert_eq!(loaded.len(), 1);
    assert_eq!(loaded[0].name, "upscale-x2");
}

#[test]
fn a_hostile_prompt_id_cannot_choose_a_local_path() {
    let dir = tempfile::tempdir().unwrap();
    let mock = MockComfy::start(clip(dir.path()), 0, false);
    let client = ComfyClient::new(&mock.url);
    let e = client
        .queue(&json!({"hostile": {"class_type": "x", "inputs": {}}}), "c")
        .unwrap_err();
    assert!(e.message.contains("unsafe prompt_id"), "{e}");
}
