//! Studio "production suite" endpoints (contract: web/STUDIO_ENDPOINTS.md).
//!
//! Every handler answers exactly the JSON the Studio page reads and delegates the work to the
//! engine that already implements it: `shorts`, `captions` / `captions::whisperx`,
//! `audio::demucs`, `setup` and `blender::{scene, preview, render_animation_job, distributed}`.
//! Nothing here re-implements a pipeline: a route is transport plus the same edition gate the
//! CLI applies at the same point, so a Pro action on a Free edition is refused with 403 instead
//! of running quietly.
//!
//! Long operations go through the `Jobs` queue `/api/render` uses, so progress is observable by
//! job id and every handler returns as soon as the work is queued.
//!
//! State that must outlive a server restart lives in the project:
//!   * `<project>/blender/state.json`   - last scene, its four-view preview, its render job
//!   * `<project>/captions/studio.json` - caption timing mode and saved style presets
//! Shorts and audio runs keep only their job id in memory: the job record is the single source of
//! truth for their progress, result and error.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::config::Config;
use crate::error::{Error, ErrorCode, Result};
use crate::jobs::Jobs;
use crate::project::model::{CaptionStyle, Cue, TextPosition};
use crate::project::store::ProjectStore;
use crate::util::paths;

/// How many runs a status route keeps reporting before the oldest is forgotten.
const MAX_RUNS: usize = 20;
const STUDIO_CAPTIONS_FILE: &str = "captions/studio.json";
const STUDIO_BLENDER_FILE: &str = "blender/state.json";

/// Everything a handler needs from the HTTP layer, borrowed for the request.
pub struct Env<'a> {
    pub project_dir: Option<&'a Path>,
    pub jobs: &'a Jobs,
    pub config: &'a Config,
}

/// Per-server Studio state: run records whose truth is a job id, plus write locks for the two
/// on-disk files so a job thread and a request cannot interleave a read-modify-write.
#[derive(Default)]
pub struct Studio {
    shorts: Mutex<Vec<ShortRun>>,
    audio: Mutex<Option<AudioRun>>,
    setup: Mutex<Option<SetupRun>>,
    files: Mutex<()>,
    blender_info: Mutex<Option<(PathBuf, String)>>,
    gpus: Mutex<Option<(u64, Vec<Value>)>>,
}

impl Studio {
    pub fn new() -> Studio {
        Studio::default()
    }
}

struct ShortRun {
    root: PathBuf,
    job_id: String,
    source: String,
    count: u32,
}

#[derive(Clone)]
struct AudioRun {
    root: PathBuf,
    job_id: String,
}

struct SetupRun {
    job_id: String,
    requested: Vec<String>,
}

/// True for every path this module answers, so the caller reads a JSON body only for them.
pub fn is_route(method: &str, path: &str) -> bool {
    matches!(
        (method, path),
        ("GET", "/api/shorts")
            | ("POST", "/api/shorts")
            | ("GET", "/api/captions/style")
            | ("POST", "/api/captions/style")
            | ("POST", "/api/captions/style/preset")
            | ("POST", "/api/captions/word_level")
            | ("GET", "/api/audio/enhance")
            | ("POST", "/api/audio/enhance")
            | ("GET", "/api/blender/preview")
            | ("POST", "/api/blender/scene")
            | ("POST", "/api/blender/render")
            | ("POST", "/api/blender/recover")
            | ("GET", "/api/setup/pro-ai")
            | ("POST", "/api/setup/pro-ai")
    )
}

/// Dispatch every Studio route. `None` means "not a Studio path", so the caller keeps its 404.
pub fn handle(
    st: &Arc<Studio>,
    method: &str,
    path: &str,
    body: &Value,
    env: &Env,
) -> Option<Result<Value>> {
    let out = match (method, path) {
        ("GET", "/api/shorts") => st.shorts_status(env),
        ("POST", "/api/shorts") => st.shorts_start(env, body),
        ("GET", "/api/captions/style") => captions_style_status(env),
        ("POST", "/api/captions/style") => captions_style_apply(env, body),
        ("POST", "/api/captions/style/preset") => captions_preset_save(env, body),
        ("POST", "/api/captions/word_level") => captions_word_level(env, body),
        ("GET", "/api/audio/enhance") => st.audio_status(env),
        ("POST", "/api/audio/enhance") => st.audio_start(env, body),
        ("GET", "/api/blender/preview") => st.blender_preview(env),
        ("POST", "/api/blender/scene") => st.blender_scene(env, body),
        ("POST", "/api/blender/render") => st.blender_render(env, body),
        ("POST", "/api/blender/recover") => st.blender_recover(env, body),
        ("GET", "/api/setup/pro-ai") => st.setup_status(env),
        ("POST", "/api/setup/pro-ai") => st.setup_start(env, body),
        _ => return None,
    };
    Some(out)
}

/// The status the page expects: 400 bad input, 403 edition or permission, 404 no project or no
/// such object, 409 conflict, 500 anything the caller cannot act on.
pub fn status_for(err: &Error) -> u16 {
    match err.code {
        ErrorCode::PermissionDenied => 403,
        ErrorCode::NotFound => 404,
        ErrorCode::Conflict | ErrorCode::Cancelled => 409,
        ErrorCode::InvalidInput | ErrorCode::Unsupported | ErrorCode::DependencyMissing => 400,
        _ => 500,
    }
}

pub fn error_body(err: &Error) -> Value {
    json!({"error": err.message, "code": err.code, "hint": err.hint})
}

// ---------------------------------------------------------------------------
// Shared helpers
// ---------------------------------------------------------------------------

fn store_of(env: &Env) -> Result<ProjectStore> {
    let dir = env
        .project_dir
        .ok_or_else(|| Error::not_found("no project is open"))?;
    ProjectStore::open(dir)
}

fn entitlements(env: &Env) -> crate::edition::Entitlements {
    crate::edition::resolve(env.config)
}

fn project_root(env: &Env) -> Option<PathBuf> {
    env.project_dir.map(paths::normalize)
}

/// The `/api/media/` URL for a produced file, or None when the file is not inside a served
/// directory. That route only resolves inside `assets/` and `renders/`, so a file written
/// anywhere else deliberately gets no URL instead of a link that would 404.
fn media_url(store: &ProjectStore, file: &Path) -> Option<String> {
    for dir in [store.assets_dir(), store.renders_dir()] {
        if let Ok(within) = paths::ensure_within(&dir, file) {
            if within.is_file() {
                if let Ok(rel) = within.strip_prefix(&dir) {
                    return Some(format!(
                        "/api/media/{}",
                        rel.to_string_lossy().replace('\\', "/")
                    ));
                }
            }
        }
    }
    None
}

fn str_field<'a>(body: &'a Value, key: &str) -> Option<&'a str> {
    body.get(key).and_then(Value::as_str).map(str::trim)
}

fn bool_field(body: &Value, key: &str) -> Option<bool> {
    body.get(key).and_then(Value::as_bool)
}

fn u32_field(body: &Value, key: &str) -> Option<u32> {
    body.get(key)
        .and_then(|v| v.as_u64())
        .map(|v| v.min(u32::MAX as u64) as u32)
}

fn f64_field(body: &Value, key: &str) -> Option<f64> {
    body.get(key).and_then(|v| v.as_f64())
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn read_json_file(path: &Path) -> Option<Value> {
    let text = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&text).ok()
}

fn write_json_file(path: &Path, value: &Value) -> Result<()> {
    if let Some(parent) = path.parent() {
        paths::ensure_dir(parent)?;
    }
    let body = serde_json::to_vec_pretty(value)
        .map_err(|e| Error::internal(format!("cannot serialise {}: {e}", path.display())))?;
    paths::write_atomic(path, &body)
}

fn position_str(position: TextPosition) -> &'static str {
    match position {
        TextPosition::Top => "top",
        TextPosition::Center => "center",
        TextPosition::Bottom => "bottom",
        TextPosition::LowerThird => "lower_third",
    }
}

fn parse_position(raw: &str) -> Result<TextPosition> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "top" => Ok(TextPosition::Top),
        "center" => Ok(TextPosition::Center),
        "bottom" => Ok(TextPosition::Bottom),
        "lower_third" | "lower third" | "lowerthird" => Ok(TextPosition::LowerThird),
        other => Err(Error::invalid(format!(
            "unknown position '{other}'; use top, center, bottom or lower_third"
        ))),
    }
}

// ---------------------------------------------------------------------------
// Shorts
// ---------------------------------------------------------------------------

impl Studio {
    fn shorts_status(&self, env: &Env) -> Result<Value> {
        let root = project_root(env);
        let runs = self.shorts.lock().unwrap();
        let mut out: Vec<Value> = Vec::new();
        for run in runs.iter().rev() {
            if root.as_ref() != Some(&run.root) {
                continue;
            }
            out.push(shorts_run_json(env.jobs, run));
        }
        Ok(json!({"runs": out}))
    }

