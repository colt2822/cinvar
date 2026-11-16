//! Alpha-preserving encodes, delivered-alpha verification and overlay compositing.
//!
//! A transparent Blender clip is only useful if the *delivered* file still carries the
//! alpha channel: Blender writing RGBA PNGs proves nothing about the encode. Everything
//! here therefore measures the encoded artifact with ffprobe/ffmpeg instead of trusting
//! the renderer, and refuses to hand back a clip whose alpha was silently flattened.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::{Error, ErrorCode, Result};
use crate::util::proc::{self, ProcOpts};
use crate::util::time::Time;

pub const PRORES_ENCODER: &str = "prores_ks";
pub const PRORES_PROFILE: &str = "4444";
pub const PRORES_PIX_FMT: &str = "yuva444p10le";
pub const VP9_ENCODER: &str = "libvpx-vp9";
pub const VP9_PIX_FMT: &str = "yuva420p";

const ENCODE_TIMEOUT_SECS: u64 = 900;
const PROBE_TIMEOUT_SECS: u64 = 120;
const MAX_STDERR_LINES: usize = 12;

/// Which alpha-capable encoders the installed ffmpeg actually provides.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct EncoderSupport {
    /// "prores_ks" is present: ProRes 4444 (yuva444p10le) is available.
    pub prores_4444: bool,
    /// "libvpx-vp9" is present: alpha-preserving WebM (yuva420p) is available.
    pub vp9_alpha: bool,
}

impl EncoderSupport {
    pub fn any(self) -> bool {
        self.prores_4444 || self.vp9_alpha
    }

    pub fn labels(self) -> Vec<&'static str> {
        let mut out = Vec::new();
        if self.prores_4444 {
            out.push(PRORES_PROFILE);
        }
        if self.vp9_alpha {
            out.push("vp9-alpha");
        }
        out
    }
}

/// Which alpha-preserving encodes this ffmpeg was *observed* to deliver.
///
/// The encoder list alone is not enough: a build can advertise libvpx-vp9 (with yuva420p in
/// its pixel formats) and still write a WebM whose decoded alpha plane is missing. The probe
/// below encodes three tiny frames with each candidate and measures the alpha of the result,
/// so a render never discovers this after 200 frames.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AlphaSupport {
    /// ProRes 4444 verified: the encoded file decodes to a real alpha plane.
    pub prores_4444: bool,
    /// Alpha WebM verified: the encoded file decodes to a real alpha plane.
    pub vp9_alpha: bool,
}

impl AlphaSupport {
    pub fn any(self) -> bool {
        self.prores_4444 || self.vp9_alpha
    }
}

static ALPHA_SUPPORT: std::sync::OnceLock<Result<AlphaSupport>> = std::sync::OnceLock::new();

/// Verified alpha capability of the installed ffmpeg, probed once per process.
pub fn detect_alpha_support() -> Result<AlphaSupport> {
    ALPHA_SUPPORT.get_or_init(probe_alpha_support).clone()
}

fn probe_alpha_support() -> Result<AlphaSupport> {
    let listed = detect_encoders()?;
    let mut support = AlphaSupport::default();
    if listed.prores_4444 {
        support.prores_4444 = probe_encoder(
            PRORES_ENCODER,
            &[
                "-profile:v",
                PRORES_PROFILE,
                "-pix_fmt",
                PRORES_PIX_FMT,
                "-vendor",
                "apl0",
            ],
            "mov",
        )?;
    }
    if listed.vp9_alpha {
        support.vp9_alpha = probe_encoder(
            VP9_ENCODER,
            &[
                "-pix_fmt",
                VP9_PIX_FMT,
                "-b:v",
                "0",
                "-crf",
                "40",
                "-auto-alt-ref",
                "0",
            ],
            "webm",
        )?;
    }
    Ok(support)
}

