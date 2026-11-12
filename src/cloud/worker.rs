//! The render worker that runs inside the pod (or on any user-owned machine: "custom remote
//! worker"). Small authenticated HTTP service; every request is short so it survives proxies
//! that cut connections (RunPod/Cloudflare: 100 s). Assets are content-addressed and uploads
//! resume from the byte offset the worker reports.

use std::collections::HashMap;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde::Serialize;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use tiny_http::{Header, Method, Request, Response, Server, StatusCode};

use super::manifest::{self, JobManifest};
use crate::error::{Error, ErrorCode, Result};
use crate::project::model::Project;
use crate::project::store::{sha256_file, ProjectStore};
use crate::render::{self, RenderRequest};
use crate::util::paths;

pub const MAX_MANIFEST_BYTES: usize = 64 * 1024 * 1024;
pub const MAX_CHUNK_BYTES: usize = 64 * 1024 * 1024;

#[derive(Debug, Clone)]
pub struct WorkerConfig {
    pub bind: String,
    pub root: PathBuf,
    pub token: String,
    /// The worker stops serving after this long, whatever else happens (cost safety net for a
    /// controller that vanished). `None` = unlimited (local custom workers).
    pub max_runtime: Option<Duration>,
}

#[derive(Debug, Clone, Serialize)]
struct JobView {
    state: &'static str,
    progress: f32,
    message: String,
    output_bytes: Option<u64>,
    output_sha256: Option<String>,
    error: Option<String>,
}

struct JobEntry {
    view: JobView,
    cancel: Arc<AtomicBool>,
    output: Option<PathBuf>,
}

pub struct WorkerHandle {
    pub addr: String,
    shutdown: Arc<AtomicBool>,
    threads: Vec<std::thread::JoinHandle<()>>,
}

impl WorkerHandle {
    pub fn url(&self) -> String {
        format!("http://{}", self.addr)
    }
    /// Blocks until the worker stops (runtime limit reached or shutdown requested).
    pub fn wait(self) {
        for t in self.threads {
            let _ = t.join();
        }
    }
    pub fn stop(self) {
        self.shutdown.store(true, Ordering::SeqCst);
        for t in self.threads {
            let _ = t.join();
        }
    }
}

struct State {
    cfg: WorkerConfig,
    jobs: Mutex<HashMap<String, JobEntry>>,
    uploads: Mutex<()>,
}

fn ct_eq(a: &str, b: &str) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.bytes()
        .zip(b.bytes())
        .fold(0u8, |acc, (x, y)| acc | (x ^ y))
        == 0
}

fn json_response(status: u16, value: Value) -> Response<std::io::Cursor<Vec<u8>>> {
    let mut r = Response::from_string(value.to_string()).with_status_code(StatusCode(status));
    r.add_header(Header::from_bytes("Content-Type", "application/json").unwrap());
    r
}

fn query(url: &str) -> (String, HashMap<String, String>) {
    let (path, q) = url.split_once('?').unwrap_or((url, ""));
    let map = q
        .split('&')
        .filter_map(|kv| kv.split_once('='))
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
    (path.to_string(), map)
}

fn is_hex64(s: &str) -> bool {
    s.len() == 64 && s.chars().all(|c| c.is_ascii_hexdigit())
}

fn job_id_ok(s: &str) -> bool {
    !s.is_empty() && s.len() <= 64 && s.chars().all(|c| c.is_ascii_alphanumeric())
}