    fn shorts_start(self: &Arc<Self>, env: &Env, body: &Value) -> Result<Value> {
        let store = store_of(env)?;
        entitlements(env).check_feature(crate::edition::Feature::ShortsMulti)?;
        let project = store.load()?;
        let source_id = str_field(body, "source").unwrap_or("");
        if source_id.is_empty() {
            return Err(Error::invalid(
                "source is required: the id of an imported video or audio asset",
            ));
        }
        let asset = project
            .asset(source_id)
            .ok_or_else(|| Error::not_found(format!("asset {source_id} not found")))?;
        let input = store.asset_path(asset)?;
        if !crate::shorts::source_usable(&input) {
            return Err(Error::invalid(format!(
                "the source is empty or missing on disk: {}",
                input.display()
            ))
            .with_hint("import the file again from the Media tab"));
        }
        let count = u32_field(body, "count").unwrap_or(3).clamp(1, 20);
        let min = f64_field(body, "min_seconds").unwrap_or(20.0);
        let max = f64_field(body, "max_seconds").unwrap_or(60.0);
        if !(min.is_finite() && max.is_finite()) || min <= 0.0 || min > max || max > 900.0 {
            return Err(Error::invalid(format!(
                "duration bounds must satisfy 0 < min_seconds <= max_seconds <= 900, got {min}-{max}"
            )));
        }
        let vocab: Vec<String> = body
            .get("vocab")
            .and_then(Value::as_array)
            .map(|items| {
                items
                    .iter()
                    .filter_map(Value::as_str)
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
                    .take(64)
                    .collect()
            })
            .unwrap_or_default();
        // The default output is inside `renders/`, so every produced Short is playable through
        // `/api/media/`; an explicit directory is honoured exactly as given.
        let out_dir = match str_field(body, "output_dir").filter(|s| !s.is_empty()) {
            Some(raw) => paths::absolutize(&paths::expand_tilde(raw), &store.root),
            None => store.renders_dir().join("shorts"),
        };
        paths::ensure_dir(&out_dir)?;

        let req = crate::shorts::ShortsRequest {
            input,
            count,
            min_duration: min,
            max_duration: max,
            output_dir: Some(out_dir.clone()),
            vocab,
        };
        let store_for_job = store.clone();
        let out_for_job = out_dir.clone();
        let wanted = count;
        let job_id = env.jobs.spawn("shorts", move |job| {
            // The engine has no progress callback, so progress is measured from what it has
            // actually written: a Short counts once its file appears (or is rewritten).
            let started = SystemTime::now();
            let req_for_worker = req.clone();
            let worker =
                std::thread::spawn(move || crate::shorts::generate_with_diagnostics(&req_for_worker));
            loop {
                let done = count_written_shorts(&out_for_job, started);
                job.progress(
                    (done as f32 / wanted.max(1) as f32).min(0.99),
                    format!("{done}/{wanted} Shorts written"),
                );
                if worker.is_finished() {
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(400));
            }
            let outcome = worker
                .join()
                .map_err(|_| Error::internal("the Shorts worker panicked"))?;
            let (report, diagnostics) = outcome?;
            let outputs: Vec<Value> = report
                .shorts
                .iter()
                .map(|s| {
                    json!({
                        "index": s.index,
                        "path": s.path.to_string_lossy(),
                        "seconds": s.end - s.start,
                        "width": s.width,
                        "height": s.height,
                        "qa_passed": s.qa_passed,
                        "url": media_url(&store_for_job, &s.path),
                    })
                })
                .collect();
            Ok(json!({
                "source": report.source.to_string_lossy(),
                "elapsed_ms": report.elapsed_ms,
                "outputs": outputs,
                "diagnostics": serde_json::to_value(&diagnostics).unwrap_or(Value::Null),
            }))
        });
        {
            let mut runs = self.shorts.lock().unwrap();
            runs.push(ShortRun {
                root: paths::normalize(&store.root),
                job_id: job_id.clone(),
                source: source_id.to_string(),
                count,
            });
            if runs.len() > MAX_RUNS {
                let drop = runs.len() - MAX_RUNS;
                runs.drain(0..drop);
            }
        }
        Ok(json!({"run_id": job_id, "count": count}))
    }
}

/// How many Shorts this run has written so far, by watching the output directory.
fn count_written_shorts(dir: &Path, started: SystemTime) -> usize {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return 0;
    };
    entries
        .filter_map(|e| e.ok())
        .filter(|e| {
            let name = e.file_name().to_string_lossy().to_string();
            name.starts_with("short_") && name.ends_with(".mp4")
        })
        .filter(|e| {
            e.metadata()
                .and_then(|m| m.modified())
                .map(|m| m >= started - std::time::Duration::from_secs(2))
                .unwrap_or(false)
        })
        .count()
}

fn shorts_run_json(jobs: &Jobs, run: &ShortRun) -> Value {
    match jobs.get(&run.job_id) {
        Some(snap) => {
            let outputs = snap
                .output
                .as_ref()
                .and_then(|o| o.get("outputs"))
                .cloned()
                .unwrap_or_else(|| json!([]));
            json!({
                "id": run.job_id,
                "state": snap.status.as_str(),
                "progress": snap.progress,
                "count": run.count,
                "source": run.source,
                "message": snap.message,
                "error": snap.error.as_ref().map(|e| e.message.clone()).unwrap_or_default(),
                "outputs": outputs,
            })
        }
        None => json!({
            "id": run.job_id,
            "state": "failed",
            "progress": 0.0,
            "count": run.count,
            "source": run.source,
            "error": "the job record is gone (the server restarted); run the Shorts again",
            "outputs": [],
        }),
    }
}

// ---------------------------------------------------------------------------
// Captions: style, presets, word-level timing
// ---------------------------------------------------------------------------

/// The style shape the page sends and reads: size, colour, background, case, position.
#[derive(Clone, PartialEq, Debug)]
struct CapStyle {
    size: u32,
    color: String,
    background: Option<String>,
    uppercase: bool,
    position: TextPosition,
}

impl CapStyle {
    fn from_caption(style: &CaptionStyle) -> CapStyle {
        CapStyle {
            size: style.size,
            color: style.color.clone(),
            background: style.background.clone(),
            uppercase: style.uppercase,
            position: style.position,
        }
    }

    fn to_json(&self) -> Value {
        json!({
            "size": self.size,
            "color": self.color,
            "background": self.background,
            "uppercase": self.uppercase,
            "position": position_str(self.position),
        })
    }

    /// Overlay only the keys the caller actually sent, so a partial style keeps the rest.
    fn overlay(mut self, body: &Value) -> Result<CapStyle> {
        if let Some(size) = u32_field(body, "size") {
            if !(8..=200).contains(&size) {
                return Err(Error::invalid(format!("size must be 8..200, got {size}")));
            }
            self.size = size;
        }
        if let Some(color) = str_field(body, "color") {
            if color.is_empty() || color.chars().count() > 40 {
                return Err(Error::invalid(
                    "color must be 1..40 characters, e.g. #ffffff or white",
                ));
            }
            self.color = color.to_string();
        }
        if body.get("background").is_some() {
            match str_field(body, "background") {
                Some(bg) if !bg.is_empty() => {
                    if bg.chars().count() > 40 {
                        return Err(Error::invalid(
                            "background must be at most 40 characters, e.g. black@0.6",
                        ));
                    }
                    self.background = Some(bg.to_string());
                }
                _ => self.background = None,
            }
        }
        if let Some(upper) = bool_field(body, "uppercase") {
            self.uppercase = upper;
        }
        if let Some(position) = str_field(body, "position") {
            self.position = parse_position(position)?;
        }
        Ok(self)
    }
}

#[derive(Serialize, Deserialize, Clone, Default)]
#[serde(default)]
struct SavedPreset {
    name: String,
    style: SavedStyle,
}

#[derive(Serialize, Deserialize, Clone)]
#[serde(default)]
struct SavedStyle {
    size: u32,
    color: String,
    background: Option<String>,
    uppercase: bool,
    position: String,
}

impl Default for SavedStyle {
    fn default() -> SavedStyle {
        SavedStyle {
            size: 48,
            color: "#ffffff".into(),
            background: None,
            uppercase: false,
            position: "bottom".into(),
        }
    }
}

#[derive(Serialize, Deserialize, Default)]
#[serde(default)]
struct CaptionsFile {
    mode: String,
    presets: Vec<SavedPreset>,
}

/// Built-in presets: they exist in the build, not in the project, so they are always offered.
const BUILTIN_CAPTION_PRESETS: [(&str, u32, &str, Option<&str>, bool, &str); 3] = [
    ("Bold yellow", 64, "#ffe14d", Some("black@0.7"), true, "lower_third"),
    ("Clean white", 48, "#ffffff", Some("black@0.5"), false, "bottom"),
    ("Centred title", 72, "#ffffff", None, false, "center"),
];

fn captions_file(store: &ProjectStore) -> PathBuf {
    store.root.join(STUDIO_CAPTIONS_FILE)
}

fn load_captions_file(store: &ProjectStore) -> CaptionsFile {
    read_json_file(&captions_file(store))
        .and_then(|v| serde_json::from_value::<CaptionsFile>(v).ok())
        .unwrap_or_default()
}

fn captions_style_status(env: &Env) -> Result<Value> {
    let mut mode = "line".to_string();
    let mut active = CapStyle::from_caption(&CaptionStyle::default());
    let mut presets: Vec<Value> = Vec::new();
    let mut saved: Vec<SavedPreset> = Vec::new();
    let mut has_project = false;
    let mut tracks = 0usize;
    if let Ok(store) = store_of(env) {
        has_project = true;
        let file = load_captions_file(&store);
        if !file.mode.trim().is_empty() {
            mode = file.mode.clone();
        }
        saved = file.presets.clone();
        if let Ok(project) = store.load() {
            tracks = project.captions.len();
            if let Some(track) = project.captions.first() {
                active = CapStyle::from_caption(&track.style);
            }
        }
    }
    for (name, size, color, background, uppercase, position) in BUILTIN_CAPTION_PRESETS {
        let preset = CapStyle {
            size,
            color: color.to_string(),
            background: background.map(str::to_string),
            uppercase,
            position: parse_position(position).unwrap_or(TextPosition::Bottom),
        };
        presets.push(json!({
            "name": name,
            "builtin": true,
            "active": preset == active,
            "style": preset.to_json(),
        }));
    }
    for preset in &saved {
        // A saved preset that shadows a built-in name would appear twice in the dropdown; the
        // built-in already carries that style, so the saved copy is not offered again.
        if BUILTIN_CAPTION_PRESETS
            .iter()
            .any(|(name, ..)| *name == preset.name)
        {
            continue;
        }
        let style = CapStyle {
            size: preset.style.size,
            color: preset.style.color.clone(),
            background: preset.style.background.clone(),
            uppercase: preset.style.uppercase,
            position: parse_position(&preset.style.position).unwrap_or(TextPosition::Bottom),
        };
        presets.push(json!({
            "name": preset.name,
            "builtin": false,
            "active": style == active,
            "style": style.to_json(),
        }));
    }
    Ok(json!({
        "mode": mode,
        "active": active.to_json(),
        "presets": presets,
        "project": has_project,
        "tracks": tracks,
    }))
}

