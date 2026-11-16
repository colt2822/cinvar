//! Blender video mockups (Argon 5D): place a real timeline clip or screen recording into a
//! rendered 3D scene (floating screens, glass browser cards, device mockups) and render the
//! finished animation back into the CINVAR timeline.
//!
//! The video is not a demo texture: the request's source is decoded to a frame-exact PNG
//! sequence with ffmpeg and bound to the 3D surface as a Blender image sequence whose frames
//! are synced to the render frames, so frame N of the render shows frame N of the clip.
//!
//! Pipeline (all deterministic for the same inputs):
//!
//! 1. probe the source (ffprobe) and normalise the request;
//! 2. extract 'tex/frame_%04d.png' from the source at the render fps (padding a short clip by
//!    holding its last frame rather than failing);
//! 3. write the generated scene driver (scene_gen::script_for) plus 'mockup_job.json';
//! 4. render 'frames/frame_%04d.png' with Blender, resuming over frames already on disk;
//! 5. read 'mockup_measure.json' (the projected screen rectangle of every frame);
//! 6. encode the clip and build the ffmpeg filtergraph that puts it back on a timeline.
//!
//! Free/Pro: the request carries the duration the caller asked for and the render reports the
//! duration it delivered; gating that duration is the caller's (tool layer's) decision.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::{Error, ErrorCode, Result};
use crate::util::paths;
use crate::util::proc::{self, ProcOpts};
use crate::util::time::Time;

use super::alpha::{self, Anchor, OverlaySource, OverlaySpec};
use super::{detect, RenderControl};

#[path = "mockup_scene.rs"]
pub mod scene_gen;

use scene_gen::{script_for, MockupJob, SCRIPT_VERSION};

/// The tool-layer name this module backs; the edition table gates it as Feature::BlenderMockup.
/// Free/Pro is the caller's decision: a request carries the duration it asked for and a render
/// reports the duration it delivered, so the tool layer clamps (see clamp_duration) or refuses.
pub const TOOL: &str = "cinvar.blender.mockup_screen";

const MAX_STDERR_LINES: usize = 20;
/// Rendered frames are capped so a bad request cannot queue days of rendering.
pub const MAX_FRAMES: u32 = 7200;
/// Longest clip a mockup render will take on.
pub const MAX_DURATION_SECS: f64 = 300.0;
/// Shortest clip worth rendering (two frames at any sane fps).
pub const MIN_DURATION_SECS: f64 = 0.1;
/// Texture frames are capped: a 4K screen texture costs render time for no visible gain.
const TEXTURE_MAX_WIDTH: u32 = 1920;
const TEXTURE_MIN_WIDTH: u32 = 320;
const DEFAULT_SAMPLES: u32 = 48;

// ---------------------------------------------------------------------------
// request
// ---------------------------------------------------------------------------

/// The physical scene the video is placed into.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SceneKind {
    /// A bezel screen floating above a studio floor.
    FloatingScreen,
    /// A browser card with a glass front pane, chrome bar and traffic lights.
    GlassBrowser,
    /// A laptop: hinged lid, key bed and trackpad.
    Laptop,
    /// A phone: rounded body, notch and side buttons.
    Phone,
}

impl SceneKind {
    pub const ALL: [SceneKind; 4] = [
        SceneKind::FloatingScreen,
        SceneKind::GlassBrowser,
        SceneKind::Laptop,
        SceneKind::Phone,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            SceneKind::FloatingScreen => "floating_screen",
            SceneKind::GlassBrowser => "glass_browser",
            SceneKind::Laptop => "laptop",
            SceneKind::Phone => "phone",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            SceneKind::FloatingScreen => "floating screen",
            SceneKind::GlassBrowser => "glass browser card",
            SceneKind::Laptop => "laptop",
            SceneKind::Phone => "phone",
        }
    }

    /// Display box (metres) the clip is fitted into for this device.
    fn nominal_display(self) -> (f64, f64) {
        match self {
            SceneKind::FloatingScreen => (2.40, 1.35),
            SceneKind::GlassBrowser => (2.30, 1.30),
            SceneKind::Laptop => (2.20, 1.38),
            SceneKind::Phone => (0.86, 1.80),
        }
    }

    /// Static tilt of the whole rig, in degrees (x, z).
    fn tilt(self) -> (f64, f64) {
        match self {
            SceneKind::FloatingScreen => (-3.0, 0.0),
            SceneKind::GlassBrowser => (-8.0, -6.0),
            SceneKind::Laptop => (-4.0, 14.0),
            SceneKind::Phone => (-6.0, 12.0),
        }
    }

    fn rig_z(self) -> f64 {
        match self {
            SceneKind::FloatingScreen => 1.25,
            SceneKind::GlassBrowser => 1.20,
            SceneKind::Laptop => 1.55,
            SceneKind::Phone => 1.30,
        }
    }
}

impl Default for SceneKind {
    fn default() -> Self {
        SceneKind::FloatingScreen
    }
}

/// How the camera moves over the clip.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CameraMotion {
    Static,
    Orbit,
    DollyIn,
    PanRight,
    Handheld,
}

impl CameraMotion {
    pub const ALL: [CameraMotion; 5] = [
        CameraMotion::Static,
        CameraMotion::Orbit,
        CameraMotion::DollyIn,
        CameraMotion::PanRight,
        CameraMotion::Handheld,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            CameraMotion::Static => "static",
            CameraMotion::Orbit => "orbit",
            CameraMotion::DollyIn => "dolly_in",
            CameraMotion::PanRight => "pan_right",
            CameraMotion::Handheld => "handheld",
        }
    }
}

impl Default for CameraMotion {
    fn default() -> Self {
        CameraMotion::Static
    }
}

/// Lighting rig for the scene.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LightingPreset {
    Studio,
    Softbox,
    Neon,
    Daylight,
}

impl LightingPreset {
    pub const ALL: [LightingPreset; 4] = [
        LightingPreset::Studio,
        LightingPreset::Softbox,
        LightingPreset::Neon,
        LightingPreset::Daylight,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            LightingPreset::Studio => "studio",
            LightingPreset::Softbox => "softbox",
            LightingPreset::Neon => "neon",
            LightingPreset::Daylight => "daylight",
        }
    }
}

impl Default for LightingPreset {
    fn default() -> Self {
        LightingPreset::Studio
    }
}

/// Render engine. Cycles is the quality path (true reflections, transmission, motion blur);
/// EEVEE renders the same scene several times faster for previews and bulk work.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MockupEngine {
    Cycles,
    Eevee,
}

impl MockupEngine {
    pub fn as_str(self) -> &'static str {
        match self {
            MockupEngine::Cycles => "cycles",
            MockupEngine::Eevee => "eevee",
        }
    }
}

impl Default for MockupEngine {
    fn default() -> Self {
        MockupEngine::Cycles
    }
}

/// One mockup render request. Every field has a default so a tool layer can send only what it
/// cares about; 'video' is the only field that must be set.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct MockupRequest {
    /// The clip that appears on the 3D surface: a timeline render or a screen recording.
    pub video: PathBuf,
    pub kind: SceneKind,
    pub camera: CameraMotion,
    /// Multiplier on every camera motion (0.2..=3.0).
    pub camera_strength: f32,
    pub lighting: LightingPreset,
    /// Multiplier on every light's power (0.1..=4.0).
    pub light_strength: f32,
    /// 0.0 = matte surfaces, 1.0 = mirror floor and glossy screen (clamped).
    pub reflections: f32,
    /// Camera f-stop; 0.0 disables depth of field.
    pub f_stop: f32,
    /// Motion blur shutter in frames; 0.0 disables motion blur.
    pub motion_blur: f32,
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    /// How much of the source to place on the surface.
    pub duration: Time,
    /// In-point inside the source clip.
    pub start: Time,
    /// Accent colour for the rig's detail lights, hex ("#4f8cff").
    pub accent: String,
    pub engine: MockupEngine,
    pub samples: u32,
    /// Render with an alpha backdrop (the device floats over transparency).
    pub transparent: bool,
    pub seed: u32,
}

impl Default for MockupRequest {
    fn default() -> Self {
        MockupRequest {
            video: PathBuf::new(),
            kind: SceneKind::FloatingScreen,
            camera: CameraMotion::Static,
            camera_strength: 1.0,
            lighting: LightingPreset::Studio,
            light_strength: 1.0,
            reflections: 0.35,
            f_stop: 2.8,
            motion_blur: 0.5,
            width: 1920,
            height: 1080,
            fps: 30,
            duration: Time::from_secs_f64(5.0),
            start: Time::ZERO,
            accent: "#4f8cff".into(),
            engine: MockupEngine::Cycles,
            samples: DEFAULT_SAMPLES,
            transparent: false,
            seed: 0,
        }
    }
}

impl MockupRequest {
    pub fn new(video: impl Into<PathBuf>, kind: SceneKind) -> Self {
        MockupRequest {
            video: video.into(),
            kind,
            ..MockupRequest::default()
        }
    }

    pub fn duration(mut self, secs: f64) -> Self {
        self.duration = Time::from_secs_f64(secs);
        self
    }

    pub fn start(mut self, secs: f64) -> Self {
        self.start = Time::from_secs_f64(secs);
        self
    }

