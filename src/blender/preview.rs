//! Closed-loop spatial and visual verification for Blender scenes.
//!
//! One Blender invocation measures the scene it just loaded (matrices, camera projection,
//! BVHTree overlaps, ray casts, rendered pixels), renders four diagnostic panels, and
//! writes JSON. CINVAR then turns those measurements into findings with messages and
//! numeric corrections, assembles the 1024x1024 contact sheet with ffmpeg, and can run a
//! bounded correction loop that stops as soon as the diagnostics pass. The loop only makes
//! numeric, reversible repairs (camera distance for framing, object height for floor
//! penetration); collisions and occlusions are reported for an editorial decision instead of
//! silently moving the caller's geometry.
//!
//! The measurements are produced by the embedded bpy SDK (cinvar_bpy_sdk.py); nothing in
//! this module estimates geometry.
//!
//! # Two backends, one set of measurements
//!
//! [preview_check] runs the same SDK through either of two backends and reports which one it
//! used. The warm backend ([preview_warm]) keeps one headless Blender alive (see
//! [crate::blender::worker]) and reuses the loaded scene, so only the measurements, the four
//! Workbench panels and the contact sheet are paid per call. The cold backend spawns a private
//! "blender -b --factory-startup" per call. The warm backend is tried first and the cold one is
//! the fallback, so a machine without a working worker behaves exactly as it did before. The
//! fallback is stated in [PreviewReport::fallback], the failed attempt stays inside
//! [PreviewReport::elapsed_ms], and an unusable worker is left alone for a minute before the next
//! attempt so that fallback is not repaid on every call. CINVAR_PREVIEW_BACKEND (auto, the
//! default; cold; warm) pins the choice, as does [preview_check_with] per call.
//!
//! The panel size is the one knob that changes per-call cost: the four panels are rendered at
//! [PreviewRequest::size] and then scaled into the [SHEET_SIZE] sheet. Measured on the test scene
//! (Blender 4.0.2, RTX 3060 Ti, warm worker with the scene already loaded), the measure op takes
//! 527 ms with 512 px panels, 380 ms at 384 px and 361 ms at 320 px, against 5 ms for the
//! measurements alone. The default stays [DEFAULT_VIEW_SIZE] (512) so the diagnostics are the same
//! pixels they were and each 1024 sheet cell is written without an upscale; a caller that wants a
//! cheaper preview asks for a smaller size.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::{Error, ErrorCode, Result};
use crate::util::paths;
use crate::util::proc::{self, ProcOpts};

use super::detect;

#[path = "preview_warm.rs"]
mod warm;

/// Rendered size of one contact-sheet panel; four of them make up the sheet.
pub const DEFAULT_VIEW_SIZE: u32 = 512;
/// Diagnostic contact sheet edge length.
pub const SHEET_SIZE: u32 = 1024;
/// Correction rounds the closed loop runs when the caller does not say.
pub const DEFAULT_MAX_ROUNDS: u32 = 3;
const BLENDER_TIMEOUT_SECS: u64 = 900;
const MAX_STDERR_LINES: usize = 25;

/// Share of an object's bounding box outside the camera frame that is worth reporting.
pub const OFF_FRAME_WARN_PCT: f64 = 10.0;
/// Below this share inside the frame an object counts as gone.
pub const OFF_FRAME_ERROR_PCT: f64 = 99.5;
pub const FLOOR_ERROR_M: f64 = 0.01;
pub const OCCLUSION_WARN_PCT: f64 = 50.0;
pub const EXPOSURE_WARN_PCT: f64 = 15.0;
/// A blank render measures exactly zero coverage; anything above this is a real subject.
pub const EMPTY_COVERAGE: f64 = 0.001;

/// The bpy measurement SDK, embedded so a packaged binary needs no data files.
pub fn sdk_source() -> &'static str {
    include_str!("cinvar_bpy_sdk.py")
}

/// Identity of the measurement SDK, so a stale diagnostics file is never mistaken for current.
pub fn sdk_hash() -> String {
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(sdk_source().as_bytes());
    digest.iter().take(8).map(|b| format!("{b:02x}")).collect()
}

