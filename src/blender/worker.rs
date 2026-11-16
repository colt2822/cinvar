//! Persistent warm Blender worker (Argon 5E): one headless Blender kept alive behind a unix
//! socket at "~/.cache/cinvar/blender-worker.sock", so iterative scene work pays startup once
//! instead of once per operation. Optional GUI attachment lives here too.
//!
//! # Shape of the thing
//!
//! A worker is a real Blender process ("blender -b --factory-startup --python worker_server.py",
//! see [server_script()]) opens a unix domain socket and answers JSON requests. Nothing is
//! re-implemented in Rust: the geometry, the renders and the measurements are Blender's own,
//! produced by the embedded bpy measurement SDK ([crate::blender::preview::sdk_source]) which is
//! copied next to the server script at spawn time.
//!
//! # Socket and framing (normative)
//!
//! * Socket path: "~/.cache/cinvar/blender-worker.sock"; override with CINVAR_WORKER_SOCKET.
//!   The cache root is CINVAR_CACHE_DIR, else XDG_CACHE_HOME/cinvar, else ~/.cache/cinvar.
//! * Optional GUI bridge socket: the same directory, "blender-gui.sock"
//!   (override with CINVAR_GUI_SOCKET).
//! * One connection carries exactly one request and one response, then closes.
//! * Every message on the wire is: u32 big-endian length, then exactly that many bytes of
//!   UTF-8 JSON. Frames are capped at [MAX_FRAME] bytes. The length prefix is what makes a
//!   partial read harmless: both ends read exactly "length" bytes or report a protocol error,
//!   so a truncated message can never be mistaken for a complete one.
//! * Request:  {"id": u64, "op": string, "params": object, "job": string?}
//! * Response: {"id": u64, "op": string, "ok": true, "result": object, "ms": f64}
//!             {"id": u64, "op": string, "ok": false, "error": {"code","message","hint"}, "ms": f64}
//!
//! # Operations
//!
//! ping, version, reset, load, save, objects, measure, render_frames, render_preview,
//! attach, shutdown. See [WorkerRequest] for the typed constructors.
//!
//! # Cached scene checkpoints and job isolation
//!
//! "save" writes the live scene out as a .blend checkpoint with a sha256; "load" can demand
//! that digest, so a cached checkpoint is never silently replaced. Every request carries a job
//! id; when the worker sees a job id it has not seen last, it factory-resets the scene and
//! verifies it is empty BEFORE running the operation, and says so in the response
//! ("auto_reset": true). Reset is therefore explicit and verified, and one job cannot leak
//! scene state into the next. Naming a job (CINVAR_WORKER_JOB, or WorkerRequest::in_job) makes
//! it the current job, so later requests without one stay in it rather than bouncing back.
//!
//! # Crash recovery
//!
//! [submit] probes the worker, reaps a dead socket, restarts Blender and retries the request
//! once; the response then carries "recovered": true. A health probe is always bounded
//! ([PING_TIMEOUT]) so a wedged worker reports an error instead of hanging the caller.
//!
//! # Optional GUI attachment
//!
//! Headless operation is complete on its own. [attach_gui] is an opt-in path for an explicitly
//! authorized interactive Blender session (the user watches construction, moves or locks the
//! camera, then rendering continues from the approved scene). It requires all three of: an
//! operator opt-in (CINVAR_GUI_ATTACH=1), an explicit authorized flag from the caller, and a
//! live GUI bridge answering on the GUI socket (worker_gui_addon.py, shipped in [gui_addon_script()]).
//! Missing any of them is a clear refusal, never a silent downgrade. The bridge is an ordinary
//! Blender addon: "blender --python worker_gui_addon.py" enables it ad hoc in a GUI session.
//! It was verified against an interactive Blender 4.0.2 (attach, ping, headless work continuing
//! while attached); that check is gated:
//! CINVAR_WORKER_GUI_TEST=1 cargo test --release --lib real_gui -- --nocapture
//!
//! # Concurrency
//!
//! One worker, one request at a time: submit is serialised by a process-wide mutex and the
//! server answers one connection per request, so a long render blocks the next submit instead
//! of corrupting it. Parallel jobs want their own socket (CINVAR_WORKER_SOCKET).
//!
//! The cold-vs-warm measurement lives in the env-gated test
//! benchmark_cold_versus_warm_measurement (CINVAR_WORKER_BENCH=1).
//!
//! # Public API for a tool layer
//!
//! [status], [ensure_running], [submit], [stop] (plus [attach_gui] and [socket_path]).

use std::io::Read;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::error::{Error, ErrorCode, Result};

#[cfg(unix)]
#[path = "worker_unix.rs"]
mod unix;