/// Starts the worker on `cfg.bind` (use port 0 to pick one). Returns once it is listening.
pub fn start(cfg: WorkerConfig) -> Result<WorkerHandle> {
    if cfg.token.len() < 16 {
        return Err(Error::invalid(
            "worker token must be at least 16 characters",
        ));
    }
    paths::ensure_dir(&cfg.root)?;
    for sub in ["cas", "incoming", "jobs"] {
        paths::ensure_dir(&cfg.root.join(sub))?;
    }
    let server = Server::http(&cfg.bind).map_err(|e| {
        Error::new(
            ErrorCode::Io,
            format!("worker cannot bind {}: {e}", cfg.bind),
        )
    })?;
    let addr = server.server_addr().to_string();
    let server = Arc::new(server);
    let shutdown = Arc::new(AtomicBool::new(false));
    let state = Arc::new(State {
        cfg: cfg.clone(),
        jobs: Mutex::new(HashMap::new()),
        uploads: Mutex::new(()),
    });
    let started = Instant::now();
    let mut threads = Vec::new();
    for _ in 0..4 {
        let (server, state, shutdown) = (server.clone(), state.clone(), shutdown.clone());
        threads.push(std::thread::spawn(move || loop {
            if shutdown.load(Ordering::SeqCst) {
                break;
            }
            if let Some(limit) = state.cfg.max_runtime {
                if started.elapsed() > limit {
                    // Safety net: stop serving; the container's main process ends and the pod stops.
                    shutdown.store(true, Ordering::SeqCst);
                    break;
                }
            }
            match server.recv_timeout(Duration::from_millis(200)) {
                Ok(Some(req)) => handle(&state, req),
                Ok(None) => {}
                Err(_) => break,
            }
        }));
    }
    Ok(WorkerHandle {
        addr,
        shutdown,
        threads,
    })
}

fn handle(state: &Arc<State>, mut req: Request) {
    let authed = req
        .headers()
        .iter()
        .find(|h| h.field.equiv("Authorization"))
        .map(|h| h.value.as_str().to_string())
        .map(|v| {
            v.strip_prefix("Bearer ")
                .map(|t| ct_eq(t, &state.cfg.token))
                .unwrap_or(false)
        })
        .unwrap_or(false);
    let url = req.url().to_string();
    let (path, q) = query(&url);
    if path != "/health" && !authed {
        let _ = req.respond(json_response(401, json!({"error": "unauthorized"})));
        return;
    }
    let method = req.method().clone();
    let parts: Vec<&str> = path.trim_matches('/').split('/').collect();
    let result: std::result::Result<(u16, Value), (u16, String)> = match (&method, parts.as_slice())
    {
        (Method::Get, ["health"]) => Ok((
            200,
            json!({"ok": true, "service": "cinvar-worker", "version": env!("CARGO_PKG_VERSION")}),
        )),
        (Method::Get, ["assets", sha]) => asset_status(state, sha),
        (Method::Put, ["assets", sha]) => upload_chunk(state, sha, &q, &mut req),
        (Method::Post, ["jobs"]) => create_job(state, &mut req),
        (Method::Get, ["jobs", id]) => job_status(state, id),
        (Method::Post, ["jobs", id, "cancel"]) => job_cancel(state, id),
        (Method::Get, ["jobs", id, "output"]) => {
            download(state, id, &q, req);
            return;
        }
        _ => Err((404, "not found".into())),
    };
    let resp = match result {
        Ok((s, v)) => json_response(s, v),
        Err((s, m)) => json_response(s, json!({"error": m})),
    };
    let _ = req.respond(resp);
}

fn asset_status(state: &State, sha: &str) -> std::result::Result<(u16, Value), (u16, String)> {
    if !is_hex64(sha) {
        return Err((400, "bad sha256".into()));
    }
    let done = state.cfg.root.join("cas").join(sha);
    if done.is_file() {
        return Ok((
            200,
            json!({"have": true, "size": done.metadata().map(|m| m.len()).unwrap_or(0)}),
        ));
    }
    let part = state.cfg.root.join("incoming").join(format!("{sha}.part"));
    let size = part.metadata().map(|m| m.len()).unwrap_or(0);
    Ok((200, json!({"have": false, "size": size})))
}

