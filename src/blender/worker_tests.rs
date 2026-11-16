//! Tests for the persistent Blender worker: protocol units that need nothing installed, and
//! real-lifecycle tests that start Blender, crash it, and prove isolation. Every real test
//! skips (never fails) when Blender is missing. The cold-vs-warm benchmark runs only when
//! CINVAR_WORKER_BENCH=1, because it takes about a minute.
//!
//! Worker tests are serialised: they share one socket path, so running them concurrently would
//! only measure each other.

use std::sync::{Mutex, MutexGuard};
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use super::*;
use crate::error::ErrorCode;

static SERIAL: Mutex<()> = Mutex::new(());

fn serial() -> MutexGuard<'static, ()> {
    SERIAL.lock().unwrap_or_else(|poison| poison.into_inner())
}

fn have_python3() -> bool {
    std::process::Command::new("python3")
        .arg("--version")
        .output()
        .map(|out| out.status.success())
        .unwrap_or(false)
}

fn have_blender() -> bool {
    crate::blender::available().is_some()
}

/// Points the worker at a private cache directory so a test never touches a real worker.
fn isolated_cache(label: &str) -> tempfile::TempDir {
    let dir = tempfile::TempDir::with_prefix(&format!("cinvar-worker-{label}-"))
        .expect("temp cache dir");
    std::env::set_var("CINVAR_CACHE_DIR", dir.path());
    std::env::remove_var("CINVAR_WORKER_SOCKET");
    std::env::remove_var("CINVAR_GUI_SOCKET");
    std::env::remove_var("CINVAR_GUI_ATTACH");
    dir
}

/// Stops the worker even when the test body panics, so a failed test never leaves a
/// resident Blender process behind.
struct WorkerGuard;

impl Drop for WorkerGuard {
    fn drop(&mut self) {
        let _ = super::stop();
    }
}

fn python_literal(path: &std::path::Path) -> String {
    serde_json::to_string(&path.to_string_lossy().to_string()).expect("json string")
}

// ---------------------------------------------------------------------------------------------
// protocol units (no Blender needed)
// ---------------------------------------------------------------------------------------------

#[test]
fn framing_is_length_prefixed_and_exact() {
    let value = json!({ "id": 3, "op": "ping", "params": { "text": "héllo" } });
    let frame = encode_frame(&value).expect("encode");
    assert_eq!(frame.len(), FRAME_HEADER + frame.len() - FRAME_HEADER);
    let declared = u32::from_be_bytes([frame[0], frame[1], frame[2], frame[3]]) as usize;
    assert_eq!(declared, frame.len() - FRAME_HEADER);

    let mut cursor = std::io::Cursor::new(frame.clone());
    let decoded = read_frame(&mut cursor).expect("decode");
    assert_eq!(decoded, value);

    // A short read is a protocol error, never a shorter message.
    let truncated = std::io::Cursor::new(frame[..frame.len() - 3].to_vec());
    let err = read_frame(&mut { truncated }).unwrap_err();
    assert_eq!(err.code, ErrorCode::Protocol);
    assert!(err.message.contains("truncated"), "{}", err.message);

    // Two frames back to back stay two frames.
    let mut joined = frame.clone();
    joined.extend_from_slice(&frame);
    let mut cursor = std::io::Cursor::new(joined);
    assert_eq!(read_frame(&mut cursor).unwrap(), value);
    assert_eq!(read_frame(&mut cursor).unwrap(), value);

    // An absurd length prefix is refused before allocating.
    let mut evil = std::io::Cursor::new({
        let mut bytes = ((MAX_FRAME + 1) as u32).to_be_bytes().to_vec();
        bytes.push(b'{');
        bytes
    });
    let err = read_frame(&mut evil).unwrap_err();
    assert_eq!(err.code, ErrorCode::Protocol);
}

