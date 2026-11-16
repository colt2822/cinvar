//! The unix side of the persistent Blender worker: process supervision, the socket round trip,
//! crash recovery, job bookkeeping and the optional GUI attach gate. See worker.rs for the
//! normative protocol description.

use std::io::Write;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::sync::{Mutex, MutexGuard};
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use super::{
    encode_frame, read_frame, WorkerRequest, WorkerResponse, WorkerStatus, WorkerStop, OP_TIMEOUT,
    PING_TIMEOUT, RENDER_TIMEOUT, START_TIMEOUT,
};
use crate::error::{Error, ErrorCode, Result};
use crate::util::paths;

/// Every path the worker machinery touches, resolved from the environment once per call.
#[derive(Debug, Clone)]
pub struct Paths {
    pub dir: PathBuf,
    pub socket: PathBuf,
    pub gui_socket: PathBuf,
    pub server_py: PathBuf,
    pub sdk_py: PathBuf,
    pub gui_addon_py: PathBuf,
    pub log: PathBuf,
}

fn env_path(name: &str) -> Option<PathBuf> {
    std::env::var(name)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
}

pub fn paths() -> Paths {
    let cache = env_path("CINVAR_CACHE_DIR")
        .or_else(|| env_path("XDG_CACHE_HOME").map(|base| base.join("cinvar")))
        .unwrap_or_else(|| paths::home_dir().join(".cache").join("cinvar"));
    let dir = cache.join("blender-worker");
    let socket = env_path("CINVAR_WORKER_SOCKET").unwrap_or_else(|| cache.join("blender-worker.sock"));
    let gui_socket =
        env_path("CINVAR_GUI_SOCKET").unwrap_or_else(|| cache.join("blender-gui.sock"));
    Paths {
        server_py: dir.join("worker_server.py"),
        sdk_py: dir.join("cinvar_bpy_sdk.py"),
        gui_addon_py: dir.join("worker_gui_addon.py"),
        log: dir.join("worker.log"),
        dir,
        socket,
        gui_socket,
    }
}

// ---------------------------------------------------------------------------------------------
// supervision state
// ---------------------------------------------------------------------------------------------

struct State {
    child: Option<Child>,
    next_id: u64,
    job: String,
    last_pid: Option<u32>,
}

impl State {
    fn new() -> Self {
        let job = std::env::var("CINVAR_WORKER_JOB")
            .ok()
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| "default".to_string());
        State {
            child: None,
            next_id: 0,
            job,
            last_pid: None,
        }
    }
}

static STATE: Mutex<Option<State>> = Mutex::new(None);

fn lock() -> MutexGuard<'static, Option<State>> {
    STATE.lock().unwrap_or_else(|poison| poison.into_inner())
}

fn init<'a>(guard: &'a mut MutexGuard<'static, Option<State>>) -> &'a mut State {
    if guard.is_none() {
        **guard = Some(State::new());
    }
    guard.as_mut().expect("worker state was just initialised")
}

// ---------------------------------------------------------------------------------------------
// socket round trip
// ---------------------------------------------------------------------------------------------

/// Connects with a bounded wait, so an unresolvable socket can never hang the caller.
fn connect(socket: &Path, timeout: Duration) -> Result<UnixStream> {
    let target = socket.to_path_buf();
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(UnixStream::connect(&target));
    });
    match rx.recv_timeout(timeout) {
        Ok(Ok(stream)) => Ok(stream),
        Ok(Err(err)) => Err(Error::new(
            ErrorCode::Io,
            format!("worker socket {}: {err}", socket.display()),
        )),
        Err(_) => Err(Error::new(
            ErrorCode::Io,
            format!(
                "timed out after {}ms connecting to {}",
                timeout.as_millis(),
                socket.display()
            ),
        )),
    }
}

/// Sends one framed request and reads exactly one framed response.
pub fn exchange(
    socket: &Path,
    request: &Value,
    connect_timeout: Duration,
    io_timeout: Duration,
) -> Result<Value> {
    let mut stream = connect(socket, connect_timeout)?;
    stream
        .set_read_timeout(Some(io_timeout))
        .map_err(|err| Error::io(format!("worker read timeout: {err}")))?;
    stream
        .set_write_timeout(Some(io_timeout))
        .map_err(|err| Error::io(format!("worker write timeout: {err}")))?;
    let frame = encode_frame(request)?;
    stream
        .write_all(&frame)
        .map_err(|err| Error::io(format!("worker write: {err}")))?;
    stream
        .flush()
        .map_err(|err| Error::io(format!("worker flush: {err}")))?;
    let value = read_frame(&mut stream)?;
    Ok(value)
}