    /// Caps the requested duration: what a Free-tier preview gate applies before rendering.
    /// The delivered duration comes back on MockupRender::duration, so nothing is implied.
    pub fn clamp_duration(mut self, max_secs: f64) -> Self {
        let cap = max_secs.max(MIN_DURATION_SECS);
        if self.duration.as_secs_f64() > cap {
            self.duration = Time::from_secs_f64(cap);
        }
        self
    }

    pub fn size(mut self, width: u32, height: u32) -> Self {
        self.width = width;
        self.height = height;
        self
    }

    pub fn fps(mut self, fps: u32) -> Self {
        self.fps = fps;
        self
    }

    pub fn camera(mut self, camera: CameraMotion) -> Self {
        self.camera = camera;
        self
    }

    pub fn camera_strength(mut self, strength: f32) -> Self {
        self.camera_strength = strength;
        self
    }

    pub fn lighting(mut self, lighting: LightingPreset) -> Self {
        self.lighting = lighting;
        self
    }

    pub fn reflections(mut self, amount: f32) -> Self {
        self.reflections = amount;
        self
    }

    pub fn dof(mut self, f_stop: f32) -> Self {
        self.f_stop = f_stop;
        self
    }

    pub fn motion_blur(mut self, shutter: f32) -> Self {
        self.motion_blur = shutter;
        self
    }

    pub fn accent(mut self, hex: impl Into<String>) -> Self {
        self.accent = hex.into();
        self
    }

    pub fn engine(mut self, engine: MockupEngine) -> Self {
        self.engine = engine;
        self
    }

    pub fn samples(mut self, samples: u32) -> Self {
        self.samples = samples;
        self
    }

    pub fn transparent(mut self, transparent: bool) -> Self {
        self.transparent = transparent;
        self
    }

    /// Even output size, at least 128x128 and at most 4096 on the long edge.
    pub fn canvas(&self) -> (u32, u32) {
        let even = |v: u32| ((v.clamp(128, 4096)) / 2) * 2;
        (even(self.width), even(self.height))
    }

    pub fn fps_clamped(&self) -> u32 {
        self.fps.clamp(1, 120)
    }

    pub fn duration_clamped(&self) -> Time {
        let secs = self
            .duration
            .as_secs_f64()
            .clamp(MIN_DURATION_SECS, MAX_DURATION_SECS);
        Time::from_secs_f64(secs)
    }

    /// Frame count actually rendered: round(duration * fps), clamped to MAX_FRAMES.
    pub fn frame_count(&self) -> u32 {
        let frames = (self.duration_clamped().as_secs_f64() * self.fps_clamped() as f64).round();
        (frames as i64).clamp(2, MAX_FRAMES as i64) as u32
    }

    fn samples_clamped(&self) -> u32 {
        self.samples.clamp(1, 4096)
    }

    fn reflections_clamped(&self) -> f64 {
        (self.reflections as f64).clamp(0.0, 1.0)
    }

    fn camera_strength_clamped(&self) -> f64 {
        (self.camera_strength as f64).clamp(0.2, 3.0)
    }

    fn light_strength_clamped(&self) -> f64 {
        (self.light_strength as f64).clamp(0.1, 4.0)
    }

    fn f_stop_clamped(&self) -> f64 {
        if !self.f_stop.is_finite() || self.f_stop <= 0.0 {
            0.0
        } else {
            (self.f_stop as f64).clamp(0.7, 64.0)
        }
    }

    fn motion_blur_clamped(&self) -> f64 {
        if !self.motion_blur.is_finite() {
            0.0
        } else {
            (self.motion_blur as f64).clamp(0.0, 2.0)
        }
    }

    /// Rejects requests that cannot produce a render before anything expensive runs.
    fn validate(&self) -> Result<()> {
        if self.video.as_os_str().is_empty() {
            return Err(Error::invalid(
                "a mockup needs the video that goes on the screen",
            ));
        }
        if !self.video.is_file() {
            return Err(Error::not_found(format!(
                "mockup source video does not exist: {}",
                self.video.display()
            ))
            .with_hint("render the timeline clip first, then pass its path as 'video'"));
        }
        if self.duration.as_secs_f64() <= 0.0 {
            return Err(Error::invalid("mockup duration must be greater than zero"));
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// media probe
// ---------------------------------------------------------------------------

/// What ffprobe measured. Used for both the source clip and the delivered file.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct MediaInfo {
    pub path: PathBuf,
    pub width: u32,
    pub height: u32,
    pub fps: f64,
    pub duration: Time,
    pub codec: String,
    pub pix_fmt: String,
    pub has_audio: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub frames: Option<u64>,
}

impl MediaInfo {
    pub fn aspect(&self) -> f64 {
        if self.height == 0 {
            16.0 / 9.0
        } else {
            self.width as f64 / self.height as f64
        }
    }

    pub fn describe(&self) -> String {
        format!(
            "{}x{} @{:.3}fps, {:.3}s, {}, {}",
            self.width,
            self.height,
            self.fps,
            self.duration.as_secs_f64(),
            self.codec,
            self.pix_fmt
        )
    }
}

fn parse_rate(text: &str) -> Option<f64> {
    if let Some((num, den)) = text.split_once('/') {
        let num: f64 = num.trim().parse().ok()?;
        let den: f64 = den.trim().parse().ok()?;
        if den == 0.0 || num <= 0.0 {
            return None;
        }
        return Some(num / den);
    }
    text.trim().parse::<f64>().ok().filter(|v| *v > 0.0)
}

/// Measures a media file with ffprobe. Never estimates: a missing video stream is an error.
pub fn probe_media(path: &Path) -> Result<MediaInfo> {
    if !path.is_file() {
        return Err(Error::not_found(format!(
            "no such media file: {}",
            path.display()
        )));
    }
    let ffprobe = proc::require("ffprobe")?;
    let args = vec![
        "-v".to_string(),
        "error".to_string(),
        "-print_format".to_string(),
        "json".to_string(),
        "-show_format".to_string(),
        "-show_streams".to_string(),
        path.to_string_lossy().to_string(),
    ];
    let out = proc::run(&ffprobe, &args, &ProcOpts::default().with_timeout(120))?;
    if !out.success {
        return Err(Error::new(
            ErrorCode::Render,
            format!(
                "ffprobe could not read {}: {}",
                path.display(),
                out.tail_stderr(3)
            ),
        ));
    }
    let value: serde_json::Value = serde_json::from_str(&out.stdout).map_err(|e| {
        Error::new(
            ErrorCode::Render,
            format!("ffprobe returned unreadable json: {e}"),
        )
    })?;
    let streams = value["streams"].as_array().cloned().unwrap_or_default();
    let video = streams
        .iter()
        .find(|s| s["codec_type"].as_str() == Some("video"))
        .ok_or_else(|| {
            Error::invalid(format!(
                "{} has no video stream: there is nothing to put on the screen",
                path.display()
            ))
        })?;
    let audio = streams
        .iter()
        .any(|s| s["codec_type"].as_str() == Some("audio"));
    let fps = video["avg_frame_rate"]
        .as_str()
        .and_then(parse_rate)
        .or_else(|| video["r_frame_rate"].as_str().and_then(parse_rate))
        .unwrap_or(0.0);
    let duration = value["format"]["duration"]
        .as_str()
        .and_then(|s| s.parse::<f64>().ok())
        .or_else(|| {
            video["duration"]
                .as_str()
                .and_then(|s| s.parse::<f64>().ok())
        })
        .unwrap_or(0.0);
    Ok(MediaInfo {
        path: path.to_path_buf(),
        width: video["width"].as_u64().unwrap_or(0) as u32,
        height: video["height"].as_u64().unwrap_or(0) as u32,
        fps,
        duration: Time::from_secs_f64(duration.max(0.0)),
        codec: video["codec_name"]
            .as_str()
            .unwrap_or("unknown")
            .to_string(),
        pix_fmt: video["pix_fmt"].as_str().unwrap_or("unknown").to_string(),
        has_audio: audio,
        frames: video["nb_frames"].as_str().and_then(|s| s.parse().ok()),
    })
}

// ---------------------------------------------------------------------------
// measurement
// ---------------------------------------------------------------------------

/// The projected screen rectangle of one rendered frame, in output pixels.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MeasureFrame {
    pub frame: u32,
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
    /// Every corner of the screen quad projected inside the frame.
    pub inside: bool,
}

impl MeasureFrame {
    /// Integer rectangle, clamped to the frame.
    pub fn rect(&self, canvas: (u32, u32)) -> Option<Rect> {
        let x0 = self.x.max(0.0).floor() as u32;
        let y0 = self.y.max(0.0).floor() as u32;
        let w = self.width.max(0.0).floor() as u32;
        let h = self.height.max(0.0).floor() as u32;
        let x1 = (x0 + w).min(canvas.0);
        let y1 = (y0 + h).min(canvas.1);
        if x1 <= x0 || y1 <= y0 {
            return None;
        }
        Some(Rect {
            x: x0,
            y: y0,
            width: x1 - x0,
            height: y1 - y0,
        })
    }
}

/// An integer pixel rectangle.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Rect {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

impl Rect {
    /// Shrinks the rectangle by a fraction of its short edge, so a measurement never samples
    /// the background just outside the projected corners.
    pub fn inset(self, fraction: f64) -> Rect {
        let pad_x = ((self.width as f64 * fraction).round() as u32).max(0);
        let pad_y = ((self.height as f64 * fraction).round() as u32).max(0);
        if self.width <= 2 * pad_x + 1 || self.height <= 2 * pad_y + 1 {
            return self;
        }
        Rect {
            x: self.x + pad_x,
            y: self.y + pad_y,
            width: self.width - 2 * pad_x,
            height: self.height - 2 * pad_y,
        }
    }
}

/// What the Blender driver measured about the scene it built.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SurfaceMeasure {
    pub blender: String,
    pub engine: String,
    pub samples: u32,
    pub screen_object: String,
    pub texture: String,
    pub texture_frames: u32,
    /// What Blender itself resolved the image sequence length to.
    pub texture_frame_duration: u32,
    pub frames: Vec<MeasureFrame>,
    pub visible_all: bool,
    /// World-space size of the device, in metres.
    pub device_size: [f64; 3],
    pub mid_frame_rect: MeasureFrame,
}