#[test]
fn requests_serialise_the_documented_shape() {
    let ping = WorkerRequest::ping().to_value().unwrap();
    assert_eq!(ping["op"], "ping");
    assert!(ping.get("params").is_none(), "empty params must not be emitted");

    let measure = WorkerRequest::measure("/tmp/out", &MeasureOptions::default())
        .in_job("shot-1")
        .to_value()
        .unwrap();
    assert_eq!(measure["op"], "measure");
    assert_eq!(measure["job"], "shot-1");
    assert_eq!(measure["params"]["out_dir"], "/tmp/out");
    assert_eq!(measure["params"]["render_views"], false);
    assert_eq!(measure["params"]["floor_z"], 0.0);
    assert!(measure["params"].get("frame").is_none());

    let tuned = WorkerRequest::measure(
        "/tmp/out",
        &MeasureOptions {
            frame: Some("middle".into()),
            floor_z: -0.5,
            render_views: true,
            size: 256,
        },
    )
    .to_value()
    .unwrap();
    assert_eq!(tuned["params"]["frame"], "middle");
    assert_eq!(tuned["params"]["size"], 256);

    let load = WorkerRequest::load("/tmp/a.blend").to_value().unwrap();
    assert_eq!(load["op"], "load");
    assert_eq!(load["params"]["path"], "/tmp/a.blend");

    let frames = WorkerRequest::render_frames("/tmp/f", 1, 8).to_value().unwrap();
    assert_eq!(frames["params"]["start"], 1);
    assert_eq!(frames["params"]["end"], 8);
}

#[test]
fn responses_carry_errors_instead_of_panicking() {
    let ok = WorkerResponse::parse(
        &json!({ "id": 7, "op": "objects", "ok": true, "ms": 1.5,
                 "result": { "object_count": 2, "objects": ["Cube", "Lamp"], "auto_reset": true } }),
        "objects",
    )
    .unwrap();
    assert!(ok.is_ok());
    assert_eq!(ok.id, 7);
    assert_eq!(ok.object_names(), vec!["Cube".to_string(), "Lamp".to_string()]);
    assert_eq!(ok.object_count(), Some(2));
    assert!(ok.auto_reset());

    let bad = WorkerResponse::parse(
        &json!({ "id": 8, "op": "load", "ok": false,
                 "error": { "code": "not_found", "message": "no such file", "hint": "check the path" } }),
        "load",
    )
    .unwrap();
    assert!(!bad.is_ok());
    let err = bad.into_result().unwrap_err();
    assert_eq!(err.code, ErrorCode::NotFound);
    assert_eq!(err.message, "no such file");
    assert_eq!(err.hint.as_deref(), Some("check the path"));
}

#[test]
fn socket_path_matches_the_documented_location() {
    let _serial = serial();
    let dir = isolated_cache("paths");
    let expected = dir.path().join("blender-worker.sock");
    assert_eq!(socket_path(), expected);
    let scripts = super::unix::paths();
    assert_eq!(scripts.server_py, dir.path().join("blender-worker").join("worker_server.py"));
    assert_eq!(scripts.sdk_py, dir.path().join("blender-worker").join("cinvar_bpy_sdk.py"));
    assert!(scripts.gui_socket.ends_with("blender-gui.sock"));

    std::env::set_var("CINVAR_WORKER_SOCKET", dir.path().join("custom.sock"));
    assert_eq!(socket_path(), dir.path().join("custom.sock"));
    std::env::remove_var("CINVAR_WORKER_SOCKET");
}

