//! Hosted text-to-video bridges: fal.ai (queue API, many models), Gemini Omni (Google), Runway, Luma.
//!
//! Each backend is submit -> poll -> download over HTTPS with the user's own API key. Generation costs
//! real money and prices change, so spending is bounded in SECONDS OF VIDEO (price-independent): an
//! opt-in switch, a per-job cap and a per-day cap, tracked in a local ledger. Nothing here runs unless
//! the caller has already passed [`check_budget`].
//!
//! Endpoint shapes follow each provider's published API docs (checked November 2026). Providers change
//! them; every base URL can be overridden with `CINVAR_VIDEOGEN_BASE_<ID>` and the HTTP layer is covered
//! by tests against a local mock server.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use base64::Engine;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::config::VideoGenSettings;
use crate::error::{Error, ErrorCode, Result};
use crate::util::paths;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Backend {
    Fal,
    Gemini,
    Runway,
    Luma,
}

pub const BACKENDS: [Backend; 4] = [Backend::Gemini, Backend::Runway, Backend::Luma, Backend::Fal];

impl Backend {
    /// Credential-store / config id. `gemini` shares the key with the Gemini text connection.
    pub fn id(self) -> &'static str {
        match self {
            Backend::Fal => "fal",
            Backend::Gemini => "gemini",
            Backend::Runway => "runway",
            Backend::Luma => "luma",
        }
    }
    pub fn parse(id: &str) -> Option<Backend> {
        BACKENDS.iter().copied().find(|b| b.id() == id.trim().to_ascii_lowercase())
    }
    pub fn label(self) -> &'static str {
        match self {
            Backend::Fal => "fal.ai (many models)",
            Backend::Gemini => "Google Gemini Omni",
            Backend::Runway => "Runway",
            Backend::Luma => "Luma",
        }
    }
    pub fn env_key(self) -> &'static str {
        match self {
            Backend::Fal => "FAL_KEY",
            Backend::Gemini => "GEMINI_API_KEY",
            Backend::Runway => "RUNWAYML_API_SECRET",
            Backend::Luma => "LUMA_API_KEY",
        }
    }
    /// Suggested model ids (the first is the default). fal accepts any model id; see `body_for_fal`.
    pub fn models(self) -> &'static [&'static str] {
        match self {
            Backend::Fal => &["fal-ai/wan/v2.2-a14b/text-to-video"],
            Backend::Gemini => &["gemini-omni-1.2-flash"],
            Backend::Runway => &["gen4.5", "gen4_turbo", "seedance2"],
            Backend::Luma => &["ray-2", "ray-flash-2"],
        }
    }
    /// Longest clip the backend accepts in one request, in seconds.
    pub fn max_seconds(self) -> u32 {
        match self {
            Backend::Fal => 10,
            Backend::Gemini => 15,
            Backend::Runway => 10,
            Backend::Luma => 9,
        }
    }
    fn default_base(self) -> &'static str {
        match self {
            Backend::Fal => "https://queue.fal.run",
            Backend::Gemini => "https://generativelanguage.googleapis.com",
            Backend::Runway => "https://api.dev.runwayml.com",
            Backend::Luma => "https://api.lumalabs.ai/dream-machine/v1",
        }
    }
}

/// The API key for a backend: the local credential store first, then the provider's usual env var.
pub fn api_key(backend: Backend) -> Option<String> {
    crate::secrets::CredentialStore::load()
        .ok()
        .and_then(|s| s.get_key(backend.id()))
        .or_else(|| std::env::var(backend.env_key()).ok())
        .map(|k| k.trim().to_string())
        .filter(|k| !k.is_empty())
}

#[derive(Debug, Clone)]
pub struct GenRequest {
    pub prompt: String,
    pub seconds: u32,
    /// "16:9" or "9:16"
    pub aspect: String,
    pub model: String,
    /// Extra provider fields merged into the request body (fal only; advanced).
    pub extra: serde_json::Map<String, Value>,
}

pub struct Endpoint {
    pub backend: Backend,
    pub base: String,
    pub key: String,
}