impl SurfaceMeasure {
    pub fn at(&self, frame: u32) -> Option<&MeasureFrame> {
        self.frames.iter().find(|f| f.frame == frame)
    }

    /// Frames whose screen quad stayed completely inside the output frame.
    pub fn inside_frames(&self) -> usize {
        self.frames.iter().filter(|f| f.inside).count()
    }
}

/// Pixel difference between two renders of a region.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RegionDiff {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
    pub mean_abs_diff: f64,
    pub changed_fraction: f64,
    pub max_abs_diff: u8,
}

/// Proof that the surface is playing the video instead of showing a still: the mapped region
/// changes between two source times while a control band outside it does not.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SurfaceMotion {
    pub frame_a: u32,
    pub frame_b: u32,
    /// The mapped screen region.
    pub screen: RegionDiff,
    /// A band of the frame outside the screen (None when the screen fills the frame).
    pub control: Option<RegionDiff>,
    pub animated: bool,
}

impl SurfaceMotion {
    pub fn describe(&self) -> String {
        let control = match &self.control {
            Some(c) => format!("{:.2} (max {})", c.mean_abs_diff, c.max_abs_diff),
            None => "none".into(),
        };
        format!(
            "frames {}->{}: screen {:.2}x{:.2}px mean |diff| {:.2}, {:.1}% pixels changed (max {}); control band mean |diff| {}; animated={}",
            self.frame_a,
            self.frame_b,
            self.screen.width as f64,
            self.screen.height as f64,
            self.screen.mean_abs_diff,
            self.screen.changed_fraction * 100.0,
            self.screen.max_abs_diff,
            control,
            self.animated
        )
    }
}

// ---------------------------------------------------------------------------
// render result
// ---------------------------------------------------------------------------

/// A finished mockup render: the clip, the scene evidence, and the timeline hand-off.
#[derive(Debug, Clone, Serialize)]
pub struct MockupRender {
    /// Primary deliverable (clip.mp4, or ProRes 4444 when transparent).
    pub clip: PathBuf,
    pub frames: u32,
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    /// Render duration actually delivered.
    pub duration: Time,
    pub kind: SceneKind,
    pub engine: String,
    pub samples: u32,
    pub job_dir: PathBuf,
    pub script: PathBuf,
    /// The clip that went onto the surface.
    pub source: MediaInfo,
    /// Texture frames extracted from the source.
    pub texture_frames: u32,
    /// Frames the source was short of, filled by holding its last frame.
    pub extended_frames: u32,
    pub transparent: bool,
    pub measure: SurfaceMeasure,
    /// What ffprobe measured on the delivered file.
    pub output: MediaInfo,
    /// The clip as a timeline overlay source.
    pub overlay: OverlaySource,
    /// ffmpeg filtergraph that lays this clip back over a timeline frame.
    pub filtergraph: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub alpha: Option<alpha::AlphaCheck>,
}

impl MockupRender {
    pub fn frames_dir(&self) -> PathBuf {
        super::frames_dir(&self.job_dir)
    }

    /// The overlay spec this render hands to a timeline composite (full frame, centred).
    pub fn overlay_spec(&self) -> OverlaySpec {
        OverlaySpec {
            start: Time::ZERO,
            duration: None,
            anchor: Anchor::Center,
            margin: 0,
            width: Some(self.width & !1),
            opacity: 1.0,
            keep_audio: true,
        }
    }

    /// Composites this clip back over a base video with ffmpeg and verifies the result.
    pub fn composite_back(&self, base: &Path, out: &Path, spec: &OverlaySpec) -> Result<PathBuf> {
        alpha::composite_over_video(base, &self.overlay, spec, out)
    }

    /// Measures whether the mapped screen region actually changes between two rendered frames.
    pub fn surface_motion(&self, frame_a: u32, frame_b: u32) -> Result<SurfaceMotion> {
        measure_surface_motion(
            &self.frames_dir(),
            &self.measure,
            (self.width, self.height),
            frame_a,
            frame_b,
        )
    }

    /// First and middle frames: the cheapest proof that the texture is animated.
    pub fn surface_motion_sample(&self) -> Result<SurfaceMotion> {
        let first = self.measure.frames.first().map(|f| f.frame).unwrap_or(1);
        let last = self.measure.frames.last().map(|f| f.frame).unwrap_or(first);
        let middle = self
            .measure
            .frames
            .get(self.measure.frames.len() / 2)
            .map(|f| f.frame)
            .unwrap_or(last);
        self.surface_motion(first, middle)
    }

    pub fn describe(&self) -> String {
        format!(
            "{} mockup: {} frames {}x{} @{}fps ({:.2}s) from {} -> {}",
            self.kind.label(),
            self.frames,
            self.width,
            self.height,
            self.fps,
            self.duration.as_secs_f64(),
            self.source.describe(),
            self.clip.display()
        )
    }
}

// ---------------------------------------------------------------------------
// paths and identity
// ---------------------------------------------------------------------------

pub fn script_path(job_dir: &Path) -> PathBuf {
    job_dir.join("cinvar_mockup.py")
}

pub fn job_json_path(job_dir: &Path) -> PathBuf {
    job_dir.join("mockup_job.json")
}

pub fn texture_dir(job_dir: &Path) -> PathBuf {
    job_dir.join("tex")
}

pub fn measure_path(job_dir: &Path) -> PathBuf {
    job_dir.join("mockup_measure.json")
}

/// The extracted texture as an ffmpeg input pattern.
pub fn texture_pattern(job_dir: &Path) -> PathBuf {
    texture_dir(job_dir).join("frame_%04d.png")
}

/// File identity of the source clip, so a re-render over changed content is not served from a
/// stale job directory.
fn source_fingerprint(path: &Path) -> String {
    let meta = std::fs::metadata(path).ok();
    let len = meta.as_ref().map(|m| m.len()).unwrap_or(0);
    let modified = meta
        .and_then(|m| m.modified().ok())
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs())
        .unwrap_or(0);
    format!("{len}:{modified}")
}

/// Stable identity of a mockup request: the same request on the same source maps to the same
/// job directory, which is what makes an interrupted render resumable.
pub fn spec_hash(request: &MockupRequest, source: &MediaInfo) -> String {
    use sha2::{Digest, Sha256};
    let canonical = format!(
        "mockup-v{SCRIPT_VERSION}:{}:{}",
        serde_json::to_string(request).unwrap_or_default(),
        source_fingerprint(&source.path)
    );
    let digest = Sha256::digest(canonical.as_bytes());
    digest.iter().take(8).map(|b| format!("{b:02x}")).collect()
}

pub fn job_dir(base: &Path, request: &MockupRequest, source: &MediaInfo) -> PathBuf {
    base.join(spec_hash(request, source))
}

fn hex_channel(text: &str, at: usize) -> Option<f64> {
    let value = u8::from_str_radix(text.get(at..at + 2)?, 16).ok()? as f64 / 255.0;
    Some(if value <= 0.04045 {
        value / 12.92
    } else {
        ((value + 0.055) / 1.055).powf(2.4)
    })
}

/// Parses "#rgb" / "#rrggbb" into linear RGB for the driver.
pub fn accent_linear(hex: &str) -> [f64; 3] {
    let text = hex.trim().trim_start_matches('#');
    let fallback = [0.0782, 0.2623, 1.0];
    match text.len() {
        6 => match (
            hex_channel(text, 0),
            hex_channel(text, 2),
            hex_channel(text, 4),
        ) {
            (Some(r), Some(g), Some(b)) => [r, g, b],
            _ => fallback,
        },
        3 => {
            let expanded: String = text.chars().flat_map(|c| [c, c]).collect();
            match (
                hex_channel(&expanded, 0),
                hex_channel(&expanded, 2),
                hex_channel(&expanded, 4),
            ) {
                (Some(r), Some(g), Some(b)) => [r, g, b],
                _ => fallback,
            }
        }
        _ => fallback,
    }
}

fn even(v: u32) -> u32 {
    v & !1
}

/// Texture width: the source's own width, capped so a 4K source does not cost render time for
/// detail the screen never shows.
fn texture_width(source: &MediaInfo) -> u32 {
    let width = source.width.clamp(TEXTURE_MIN_WIDTH, TEXTURE_MAX_WIDTH);
    even(width.max(2))
}