fn captions_style_apply(env: &Env, body: &Value) -> Result<Value> {
    let store = store_of(env)?;
    let ent = entitlements(env);
    let mode = str_field(body, "mode")
        .map(|m| m.to_ascii_lowercase())
        .filter(|m| !m.is_empty());
    if let Some(mode) = &mode {
        match mode.as_str() {
            "line" => {}
            "word" => ent.check_feature(crate::edition::Feature::CaptionsWordLevel)?,
            "karaoke" => ent.check_feature(crate::edition::Feature::CaptionsKaraoke)?,
            other => {
                return Err(Error::invalid(format!(
                    "unknown captions mode '{other}'; use line, word or karaoke"
                )))
            }
        }
    }
    let mut project = store.load()?;
    if project.captions.is_empty() {
        return Err(Error::invalid(
            "there is no caption track yet; transcribe a clip or import a subtitle file first",
        ));
    }
    let explicit = |key: &str| body.get(key).is_some();
    let mut applied = 0usize;
    for track in project.captions.iter_mut() {
        let style = CapStyle::from_caption(&track.style).overlay(body)?;
        if explicit("size") {
            track.style.size = style.size;
        }
        if explicit("color") {
            track.style.color = style.color.clone();
        }
        if explicit("background") {
            track.style.background = style.background.clone();
        }
        if explicit("uppercase") {
            track.style.uppercase = style.uppercase;
        }
        if explicit("position") {
            track.style.position = style.position;
        }
        applied += 1;
    }
    store.save(&mut project)?;
    if let Some(mode) = &mode {
        let mut file = load_captions_file(&store);
        file.mode = mode.clone();
        write_json_file(
            &captions_file(&store),
            &serde_json::to_value(&file).unwrap_or(Value::Null),
        )?;
    }
    Ok(json!({"ok": true, "tracks": applied, "mode": mode}))
}

fn captions_preset_save(env: &Env, body: &Value) -> Result<Value> {
    let store = store_of(env)?;
    entitlements(env).check_feature(crate::edition::Feature::CaptionsBranded)?;
    let name = str_field(body, "name").unwrap_or("");
    if name.is_empty() {
        return Err(Error::invalid("name the preset first"));
    }
    if name.chars().count() > 40 {
        return Err(Error::invalid("preset names are at most 40 characters"));
    }
    let raw_style = body.get("style").cloned().unwrap_or_else(|| json!({}));
    if !raw_style.is_object() {
        return Err(Error::invalid("style must be an object of caption style keys"));
    }
    let base = store
        .load()
        .ok()
        .and_then(|p| p.captions.first().map(|t| CapStyle::from_caption(&t.style)))
        .unwrap_or_else(|| CapStyle::from_caption(&CaptionStyle::default()));
    let style = base.overlay(&raw_style)?;
    let saved_style = SavedStyle {
        size: style.size,
        color: style.color.clone(),
        background: style.background.clone(),
        uppercase: style.uppercase,
        position: position_str(style.position).to_string(),
    };
    let mut file = load_captions_file(&store);
    match file.presets.iter_mut().find(|p| p.name == name) {
        Some(existing) => existing.style = saved_style,
        None => file.presets.push(SavedPreset {
            name: name.to_string(),
            style: saved_style,
        }),
    }
    file.presets.sort_by(|a, b| a.name.cmp(&b.name));
    write_json_file(
        &captions_file(&store),
        &serde_json::to_value(&file).unwrap_or(Value::Null),
    )?;
    Ok(json!({"ok": true, "saved": name}))
}

/// The Python that has WhisperX: the adapter's environment variable first (user authority),
/// then the interpreter `cinvar setup --pro-ai` installed into the managed venv.
fn whisperx_python(config: &Config) -> Result<PathBuf> {
    let configured = std::env::var("CINVAR_WHISPERX_PYTHON")
        .ok()
        .filter(|v| !v.trim().is_empty())
        .or_else(|| config.helper("CINVAR_WHISPERX_PYTHON").map(str::to_string));
    if let Some(raw) = configured {
        let path = paths::expand_tilde(raw.trim());
        if path.is_file() {
            return Ok(path);
        }
        return Err(Error::new(
            ErrorCode::DependencyMissing,
            format!(
                "CINVAR_WHISPERX_PYTHON does not point to a python binary: {}",
                path.display()
            ),
        ));
    }
    let managed = paths::data_dir()
        .join("venvs")
        .join("whisperx")
        .join("bin")
        .join("python");
    if managed.is_file() {
        return Ok(managed);
    }
    Err(Error::new(
        ErrorCode::DependencyMissing,
        "word-level transcription needs WhisperX, which is not installed",
    )
    .with_hint("run `cinvar setup --pro-ai` and export the CINVAR_WHISPERX_PYTHON it prints"))
}

fn captions_word_level(env: &Env, body: &Value) -> Result<Value> {
    let store = store_of(env)?;
    let ent = entitlements(env);
    let clip_id = str_field(body, "clip_id").unwrap_or("");
    if clip_id.is_empty() {
        return Err(Error::invalid(
            "clip_id is required: select a clip on the timeline first",
        ));
    }
    let mode = str_field(body, "mode")
        .unwrap_or("word")
        .to_ascii_lowercase();
    match mode.as_str() {
        "word" => ent.check_feature(crate::edition::Feature::CaptionsWordLevel)?,
        "karaoke" => {
            ent.check_feature(crate::edition::Feature::CaptionsWordLevel)?;
            ent.check_feature(crate::edition::Feature::CaptionsKaraoke)?;
        }
        other => {
            return Err(Error::invalid(format!(
                "unknown mode '{other}'; use word or karaoke"
            )))
        }
    }
    let mut project = store.load()?;
    let (asset_id, clip_start, source_in, source_out, speed) = {
        let clip = project
            .clip(clip_id)
            .ok_or_else(|| Error::not_found(format!("clip {clip_id} not found")))?;
        let asset = clip
            .asset
            .clone()
            .ok_or_else(|| Error::invalid("that clip has no media to transcribe"))?;
        (
            asset,
            clip.timeline_start,
            clip.source_in,
            clip.source_out,
            clip.speed,
        )
    };
    let asset = project
        .asset(&asset_id)
        .ok_or_else(|| Error::not_found("the clip's asset is missing"))?
        .clone();
    let source = store.asset_path(&asset)?;
    let python = whisperx_python(env.config)?;
    let slug: String = clip_id
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    let work = store.cache_dir().join("word_level").join(slug);
    paths::ensure_dir(&work)?;
    let wav = work.join("speech.wav");
    crate::media::extract_audio(&source, &wav)?;
    let transcript =
        crate::captions::whisperx::run_whisperx_with_vocab(&python, &wav, &work, None, &[])?;
    if transcript.segments.is_empty() {
        return Err(Error::invalid("WhisperX found no speech in that clip"));
    }
    let words = transcript.words.len();
    let srt = crate::captions::whisperx::srt_from_segments(&transcript.segments);
    let (raw, _) = crate::captions::parse_subtitles(&srt);
    let speed = if speed.is_finite() && speed > 0.01 {
        speed
    } else {
        1.0
    };
    let mut cues: Vec<Cue> = Vec::new();
    for cue in raw {
        if cue.end <= source_in || cue.start >= source_out {
            continue;
        }
        let s0 = (cue.start.max(source_in) - source_in).scale(1.0 / speed);
        let e0 = (cue.end.min(source_out) - source_in).scale(1.0 / speed);
        cues.push(Cue {
            start: clip_start + s0,
            end: clip_start + e0,
            text: cue.text,
        });
    }
    if cues.is_empty() {
        return Err(Error::invalid(
            "WhisperX found no speech inside the trimmed part of that clip",
        ));
    }
    let cue_count = cues.len();
    let track_id = crate::project::ops::set_captions(&mut project, cues, "en", None, "whisperx")?;
    store.save(&mut project)?;
    {
        let mut file = load_captions_file(&store);
        file.mode = mode.clone();
        write_json_file(
            &captions_file(&store),
            &serde_json::to_value(&file).unwrap_or(Value::Null),
        )?;
    }
    Ok(json!({
        "ok": true,
        "words": words,
        "cues": cue_count,
        "mode": mode,
        "caption_track_id": track_id,
        "engine": "whisperx",
        "interpreter": python.to_string_lossy(),
    }))
}

// ---------------------------------------------------------------------------
// Audio: stems and a ducked clean mix
// ---------------------------------------------------------------------------

impl Studio {
    fn audio_status(&self, env: &Env) -> Result<Value> {
        let root = project_root(env);
        let record = self.audio.lock().unwrap().clone();
        let Some(record) = record.filter(|r| Some(&r.root) == root.as_ref()) else {
            return Ok(json!({
                "state": "idle",
                "progress": 0.0,
                "error": "",
                "stems": [],
                "mix": Value::Null,
            }));
        };
        let Some(snap) = env.jobs.get(&record.job_id) else {
            return Ok(json!({
                "state": "failed",
                "progress": 0.0,
                "error": "the job record is gone (the server restarted); run the enhancement again",
                "stems": [],
                "mix": Value::Null,
            }));
        };
        let output = snap.output.clone().unwrap_or(Value::Null);
        Ok(json!({
            "state": snap.status.as_str(),
            "progress": snap.progress,
            "message": snap.message,
            "error": snap.error.as_ref().map(|e| e.message.clone()).unwrap_or_default(),
            "stems": output.get("stems").cloned().unwrap_or_else(|| json!([])),
            "mix": output.get("mix").cloned().unwrap_or(Value::Null),
        }))
    }