#[cfg(test)]
#[path = "worker_tests.rs"]
mod worker_tests;

/// Wire protocol generation. Bump when a request or response shape changes incompatibly.
pub const PROTOCOL_VERSION: u32 = 1;

/// Size of the length prefix of every frame on the socket.
pub const FRAME_HEADER: usize = 4;

/// Largest message either end will accept (8 MiB). Anything larger is a protocol error.
pub const MAX_FRAME: usize = 8 * 1024 * 1024;

/// Bounded wait for a health probe, so a wedged worker never hangs the caller.
pub const PING_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

/// How long a freshly spawned Blender may take to answer its first ping.
pub const START_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(90);

/// Default per-request timeout for an ordinary scene operation.
pub const OP_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(120);

/// Per-request timeout for operations that render pixels.
pub const RENDER_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(3600);

/// The in-Blender server, embedded so a packaged binary needs no data files.
pub fn server_script() -> &'static str {
    include_str!("worker_server.py")
}

/// The optional GUI bridge addon, embedded for the same reason.
pub fn gui_addon_script() -> &'static str {
    include_str!("worker_gui_addon.py")
}

// ---------------------------------------------------------------------------------------------
// framing
// ---------------------------------------------------------------------------------------------

/// Frames one JSON value: u32 big-endian length, then the UTF-8 JSON body.
pub fn encode_frame(value: &Value) -> Result<Vec<u8>> {
    let body = serde_json::to_vec(value)
        .map_err(|e| Error::new(ErrorCode::Protocol, format!("worker frame encode: {e}")))?;
    if body.is_empty() || body.len() > MAX_FRAME {
        return Err(Error::new(
            ErrorCode::Protocol,
            format!("worker frame of {} bytes is outside 1..={MAX_FRAME}", body.len()),
        ));
    }
    let mut frame = Vec::with_capacity(FRAME_HEADER + body.len());
    frame.extend_from_slice(&(body.len() as u32).to_be_bytes());
    frame.extend_from_slice(&body);
    Ok(frame)
}

/// Reads exactly one framed JSON value. A short read is an error, never a short message.
pub fn read_frame<R: Read>(reader: &mut R) -> Result<Value> {
    let mut header = [0u8; FRAME_HEADER];
    reader.read_exact(&mut header).map_err(|e| {
        Error::new(ErrorCode::Protocol, format!("worker frame header: {e}"))
    })?;
    let length = u32::from_be_bytes(header) as usize;
    if length == 0 || length > MAX_FRAME {
        return Err(Error::new(
            ErrorCode::Protocol,
            format!("worker announced {length} bytes, outside 1..={MAX_FRAME}"),
        ));
    }
    let mut body = vec![0u8; length];
    reader.read_exact(&mut body).map_err(|e| {
        Error::new(
            ErrorCode::Protocol,
            format!("worker frame body truncated at {length} bytes: {e}"),
        )
    })?;
    serde_json::from_slice(&body)
        .map_err(|e| Error::new(ErrorCode::Protocol, format!("worker frame json: {e}")))
}

/// Maps a wire error code back onto the crate's typed codes.
pub fn error_code(name: &str) -> ErrorCode {
    match name {
        "not_found" => ErrorCode::NotFound,
        "invalid_input" => ErrorCode::InvalidInput,
        "unsupported" => ErrorCode::Unsupported,
        "permission_denied" => ErrorCode::PermissionDenied,
        "dependency_missing" => ErrorCode::DependencyMissing,
        "io_error" => ErrorCode::Io,
        "provider_error" => ErrorCode::Provider,
        "protocol_error" => ErrorCode::Protocol,
        "render_failed" => ErrorCode::Render,
        "cancelled" => ErrorCode::Cancelled,
        "budget_exceeded" => ErrorCode::Budget,
        "not_implemented" => ErrorCode::NotImplemented,
        "conflict" => ErrorCode::Conflict,
        _ => ErrorCode::Internal,
    }
}

// ---------------------------------------------------------------------------------------------
// request / response
// ---------------------------------------------------------------------------------------------

/// One worker operation. Build with the named constructors, override with [Self::with_params].
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct WorkerRequest {
    pub op: String,
    #[serde(default, skip_serializing_if = "Value::is_null")]
    pub params: Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub job: Option<String>,
}

/// Tuning for a [WorkerRequest::measure].
#[derive(Debug, Clone)]
pub struct MeasureOptions {
    /// "start" | "middle" | "end" | an explicit frame number as a string; None keeps the SDK default.
    pub frame: Option<String>,
    /// Floor height the SDK judges contact against, in metres.
    pub floor_z: f64,
    /// Also render the four diagnostic panels (much slower than the measurements themselves).
    pub render_views: bool,
    /// Panel edge length in pixels when render_views is on.
    pub size: u32,
}