impl Endpoint {
    pub fn new(backend: Backend, key: String) -> Endpoint {
        let base = std::env::var(format!("CINVAR_VIDEOGEN_BASE_{}", backend.id().to_ascii_uppercase()))
            .ok()
            .filter(|b| !b.trim().is_empty())
            .unwrap_or_else(|| backend.default_base().to_string());
        Endpoint { backend, base: base.trim_end_matches('/').to_string(), key }
    }
}

fn http_err(what: &str, e: ureq::Error) -> Error {
    let msg = match e {
        ureq::Error::Status(code, resp) => {
            let body = resp.into_string().unwrap_or_default();
            format!("{what}: HTTP {code}: {}", body.chars().take(300).collect::<String>())
        }
        other => format!("{what}: {other}"),
    };
    Error::new(ErrorCode::Provider, crate::secrets::redact(&msg))
}

fn agent(timeout: Duration) -> ureq::Agent {
    ureq::AgentBuilder::new().timeout_connect(Duration::from_secs(10)).timeout(timeout).build()
}

fn json_of(r: ureq::Response, what: &str) -> Result<Value> {
    r.into_json::<Value>().map_err(|e| Error::new(ErrorCode::Provider, format!("{what}: unreadable response: {e}")))
}

/// Only https (or loopback http for local mocks/proxies) may be fetched.
fn check_url(url: &str) -> Result<()> {
    if url.starts_with("https://") || url.starts_with("http://127.0.0.1") || url.starts_with("http://localhost") {
        Ok(())
    } else {
        Err(Error::new(ErrorCode::Provider, "the provider returned a non-https download link; refusing to fetch it"))
    }
}

fn download(url: &str, dest: &Path, headers: &[(&str, &str)]) -> Result<()> {
    check_url(url)?;
    let mut req = agent(Duration::from_secs(600)).get(url);
    for (k, v) in headers {
        req = req.set(k, v);
    }
    let resp = req.call().map_err(|e| http_err("download", e))?;
    let mut reader = resp.into_reader().take(2 * 1024 * 1024 * 1024);
    let tmp = dest.with_extension("part");
    let mut file = std::fs::File::create(&tmp).map_err(|e| Error::new(ErrorCode::Io, format!("cannot create {}: {e}", tmp.display())))?;
    let n = std::io::copy(&mut reader, &mut file).map_err(|e| Error::new(ErrorCode::Io, format!("download interrupted: {e}")))?;
    file.flush().ok();
    if n == 0 {
        let _ = std::fs::remove_file(&tmp);
        return Err(Error::new(ErrorCode::Provider, "the provider returned an empty video"));
    }
    std::fs::rename(&tmp, dest).map_err(|e| Error::new(ErrorCode::Io, e.to_string()))
}

pub struct Control<'a> {
    pub cancelled: &'a dyn Fn() -> bool,
    pub progress: &'a dyn Fn(f32, &str),
    pub poll_every: Duration,
    pub timeout: Duration,
}

fn wait_loop<F: FnMut() -> Result<Option<T>>, T>(ctl: &Control, what: &str, mut step: F) -> Result<T> {
    let start = Instant::now();
    loop {
        if (ctl.cancelled)() {
            return Err(Error::new(ErrorCode::Cancelled, "cancelled"));
        }
        if let Some(done) = step()? {
            return Ok(done);
        }
        if start.elapsed() > ctl.timeout {
            return Err(Error::new(ErrorCode::Provider, format!("{what} did not finish within {} seconds", ctl.timeout.as_secs())));
        }
        let frac = (start.elapsed().as_secs_f32() / ctl.timeout.as_secs_f32()).min(0.9);
        (ctl.progress)(0.05 + frac * 0.85, what);
        std::thread::sleep(ctl.poll_every);
    }
}