/// Encodes three half-transparent frames and measures the delivered alpha plane.
fn probe_encoder(encoder: &str, extra: &[&str], extension: &str) -> Result<bool> {
    let ffmpeg = proc::require("ffmpeg")?;
    let probe_path = unique_temp_path(Path::new(encoder), &format!("probe.{extension}"));
    let mut args: Vec<String> = vec![
        "-y".into(),
        "-hide_banner".into(),
        "-nostdin".into(),
        "-v".into(),
        "error".into(),
        "-f".into(),
        "lavfi".into(),
        "-i".into(),
        "color=c=red@0.5:s=32x32:d=0.25:r=12,format=rgba".into(),
        "-c:v".into(),
        encoder.to_string(),
    ];
    args.extend(extra.iter().map(|a| a.to_string()));
    args.push(probe_path.to_string_lossy().to_string());
    let run = proc::run(
        &ffmpeg,
        &args,
        &ProcOpts::default().with_timeout(PROBE_TIMEOUT_SECS),
    );
    let delivered = run.map(|out| out.success).unwrap_or(false)
        && verify_alpha(&probe_path)
            .map(|check| check.verified)
            .unwrap_or(false);
    let _ = std::fs::remove_file(&probe_path);
    Ok(delivered)
}

/// Parses the output of "ffmpeg -encoders". Pure so the capability gate is unit-testable.
pub fn parse_encoder_list(text: &str) -> EncoderSupport {
    let mut support = EncoderSupport::default();
    for line in text.lines() {
        let mut fields = line.split_whitespace();
        let Some(_flags) = fields.next() else {
            continue;
        };
        let Some(name) = fields.next() else { continue };
        if name == PRORES_ENCODER {
            support.prores_4444 = true;
        }
        if name == VP9_ENCODER {
            support.vp9_alpha = true;
        }
    }
    support
}

/// Asks the installed ffmpeg which alpha-capable encoders it has. Never guesses.
pub fn detect_encoders() -> Result<EncoderSupport> {
    let ffmpeg = proc::require("ffmpeg")?;
    let args: Vec<String> = vec!["-hide_banner".into(), "-encoders".into()];
    let out = proc::run(
        &ffmpeg,
        &args,
        &ProcOpts::default().with_timeout(PROBE_TIMEOUT_SECS),
    )?;
    if !out.success {
        return Err(Error::new(
            ErrorCode::DependencyMissing,
            format!(
                "ffmpeg could not list its encoders: {}",
                crate::secrets::redact(&out.tail_stderr(MAX_STDERR_LINES))
            ),
        ));
    }
    Ok(parse_encoder_list(&format!(
        "{}\n{}",
        out.stdout, out.stderr
    )))
}

fn no_alpha_encoder(listed: EncoderSupport, support: AlphaSupport) -> Error {
    Error::new(
        ErrorCode::Unsupported,
        format!(
            "this ffmpeg cannot deliver alpha: prores_ks listed={} verified={}, libvpx-vp9 listed={} verified={}",
            listed.prores_4444, support.prores_4444, listed.vp9_alpha, support.vp9_alpha
        ),
    )
    .with_hint("install an ffmpeg whose prores_ks or libvpx-vp9 keeps the alpha plane, or accept the PNG sequence (frames/frame_%%04d.png) and composite the frames")
}

/// A PNG sequence to encode plus where the alpha-preserving outputs should land.
pub struct AlphaEncodeRequest<'a> {
    /// Input pattern, e.g. "/job/frames/frame_%04d.png".
    pub sequence: &'a str,
    pub start_number: u32,
    pub fps: u32,
    pub prores: PathBuf,
    pub webm: Option<PathBuf>,
}

/// Delivered alpha-preserving outputs, each with the verification that accepted it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AlphaEncode {
    pub prores: Option<PathBuf>,
    pub webm: Option<PathBuf>,
    /// What the encoder list offered.
    pub listed: EncoderSupport,
    /// What the probe verified the installed ffmpeg can actually deliver.
    pub support: AlphaSupport,
    pub checks: Vec<AlphaCheck>,
}

fn sequence_input(request: &AlphaEncodeRequest, args: &mut Vec<String>) {
    for arg in [
        "-framerate".to_string(),
        request.fps.max(1).to_string(),
        "-start_number".to_string(),
        request.start_number.to_string(),
        "-i".to_string(),
        request.sequence.to_string(),
    ] {
        args.push(arg);
    }
}