impl Default for MeasureOptions {
    fn default() -> Self {
        MeasureOptions {
            frame: None,
            floor_z: 0.0,
            render_views: false,
            size: 384,
        }
    }
}

fn path_str(path: &Path) -> String {
    path.to_string_lossy().to_string()
}

impl WorkerRequest {
    pub fn new(op: impl Into<String>) -> Self {
        WorkerRequest {
            op: op.into(),
            params: Value::Null,
            job: None,
        }
    }

    /// Replaces the parameter object wholesale.
    pub fn with_params(mut self, params: Value) -> Self {
        self.params = params;
        self
    }

    /// Runs this request inside a named job. Switching jobs forces a verified scene reset.
    pub fn in_job(mut self, job: impl Into<String>) -> Self {
        self.job = Some(job.into());
        self
    }

    pub fn ping() -> Self {
        Self::new("ping")
    }

    pub fn version() -> Self {
        Self::new("version")
    }

    /// Factory reset to an empty scene; the worker verifies the reset and fails if it was unclean.
    pub fn reset() -> Self {
        Self::new("reset")
    }

    pub fn objects() -> Self {
        Self::new("objects")
    }

    pub fn load(path: impl AsRef<Path>) -> Self {
        Self::new("load").with_params(json!({ "path": path_str(path.as_ref()) }))
    }

    /// Loads a cached checkpoint only if its bytes still hash to the recorded digest.
    pub fn load_checkpoint(path: impl AsRef<Path>, sha256: &str) -> Self {
        Self::new("load").with_params(json!({
            "path": path_str(path.as_ref()),
            "sha256": sha256,
        }))
    }

    /// Writes the live scene out as a reusable checkpoint and reports its digest.
    pub fn save_checkpoint(path: impl AsRef<Path>) -> Self {
        Self::new("save").with_params(json!({ "path": path_str(path.as_ref()) }))
    }

    /// Runs the measurement SDK against the live scene in the warm process.
    pub fn measure(out_dir: impl AsRef<Path>, opts: &MeasureOptions) -> Self {
        let mut params = json!({
            "out_dir": path_str(out_dir.as_ref()),
            "floor_z": opts.floor_z,
            "render_views": opts.render_views,
            "size": opts.size,
        });
        if let Some(frame) = &opts.frame {
            params["frame"] = json!(frame);
        }
        Self::new("measure").with_params(params)
    }

    /// Renders a PNG sequence out of the live scene. Tune engine/size with [Self::with_params].
    pub fn render_frames(out_dir: impl AsRef<Path>, start: u32, end: u32) -> Self {
        Self::new("render_frames").with_params(json!({
            "out_dir": path_str(out_dir.as_ref()),
            "start": start,
            "end": end,
        }))
    }

    /// Renders the four diagnostic panels out of the live scene.
    pub fn render_preview(out_dir: impl AsRef<Path>, size: u32) -> Self {
        Self::new("render_preview").with_params(json!({
            "out_dir": path_str(out_dir.as_ref()),
            "size": size,
        }))
    }

    /// Asks the worker to record an authorized GUI attachment (see [attach_gui]).
    pub fn attach_authorized(gui_socket: impl AsRef<Path>) -> Self {
        Self::new("attach").with_params(json!({
            "authorized": true,
            "gui_socket": path_str(gui_socket.as_ref()),
        }))
    }

    pub fn shutdown() -> Self {
        Self::new("shutdown")
    }

    pub fn to_value(&self) -> Result<Value> {
        serde_json::to_value(self)
            .map_err(|e| Error::new(ErrorCode::Internal, format!("worker request encode: {e}")))
    }
}

/// One answered operation. A failed operation still produces a response (with an error), so
/// the caller can report what the worker said instead of guessing.
#[derive(Debug, Clone)]
pub struct WorkerResponse {
    pub id: u64,
    pub op: String,
    pub result: Value,
    pub error: Option<Error>,
    pub ms: f64,
    /// True when this response came from a worker that had to be restarted first.
    pub recovered: bool,
}

impl WorkerResponse {
    pub fn is_ok(&self) -> bool {
        self.error.is_none()
    }

    /// The result object, or the worker's error.
    pub fn into_result(self) -> Result<Value> {
        match self.error {
            Some(err) => Err(err),
            None => Ok(self.result),
        }
    }

    pub fn result(&self) -> Result<&Value> {
        match &self.error {
            Some(err) => Err(err.clone()),
            None => Ok(&self.result),
        }
    }