/// Generate one clip and write it into `dest_dir`. Returns the saved file paths.
pub fn generate(ep: &Endpoint, req: &GenRequest, dest_dir: &Path, ctl: &Control) -> Result<Vec<PathBuf>> {
    paths::ensure_dir(dest_dir)?;
    let name = format!("{}-{}.mp4", ep.backend.id(), crate::util::ids::new_id("vg"));
    let out = dest_dir.join(name);
    (ctl.progress)(0.02, "submitting");
    match ep.backend {
        Backend::Fal => fal(ep, req, &out, ctl)?,
        Backend::Gemini => gemini(ep, req, &out, ctl)?,
        Backend::Runway => runway(ep, req, &out, ctl)?,
        Backend::Luma => luma(ep, req, &out, ctl)?,
    }
    (ctl.progress)(1.0, "done");
    Ok(vec![out])
}

// ---------------------------------------------------------------- fal.ai

pub fn body_for_fal(req: &GenRequest) -> Value {
    let mut body = if req.model.contains("/wan/") {
        // Wan family: length is a frame count (default 16 fps), resolution is an enum.
        let fps = 16u32;
        json!({"prompt": req.prompt, "aspect_ratio": req.aspect, "resolution": "720p", "frames_per_second": fps,
               "num_frames": (req.seconds * fps + 1).clamp(17, 161)})
    } else {
        // Other models: best-effort common fields. Use `extra` to set model-specific ones.
        json!({"prompt": req.prompt, "aspect_ratio": req.aspect, "duration": req.seconds.to_string()})
    };
    for (k, v) in &req.extra {
        body[k] = v.clone();
    }
    body
}

fn fal(ep: &Endpoint, req: &GenRequest, out: &Path, ctl: &Control) -> Result<()> {
    let a = agent(Duration::from_secs(60));
    let auth = format!("Key {}", ep.key);
    let sub = json_of(
        a.post(&format!("{}/{}", ep.base, req.model.trim_matches('/'))).set("Authorization", &auth).send_json(body_for_fal(req)).map_err(|e| http_err("fal submit", e))?,
        "fal submit",
    )?;
    let status_url = sub["status_url"].as_str().ok_or_else(|| Error::new(ErrorCode::Provider, "fal: no status_url in the submit response"))?.to_string();
    let response_url = sub["response_url"].as_str().ok_or_else(|| Error::new(ErrorCode::Provider, "fal: no response_url"))?.to_string();
    check_url(&status_url)?;
    check_url(&response_url)?;
    wait_loop(ctl, "fal is generating", || {
        let s = json_of(a.get(&status_url).set("Authorization", &auth).call().map_err(|e| http_err("fal status", e))?, "fal status")?;
        if let Some(err) = s["error"].as_str().filter(|e| !e.is_empty()) {
            return Err(Error::new(ErrorCode::Provider, format!("fal failed: {err}")));
        }
        Ok(if s["status"] == "COMPLETED" { Some(()) } else { None })
    })?;
    let result = json_of(a.get(&response_url).set("Authorization", &auth).call().map_err(|e| http_err("fal result", e))?, "fal result")?;
    let url = result["video"]["url"].as_str().or_else(|| result["videos"][0]["url"].as_str())
        .ok_or_else(|| Error::new(ErrorCode::Provider, "fal: the result has no video url (this model may use a different output field)"))?;
    (ctl.progress)(0.95, "downloading");
    download(url, out, &[])
}

// ---------------------------------------------------------------- Gemini Omni

/// Find the first `{"type":"video", data|uri}` object anywhere in a response.
fn find_video(v: &Value) -> Option<&Value> {
    match v {
        Value::Object(m) => {
            if m.get("type").and_then(|t| t.as_str()) == Some("video") && (m.contains_key("data") || m.contains_key("uri")) {
                return Some(v);
            }
            m.values().find_map(find_video)
        }
        Value::Array(a) => a.iter().find_map(find_video),
        _ => None,
    }
}