fn encode_args(request: &AlphaEncodeRequest, encoder_args: &[&str], out: &Path) -> Vec<String> {
    let mut args: Vec<String> = vec!["-y".into(), "-hide_banner".into(), "-nostdin".into()];
    sequence_input(request, &mut args);
    args.extend(encoder_args.iter().map(|a| a.to_string()));
    args.push(out.to_string_lossy().to_string());
    args
}

/// Encodes the frames as ProRes 4444 and/or alpha WebM, then verifies the delivered files.
/// Errors instead of quietly emitting an alpha-less clip.
pub fn encode_alpha(request: &AlphaEncodeRequest) -> Result<AlphaEncode> {
    let listed = detect_encoders()?;
    let support = detect_alpha_support()?;
    if !support.any() {
        return Err(no_alpha_encoder(listed, support));
    }
    let ffmpeg = proc::require("ffmpeg")?;
    let mut encode = AlphaEncode {
        prores: None,
        webm: None,
        listed,
        support,
        checks: Vec::new(),
    };
    let opts = ProcOpts::default().with_timeout(ENCODE_TIMEOUT_SECS);

    if support.prores_4444 {
        let args = encode_args(
            request,
            &[
                "-c:v",
                PRORES_ENCODER,
                "-profile:v",
                PRORES_PROFILE,
                "-pix_fmt",
                PRORES_PIX_FMT,
                "-vendor",
                "apl0",
                "-bits_per_mb",
                "8000",
            ],
            &request.prores,
        );
        let out = proc::run(&ffmpeg, &args, &opts)?;
        if !out.success {
            return Err(Error::new(
                ErrorCode::Render,
                format!(
                    "ffmpeg could not encode ProRes 4444: {}",
                    crate::secrets::redact(&out.tail_stderr(MAX_STDERR_LINES))
                ),
            ));
        }
        encode.prores = Some(request.prores.clone());
        encode.checks.push(verify_alpha(&request.prores)?);
    }

    if support.vp9_alpha {
        if let Some(webm) = request.webm.clone() {
            let args = encode_args(
                request,
                &[
                    "-c:v",
                    VP9_ENCODER,
                    "-pix_fmt",
                    VP9_PIX_FMT,
                    "-b:v",
                    "0",
                    "-crf",
                    "30",
                    "-auto-alt-ref",
                    "0",
                    "-row-mt",
                    "1",
                ],
                &webm,
            );
            let out = proc::run(&ffmpeg, &args, &opts)?;
            if !out.success {
                return Err(Error::new(
                    ErrorCode::Render,
                    format!(
                        "ffmpeg could not encode alpha WebM: {}",
                        crate::secrets::redact(&out.tail_stderr(MAX_STDERR_LINES))
                    ),
                ));
            }
            encode.webm = Some(webm.clone());
            encode.checks.push(verify_alpha(&webm)?);
        }
    }

    for check in &encode.checks {
        if !check.has_alpha_channel {
            return Err(Error::new(
                ErrorCode::Render,
                format!(
                    "{} was delivered without an alpha channel (pix_fmt {}): the transparent render would composite as a black box",
                    check.path.display(),
                    check.pix_fmt
                ),
            )
            .with_hint("re-run with an ffmpeg that keeps yuva444p10le/yuva420p, or render the PNG sequence and composite the frames directly"));
        }
        if check.all_zero {
            return Err(Error::new(
                ErrorCode::Render,
                format!(
                    "{} decoded to a fully transparent frame: nothing was rendered",
                    check.path.display()
                ),
            ));
        }
    }
    Ok(encode)
}

/// Proof that a delivered file really carries non-trivial alpha.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AlphaCheck {
    pub path: PathBuf,
    pub codec: String,
    pub pix_fmt: String,
    pub has_alpha_channel: bool,
    pub frames_sampled: usize,
    pub alpha_min: u8,
    pub alpha_max: u8,
    pub alpha_mean: f64,
    /// Share of pixels whose alpha is at least 50% opaque.
    pub coverage: f64,
    /// Alpha is present, not all-zero and not uniformly opaque.
    pub verified: bool,
    pub all_zero: bool,
    pub fully_opaque: bool,
}