    /// Object names the worker reports for the live scene, if this response carries any.
    pub fn object_names(&self) -> Vec<String> {
        self.result
            .get("objects")
            .and_then(Value::as_array)
            .map(|items| {
                items
                    .iter()
                    .filter_map(|item| item.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default()
    }

    pub fn object_count(&self) -> Option<u64> {
        self.result.get("object_count").and_then(Value::as_u64)
    }

    pub fn auto_reset(&self) -> bool {
        self.result
            .get("auto_reset")
            .and_then(Value::as_bool)
            .unwrap_or(false)
    }

    /// Parses one wire response object.
    pub fn parse(value: &Value, op: &str) -> Result<WorkerResponse> {
        let id = value.get("id").and_then(Value::as_u64).unwrap_or(0);
        let ms = value.get("ms").and_then(Value::as_f64).unwrap_or(0.0);
        let op = value
            .get("op")
            .and_then(Value::as_str)
            .filter(|name| !name.is_empty())
            .unwrap_or(op)
            .to_string();
        let ok = value.get("ok").and_then(Value::as_bool).unwrap_or(false);
        if ok {
            return Ok(WorkerResponse {
                id,
                op,
                result: value.get("result").cloned().unwrap_or(Value::Null),
                error: None,
                ms,
                recovered: false,
            });
        }
        let detail = value.get("error");
        let code = detail
            .and_then(|e| e.get("code"))
            .and_then(Value::as_str)
            .unwrap_or("internal_error");
        let message = detail
            .and_then(|e| e.get("message"))
            .and_then(Value::as_str)
            .unwrap_or("the worker reported a failure without a message");
        let mut err = Error::new(error_code(code), message.to_string());
        if let Some(hint) = detail.and_then(|e| e.get("hint")).and_then(Value::as_str) {
            err = err.with_hint(hint);
        }
        Ok(WorkerResponse {
            id,
            op,
            result: Value::Null,
            error: Some(err),
            ms,
            recovered: false,
        })
    }
}

/// What the worker looks like right now, without starting one.
#[derive(Debug, Clone, Default, Serialize)]
pub struct WorkerStatus {
    pub socket: PathBuf,
    pub running: bool,
    /// A socket file is present but nothing answers on it (a crashed worker's leftover).
    pub stale_socket: bool,
    pub pid: Option<u32>,
    pub protocol: Option<u32>,
    pub blender: Option<String>,
    pub job: Option<String>,
    pub objects: usize,
    pub requests: Option<u64>,
    pub uptime_s: Option<f64>,
}

/// The outcome of a clean stop.
#[derive(Debug, Clone, Default, Serialize)]
pub struct WorkerStop {
    pub was_running: bool,
    pub pid: Option<u32>,
    pub socket_removed: bool,
    pub forced: bool,
}

// ---------------------------------------------------------------------------------------------
// public API
// ---------------------------------------------------------------------------------------------

/// The socket the worker listens on (see the module docs for the precedence rules).
pub fn socket_path() -> PathBuf {
    #[cfg(unix)]
    {
        unix::paths().socket
    }
    #[cfg(not(unix))]
    {
        PathBuf::from("blender-worker.sock")
    }
}

/// Reports whether a worker is alive, without starting one. Bounded by [PING_TIMEOUT].
pub fn status() -> Result<WorkerStatus> {
    #[cfg(unix)]
    {
        unix::status()
    }
    #[cfg(not(unix))]
    {
        Err(unsupported())
    }
}

/// Starts a worker if none is running and returns the resulting status, or reports why not.
pub fn ensure_running() -> Result<WorkerStatus> {
    #[cfg(unix)]
    {
        unix::ensure_running()
    }
    #[cfg(not(unix))]
    {
        Err(unsupported())
    }
}

/// Runs one operation against the worker, starting it if needed and recovering it if it died.
pub fn submit(request: WorkerRequest) -> Result<WorkerResponse> {
    #[cfg(unix)]
    {
        unix::submit(request)
    }
    #[cfg(not(unix))]
    {
        let _ = request;
        Err(unsupported())
    }
}

/// Stops the worker cleanly and reaps its socket.
pub fn stop() -> Result<WorkerStop> {
    #[cfg(unix)]
    {
        unix::stop()
    }
    #[cfg(not(unix))]
    {
        Err(unsupported())
    }
}

/// Attaches to an explicitly authorized GUI Blender session, or refuses with a clear reason.
pub fn attach_gui(authorized: bool) -> Result<WorkerResponse> {
    #[cfg(unix)]
    {
        unix::attach_gui(authorized)
    }
    #[cfg(not(unix))]
    {
        let _ = authorized;
        Err(unsupported())
    }
}

#[cfg(not(unix))]
fn unsupported() -> Error {
    Error::new(
        ErrorCode::Unsupported,
        "the persistent Blender worker needs unix domain sockets; this build has no worker transport",
    )
}