fn gemini(ep: &Endpoint, req: &GenRequest, out: &Path, ctl: &Control) -> Result<()> {
    let a = agent(Duration::from_secs(600)); // synchronous generation can take minutes
    let body = json!({
        "model": req.model, "input": req.prompt, "background": false, "store": false, "stream": false,
        "response_format": {"type": "video", "aspect_ratio": req.aspect, "resolution": "720p", "duration": format!("{}s", req.seconds.clamp(3, 15)), "delivery": "uri"},
    });
    (ctl.progress)(0.1, "Gemini Omni is generating (this call blocks until the clip is ready)");
    let v = json_of(
        a.post(&format!("{}/v1beta/interactions", ep.base)).set("x-goog-api-key", &ep.key).send_json(body).map_err(|e| http_err("Gemini", e))?,
        "Gemini",
    )?;
    let video = find_video(&v).ok_or_else(|| {
        let refusal = v.to_string().chars().take(200).collect::<String>();
        Error::new(ErrorCode::Provider, format!("Gemini returned no video (possibly blocked by safety filters): {refusal}"))
    })?;
    (ctl.progress)(0.9, "downloading");
    if let Some(data) = video["data"].as_str() {
        let bytes = base64::engine::general_purpose::STANDARD.decode(data).map_err(|e| Error::new(ErrorCode::Provider, format!("Gemini: bad base64 video: {e}")))?;
        return paths::write_atomic(out, &bytes);
    }
    let uri = video["uri"].as_str().ok_or_else(|| Error::new(ErrorCode::Provider, "Gemini: video has neither data nor uri"))?;
    // A Files API resource: wait until ACTIVE, then fetch the bytes.
    let file_name = uri.rsplit("/files/").next().unwrap_or("").split(':').next().unwrap_or("");
    if uri.contains("/files/") && !file_name.is_empty() {
        let info_url = format!("{}/v1beta/files/{}", ep.base, file_name);
        wait_loop(ctl, "Gemini is preparing the file", || {
            let f = json_of(a.get(&info_url).set("x-goog-api-key", &ep.key).call().map_err(|e| http_err("Gemini file", e))?, "Gemini file")?;
            match f["state"].as_str().unwrap_or("ACTIVE") {
                "FAILED" => Err(Error::new(ErrorCode::Provider, "Gemini failed to prepare the video file")),
                "ACTIVE" => Ok(Some(())),
                _ => Ok(None),
            }
        })?;
        return download(&format!("{}/v1beta/files/{}:download?alt=media", ep.base, file_name), out, &[("x-goog-api-key", &ep.key)]);
    }
    download(uri, out, &[("x-goog-api-key", &ep.key)])
}

// ---------------------------------------------------------------- Runway

fn runway(ep: &Endpoint, req: &GenRequest, out: &Path, ctl: &Control) -> Result<()> {
    let a = agent(Duration::from_secs(60));
    let auth = format!("Bearer {}", ep.key);
    let ratio = if req.aspect == "9:16" { "720:1280" } else { "1280:720" };
    let created = json_of(
        a.post(&format!("{}/v1/text_to_video", ep.base)).set("Authorization", &auth).set("X-Runway-Version", "2024-11-06")
            .send_json(json!({"model": req.model, "promptText": req.prompt, "ratio": ratio, "duration": req.seconds.clamp(2, 10)}))
            .map_err(|e| http_err("Runway submit", e))?,
        "Runway submit",
    )?;
    let id = created["id"].as_str().ok_or_else(|| Error::new(ErrorCode::Provider, "Runway: no task id"))?.to_string();
    let url = wait_loop(ctl, "Runway is generating", || {
        let t = json_of(
            a.get(&format!("{}/v1/tasks/{}", ep.base, id)).set("Authorization", &auth).set("X-Runway-Version", "2024-11-06").call().map_err(|e| http_err("Runway task", e))?,
            "Runway task",
        )?;
        match t["status"].as_str().unwrap_or("") {
            "SUCCEEDED" => t["output"][0].as_str().map(|u| Some(u.to_string())).ok_or_else(|| Error::new(ErrorCode::Provider, "Runway: task succeeded without an output url")),
            "FAILED" | "CANCELLED" => Err(Error::new(ErrorCode::Provider, format!("Runway task {}: {}", t["status"].as_str().unwrap_or("failed").to_lowercase(), t["failure"].as_str().unwrap_or("no reason given")))),
            _ => Ok(None), // PENDING / THROTTLED / RUNNING
        }
    })?;
    (ctl.progress)(0.95, "downloading");
    download(&url, out, &[])
}

// ---------------------------------------------------------------- Luma