/// True for pixel formats that carry a real alpha channel.
pub fn pix_fmt_has_alpha(pix_fmt: &str) -> bool {
    let lower = pix_fmt.to_ascii_lowercase();
    if lower.is_empty() || lower == "none" {
        return false;
    }
    lower.contains("yuva")
        || lower.contains("rgba")
        || lower.contains("argb")
        || lower.contains("abgr")
        || lower.contains("bgra")
        || lower.contains("gbrap")
        || lower.contains("ya8")
        || lower.contains("ya16")
}

#[derive(Debug, Clone, Default)]
struct Probe {
    codec: String,
    pix_fmt: String,
    width: u32,
    height: u32,
    duration: f64,
}

fn probe_clip(path: &Path) -> Result<Probe> {
    if !path.exists() {
        return Err(Error::not_found(format!(
            "no such file: {}",
            path.display()
        )));
    }
    let ffprobe = proc::require("ffprobe")?;
    let args: Vec<String> = vec![
        "-v".into(),
        "error".into(),
        "-select_streams".into(),
        "v:0".into(),
        "-show_entries".into(),
        "stream=codec_name,pix_fmt,width,height:format=duration".into(),
        "-of".into(),
        "json".into(),
        path.to_string_lossy().to_string(),
    ];
    let out = proc::run(
        &ffprobe,
        &args,
        &ProcOpts::default().with_timeout(PROBE_TIMEOUT_SECS),
    )?;
    if !out.success {
        return Err(Error::new(
            ErrorCode::Render,
            format!(
                "ffprobe could not read {}: {}",
                path.display(),
                crate::secrets::redact(&out.tail_stderr(3))
            ),
        ));
    }
    let value: serde_json::Value = serde_json::from_str(&out.stdout)
        .map_err(|e| Error::new(ErrorCode::Render, format!("ffprobe json: {e}")))?;
    let mut probe = Probe::default();
    if let Some(stream) = value
        .get("streams")
        .and_then(|s| s.as_array())
        .and_then(|s| s.first())
    {
        probe.codec = stream
            .get("codec_name")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        probe.pix_fmt = stream
            .get("pix_fmt")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        probe.width = stream.get("width").and_then(|v| v.as_u64()).unwrap_or(0) as u32;
        probe.height = stream.get("height").and_then(|v| v.as_u64()).unwrap_or(0) as u32;
    }
    probe.duration = value
        .get("format")
        .and_then(|f| f.get("duration"))
        .and_then(|d| d.as_str())
        .and_then(|d| d.parse::<f64>().ok())
        .unwrap_or(0.0);
    Ok(probe)
}

/// Decodes the alpha plane of one frame (or image) and measures it.
/// alphaextract plus a raw grey read is the ground truth: no estimate is involved.
pub fn alpha_plane_stats(image: &Path, at: Option<f64>) -> Result<(u8, u8, f64, f64)> {
    if let Some(stats) = alpha_plane_once(image, at)? {
        return Ok(stats);
    }
    // Very short files and containers with a stream delay can seek past their only frame.
    // Measuring frame 0 of the same delivered file is still a real measurement.
    if at.is_some() {
        if let Some(stats) = alpha_plane_once(image, None)? {
            return Ok(stats);
        }
    }
    Err(Error::new(
        ErrorCode::Render,
        format!("could not decode any frame of {}", image.display()),
    ))
}

/// One decode attempt: None means the file yielded no frame at that position.
fn alpha_plane_once(image: &Path, at: Option<f64>) -> Result<Option<(u8, u8, f64, f64)>> {
    let ffmpeg = proc::require("ffmpeg")?;
    let raw = unique_temp_path(image, "alpha.gray");
    let mut args: Vec<String> = vec!["-y".into(), "-hide_banner".into(), "-nostdin".into()];
    if let Some(seconds) = at.filter(|s| *s > 0.0) {
        args.push("-ss".into());
        args.push(format!("{seconds:.6}"));
    }
    for arg in [
        "-i".to_string(),
        image.to_string_lossy().to_string(),
        "-frames:v".to_string(),
        "1".to_string(),
        "-vf".to_string(),
        "alphaextract,format=gray".to_string(),
        "-f".to_string(),
        "rawvideo".to_string(),
        "-pix_fmt".to_string(),
        "gray".to_string(),
        raw.to_string_lossy().to_string(),
    ] {
        args.push(arg);
    }
    let out = proc::run(
        &ffmpeg,
        &args,
        &ProcOpts::default().with_timeout(PROBE_TIMEOUT_SECS),
    );
    let bytes = std::fs::read(&raw).unwrap_or_default();
    let _ = std::fs::remove_file(&raw);
    let out = out?;
    if !out.success || bytes.is_empty() {
        return Ok(None);
    }
    let mut min = 255u8;
    let mut max = 0u8;
    let mut sum = 0f64;
    let mut covered = 0usize;
    for byte in &bytes {
        min = min.min(*byte);
        max = max.max(*byte);
        sum += *byte as f64;
        if *byte as f64 / 255.0 >= 0.5 {
            covered += 1;
        }
    }
    Ok(Some((
        min,
        max,
        sum / bytes.len() as f64,
        covered as f64 / bytes.len() as f64,
    )))
}