fn upload_chunk(
    state: &State,
    sha: &str,
    q: &HashMap<String, String>,
    req: &mut Request,
) -> std::result::Result<(u16, Value), (u16, String)> {
    if !is_hex64(sha) {
        return Err((400, "bad sha256".into()));
    }
    let offset: u64 = q
        .get("offset")
        .and_then(|v| v.parse().ok())
        .ok_or((400, "offset required".to_string()))?;
    let total: u64 = q
        .get("total")
        .and_then(|v| v.parse().ok())
        .ok_or((400, "total required".to_string()))?;
    let len = req.body_length().unwrap_or(0);
    if len > MAX_CHUNK_BYTES {
        return Err((413, "chunk too large".into()));
    }
    let _g = state.uploads.lock().unwrap_or_else(|e| e.into_inner());
    let part = state.cfg.root.join("incoming").join(format!("{sha}.part"));
    let have = part.metadata().map(|m| m.len()).unwrap_or(0);
    if have != offset {
        return Ok((409, json!({"error": "offset mismatch", "size": have})));
    }
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&part)
        .map_err(|e| (500, e.to_string()))?;
    let mut buf = Vec::with_capacity(len);
    req.as_reader()
        .take(MAX_CHUNK_BYTES as u64 + 1)
        .read_to_end(&mut buf)
        .map_err(|e| (400, e.to_string()))?;
    if offset + buf.len() as u64 > total {
        return Err((400, "chunk exceeds declared total".into()));
    }
    file.write_all(&buf).map_err(|e| (500, e.to_string()))?;
    file.sync_all().ok();
    let size = offset + buf.len() as u64;
    if size == total {
        let actual = sha256_file(&part).map_err(|e| (500, e.message))?;
        if !actual.eq_ignore_ascii_case(sha) {
            let _ = std::fs::remove_file(&part);
            return Err((422, "checksum mismatch; upload discarded".into()));
        }
        std::fs::rename(
            &part,
            state.cfg.root.join("cas").join(sha.to_ascii_lowercase()),
        )
        .map_err(|e| (500, e.to_string()))?;
        return Ok((200, json!({"have": true, "size": size})));
    }
    Ok((200, json!({"have": false, "size": size})))
}

fn create_job(
    state: &Arc<State>,
    req: &mut Request,
) -> std::result::Result<(u16, Value), (u16, String)> {
    if req.body_length().unwrap_or(0) > MAX_MANIFEST_BYTES {
        return Err((413, "manifest too large".into()));
    }
    let mut body = Vec::new();
    req.as_reader()
        .take(MAX_MANIFEST_BYTES as u64 + 1)
        .read_to_end(&mut body)
        .map_err(|e| (400, e.to_string()))?;
    let manifest: JobManifest =
        serde_json::from_slice(&body).map_err(|e| (400, format!("bad manifest: {e}")))?;
    manifest::validate(&manifest).map_err(|e| (400, e.message))?;
    let missing: Vec<String> = manifest
        .assets
        .iter()
        .filter(|a| {
            !state
                .cfg
                .root
                .join("cas")
                .join(a.sha256.to_ascii_lowercase())
                .is_file()
        })
        .map(|a| a.sha256.clone())
        .collect();
    if !missing.is_empty() {
        return Ok((409, json!({"error": "assets missing", "missing": missing})));
    }
    let id: String = manifest.hash().chars().take(16).collect();
    {
        let mut jobs = state.jobs.lock().unwrap();
        if let Some(existing) = jobs.get(&id) {
            return Ok((
                200,
                json!({"job_id": id, "duplicate": true, "state": existing.view.state}),
            ));
        }
        jobs.insert(
            id.clone(),
            JobEntry {
                view: JobView {
                    state: "running",
                    progress: 0.0,
                    message: "preparing".into(),
                    output_bytes: None,
                    output_sha256: None,
                    error: None,
                },
                cancel: Arc::new(AtomicBool::new(false)),
                output: None,
            },
        );
    }
    let st = state.clone();
    let jid = id.clone();
    std::thread::spawn(move || run_job(st, jid, manifest));
    Ok((202, json!({"job_id": id, "duplicate": false})))
}

fn set_view<F: FnOnce(&mut JobEntry)>(state: &State, id: &str, f: F) {
    if let Some(e) = state.jobs.lock().unwrap().get_mut(id) {
        f(e);
    }
}