impl MockupRequest {
    /// The job description handed to the scene driver.
    pub fn job(&self, source: &MediaInfo) -> MockupJob {
        let (width, height) = self.canvas();
        let frames = self.frame_count();
        let (display_w, display_h) = self.kind.nominal_display();
        let (tilt_x, tilt_z) = self.kind.tilt();
        MockupJob {
            kind: self.kind.as_str().into(),
            camera: self.camera.as_str().into(),
            camera_strength: self.camera_strength_clamped(),
            lighting: self.lighting.as_str().into(),
            light_strength: self.light_strength_clamped(),
            reflections: self.reflections_clamped(),
            f_stop: self.f_stop_clamped(),
            motion_blur: self.motion_blur_clamped(),
            engine: self.engine.as_str().into(),
            samples: self.samples_clamped(),
            denoise: true,
            transparent: self.transparent,
            width,
            height,
            fps: self.fps_clamped(),
            frame_start: 1,
            frame_end: frames,
            tex_frames: frames,
            aspect: source.aspect(),
            seed: self.seed,
            rig_z: self.kind.rig_z(),
            tilt_x_deg: tilt_x,
            tilt_z_deg: tilt_z,
            accent: accent_linear(&self.accent),
            nominal_display: [display_w, display_h],
        }
    }
}

// ---------------------------------------------------------------------------
// frames
// ---------------------------------------------------------------------------

fn png_complete(path: &Path) -> bool {
    use std::io::{Read, Seek, SeekFrom};
    let Ok(mut file) = std::fs::File::open(path) else {
        return false;
    };
    let mut head = [0u8; 8];
    if file.read_exact(&mut head).is_err()
        || head != [0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a]
    {
        return false;
    }
    let mut tail = [0u8; 12];
    if file.seek(SeekFrom::End(-12)).is_err() || file.read_exact(&mut tail).is_err() {
        return false;
    }
    &tail[4..8] == b"IEND"
}

fn count_complete(dir: &Path) -> u32 {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return 0;
    };
    entries
        .flatten()
        .filter(|e| {
            e.path().extension().and_then(|x| x.to_str()) == Some("png") && png_complete(&e.path())
        })
        .count() as u32
}

fn prune_incomplete(dir: &Path) -> u32 {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return 0;
    };
    let mut valid = 0;
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|x| x.to_str()) != Some("png") {
            continue;
        }
        if png_complete(&path) {
            valid += 1;
        } else {
            let _ = std::fs::remove_file(&path);
        }
    }
    valid
}

fn frame_path(dir: &Path, frame: u32) -> PathBuf {
    dir.join(format!("frame_{frame:04}.png"))
}

// ---------------------------------------------------------------------------
// texture extraction
// ---------------------------------------------------------------------------

/// Decodes part of the source clip into a frame-exact PNG sequence at the render fps.
/// Frames are numbered 1..=count so they line up with the render frames one to one.
fn extract_texture(
    request: &MockupRequest,
    source: &MediaInfo,
    job_dir: &Path,
) -> Result<(u32, u32)> {
    let want = request.frame_count();
    let dir = texture_dir(job_dir);
    paths::ensure_dir(&dir)?;
    // How many frames the source itself supplied is recorded next to the frames: a resumed job
    // must still be able to say how much of the clip was padded by holding the last frame.
    let natural_file = dir.join("natural.txt");
    let mut have = prune_incomplete(&dir);
    if have < want {
        have = run_extraction(request, source, job_dir)?;
        paths::write_atomic(&natural_file, have.to_string().as_bytes())?;
    }
    let natural = std::fs::read_to_string(&natural_file)
        .ok()
        .and_then(|text| text.trim().parse::<u32>().ok())
        .unwrap_or(have.min(want));
    // A source shorter than the requested duration keeps playing its last frame instead of
    // dropping to an empty screen.
    let last = frame_path(&dir, have.min(want));
    while have < want {
        if !last.is_file() {
            break;
        }
        have += 1;
        std::fs::copy(&last, frame_path(&dir, have))?;
    }
    Ok((have, want.saturating_sub(natural)))
}

fn run_extraction(request: &MockupRequest, source: &MediaInfo, job_dir: &Path) -> Result<u32> {
    let dir = texture_dir(job_dir);
    let ffmpeg = proc::require("ffmpeg")?;
    let width = texture_width(source);
    let start = request.start.as_secs_f64().max(0.0);
    let secs = request.duration_clamped().as_secs_f64();
    // -ss after -i decodes from the start, which is the honest way to get the exact in-point;
    // the fps filter then resamples to the render rate so frames map 1:1 onto render frames.
    let filter = format!(
        "fps={},scale={}:{}:flags=lanczos,setsar=1,format=rgb24",
        request.fps_clamped(),
        width,
        -2
    );
    let args: Vec<String> = vec![
        "-y".to_string(),
        "-hide_banner".to_string(),
        "-nostdin".to_string(),
        "-i".to_string(),
        source.path.to_string_lossy().to_string(),
        "-ss".to_string(),
        format!("{start:.6}"),
        "-t".to_string(),
        format!("{secs:.6}"),
        "-an".to_string(),
        "-vf".to_string(),
        filter,
        "-fps_mode".to_string(),
        "cfr".to_string(),
        "-start_number".to_string(),
        "1".to_string(),
        texture_pattern(job_dir).to_string_lossy().to_string(),
    ];
    let out = proc::run(&ffmpeg, &args, &ProcOpts::default().with_timeout(900))?;
    if !out.success {
        return Err(Error::new(
            ErrorCode::Render,
            format!(
                "ffmpeg could not decode the mockup source: {}",
                crate::secrets::redact(&out.tail_stderr(MAX_STDERR_LINES))
            ),
        ));
    }
    let have = count_complete(&dir);
    if have == 0 {
        return Err(Error::new(
            ErrorCode::Render,
            format!(
                "no texture frames were decoded from {} (is the in-point past the end of the clip?)",
                source.path.display()
            ),
        ));
    }
    Ok(have)
}

// ---------------------------------------------------------------------------
// render pipeline
// ---------------------------------------------------------------------------

fn read_measure(job_dir: &Path) -> Result<SurfaceMeasure> {
    let path = measure_path(job_dir);
    let text = std::fs::read_to_string(&path).map_err(|e| {
        Error::new(
            ErrorCode::Render,
            format!(
                "the mockup scene driver did not report what it built ({}: {e})",
                path.display()
            ),
        )
    })?;
    serde_json::from_str(&text).map_err(|e| {
        Error::new(
            ErrorCode::Render,
            format!("unreadable mockup measurement {}: {e}", path.display()),
        )
    })
}

fn log_tail(job_dir: &Path) -> String {
    let path = job_dir.join("blender.log");
    let Ok(text) = std::fs::read_to_string(&path) else {
        return "(no Blender log)".into();
    };
    let lines: Vec<&str> = text.lines().collect();
    let start = lines.len().saturating_sub(MAX_STDERR_LINES);
    lines[start..].join("\n")
}

fn render_timeout(request: &MockupRequest) -> std::time::Duration {
    let per_frame = match request.engine {
        MockupEngine::Cycles => 40,
        MockupEngine::Eevee => 8,
    };
    let secs = 300u64
        .saturating_add(request.frame_count() as u64 * per_frame)
        .min(6 * 3600);
    std::time::Duration::from_secs(secs)
}

fn run_blender(
    request: &MockupRequest,
    job_dir: &Path,
    total: u32,
    ctl: &RenderControl,
) -> Result<()> {
    let info = detect()?;
    let frames = super::frames_dir(job_dir);
    let log_path = job_dir.join("blender.log");
    let log = std::fs::File::create(&log_path)?;
    let mut cmd = std::process::Command::new(&info.path);
    cmd.args(["-b", "--factory-startup", "--python"])
        .arg(script_path(job_dir))
        .current_dir(job_dir)
        .stdin(std::process::Stdio::null())
        .stdout(log.try_clone()?)
        .stderr(log);
    let mut child = proc::spawn_retrying(&mut cmd)?;
    let timeout = render_timeout(request);
    let started = std::time::Instant::now();
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break status;
        }
        if (ctl.cancelled)() {
            let _ = child.kill();
            let _ = child.wait();
            return Err(
                Error::new(ErrorCode::Cancelled, "Blender mockup render cancelled")
                    .with_hint("completed frames are kept; re-run the same request to resume"),
            );
        }
        if started.elapsed() > timeout {
            let _ = child.kill();
            let _ = child.wait();
            return Err(Error::new(
                ErrorCode::Render,
                format!(
                    "Blender timed out after {}s on the mockup scene",
                    timeout.as_secs()
                ),
            )
            .with_hint(log_tail(job_dir)));
        }
        // Count only: Blender is still writing frames, and deleting one mid-write would throw
        // away a frame the render actually finished.
        (ctl.progress)(count_complete(&frames) as usize, total as usize);
        std::thread::sleep(std::time::Duration::from_millis(200));
    };
    if !status.success() {
        return Err(Error::new(
            ErrorCode::Render,
            format!(
                "Blender exited with status {:?} on the mockup scene",
                status.code()
            ),
        )
        .with_hint(log_tail(job_dir)));
    }
    Ok(())
}