fn unique_temp_path(source: &Path, suffix: &str) -> PathBuf {
    let stem = source
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("alpha");
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0);
    std::env::temp_dir().join(format!("cinvar-{stem}-{nanos}-{suffix}"))
}

/// Samples up to three frames of a delivered clip and measures the alpha plane of each.
pub fn verify_alpha(clip: &Path) -> Result<AlphaCheck> {
    let probe = probe_clip(clip)?;
    let has_alpha_channel = pix_fmt_has_alpha(&probe.pix_fmt);
    let sample_times: Vec<Option<f64>> = if probe.duration > 0.05 {
        vec![
            Some(probe.duration * 0.25),
            Some(probe.duration * 0.5),
            Some(probe.duration * 0.75),
        ]
    } else {
        vec![None]
    };
    let mut check = AlphaCheck {
        path: clip.to_path_buf(),
        codec: probe.codec,
        pix_fmt: probe.pix_fmt,
        has_alpha_channel,
        frames_sampled: 0,
        alpha_min: 255,
        alpha_max: 0,
        alpha_mean: 0.0,
        coverage: 0.0,
        verified: false,
        all_zero: false,
        fully_opaque: false,
    };
    if !has_alpha_channel {
        // Nothing to decode: the encode already flattened the alpha channel.
        return Ok(check);
    }
    let mut mean_sum = 0.0;
    for at in sample_times {
        let (min, max, mean, coverage) = alpha_plane_stats(clip, at)?;
        check.alpha_min = check.alpha_min.min(min);
        check.alpha_max = check.alpha_max.max(max);
        mean_sum += mean;
        check.coverage = check.coverage.max(coverage);
        check.frames_sampled += 1;
    }
    check.alpha_mean = mean_sum / check.frames_sampled.max(1) as f64;
    check.all_zero = check.alpha_max == 0;
    check.fully_opaque = check.alpha_min == 255;
    check.verified = check.has_alpha_channel && !check.all_zero && !check.fully_opaque;
    Ok(check)
}

/// Where an overlay comes from: an encoded clip or a PNG sequence (for timeline compositing).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum OverlaySource {
    File {
        path: PathBuf,
    },
    Sequence {
        /// "%04d" pattern, e.g. "/job/frames/frame_%04d.png".
        pattern: PathBuf,
        fps: u32,
        start_number: u32,
    },
}

impl OverlaySource {
    pub fn file(path: impl Into<PathBuf>) -> Self {
        OverlaySource::File { path: path.into() }
    }

    pub fn sequence(pattern: impl Into<PathBuf>, fps: u32, start_number: u32) -> Self {
        OverlaySource::Sequence {
            pattern: pattern.into(),
            fps: fps.max(1),
            start_number,
        }
    }

    fn input_args(&self) -> Vec<String> {
        match self {
            OverlaySource::File { path } => vec!["-i".into(), path.to_string_lossy().to_string()],
            OverlaySource::Sequence {
                pattern,
                fps,
                start_number,
            } => vec![
                "-framerate".into(),
                (*fps).max(1).to_string(),
                "-start_number".into(),
                start_number.to_string(),
                "-i".into(),
                pattern.to_string_lossy().to_string(),
            ],
        }
    }
}

/// Named placement of the overlay inside the base frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Anchor {
    TopLeft,
    TopCenter,
    TopRight,
    CenterLeft,
    Center,
    CenterRight,
    BottomLeft,
    BottomCenter,
    BottomRight,
}