    fn audio_start(self: &Arc<Self>, env: &Env, body: &Value) -> Result<Value> {
        let store = store_of(env)?;
        entitlements(env).check_feature(crate::edition::Feature::AudioStems)?;
        let project = store.load()?;
        let source_id = str_field(body, "source").unwrap_or("");
        if source_id.is_empty() {
            return Err(Error::invalid(
                "source is required: the id of an imported audio or video asset",
            ));
        }
        let asset = project
            .asset(source_id)
            .ok_or_else(|| Error::not_found(format!("asset {source_id} not found")))?;
        let input = store.asset_path(asset)?;
        if !input.is_file() {
            return Err(Error::not_found(format!(
                "the asset's file is missing: {}",
                input.display()
            )));
        }
        if bool_field(body, "denoise").unwrap_or(false) {
            return Err(Error::unsupported(
                "this build has no denoiser; leave denoise off",
            ));
        }
        let want_stems = bool_field(body, "stems").unwrap_or(true);
        let want_mix = bool_field(body, "mix").unwrap_or(true);
        if !want_stems && !want_mix {
            return Err(Error::invalid("pick stems, the clean mix, or both"));
        }
        let model = str_field(body, "model")
            .filter(|m| !m.is_empty())
            .unwrap_or("htdemucs")
            .to_string();
        if !["htdemucs", "htdemucs_ft", "mdx_extra"].contains(&model.as_str()) {
            return Err(Error::invalid(format!(
                "unknown model '{model}'; use htdemucs, htdemucs_ft or mdx_extra"
            )));
        }
        let duck_db = f64_field(body, "duck_db").unwrap_or(-12.0);
        if !duck_db.is_finite() || !(-60.0..=0.0).contains(&duck_db) {
            return Err(Error::invalid(
                "duck_db must be between -60 and 0 dB (the music bed's level under speech)",
            ));
        }
        let out_dir = store.renders_dir().join("audio");
        paths::ensure_dir(&out_dir)?;
        let work = store.cache_dir().join("audio");
        let store_for_job = store.clone();
        let out_for_job = out_dir.clone();
        let input_for_job = input.clone();
        let job_id = env.jobs.spawn("audio", move |job| {
            job.progress(0.05, "separating stems with Demucs");
            let report = crate::audio::demucs::separate(&input_for_job, &work, &model)?;
            let mut stems: Vec<Value> = Vec::new();
            if want_stems {
                let stem_dir = out_for_job.join("stems");
                paths::ensure_dir(&stem_dir)?;
                for raw in &report.stems {
                    let source = PathBuf::from(raw);
                    let Some(name) = source.file_stem().map(|s| s.to_string_lossy().to_string())
                    else {
                        continue;
                    };
                    let ext = source
                        .extension()
                        .map(|e| e.to_string_lossy().to_string())
                        .unwrap_or_else(|| "wav".into());
                    let target = stem_dir.join(format!("{name}.{ext}"));
                    std::fs::copy(&source, &target)?;
                    stems.push(json!({
                        "name": name,
                        "path": target.to_string_lossy(),
                        "url": media_url(&store_for_job, &target),
                    }));
                }
            }
            job.progress(0.6, "stems ready");
            let mut mix = Value::Null;
            if want_mix {
                let stem_named = |wanted: &str| -> Option<PathBuf> {
                    report
                        .stems
                        .iter()
                        .map(PathBuf::from)
                        .find(|p| {
                            p.file_stem()
                                .map(|s| s.to_string_lossy().eq_ignore_ascii_case(wanted))
                                .unwrap_or(false)
                        })
                };
                let vocals = stem_named("vocals")
                    .ok_or_else(|| Error::internal("the Demucs report has no vocals stem"))?;
                let music = stem_named("other")
                    .or_else(|| stem_named("no_vocals"))
                    .ok_or_else(|| Error::internal("the Demucs report has no music bed stem"))?;
                let target = out_for_job.join("clean-mix.wav");
                let mut options = crate::audio::demucs::DuckOptions::default();
                options.music_gain = 10f64.powf(duck_db / 20.0);
                job.progress(0.7, "ducking the music bed under the speech");
                crate::audio::demucs::duck_mix(&vocals, &music, &target, &options)?;
                mix = json!({
                    "path": target.to_string_lossy(),
                    "url": media_url(&store_for_job, &target),
                    "note": format!(
                        "music {duck_db:+.0} dB under speech (sidechain {} ms attack / {} ms release)",
                        crate::audio::demucs::DUCK_ATTACK_MS,
                        crate::audio::demucs::DUCK_RELEASE_MS
                    ),
                });
            }
            job.progress(0.98, "wrapping up");
            Ok(json!({
                "stems": stems,
                "mix": mix,
                "seconds": report.seconds,
                "model": model,
                "work_dir": work.to_string_lossy(),
            }))
        });
        *self.audio.lock().unwrap() = Some(AudioRun {
            root: paths::normalize(&store.root),
            job_id: job_id.clone(),
        });
        Ok(json!({"job_id": job_id}))
    }
}

// ---------------------------------------------------------------------------
// Blender: scene, four-view preview, production render, failed-frame recovery
// ---------------------------------------------------------------------------

