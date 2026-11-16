//! Warm-worker backend for [preview_check]: the same measurements, without paying Blender
//! process start per call.
//!
//! [super::preview_check] used to spawn "blender -b --factory-startup scene.blend --python driver"
//! for every call, so every inspection paid process start plus a scene load before a single
//! measurement was taken. The warm worker ([crate::blender::worker]) keeps one headless Blender
//! alive and owns a loaded scene, so a repeated inspection only pays the measurement, the four
//! Workbench panels and the contact sheet.
//!
//! # What this module does, in order
//!
//! 1. Identify the .blend by canonical path, byte length and mtime. The worker's job id is
//!    derived from that identity, so the same file maps to the same job and a repeated call
//!    never trips the worker's job-boundary reset (a *different* file gets a different job, and
//!    the reset that follows is the correct behaviour).
//! 2. If this process has already loaded that exact file, ask the worker what it is holding
//!    ("objects", one bounded round trip) and compare the reported .blend path and object count.
//!    Matching means the scene is still loaded and the reload is skipped; anything else (a dead
//!    and restarted worker, another job having reset the scene, the file changed on disk) means
//!    load again. No measurement is ever taken from a scene this module cannot identify.
//! 3. Run the measurement SDK in the worker ("measure"), which produces diagnostics.json and the
//!    four view panels exactly as the cold driver does, from the same SDK source.
//! 4. Assemble the 1024x1024 contact sheet with the same ffmpeg filtergraph as the cold path.
//!
//! The diagnostics are *the same computation* the cold path runs (cinvar_bpy_sdk.collect and
//! render_views); this module adds no measurement of its own and removes none.

use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Instant, UNIX_EPOCH};

use serde_json::Value;

use crate::error::{Error, ErrorCode, Result};
use crate::util::paths;

use super::{
    assemble_contact_sheet, read_json, sheet_path, Diagnostics, FrameChoice, PreviewBackend,
    PreviewReport, PreviewRequest,
};
use crate::blender::worker::{self, MeasureOptions, WorkerRequest};

/// One scene this process has loaded into the worker, and what it looked like on disk.
#[derive(Debug, Clone)]
struct Loaded {
    blend: PathBuf,
    bytes: u64,
    modified_ns: u128,
    objects: u64,
    blender_version: String,
}

impl Loaded {
    fn matches(&self, blend: &Path, bytes: u64, modified_ns: u128) -> bool {
        self.blend == blend && self.bytes == bytes && self.modified_ns == modified_ns
    }
}

/// The single scene this process believes is loaded. One entry is enough: the worker holds one
/// scene at a time, so remembering more would only remember scenes that are already gone.
static LOADED: Mutex<Option<Loaded>> = Mutex::new(None);

/// How long an unusable worker is left alone before it is tried again. Without this, a worker
/// that cannot start (a bad socket directory, a missing Blender, a sandbox denial) would make
/// *every* inspection pay the worker's start timeout before falling back, which is worse than
/// the cold path it falls back to. One failed attempt per minute bounds that.
const RETRY_AFTER: std::time::Duration = std::time::Duration::from_secs(60);

/// The last warm failure and when it happened, for the retry window above.
static UNAVAILABLE: Mutex<Option<(std::time::Instant, String)>> = Mutex::new(None);