fn encode_opaque(request: &MockupRequest, job_dir: &Path) -> Result<PathBuf> {
    let ffmpeg = proc::require("ffmpeg")?;
    let clip = job_dir.join("clip.mp4");
    let sequence = super::frames_pattern(job_dir);
    let args: Vec<String> = vec![
        "-y".to_string(),
        "-hide_banner".to_string(),
        "-nostdin".to_string(),
        "-framerate".to_string(),
        request.fps_clamped().to_string(),
        "-start_number".to_string(),
        "1".to_string(),
        "-i".to_string(),
        sequence.to_string_lossy().to_string(),
        "-c:v".to_string(),
        "libx264".to_string(),
        "-pix_fmt".to_string(),
        "yuv420p".to_string(),
        "-crf".to_string(),
        "18".to_string(),
        "-movflags".to_string(),
        "+faststart".to_string(),
        clip.to_string_lossy().to_string(),
    ];
    let out = proc::run(&ffmpeg, &args, &ProcOpts::default().with_timeout(1800))?;
    if !out.success {
        return Err(Error::new(
            ErrorCode::Render,
            format!(
                "ffmpeg could not encode the mockup clip: {}",
                crate::secrets::redact(&out.tail_stderr(MAX_STDERR_LINES))
            ),
        ));
    }
    Ok(clip)
}

fn encode_transparent(
    request: &MockupRequest,
    job_dir: &Path,
) -> Result<(PathBuf, alpha::AlphaCheck)> {
    let prores = job_dir.join("clip_prores4444.mov");
    let sequence = super::frames_pattern(job_dir).to_string_lossy().to_string();
    let encode = alpha::encode_alpha(&alpha::AlphaEncodeRequest {
        sequence: &sequence,
        start_number: 1,
        fps: request.fps_clamped(),
        prores: prores.clone(),
        webm: None,
    })?;
    let Some(check) = encode.checks.iter().find(|c| c.path == prores).cloned() else {
        return Err(Error::new(
            ErrorCode::Render,
            "the transparent mockup render produced no verified alpha clip".to_string(),
        ));
    };
    Ok((prores, check))
}

/// Renders a mockup: source clip -> 3D scene -> animation -> timeline hand-off.
pub fn render_mockup(
    request: &MockupRequest,
    base: &Path,
    ctl: &RenderControl,
) -> Result<MockupRender> {
    request.validate()?;
    let source = probe_media(&request.video)?;
    if source.width == 0 || source.height == 0 {
        return Err(Error::invalid(format!(
            "{} reports no frame size, so it cannot be mapped onto a surface",
            source.path.display()
        )));
    }
    if request.start.as_secs_f64() >= source.duration.as_secs_f64()
        && source.duration.as_secs_f64() > 0.0
    {
        return Err(Error::invalid(format!(
            "mockup in-point {:.3}s is past the end of the source clip ({:.3}s)",
            request.start.as_secs_f64(),
            source.duration.as_secs_f64()
        )));
    }
    let job = job_dir(base, request, &source);
    paths::ensure_dir(&job)?;
    paths::ensure_dir(&super::frames_dir(&job))?;
    let frame = request.frame_count();
    let (width, height) = request.canvas();

    // The measured job description is written first: a failed render can be diagnosed from it.
    let scene_job = request.job(&source);
    let job_json = serde_json::to_string_pretty(&scene_job)
        .map_err(|e| Error::internal(format!("mockup job is not serializable: {e}")))?;
    paths::write_atomic(&job_json_path(&job), job_json.as_bytes())?;
    paths::write_atomic(&script_path(&job), script_for(&scene_job).as_bytes())?;

    let (texture_frames, extended_frames) = extract_texture(request, &source, &job)?;
    let done_before = prune_incomplete(&super::frames_dir(&job));
    if done_before < frame {
        run_blender(request, &job, frame, ctl)?;
    }
    let done = prune_incomplete(&super::frames_dir(&job));
    (ctl.progress)(done as usize, frame as usize);
    if done < frame {
        return Err(Error::new(
            ErrorCode::Render,
            format!("Blender produced {done} of {frame} mockup frames"),
        )
        .with_hint(log_tail(&job)));
    }
    let measure = read_measure(&job)?;

    let (clip, alpha_check) = if request.transparent {
        let (clip, check) = encode_transparent(request, &job)?;
        (clip, Some(check))
    } else {
        (encode_opaque(request, &job)?, None)
    };
    let output = probe_media(&clip)?;
    let overlay = OverlaySource::file(clip.clone());
    let spec = OverlaySpec {
        start: Time::ZERO,
        duration: None,
        anchor: Anchor::Center,
        margin: 0,
        width: Some(even(width).max(2)),
        opacity: 1.0,
        keep_audio: true,
    };
    Ok(MockupRender {
        clip,
        frames: done,
        width,
        height,
        fps: request.fps_clamped(),
        duration: Time::from_secs_f64(done as f64 / request.fps_clamped() as f64),
        kind: request.kind,
        engine: scene_job.engine.clone(),
        samples: scene_job.samples,
        script: script_path(&job),
        job_dir: job,
        source,
        texture_frames,
        extended_frames,
        transparent: request.transparent,
        measure,
        output,
        overlay,
        filtergraph: alpha::overlay_filtergraph(&spec),
        alpha: alpha_check,
    })
}

/// Convenience wrapper for callers that do not need progress or cancellation.
pub fn render(request: &MockupRequest, base: &Path) -> Result<MockupRender> {
    render_mockup(request, base, &RenderControl::none())
}

// ---------------------------------------------------------------------------
// pixel measurement
// ---------------------------------------------------------------------------

/// Reads one region of a PNG as 8-bit luma through ffmpeg, so the measurement needs no image
/// decoder of its own and works on whatever Blender wrote.
fn gray_region(png: &Path, rect: Rect) -> Result<Vec<u8>> {
    let ffmpeg = proc::require("ffmpeg")?;
    let filter = format!(
        "crop={}:{}:{}:{},format=gray",
        rect.width, rect.height, rect.x, rect.y
    );
    let output = std::process::Command::new(ffmpeg)
        .args(["-v", "error", "-nostdin", "-i"])
        .arg(png)
        .args([
            "-vf",
            &filter,
            "-frames:v",
            "1",
            "-f",
            "rawvideo",
            "-pix_fmt",
            "gray",
            "-",
        ])
        .output()
        .map_err(|e| Error::io(format!("ffmpeg could not be started: {e}")))?;
    if !output.status.success() {
        return Err(Error::new(
            ErrorCode::Render,
            format!(
                "ffmpeg could not read {}: {}",
                png.display(),
                String::from_utf8_lossy(&output.stderr).trim()
            ),
        ));
    }
    let expected = rect.width as usize * rect.height as usize;
    if output.stdout.len() < expected {
        return Err(Error::new(
            ErrorCode::Render,
            format!(
                "reading {}x{} at ({},{}) from {} returned {} bytes, expected {expected}",
                rect.width,
                rect.height,
                rect.x,
                rect.y,
                png.display(),
                output.stdout.len()
            ),
        ));
    }
    Ok(output.stdout[..expected].to_vec())
}

fn diff_region(a: &[u8], b: &[u8], rect: Rect) -> RegionDiff {
    let mut total = 0u64;
    let mut changed = 0u64;
    let mut max = 0u8;
    for (left, right) in a.iter().zip(b.iter()) {
        let delta = left.abs_diff(*right);
        total += delta as u64;
        if delta > 6 {
            changed += 1;
        }
        max = max.max(delta);
    }
    let count = a.len().max(1) as f64;
    RegionDiff {
        x: rect.x,
        y: rect.y,
        width: rect.width,
        height: rect.height,
        mean_abs_diff: total as f64 / count,
        changed_fraction: changed as f64 / count,
        max_abs_diff: max,
    }
}

/// A band of the frame that the screen does not cover, used as the control in the motion test.
/// Two pixels of clearance keep the screen's antialiased edge out of the control sample.
fn control_rect(screen: Rect, canvas: (u32, u32)) -> Option<Rect> {
    let above = screen.y.saturating_sub(2).min(canvas.1);
    if above >= 8 {
        return Some(Rect {
            x: 0,
            y: 0,
            width: canvas.0,
            height: above,
        });
    }
    let below = screen
        .y
        .saturating_add(screen.height)
        .saturating_add(2)
        .min(canvas.1);
    if below + 8 <= canvas.1 {
        return Some(Rect {
            x: 0,
            y: below,
            width: canvas.0,
            height: canvas.1 - below,
        });
    }
    None
}