impl Anchor {
    /// ffmpeg overlay x/y expressions; W/H are the base, w/h the overlay, m the margin.
    pub fn expressions(self, margin: u32) -> (String, String) {
        let m = margin.to_string();
        match self {
            Anchor::TopLeft => (m.clone(), m),
            Anchor::TopCenter => ("(W-w)/2".into(), m),
            Anchor::TopRight => (format!("W-w-{m}"), m),
            Anchor::CenterLeft => (m, "(H-h)/2".into()),
            Anchor::Center => ("(W-w)/2".into(), "(H-h)/2".into()),
            Anchor::CenterRight => (format!("W-w-{m}"), "(H-h)/2".into()),
            Anchor::BottomLeft => (m.clone(), format!("H-h-{m}")),
            Anchor::BottomCenter => ("(W-w)/2".into(), format!("H-h-{m}")),
            Anchor::BottomRight => (format!("W-w-{m}"), format!("H-h-{m}")),
        }
    }
}

/// How a transparent Blender clip is laid over an existing video.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OverlaySpec {
    /// When the overlay appears on the base timeline.
    pub start: Time,
    /// How long it stays visible; None keeps it until the overlay stream ends.
    pub duration: Option<Time>,
    pub anchor: Anchor,
    pub margin: u32,
    /// Scales the overlay to this width (aspect preserved). None keeps its own size.
    pub width: Option<u32>,
    /// 0.0..=1.0 multiplier on the overlay alpha.
    pub opacity: f32,
    /// Copy the base audio track through to the output.
    pub keep_audio: bool,
}

impl Default for OverlaySpec {
    fn default() -> Self {
        OverlaySpec {
            start: Time::from_secs_f64(0.0),
            duration: None,
            anchor: Anchor::Center,
            margin: 0,
            width: None,
            opacity: 1.0,
            keep_audio: true,
        }
    }
}

impl OverlaySpec {
    pub fn at(mut self, start: Time) -> Self {
        self.start = start;
        self
    }

    pub fn anchor(mut self, anchor: Anchor, margin: u32) -> Self {
        self.anchor = anchor;
        self.margin = margin;
        self
    }

    pub fn width(mut self, width: u32) -> Self {
        self.width = Some(width & !1);
        self
    }

    pub fn duration(mut self, duration: Time) -> Self {
        self.duration = Some(duration);
        self
    }
}

/// The filter_complex graph that lays input [1:v] over [0:v] with correct timestamps.
/// Pure so the overlay geometry is testable without ffmpeg.
pub fn overlay_filtergraph(spec: &OverlaySpec) -> String {
    let mut chain = String::from("[1:v]format=rgba");
    if let Some(width) = spec.width.filter(|w| *w >= 2) {
        chain.push_str(&format!(",scale={}:-2:flags=lanczos", width & !1));
    }
    let opacity = spec.opacity.clamp(0.0, 1.0) as f64;
    if opacity < 0.999 {
        chain.push_str(&format!(",colorchannelmixer=aa={opacity:.3}"));
    }
    let offset = spec.start.as_secs_f64().max(0.0);
    chain.push_str(&format!(",setpts=PTS-STARTPTS+{offset:.6}/TB[ov]"));
    let (x, y) = spec.anchor.expressions(spec.margin);
    let mut overlay =
        format!("[0:v][ov]overlay=x={x}:y={y}:format=auto:eof_action=pass:shortest=0");
    if let Some(duration) = spec.duration.filter(|d| d.as_secs_f64() > 0.0) {
        let end = offset + duration.as_secs_f64();
        overlay.push_str(&format!(":enable='between(t,{offset:.6},{end:.6})'"));
    }
    overlay.push_str("[v]");
    format!("{chain};{overlay}")
}

fn require_file(path: &Path, what: &str) -> Result<()> {
    if path.is_file() {
        Ok(())
    } else {
        Err(Error::not_found(format!(
            "{what} does not exist: {}",
            path.display()
        )))
    }
}