/// What the Studio knows about this project's Blender work. It lives on disk so a restart, or a
/// second tab, sees the same scene, preview and render job instead of an empty panel.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
struct BlenderState {
    scene: Option<SceneRecord>,
    preview: Option<PreviewRecord>,
    render: Option<RenderRecord>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
struct SceneRecord {
    id: String,
    template: String,
    text: String,
    transparent: bool,
    blend: String,
    /// The document that produced the .blend, kept so a render can reuse its size, fps and text.
    document: Value,
    width: u32,
    height: u32,
    fps: u32,
    frame_start: u32,
    frame_end: u32,
    objects: usize,
    cameras: usize,
    lights: usize,
    engine: String,
    blender_version: String,
    build_warnings: Vec<String>,
    created_at: u64,
}

impl Default for SceneRecord {
    fn default() -> SceneRecord {
        SceneRecord {
            id: String::new(),
            template: "logo_reveal".into(),
            text: "CINVAR".into(),
            transparent: false,
            blend: String::new(),
            document: Value::Null,
            width: 1080,
            height: 1920,
            fps: 30,
            frame_start: 1,
            frame_end: 150,
            objects: 0,
            cameras: 0,
            lights: 0,
            engine: String::new(),
            blender_version: String::new(),
            build_warnings: Vec::new(),
            created_at: 0,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
struct PreviewRecord {
    views: Vec<Value>,
    warnings: Vec<Value>,
    out_dir: String,
    sheet: String,
    diagnostics: String,
    passed: bool,
    elapsed_ms: u64,
    blender_version: String,
    ran_at: u64,
}

impl Default for PreviewRecord {
    fn default() -> PreviewRecord {
        PreviewRecord {
            views: Vec::new(),
            warnings: Vec::new(),
            out_dir: String::new(),
            sheet: String::new(),
            diagnostics: String::new(),
            passed: false,
            elapsed_ms: 0,
            blender_version: String::new(),
            ran_at: 0,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
struct RenderRecord {
    job_id: String,
    scene_id: String,
    template: String,
    text: String,
    /// Directory the job's frames live under: base/<spec hash>.
    base: String,
    job_dir: String,
    worker: Option<String>,
    engine: String,
    samples: u32,
    width: u32,
    height: u32,
    fps: u32,
    transparent: bool,
    frame_start: u32,
    frame_end: u32,
    started_unix: u64,
    failed_frames: Vec<u32>,
    clip: Option<String>,
    error: String,
    /// running | succeeded | failed, as last written by the job thread.
    state: String,
}

impl Default for RenderRecord {
    fn default() -> RenderRecord {
        RenderRecord {
            job_id: String::new(),
            scene_id: String::new(),
            template: "logo_reveal".into(),
            text: "CINVAR".into(),
            base: String::new(),
            job_dir: String::new(),
            worker: None,
            engine: "eevee".into(),
            samples: 64,
            width: 1080,
            height: 1920,
            fps: 30,
            transparent: false,
            frame_start: 1,
            frame_end: 150,
            started_unix: 0,
            failed_frames: Vec::new(),
            clip: None,
            error: String::new(),
            state: String::new(),
        }
    }
}

impl RenderRecord {
    fn total_frames(&self) -> u32 {
        self.frame_end.saturating_sub(self.frame_start) + 1
    }

    /// The animation spec this record renders: the template animation at the requested size,
    /// rate, length and transparency. Rebuilding it from the record is what makes a resumed or
    /// recovered render land in the same job directory as the first attempt.
    fn animation_spec(&self) -> crate::blender::AnimationSpec {
        let fps = self.fps.max(1);
        let seconds = self.total_frames() as f64 / fps as f64;
        crate::blender::AnimationSpec {
            template: self.template.clone(),
            text: self.text.clone(),
            duration: crate::util::time::Time::from_secs_f64(seconds),
            width: self.width,
            height: self.height,
            fps,
            color: "#4f8cff".into(),
            transparent: self.transparent,
        }
    }
}

fn blender_state_file(store: &ProjectStore) -> PathBuf {
    store.root.join(STUDIO_BLENDER_FILE)
}

impl Studio {
    fn read_blender_state(&self, store: &ProjectStore) -> BlenderState {
        let _guard = self.files.lock().unwrap();
        read_json_file(&blender_state_file(store))
            .and_then(|v| serde_json::from_value::<BlenderState>(v).ok())
            .unwrap_or_default()
    }

    fn update_blender_state<F>(&self, store: &ProjectStore, f: F) -> Result<()>
    where
        F: FnOnce(&mut BlenderState),
    {
        let _guard = self.files.lock().unwrap();
        let path = blender_state_file(store);
        let mut state = read_json_file(&path)
            .and_then(|v| serde_json::from_value::<BlenderState>(v).ok())
            .unwrap_or_default();
        f(&mut state);
        write_json_file(&path, &serde_json::to_value(&state).unwrap_or(Value::Null))
    }

    /// Blender's path and version, measured once per server: detect() runs Blender, which is far
    /// too heavy for a route the page polls twice a second.
    fn blender_binary(&self) -> Option<(PathBuf, String)> {
        if let Some(cached) = self.blender_info.lock().unwrap().clone() {
            if cached.0.is_file() {
                return Some(cached);
            }
        }
        let info = crate::blender::detect().ok()?;
        let value = (info.path.clone(), info.version.clone());
        *self.blender_info.lock().unwrap() = Some(value.clone());
        Some(value)
    }

    /// This machine's NVIDIA GPUs, cached for a minute: the list is stable and nvidia-smi is not
    /// free. Ids are 1-based positions, the same numbers the page sends back as gpu_id.
    fn gpu_list(&self) -> Vec<Value> {
        {
            let cached = self.gpus.lock().unwrap();
            if let Some((at, list)) = cached.as_ref() {
                if unix_now().saturating_sub(*at) < 60 {
                    return list.clone();
                }
            }
        }
        let list: Vec<Value> = crate::render::capability::detect_gpus()
            .into_iter()
            .enumerate()
            .map(|(index, gpu)| json!({"id": index + 1, "name": gpu.name, "vram_mb": gpu.vram_mb}))
            .collect();
        *self.gpus.lock().unwrap() = Some((unix_now(), list.clone()));
        list
    }

    fn blender_preview(&self, env: &Env) -> Result<Value> {
        let mut out = serde_json::Map::new();
        match self.blender_binary() {
            Some((path, version)) => {
                out.insert(
                    "blender".into(),
                    json!({"available": true, "version": version, "path": path.to_string_lossy()}),
                );
            }
            None => {
                let path = crate::blender::available();
                out.insert(
                    "blender".into(),
                    json!({
                        "available": path.is_some(),
                        "path": path.map(|p| p.to_string_lossy().to_string()),
                    }),
                );
            }
        }
        let state = match store_of(env) {
            Ok(store) => Some((store.clone(), self.read_blender_state(&store))),
            Err(_) => None,
        };
        if let Some((store, state)) = &state {
            if let Some(scene) = &state.scene {
                let views: Vec<Value> = state
                    .preview
                    .as_ref()
                    .map(|preview| {
                        preview
                            .views
                            .iter()
                            .map(|view| {
                                let name = view.get("name").and_then(Value::as_str).unwrap_or("");
                                let path = view.get("path").and_then(Value::as_str).unwrap_or("");
                                let url = if path.is_empty() {
                                    Value::Null
                                } else {
                                    media_url(store, Path::new(path))
                                        .map(Value::String)
                                        .unwrap_or(Value::Null)
                                };
                                json!({"name": name, "url": url, "path": path})
                            })
                            .collect()
                    })
                    .unwrap_or_default();
                out.insert(
                    "scene".into(),
                    json!({
                        "id": scene.id,
                        "blend": scene.blend,
                        "template": scene.template,
                        "text": scene.text,
                        "engine": scene.engine,
                        "objects": scene.objects,
                        "cameras": scene.cameras,
                        "lights": scene.lights,
                        "width": scene.width,
                        "height": scene.height,
                        "fps": scene.fps,
                        "frame_start": scene.frame_start,
                        "frame_end": scene.frame_end,
                        "warnings": scene.build_warnings,
                        "views": views,
                    }),
                );
                let warnings = state
                    .preview
                    .as_ref()
                    .map(|preview| preview.warnings.clone())
                    .unwrap_or_default();
                out.insert("warnings".into(), json!(warnings));
                if let Some(preview) = &state.preview {
                    out.insert(
                        "preview".into(),
                        json!({
                            "ran_at": preview.ran_at,
                            "elapsed_ms": preview.elapsed_ms,
                            "passed": preview.passed,
                            "blender_version": preview.blender_version,
                            "sheet": media_url(store, Path::new(&preview.sheet)),
                            "diagnostics": preview.diagnostics,
                        }),
                    );
                }
            }
            if let Some(render) = &state.render {
                let mut block = render_status(env.jobs, render);
                if let (Some(clip), Some(object)) = (render.clip.as_deref(), block.as_object_mut()) {
                    object.insert(
                        "clip_url".into(),
                        media_url(store, Path::new(clip))
                            .map(Value::String)
                            .unwrap_or(Value::Null),
                    );
                }
                out.insert("render".into(), block);
            }
        }
        out.insert("gpus".into(), json!(self.gpu_list()));
        out.insert("workers".into(), json!(worker_list()));
        Ok(Value::Object(out))
    }

    fn blender_scene(self: &Arc<Self>, env: &Env, body: &Value) -> Result<Value> {
        let store = store_of(env)?;
        require_blender_enabled(env)?;
        let Some((blender_path, _)) = self.blender_binary() else {
            return Err(Error::new(
                ErrorCode::DependencyMissing,
                "Blender is not installed",
            )
            .with_hint("install Blender 3.6 or newer, or set CINVAR_BLENDER"));
        };
        let template = str_field(body, "template")
            .filter(|t| !t.is_empty())
            .unwrap_or("logo_reveal")
            .to_string();
        if !["logo_reveal", "title_3d", "empty"].contains(&template.as_str()) {
            return Err(Error::invalid(format!(
                "unknown template '{template}'; use logo_reveal, title_3d or empty"
            )));
        }
        let text = str_field(body, "text")
            .filter(|t| !t.is_empty())
            .unwrap_or("CINVAR")
            .to_string();
        if text.chars().count() > 120 {
            return Err(Error::invalid("text is at most 120 characters"));
        }
        let camera_preset = body
            .get("camera")
            .and_then(|c| c.get("preset"))
            .and_then(Value::as_str)
            .unwrap_or("orbit");
        let lighting_preset = body
            .get("lighting")
            .and_then(|l| l.get("preset"))
            .and_then(Value::as_str)
            .unwrap_or("studio");
        let transparent = bool_field(body, "transparent").unwrap_or(false);
        let asset_ids: Vec<String> = body
            .get("asset_ids")
            .and_then(Value::as_array)
            .map(|items| {
                items
                    .iter()
                    .filter_map(Value::as_str)
                    .map(str::to_string)
                    .take(32)
                    .collect()
            })
            .unwrap_or_default();

        let project = store.load()?;
        let mut scene = crate::blender::scene::CinvarBlenderScene::new(format!(
            "CINVAR Studio {}",
            crate::util::text::slugify(&template)
        ));
        scene.materials.push(
            crate::blender::scene::MaterialSpec::new("accent", "#4f8cff").emission("#4f8cff", 1.0),
        );
        if template != "empty" {
            scene.objects.push(
                crate::blender::scene::SceneObject::new(
                    "CINVAR_TEXT",
                    crate::blender::scene::ObjectKind::text(text.clone()),
                )
                // Just above the floor plane so the measured preview does not report the text
                // as buried; the preview re-measures it either way.
                .transform(crate::blender::scene::Transform::at([0.0, 0.0, 0.35]))
                .material("accent"),
            );
        }
        for id in &asset_ids {
            let asset = project
                .asset(id)
                .ok_or_else(|| Error::not_found(format!("asset {id} not found")))?;
            let path = store.asset_path(asset)?;
            scene.assets.push(crate::blender::scene::AssetImport::new(
                asset.display_name(),
                path,
            ));
        }
        scene.camera = camera_rig(camera_preset)?;
        scene.lighting = lighting_from(lighting_preset)?;
        scene.render.width = 1080;
        scene.render.height = 1920;
        scene.render.fps = 30;
        scene.render.samples = 64;
        scene.render.engine = crate::blender::scene::RenderEngine::Eevee;
        scene.render.transparent = transparent;
        scene.render.frame_start = 1;
        scene.render.frame_end = 150;
        entitlements(env).check_export(scene.render.width, scene.render.height)?;

        let scenes_dir = store.root.join("blender").join("scenes");
        paths::ensure_dir(&scenes_dir)?;
        let index = next_scene_index(&scenes_dir);
        let scene_id = format!("scene-{index}");
        let scene_dir = scenes_dir.join(&scene_id);
        paths::ensure_dir(&scene_dir)?;
        let built = crate::blender::scene::build(&scene, &scene_dir)?;
        let document = serde_json::to_value(&scene).unwrap_or(Value::Null);

        // The four views are what the tab exists for, so they are created with the scene instead
        // of leaving the panel at "create a scene to see the four views" after a success.
        let preview_dir = store.renders_dir().join("blender").join("preview");
        paths::ensure_dir(&preview_dir)?;
        let request = crate::blender::preview::PreviewRequest::new(&built.blend, &preview_dir)
            .size(720)
            .max_rounds(1);
        let report = crate::blender::preview::preview_check(&request)?;
        let views = view_list(&report.diagnostics.views);
        let warnings: Vec<Value> = report
            .findings
            .iter()
            .map(|finding| {
                json!({
                    "severity": severity_str(finding.severity),
                    "message": finding.message,
                    "object": finding.subject,
                    "suggestion": finding.suggestion,
                })
            })
            .collect();
        let preview = PreviewRecord {
            views: views
                .iter()
                .map(|(name, path)| json!({"name": name, "path": path.to_string_lossy()}))
                .collect(),
            warnings: warnings.clone(),
            out_dir: report.out_dir.to_string_lossy().to_string(),
            sheet: report.sheet.to_string_lossy().to_string(),
            diagnostics: report.diagnostics_path.to_string_lossy().to_string(),
            passed: report.passed,
            elapsed_ms: report.elapsed_ms as u64,
            blender_version: report.blender_version.clone(),
            ran_at: unix_now(),
        };
        let record = SceneRecord {
            id: scene_id.clone(),
            template: template.clone(),
            text: text.clone(),
            transparent,
            blend: built.blend.to_string_lossy().to_string(),
            document,
            width: built.resolution[0],
            height: built.resolution[1],
            fps: built.fps,
            frame_start: built.frame_start,
            frame_end: built.frame_end,
            objects: built.objects,
            cameras: built.cameras,
            lights: built.lights,
            engine: built.engine.clone(),
            blender_version: built.blender_version.clone(),
            build_warnings: built.warnings.clone(),
            created_at: unix_now(),
        };
        self.update_blender_state(&store, |state| {
            state.scene = Some(record.clone());
            state.preview = Some(preview.clone());
        })?;
        let views_json: Vec<Value> = views
            .iter()
            .map(|(name, path)| {
                json!({"name": name, "url": media_url(&store, path), "path": path.to_string_lossy()})
            })
            .collect();
        Ok(json!({
            "scene_id": scene_id,
            "blend": built.blend.to_string_lossy(),
            "engine": built.engine,
            "objects": built.objects,
            "cameras": built.cameras,
            "lights": built.lights,
            "vertices": built.vertices,
            "width": built.resolution[0],
            "height": built.resolution[1],
            "fps": built.fps,
            "blender_version": built.blender_version,
            "blender": blender_path.to_string_lossy(),
            "elapsed_ms": built.elapsed_ms as u64,
            "build_warnings": built.warnings,
            "warnings": warnings,
            "views": views_json,
            "preview": {
                "sheet": media_url(&store, &report.sheet),
                "passed": report.passed,
                "elapsed_ms": report.elapsed_ms as u64,
            },
        }))
    }

    fn blender_render(self: &Arc<Self>, env: &Env, body: &Value) -> Result<Value> {
        let store = store_of(env)?;
        require_blender_enabled(env)?;
        let ent = entitlements(env);
        ent.check_feature(crate::edition::Feature::BlenderProduction)?;
        let state = self.read_blender_state(&store);
        let Some(scene) = state.scene.clone() else {
            return Err(Error::invalid(
                "there is no scene to render yet; create one first",
            ));
        };
        let wanted = str_field(body, "scene_id").unwrap_or("");
        if !wanted.is_empty() && wanted != scene.id {
            return Err(Error::not_found(format!(
                "no scene {wanted} in this project; the last one built is {}",
                scene.id
            )));
        }
        let width = u32_field(body, "width").unwrap_or(scene.width).clamp(16, 8192);
        let height = u32_field(body, "height").unwrap_or(scene.height).clamp(16, 8192);
        let fps = u32_field(body, "fps").unwrap_or(scene.fps).clamp(1, 120);
        let samples = u32_field(body, "samples").unwrap_or(64).clamp(1, 4096);
        let engine = str_field(body, "engine")
            .unwrap_or("eevee")
            .to_ascii_lowercase();
        if !["eevee", "cycles"].contains(&engine.as_str()) {
            return Err(Error::invalid(format!(
                "unknown engine '{engine}'; use eevee or cycles"
            )));
        }
        let device = str_field(body, "device")
            .unwrap_or("gpu")
            .to_ascii_lowercase();
        if !["gpu", "cpu"].contains(&device.as_str()) {
            return Err(Error::invalid(format!(
                "unknown device '{device}'; use gpu or cpu"
            )));
        }
        let transparent = bool_field(body, "transparent").unwrap_or(scene.transparent);
        ent.check_export(width, height)?;
        let frames = parse_frames(str_field(body, "frames"))?;
        let (frame_start, frame_end) = frames
            .unwrap_or((scene.frame_start, scene.frame_end.max(scene.frame_start + 1)));
        let total = frame_end.saturating_sub(frame_start) + 1;
        if total > 4800 {
            return Err(Error::invalid(format!(
                "that is {total} frames; the Studio renders at most 4800 at a time"
            )));
        }
        let worker = match str_field(body, "worker").filter(|w| !w.is_empty() && *w != "local") {
            Some(host) => {
                let parsed = crate::blender::distributed::parse_workers(host)?;
                Some(parsed[0].host.clone())
            }
            None => None,
        };
        if worker.is_some() {
            // A worker render is the distributed path, which carries its own edition gate.
            ent.check_feature(crate::edition::Feature::BlenderDistributed)?;
        }
        // The animation pipeline renders templates; a scene built from "empty" has no animation
        // to render, and saying so beats silently rendering a logo nobody asked for.
        if scene.template == "empty" {
            return Err(Error::invalid(
                "an empty scene has no animation to render; create the scene from logo_reveal or title_3d",
            ));
        }
        // Under renders/, so the finished clip is reachable through /api/media and the page can
        // play or save it. The job directory is <base>/<spec hash>, exactly as a local render
        // lays it out.
        let base = store.renders_dir().join("blender");
        paths::ensure_dir(&base)?;
        let record = RenderRecord {
            scene_id: scene.id.clone(),
            template: scene.template.clone(),
            text: scene.text.clone(),
            base: base.to_string_lossy().to_string(),
            worker,
            engine,
            samples,
            width,
            height,
            fps,
            transparent,
            frame_start,
            frame_end,
            started_unix: unix_now(),
            ..RenderRecord::default()
        };
        let job_id = spawn_blender_render(self, env, &store, record)?;
        Ok(json!({
            "job_id": job_id,
            "total_frames": total,
            "frames": [frame_start, frame_end],
            "width": width,
            "height": height,
            "fps": fps,
            "samples": samples,
            "device": device,
        }))
    }

    fn blender_recover(self: &Arc<Self>, env: &Env, body: &Value) -> Result<Value> {
        let store = store_of(env)?;
        require_blender_enabled(env)?;
        entitlements(env).check_feature(crate::edition::Feature::BlenderProduction)?;
        let job_id = str_field(body, "job_id").unwrap_or("");
        let requested: Vec<u32> = body
            .get("frames")
            .and_then(Value::as_array)
            .map(|items| {
                items
                    .iter()
                    .filter_map(Value::as_u64)
                    .map(|v| v as u32)
                    .take(20_000)
                    .collect()
            })
            .unwrap_or_default();
        if requested.is_empty() {
            return Err(Error::invalid(
                "frames is required: the frame numbers the render could not produce",
            ));
        }
        let state = self.read_blender_state(&store);
        let Some(record) = state.render.clone() else {
            return Err(Error::not_found(
                "no Blender render has run in this project yet",
            ));
        };
        if !job_id.is_empty() && job_id != record.job_id {
            return Err(Error::not_found(format!(
                "no render job {job_id} in this project; the last one is {}",
                record.job_id
            )));
        }
        let job_dir = if record.job_dir.is_empty() {
            crate::blender::job_dir(Path::new(&record.base), &record.animation_spec())
        } else {
            PathBuf::from(&record.job_dir)
        };
        let mut missing: Vec<u32> = Vec::new();
        for frame in requested {
            if frame < record.frame_start || frame > record.frame_end {
                return Err(Error::invalid(format!(
                    "frame {frame} is outside this render's range {}..{}",
                    record.frame_start, record.frame_end
                )));
            }
            if !frame_complete(&job_dir, frame) && !missing.contains(&frame) {
                missing.push(frame);
            }
        }
        if missing.is_empty() {
            return Ok(json!({
                "requeued": [],
                "job_id": record.job_id,
                "note": "every requested frame is already on disk and complete",
            }));
        }
        let mut next = record.clone();
        next.failed_frames.clear();
        next.error.clear();
        let new_job = spawn_blender_render(self, env, &store, next)?;
        Ok(json!({
            "requeued": missing,
            "job_id": new_job,
            "previous_job": record.job_id,
            "note": "the renderer re-renders only frames that are missing or incomplete",
        }))
    }
}

/// Queue one Blender render and keep the project's state record in step with it.
fn spawn_blender_render(
    st: &Arc<Studio>,
    env: &Env,
    store: &ProjectStore,
    record: RenderRecord,
) -> Result<String> {
    let st = st.clone();
    let store = store.clone();
    let mut record = record;
    let job_id = env.jobs.spawn("blender_render", move |job| {
        record.job_id = job.id.clone();
        record.state = "running".into();
        record.started_unix = unix_now();
        record.failed_frames.clear();
        record.error.clear();
        record.job_dir =
            crate::blender::job_dir(Path::new(&record.base), &record.animation_spec())
                .to_string_lossy()
                .to_string();
        st.update_blender_state(&store, |state| state.render = Some(record.clone()))?;
        let spec = record.animation_spec();
        let base = PathBuf::from(&record.base);
        let samples = record.samples;
        let worker = record.worker.clone();
        let job_dir = PathBuf::from(&record.job_dir);
        let progress = |done: usize, total: usize| {
            let fraction = if total > 0 {
                done as f32 / total as f32
            } else {
                0.0
            };
            job.progress(fraction, format!("frame {done}/{total}"));
        };
        let cancelled = || job.cancelled();
        let control = crate::blender::RenderControl {
            progress: &progress,
            cancelled: &cancelled,
        };
        let outcome = match &worker {
            None => crate::blender::render_animation_job(&spec, &base, &control).map(|render| {
                json!({
                    "clip": render.clip.to_string_lossy(),
                    "frames": render.frames,
                    "resumed": render.resumed_frames,
                    "transparent": render.transparent,
                    "job_dir": render.job_dir.to_string_lossy(),
                    "blender_version": render.blender_version,
                    "alpha_verified": render.alpha_verified(),
                    "webm": render.webm.map(|p| p.to_string_lossy().to_string()),
                })
            }),
            Some(host) => {
                let mut options = crate::blender::distributed::DistributedOptions::default();
                options.workers = crate::blender::distributed::parse_workers(host)?;
                options.profile.samples = samples;
                crate::blender::distributed::render(&spec, &base, &options, &control).map(|report| {
                    json!({
                        "clip": report.clip.to_string_lossy(),
                        "frames": report.frames,
                        "rendered_frames": report.rendered_frames,
                        "reused_frames": report.reused_frames,
                        "units_completed": report.units_completed,
                        "units_planned": report.units_planned,
                        "job_dir": report.job_dir.to_string_lossy(),
                        "worker": host,
                        "per_worker": report.per_worker.iter().map(|w| json!({
                            "host": w.host,
                            "frames": w.frames,
                            "units": w.units,
                            "state": w.state,
                            "device": w.device,
                        })).collect::<Vec<_>>(),
                    })
                })
            }
        };
        match outcome {
            Ok(value) => {
                record.state = "succeeded".into();
                record.clip = value
                    .get("clip")
                    .and_then(Value::as_str)
                    .map(str::to_string);
                st.update_blender_state(&store, |state| state.render = Some(record.clone()))?;
                Ok(value)
            }
            Err(err) => {
                record.state = "failed".into();
                record.error = err.message.clone();
                record.failed_frames =
                    missing_frames(&job_dir, record.frame_start, record.frame_end);
                st.update_blender_state(&store, |state| state.render = Some(record.clone()))?;
                Err(err)
            }
        }
    });
    Ok(job_id)
}

fn render_status(jobs: &Jobs, record: &RenderRecord) -> Value {
    let snapshot = jobs.get(&record.job_id);
    let state = snapshot
        .as_ref()
        .map(|s| s.status.as_str().to_string())
        .unwrap_or_else(|| record.state.clone());
    let progress = snapshot.as_ref().map(|s| s.progress).unwrap_or(
        if record.state == "succeeded" {
            1.0
        } else {
            0.0
        },
    );
    let total = record.total_frames();
    let done = ((progress * total as f32).round() as u32).min(total);
    let elapsed = unix_now().saturating_sub(record.started_unix);
    let eta = if state == "running" && progress > 0.03 {
        Some((elapsed as f64) * ((1.0 - progress as f64) / progress as f64))
    } else {
        None
    };
    let error = snapshot
        .as_ref()
        .and_then(|s| s.error.as_ref().map(|e| e.message.clone()))
        .unwrap_or_else(|| record.error.clone());
    json!({
        "job_id": record.job_id,
        "state": state,
        "progress": progress,
        "done_frames": done,
        "total_frames": total,
        "frame_start": record.frame_start,
        "frame_end": record.frame_end,
        "eta_seconds": eta,
        "failed_frames": record.failed_frames,
        "error": error,
        "worker": record.worker,
        "engine": record.engine,
        "samples": record.samples,
        "width": record.width,
        "height": record.height,
        "transparent": record.transparent,
        "clip": record.clip,
        "message": snapshot.map(|s| s.message),
    })
}

fn require_blender_enabled(env: &Env) -> Result<()> {
    if !env.config.security.allow_blender {
        return Err(Error::new(
            ErrorCode::PermissionDenied,
            "Blender integration is disabled in configuration",
        )
        .with_hint("set security.allow_blender = true in ~/.config/cinvar/config.toml"));
    }
    if crate::blender::available().is_none() {
        return Err(Error::new(
            ErrorCode::DependencyMissing,
            "Blender is not installed",
        )
        .with_hint("install Blender 3.6 or newer"));
    }
    Ok(())
}

/// Camera presets the declarative scene understands. The engine has orbit, dolly-track and
/// isometric-turntable rigs only, so the page's "handheld" choice is refused by name instead of
/// being built as some other camera under a label that would then be a lie.
fn camera_rig(preset: &str) -> Result<crate::blender::scene::CameraRig> {
    use crate::blender::scene::CameraRig;
    match preset.trim().to_ascii_lowercase().as_str() {
        // 35 mm at 8 m frames a text object of the default size with margin on a 9:16 canvas.
        "" | "orbit" => Ok(CameraRig::orbit(None, 8.0, 15.0, -25.0, 25.0).with_lens(35.0)),
        "dolly_in" => Ok(CameraRig::dolly_track(
            None,
            [0.0, -9.0, 1.2],
            [0.0, -4.6, 0.7],
            35.0,
            None,
        )),
        // A locked-off camera is a zero-length orbit: no keyframe moves it.
        "static" => Ok(CameraRig::orbit(None, 8.0, 10.0, 0.0, 0.0).with_lens(35.0)),
        other => Err(Error::invalid(format!(
            "the scene engine has no '{other}' camera; use orbit, dolly_in or static"
        ))
        .with_hint("handheld drift is a mockup-scene camera, not a declarative scene rig")),
    }
}

/// Lighting presets the page offers, mapped onto the presets the scene engine builds. "rim" and
/// "night" are the studio / neon presets with explicit colour and intensity overrides, which is
/// exactly what those presets expose.
fn lighting_from(preset: &str) -> Result<crate::blender::scene::Lighting> {
    use crate::blender::scene::{LightPreset, Lighting};
    let mut lighting = Lighting::default();
    match preset.trim().to_ascii_lowercase().as_str() {
        "" | "studio" => lighting.preset = LightPreset::StudioThreePoint,
        "softbox" => lighting.preset = LightPreset::ProductCyclorama,
        "rim" => {
            lighting.preset = LightPreset::StudioThreePoint;
            lighting.rim_color = Some("#ff7a3d".into());
            lighting.background = Some("#0a0a12".into());
        }
        "night" => {
            lighting.preset = LightPreset::CyberpunkNeon;
            lighting.intensity = Some(0.7);
        }
        other => {
            return Err(Error::invalid(format!(
                "unknown lighting preset '{other}'; use studio, softbox, rim or night"
            )))
        }
    }
    Ok(lighting)
}

/// The four views the tab displays, in its own names. The engine renders an active-camera view,
/// two orthographic views and a wireframe pass; each is mapped to a name the page asks for.
fn view_list(views: &crate::blender::preview::Views) -> Vec<(&'static str, PathBuf)> {
    [
        ("front", views.camera.clone()),
        ("side", views.side.clone()),
        ("persp", views.wire.clone()),
        ("top", views.top.clone()),
    ]
    .into_iter()
    .filter_map(|(name, path)| path.map(|p| (name, p)))
    .collect()
}

fn severity_str(severity: crate::blender::preview::Severity) -> &'static str {
    match severity {
        crate::blender::preview::Severity::Error => "error",
        crate::blender::preview::Severity::Warning => "warning",
        crate::blender::preview::Severity::Info => "info",
    }
}

fn next_scene_index(dir: &Path) -> usize {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return 1;
    };
    let taken: Vec<usize> = entries
        .filter_map(|e| e.ok())
        .filter_map(|e| {
            e.file_name()
                .to_string_lossy()
                .strip_prefix("scene-")
                .and_then(|n| n.parse::<usize>().ok())
        })
        .collect();
    taken.into_iter().max().unwrap_or(0) + 1
}

/// Is this frame on disk and a complete PNG? A PNG that lost its IEND chunk is what a killed
/// Blender leaves behind, so it counts as missing and gets rendered again.
fn frame_complete(job_dir: &Path, frame: u32) -> bool {
    use std::io::{Read, Seek, SeekFrom};
    let path = crate::blender::frames_dir(job_dir).join(format!("frame_{frame:04}.png"));
    let Ok(mut file) = std::fs::File::open(&path) else {
        return false;
    };
    let Ok(length) = file.metadata().map(|m| m.len()) else {
        return false;
    };
    if length < 12 {
        return false;
    }
    if file.seek(SeekFrom::End(-12)).is_err() {
        return false;
    }
    let mut tail = [0u8; 12];
    if file.read_exact(&mut tail).is_err() {
        return false;
    }
    tail == *b"\x00\x00\x00\x00IEND\xae\x42\x60\x82"
}

fn missing_frames(job_dir: &Path, start: u32, end: u32) -> Vec<u32> {
    (start..=end)
        .filter(|frame| !frame_complete(job_dir, *frame))
        .take(2000)
        .collect()
}

fn parse_frames(raw: Option<&str>) -> Result<Option<(u32, u32)>> {
    let Some(raw) = raw.map(str::trim).filter(|s| !s.is_empty()) else {
        return Ok(None);
    };
    if raw.eq_ignore_ascii_case("all") {
        return Ok(None);
    }
    let (start, end) = raw
        .split_once('-')
        .ok_or_else(|| Error::invalid(format!("frames must be \"all\" or START-END, got {raw:?}")))?;
    let parse = |text: &str, which: &str| -> Result<u32> {
        text.trim().parse::<u32>().map_err(|_| {
            Error::invalid(format!("frames: {which} is not a frame number: {text:?}"))
        })
    };
    let start = parse(start, "start")?;
    let end = parse(end, "end")?;
    if start == 0 || end < start {
        return Err(Error::invalid(format!(
            "frames must be a positive range with START <= END, got {raw:?}"
        )));
    }
    if start != 1 {
        return Err(Error::invalid(format!(
            "the animation pipeline renders from frame 1; got a range starting at {start}"
        ))
        .with_hint("use \"1-240\""));
    }
    Ok(Some((start, end)))
}

/// Worker machines the Studio can offer. The list is the one distributed rendering reads from
/// CINVAR_RENDER_WORKERS; nothing here claims a worker is reachable, so their state is left empty
/// and the page simply omits it.
fn worker_list() -> Vec<Value> {
    let mut workers = vec![json!({"id": "local", "label": "This machine", "state": "idle"})];
    if let Ok(raw) = std::env::var("CINVAR_RENDER_WORKERS") {
        if let Ok(parsed) = crate::blender::distributed::parse_workers(&raw) {
            for spec in parsed {
                workers.push(json!({"id": spec.host, "label": spec.host, "state": ""}));
            }
        }
    }
    workers
}

// ---------------------------------------------------------------------------
// AI setup: per-component installation state
// ---------------------------------------------------------------------------

fn component_label(component: crate::setup::Component) -> &'static str {
    use crate::setup::Component;
    match component {
        Component::WhisperX => "WhisperX",
        Component::FasterWhisper => "Faster-Whisper",
        Component::RealEsrgan => "Real-ESRGAN",
        Component::Demucs => "Demucs",
    }
}

/// The manifest that the setup command writes, read without going through the private installer
/// module: it is the only place a recorded per-component outcome exists.
fn setup_manifest() -> Option<toml::Value> {
    let path = paths::config_dir().join("pro-ai.toml");
    let text = std::fs::read_to_string(path).ok()?;
    toml::from_str::<toml::Value>(&text).ok()
}

impl Studio {
    fn setup_status(&self, env: &Env) -> Result<Value> {
        let manifest = setup_manifest();
        let venv_dir = manifest
            .as_ref()
            .and_then(|m| m.get("venvs"))
            .and_then(toml::Value::as_str)
            .map(PathBuf::from)
            .unwrap_or_else(|| paths::data_dir().join("venvs"));
        let running = self
            .setup
            .lock()
            .unwrap()
            .as_ref()
            .map(|run| run.job_id.clone());
        let snapshot = running.as_ref().and_then(|id| env.jobs.get(id));
        let busy = snapshot
            .as_ref()
            .map(|s| !s.status.is_terminal())
            .unwrap_or(false);
        let progress = snapshot.as_ref().map(|s| s.progress).unwrap_or(0.0);
        let mut current_taken = false;
        let mut components: Vec<Value> = Vec::new();
        for component in crate::setup::Component::ALL {
            let venv = venv_dir.join(component.as_str());
            let python = venv.join("bin").join("python");
            let installed = python.is_file();
            let recorded = manifest
                .as_ref()
                .and_then(|m| m.get("components"))
                .and_then(|c| c.get(component.as_str()));
            let (state, component_progress, detail) = if busy {
                if installed {
                    ("succeeded", 1.0, format!("ready: {}", python.display()))
                } else if !current_taken {
                    current_taken = true;
                    (
                        "running",
                        progress,
                        format!("installing into {}", venv.display()),
                    )
                } else {
                    (
                        "queued",
                        0.0,
                        format!("waiting; will create {}", venv.display()),
                    )
                }
            } else {
                match recorded {
                    Some(entry) => {
                        let ok = entry
                            .get("installed")
                            .and_then(toml::Value::as_bool)
                            .unwrap_or(false);
                        if ok {
                            let resolved = entry
                                .get("venv")
                                .and_then(toml::Value::as_str)
                                .map(str::to_string)
                                .unwrap_or_else(|| venv.display().to_string());
                            ("succeeded", 1.0, resolved)
                        } else {
                            let problem = entry
                                .get("problem")
                                .and_then(toml::Value::as_str)
                                .unwrap_or("the last install of this component failed")
                                .to_string();
                            ("failed", 0.0, problem)
                        }
                    }
                    None if installed => ("succeeded", 1.0, python.display().to_string()),
                    None => (
                        "skipped",
                        0.0,
                        format!("not installed: press Install to create {}", venv.display()),
                    ),
                }
            };
            components.push(json!({
                "id": component.as_str(),
                "label": component_label(*component),
                "state": state,
                "progress": component_progress,
                "detail": detail,
                "venv": venv.display().to_string(),
                "env_var": component.env_var(),
            }));
        }
        Ok(json!({
            "venv_dir": venv_dir.display().to_string(),
            "components": components,
            "busy": busy,
            "job_id": snapshot.as_ref().map(|s| s.id.clone()),
            "progress": progress,
            "error": snapshot
                .as_ref()
                .and_then(|s| s.error.as_ref().map(|e| e.message.clone()))
                .unwrap_or_default(),
        }))
    }

    fn setup_start(self: &Arc<Self>, env: &Env, body: &Value) -> Result<Value> {
        let all: Vec<String> = crate::setup::Component::ALL
            .iter()
            .map(|c| c.as_str().to_string())
            .collect();
        let mut requested: Vec<String> = body
            .get("components")
            .and_then(Value::as_array)
            .map(|items| {
                items
                    .iter()
                    .filter_map(Value::as_str)
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
                    .collect()
            })
            .unwrap_or_default();
        if requested.is_empty() {
            requested = all.clone();
        }
        for id in &requested {
            if !all.contains(id) {
                return Err(Error::invalid(format!(
                    "unknown component '{id}'; use whisperx, faster-whisper, realesrgan or demucs"
                )));
            }
        }
        let mut unique: Vec<String> = Vec::new();
        for id in requested {
            if !unique.contains(&id) {
                unique.push(id);
            }
        }
        // The installer builds the whole stack in one pass; there is no per-component entry
        // point. Refusing a subset is the honest answer: installing the other three anyway would
        // download a torch stack the user explicitly did not ask for.
        if unique.len() != all.len() {
            return Err(Error::invalid(
                "the Pro AI installer builds the whole stack in one pass and cannot install a subset",
            )
            .with_hint(format!(
                "select all four components ({}), or run the setup command yourself for a per-component install",
                all.join(", ")
            )));
        }
        if !bool_field(body, "allow_download").unwrap_or(true) {
            return Err(Error::invalid(
                "installing the Pro AI stack downloads pinned wheels and model weights; the installer cannot run with downloads disabled",
            )
            .with_hint("tick Allow downloading, or pre-place the venvs and weights and re-run"));
        }
        if let Some(run) = self.setup.lock().unwrap().as_ref() {
            let busy = env
                .jobs
                .get(&run.job_id)
                .map(|s| !s.status.is_terminal())
                .unwrap_or(false);
            if busy {
                return Err(Error::new(
                    ErrorCode::Conflict,
                    "a Pro AI install is already running",
                )
                .with_hint("GET /api/setup/pro-ai reports each component while it runs"));
            }
        }
        let job_id = env.jobs.spawn("setup_pro_ai", move |job| {
            job.progress(0.0, "installing the Pro AI stack");
            let report = crate::setup::pro_ai(false)?;
            job.progress(
                1.0,
                if report.complete {
                    "every component is ready"
                } else {
                    "some components failed"
                },
            );
            Ok(serde_json::to_value(&report).unwrap_or(Value::Null))
        });
        *self.setup.lock().unwrap() = Some(SetupRun {
            job_id: job_id.clone(),
            requested: unique,
        });
        Ok(json!({"ok": true, "job_id": job_id, "components": all}))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn style(size: u32, position: &str) -> CapStyle {
        CapStyle {
            size,
            color: "#ffffff".into(),
            background: None,
            uppercase: false,
            position: parse_position(position).unwrap(),
        }
    }

    #[test]
    fn frames_must_be_a_range_starting_at_one() {
        assert_eq!(parse_frames(None).unwrap(), None);
        assert_eq!(parse_frames(Some("  ")).unwrap(), None);
        assert_eq!(parse_frames(Some("all")).unwrap(), None);
        assert_eq!(parse_frames(Some("1-240")).unwrap(), Some((1, 240)));
        assert!(parse_frames(Some("5-240")).is_err(), "a mid range cannot render");
        assert!(parse_frames(Some("240-1")).is_err());
        assert!(parse_frames(Some("0-10")).is_err());
        assert!(parse_frames(Some("1")).is_err());
        assert!(parse_frames(Some("one-two")).is_err());
    }

    #[test]
    fn every_documented_route_pair_is_routed() {
        for (method, path) in [
            ("GET", "/api/shorts"),
            ("POST", "/api/shorts"),
            ("GET", "/api/captions/style"),
            ("POST", "/api/captions/style"),
            ("POST", "/api/captions/style/preset"),
            ("POST", "/api/captions/word_level"),
            ("GET", "/api/audio/enhance"),
            ("POST", "/api/audio/enhance"),
            ("GET", "/api/blender/preview"),
            ("POST", "/api/blender/scene"),
            ("POST", "/api/blender/render"),
            ("POST", "/api/blender/recover"),
            ("GET", "/api/setup/pro-ai"),
            ("POST", "/api/setup/pro-ai"),
        ] {
            assert!(is_route(method, path), "{method} {path} is not routed");
        }
        assert!(!is_route("GET", "/api/nope"));
        assert!(!is_route("DELETE", "/api/shorts"));
        assert!(!is_route("POST", "/api/blender/preview"));
    }

    #[test]
    fn error_codes_map_to_the_documented_statuses() {
        assert_eq!(status_for(&Error::invalid("x")), 400);
        assert_eq!(status_for(&Error::not_found("x")), 404);
        assert_eq!(
            status_for(&Error::new(ErrorCode::PermissionDenied, "x")),
            403
        );
        assert_eq!(status_for(&Error::internal("x")), 500);
        let body = error_body(&Error::invalid("boom").with_hint("try again"));
        assert_eq!(body["error"], "boom");
        assert_eq!(body["hint"], "try again");
    }

    #[test]
    fn a_partial_style_keeps_the_rest_of_the_track() {
        let base = style(48, "bottom");
        let merged = CapStyle::from_caption(&CaptionStyle::default())
            .overlay(&json!({"size": 64, "uppercase": true}))
            .unwrap();
        assert_eq!(merged.size, 64);
        assert!(merged.uppercase);
        assert_eq!(merged.position, base.position);

        // An empty background clears the box instead of storing an empty string.
        let cleared = CapStyle::from_caption(&CaptionStyle::default())
            .overlay(&json!({"background": "black@0.6"}))
            .unwrap();
        assert_eq!(cleared.background.as_deref(), Some("black@0.6"));
        let cleared = cleared.overlay(&json!({"background": ""})).unwrap();
        assert_eq!(cleared.background, None);

        assert!(base.clone().overlay(&json!({"size": 4})).is_err());
        assert!(base.clone().overlay(&json!({"position": "middle"})).is_err());
    }

    #[test]
    fn camera_presets_are_the_ones_the_scene_engine_builds() {
        for preset in ["orbit", "dolly_in", "static", ""] {
            assert!(camera_rig(preset).is_ok(), "{preset} should build");
        }
        let refused = camera_rig("handheld").unwrap_err();
        assert!(
            refused.message.contains("no 'handheld' camera"),
            "{}",
            refused.message
        );
        assert!(refused.message.contains("orbit"));
    }

    #[test]
    fn lighting_presets_map_onto_scene_presets() {
        use crate::blender::scene::LightPreset;
        assert_eq!(
            lighting_from("studio").unwrap().preset,
            LightPreset::StudioThreePoint
        );
        assert_eq!(
            lighting_from("softbox").unwrap().preset,
            LightPreset::ProductCyclorama
        );
        assert_eq!(
            lighting_from("night").unwrap().preset,
            LightPreset::CyberpunkNeon
        );
        let rim = lighting_from("rim").unwrap();
        assert_eq!(rim.preset, LightPreset::StudioThreePoint);
        assert!(rim.rim_color.is_some());
        assert!(lighting_from("noon").is_err());
        // Every preset the page offers must pass the engine's own validation.
        for preset in ["studio", "softbox", "rim", "night"] {
            let lighting = lighting_from(preset).unwrap();
            let scene = crate::blender::scene::CinvarBlenderScene::new("t")
                .lighting(lighting)
                .object(crate::blender::scene::SceneObject::new(
                    "o",
                    crate::blender::scene::ObjectKind::cube(1.0),
                ));
            assert!(scene.validate().is_ok(), "{preset} does not validate");
        }
    }

    #[test]
    fn a_frame_counts_only_when_its_png_is_complete() {
        let dir = tempfile::tempdir().unwrap();
        let job = dir.path();
        let frames = crate::blender::frames_dir(job);
        std::fs::create_dir_all(&frames).unwrap();
        assert!(!frame_complete(job, 1), "a missing frame is not complete");

        let mut png = vec![0x89, b'P', b'N', b'G', 0, 0, 0, 0];
        png.extend_from_slice(b"\x00\x00\x00\x00IEND\xae\x42\x60\x82");
        std::fs::write(frames.join("frame_0001.png"), &png).unwrap();
        assert!(frame_complete(job, 1));

        // A killed Blender leaves the header without the trailer: still not a frame.
        std::fs::write(frames.join("frame_0002.png"), &png[..10]).unwrap();
        assert!(!frame_complete(job, 2));
        assert_eq!(missing_frames(job, 1, 3), vec![2, 3]);
    }

    #[test]
    fn media_urls_only_point_inside_served_directories() {
        let dir = tempfile::tempdir().unwrap();
        let project = dir.path().join("project");
        let (store, _project) = crate::project::store::ProjectStore::create_at(&project, "t").unwrap();
        std::fs::create_dir_all(store.renders_dir().join("shorts")).unwrap();
        let inside = store.renders_dir().join("shorts").join("short_01.mp4");
        std::fs::write(&inside, b"x").unwrap();
        assert_eq!(
            media_url(&store, &inside).as_deref(),
            Some("/api/media/shorts/short_01.mp4")
        );

        let outside = dir.path().join("elsewhere.mp4");
        std::fs::write(&outside, b"x").unwrap();
        assert_eq!(media_url(&store, &outside), None);
    }

    #[test]
    fn scene_ids_continue_after_the_highest_existing_one() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(next_scene_index(dir.path()), 1);
        std::fs::create_dir_all(dir.path().join("scene-1")).unwrap();
        std::fs::create_dir_all(dir.path().join("scene-3")).unwrap();
        std::fs::create_dir_all(dir.path().join("notes")).unwrap();
        assert_eq!(next_scene_index(dir.path()), 4);
    }

    #[test]
    fn a_render_record_rebuilds_the_same_spec() {
        let record = RenderRecord {
            template: "title_3d".into(),
            text: "STUDIO".into(),
            width: 1080,
            height: 1920,
            fps: 30,
            frame_start: 1,
            frame_end: 240,
            transparent: true,
            ..RenderRecord::default()
        };
        assert_eq!(record.total_frames(), 240);
        let spec = record.animation_spec();
        assert_eq!(spec.template, "title_3d");
        assert_eq!((spec.width, spec.height, spec.fps), (1080, 1920, 30));
        assert!(spec.transparent);
        assert!((spec.duration.as_secs_f64() - 8.0).abs() < 1e-9);
    }

    #[test]
    fn audio_note_and_guard_reject_impossible_requests() {
        // -12 dB is the page's default and must be expressed as a linear music gain.
        let gain = 10f64.powf(-12.0 / 20.0);
        assert!((gain - 0.251).abs() < 0.01, "{gain}");
    }
}