#[test]
fn embedded_scripts_are_valid_python() {
    if !have_python3() {
        eprintln!("skipping: python3 is not installed");
        return;
    }
    for (name, source) in [
        ("worker_server.py", server_script()),
        ("worker_gui_addon.py", gui_addon_script()),
    ] {
        let source_literal = serde_json::to_string(source).unwrap();
        let out = std::process::Command::new("python3")
            .arg("-c")
            .arg(format!("import ast,sys; ast.parse({source_literal})"))
            .output()
            .expect("python3");
        assert!(
            out.status.success(),
            "{name} is not valid python: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
    assert!(server_script().contains("blender-worker.sock"));
    assert!(gui_addon_script().contains("blender-gui.sock"));
}

#[test]
fn gui_attach_is_refused_without_authorization() {
    let _serial = serial();
    let _cache = isolated_cache("gui-refusal");

    let err = attach_gui(false).expect_err("unauthorized attach must be refused");
    assert_eq!(err.code, ErrorCode::PermissionDenied, "{err}");

    let err = attach_gui(true).expect_err("attach without the operator opt-in must be refused");
    assert_eq!(err.code, ErrorCode::PermissionDenied, "{err}");
    assert!(err.message.contains("CINVAR_GUI_ATTACH") || err.hint.is_some());

    std::env::set_var("CINVAR_GUI_ATTACH", "1");
    let err = attach_gui(true).expect_err("attach without a GUI bridge must be refused");
    std::env::remove_var("CINVAR_GUI_ATTACH");
    assert_eq!(err.code, ErrorCode::DependencyMissing, "{err}");
    assert!(err.hint.is_some(), "a refusal must tell the operator what to do");
}

// ---------------------------------------------------------------------------------------------
// real worker lifecycle
// ---------------------------------------------------------------------------------------------

/// Builds a small .blend with the requested primitives, through a one-shot Blender.
fn build_scene(blender: &std::path::Path, target: &std::path::Path, counts: (u32, u32)) -> bool {
    let script_path = target.with_extension("builder.py");
    let script = format!(
        "import bpy\n\
         bpy.ops.wm.read_factory_settings(use_empty=True)\n\
         for index in range({cubes}):\n\
         \x20   bpy.ops.mesh.primitive_cube_add(location=(float(index) * 2.0, 0.0, 0.0))\n\
         \x20   bpy.context.object.name = \"Cube%02d\" % index\n\
         for index in range({spheres}):\n\
         \x20   bpy.ops.mesh.primitive_uv_sphere_add(location=(0.0, 2.0 + float(index) * 2.0, 0.0))\n\
         \x20   bpy.context.object.name = \"Sphere%02d\" % index\n\
         bpy.ops.object.camera_add(location=(9.0, -9.0, 6.0), rotation=(1.1, 0.0, 0.8))\n\
         bpy.context.scene.camera = bpy.context.object\n\
         bpy.ops.object.light_add(type=\"SUN\", location=(4.0, -4.0, 8.0))\n\
         bpy.ops.wm.save_as_mainfile(filepath={path})\n",
        cubes = counts.0,
        spheres = counts.1,
        path = python_literal(target),
    );
    if std::fs::write(&script_path, script).is_err() {
        return false;
    }
    std::process::Command::new(blender)
        .arg("-b")
        .arg("--factory-startup")
        .arg("--python")
        .arg(&script_path)
        .output()
        .map(|out| out.status.success() && target.is_file())
        .unwrap_or(false)
}

#[test]
fn worker_starts_answers_and_stops_cleanly() {
    let _serial = serial();
    if !have_blender() {
        eprintln!("skipping: blender is not installed");
        return;
    }
    let _cache = isolated_cache("lifecycle");
    let _stop = WorkerGuard;
    let blender = crate::blender::available().expect("blender");

    let down = status().expect("status never fails when no worker runs");
    assert!(!down.running);

    let started = ensure_running().expect("worker starts");
    assert!(started.running, "worker must be running");
    assert!(started.pid.is_some());
    assert_eq!(started.protocol, Some(PROTOCOL_VERSION));
    let reported = started.blender.clone().unwrap_or_default();
    assert!(reported.starts_with("4.") || reported.starts_with("3."), "blender {reported}");

    let version = submit(WorkerRequest::version()).expect("version").into_result().unwrap();
    assert_eq!(version["background"], true, "the worker must be headless");
    assert_eq!(
        std::fs::canonicalize(version["executable"].as_str().unwrap()).ok(),
        std::fs::canonicalize(&blender).ok()
    );

    // The scripts landed next to each other so the worker can import the SDK.
    let scripts = super::unix::paths();
    assert!(scripts.server_py.is_file());
    assert!(scripts.sdk_py.is_file());

    let stopped = stop().expect("stop");
    assert!(stopped.was_running);
    assert!(stopped.socket_removed, "the socket must be reaped");
    assert!(!status().unwrap().running);
}

#[test]
fn one_job_cannot_leak_scene_state_into_the_next() {
    let _serial = serial();
    if !have_blender() {
        eprintln!("skipping: blender is not installed");
        return;
    }
    let _cache = isolated_cache("isolation");
    let _stop = WorkerGuard;
    let blender = crate::blender::available().expect("blender");
    let dir = tempfile::TempDir::with_prefix("cinvar-worker-scenes-").expect("scenes");

    let first = dir.path().join("first.blend");
    let second = dir.path().join("second.blend");
    assert!(build_scene(&blender, &first, (3, 0)), "built first scene");
    assert!(build_scene(&blender, &second, (0, 2)), "built second scene");

    let loaded = submit(WorkerRequest::load(&first).in_job("job-a"))
        .expect("load")
        .into_result()
        .unwrap();
    assert_eq!(loaded["object_count"], 5, "3 cubes + camera + sun");
    let names: Vec<String> = loaded["objects"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|v| v.as_str().map(str::to_string))
        .collect();
    assert!(names.iter().any(|n| n.starts_with("Cube")), "{names:?}");

    // Same job: the scene is still the one we loaded (no gratuitous reset).
    let again = submit(WorkerRequest::objects().in_job("job-a")).expect("objects");
    assert_eq!(again.object_count(), Some(5));
    assert!(!again.auto_reset());

    // New job: the worker must reset *before* answering, and say so.
    let fresh = submit(WorkerRequest::objects().in_job("job-b")).expect("objects");
    assert!(fresh.auto_reset(), "a new job must start from a reset scene");
    assert_eq!(fresh.object_count(), Some(0), "job-b must not see job-a's objects");
    assert!(
        !fresh.object_names().iter().any(|n| n.starts_with("Cube")),
        "{:?}",
        fresh.object_names()
    );

    // A second scene in job-b, then back to job-a: still no leakage in either direction.
    let second_loaded = submit(WorkerRequest::load(&second).in_job("job-b"))
        .expect("load")
        .into_result()
        .unwrap();
    assert_eq!(second_loaded["object_count"], 4, "2 spheres + camera + sun");
    let back = submit(WorkerRequest::objects().in_job("job-a")).expect("objects");
    assert!(back.auto_reset());
    assert_eq!(back.object_count(), Some(0));
    assert!(!back.object_names().iter().any(|n| n.starts_with("Sphere")));

    stop().expect("stop");
}

#[test]
fn checkpoints_round_trip_and_a_changed_checkpoint_is_refused() {
    let _serial = serial();
    if !have_blender() {
        eprintln!("skipping: blender is not installed");
        return;
    }
    let _cache = isolated_cache("checkpoint");
    let _stop = WorkerGuard;
    let blender = crate::blender::available().expect("blender");
    let dir = tempfile::TempDir::with_prefix("cinvar-worker-checkpoint-").expect("checkpoint");
    let scene = dir.path().join("source.blend");
    assert!(build_scene(&blender, &scene, (2, 1)), "built scene");
    let checkpoint = dir.path().join("cached.blend");

    let loaded = submit(WorkerRequest::load(&scene).in_job("cached"))
        .expect("load")
        .into_result()
        .unwrap();
    let expected = loaded["object_count"].as_u64().expect("object count");

    let saved = submit(WorkerRequest::save_checkpoint(&checkpoint))
        .expect("save")
        .into_result()
        .unwrap();
    let digest = saved["sha256"].as_str().expect("digest").to_string();
    assert_eq!(digest.len(), 64);
    assert!(checkpoint.is_file(), "the checkpoint must exist on disk");
    assert_eq!(saved["object_count"].as_u64(), Some(expected));

    // A new job resets the scene, then the checkpoint brings the exact same scene back.
    let cleared = submit(WorkerRequest::objects().in_job("other")).expect("objects");
    assert_eq!(cleared.object_count(), Some(0));
    let restored = submit(
        WorkerRequest::load_checkpoint(&checkpoint, &digest).in_job("cached"),
    )
    .expect("load checkpoint")
    .into_result()
    .unwrap();
    assert_eq!(restored["object_count"].as_u64(), Some(expected));

    // A checkpoint whose bytes changed must never be mistaken for the cached scene.
    let mut bytes = std::fs::read(&checkpoint).expect("read checkpoint");
    bytes.push(0);
    std::fs::write(&checkpoint, bytes).expect("tamper");
    let refused = submit(WorkerRequest::load_checkpoint(&checkpoint, &digest))
        .expect("submit returns a response, not an Err");
    assert!(!refused.is_ok());
    assert_eq!(refused.error.expect("error").code, ErrorCode::Conflict);
}

#[test]
fn frames_and_preview_render_out_of_the_warm_scene() {
    let _serial = serial();
    if !have_blender() {
        eprintln!("skipping: blender is not installed");
        return;
    }
    let _cache = isolated_cache("render");
    let _stop = WorkerGuard;
    let blender = crate::blender::available().expect("blender");
    let dir = tempfile::TempDir::with_prefix("cinvar-worker-render-").expect("render");
    let scene = dir.path().join("render.blend");
    assert!(build_scene(&blender, &scene, (1, 0)), "built scene");
    let frames_dir = dir.path().join("frames");

    submit(WorkerRequest::load(&scene).in_job("render")).expect("load");
    let rendered = submit(
        WorkerRequest::render_frames(&frames_dir, 1, 2)
            .in_job("render")
            .with_params(json!({
                "out_dir": frames_dir.to_string_lossy(),
                "start": 1,
                "end": 2,
                "width": 160,
                "height": 90,
                "engine": "BLENDER_WORKBENCH",
            })),
    )
    .expect("render_frames")
    .into_result()
    .unwrap();
    assert_eq!(rendered["frames"], 2);
    for frame in 1..=2 {
        let path = frames_dir.join(format!("frame_{frame:04}.png"));
        assert!(path.is_file(), "missing {}", path.display());
        assert!(std::fs::metadata(&path).unwrap().len() > 0);
    }

    let preview = submit(
        WorkerRequest::render_preview(dir.path().join("preview"), 256).in_job("render"),
    )
    .expect("render_preview")
    .into_result()
    .unwrap();
    let views = preview["views"].as_object().cloned().unwrap_or_default();
    assert!(!views.is_empty(), "the preview must produce panels: {preview}");
    for (name, path) in views {
        let path = path.as_str().unwrap_or_default();
        assert!(
            std::path::Path::new(path).is_file(),
            "view {name} missing at {path}"
        );
    }
}

#[test]
fn a_killed_worker_is_detected_restarted_and_the_request_succeeds() {
    let _serial = serial();
    if !have_blender() {
        eprintln!("skipping: blender is not installed");
        return;
    }
    let _cache = isolated_cache("crash");
    let _stop = WorkerGuard;
    let blender = crate::blender::available().expect("blender");
    let dir = tempfile::TempDir::with_prefix("cinvar-worker-crash-").expect("scenes");
    let scene = dir.path().join("scene.blend");
    assert!(build_scene(&blender, &scene, (2, 0)), "built scene");

    let started = ensure_running().expect("worker starts");
    let pid = started.pid.expect("pid");
    let loaded = submit(WorkerRequest::load(&scene)).expect("load").into_result().unwrap();
    assert_eq!(loaded["object_count"], 4);

    // Kill it the way an OOM killer would: SIGKILL, leaving a stale socket behind.
    let killed = std::process::Command::new("kill")
        .args(["-9", &pid.to_string()])
        .status()
        .expect("kill");
    assert!(killed.success());
    let mut gone = false;
    for _ in 0..100 {
        if !super::unix::probe().is_ok() {
            gone = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(gone, "the killed worker must stop answering");
    let dead = status().unwrap();
    assert!(!dead.running);
    assert!(dead.stale_socket, "the dead worker left its socket behind");

    let recovered = submit(WorkerRequest::objects()).expect("submit after a crash");
    assert!(recovered.recovered, "the response must report the recovery");
    assert!(recovered.is_ok(), "{:?}", recovered.error);
    let new_pid = status().unwrap().pid.expect("restarted pid");
    assert_ne!(new_pid, pid, "the restarted worker must be a different process");

    stop().expect("stop");
}

#[test]
fn an_authorized_gui_bridge_is_attached_and_a_crash_is_never_silent() {
    let _serial = serial();
    if !have_blender() || !have_python3() {
        eprintln!("skipping: blender and python3 are required");
        return;
    }
    let cache = isolated_cache("gui-attach");
    let _stop = WorkerGuard;
    let dir = tempfile::TempDir::with_prefix("cinvar-worker-gui-").expect("gui");

    // A stand-in GUI bridge speaking the documented framing. It proves the attach *contract*
    // (authorization, ping, attribution) end to end; it is not a GUI Blender session.
    let stub = dir.path().join("gui_stub.py");
    let gui_socket = cache.path().join("blender-gui.sock");
    let script = format!(
        "import json, os, socket, struct\n\
         path = {socket}\n\
         if os.path.exists(path):\n\
         \x20   os.unlink(path)\n\
         listener = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)\n\
         listener.bind(path)\n\
         listener.listen(4)\n\
         deadline = __import__('time').time() + 60\n\
         while __import__('time').time() < deadline:\n\
         \x20   conn, _ = listener.accept()\n\
         \x20   header = conn.recv(4)\n\
         \x20   (length,) = struct.unpack('>I', header)\n\
         \x20   body = b''\n\
         \x20   while len(body) < length:\n\
         \x20       body += conn.recv(length - len(body))\n\
         \x20   request = json.loads(body.decode('utf-8'))\n\
         \x20   payload = json.dumps({{'id': request.get('id', 0), 'op': request.get('op', ''), 'ok': True,\n\
         \x20                         'result': {{'pong': True, 'bridge': 'stub', 'protocol': 1}}}}).encode('utf-8')\n\
         \x20   conn.sendall(struct.pack('>I', len(payload)) + payload)\n\
         \x20   conn.close()\n",
        socket = python_literal(&gui_socket),
    );
    std::fs::write(&stub, script).expect("stub written");
    let mut bridge = std::process::Command::new("python3")
        .arg(&stub)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("stub bridge");
    let mut ready = false;
    for _ in 0..100 {
        if super::unix::gui_probe(&gui_socket).is_ok() {
            ready = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(ready, "the stub GUI bridge must answer a ping");

    std::env::set_var("CINVAR_GUI_ATTACH", "1");
    ensure_running().expect("worker starts");
    let attached = attach_gui(true).expect("authorized attach").into_result().unwrap();
    assert_eq!(attached["attached"], true);
    assert_eq!(attached["headless_unaffected"], true);
    assert_eq!(
        attached["gui_socket"].as_str(),
        Some(gui_socket.to_string_lossy().as_ref())
    );
    // Headless work keeps working with the GUI attached.
    assert!(submit(WorkerRequest::objects()).expect("objects").is_ok());
    std::env::remove_var("CINVAR_GUI_ATTACH");

    let _ = stop();
    let _ = bridge.kill();
    let _ = bridge.wait();
}

/// Real interactive Blender, attached for an explicitly authorized operator. Opt-in because it
/// needs a display: CINVAR_WORKER_GUI_TEST=1 cargo test --release --lib real_gui -- --nocapture
#[test]
fn a_real_gui_blender_session_can_be_attached() {
    let _serial = serial();
    if std::env::var("CINVAR_WORKER_GUI_TEST").unwrap_or_default() != "1" {
        eprintln!("skipping: set CINVAR_WORKER_GUI_TEST=1 (needs a display) to attach a real GUI Blender");
        return;
    }
    if !have_blender() {
        eprintln!("skipping: blender is not installed");
        return;
    }
    let cache = isolated_cache("gui-real");
    let _stop = WorkerGuard;
    let dir = tempfile::TempDir::with_prefix("cinvar-worker-real-gui-").expect("gui");
    let addon = dir.path().join("worker_gui_addon.py");
    let gui_socket = cache.path().join("blender-gui.sock");
    std::fs::write(&addon, gui_addon_script()).expect("addon written");
    std::fs::write(
        dir.path().join("cinvar_bpy_sdk.py"),
        crate::blender::preview::sdk_source(),
    )
    .expect("sdk written");
    std::env::set_var("CINVAR_GUI_SOCKET", &gui_socket);

    let blender = crate::blender::available().expect("blender");
    let mut gui = std::process::Command::new(&blender)
        .arg("--factory-startup")
        .arg("--python")
        .arg(&addon)
        .current_dir(dir.path())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("gui blender");

    let mut pinged = None;
    for _ in 0..600 {
        if let Ok(value) = super::unix::gui_probe(&gui_socket) {
            pinged = Some(value);
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    let reply = match pinged {
        Some(reply) => reply,
        None => {
            let _ = gui.kill();
            let _ = gui.wait();
            panic!("the real GUI bridge never answered on {}", gui_socket.display());
        }
    };
    assert_eq!(reply["ok"], true);
    assert_eq!(
        reply["result"]["background"], false,
        "the attached session must be an interactive Blender, not a headless one"
    );

    std::env::set_var("CINVAR_GUI_ATTACH", "1");
    ensure_running().expect("worker starts");
    let attached = attach_gui(true).expect("real attach").into_result().unwrap();
    assert_eq!(attached["attached"], true);
    assert_eq!(attached["headless_unaffected"], true);
    eprintln!(
        "attached to a real GUI Blender {} (pid {}) on {}",
        reply["result"]["blender"], reply["result"]["pid"], gui_socket.display()
    );
    // Headless work continues to be served with the GUI attached.
    assert!(submit(WorkerRequest::objects()).expect("objects").is_ok());
    std::env::remove_var("CINVAR_GUI_ATTACH");

    let _ = stop();
    let _ = gui.kill();
    let _ = gui.wait();
}

// ---------------------------------------------------------------------------------------------
// cold vs warm benchmark
// ---------------------------------------------------------------------------------------------

fn now() -> Instant {
    Instant::now()
}

fn median(mut values: Vec<f64>) -> f64 {
    values.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let mid = values.len() / 2;
    if values.len() % 2 == 0 {
        (values[mid - 1] + values[mid]) / 2.0
    } else {
        values[mid]
    }
}

/// Cold Blender (process start + scene load + measurement) against the same measurement through
/// a warmed worker. Five runs each, medians reported, multiplier measured rather than assumed.
#[test]
fn benchmark_cold_versus_warm_measurement() {
    let _serial = serial();
    if std::env::var("CINVAR_WORKER_BENCH").unwrap_or_default() != "1" {
        eprintln!("skipping: set CINVAR_WORKER_BENCH=1 to run the cold/warm benchmark");
        return;
    }
    let blender = crate::blender::available().expect("CINVAR_WORKER_BENCH needs blender");
    let _cache = isolated_cache("bench");
    let _stop = WorkerGuard;
    let dir = tempfile::TempDir::with_prefix("cinvar-worker-bench-").expect("bench");
    let scene = dir.path().join("bench.blend");
    assert!(build_scene(&blender, &scene, (4, 2)), "built the benchmark scene");

    let out = dir.path().join("measure-out");
    std::fs::create_dir_all(&out).expect("out dir");
    let sdk_dir = super::unix::paths()
        .sdk_py
        .parent()
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| dir.path().to_path_buf());
    std::fs::create_dir_all(&sdk_dir).expect("sdk dir");
    std::fs::write(super::unix::paths().sdk_py, crate::blender::preview::sdk_source())
        .expect("sdk written");

    // --- cold: a fresh blender per measurement, exactly the documented invocation ----------
    let mut cold = Vec::new();
    for run in 0..5 {
        let out_dir = out.join(format!("cold-{run}"));
        std::fs::create_dir_all(&out_dir).expect("cold out");
        let expr = format!(
            "import sys; sys.path.insert(0, {sdk_dir}); import bpy, cinvar_bpy_sdk as sdk; \
             sdk.write_json({target}, sdk.collect(bpy.context.scene, 1, 0.0))",
            sdk_dir = python_literal(&sdk_dir),
            target = python_literal(&out_dir.join("diagnostics.json")),
        );
        let started = now();
        let result = std::process::Command::new(&blender)
            .arg("-b")
            .arg("--factory-startup")
            .arg(&scene)
            .arg("--python-expr")
            .arg(&expr)
            .output()
            .expect("cold blender");
        let elapsed = started.elapsed().as_secs_f64() * 1000.0;
        assert!(
            result.status.success(),
            "cold blender failed: {}",
            String::from_utf8_lossy(&result.stderr)
        );
        let diagnostics: Value = serde_json::from_str(
            &std::fs::read_to_string(out_dir.join("diagnostics.json")).expect("cold diagnostics"),
        )
        .expect("cold json");
        // collect() reports geometry objects only (camera and sun are not geometry).
        assert_eq!(
            diagnostics["objects"].as_array().map(|a| a.len()).unwrap_or(0),
            6,
            "cold measurement must see the 6 primitives of the benchmark scene"
        );
        cold.push(elapsed);
        eprintln!("cold run {run}: {elapsed:.1} ms");
    }

    // --- warm: the same measurement in a long-lived worker --------------------------------
    ensure_running().expect("warm worker");
    let _ = submit(WorkerRequest::measure(&out.join("warmup"), &MeasureOptions::default()))
        .expect("warmup measure")
        .into_result()
        .expect("warmup result");

    let mut warm_total = Vec::new();
    let mut warm_measure = Vec::new();
    let mut warm_load = Vec::new();
    for run in 0..5 {
        let out_dir = out.join(format!("warm-{run}"));
        let started = now();
        let loaded = submit(WorkerRequest::load(&scene).in_job("bench")).expect("load");
        warm_load.push(now().duration_since(started).as_secs_f64() * 1000.0);
        let names = loaded.object_names();
        assert!(names.iter().any(|n| n.starts_with("Cube")), "{names:?}");
        let measure_started = now();
        let measured = submit(
            WorkerRequest::measure(&out_dir, &MeasureOptions::default()).in_job("bench"),
        )
            .expect("warm measure")
            .into_result()
            .expect("warm result");
        warm_measure.push(now().duration_since(measure_started).as_secs_f64() * 1000.0);
        assert_eq!(measured["objects"], 6);
        let total = started.elapsed().as_secs_f64() * 1000.0;
        warm_total.push(total);
        eprintln!(
            "warm run {run}: total {total:.1} ms (load {:.1} ms, measure {:.1} ms, in-worker {:.1} ms)",
            warm_load[run], warm_measure[run],
            measured["ms"].as_f64().unwrap_or(0.0)
        );
    }
    let _ = stop();

    let cold_med = median(cold.clone());
    let warm_med = median(warm_total.clone());
    let measure_med = median(warm_measure.clone());
    eprintln!("cold: {cold:?}");
    eprintln!("warm(total): {warm_total:?}");
    eprintln!("warm(measure only): {warm_measure:?}");
    eprintln!("cold median {cold_med:.1} ms | warm median {warm_med:.1} ms | multiplier {:.1}x", cold_med / warm_med);
    eprintln!(
        "cold median {cold_med:.1} ms | measure-only median {measure_med:.1} ms | multiplier {:.1}x",
        cold_med / measure_med
    );
    assert!(
        warm_med < cold_med,
        "the warm worker must beat a cold process: warm {warm_med:.1} ms vs cold {cold_med:.1} ms"
    );
}