/// Composites a transparent clip (ProRes 4444, alpha WebM or a PNG sequence) over an
/// existing video with an ffmpeg overlay filtergraph, then verifies the delivered file.
pub fn composite_over_video(
    base: &Path,
    overlay: &OverlaySource,
    spec: &OverlaySpec,
    out: &Path,
) -> Result<PathBuf> {
    require_file(base, "base video")?;
    match overlay {
        OverlaySource::File { path } => require_file(path, "overlay clip")?,
        OverlaySource::Sequence { pattern, .. } => {
            if pattern.to_string_lossy().contains('%') {
                let parent = pattern.parent().unwrap_or(Path::new("."));
                let any = std::fs::read_dir(parent)
                    .map(|entries| {
                        entries
                            .filter_map(|e| e.ok())
                            .any(|e| e.path().extension().and_then(|x| x.to_str()) == Some("png"))
                    })
                    .unwrap_or(false);
                if !any {
                    return Err(Error::not_found(format!(
                        "overlay image sequence is empty: {}",
                        pattern.display()
                    )));
                }
            } else {
                require_file(pattern, "overlay image sequence")?;
            }
        }
    }
    let base_probe = probe_clip(base)?;
    if base_probe.pix_fmt.is_empty() {
        return Err(Error::invalid(format!(
            "{} has no video stream to composite over",
            base.display()
        )));
    }
    let ffmpeg = proc::require("ffmpeg")?;
    if let Some(parent) = out.parent() {
        crate::util::paths::ensure_dir(parent)?;
    }
    let mut args: Vec<String> = vec!["-y".into(), "-hide_banner".into(), "-nostdin".into()];
    args.push("-i".into());
    args.push(base.to_string_lossy().to_string());
    args.extend(overlay.input_args());
    for arg in [
        "-filter_complex".to_string(),
        overlay_filtergraph(spec),
        "-map".to_string(),
        "[v]".to_string(),
    ] {
        args.push(arg);
    }
    if spec.keep_audio {
        args.push("-map".into());
        args.push("0:a?".into());
        args.push("-c:a".into());
        args.push("copy".into());
    }
    for arg in [
        "-c:v".to_string(),
        "libx264".to_string(),
        "-pix_fmt".to_string(),
        "yuv420p".to_string(),
        "-crf".to_string(),
        "18".to_string(),
        "-preset".to_string(),
        "medium".to_string(),
        "-movflags".to_string(),
        "+faststart".to_string(),
        out.to_string_lossy().to_string(),
    ] {
        args.push(arg);
    }
    let run = proc::run(
        &ffmpeg,
        &args,
        &ProcOpts::default().with_timeout(ENCODE_TIMEOUT_SECS),
    )?;
    if !run.success {
        return Err(Error::new(
            ErrorCode::Render,
            format!(
                "ffmpeg could not composite the overlay: {}",
                crate::secrets::redact(&run.tail_stderr(MAX_STDERR_LINES))
            ),
        ));
    }
    verify_composite(base, out)?;
    Ok(out.to_path_buf())
}