fn luma(ep: &Endpoint, req: &GenRequest, out: &Path, ctl: &Control) -> Result<()> {
    let a = agent(Duration::from_secs(60));
    let auth = format!("Bearer {}", ep.key);
    let duration = if req.seconds <= 5 { "5s" } else { "9s" };
    let created = json_of(
        a.post(&format!("{}/generations", ep.base)).set("Authorization", &auth)
            .send_json(json!({"prompt": req.prompt, "model": req.model, "resolution": "720p", "duration": duration, "aspect_ratio": req.aspect}))
            .map_err(|e| http_err("Luma submit", e))?,
        "Luma submit",
    )?;
    let id = created["id"].as_str().ok_or_else(|| Error::new(ErrorCode::Provider, "Luma: no generation id"))?.to_string();
    let url = wait_loop(ctl, "Luma is generating", || {
        let g = json_of(a.get(&format!("{}/generations/{}", ep.base, id)).set("Authorization", &auth).call().map_err(|e| http_err("Luma status", e))?, "Luma status")?;
        match g["state"].as_str().unwrap_or("") {
            "completed" => g["assets"]["video"].as_str().map(|u| Some(u.to_string())).ok_or_else(|| Error::new(ErrorCode::Provider, "Luma: completed without a video url")),
            "failed" => Err(Error::new(ErrorCode::Provider, format!("Luma failed: {}", g["failure_reason"].as_str().unwrap_or("no reason given")))),
            _ => Ok(None), // queued / dreaming
        }
    })?;
    (ctl.progress)(0.95, "downloading");
    download(&url, out, &[])
}

// ---------------------------------------------------------------- spend limits

#[derive(Debug, Serialize, Deserialize)]
struct LedgerLine {
    day: String,
    backend: String,
    model: String,
    seconds: u32,
}

fn ledger_path() -> PathBuf {
    paths::config_dir().join("videogen-ledger.jsonl")
}

fn today() -> String {
    // UTC day number is enough for a daily cap and avoids a calendar dependency.
    let secs = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    format!("{}", secs / 86_400)
}

pub fn seconds_used_today() -> u32 {
    let day = today();
    std::fs::read_to_string(ledger_path())
        .unwrap_or_default()
        .lines()
        .filter_map(|l| serde_json::from_str::<LedgerLine>(l).ok())
        .filter(|l| l.day == day)
        .map(|l| l.seconds)
        .sum()
}

/// Refuse unless paid generation is switched on and the clip fits both caps.
pub fn check_budget(settings: &VideoGenSettings, seconds: u32) -> Result<()> {
    if !settings.enabled {
        return Err(Error::new(ErrorCode::PermissionDenied, "paid video generation is switched off")
            .with_hint("turn it on in Settings > Video generation, and set the seconds limits"));
    }
    if seconds == 0 || seconds > settings.max_job_seconds {
        return Err(Error::new(ErrorCode::Budget, format!("a clip may be at most {} seconds (your per-job limit); asked for {seconds}", settings.max_job_seconds)));
    }
    let used = seconds_used_today();
    if used + seconds > settings.max_day_seconds {
        return Err(Error::new(ErrorCode::Budget, format!("daily video limit reached: {used}s used of {}s; this clip needs {seconds}s", settings.max_day_seconds)));
    }
    Ok(())
}

/// Record spend BEFORE submitting: a failed job may still be billed, so the cap errs on the safe side.
pub fn record(backend: Backend, model: &str, seconds: u32) -> Result<()> {
    let line = serde_json::to_string(&LedgerLine { day: today(), backend: backend.id().into(), model: model.into(), seconds })
        .map_err(|e| Error::new(ErrorCode::Internal, e.to_string()))?;
    let path = ledger_path();
    if let Some(parent) = path.parent() {
        paths::ensure_dir(parent)?;
    }
    let mut f = std::fs::OpenOptions::new().create(true).append(true).open(&path).map_err(|e| Error::new(ErrorCode::Io, e.to_string()))?;
    writeln!(f, "{line}").map_err(|e| Error::new(ErrorCode::Io, e.to_string()))
}

#[cfg(test)]
mod tests;