/// One bounded health probe. Never starts a worker and never waits longer than PING_TIMEOUT.
pub fn probe() -> Result<Value> {
    let p = paths();
    exchange(
        &p.socket,
        &json!({ "id": 0, "op": "ping" }),
        Duration::from_secs(2),
        PING_TIMEOUT,
    )
}

fn result_of(value: &Value) -> Option<&Value> {
    value.get("result")
}

fn pid_of(value: &Value) -> Option<u32> {
    value
        .get("result")
        .and_then(|r| r.get("pid"))
        .and_then(Value::as_u64)
        .map(|pid| pid as u32)
}

// ---------------------------------------------------------------------------------------------
// lifecycle
// ---------------------------------------------------------------------------------------------

fn log_tail(path: &Path, lines: usize) -> String {
    let text = std::fs::read_to_string(path).unwrap_or_default();
    let collected: Vec<&str> = text.lines().collect();
    let start = collected.len().saturating_sub(lines);
    collected[start..].join("\n")
}

fn write_if_changed(path: &Path, bytes: &[u8]) -> Result<()> {
    if std::fs::read(path).map(|existing| existing == bytes).unwrap_or(false) {
        return Ok(());
    }
    paths::write_atomic(path, bytes)
}

fn install_scripts(p: &Paths) -> Result<()> {
    std::fs::create_dir_all(&p.dir)?;
    write_if_changed(&p.server_py, super::server_script().as_bytes())?;
    write_if_changed(&p.sdk_py, crate::blender::preview::sdk_source().as_bytes())?;
    write_if_changed(&p.gui_addon_py, super::gui_addon_script().as_bytes())?;
    Ok(())
}

/// Spawns the Blender process that serves the socket. Its stdio goes to the worker log.
pub fn spawn(p: &Paths) -> Result<Child> {
    let info = crate::blender::detect()?;
    install_scripts(p)?;
    let log = std::fs::File::create(&p.log)?;
    let mut cmd = Command::new(&info.path);
    cmd.arg("-b")
        .arg("--factory-startup")
        .arg("--python")
        .arg(&p.server_py)
        .current_dir(&p.dir)
        .stdin(Stdio::null())
        .stdout(log.try_clone()?)
        .stderr(log)
        .env("CINVAR_WORKER_SOCKET", &p.socket);
    crate::util::proc::spawn_retrying(&mut cmd).map_err(|err| {
        Error::new(
            ErrorCode::DependencyMissing,
            format!("could not start {}: {err}", info.path.display()),
        )
        .with_hint("set CINVAR_BLENDER to a working blender executable")
    })
}

fn reap_socket(p: &Paths) {
    match std::fs::symlink_metadata(&p.socket) {
        Ok(meta) if meta.is_dir() => {}
        Ok(_) => {
            let _ = std::fs::remove_file(&p.socket);
        }
        Err(_) => {}
    }
}

fn wait_ready(p: &Paths, st: &mut State) -> Result<()> {
    let started = Instant::now();
    let mut last = String::from("no probe attempted");
    while started.elapsed() < START_TIMEOUT {
        if let Some(child) = st.child.as_mut() {
            match child.try_wait() {
                Ok(Some(status)) => {
                    return Err(Error::new(
                        ErrorCode::DependencyMissing,
                        format!("the Blender worker exited before it was ready (status {status:?})"),
                    )
                    .with_hint(format!("worker log tail:\n{}", log_tail(&p.log, 15))));
                }
                Ok(None) => {}
                Err(_) => {}
            }
        }
        match probe() {
            Ok(value) => {
                st.last_pid = pid_of(&value);
                return Ok(());
            }
            Err(err) => last = err.to_string(),
        }
        std::thread::sleep(Duration::from_millis(40));
    }
    Err(Error::new(
        ErrorCode::DependencyMissing,
        format!(
            "the Blender worker did not answer within {}s",
            START_TIMEOUT.as_secs()
        ),
    )
    .with_hint(format!(
        "last probe: {last}; worker log tail:\n{}",
        log_tail(&p.log, 15)
    )))
}