/// Measures the mapped region between two rendered frames, plus a control band outside it.
pub fn measure_surface_motion(
    frames_dir: &Path,
    measure: &SurfaceMeasure,
    canvas: (u32, u32),
    frame_a: u32,
    frame_b: u32,
) -> Result<SurfaceMotion> {
    let a = measure.at(frame_a).ok_or_else(|| {
        Error::invalid(format!("frame {frame_a} is not in the mockup measurement"))
    })?;
    let b = measure.at(frame_b).ok_or_else(|| {
        Error::invalid(format!("frame {frame_b} is not in the mockup measurement"))
    })?;
    let rect_a = a
        .rect(canvas)
        .ok_or_else(|| Error::invalid(format!("the screen is off-frame at frame {frame_a}")))?
        .inset(0.02);
    let rect_b = b
        .rect(canvas)
        .ok_or_else(|| Error::invalid(format!("the screen is off-frame at frame {frame_b}")))?
        .inset(0.02);
    let path_a = frame_path(frames_dir, frame_a);
    let path_b = frame_path(frames_dir, frame_b);
    let screen = diff_region(
        &gray_region(&path_a, rect_a)?,
        &gray_region(&path_b, rect_b)?,
        rect_a,
    );
    let control = match (control_rect(rect_a, canvas), control_rect(rect_b, canvas)) {
        (Some(control_a), Some(control_b)) => {
            let height = control_a.height.min(control_b.height);
            let area_a = Rect {
                x: 0,
                y: control_a.y.min(control_b.y),
                width: canvas.0,
                height,
            };
            let area_b = Rect {
                x: 0,
                y: control_b.y.min(control_a.y),
                width: canvas.0,
                height,
            };
            Some(diff_region(
                &gray_region(&path_a, area_a)?,
                &gray_region(&path_b, area_b)?,
                area_a,
            ))
        }
        _ => None,
    };
    let animated = screen.changed_fraction >= 0.05
        && screen.mean_abs_diff >= 2.0
        && control
            .as_ref()
            .map(|c| screen.mean_abs_diff >= 3.0 * c.mean_abs_diff + 0.5)
            .unwrap_or(true);
    Ok(SurfaceMotion {
        frame_a,
        frame_b,
        screen,
        control,
        animated,
    })
}
// ---------------------------------------------------------------------------
// tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn tools_or_skip(program: &str) -> Option<PathBuf> {
        let found = proc::which(program);
        if found.is_none() {
            eprintln!("{program} is not installed; skipping");
        }
        found
    }

    fn ffmpeg_media(dir: &Path, name: &str, lavfi: &str, seconds: f64, size: &str) -> PathBuf {
        let ffmpeg = proc::which("ffmpeg").expect("ffmpeg");
        let out = dir.join(name);
        let status = std::process::Command::new(ffmpeg)
            .args(["-v", "error", "-y", "-f", "lavfi", "-i"])
            .arg(lavfi)
            .args([
                "-t",
                &format!("{seconds}"),
                "-s",
                size,
                "-c:v",
                "libx264",
                "-pix_fmt",
                "yuv420p",
            ])
            .arg(&out)
            .status()
            .expect("ffmpeg runs");
        assert!(status.success(), "ffmpeg failed to build {name}");
        out
    }

    #[test]
    fn request_defaults_fill_in_from_a_tool_payload() {
        let json =
            r##"{"video": "/tmp/screen recording.mp4", "kind": "laptop", "duration": 3000000000}"##;
        let request: MockupRequest = serde_json::from_str(json).unwrap();
        assert_eq!(request.kind, SceneKind::Laptop);
        assert_eq!(request.video, PathBuf::from("/tmp/screen recording.mp4"));
        assert_eq!(request.duration.as_secs_f64(), 3.0);
        assert_eq!(request.camera, CameraMotion::Static);
        assert_eq!(request.lighting, LightingPreset::Studio);
        assert_eq!(request.engine, MockupEngine::Cycles);
        assert_eq!(request.fps, 30);
        assert_eq!(request.width, 1920);
        assert_eq!(request.accent, "#4f8cff");
        assert!(!request.transparent);
        let again: MockupRequest =
            serde_json::from_str(&serde_json::to_string(&request).unwrap()).unwrap();
        assert_eq!(again.kind, request.kind);
        assert_eq!(again.duration, request.duration);
    }

    #[test]
    fn canvas_and_frame_count_are_normalised() {
        let odd = MockupRequest::new("a.mp4", SceneKind::Phone).size(1279, 719);
        assert_eq!(odd.canvas(), (1278, 718));
        let tiny = MockupRequest::new("a.mp4", SceneKind::Phone).size(4, 9);
        assert_eq!(tiny.canvas(), (128, 128));
        let huge = MockupRequest::new("a.mp4", SceneKind::Phone).size(9000, 100);
        assert_eq!(huge.canvas().0, 4096);

        let request = MockupRequest::new("a.mp4", SceneKind::Laptop)
            .fps(24)
            .duration(2.5);
        assert_eq!(request.frame_count(), 60);
        // A single frame is impossible: two are the minimum a clip can show.
        assert_eq!(
            MockupRequest::new("a.mp4", SceneKind::Laptop)
                .fps(1)
                .duration(0.001)
                .frame_count(),
            2
        );
        assert_eq!(
            MockupRequest::new("a.mp4", SceneKind::Laptop)
                .fps(30)
                .duration(0.001)
                .frame_count(),
            3
        );
        // Absurd requests are capped, not queued.
        assert_eq!(
            MockupRequest::new("a.mp4", SceneKind::Laptop)
                .fps(120)
                .duration(10_000.0)
                .frame_count(),
            MAX_FRAMES
        );
        assert_eq!(
            MockupRequest::new("a.mp4", SceneKind::Laptop)
                .fps(0)
                .fps_clamped(),
            1
        );
        assert_eq!(
            MockupRequest::new("a.mp4", SceneKind::Laptop)
                .fps(240)
                .fps_clamped(),
            120
        );
        assert_eq!(
            MockupRequest::new("a.mp4", SceneKind::Laptop)
                .duration(0.0)
                .duration_clamped()
                .as_secs_f64(),
            MIN_DURATION_SECS
        );
    }

    #[test]
    fn a_caller_can_gate_the_duration_before_rendering() {
        let request = MockupRequest::new("a.mp4", SceneKind::Laptop).duration(12.0);
        assert_eq!(
            request.duration.as_secs_f64(),
            12.0,
            "the caller sees what it asked for"
        );
        let preview = request.clone().clamp_duration(3.0);
        assert_eq!(preview.duration.as_secs_f64(), 3.0);
        assert_eq!(preview.frame_count(), 90);
        // A shorter request is never stretched by the cap.
        assert_eq!(
            MockupRequest::new("a.mp4", SceneKind::Laptop)
                .duration(1.5)
                .clamp_duration(3.0)
                .duration
                .as_secs_f64(),
            1.5
        );
        assert_eq!(TOOL, "cinvar.blender.mockup_screen");
    }

    #[test]
    fn accent_colours_are_parsed_to_linear() {
        let accent = accent_linear("#4f8cff");
        assert!((accent[0] - 0.0782).abs() < 0.001, "{accent:?}");
        assert!((accent[1] - 0.2623).abs() < 0.001, "{accent:?}");
        assert!((accent[2] - 1.0).abs() < 0.001, "{accent:?}");
        // The default accent is parsed, not hard-coded twice.
        assert_eq!(accent, accent_linear(&MockupRequest::default().accent));
        assert_eq!(accent_linear("#fff"), [1.0, 1.0, 1.0]);
        assert_eq!(accent_linear("nonsense"), accent_linear(""));
    }

    #[test]
    fn the_job_describes_the_scene_the_driver_builds() {
        let source = MediaInfo {
            path: PathBuf::from("/tmp/clip.mp4"),
            width: 1920,
            height: 1080,
            fps: 30.0,
            duration: Time::from_secs_f64(10.0),
            ..MediaInfo::default()
        };
        let request = MockupRequest::new("/tmp/clip.mp4", SceneKind::Laptop)
            .fps(25)
            .duration(4.0)
            .size(1280, 720)
            .camera(CameraMotion::Orbit)
            .engine(MockupEngine::Eevee)
            .samples(96)
            .transparent(true);
        let job = request.job(&source);
        assert_eq!(job.kind, "laptop");
        assert_eq!(job.camera, "orbit");
        assert_eq!(job.engine, "eevee");
        assert_eq!(job.width, 1280);
        assert_eq!(job.height, 720);
        assert_eq!(job.fps, 25);
        assert_eq!(job.samples, 96);
        assert_eq!(job.frame_start, 1);
        assert_eq!(job.frame_end, 100);
        assert_eq!(job.frame_count(), 100);
        assert_eq!(job.tex_frames, 100);
        assert!((job.aspect - 16.0 / 9.0).abs() < 1e-6);
        assert_eq!(job.nominal_display, [2.20, 1.38]);
        assert_eq!(job.tilt_z_deg, 14.0);
        assert!(job.transparent);
        // The same request always produces the same job.
        assert_eq!(
            serde_json::to_string(&job).unwrap(),
            serde_json::to_string(&request.job(&source)).unwrap()
        );
    }

    #[test]
    fn the_script_is_versioned_and_stable() {
        let source = MediaInfo::default();
        let job = MockupRequest::new("/tmp/clip.mp4", SceneKind::GlassBrowser).job(&source);
        let script = script_for(&job);
        assert_eq!(script, script_for(&job), "the driver must be deterministic");
        assert!(script.contains(&format!("# script version: {SCRIPT_VERSION}")));
        assert!(script.contains("mockup_job.json"));
        assert!(script.contains("glass_browser"));
        assert!(script.contains("image_user.frame_duration = TEX_FRAMES"));
        assert!(script.contains("world_to_camera_view"));
        // No caller-supplied python: every scene kind is built by this driver.
        for kind in ["floating_screen", "glass_browser", "laptop", "phone"] {
            assert!(script.contains(kind), "driver does not build {kind}");
        }
    }

    #[test]
    fn the_driver_is_valid_python() {
        let Some(python) = tools_or_skip("python3") else {
            return;
        };
        let dir = tempfile::tempdir().unwrap();
        let source = MediaInfo::default();
        let job = MockupRequest::new("/tmp/clip.mp4", SceneKind::Phone).job(&source);
        let target = dir.path().join("cinvar_mockup.py");
        std::fs::write(&target, script_for(&job)).unwrap();
        let out = std::process::Command::new(python)
            .args(["-m", "py_compile"])
            .arg(&target)
            .output()
            .expect("python3 runs");
        assert!(
            out.status.success(),
            "generated driver is not valid python: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    #[test]
    fn the_job_hash_is_stable_and_reacts_to_every_input() {
        let dir = tempfile::tempdir().unwrap();
        let video = dir.path().join("clip.mp4");
        std::fs::write(&video, b"first").unwrap();
        let request = MockupRequest::new(&video, SceneKind::FloatingScreen).duration(3.0);
        let source = MediaInfo {
            path: video.clone(),
            ..MediaInfo::default()
        };
        let first = spec_hash(&request, &source);
        assert_eq!(first.len(), 16);
        assert_eq!(first, spec_hash(&request, &source));
        assert_eq!(
            job_dir(dir.path(), &request, &source),
            dir.path().join(&first)
        );

        let other_kind = MockupRequest {
            kind: SceneKind::Phone,
            ..request.clone()
        };
        assert_ne!(first, spec_hash(&other_kind, &source));

        let other_duration = MockupRequest {
            duration: Time::from_secs_f64(4.0),
            ..request.clone()
        };
        assert_ne!(first, spec_hash(&other_duration, &source));

        // New content behind the same path must not resume the old frames.
        std::thread::sleep(std::time::Duration::from_millis(1100));
        std::fs::write(&video, b"second, longer").unwrap();
        assert_ne!(first, spec_hash(&request, &source));
    }

    #[test]
    fn a_missing_source_is_rejected_before_blender_runs() {
        let dir = tempfile::tempdir().unwrap();
        let request = MockupRequest::new(dir.path().join("nope.mp4"), SceneKind::Laptop);
        let error = render(&request, dir.path()).unwrap_err();
        assert_eq!(error.code, ErrorCode::NotFound);
        assert!(error.message.contains("nope.mp4"), "{}", error.message);

        let empty = MockupRequest::default();
        assert_eq!(
            render(&empty, dir.path()).unwrap_err().code,
            ErrorCode::InvalidInput
        );
    }

    #[test]
    fn a_source_shorter_than_the_render_holds_its_last_frame() {
        if tools_or_skip("ffmpeg").is_none() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let video = ffmpeg_media(
            dir.path(),
            "short.mp4",
            "testsrc2=size=160x90:rate=12:duration=1",
            1.0,
            "160x90",
        );
        let source = probe_media(&video).unwrap();
        let request = MockupRequest::new(&video, SceneKind::Laptop)
            .fps(12)
            .duration(2.0);
        let job = dir.path().join("job");
        std::fs::create_dir_all(&job).unwrap();

        let (frames, extended) = extract_texture(&request, &source, &job).unwrap();
        assert_eq!(frames, 24, "2 s at 12 fps must produce 24 texture frames");
        assert!(
            (10..=13).contains(&extended),
            "a 1 s source should need about half the frames padded, got {extended}"
        );
        assert!(frame_path(&texture_dir(&job), 24).is_file());
        // The padded frames really are the last decoded frame, so the screen never goes empty.
        let last_natural = frame_path(&texture_dir(&job), 24 - extended);
        let padded = frame_path(&texture_dir(&job), 24);
        assert_eq!(
            std::fs::read(&last_natural).unwrap(),
            std::fs::read(&padded).unwrap()
        );

        // Re-running the same job is a no-op that still reports the same padding.
        let (again, extended_again) = extract_texture(&request, &source, &job).unwrap();
        assert_eq!(again, 24);
        assert_eq!(extended_again, extended);
    }

    #[test]
    fn probing_a_real_clip_reports_measured_values() {
        if tools_or_skip("ffmpeg").is_none() || tools_or_skip("ffprobe").is_none() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let video = ffmpeg_media(
            dir.path(),
            "source.mp4",
            "testsrc2=size=320x240:rate=25:duration=2",
            2.0,
            "320x240",
        );
        let info = probe_media(&video).unwrap();
        assert_eq!(info.width, 320);
        assert_eq!(info.height, 240);
        assert!((info.fps - 25.0).abs() < 0.01, "{}", info.fps);
        assert!(
            (info.duration.as_secs_f64() - 2.0).abs() < 0.15,
            "{}",
            info.duration.as_secs_f64()
        );
        assert_eq!(info.codec, "h264");
        assert!(info.describe().contains("320x240"));
        assert!((info.aspect() - 4.0 / 3.0).abs() < 1e-6);

        // A file with no video stream is an error, not an empty render.
        let empty = dir.path().join("audio-only.mp4");
        let ffmpeg = proc::which("ffmpeg").unwrap();
        let status = std::process::Command::new(ffmpeg)
            .args([
                "-v",
                "error",
                "-y",
                "-f",
                "lavfi",
                "-i",
                "sine=frequency=440:duration=1",
                "-c:a",
                "aac",
            ])
            .arg(&empty)
            .status()
            .unwrap();
        assert!(status.success());
        let error = probe_media(&empty).unwrap_err();
        assert_eq!(error.code, ErrorCode::InvalidInput);
    }

    #[test]
    fn measure_rects_are_clamped_to_the_canvas() {
        let frame = MeasureFrame {
            frame: 7,
            x: -4.0,
            y: 10.0,
            width: 100.0,
            height: 50.0,
            inside: false,
        };
        let rect = frame.rect((64, 48)).unwrap();
        assert_eq!(
            rect,
            Rect {
                x: 0,
                y: 10,
                width: 64,
                height: 38
            }
        );
        let inset = rect.inset(0.02);
        assert!(inset.x >= rect.x && inset.width < rect.width && inset.height < rect.height);
        let off = MeasureFrame {
            x: 100.0,
            width: 10.0,
            ..frame
        };
        assert!(off.rect((64, 48)).is_none());
        // Degenerate rectangles are left alone rather than inverted.
        let sliver = Rect {
            x: 0,
            y: 0,
            width: 3,
            height: 3,
        };
        assert_eq!(sliver.inset(0.5), sliver);
    }

    #[test]
    fn surface_motion_separates_a_playing_screen_from_a_still_one() {
        if tools_or_skip("ffmpeg").is_none() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let ffmpeg = proc::which("ffmpeg").unwrap();
        let make = |name: &str, filter: &str| -> PathBuf {
            let path = dir.path().join(name);
            let status = std::process::Command::new(&ffmpeg)
                .args([
                    "-v",
                    "error",
                    "-y",
                    "-f",
                    "lavfi",
                    "-i",
                    filter,
                    "-frames:v",
                    "1",
                ])
                .arg(&path)
                .status()
                .unwrap();
            assert!(status.success());
            path
        };
        // The background stays put; only the band the screen covers changes, exactly like a
        // still studio with a playing screen inside it.
        let dark = make(
            "a.png",
            "color=c=0x101010:s=64x64,drawbox=x=0:y=16:w=64:h=32:color=0xd0d0d0:t=fill",
        );
        let bright = make(
            "b.png",
            "color=c=0x101010:s=64x64,drawbox=x=0:y=16:w=64:h=32:color=0x181818:t=fill",
        );
        let frames = dir.path().join("frames");
        std::fs::create_dir_all(&frames).unwrap();
        for (index, source) in [(1u32, &dark), (2u32, &bright), (3u32, &dark)] {
            std::fs::copy(source, frames.join(format!("frame_{index:04}.png"))).unwrap();
        }
        let rect = MeasureFrame {
            frame: 1,
            x: 0.0,
            y: 16.0,
            width: 64.0,
            height: 32.0,
            inside: true,
        };
        let measure = SurfaceMeasure {
            blender: "4.0.2".into(),
            engine: "CYCLES".into(),
            samples: 8,
            screen_object: "CINVAR_SCREEN".into(),
            texture: "tex/frame_%04d.png".into(),
            texture_frames: 3,
            texture_frame_duration: 3,
            frames: vec![
                rect.clone(),
                MeasureFrame {
                    frame: 2,
                    ..rect.clone()
                },
                MeasureFrame { frame: 3, ..rect },
            ],
            visible_all: true,
            device_size: [1.0, 0.1, 1.0],
            mid_frame_rect: MeasureFrame {
                frame: 2,
                x: 0.0,
                y: 16.0,
                width: 64.0,
                height: 32.0,
                inside: true,
            },
        };
        let motion = measure_surface_motion(&frames, &measure, (64, 64), 1, 2).unwrap();
        assert!(motion.animated, "{}", motion.describe());
        assert!(motion.screen.mean_abs_diff > 100.0, "{}", motion.describe());
        assert_eq!(motion.screen.changed_fraction, 1.0);
        // The band above the screen is the control: it must not move.
        let control = motion
            .control
            .as_ref()
            .expect("a control band above the screen");
        assert_eq!(control.mean_abs_diff, 0.0, "{control:?}");

        let still = measure_surface_motion(&frames, &measure, (64, 64), 1, 3).unwrap();
        assert!(!still.animated, "{}", still.describe());
        assert_eq!(still.screen.mean_abs_diff, 0.0);
    }

    // -- real renders ------------------------------------------------------
    //
    // These drive Blender and ffmpeg end to end and take minutes, so they only run when
    // CINVAR_MOCKUP_E2E=1. Without it 'cargo test' stays fast and machine independent.

    fn e2e_enabled() -> bool {
        match std::env::var("CINVAR_MOCKUP_E2E").ok().as_deref() {
            Some("1") => true,
            _ => {
                eprintln!("CINVAR_MOCKUP_E2E=1 not set; skipping the real Blender mockup render");
                false
            }
        }
    }

    fn e2e_base(name: &str) -> PathBuf {
        let root = std::env::var("CINVAR_MOCKUP_E2E_OUT")
            .map(PathBuf::from)
            .unwrap_or_else(|_| std::env::temp_dir().join("cinvar-mockup-e2e"));
        let base = root.join(name);
        paths::ensure_dir(&base).unwrap();
        base
    }

    #[test]
    fn e2e_floating_screen_puts_a_real_recording_on_a_3d_surface() {
        if !e2e_enabled() {
            return;
        }
        let base = e2e_base("floating-screen");
        let source = ffmpeg_media(
            &base,
            "screen-recording.mp4",
            "testsrc2=size=640x360:rate=24:duration=8",
            8.0,
            "640x360",
        );
        let source_info = probe_media(&source).unwrap();
        eprintln!("source: {}", source_info.describe());

        // A static camera keeps the projected screen rectangle fixed, so the pixel difference
        // below can only come from the texture.
        let request = MockupRequest::new(&source, SceneKind::FloatingScreen)
            .size(960, 540)
            .fps(24)
            .duration(3.0)
            .camera(CameraMotion::Static)
            .motion_blur(0.0)
            .samples(48)
            .engine(MockupEngine::Eevee);
        let started = std::time::Instant::now();
        let render = render(&request, &base).unwrap();
        let elapsed = started.elapsed().as_secs_f64();
        eprintln!("{}", render.describe());
        eprintln!(
            "render took {:.1}s, output: {}",
            elapsed,
            render.output.describe()
        );

        assert_eq!(render.frames, 72);
        assert!(render.clip.is_file());
        assert_eq!(render.output.width, 960);
        assert_eq!(render.output.height, 540);
        assert_eq!(render.output.codec, "h264");
        assert!((render.output.duration.as_secs_f64() - 3.0).abs() < 0.25);
        assert_eq!(render.texture_frames, 72);
        assert!(render.measure.visible_all);
        assert_eq!(render.measure.frames.len(), 72);
        assert_eq!(render.measure.screen_object, "CINVAR_SCREEN");
        assert!(render.filtergraph.contains("overlay=x="));

        let motion = render.surface_motion(1, 49).unwrap();
        eprintln!("surface motion: {}", motion.describe());
        eprintln!("measure mid frame: {:?}", render.measure.mid_frame_rect);
        assert!(
            motion.animated,
            "the mapped screen region did not change between source times: {}",
            motion.describe()
        );
        let control = motion.control.as_ref().expect("control band");
        assert!(
            motion.screen.mean_abs_diff > 3.0 * control.mean_abs_diff,
            "{}",
            motion.describe()
        );

        // The timeline hand-off: the returned clip and filtergraph put it back on a track.
        let timeline = ffmpeg_media(
            &base,
            "timeline-base.mp4",
            "smptebars=size=960x540:rate=24:duration=3",
            3.0,
            "960x540",
        );
        let composed = base.join("composited-over-timeline.mp4");
        let out = render
            .composite_back(&timeline, &composed, &render.overlay_spec())
            .unwrap();
        eprintln!("composited back onto a timeline frame: {}", out.display());
        eprintln!("filtergraph: {}", render.filtergraph);
        assert!(out.is_file());
        assert_eq!(probe_media(&out).unwrap().width, 960);
    }

    #[test]
    fn e2e_laptop_renders_a_moving_full_length_clip() {
        if !e2e_enabled() {
            return;
        }
        let base = e2e_base("laptop");
        let source = ffmpeg_media(
            &base,
            "screen-recording.mp4",
            "testsrc2=size=1280x720:rate=24:duration=8",
            8.0,
            "1280x720",
        );
        let request = MockupRequest::new(&source, SceneKind::Laptop)
            .size(1280, 720)
            .fps(24)
            .duration(7.0)
            .camera(CameraMotion::Orbit)
            .lighting(LightingPreset::Studio)
            .reflections(0.6)
            .samples(48)
            .engine(MockupEngine::Eevee);
        let started = std::time::Instant::now();
        let render = render(&request, &base).unwrap();
        eprintln!(
            "{} (took {:.1}s)",
            render.describe(),
            started.elapsed().as_secs_f64()
        );
        eprintln!("output: {}", render.output.describe());
        eprintln!(
            "screen stayed inside the frame for {}/{} frames; device size {:?} m",
            render.measure.inside_frames(),
            render.measure.frames.len(),
            render.measure.device_size
        );

        assert_eq!(render.frames, 168);
        assert_eq!(render.measure.inside_frames(), 168);
        assert!((render.output.duration.as_secs_f64() - 7.0).abs() < 0.25);
        assert_eq!(render.output.codec, "h264");
        // The camera orbits, so the screen rectangle must move across the clip.
        let first = render.measure.frames.first().unwrap().x;
        let last = render.measure.frames.last().unwrap().x;
        assert!(
            (first - last).abs() > 1.0,
            "the orbit did not move the screen: {first} vs {last}"
        );
    }

    #[test]
    fn e2e_glass_browser_renders_with_a_glass_pane_and_motion_blur() {
        if !e2e_enabled() {
            return;
        }
        let base = e2e_base("glass-browser");
        let source = ffmpeg_media(
            &base,
            "screen-recording.mp4",
            "testsrc2=size=480x270:rate=24:duration=4",
            4.0,
            "480x270",
        );
        let request = MockupRequest::new(&source, SceneKind::GlassBrowser)
            .size(480, 270)
            .fps(24)
            .duration(1.5)
            .camera(CameraMotion::Handheld)
            .lighting(LightingPreset::Softbox)
            .reflections(0.8)
            .dof(2.0)
            .motion_blur(0.5)
            .samples(32)
            .engine(MockupEngine::Eevee);
        let started = std::time::Instant::now();
        let render = render(&request, &base).unwrap();
        eprintln!(
            "{} (took {:.1}s)",
            render.describe(),
            started.elapsed().as_secs_f64()
        );
        eprintln!("output: {}", render.output.describe());
        assert_eq!(render.frames, 36);
        assert_eq!(render.measure.inside_frames(), 36);
        assert!((render.output.duration.as_secs_f64() - 1.5).abs() < 0.2);
        assert_eq!(render.output.width, 480);
    }

    #[test]
    fn e2e_transparent_phone_delivers_a_verified_alpha_clip() {
        if !e2e_enabled() {
            return;
        }
        let support = alpha::detect_alpha_support().unwrap();
        if !support.any() {
            eprintln!("this ffmpeg cannot encode alpha; skipping the transparent render");
            return;
        }
        let base = e2e_base("transparent-phone");
        let source = ffmpeg_media(
            &base,
            "screen-recording.mp4",
            "testsrc2=size=360x640:rate=12:duration=3",
            3.0,
            "360x640",
        );
        let request = MockupRequest::new(&source, SceneKind::Phone)
            .size(320, 180)
            .fps(12)
            .duration(1.0)
            .transparent(true)
            .samples(8)
            .engine(MockupEngine::Eevee);
        let render = render(&request, &base).unwrap();
        eprintln!("{}", render.describe());
        let check = render
            .alpha
            .as_ref()
            .expect("a transparent render carries an alpha check");
        eprintln!(
            "alpha: {} {} verified={}",
            check.codec, check.pix_fmt, check.verified
        );
        assert!(render.clip.to_string_lossy().ends_with(".mov"));
        assert!(check.has_alpha_channel && check.verified);
        assert_eq!(render.frames, 12);
        assert!(render.transparent);
    }

    #[test]
    fn e2e_cycles_path_renders_the_scene_without_a_denoiser() {
        if !e2e_enabled() {
            return;
        }
        let base = e2e_base("cycles-path");
        let source = ffmpeg_media(
            &base,
            "screen-recording.mp4",
            "testsrc2=size=320x180:rate=24:duration=3",
            3.0,
            "320x180",
        );
        let request = MockupRequest::new(&source, SceneKind::FloatingScreen)
            .size(320, 180)
            .fps(24)
            .duration(1.0)
            .camera(CameraMotion::DollyIn)
            .dof(1.8)
            .motion_blur(0.4)
            .samples(16)
            .engine(MockupEngine::Cycles);
        let started = std::time::Instant::now();
        let render = render(&request, &base).unwrap();
        eprintln!(
            "{} on {} with {} samples (took {:.1}s)",
            render.describe(),
            render.engine,
            render.samples,
            started.elapsed().as_secs_f64()
        );
        assert_eq!(render.engine, "cycles");
        // Blender really ran Cycles, not something else.
        assert_eq!(render.measure.engine, "CYCLES");
        assert_eq!(render.frames, 24);
        assert_eq!(render.output.width, 320);
        // A dolly-in must make the screen rectangle grow across the clip.
        let first = render.measure.frames.first().unwrap().width;
        let last = render.measure.frames.last().unwrap().width;
        assert!(
            last > first * 1.05,
            "the dolly did not move in: {first} -> {last}"
        );
    }
}