/// Takes one of the statics above, recovering from a poisoned lock instead of panicking: a
/// backend that remembers the wrong thing is better than a preview that dies.
fn lock<T>(cell: &'static Mutex<T>) -> std::sync::MutexGuard<'static, T> {
    cell.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn unavailable() -> Option<(std::time::Instant, String)> {
    lock(&UNAVAILABLE).clone()
}

/// Why the warm backend is being skipped right now, if it is.
pub(super) fn cooldown_reason() -> Option<String> {
    let (when, reason) = unavailable()?;
    if when.elapsed() < RETRY_AFTER {
        return Some(format!(
            "the Blender worker failed {:.0}s ago and is not retried for another {:.0}s: {reason}",
            when.elapsed().as_secs_f64(),
            (RETRY_AFTER - when.elapsed()).as_secs_f64()
        ));
    }
    None
}

/// Remembers why the warm backend failed, so [cooldown_reason] can hold it off for a while.
pub(super) fn record_failure(err: &Error) {
    let reason = format!("{:?}: {err}", err.code);
    *lock(&UNAVAILABLE) = Some((std::time::Instant::now(), reason));
}

/// The worker answered, so nothing is held off.
pub(super) fn record_success() {
    *lock(&UNAVAILABLE) = None;
}

fn cache_get() -> Option<Loaded> {
    lock(&LOADED).clone()
}

fn cache_set(entry: Option<Loaded>) {
    *lock(&LOADED) = entry;
}

/// Canonical path, byte length and mtime of the file a request names.
fn identity(blend: &Path) -> Result<(PathBuf, u64, u128)> {
    let canonical = std::fs::canonicalize(blend).map_err(|e| {
        Error::not_found(format!("no such .blend file: {} ({e})", blend.display()))
    })?;
    let meta = std::fs::metadata(&canonical)?;
    // A filesystem without mtime support yields 0 here: the identity is then path plus length,
    // and the load probe below still proves which scene the worker is holding.
    let modified_ns = meta
        .modified()
        .ok()
        .and_then(|stamp| stamp.duration_since(UNIX_EPOCH).ok())
        .map(|age| age.as_nanos())
        .unwrap_or_default();
    Ok((canonical, meta.len(), modified_ns))
}

/// Stable job id for one file identity and one measurement SDK. Pure, so the same file always
/// maps to the same job in every process, and a rewritten file (or a changed SDK) maps to a new
/// one -- a scene loaded under one SDK identity is never measured as if it were the other.
fn job_name(blend: &Path, bytes: u64, modified_ns: u128, sdk: &str) -> String {
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(
        format!("preview:{sdk}:{}:{bytes}:{modified_ns}", blend.display()).as_bytes(),
    );
    let mut name = String::from("preview-");
    for byte in digest.iter().take(6) {
        name.push_str(&format!("{byte:02x}"));
    }
    name
}

/// The frame argument the worker's measure op understands ("first" | "middle" | "final" | a
/// number), which is the same vocabulary the cold job document uses.
fn frame_arg(frame: FrameChoice) -> String {
    match frame {
        FrameChoice::First => "first".to_string(),
        FrameChoice::Middle => "middle".to_string(),
        FrameChoice::Final => "final".to_string(),
        FrameChoice::Exact(number) => number.to_string(),
    }
}

/// Asks the worker what it is holding and whether that is still the scene we think it is.
fn still_loaded(blend: &Path, job: &str, objects: u64) -> bool {
    let Ok(response) = worker::submit(WorkerRequest::objects().in_job(job)) else {
        return false;
    };
    let Ok(result) = response.result() else {
        return false;
    };
    let held = result.get("blend").and_then(Value::as_str);
    let count = result.get("object_count").and_then(Value::as_u64);
    match (held, count) {
        (Some(held), Some(count)) => Path::new(held) == blend && count == objects,
        _ => false,
    }
}

fn worker_version(job: &str) -> Result<String> {
    let response = worker::submit(WorkerRequest::version().in_job(job))?;
    let value = response.into_result()?;
    Ok(value
        .get("blender")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string())
}

/// Loads the scene into the worker and records it as the loaded identity.
fn load(
    blend: &Path,
    job: &str,
    bytes: u64,
    modified_ns: u128,
    known_version: Option<String>,
) -> Result<Loaded> {
    let response = worker::submit(WorkerRequest::load(blend).in_job(job))?;
    let value = response.into_result()?;
    let objects = value
        .get("object_count")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let blender_version = match known_version {
        Some(version) if !version.is_empty() => version,
        _ => worker_version(job)?,
    };
    let entry = Loaded {
        blend: blend.to_path_buf(),
        bytes,
        modified_ns,
        objects,
        blender_version,
    };
    cache_set(Some(entry.clone()));
    Ok(entry)
}

fn measure_request(req: &PreviewRequest, job: &str) -> WorkerRequest {
    WorkerRequest::measure(
        &req.out_dir,
        &MeasureOptions {
            frame: Some(frame_arg(req.frame)),
            floor_z: req.floor_z,
            // The four diagnostic panels are the point of a preview: always rendered.
            render_views: true,
            size: req.size,
        },
    )
    .in_job(job)
}

/// Runs one inspection through the warm worker. Errors (never panics) when the worker is
/// unavailable, so [super::preview_check] can fall back to the cold path.
pub(super) fn check(req: &PreviewRequest) -> Result<PreviewReport> {
    let started = Instant::now();
    paths::ensure_dir(&req.out_dir)?;
    let (blend, bytes, modified_ns) = identity(&req.blend)?;
    let job = job_name(&blend, bytes, modified_ns, &super::sdk_hash());

    // Everything paid to Blender, measured together: worker start, scene verification, load and
    // measurement. The contact sheet is timed on its own.
    let blender_started = Instant::now();
    let mut scene_load_ms = 0.0;
    // A ping never starts a worker and never resets the scene, so it is safe to ask first: it
    // tells us whether this call pays the one-time worker start, and it carries the version.
    let status = worker::status().unwrap_or_default();
    let worker_started = !status.running;

    let cached = cache_get();
    let mut scene_reused = false;
    let mut entry = None;
    // The ping already says which job owns the worker: if it is not ours, no round trip is owed
    // to prove it, and the scene has to be loaded either way.
    let owns_worker = status.job.as_deref() == Some(job.as_str());
    if let Some(existing) = cached
        .as_ref()
        .filter(|existing| existing.matches(&blend, bytes, modified_ns))
    {
        if owns_worker && still_loaded(&blend, &job, existing.objects) {
            scene_reused = true;
            entry = Some(existing.clone());
        }
    }
    let entry = match entry {
        Some(entry) => entry,
        None => {
            let load_started = Instant::now();
            let entry = load(&blend, &job, bytes, modified_ns, status.blender.clone())?;
            scene_load_ms = load_started.elapsed().as_secs_f64() * 1000.0;
            entry
        }
    };
    let blender_version = entry.blender_version;

    let mut response = worker::submit(measure_request(req, &job))?;
    if response.recovered {
        // The worker died and was restarted for this request, so whatever it measured was the
        // factory-empty scene that restart left behind. Load again and measure once more rather
        // than reporting diagnostics for a scene nobody asked about.
        load(&blend, &job, bytes, modified_ns, None)?;
        response = worker::submit(measure_request(req, &job))?;
        if response.recovered {
            return Err(Error::new(
                ErrorCode::Provider,
                "the Blender worker restarted twice while measuring, so its diagnostics are not trustworthy",
            ));
        }
        scene_reused = false;
    }
    let blender_ms = blender_started.elapsed().as_secs_f64() * 1000.0;
    response.into_result()?;

    let diagnostics_path = req.out_dir.join("diagnostics.json");
    let mut diagnostics: Diagnostics = read_json(&diagnostics_path)?;
    if diagnostics.views.camera.is_none()
        && diagnostics.views.top.is_none()
        && diagnostics.views.side.is_none()
        && diagnostics.views.wire.is_none()
    {
        return Err(Error::new(
            ErrorCode::Render,
            "the worker measured the scene but rendered no diagnostic panels",
        ));
    }
    // The panels must belong to this call's out_dir; a stale path from a previous run would
    // silently ship someone else's picture.
    for view in [
        &mut diagnostics.views.camera,
        &mut diagnostics.views.top,
        &mut diagnostics.views.side,
        &mut diagnostics.views.wire,
    ]
    .into_iter()
    .flatten()
    {
        if !view.is_file() {
            return Err(Error::new(
                ErrorCode::Render,
                format!("the worker reported a missing panel: {}", view.display()),
            ));
        }
    }

    let findings = diagnostics.findings();
    let passed = diagnostics.passed();
    let sheet_started = Instant::now();
    let sheet = assemble_contact_sheet(&diagnostics.views, &sheet_path(&req.out_dir))?;
    let sheet_ms = sheet_started.elapsed().as_secs_f64() * 1000.0;

    Ok(PreviewReport {
        sheet,
        diagnostics,
        findings,
        passed,
        elapsed_ms: started.elapsed().as_millis(),
        blender_version,
        diagnostics_path,
        out_dir: req.out_dir.clone(),
        backend: PreviewBackend::Warm,
        worker_started,
        scene_reused,
        blender_ms,
        scene_load_ms,
        sheet_ms,
        fallback: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_file_identity_maps_to_one_stable_job() {
        let blend = Path::new("/tmp/scene.blend");
        let job = job_name(blend, 1024, 7, "aaaa0000bbbb1111");
        assert_eq!(job, job_name(blend, 1024, 7, "aaaa0000bbbb1111"));
        assert!(job.starts_with("preview-"), "{job}");
        assert_eq!(job.len(), "preview-".len() + 12);
        // A different file, a different length, a different mtime or a different measurement SDK
        // is a different identity, and must therefore never reuse the previous job (which would
        // skip the reload and measure one scene as if it were the other).
        assert_ne!(job, job_name(Path::new("/tmp/other.blend"), 1024, 7, "aaaa0000bbbb1111"));
        assert_ne!(job, job_name(blend, 2048, 7, "aaaa0000bbbb1111"));
        assert_ne!(job, job_name(blend, 1024, 8, "aaaa0000bbbb1111"));
        assert_ne!(job, job_name(blend, 1024, 7, "cccc2222dddd3333"));
    }

    #[test]
    fn frame_arguments_use_the_worker_vocabulary() {
        assert_eq!(frame_arg(FrameChoice::First), "first");
        assert_eq!(frame_arg(FrameChoice::Middle), "middle");
        assert_eq!(frame_arg(FrameChoice::Final), "final");
        assert_eq!(frame_arg(FrameChoice::Exact(45)), "45");
    }

    #[test]
    fn a_loaded_scene_only_matches_the_file_it_was_loaded_from() {
        let entry = Loaded {
            blend: PathBuf::from("/tmp/scene.blend"),
            bytes: 1024,
            modified_ns: 7,
            objects: 5,
            blender_version: "4.0.2".into(),
        };
        assert!(entry.matches(Path::new("/tmp/scene.blend"), 1024, 7));
        assert!(!entry.matches(Path::new("/tmp/scene.blend"), 1024, 8));
        assert!(!entry.matches(Path::new("/tmp/scene.blend"), 2048, 7));
        assert!(!entry.matches(Path::new("/tmp/other.blend"), 1024, 7));
    }

    #[test]
    fn missing_blend_is_rejected_before_the_worker_is_asked() {
        let dir = tempfile::tempdir().unwrap();
        let err = check(&PreviewRequest::new(dir.path().join("nope.blend"), dir.path())).unwrap_err();
        assert_eq!(err.code, ErrorCode::NotFound);
    }
}