struct Ensured {
    recovered: bool,
}

/// Guarantees a live worker. force = reap and restart even if something answers.
fn ensure(st: &mut State, force: bool) -> Result<Ensured> {
    let p = paths();
    if !force {
        if let Ok(value) = probe() {
            st.last_pid = pid_of(&value);
            return Ok(Ensured { recovered: false });
        }
    }
    let existed = p.socket.exists();
    reap_socket(&p);
    if let Some(mut previous) = st.child.take() {
        let _ = previous.kill();
        let _ = previous.wait();
    }
    st.child = Some(spawn(&p)?);
    st.last_pid = None;
    wait_ready(&p, st)?;
    Ok(Ensured { recovered: existed })
}

pub fn status() -> Result<WorkerStatus> {
    let p = paths();
    let mut status = WorkerStatus {
        socket: p.socket.clone(),
        ..Default::default()
    };
    match probe() {
        Ok(value) => {
            status.running = value.get("ok").and_then(Value::as_bool).unwrap_or(false);
            if let Some(result) = result_of(&value) {
                status.pid = result.get("pid").and_then(Value::as_u64).map(|v| v as u32);
                status.protocol = result
                    .get("protocol")
                    .and_then(Value::as_u64)
                    .map(|v| v as u32);
                status.blender = result
                    .get("blender")
                    .and_then(Value::as_str)
                    .map(str::to_string);
                status.job = result.get("job").and_then(Value::as_str).map(str::to_string);
                status.requests = result.get("requests").and_then(Value::as_u64);
                status.uptime_s = result.get("uptime_s").and_then(Value::as_f64);
                status.objects = result
                    .get("scene")
                    .and_then(|s| s.get("object_count"))
                    .and_then(Value::as_u64)
                    .unwrap_or(0) as usize;
            }
        }
        Err(_) => {
            status.running = false;
            status.stale_socket = p.socket.exists();
        }
    }
    Ok(status)
}

pub fn ensure_running() -> Result<WorkerStatus> {
    {
        let mut guard = lock();
        let st = init(&mut guard);
        ensure(st, false)?;
    }
    status()
}

fn timeout_for(op: &str) -> Duration {
    match op {
        "ping" | "version" | "objects" | "attach" => PING_TIMEOUT,
        "render_frames" | "render_preview" => RENDER_TIMEOUT,
        "measure" => Duration::from_secs(1800),
        "shutdown" => Duration::from_secs(30),
        _ => OP_TIMEOUT,
    }
}

fn finish_shutdown(st: &mut State) {
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        let exited = match st.child.as_mut() {
            Some(child) => matches!(child.try_wait(), Ok(Some(_))),
            None => true,
        };
        if exited || Instant::now() >= deadline {
            break;
        }
        std::thread::sleep(Duration::from_millis(40));
    }
    if let Some(mut child) = st.child.take() {
        let _ = child.kill();
        let _ = child.wait();
    }
    reap_socket(&paths());
    st.last_pid = None;
}

pub fn submit(request: WorkerRequest) -> Result<WorkerResponse> {
    let op = request.op.clone();
    if op.trim().is_empty() {
        return Err(Error::invalid("worker request has no op"));
    }
    let mut guard = lock();
    let st = init(&mut guard);
    let job = request.job.clone().unwrap_or_else(|| st.job.clone());
    // Naming a job makes it the current one, so later requests without a job stay inside it
    // instead of silently bouncing back to the previous job (and resetting the scene).
    st.job = job.clone();
    st.next_id += 1;
    let id = st.next_id;
    let mut payload = request.to_value()?;
    payload["id"] = json!(id);
    payload["job"] = json!(job);
    let timeout = timeout_for(&op);

    let mut recovered = false;
    let value = match roundtrip(st, &payload, timeout) {
        Ok((value, restarted_first)) => {
            recovered = restarted_first;
            value
        }
        Err(first) => {
            // A missing Blender is not a crash: report it instead of restarting into the same error.
            if first.code == ErrorCode::DependencyMissing {
                return Err(first);
            }
            let restart = ensure(st, true)?;
            recovered = restart.recovered;
            let p = paths();
            match exchange(
                &p.socket,
                &payload,
                Duration::from_secs(3),
                timeout,
            ) {
                Ok(value) => value,
                Err(second) => {
                    return Err(Error::new(
                        ErrorCode::Provider,
                        format!("the worker failed twice for op {op}: first {first}; after restart {second}"),
                    )
                    .with_hint(format!(
                        "worker log tail:\n{}",
                        log_tail(&p.log, 15)
                    )));
                }
            }
        }
    };

    let mut response = WorkerResponse::parse(&value, &op)?;
    response.recovered = recovered;
    if op == "shutdown" && response.is_ok() {
        finish_shutdown(st);
    }
    Ok(response)
}