/// Confirms the composited file kept the base timeline (duration + resolution + frames).
pub fn verify_composite(base: &Path, out: &Path) -> Result<()> {
    let base_probe = probe_clip(base)?;
    let out_probe = probe_clip(out)?;
    if out_probe.width == 0 || out_probe.height == 0 {
        return Err(Error::new(
            ErrorCode::Render,
            format!("composited output has no video stream: {}", out.display()),
        ));
    }
    if (out_probe.width, out_probe.height) != (base_probe.width, base_probe.height) {
        return Err(Error::new(
            ErrorCode::Render,
            format!(
                "composited output is {}x{} but the base is {}x{}",
                out_probe.width, out_probe.height, base_probe.width, base_probe.height
            ),
        ));
    }
    if base_probe.duration > 0.0 {
        let drift = (out_probe.duration - base_probe.duration).abs();
        if drift > 0.25 {
            return Err(Error::new(
                ErrorCode::Render,
                format!(
                    "composited output is {:.2}s but the base is {:.2}s: the overlay changed the timeline",
                    out_probe.duration, base_probe.duration
                ),
            )
            .with_hint("check the overlay filtergraph timestamps (setpts/offset)"));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const ENCODERS: &str = "Encoders:
 V..... = Video
 A..... = Audio
 ------
 VF...D prores                 Apple ProRes
 VFS... prores_ks              Apple ProRes (iCodec Pro)
 V....D libvpx-vp9             libvpx VP9
";

    #[test]
    fn detects_alpha_capable_encoders() {
        let support = parse_encoder_list(ENCODERS);
        assert!(support.prores_4444);
        assert!(support.vp9_alpha);
        assert!(support.any());
        let without = parse_encoder_list(" V....D libx264  H.264\n");
        assert!(!without.prores_4444 && !without.vp9_alpha);
        assert!(!without.any());
        assert!(without.labels().is_empty());
    }

    #[test]
    fn alpha_pixel_formats_are_recognised() {
        assert!(pix_fmt_has_alpha("yuva444p10le"));
        assert!(pix_fmt_has_alpha("yuva420p"));
        assert!(pix_fmt_has_alpha("rgba"));
        assert!(pix_fmt_has_alpha("gbrapf32le"));
        assert!(!pix_fmt_has_alpha("yuv420p"));
        assert!(!pix_fmt_has_alpha("yuv444p10le"));
        assert!(!pix_fmt_has_alpha(""));
    }

    #[test]
    fn overlay_graph_offsets_and_places_the_clip() {
        let spec = OverlaySpec::default()
            .at(Time::from_secs_f64(2.5))
            .anchor(Anchor::BottomRight, 32)
            .width(641)
            .duration(Time::from_secs_f64(1.5));
        let graph = overlay_filtergraph(&spec);
        assert!(graph.contains("setpts=PTS-STARTPTS+2.500000/TB"), "{graph}");
        assert!(graph.contains("scale=640:-2"), "{graph}");
        assert!(graph.contains("x=W-w-32:y=H-h-32"), "{graph}");
        assert!(graph.contains("eof_action=pass"), "{graph}");
        assert!(graph.contains("between(t,2.500000,4.000000)"), "{graph}");
        assert!(graph.contains("[0:v][ov]overlay"), "{graph}");
        assert!(graph.ends_with("[v]"), "{graph}");
    }

    #[test]
    fn overlay_graph_uses_centre_by_default_and_keeps_opaque_alpha() {
        let graph = overlay_filtergraph(&OverlaySpec::default());
        assert!(graph.contains("x=(W-w)/2:y=(H-h)/2"), "{graph}");
        assert!(!graph.contains("colorchannelmixer"), "{graph}");
        assert!(!graph.contains("enable="), "{graph}");
    }

    #[test]
    fn overlay_opacity_reaches_the_alpha_channel() {
        let mut spec = OverlaySpec::default();
        spec.opacity = 0.5;
        assert!(overlay_filtergraph(&spec).contains("colorchannelmixer=aa=0.500"));
    }

    #[test]
    fn sequence_overlay_inputs_are_ordered() {
        let source = OverlaySource::sequence("/job/frames/frame_%04d.png", 0, 1);
        assert_eq!(
            source.input_args(),
            vec![
                "-framerate".to_string(),
                "1".to_string(),
                "-start_number".to_string(),
                "1".to_string(),
                "-i".to_string(),
                "/job/frames/frame_%04d.png".to_string(),
            ]
        );
    }

    #[test]
    fn missing_inputs_are_reported_not_encoded() {
        let dir = tempfile::tempdir().unwrap();
        let err = composite_over_video(
            &dir.path().join("base.mov"),
            &OverlaySource::file(dir.path().join("overlay.mov")),
            &OverlaySpec::default(),
            &dir.path().join("out.mp4"),
        )
        .unwrap_err();
        assert_eq!(err.code, ErrorCode::NotFound);
    }

    #[test]
    fn empty_sequence_is_reported() {
        let dir = tempfile::tempdir().unwrap();
        let base = dir.path().join("base.mp4");
        std::fs::write(&base, b"not a video").unwrap();
        let err = composite_over_video(
            &base,
            &OverlaySource::sequence(dir.path().join("frames/frame_%04d.png"), 24, 1),
            &OverlaySpec::default(),
            &dir.path().join("out.mp4"),
        )
        .unwrap_err();
        assert_eq!(err.code, ErrorCode::NotFound);
        assert!(err.message.contains("empty"), "{err}");
    }
}