const DRIVER: &str = "import json, os, sys
HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)
import cinvar_bpy_sdk as sdk
with open(os.path.join(HERE, \"cinvar_preview_job.json\"), \"r\", encoding=\"utf-8\") as handle:
    JOB = json.load(handle)
sdk.run(JOB)
";

/// Which frame of the scene the diagnostics describe.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FrameChoice {
    First,
    Middle,
    Final,
    Exact(u32),
}

impl FrameChoice {
    fn job_value(self) -> serde_json::Value {
        match self {
            FrameChoice::First => serde_json::json!("first"),
            FrameChoice::Middle => serde_json::json!("middle"),
            FrameChoice::Final => serde_json::json!("final"),
            FrameChoice::Exact(frame) => serde_json::json!(frame),
        }
    }
}

/// One preview request: a .blend file, where the artifacts go, and how hard to try.
#[derive(Debug, Clone)]
pub struct PreviewRequest {
    pub blend: PathBuf,
    pub out_dir: PathBuf,
    pub frame: FrameChoice,
    /// Per-panel render size (the sheet is always SHEET_SIZE).
    pub size: u32,
    /// 1 means inspect only; more only matters for preview_correct.
    pub max_rounds: u32,
    /// World height treated as the floor plane.
    pub floor_z: f64,
}

impl PreviewRequest {
    pub fn new(blend: impl Into<PathBuf>, out_dir: impl Into<PathBuf>) -> Self {
        PreviewRequest {
            blend: blend.into(),
            out_dir: out_dir.into(),
            frame: FrameChoice::Middle,
            size: DEFAULT_VIEW_SIZE,
            max_rounds: DEFAULT_MAX_ROUNDS,
            floor_z: 0.0,
        }
    }

    pub fn frame(mut self, frame: FrameChoice) -> Self {
        self.frame = frame;
        self
    }

    pub fn size(mut self, size: u32) -> Self {
        self.size = size.clamp(256, 2048);
        self
    }

    pub fn max_rounds(mut self, rounds: u32) -> Self {
        self.max_rounds = rounds.clamp(1, 10);
        self
    }

    pub fn floor_z(mut self, floor_z: f64) -> Self {
        self.floor_z = floor_z;
        self
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SceneInfo {
    pub name: String,
    pub engine: String,
    pub resolution: [u32; 2],
    pub fps: f64,
    pub frame_start: i32,
    pub frame_end: i32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CameraInfo {
    pub name: String,
    pub present: bool,
    pub lens: f64,
    pub sensor_width: f64,
    pub location: [f64; 3],
    pub rotation_euler: [f64; 3],
}

/// Per-object measurements taken from the object matrix and the camera projection.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ObjectReport {
    pub name: String,
    pub kind: String,
    pub bbox_min: [f64; 3],
    pub bbox_max: [f64; 3],
    pub center: [f64; 3],
    /// Share of the bounding-box lattice that projects inside the camera frame.
    pub inside_pct: f64,
    pub outside_pct: f64,
    pub samples_inside: u32,
    pub samples_total: u32,
    #[serde(default)]
    pub samples_behind_camera: u32,
    pub behind_camera: bool,
    pub visible: bool,
    #[serde(default)]
    pub occluded_pct: f64,
    #[serde(default)]
    pub occluder: Option<String>,
    #[serde(default)]
    pub floor_penetration: f64,
    #[serde(default)]
    pub screen_min: Option<[f64; 2]>,
    #[serde(default)]
    pub screen_max: Option<[f64; 2]>,
}

impl ObjectReport {
    /// How far past the frame edge the object reaches, as a multiple of the half-frame (1.0 = filling it).
    pub fn extent_factor(&self) -> Option<f64> {
        let low = self.screen_min?;
        let high = self.screen_max?;
        let mut worst: f64 = 0.0;
        for axis in 0..2 {
            worst = worst
                .max((low[axis] - 0.5).abs() * 2.0)
                .max((high[axis] - 0.5).abs() * 2.0);
        }
        if worst.is_finite() && worst > 0.0 {
            Some(worst)
        } else {
            None
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Intersection {
    pub a: String,
    pub b: String,
    /// Overlap volume as a share of the smaller bounding box (0.0 = no contact).
    pub bbox_overlap: f64,
    /// Triangle pairs reported by BVHTree.overlap; 0 means no mesh contact.
    #[serde(default)]
    pub mesh_pairs: u32,
}

/// Measured from the rendered pixels, not predicted from the scene.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ImageStats {
    #[serde(default)]
    pub luminance_hist: Vec<f64>,
    #[serde(default)]
    pub mean_luma: f64,
    #[serde(default)]
    pub over_exposed_pct: f64,
    #[serde(default)]
    pub under_exposed_pct: f64,
    #[serde(default)]
    pub alpha_coverage: f64,
    #[serde(default)]
    pub covered_pixels: u64,
    #[serde(default)]
    pub total_pixels: u64,
    #[serde(default)]
    pub empty: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Views {
    #[serde(default)]
    pub camera: Option<PathBuf>,
    #[serde(default)]
    pub top: Option<PathBuf>,
    #[serde(default)]
    pub side: Option<PathBuf>,
    #[serde(default)]
    pub wire: Option<PathBuf>,
}

impl Views {
    fn ordered(&self) -> [Option<PathBuf>; 4] {
        [
            self.camera.clone(),
            self.top.clone(),
            self.side.clone(),
            self.wire.clone(),
        ]
    }
}

/// Everything measured about one frame of one scene.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Diagnostics {
    pub frame: i32,
    pub scene: SceneInfo,
    pub camera: CameraInfo,
    pub objects: Vec<ObjectReport>,
    #[serde(default)]
    pub intersections: Vec<Intersection>,
    #[serde(default)]
    pub floor_z: f64,
    #[serde(default)]
    pub image: ImageStats,
    #[serde(default)]
    pub views: Views,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Severity {
    Info,
    Warning,
    Error,
}

/// A measured problem plus the action that fixes it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Finding {
    pub code: String,
    pub severity: Severity,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub subject: Option<String>,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub suggestion: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub value: Option<f64>,
}

/// A correction the closed loop applied, reported verbatim.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Correction {
    pub op: String,
    #[serde(default)]
    pub target: Option<String>,
    #[serde(default)]
    pub delta: Option<f64>,
    #[serde(default)]
    pub factor: Option<f64>,
    #[serde(default)]
    pub reason: String,
}

impl Correction {
    pub fn describe(&self) -> String {
        match self.op.as_str() {
            "camera_dolly" => format!(
                "pulled the camera back x{:.2} ({})",
                self.factor.unwrap_or(1.0),
                self.reason
            ),
            "raise_object" => format!(
                "raised {} by {:.2} m ({})",
                self.target.clone().unwrap_or_else(|| "?".into()),
                self.delta.unwrap_or(0.0),
                self.reason
            ),
            other => format!("{other}: {}", self.reason),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RoundRecord {
    pub round: u32,
    pub frame: i32,
    pub diagnostics: Diagnostics,
    #[serde(default)]
    pub corrections: Vec<Correction>,
}

impl RoundRecord {
    pub fn findings(&self) -> Vec<Finding> {
        self.diagnostics.findings()
    }
}

impl Diagnostics {
    /// Turns measurements into actionable findings. Pure: no Blender, no ffmpeg.
    pub fn findings(&self) -> Vec<Finding> {
        let mut out = Vec::new();
        let frame = self.frame;
        if !self.camera.present {
            out.push(Finding {
                code: "no_camera".into(),
                severity: Severity::Error,
                subject: None,
                message: format!(
                    "scene {} has no active camera at frame {frame}",
                    self.scene.name
                ),
                suggestion: Some(
                    "assign a camera with bpy.context.scene.camera before previewing".into(),
                ),
                value: None,
            });
        } else {
            out.push(Finding {
                code: "camera".into(),
                severity: Severity::Info,
                subject: Some(self.camera.name.clone()),
                message: format!(
                    "active camera {} ({:.0} mm) at ({:.2}, {:.2}, {:.2}) looking at frame {frame}",
                    self.camera.name,
                    self.camera.lens,
                    self.camera.location[0],
                    self.camera.location[1],
                    self.camera.location[2]
                ),
                suggestion: None,
                value: Some(self.camera.lens),
            });
        }

        for object in &self.objects {
            if !self.camera.present {
                break;
            }
            let outside = object.outside_pct.clamp(0.0, 100.0);
            if object.behind_camera || outside >= OFF_FRAME_ERROR_PCT {
                out.push(Finding {
                    code: "object_off_frame".into(),
                    severity: Severity::Error,
                    subject: Some(object.name.clone()),
                    message: format!(
                        "{} is {:.0}% outside the camera frame at frame {frame}.",
                        object.name, outside
                    ),
                    suggestion: self.off_frame_suggestion(object),
                    value: Some(outside),
                });
            } else if outside > OFF_FRAME_WARN_PCT {
                out.push(Finding {
                    code: "object_off_frame".into(),
                    severity: Severity::Warning,
                    subject: Some(object.name.clone()),
                    message: format!(
                        "{} is {:.0}% outside the camera frame at frame {frame}.",
                        object.name, outside
                    ),
                    suggestion: self.off_frame_suggestion(object),
                    value: Some(outside),
                });
            }
            if object.floor_penetration > FLOOR_ERROR_M {
                out.push(Finding {
                    code: "floor_penetration".into(),
                    severity: Severity::Error,
                    subject: Some(object.name.clone()),
                    message: format!(
                        "{} is {:.2} m below the floor plane (z={:.2}) at frame {frame}.",
                        object.name, object.floor_penetration, self.floor_z
                    ),
                    suggestion: Some(format!(
                        "raise it by {:.2} m",
                        object.floor_penetration + 0.01
                    )),
                    value: Some(object.floor_penetration),
                });
            }
            if object.occluded_pct > OCCLUSION_WARN_PCT {
                let occluder = object
                    .occluder
                    .clone()
                    .unwrap_or_else(|| "another object".into());
                out.push(Finding {
                    code: "object_occluded".into(),
                    severity: Severity::Warning,
                    subject: Some(object.name.clone()),
                    message: format!(
                        "{} is {:.0}% hidden behind {} at frame {frame}.",
                        object.name, object.occluded_pct, occluder
                    ),
                    suggestion: Some(format!(
                        "move {} or {} so the line of sight to the camera is clear",
                        occluder, object.name
                    )),
                    value: Some(object.occluded_pct),
                });
            }
        }

        for pair in &self.intersections {
            if pair.mesh_pairs > 0 {
                out.push(Finding {
                    code: "mesh_collision".into(),
                    severity: Severity::Error,
                    subject: Some(pair.a.clone()),
                    message: format!(
                        "{} and {} intersect: {} triangle pairs overlap at frame {frame}.",
                        pair.a, pair.b, pair.mesh_pairs
                    ),
                    suggestion: Some(format!(
                        "separate {} and {} along their shallowest overlap axis",
                        pair.a, pair.b
                    )),
                    value: Some(pair.mesh_pairs as f64),
                });
            } else if pair.bbox_overlap > 0.0 {
                out.push(Finding {
                    code: "bbox_overlap".into(),
                    severity: Severity::Warning,
                    subject: Some(pair.a.clone()),
                    message: format!(
                        "the bounding boxes of {} and {} overlap by {:.0}% at frame {frame} (no triangle contact).",
                        pair.a,
                        pair.b,
                        pair.bbox_overlap * 100.0
                    ),
                    suggestion: Some(
                        "ignore if the shapes are meant to interlock, otherwise move one clear of the other"
                            .into(),
                    ),
                    value: Some(pair.bbox_overlap),
                });
            }
        }

        if self.image.empty {
            out.push(Finding {
                code: "empty_frame".into(),
                severity: Severity::Error,
                subject: None,
                message: format!(
                    "frame {frame} is empty: the camera sees nothing (alpha coverage {:.2}%).",
                    self.image.alpha_coverage * 100.0
                ),
                suggestion: Some(
                    "check the camera points at the subject, or that the subject is not fully transparent"
                        .into(),
                ),
                value: Some(self.image.alpha_coverage),
            });
        } else {
            if self.image.over_exposed_pct > EXPOSURE_WARN_PCT {
                out.push(Finding {
                    code: "over_exposed".into(),
                    severity: Severity::Warning,
                    subject: None,
                    message: format!(
                        "{:.0}% of the visible pixels are clipped white at frame {frame}.",
                        self.image.over_exposed_pct
                    ),
                    suggestion: Some(
                        "lower the key light energy or set view_settings.exposure to -0.7".into(),
                    ),
                    value: Some(self.image.over_exposed_pct),
                });
            }
            if self.image.under_exposed_pct > EXPOSURE_WARN_PCT {
                out.push(Finding {
                    code: "under_exposed".into(),
                    severity: Severity::Warning,
                    subject: None,
                    message: format!(
                        "{:.0}% of the visible pixels are crushed black at frame {frame}.",
                        self.image.under_exposed_pct
                    ),
                    suggestion: Some(
                        "raise the key light energy or set view_settings.exposure to +0.7".into(),
                    ),
                    value: Some(self.image.under_exposed_pct),
                });
            }
        }
        out
    }

    fn off_frame_suggestion(&self, object: &ObjectReport) -> Option<String> {
        let factor = object.extent_factor()?;
        if factor <= 1.0 {
            return Some(format!(
                "bring {} closer to the centre of the frame",
                object.name
            ));
        }
        let lens = if self.camera.lens > 0.0 {
            self.camera.lens
        } else {
            50.0
        };
        Some(format!(
            "widen the lens from {lens:.0}mm to {:.0}mm, or dolly the camera back x{:.2}",
            lens / factor,
            factor
        ))
    }

    /// Accepted when nothing measured is an error.
    pub fn passed(&self) -> bool {
        !self
            .findings()
            .iter()
            .any(|f| f.severity == Severity::Error)
    }

    pub fn errors(&self) -> Vec<Finding> {
        self.findings()
            .into_iter()
            .filter(|f| f.severity == Severity::Error)
            .collect()
    }
}

/// Which backend produced a [PreviewReport].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PreviewBackend {
    /// A private "blender -b --factory-startup" per call: no warm worker was used.
    Cold,
    /// The persistent worker measured a scene it already had open.
    Warm,
}

/// Which backend [preview_check_with] should use.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PreviewMode {
    /// Warm when the worker answers, cold otherwise. The default.
    Auto,
    /// Never use the worker.
    Cold,
    /// Require the worker; report the failure instead of falling back.
    Warm,
}

impl PreviewMode {
    /// Reads CINVAR_PREVIEW_BACKEND (auto | cold | warm) so operators can pin a backend.
    pub fn from_env() -> Self {
        Self::parse(std::env::var("CINVAR_PREVIEW_BACKEND").ok().as_deref())
    }

    pub fn parse(raw: Option<&str>) -> Self {
        match raw.map(|value| value.trim().to_ascii_lowercase()).as_deref() {
            Some("cold") => PreviewMode::Cold,
            Some("warm") => PreviewMode::Warm,
            _ => PreviewMode::Auto,
        }
    }
}

/// Result of a single inspection pass.
#[derive(Debug, Clone, Serialize)]
pub struct PreviewReport {
    pub sheet: PathBuf,
    pub diagnostics: Diagnostics,
    pub findings: Vec<Finding>,
    pub passed: bool,
    pub elapsed_ms: u128,
    pub blender_version: String,
    pub diagnostics_path: PathBuf,
    pub out_dir: PathBuf,
    /// Which backend ran the inspection.
    pub backend: PreviewBackend,
    /// The warm call had to start the worker first (the one-time cold cost).
    pub worker_started: bool,
    /// The worker already had this exact .blend open, so no scene load was paid.
    pub scene_reused: bool,
    /// Milliseconds paid to Blender for this call: worker start, scene load and measurement.
    /// Never includes the contact-sheet assembly, which is reported separately.
    pub blender_ms: f64,
    /// Milliseconds the scene load took; 0.0 when the warm worker already had it open. On the
    /// first call of a worker's life this includes the one-time worker start.
    pub scene_load_ms: f64,
    /// Milliseconds spent assembling the contact sheet with ffmpeg.
    pub sheet_ms: f64,
    /// Why the warm backend was abandoned, when it was. Never silent.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fallback: Option<String>,
}

/// Result of the bounded correction loop.
#[derive(Debug, Clone, Serialize)]
pub struct CorrectionReport {
    pub accepted: bool,
    pub rounds: Vec<RoundRecord>,
    pub findings: Vec<Finding>,
    pub sheet: PathBuf,
    pub history_path: PathBuf,
    pub elapsed_ms: u128,
    pub blender_version: String,
    pub out_dir: PathBuf,
}

fn require_blend(blend: &Path) -> Result<()> {
    if blend.is_file() {
        Ok(())
    } else {
        Err(Error::not_found(format!(
            "no such .blend file: {}",
            blend.display()
        )))
    }
}

fn prepare_job(req: &PreviewRequest) -> Result<PathBuf> {
    paths::ensure_dir(&req.out_dir)?;
    paths::write_atomic(
        &req.out_dir.join("cinvar_bpy_sdk.py"),
        sdk_source().as_bytes(),
    )?;
    let driver = req.out_dir.join("cinvar_preview.py");
    paths::write_atomic(&driver, DRIVER.as_bytes())?;
    let job = serde_json::json!({
        "out_dir": req.out_dir.to_string_lossy(),
        "frame": req.frame.job_value(),
        "size": req.size,
        "max_rounds": req.max_rounds,
        "floor_z": req.floor_z,
        "mode": if req.max_rounds > 1 { "correct" } else { "check" },
        "sdk": sdk_hash(),
    });
    paths::write_atomic(
        &req.out_dir.join("cinvar_preview_job.json"),
        serde_json::to_string_pretty(&job)?.as_bytes(),
    )?;
    Ok(driver)
}

/// Runs the measuring Blender in the background, streaming its output to blender.log.
fn run_blender(blend: &Path, driver: &Path, out_dir: &Path) -> Result<String> {
    let info = detect()?;
    let log_path = out_dir.join("blender_preview.log");
    let log = std::fs::File::create(&log_path)?;
    let mut cmd = std::process::Command::new(&info.path);
    cmd.arg("-b")
        .arg("-noaudio")
        .arg("--factory-startup")
        .arg(blend)
        .arg("--python")
        .arg(driver)
        .current_dir(out_dir)
        .stdin(std::process::Stdio::null())
        .stdout(log.try_clone()?)
        .stderr(log);
    let mut child = proc::spawn_retrying(&mut cmd)?;
    let started = std::time::Instant::now();
    let timeout = std::time::Duration::from_secs(BLENDER_TIMEOUT_SECS);
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break status;
        }
        if started.elapsed() > timeout {
            let _ = child.kill();
            let _ = child.wait();
            return Err(Error::new(
                ErrorCode::Render,
                format!("Blender preview timed out after {}s", timeout.as_secs()),
            ));
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    };
    if !status.success() {
        let detail = std::fs::read_to_string(out_dir.join("error.json"))
            .ok()
            .and_then(|text| serde_json::from_str::<serde_json::Value>(&text).ok())
            .and_then(|value| {
                value
                    .get("traceback")
                    .and_then(|t| t.as_str())
                    .map(str::to_string)
            })
            .unwrap_or_else(|| tail_of(&log_path));
        return Err(Error::new(
            ErrorCode::Render,
            format!(
                "Blender preview failed: {}",
                crate::secrets::redact(&detail)
            ),
        )
        .with_hint("check blender_preview.log in the preview directory"));
    }
    Ok(info.version)
}

fn tail_of(log: &Path) -> String {
    let text = std::fs::read_to_string(log).unwrap_or_default();
    let lines: Vec<&str> = text.lines().collect();
    lines[lines.len().saturating_sub(MAX_STDERR_LINES)..].join("\n")
}

fn read_json<T: for<'de> Deserialize<'de>>(path: &Path) -> Result<T> {
    let text = std::fs::read_to_string(path).map_err(|e| {
        Error::new(
            ErrorCode::Render,
            format!("missing preview output {}: {e}", path.display()),
        )
    })?;
    serde_json::from_str(&text).map_err(|e| {
        Error::new(
            ErrorCode::Render,
            format!("unreadable preview telemetry {}: {e}", path.display()),
        )
    })
}

/// The ffmpeg filtergraph that turns four panels into one square diagnostic sheet.
/// Pure so the assembly is testable without ffmpeg.
pub fn sheet_filtergraph(present: &[bool; 4], panel: u32) -> (String, usize) {
    let panel = panel & !1;
    let mut inputs = 0usize;
    let mut parts: Vec<String> = Vec::new();
    for (index, has) in present.iter().enumerate() {
        if *has {
            parts.push(format!(
                "[{inputs}:v]scale={panel}:{panel}:force_original_aspect_ratio=decrease,setsar=1[src{index}]"
            ));
            inputs += 1;
        } else {
            parts.push(format!("color=c=0x20242c:s={panel}x{panel}[src{index}]"));
        }
        parts.push(format!(
            "color=c=0x11151b:s={panel}x{panel}[bg{index}];[bg{index}][src{index}]overlay=(W-w)/2:(H-h)/2:format=rgb[o{index}]"
        ));
    }
    parts.push("[o0][o1]hstack=inputs=2[row0]".into());
    parts.push("[o2][o3]hstack=inputs=2[row1]".into());
    parts.push(format!(
        "[row0][row1]vstack=inputs=2,scale={SHEET_SIZE}:{SHEET_SIZE},setsar=1[sheet]"
    ));
    (parts.join(";"), inputs)
}

/// Assembles the four rendered panels into one 1024x1024 PNG with ffmpeg.
pub fn assemble_contact_sheet(views: &Views, out: &Path) -> Result<PathBuf> {
    let panels = views.ordered();
    let present: [bool; 4] = [
        panels[0].as_ref().is_some(),
        panels[1].as_ref().is_some(),
        panels[2].as_ref().is_some(),
        panels[3].as_ref().is_some(),
    ];
    if !present.iter().any(|p| *p) {
        return Err(Error::new(
            ErrorCode::Render,
            "no diagnostic panels were rendered, so there is no contact sheet to assemble",
        ));
    }
    for path in panels.iter().flatten() {
        if !path.is_file() {
            return Err(Error::new(
                ErrorCode::Render,
                format!("diagnostic panel is missing: {}", path.display()),
            ));
        }
    }
    let ffmpeg = proc::require("ffmpeg")?;
    if let Some(parent) = out.parent() {
        paths::ensure_dir(parent)?;
    }
    let (graph, _inputs) = sheet_filtergraph(&present, SHEET_SIZE / 2);
    let mut args: Vec<String> = vec![
        "-y".into(),
        "-hide_banner".into(),
        "-nostdin".into(),
        "-v".into(),
        "error".into(),
    ];
    for path in panels.iter().flatten() {
        args.push("-i".into());
        args.push(path.to_string_lossy().to_string());
    }
    for arg in [
        "-filter_complex".to_string(),
        graph,
        "-map".to_string(),
        "[sheet]".to_string(),
        "-frames:v".to_string(),
        "1".to_string(),
        "-update".to_string(),
        "1".to_string(),
        out.to_string_lossy().to_string(),
    ] {
        args.push(arg);
    }
    let run = proc::run(&ffmpeg, &args, &ProcOpts::default().with_timeout(120))?;
    if !run.success {
        return Err(Error::new(
            ErrorCode::Render,
            format!(
                "ffmpeg could not assemble the diagnostic sheet: {}",
                crate::secrets::redact(&run.tail_stderr(6))
            ),
        ));
    }
    if !out.is_file() || std::fs::metadata(out).map(|m| m.len()).unwrap_or(0) == 0 {
        return Err(Error::new(
            ErrorCode::Render,
            format!("diagnostic sheet is empty: {}", out.display()),
        ));
    }
    Ok(out.to_path_buf())
}

fn sheet_path(out_dir: &Path) -> PathBuf {
    out_dir.join("contact_sheet.png")
}

/// Inspects one frame of a .blend file: measurements, findings and a 1024x1024 sheet.
///
/// Uses the warm worker when it answers and the cold per-call Blender otherwise; the report says
/// which one ran it and why, in [PreviewReport::backend] and [PreviewReport::fallback].
pub fn preview_check(req: &PreviewRequest) -> Result<PreviewReport> {
    preview_check_with(req, PreviewMode::from_env())
}

/// [preview_check] with the backend chosen by the caller instead of CINVAR_PREVIEW_BACKEND.
pub fn preview_check_with(req: &PreviewRequest, mode: PreviewMode) -> Result<PreviewReport> {
    require_blend(&req.blend)?;
    // Timed from before the backend is chosen, so a failed warm attempt is part of the latency
    // the report states rather than hidden by the fallback.
    let started = std::time::Instant::now();
    match mode {
        PreviewMode::Cold => cold_check(req, started, None),
        PreviewMode::Warm => {
            let outcome = warm::check(req);
            match &outcome {
                Ok(_) => warm::record_success(),
                Err(err) => warm::record_failure(err),
            }
            outcome
        }
        PreviewMode::Auto => {
            if let Some(reason) = warm::cooldown_reason() {
                let err = Error::new(ErrorCode::Provider, reason)
                    .with_hint("the cold backend ran this inspection instead");
                return cold_check(req, started, Some(err));
            }
            match warm::check(req) {
                Ok(report) => {
                    warm::record_success();
                    Ok(report)
                }
                Err(err) => {
                    warm::record_failure(&err);
                    cold_check(req, started, Some(err))
                }
            }
        }
    }
}

/// The original single-process backend: one private Blender per call, no worker involved.
fn cold_check(
    req: &PreviewRequest,
    started: std::time::Instant,
    fallback: Option<Error>,
) -> Result<PreviewReport> {
    // Inspection only: a single pass, no corrections (preview_correct owns the loop).
    let mut request = req.clone();
    request.max_rounds = 1;
    let driver = prepare_job(&request)?;
    let blender_started = std::time::Instant::now();
    let blender_version = run_blender(&request.blend, &driver, &request.out_dir)?;
    let blender_ms = blender_started.elapsed().as_secs_f64() * 1000.0;
    let diagnostics: Diagnostics = read_json(&request.out_dir.join("diagnostics.json"))?;
    let findings = diagnostics.findings();
    let passed = diagnostics.passed();
    let sheet_started = std::time::Instant::now();
    let sheet = assemble_contact_sheet(&diagnostics.views, &sheet_path(&request.out_dir))?;
    let sheet_ms = sheet_started.elapsed().as_secs_f64() * 1000.0;
    Ok(PreviewReport {
        sheet,
        diagnostics,
        findings,
        passed,
        elapsed_ms: started.elapsed().as_millis(),
        blender_version,
        diagnostics_path: request.out_dir.join("diagnostics.json"),
        out_dir: request.out_dir,
        backend: PreviewBackend::Cold,
        worker_started: false,
        scene_reused: false,
        blender_ms,
        scene_load_ms: 0.0,
        sheet_ms,
        fallback: fallback.map(|err| err.to_string()),
    })
}

/// Runs the bounded loop scene -> preview -> diagnostics -> correction -> preview.
/// Stops early as soon as the diagnostics pass, and persists the whole history to JSON.
pub fn preview_correct(req: &PreviewRequest) -> Result<CorrectionReport> {
    require_blend(&req.blend)?;
    let started = std::time::Instant::now();
    let mut request = req.clone();
    request.max_rounds = req.max_rounds.max(1);
    let driver = prepare_job(&request)?;
    let blender_version = run_blender(&request.blend, &driver, &request.out_dir)?;
    let rounds_file: RoundsFile = read_json(&request.out_dir.join("rounds.json"))?;
    if rounds_file.rounds.is_empty() {
        return Err(Error::new(
            ErrorCode::Render,
            "the correction loop produced no diagnostic rounds",
        ));
    }
    let diagnostics_path = request.out_dir.join("diagnostics.json");
    let final_diagnostics: Diagnostics = read_json(&diagnostics_path)?;
    let findings = final_diagnostics.findings();
    let accepted = final_diagnostics.passed();
    let sheet = assemble_contact_sheet(&final_diagnostics.views, &sheet_path(&request.out_dir))?;
    let history = serde_json::json!({
        "blend": request.blend.to_string_lossy(),
        "frame": request.frame.job_value(),
        "max_rounds": rounds_file.max_rounds,
        "accepted": accepted,
        "accepted_round": if accepted {
            serde_json::json!(rounds_file.rounds.last().map(|r| r.round).unwrap_or_default())
        } else {
            serde_json::Value::Null
        },
        "rounds": rounds_file.rounds.iter().map(|round| {
            serde_json::json!({
                "round": round.round,
                "frame": round.frame,
                "corrections": round.corrections.iter().map(|c| c.describe()).collect::<Vec<_>>(),
                "findings": round.findings(),
                "passed": round.diagnostics.passed(),
                "diagnostics": round.diagnostics,
            })
        }).collect::<Vec<_>>(),
    });
    let history_path = request.out_dir.join("correction_history.json");
    paths::write_atomic(
        &history_path,
        serde_json::to_string_pretty(&history)?.as_bytes(),
    )?;
    Ok(CorrectionReport {
        accepted,
        rounds: rounds_file.rounds,
        findings,
        sheet,
        history_path,
        elapsed_ms: started.elapsed().as_millis(),
        blender_version,
        out_dir: request.out_dir,
    })
}

#[derive(Debug, Clone, Deserialize)]
struct RoundsFile {
    #[serde(default)]
    max_rounds: u32,
    #[serde(default)]
    rounds: Vec<RoundRecord>,
}

#[cfg(test)]
mod tests {
    use super::*;

    const OFF_FRAME_JSON: &str = r#"{
      "frame": 45,
      "scene": {"name": "Scene", "engine": "BLENDER_EEVEE", "resolution": [1920, 1080], "fps": 30.0, "frame_start": 1, "frame_end": 90},
      "camera": {"name": "Camera", "present": true, "lens": 50.0, "sensor_width": 36.0, "location": [0.0, -6.0, 1.0], "rotation_euler": [85.0, 0.0, 0.0]},
      "objects": [
        {"name": "CINVAR_TEXT", "kind": "FONT", "bbox_min": [-1.0, 0.0, -0.4], "bbox_max": [1.0, 0.0, 0.4], "center": [0.0, 0.0, 0.0],
         "inside_pct": 100.0, "outside_pct": 0.0, "samples_inside": 27, "samples_total": 27, "samples_behind_camera": 0,
         "behind_camera": false, "visible": true, "occluded_pct": 0.0, "occluder": null, "floor_penetration": 0.0,
         "screen_min": [0.3, 0.4], "screen_max": [0.7, 0.6]},
        {"name": "OFFSCREEN_CUBE", "kind": "MESH", "bbox_min": [7.5, -0.5, 0.0], "bbox_max": [8.5, 0.5, 1.0], "center": [8.0, 0.0, 0.5],
         "inside_pct": 58.0, "outside_pct": 42.0, "samples_inside": 15, "samples_total": 27, "samples_behind_camera": 0,
         "behind_camera": false, "visible": true, "occluded_pct": 0.0, "occluder": null, "floor_penetration": 0.0,
         "screen_min": [0.999, 0.39], "screen_max": [1.42, 0.75]}
      ],
      "intersections": [],
      "floor_z": 0.0,
      "image": {"luminance_hist": [0.1, 0.2], "mean_luma": 0.4, "over_exposed_pct": 0.0, "under_exposed_pct": 0.0,
                "alpha_coverage": 0.12, "covered_pixels": 132710, "total_pixels": 1105920, "empty": false},
      "views": {"camera": "/tmp/view_camera.png", "top": "/tmp/view_top.png", "side": "/tmp/view_side.png", "wire": "/tmp/view_wire.png"}
    }"#;

    fn off_frame_diagnostics() -> Diagnostics {
        serde_json::from_str(OFF_FRAME_JSON).expect("fixture parses")
    }

    #[test]
    fn off_frame_object_is_named_with_its_percentage() {
        let diagnostics = off_frame_diagnostics();
        let findings = diagnostics.findings();
        let finding = findings
            .iter()
            .find(|f| {
                f.code == "object_off_frame" && f.subject.as_deref() == Some("OFFSCREEN_CUBE")
            })
            .expect("offscreen object must be reported");
        assert_eq!(
            finding.message,
            "OFFSCREEN_CUBE is 42% outside the camera frame at frame 45."
        );
        assert_eq!(finding.severity, Severity::Warning);
        assert!(finding
            .suggestion
            .as_deref()
            .unwrap()
            .contains("widen the lens"));
        assert_eq!(finding.value, Some(42.0));
        let text = findings
            .iter()
            .find(|f| f.subject.as_deref() == Some("CINVAR_TEXT"))
            .is_none();
        assert!(text, "an in-frame object must not raise a finding");
    }

    #[test]
    fn fully_off_screen_object_is_an_error_and_fails_the_frame() {
        let mut diagnostics = off_frame_diagnostics();
        diagnostics.objects[1].outside_pct = 100.0;
        diagnostics.objects[1].inside_pct = 0.0;
        diagnostics.objects[1].visible = false;
        let errors = diagnostics.errors();
        assert!(errors.iter().any(|f| f.code == "object_off_frame"));
        assert!(!diagnostics.passed());
    }

    #[test]
    fn clean_frame_passes_with_only_informational_findings() {
        let mut diagnostics = off_frame_diagnostics();
        diagnostics.objects.pop();
        assert!(diagnostics.passed());
        assert!(diagnostics
            .findings()
            .iter()
            .all(|f| f.severity == Severity::Info));
    }

    #[test]
    fn floor_penetration_collision_and_empty_frame_are_errors() {
        let mut diagnostics = off_frame_diagnostics();
        diagnostics.objects[0].floor_penetration = 0.42;
        diagnostics.intersections.push(Intersection {
            a: "CINVAR_TEXT".into(),
            b: "OFFSCREEN_CUBE".into(),
            bbox_overlap: 0.2,
            mesh_pairs: 17,
        });
        diagnostics.image.empty = true;
        diagnostics.image.alpha_coverage = 0.0;
        let findings = diagnostics.findings();
        let codes: Vec<&str> = findings.iter().map(|f| f.code.as_str()).collect();
        assert!(codes.contains(&"floor_penetration"), "{codes:?}");
        assert!(codes.contains(&"mesh_collision"), "{codes:?}");
        assert!(codes.contains(&"empty_frame"), "{codes:?}");
        let floor = findings
            .iter()
            .find(|f| f.code == "floor_penetration")
            .unwrap();
        assert!(
            floor.message.contains("0.42 m below the floor plane"),
            "{}",
            floor.message
        );
        assert!(floor
            .suggestion
            .as_deref()
            .unwrap()
            .contains("raise it by 0.43 m"));
        let collision = findings
            .iter()
            .find(|f| f.code == "mesh_collision")
            .unwrap();
        assert!(
            collision.message.contains("17 triangle pairs overlap"),
            "{}",
            collision.message
        );
        assert!(!diagnostics.passed());
    }

    #[test]
    fn occluded_object_warns_with_the_occluder_name() {
        let mut diagnostics = off_frame_diagnostics();
        diagnostics.objects[0].occluded_pct = 91.0;
        diagnostics.objects[0].occluder = Some("Backdrop".into());
        let finding = diagnostics
            .findings()
            .into_iter()
            .find(|f| f.code == "object_occluded")
            .expect("occlusion must be reported");
        assert_eq!(
            finding.message,
            "CINVAR_TEXT is 91% hidden behind Backdrop at frame 45."
        );
        assert_eq!(finding.severity, Severity::Warning);
    }

    #[test]
    fn exposure_warnings_come_from_measured_pixels() {
        let mut diagnostics = off_frame_diagnostics();
        diagnostics.image.over_exposed_pct = 40.0;
        diagnostics.image.under_exposed_pct = 30.0;
        let codes: Vec<String> = diagnostics.findings().into_iter().map(|f| f.code).collect();
        assert!(codes.iter().any(|c| c == "over_exposed"));
        assert!(codes.iter().any(|c| c == "under_exposed"));
    }

    #[test]
    fn missing_camera_is_an_error() {
        let mut diagnostics = off_frame_diagnostics();
        diagnostics.camera.present = false;
        diagnostics.camera.name = String::new();
        assert!(diagnostics.errors().iter().any(|f| f.code == "no_camera"));
        assert!(!diagnostics.passed());
    }

    #[test]
    fn sheet_graph_tiles_four_panels_and_pads_missing_ones() {
        let (graph, inputs) = sheet_filtergraph(&[true, true, true, true], 512);
        assert_eq!(inputs, 4);
        assert!(graph.contains("[0:v]scale=512:512"), "{graph}");
        assert!(graph.contains("hstack=inputs=2[row0]"), "{graph}");
        assert!(graph.contains("vstack=inputs=2"), "{graph}");
        assert!(graph.contains("scale=1024:1024"), "{graph}");
        let (graph, inputs) = sheet_filtergraph(&[true, false, false, false], 512);
        assert_eq!(inputs, 1);
        assert_eq!(graph.matches("color=c=0x20242c").count(), 3, "{graph}");
    }

    #[test]
    fn embedded_sdk_measures_instead_of_guessing() {
        let sdk = sdk_source();
        for needle in [
            "world_to_camera_view",
            "BVHTree.FromObject",
            ".overlap(",
            "scene.ray_cast",
            "alpha_coverage",
            "luminance_hist",
            "floor_penetration",
            "image.pixels.foreach_get",
        ] {
            assert!(sdk.contains(needle), "SDK is missing {needle}");
        }
        assert_eq!(sdk_hash().len(), 16);
    }

    #[test]
    fn driver_and_sdk_are_written_into_the_job_directory() {
        let dir = tempfile::tempdir().unwrap();
        let name = sdk_hash();
        let req = PreviewRequest::new(dir.path().join("scene.blend"), dir.path());
        let driver = prepare_job(&req).unwrap();
        assert!(driver.is_file());
        assert!(dir.path().join("cinvar_bpy_sdk.py").is_file());
        let read_job = || {
            serde_json::from_str::<serde_json::Value>(
                &std::fs::read_to_string(dir.path().join("cinvar_preview_job.json")).unwrap(),
            )
            .unwrap()
        };
        let job = read_job();
        assert_eq!(job["mode"], "correct", "the default request runs the loop");
        assert_eq!(job["max_rounds"], serde_json::json!(DEFAULT_MAX_ROUNDS));
        assert_eq!(job["sdk"], name);
        assert_eq!(job["frame"], "middle");

        // A single pass is what preview_check asks for.
        let single = PreviewRequest::new(dir.path().join("scene.blend"), dir.path()).max_rounds(1);
        prepare_job(&single).unwrap();
        assert_eq!(read_job()["mode"], "check");
    }

    #[test]
    fn missing_blend_is_rejected_before_blender_runs() {
        let dir = tempfile::tempdir().unwrap();
        let req = PreviewRequest::new(dir.path().join("nope.blend"), dir.path());
        let err = preview_check(&req).unwrap_err();
        assert_eq!(err.code, ErrorCode::NotFound);
    }

    #[test]
    fn sdk_is_valid_python() {
        let python = proc::which("python3");
        let Some(python) = python else {
            eprintln!("python3 not installed; skipping the syntax check");
            return;
        };
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("cinvar_bpy_sdk.py");
        std::fs::write(&target, sdk_source()).unwrap();
        let out = std::process::Command::new(python)
            .arg("-m")
            .arg("py_compile")
            .arg(&target)
            .output()
            .expect("python3 runs");
        assert!(
            out.status.success(),
            "generated SDK is not valid python: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    #[test]
    fn requests_default_to_three_correction_rounds_and_can_be_bounded() {
        let req = PreviewRequest::new("/tmp/a.blend", "/tmp/out");
        assert_eq!(req.max_rounds, DEFAULT_MAX_ROUNDS);
        assert_eq!(req.max_rounds, 3);
        assert_eq!(req.clone().max_rounds(0).max_rounds, 1);
        assert_eq!(req.clone().max_rounds(99).max_rounds, 10);
        assert_eq!(
            req.clone().frame(FrameChoice::Final).frame,
            FrameChoice::Final
        );
        assert_eq!(req.size(64).size, 256);
    }

    #[test]
    fn frame_choice_maps_to_the_sdk_job_value() {
        assert_eq!(FrameChoice::First.job_value(), serde_json::json!("first"));
        assert_eq!(FrameChoice::Final.job_value(), serde_json::json!("final"));
        assert_eq!(FrameChoice::Exact(45).job_value(), serde_json::json!(45));
    }

    #[test]
    fn the_operator_override_selects_the_backend() {
        assert_eq!(PreviewMode::parse(None), PreviewMode::Auto);
        assert_eq!(PreviewMode::parse(Some("")), PreviewMode::Auto);
        assert_eq!(PreviewMode::parse(Some("auto")), PreviewMode::Auto);
        assert_eq!(PreviewMode::parse(Some(" COLD ")), PreviewMode::Cold);
        assert_eq!(PreviewMode::parse(Some("warm")), PreviewMode::Warm);
        // Anything unrecognised stays on the default rather than surprising the caller.
        assert_eq!(PreviewMode::parse(Some("coldish")), PreviewMode::Auto);
    }

    #[test]
    fn the_report_says_which_backend_ran() {
        assert_eq!(
            serde_json::to_value(PreviewBackend::Cold).unwrap(),
            serde_json::json!("cold")
        );
        assert_eq!(
            serde_json::to_value(PreviewBackend::Warm).unwrap(),
            serde_json::json!("warm")
        );
    }

    #[test]
    fn correction_descriptions_are_human_readable() {
        let dolly = Correction {
            op: "camera_dolly".into(),
            target: None,
            delta: None,
            factor: Some(1.25),
            reason: "objects reach 118% of the frame".into(),
        };
        assert_eq!(
            dolly.describe(),
            "pulled the camera back x1.25 (objects reach 118% of the frame)"
        );
        let raise = Correction {
            op: "raise_object".into(),
            target: Some("LOGO".into()),
            delta: Some(0.42),
            factor: None,
            reason: "below the floor".into(),
        };
        assert_eq!(raise.describe(), "raised LOGO by 0.42 m (below the floor)");
    }
}