/// Sends one request, reporting whether a live worker had to be started/recovered first.
fn roundtrip(st: &mut State, payload: &Value, timeout: Duration) -> Result<(Value, bool)> {
    let ensured = ensure(st, false)?;
    let p = paths();
    let value = exchange(&p.socket, payload, Duration::from_secs(3), timeout)?;
    Ok((value, ensured.recovered))
}

pub fn stop() -> Result<WorkerStop> {
    let mut guard = lock();
    let st = init(&mut guard);
    let p = paths();
    let mut report = WorkerStop::default();
    if let Ok(value) = probe() {
        report.was_running = true;
        report.pid = pid_of(&value);
        st.last_pid = report.pid;
        let _ = exchange(
            &p.socket,
            &json!({ "id": 0, "op": "shutdown" }),
            Duration::from_secs(2),
            Duration::from_secs(30),
        );
    }
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        let answering = probe().is_ok();
        let exited = match st.child.as_mut() {
            Some(child) => matches!(child.try_wait(), Ok(Some(_))),
            None => true,
        };
        if !answering && exited {
            break;
        }
        if Instant::now() >= deadline {
            report.forced = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    if report.forced {
        if let Some(mut child) = st.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
        std::thread::sleep(Duration::from_millis(80));
    }
    reap_socket(&p);
    report.socket_removed = !p.socket.exists();
    st.last_pid = None;
    Ok(report)
}

// ---------------------------------------------------------------------------------------------
// optional GUI attach
// ---------------------------------------------------------------------------------------------

fn opt_in_enabled() -> bool {
    std::env::var("CINVAR_GUI_ATTACH")
        .map(|value| matches!(value.trim(), "1" | "true" | "yes" | "on"))
        .unwrap_or(false)
}

/// Pings the GUI-side bridge with the same framing the headless worker speaks.
pub fn gui_probe(socket: &Path) -> Result<Value> {
    exchange(
        socket,
        &json!({ "id": 0, "op": "ping" }),
        Duration::from_secs(2),
        Duration::from_secs(5),
    )
}

pub fn attach_gui(authorized: bool) -> Result<WorkerResponse> {
    let p = paths();
    if !authorized {
        return Err(Error::new(
            ErrorCode::PermissionDenied,
            "GUI attachment requires an explicit authorization from the caller",
        )
        .with_hint("pass authorized=true only when the user asked to attach an open Blender GUI"));
    }
    if !opt_in_enabled() {
        return Err(Error::new(
            ErrorCode::PermissionDenied,
            "GUI attachment is not enabled for this operator",
        )
        .with_hint(
            "set CINVAR_GUI_ATTACH=1 to allow attaching an open Blender GUI; headless work never needs it",
        ));
    }
    let bridge = gui_probe(&p.gui_socket).map_err(|err| {
        Error::new(
            ErrorCode::DependencyMissing,
            format!(
                "no authorized Blender GUI session is answering on {}: {err}",
                p.gui_socket.display()
            ),
        )
        .with_hint(
            "start Blender interactively, load worker_gui_addon.py (CINVAR GUI Bridge), then retry attach",
        )
    })?;
    if !bridge.get("ok").and_then(Value::as_bool).unwrap_or(false) {
        return Err(Error::new(
            ErrorCode::DependencyMissing,
            "the GUI bridge answered but rejected the ping",
        )
        .with_hint(format!(
            "GUI bridge said: {}",
            bridge.get("error").cloned().unwrap_or(Value::Null)
        )));
    }
    submit(WorkerRequest::attach_authorized(&p.gui_socket))
}