fn run_job(state: Arc<State>, id: String, manifest: JobManifest) {
    let outcome = (|| -> Result<(PathBuf, u64, String)> {
        let work = state.cfg.root.join("jobs").join(&id).join("project");
        if work.exists() {
            std::fs::remove_dir_all(&work)?;
        }
        let store = ProjectStore { root: work.clone() };
        for sub in ["assets", "cache", "renders", "revisions", "sessions", "tmp"] {
            paths::ensure_dir(&work.join(sub))?;
        }
        let mut project: Project = serde_json::from_value(manifest.project.clone())?;
        for a in &manifest.assets {
            let dest = paths::ensure_within(&work, std::path::Path::new(&a.path))?;
            if let Some(p) = dest.parent() {
                paths::ensure_dir(p)?;
            }
            let src = state
                .cfg
                .root
                .join("cas")
                .join(a.sha256.to_ascii_lowercase());
            if std::fs::hard_link(&src, &dest).is_err() {
                std::fs::copy(&src, &dest)?;
            }
        }
        store.save(&mut project)?;
        let output = store.renders_dir().join(&manifest.output_name);
        let mut request = RenderRequest::for_output(&output);
        let r = &manifest.render;
        request.size_preset = r.size_preset.clone();
        request.width = r.width;
        request.height = r.height;
        request.codec = r.codec.clone();
        request.crf = r.crf;
        request.bit_depth = r.bit_depth;
        request.format = r.format.clone();
        request.burn_captions = r.burn_captions;
        request.timeline_id = r.timeline_id.clone();
        let cancel = state
            .jobs
            .lock()
            .unwrap()
            .get(&id)
            .map(|e| e.cancel.clone())
            .unwrap();
        let progress = |p: f32, m: &str| {
            set_view(&state, &id, |e| {
                e.view.progress = p;
                e.view.message = m.to_string();
            })
        };
        let cancelled = || cancel.load(Ordering::SeqCst);
        render::render(&project, &store, &request, &progress, &cancelled)?;
        let bytes = std::fs::metadata(&output)?.len();
        let sha = sha256_file(&output)?;
        Ok((output, bytes, sha))
    })();
    set_view(&state, &id, |e| match outcome {
        Ok((path, bytes, sha)) => {
            e.view.state = "succeeded";
            e.view.progress = 1.0;
            e.view.message = "done".into();
            e.view.output_bytes = Some(bytes);
            e.view.output_sha256 = Some(sha);
            e.output = Some(path);
        }
        Err(err) => {
            e.view.state = if err.code == ErrorCode::Cancelled {
                "cancelled"
            } else {
                "failed"
            };
            e.view.message = err.message.clone();
            e.view.error = Some(err.message);
        }
    });
}

fn job_status(state: &State, id: &str) -> std::result::Result<(u16, Value), (u16, String)> {
    if !job_id_ok(id) {
        return Err((400, "bad job id".into()));
    }
    match state.jobs.lock().unwrap().get(id) {
        Some(e) => Ok((200, serde_json::to_value(&e.view).unwrap())),
        None => Err((404, "unknown job".into())),
    }
}

fn job_cancel(state: &State, id: &str) -> std::result::Result<(u16, Value), (u16, String)> {
    if !job_id_ok(id) {
        return Err((400, "bad job id".into()));
    }
    match state.jobs.lock().unwrap().get(id) {
        Some(e) => {
            e.cancel.store(true, Ordering::SeqCst);
            Ok((200, json!({"cancel_requested": true})))
        }
        None => Err((404, "unknown job".into())),
    }
}

fn download(state: &State, id: &str, q: &HashMap<String, String>, req: Request) {
    let path = if job_id_ok(id) {
        state
            .jobs
            .lock()
            .unwrap()
            .get(id)
            .and_then(|e| e.output.clone())
    } else {
        None
    };
    let Some(path) = path else {
        let _ = req.respond(json_response(404, json!({"error": "no output yet"})));
        return;
    };
    let offset: u64 = q.get("offset").and_then(|v| v.parse().ok()).unwrap_or(0);
    let len: u64 = q
        .get("len")
        .and_then(|v| v.parse().ok())
        .unwrap_or(super::CHUNK_BYTES)
        .min(MAX_CHUNK_BYTES as u64);
    let mut buf = Vec::new();
    let read = (|| -> std::io::Result<()> {
        let mut f = std::fs::File::open(&path)?;
        f.seek(SeekFrom::Start(offset))?;
        f.take(len).read_to_end(&mut buf)?;
        Ok(())
    })();
    if read.is_err() {
        let _ = req.respond(json_response(500, json!({"error": "read failed"})));
        return;
    }
    let mut r = Response::from_data(buf);
    r.add_header(Header::from_bytes("Content-Type", "application/octet-stream").unwrap());
    let _ = req.respond(r);
}

/// Convenience used by tests and the CLI: sha256 of a byte slice as hex.
pub fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}
