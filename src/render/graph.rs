use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::error::{Error, Result};
use crate::project::model::{
    CaptionTrack, Clip, Fit, MediaInfo, Project, TextAlign, TextOverlay, TextPosition, Timeline,
    Track, TrackKind,
};
use crate::project::store::ProjectStore;
use crate::util::time::{Fps, Time};

use super::anim;
use super::secs;

pub(super) struct GraphParams {
    pub width: u32,
    pub height: u32,
    pub fps: Fps,
    pub duration: Time,
    pub range_start: Time,
    pub background: String,
    pub burn_captions: bool,
    pub loudness_target: f64,
    pub sample_rate: u32,
}

pub(super) struct Graph {
    pub inputs: Vec<PathBuf>,
    pub input_args: Vec<Vec<String>>,
    pub filtergraph: String,
    pub video_label: String,
    pub audio_label: Option<String>,
    /// Audio-only graph ending at the pre-loudnorm mix, plus that label and the single-pass filter text.
    pub audio_measure: Option<AudioMeasure>,
    pub warnings: Vec<String>,
}

pub(super) struct AudioMeasure {
    pub graph: String,
    pub mix_label: String,
    pub single_pass: String,
}

struct Layer<'a> {
    z: i32,
    start: i64,
    clip: &'a Clip,
}

struct Builder<'a> {
    project: &'a Project,
    store: &'a ProjectStore,
    p: &'a GraphParams,
    inputs: Vec<PathBuf>,
    input_args: Vec<Vec<String>>,
    chains: Vec<String>,
    /// Audio filter chains, kept apart so loudness can be measured without rendering video.
    audio_chains: Vec<String>,
    warnings: Vec<String>,
    counter: usize,
    audio_inputs: BTreeMap<String, usize>,
    /// Render width / timeline width: timeline pixels (font sizes, offsets) are scaled by this so a
    /// preview looks like the export.
    sf: f64,
    /// First unrecoverable problem found while building; the render is refused with it.
    error: Option<Error>,
}

fn even(value: u32) -> u32 {
    value & !1
}

fn color_arg(raw: &str) -> String {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return "black".to_string();
    }
    let cleaned: String = trimmed
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '#' | '.' | '@' | 'x'))
        .collect();
    if cleaned.is_empty() {
        "black".to_string()
    } else if let Some(hex) = cleaned.strip_prefix('#') {
        format!("0x{hex}")
    } else {
        cleaned
    }
}

fn sanitize_color(raw: &str, fallback: &str) -> String {
    let cleaned: String = raw
        .trim()
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '#' | '.' | '@' | 'x'))
        .collect();
    if cleaned.is_empty() {
        fallback.to_string()
    } else if let Some(hex) = cleaned.strip_prefix('#') {
        format!("0x{hex}")
    } else {
        cleaned
    }
}

/// Gives FFmpeg a LUT path it can parse. Quote characters in a project path cannot be expressed
/// reliably through both filtergraph escaping layers, so the (already validated) LUT is copied to
/// a content-named file in the temp directory and that plain path is single-quoted.
fn stage_lut_for_filter(src: &std::path::Path) -> Option<String> {
    use sha2::{Digest, Sha256};
    let bytes = std::fs::read(src).ok()?;
    let digest: String = Sha256::digest(&bytes)
        .iter()
        .take(8)
        .map(|b| format!("{b:02x}"))
        .collect();
    let staged = std::env::temp_dir().join(format!("cinvar-lut-{digest}.cube"));
    if !staged.is_file() {
        std::fs::write(&staged, &bytes).ok()?;
    }
    let mut text = staged.to_string_lossy().to_string();
    if cfg!(windows) {
        // Backslashes and the drive colon are filtergraph syntax; forward slashes work on Windows.
        text = text.replace('\\', "/").replace(':', "\\:");
    }
    if text.contains('\'') {
        return None;
    }
    Some(format!("'{text}'"))
}

fn escape_text(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 8);
    for ch in text.chars() {
        match ch {
            // An apostrophe cannot live inside the filtergraph's '...' quoting: close the quote,
            // emit an escaped apostrophe (escaped once more for drawtext), reopen the quote.
            '\'' => out.push_str("'\\\\\\''"),
            '\\' | ':' | ',' | ';' | '[' | ']' | '%' => {
                out.push('\\');
                out.push(ch);
            }
            '\n' => out.push('\n'),
            '\r' | '\t' => out.push(' '),
            _ => out.push(ch),
        }
    }
    out
}

fn param_f64(value: &serde_json::Value, keys: &[&str]) -> Option<f64> {
    let obj = value.as_object()?;
    for key in keys {
        match obj.get(*key) {
            Some(serde_json::Value::Number(n)) => return n.as_f64(),
            Some(serde_json::Value::String(s)) => {
                if let Ok(parsed) = s.trim().parse::<f64>() {
                    return Some(parsed);
                }
            }
            Some(serde_json::Value::Bool(b)) => return Some(if *b { 1.0 } else { 0.0 }),
            _ => {}
        }
    }
    None
}

fn keyframe_value(clip: &Clip, property: &str) -> Option<f64> {
    clip.keyframe_track(property)
        .and_then(|track| track.keyframes.first())
        .map(|frame| frame.value)
}

/// Compiled animation expression for a property, when it has two or more keyframes.
fn animated_expr(clip: &Clip, property: &str, shift: f64) -> Result<Option<String>> {
    match clip.keyframe_track(property) {
        Some(track) if anim::is_animated(track) => anim::compile_shifted(track, shift).map(Some),
        _ => Ok(None),
    }
}

/// Drawtext argument list for one piece of on-screen text.
#[allow(clippy::too_many_arguments)]
fn drawtext(
    text: &str,
    size: u32,
    color: &str,
    outline_color: &str,
    outline_width: u32,
    bg_color: Option<&str>,
    position: TextPosition,
    align: TextAlign,
    font: Option<&str>,
    enable: Option<(f64, f64)>,
    motion: &TextMotion,
    sf: f64,
) -> String {
    let mut out = format!("drawtext=text='{}'", escape_text(text));
    out.push_str(&format!(":fontsize={}", size.max(8)));
    out.push_str(&format!(":fontcolor={}", sanitize_color(color, "white")));
    if outline_width > 0 {
        out.push_str(&format!(
            ":borderw={}",
            ((outline_width as f64 * sf).round() as u32).max(1)
        ));
        out.push_str(&format!(
            ":bordercolor={}",
            sanitize_color(outline_color, "black")
        ));
    }
    if let Some(bg) = bg_color {
        out.push_str(":box=1");
        out.push_str(&format!(":boxcolor={}", sanitize_color(bg, "black")));
        out.push_str(&format!(
            ":boxborderw={}",
            (12.0 * sf).round().max(2.0) as u32
        ));
    }
    if let Some(font) = font {
        let looks_like_path = font.contains('/')
            || font.contains('\\')
            || font.ends_with(".ttf")
            || font.ends_with(".otf");
        if looks_like_path {
            if Path::new(font).is_file() {
                out.push_str(&format!(":fontfile={}", escape_text(font)));
            }
        } else if !font.trim().is_empty() {
            out.push_str(&format!(":font={}", escape_text(font)));
        }
    }
    if text.contains('\n') {
        out.push_str(&format!(":line_spacing={}", (10.0 * sf).round() as i32));
        if supports_text_align() {
            out.push_str(match align {
                TextAlign::Left => ":text_align=left",
                TextAlign::Center => ":text_align=center",
                TextAlign::Right => ":text_align=right",
            });
        }
    }
    let mut x = match align {
        TextAlign::Left => format!("{:.0}", 64.0 * sf),
        TextAlign::Center => "(w-text_w)/2".to_string(),
        TextAlign::Right => format!("w-text_w-{:.0}", 64.0 * sf),
    };
    let mut y = match position {
        TextPosition::Top => format!("{:.0}", 56.0 * sf),
        TextPosition::Center => "(h-text_h)/2".to_string(),
        TextPosition::Bottom => format!("h-text_h-{:.0}", 72.0 * sf),
        TextPosition::LowerThird => "h*0.66".to_string(),
    };
    if let Some(dx) = &motion.x_off {
        x = format!("{x}+({dx})");
    }
    if let Some(dy) = &motion.y_off {
        y = format!("{y}+({dy})");
    }
    if motion.x_off.is_some() || motion.y_off.is_some() {
        out.push_str(&format!(":x='{x}':y='{y}'"));
    } else {
        out.push_str(&format!(":x={x}:y={y}"));
    }
    if let Some(alpha) = &motion.alpha {
        out.push_str(&format!(":alpha='{alpha}'"));
    }
    if let Some((start, end)) = enable {
        out.push_str(&format!(
            ":enable=between(t\\,{:.3}\\,{:.3})",
            start.max(0.0),
            end.max(0.0)
        ));
    }
    out
}

/// Conservative average glyph advance in em for width estimates. Bold capitals run near 0.68 em;
/// an optimistic 0.56 let a 135 px bold title fill 86% of the frame and a 170 px one clip.
const TEXT_EM_WIDTH: f64 = 0.68;

/// Motion expressions for a piece of text, all in seconds of text-layer time `t`.
#[derive(Default, Clone)]
struct TextMotion {
    x_off: Option<String>,
    y_off: Option<String>,
    alpha: Option<String>,
}

/// Whether this ffmpeg's drawtext knows `text_align` (added in ffmpeg 6.1).
fn supports_text_align() -> bool {
    static CACHE: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *CACHE.get_or_init(|| {
        std::process::Command::new("ffmpeg")
            .args(["-hide_banner", "-h", "filter=drawtext"])
            .output()
            .map(|o| String::from_utf8_lossy(&o.stdout).contains("text_align"))
            .unwrap_or(false)
    })
}

/// Greedy word wrap to at most `max_chars` per line; existing newlines are kept.
pub(super) fn wrap_text(text: &str, max_chars: usize) -> String {
    let max_chars = max_chars.max(4);
    let mut lines: Vec<String> = Vec::new();
    for paragraph in text.split('\n') {
        let mut line = String::new();
        for word in paragraph.split_whitespace() {
            let needed =
                line.chars().count() + usize::from(!line.is_empty()) + word.chars().count();
            if !line.is_empty() && needed > max_chars {
                lines.push(std::mem::take(&mut line));
            }
            if !line.is_empty() {
                line.push(' ');
            }
            line.push_str(word);
        }
        lines.push(line);
    }
    lines.join("\n")
}

/// Wrap `text` for the canvas and shrink the font only if a single word still overflows.
fn layout_text(text: &str, size: u32, canvas_width: u32, max_width_pct: f64) -> (String, u32) {
    let size = size.max(8);
    let usable = canvas_width as f64 * max_width_pct.clamp(0.1, 1.0);
    let max_chars = ((usable / (size as f64 * TEXT_EM_WIDTH)).floor() as usize).max(4);
    let wrapped = wrap_text(text, max_chars);
    let longest = wrapped
        .split('\n')
        .map(|l| l.chars().count())
        .max()
        .unwrap_or(1)
        .max(1);
    let fitted = fit_font_size(size, &"x".repeat(longest), canvas_width, max_width_pct);
    (wrapped, fitted)
}

/// Build the in/out/keyframe motion for a text clip. `shift` is how far into the clip the render starts.
fn text_motion(clip: &Clip, overlay: &TextOverlay, shift: f64, sf: f64) -> Result<TextMotion> {
    for track in &clip.keyframes {
        anim::check_property(&track.property)?;
        if matches!(track.property.as_str(), "scale" | "rotation") {
            return Err(Error::invalid(format!(
                "text clips cannot animate '{}'; animate x, y or opacity, or set animate_in/animate_out",
                track.property
            )));
        }
    }
    let total = clip.duration().as_secs_f64();
    let ramp = (total / 3.0).clamp(0.05, 0.5);
    let clock = format!("(t+{shift:.4})");
    let mut alpha_parts: Vec<String> = Vec::new();
    let mut y_parts: Vec<String> = Vec::new();
    let mut x_parts: Vec<String> = Vec::new();

    let kinds = [
        ("in", overlay.animate_in.as_deref()),
        ("out", overlay.animate_out.as_deref()),
    ];
    for (side, kind) in kinds {
        let Some(kind) = kind
            .map(|k| k.to_ascii_lowercase())
            .filter(|k| !k.is_empty() && k != "none")
        else {
            continue;
        };
        // progress 0..1 of the ramp, eased out
        let p = if side == "in" {
            format!("min(1,max(0,{clock}/{ramp:.4}))")
        } else {
            format!("min(1,max(0,({total:.4}-{clock})/{ramp:.4}))")
        };
        let eased = format!("(1-(1-{p})*(1-{p}))");
        match kind.as_str() {
            "fade" => alpha_parts.push(eased),
            "slide_up" | "slide_down" => {
                let sign = if (kind == "slide_up") == (side == "in") {
                    1
                } else {
                    -1
                };
                alpha_parts.push(eased.clone());
                y_parts.push(format!("{}*(1-{eased})*{:.2}", sign, 48.0 * sf));
            }
            other => {
                return Err(Error::invalid(format!(
                    "unknown text animation '{other}'; use none, fade, slide_up or slide_down"
                )));
            }
        }
    }
    if let Some(track) = clip.keyframe_track("opacity") {
        alpha_parts.push(format!(
            "max(0,min(1,{}))",
            anim::compile_shifted(track, shift)?
        ));
    }
    if let Some(track) = clip.keyframe_track("x") {
        x_parts.push(format!(
            "({})*{sf:.5}",
            anim::compile_shifted(track, shift)?
        ));
    }
    if let Some(track) = clip.keyframe_track("y") {
        y_parts.push(format!(
            "({})*{sf:.5}",
            anim::compile_shifted(track, shift)?
        ));
    }
    let join = |parts: Vec<String>, op: &str| {
        if parts.is_empty() {
            None
        } else {
            Some(parts.join(op))
        }
    };
    Ok(TextMotion {
        x_off: join(x_parts, "+"),
        y_off: join(y_parts, "+"),
        alpha: join(alpha_parts, "*"),
    })
}

/// Shrink the requested font size so a single line fits the allowed width.
fn fit_font_size(size: u32, text: &str, canvas_width: u32, max_width_pct: f64) -> u32 {
    let size = size.max(8);
    let usable = canvas_width as f64 * max_width_pct.clamp(0.1, 1.0);
    let chars = text.chars().count().max(1) as f64;
    let estimated = chars * size as f64 * TEXT_EM_WIDTH;
    if estimated <= usable {
        return size;
    }
    ((usable / (chars * TEXT_EM_WIDTH)).floor() as u32).clamp(8, size)
}

impl<'a> Builder<'a> {
    fn label(&mut self, prefix: &str) -> String {
        self.counter += 1;
        format!("{prefix}{}", self.counter)
    }

    fn fail(&mut self, err: Error) {
        if self.error.is_none() {
            self.error = Some(err);
        }
    }

    fn warn(&mut self, message: String) {
        if !self.warnings.contains(&message) {
            self.warnings.push(message);
        }
    }

    fn add_input(&mut self, path: &Path, seek: Time, len: Time, image: bool) -> usize {
        let mut args: Vec<String> = Vec::new();
        if image {
            args.push("-loop".into());
            args.push("1".into());
            args.push("-t".into());
            args.push(secs(len.as_secs_f64()));
        } else {
            if seek.nanos > 0 {
                args.push("-ss".into());
                args.push(seek.ffmpeg_timestamp());
            }
            args.push("-t".into());
            args.push(secs(len.as_secs_f64()));
        }
        args.push("-i".into());
        args.push(path.to_string_lossy().to_string());
        self.input_args.push(args);
        self.inputs.push(path.to_path_buf());
        self.inputs.len() - 1
    }

    fn clip_info(&self, clip: &Clip) -> Option<(PathBuf, MediaInfo, bool)> {
        let asset_id = clip.asset.as_ref()?;
        let asset = self.project.asset(asset_id)?;
        let path = match self.store.asset_path(asset) {
            Ok(p) => p,
            Err(_) => PathBuf::from(&asset.path),
        };
        if !path.is_file() {
            return None;
        }
        let image = matches!(asset.kind, crate::project::model::AssetKind::Image)
            || asset
                .info
                .as_ref()
                .map(|i| i.duration.nanos <= 0 && i.has_video)
                .unwrap_or(false);
        let info = match asset.info.clone() {
            Some(info) => info,
            None => crate::media::probe(&path).ok()?,
        };
        Some((path, info, image))
    }

    fn overlay_onto(&mut self, base: String, segment: String) -> String {
        let out = self.label("v");
        self.chains.push(format!(
            "[{base}][{segment}]overlay=eof_action=pass:repeatlast=0:format=auto[{out}]"
        ));
        out
    }

    fn video_segment(
        &mut self,
        clip: &Clip,
        _info: &MediaInfo,
        path: &Path,
        image: bool,
    ) -> Option<String> {
        let end_limit = self.p.range_start + self.p.duration;
        let vis_start = clip.timeline_start.max(self.p.range_start);
        let vis_end = clip.timeline_end().min(end_limit);
        if vis_end <= vis_start {
            return None;
        }
        let visible = vis_end - vis_start;
        let place = vis_start - self.p.range_start;
        let head_cut = vis_start - clip.timeline_start;

        let speed = if clip.speed.is_finite() && clip.speed > 0.01 {
            clip.speed
        } else {
            1.0
        };
        let source_window = if image {
            visible
        } else {
            let wanted = clip.source_duration();
            if wanted.nanos > 0 {
                wanted
            } else {
                visible.scale(speed)
            }
        };
        let input = self.add_input(path, clip.source_in, source_window, image);
        self.audio_inputs.insert(clip.id.clone(), input);

        let (w, h) = (self.p.width, self.p.height);
        let background = color_arg(&self.p.background);

        for track in &clip.keyframes {
            if let Err(err) = anim::check_property(&track.property) {
                self.fail(err);
                return None;
            }
        }
        let (ex_scale, ex_x, ex_y, ex_rot, ex_op) = match (
            animated_expr(clip, "scale", head_cut.as_secs_f64()),
            animated_expr(clip, "x", head_cut.as_secs_f64()),
            animated_expr(clip, "y", head_cut.as_secs_f64()),
            animated_expr(clip, "rotation", head_cut.as_secs_f64()),
            animated_expr(clip, "opacity", head_cut.as_secs_f64()),
        ) {
            (Ok(a), Ok(b), Ok(c), Ok(d), Ok(e)) => (a, b, c, d, e),
            (a, b, c, d, e) => {
                let err = [a.err(), b.err(), c.err(), d.err(), e.err()]
                    .into_iter()
                    .flatten()
                    .next();
                if let Some(err) = err {
                    self.fail(err);
                }
                return None;
            }
        };
        let opacity = keyframe_value(clip, "opacity")
            .unwrap_or(clip.transform.opacity)
            .clamp(0.0, 1.0);
        let scale = keyframe_value(clip, "scale").unwrap_or(clip.transform.scale);
        let pan_x = keyframe_value(clip, "x").unwrap_or(clip.transform.x as f64);
        let pan_y = keyframe_value(clip, "y").unwrap_or(clip.transform.y as f64);
        let mut rotation =
            clip.transform.rotation_deg + keyframe_value(clip, "rotation").unwrap_or(0.0);
        let head_source = head_cut.scale(speed);

        let mut steps: Vec<String> = Vec::new();
        steps.push("setpts=PTS-STARTPTS".to_string());
        if head_source.nanos > 0 && !image {
            steps.push(format!("trim=start={}", secs(head_source.as_secs_f64())));
            steps.push("setpts=PTS-STARTPTS".to_string());
        }
        if clip.reversed && !image {
            steps.push("reverse".to_string());
            steps.push("setpts=PTS-STARTPTS".to_string());
            if visible.as_secs_f64() > 60.0 {
                self.warn(format!(
                    "clip '{}' is reversed and {:.0}s long; the reverse filter buffers it in memory",
                    clip.name,
                    visible.as_secs_f64()
                ));
            }
        }
        if let Some(freeze) = clip.freeze_at {
            let at = (freeze - clip.source_in).clamp_non_negative();
            let epsilon = (1.0 / self.p.fps.as_f64().max(1.0)).max(0.001);
            steps.push(format!(
                "trim=start={}:duration={}",
                secs(at.as_secs_f64()),
                secs(epsilon)
            ));
            steps.push("setpts=PTS-STARTPTS".to_string());
            steps.push(format!(
                "tpad=stop_mode=clone:stop_duration={}",
                secs(visible.as_secs_f64())
            ));
            steps.push(format!("trim=duration={}", secs(visible.as_secs_f64())));
            steps.push("setpts=PTS-STARTPTS".to_string());
        } else if !image {
            let needed = visible.scale(speed);
            if needed.nanos < source_window.nanos.saturating_sub(1_000_000) {
                steps.push(format!("trim=duration={}", secs(needed.as_secs_f64())));
                steps.push("setpts=PTS-STARTPTS".to_string());
            }
            if (speed - 1.0).abs() > 1e-4 {
                steps.push(format!("setpts=PTS/{speed:.6}"));
            }
        }

        // Effects.
        let mut zoom = 1.0_f64;
        for effect in clip.effects.iter().filter(|e| e.enabled) {
            match effect.kind.as_str() {
                "brightness" => {
                    let b = param_f64(&effect.params, &["brightness", "value", "amount"])
                        .unwrap_or(0.0);
                    steps.push(format!("eq=brightness={:.4}", b.clamp(-1.0, 1.0)));
                }
                "contrast" => {
                    let c =
                        param_f64(&effect.params, &["contrast", "value", "amount"]).unwrap_or(1.0);
                    steps.push(format!("eq=contrast={:.4}", c.clamp(0.0, 4.0)));
                }
                "saturation" => {
                    let s = param_f64(&effect.params, &["saturation", "value", "amount"])
                        .unwrap_or(1.0);
                    steps.push(format!("eq=saturation={:.4}", s.clamp(0.0, 4.0)));
                }
                "gamma" => {
                    let g = param_f64(&effect.params, &["gamma", "value", "amount"]).unwrap_or(1.0);
                    steps.push(format!("eq=gamma={:.4}", g.clamp(0.1, 10.0)));
                }
                "eq" => {
                    let mut parts: Vec<String> = Vec::new();
                    if let Some(b) = param_f64(&effect.params, &["brightness"]) {
                        parts.push(format!("brightness={:.4}", b.clamp(-1.0, 1.0)));
                    }
                    if let Some(c) = param_f64(&effect.params, &["contrast"]) {
                        parts.push(format!("contrast={:.4}", c.clamp(0.0, 4.0)));
                    }
                    if let Some(s) = param_f64(&effect.params, &["saturation"]) {
                        parts.push(format!("saturation={:.4}", s.clamp(0.0, 4.0)));
                    }
                    if let Some(g) = param_f64(&effect.params, &["gamma"]) {
                        parts.push(format!("gamma={:.4}", g.clamp(0.1, 10.0)));
                    }
                    if parts.is_empty() {
                        self.warn(format!(
                            "clip '{}': eq effect had no recognised parameters and was skipped",
                            clip.name
                        ));
                    } else {
                        steps.push(format!("eq={}", parts.join(":")));
                    }
                }
                "blur" | "gblur" => {
                    let sigma =
                        param_f64(&effect.params, &["sigma", "value", "amount"]).unwrap_or(2.0);
                    steps.push(format!("gblur=sigma={:.3}", sigma.clamp(0.0, 50.0)));
                }
                "sharpen" | "unsharp" => {
                    let amount =
                        param_f64(&effect.params, &["amount", "value", "strength"]).unwrap_or(1.0);
                    steps.push(format!("unsharp=5:5:{:.3}:5:5:0", amount.clamp(0.0, 5.0)));
                }
                "lut" => {
                    // An explicit 3D LUT (usually baked from an OCIO transform). Only .cube files
                    // inside the project are followed; anything else is skipped with a warning.
                    let file = effect
                        .params
                        .get("file")
                        .and_then(|v| v.as_str())
                        .unwrap_or("");
                    let resolved = crate::util::paths::ensure_within(
                        &self.store.root,
                        std::path::Path::new(file),
                    )
                    .ok()
                    .filter(|p| {
                        p.extension()
                            .and_then(|e| e.to_str())
                            .map(|e| e.eq_ignore_ascii_case("cube"))
                            == Some(true)
                    })
                    .filter(|p| crate::color::validate_cube(p).is_ok());
                    match resolved.as_deref().and_then(stage_lut_for_filter) {
                        Some(quoted) => {
                            steps.push(format!("lut3d=file={quoted}:interp=tetrahedral"))
                        }
                        None => self.warn(format!(
                            "clip '{}': lut effect needs a valid .cube inside the project; skipped",
                            clip.name
                        )),
                    }
                }
                "vignette" => steps.push("vignette".to_string()),
                "grayscale" => steps.push("hue=s=0".to_string()),
                "sepia" => steps.push(
                    "colorchannelmixer=.393:.769:.189:0:.349:.686:.168:0:.272:.534:.131"
                        .to_string(),
                ),
                "crop" => {
                    let cw = param_f64(&effect.params, &["width", "w"]);
                    let ch = param_f64(&effect.params, &["height", "h"]);
                    match (cw, ch) {
                        (Some(cw), Some(ch)) if cw >= 8.0 && ch >= 8.0 => {
                            let cx = param_f64(&effect.params, &["x"]).unwrap_or(0.0).max(0.0);
                            let cy = param_f64(&effect.params, &["y"]).unwrap_or(0.0).max(0.0);
                            steps.push(format!(
                                "crop={}:{}:{:.0}:{:.0}",
                                even(cw as u32),
                                even(ch as u32),
                                cx,
                                cy
                            ));
                        }
                        _ => self.warn(format!(
                            "clip '{}': crop effect needs width and height; it was skipped",
                            clip.name
                        )),
                    }
                }
                "rotate" => {
                    let deg =
                        param_f64(&effect.params, &["degrees", "angle", "value"]).unwrap_or(0.0);
                    rotation += deg;
                }
                "zoom" => {
                    zoom *= param_f64(&effect.params, &["factor", "zoom", "value"]).unwrap_or(1.0);
                }
                "letterbox" => {}
                "speed" => self.warn(format!(
                    "clip '{}': the speed effect is ignored; use the clip speed setting instead",
                    clip.name
                )),
                other => self.warn(format!(
                    "clip '{}': effect '{other}' is not supported by the renderer and was skipped",
                    clip.name
                )),
            }
        }

        if let Some(expr) = &ex_rot {
            // Keyframed rotation: a fixed square output big enough for any angle.
            let a = format!("(({rotation:.6})+({expr}))*PI/180");
            steps.push("format=rgba".to_string());
            steps.push(format!(
                "rotate=a='{a}':ow='ceil(hypot(iw\\,ih)/2)*2':oh='ceil(hypot(iw\\,ih)/2)*2':c=black@0"
            ));
        } else if rotation.abs() > 0.01 {
            let rad = format!("{:.6}*PI/180", rotation);
            steps.push(format!(
                "rotate={rad}:ow=rotw({rad}):oh=roth({rad}):fillcolor=black"
            ));
        }

        let pan_x = pan_x * self.sf;
        let pan_y = pan_y * self.sf;
        let px = pan_x.round() as i32;
        let py = pan_y.round() as i32;
        // Normalise onto the canvas.
        let mut start = format!("[{}:v]", input);
        // Letterbox padding must be transparent so layers beneath stay visible (PiP, logos).
        let mut force_alpha = false;
        let geometry_animated = ex_scale.is_some() || ex_x.is_some() || ex_y.is_some();
        if geometry_animated {
            // Per-frame scale on a 2x supersampled canvas, placed with per-frame overlay
            // coordinates, then scaled down: this gives half-pixel motion precision.
            const SS: u32 = 2;
            let (sw, sh) = (w * SS, h * SS);
            steps.push(format!("fps={}", self.p.fps.ffprobe_fraction()));
            match clip.transform.fit {
                Fit::Cover => steps.push(format!(
                    "scale={sw}:{sh}:force_original_aspect_ratio=increase"
                )),
                Fit::Contain => steps.push(format!(
                    "scale={sw}:{sh}:force_original_aspect_ratio=decrease"
                )),
                Fit::Stretch => steps.push(format!("scale={sw}:{sh}")),
            }
            let scale_expr = match &ex_scale {
                Some(expr) => format!("max(0.05\\,min(8\\,({expr})*{zoom:.6}))"),
                None => format!("{:.6}", (scale * zoom).clamp(0.05, 8.0)),
            };
            steps.push(format!(
                "scale=w='trunc(iw*{scale_expr}/2)*2':h='trunc(ih*{scale_expr}/2)*2':eval=frame"
            ));
            steps.push("setsar=1".to_string());
            let moving = self.label("m");
            self.chains
                .push(format!("{start}{}[{}]", steps.join(","), moving));
            steps.clear();
            let canvas = self.label("c");
            self.chains.push(format!(
                "color=c=black@0.0:s={sw}x{sh}:r={}:d={},format=rgba[{}]",
                self.p.fps.ffprobe_fraction(),
                secs(visible.as_secs_f64()),
                canvas
            ));
            let x_off = match &ex_x {
                Some(expr) => format!("({expr})*{:.5}", self.sf),
                None => format!("{px}"),
            };
            let y_off = match &ex_y {
                Some(expr) => format!("({expr})*{:.5}", self.sf),
                None => format!("{py}"),
            };
            let placed = self.label("g");
            self.chains.push(format!(
                "[{canvas}][{moving}]overlay=x='(W-w)/2+({x_off})*{SS}':y='(H-h)/2+({y_off})*{SS}':eval=frame:eof_action=repeat:format=auto,scale={w}:{h}:flags=lanczos[{placed}]"
            ));
            start = format!("[{placed}]");
            force_alpha = true;
        } else {
            let total_scale = (scale * zoom).clamp(0.05, 8.0);
            let box_w = even((w as f64 * total_scale).round().max(2.0) as u32).max(16);
            let box_h = even((h as f64 * total_scale).round().max(2.0) as u32).max(16);
            match clip.transform.fit {
                Fit::Cover => {
                    steps.push(format!(
                        "scale={box_w}:{box_h}:force_original_aspect_ratio=increase"
                    ));
                    steps.push(format!(
                        "crop={w}:{h}:max(0\\,(in_w-{w})/2-{px}):max(0\\,(in_h-{h})/2-{py})"
                    ));
                }
                Fit::Stretch => {
                    steps.push(format!("scale={box_w}:{box_h}"));
                    if box_w != w || box_h != h || px != 0 || py != 0 {
                        force_alpha = true;
                        steps.push("format=rgba".to_string());
                        steps.push(format!(
                            "pad={w}:{h}:max(0\\,(ow-iw)/2+{px}):max(0\\,(oh-ih)/2+{py}):color=black@0"
                        ));
                    }
                }
                Fit::Contain => {
                    steps.push(format!(
                        "scale={box_w}:{box_h}:force_original_aspect_ratio=decrease"
                    ));
                    let exact_fit = px == 0
                        && py == 0
                        && (total_scale - 1.0).abs() < 1e-6
                        && _info.width > 0
                        && _info.height > 0
                        && (_info.width as u64 * h as u64 == _info.height as u64 * w as u64);
                    if !exact_fit {
                        force_alpha = true;
                        steps.push("format=rgba".to_string());
                    }
                    steps.push(format!(
                        "pad={w}:{h}:max(0\\,(ow-iw)/2+{px}):max(0\\,(oh-ih)/2+{py}):color={}",
                        if exact_fit {
                            background.clone()
                        } else {
                            "black@0".to_string()
                        }
                    ));
                }
            }
        }
        steps.push("setsar=1".to_string());
        if !geometry_animated {
            // The animated path already conformed the frame rate before its per-frame scale; a second
            // fps filter after the overlay drops the clip's final frame (a one-frame black flash).
            steps.push(format!("fps={}", self.p.fps.ffprobe_fraction()));
        }
        steps.push("format=yuv420p".to_string());

        // Transitions.
        let mut alpha = opacity < 0.999 || ex_op.is_some() || force_alpha;
        let mut tail_clone = Time::ZERO;
        if let Some(transition) = clip
            .transition_out
            .as_ref()
            .filter(|t| t.duration.nanos > 0)
        {
            let d = transition.duration.min(visible);
            let kind = transition.kind.to_ascii_lowercase();
            match kind.as_str() {
                "crossfade" | "dissolve" => {
                    tail_clone = d;
                    alpha = true;
                }
                "fade" | "fadeblack" | "fadewhite" => {
                    let start = (visible - d).as_secs_f64().max(0.0);
                    if kind == "fadewhite" {
                        steps.push(format!(
                            "fade=t=out:st={}:d={}:color=white",
                            secs(start),
                            secs(d.as_secs_f64())
                        ));
                    } else {
                        steps.push(format!(
                            "fade=t=out:st={}:d={}",
                            secs(start),
                            secs(d.as_secs_f64())
                        ));
                    }
                }
                other => {
                    self.warn(format!(
                        "clip '{}': transition '{other}' is rendered as a plain fade because the child clips sit end to end",
                        clip.name
                    ));
                    let start = (visible - d).as_secs_f64().max(0.0);
                    steps.push(format!(
                        "fade=t=out:st={}:d={}",
                        secs(start),
                        secs(d.as_secs_f64())
                    ));
                }
            }
        }
        if tail_clone.nanos > 0 {
            steps.push(format!(
                "tpad=stop_mode=clone:stop_duration={}",
                secs(tail_clone.as_secs_f64())
            ));
        }
        let mut alpha_in = 0.0_f64;
        if let Some(transition) = clip.transition_in.as_ref().filter(|t| t.duration.nanos > 0) {
            let d = transition.duration.min(visible);
            let kind = transition.kind.to_ascii_lowercase();
            match kind.as_str() {
                "crossfade" | "dissolve" => {
                    alpha = true;
                    alpha_in = d.as_secs_f64();
                }
                "fade" | "fadeblack" | "fadewhite" => {
                    if kind == "fadewhite" {
                        steps.push(format!(
                            "fade=t=in:st=0:d={}:color=white",
                            secs(d.as_secs_f64())
                        ));
                    } else {
                        steps.push(format!("fade=t=in:st=0:d={}", secs(d.as_secs_f64())));
                    }
                }
                other => {
                    self.warn(format!(
                        "clip '{}': transition '{other}' is rendered as a plain fade because the child clips sit end to end",
                        clip.name
                    ));
                    steps.push(format!("fade=t=in:st=0:d={}", secs(d.as_secs_f64())));
                }
            }
        }

        if alpha {
            let idx = steps
                .iter()
                .position(|s| s == "format=yuv420p")
                .unwrap_or(steps.len());
            steps[idx] = "format=rgba".to_string();
            if let Some(expr) = &ex_op {
                steps.push("format=yuva420p".to_string());
                steps.push(format!(
                    "geq=lum='lum(X,Y)':cb='cb(X,Y)':cr='cr(X,Y)':a='alpha(X,Y)*max(0,min(1,{}))'",
                    anim::with_time_var(expr, "T")
                ));
                steps.push("format=rgba".to_string());
            } else if opacity < 0.999 {
                steps.push(format!("colorchannelmixer=aa={opacity:.4}"));
            }
            if tail_clone.nanos > 0 {
                steps.push(format!(
                    "fade=t=out:st={}:d={}:alpha=1",
                    secs(visible.as_secs_f64()),
                    secs(tail_clone.as_secs_f64())
                ));
            }
            if alpha_in > 0.0 {
                steps.push(format!("fade=t=in:st=0:d={}:alpha=1", secs(alpha_in)));
            }
            steps.push("format=yuva420p".to_string());
        }

        steps.push(format!("setpts=PTS+{}/TB", secs(place.as_secs_f64())));

        let label = self.label("s");
        self.chains
            .push(format!("{start}{}[{}]", steps.join(","), label));
        Some(label)
    }

    fn text_segment(&mut self, clip: &Clip, overlay: &TextOverlay) -> Option<String> {
        let end_limit = self.p.range_start + self.p.duration;
        let vis_start = clip.timeline_start.max(self.p.range_start);
        let vis_end = clip.timeline_end().min(end_limit);
        if vis_end <= vis_start {
            return None;
        }
        let text = if overlay.text.trim().is_empty() {
            clip.name.clone()
        } else {
            overlay.text.clone()
        };
        if text.trim().is_empty() {
            return None;
        }
        let place = vis_start - self.p.range_start;
        let visible = vis_end - vis_start;
        let (text, size) = layout_text(
            &text,
            ((overlay.size as f64) * self.sf).round().max(8.0) as u32,
            self.p.width,
            overlay.max_width_pct,
        );
        let motion = match text_motion(
            clip,
            overlay,
            (vis_start - clip.timeline_start).as_secs_f64(),
            self.sf,
        ) {
            Ok(motion) => motion,
            Err(err) => {
                self.fail(err);
                return None;
            }
        };
        let draw = drawtext(
            &text,
            size,
            &overlay.color,
            &overlay.outline_color,
            overlay.outline_width,
            overlay.bg_color.as_deref(),
            overlay.position,
            overlay.align,
            overlay.font.as_deref(),
            None,
            &motion,
            self.sf,
        );
        let label = self.label("t");
        self.chains.push(format!(
            "color=c=black@0.0:s={}x{}:r={}:d={},format=rgba,{},setpts=PTS+{}/TB[{}]",
            self.p.width,
            self.p.height,
            self.p.fps.ffprobe_fraction(),
            secs(visible.as_secs_f64()),
            draw,
            secs(place.as_secs_f64()),
            label
        ));
        Some(label)
    }

    fn audio_segment(
        &mut self,
        track: &Track,
        clip: &Clip,
        info: &MediaInfo,
        path: &Path,
    ) -> Option<String> {
        if clip.audio.muted {
            return None;
        }
        let end_limit = self.p.range_start + self.p.duration;
        let vis_start = clip.timeline_start.max(self.p.range_start);
        let vis_end = clip.timeline_end().min(end_limit);
        if vis_end <= vis_start {
            return None;
        }
        let visible = vis_end - vis_start;
        let place = vis_start - self.p.range_start;
        let head_cut = vis_start - clip.timeline_start;
        let speed = if clip.speed.is_finite() && clip.speed > 0.01 {
            clip.speed
        } else {
            1.0
        };

        let input = match self.audio_inputs.get(&clip.id) {
            Some(idx) => *idx,
            None => {
                let source_window = clip.source_duration();
                let len = if source_window.nanos > 0 {
                    source_window
                } else {
                    visible.scale(speed)
                };
                let idx = self.add_input(path, clip.source_in, len, false);
                self.audio_inputs.insert(clip.id.clone(), idx);
                idx
            }
        };

        let head_source = head_cut.scale(speed);
        let mut steps: Vec<String> = vec!["asetpts=PTS-STARTPTS".to_string()];
        if head_source.nanos > 0 {
            steps.push(format!("atrim=start={}", secs(head_source.as_secs_f64())));
            steps.push("asetpts=PTS-STARTPTS".to_string());
        }
        let needed = visible.scale(speed);
        steps.push(format!("atrim=end={}", secs(needed.as_secs_f64())));
        steps.push("asetpts=PTS-STARTPTS".to_string());
        if (speed - 1.0).abs() > 1e-4 {
            steps.extend(tempo_chain(speed));
        }
        if clip.audio.normalize {
            // Per-clip loudness normalisation: single-pass loudnorm raises or lowers this
            // clip's own level before the track gain and the final mix normalisation.
            steps.push("loudnorm=I=-16:TP=-1.5:LRA=11".to_string());
        }
        let gain = clip.audio.gain_db + track.gain_db;
        if gain.abs() > 1e-3 {
            steps.push(format!("volume={gain:.2}dB"));
        }
        let visible_secs = visible.as_secs_f64();
        if clip.audio.fade_in.nanos > 0 {
            let d = clip.audio.fade_in.as_secs_f64().min(visible_secs);
            steps.push(format!("afade=t=in:st=0:d={}", secs(d)));
        }
        if clip.audio.fade_out.nanos > 0 {
            let d = clip.audio.fade_out.as_secs_f64().min(visible_secs);
            let st = (visible_secs - d).max(0.0);
            steps.push(format!("afade=t=out:st={}:d={}", secs(st), secs(d)));
        }
        let delay_ms = (place.as_secs_f64() * 1000.0).round().max(0.0) as u64;
        if delay_ms > 0 {
            steps.push(format!("adelay={delay_ms}|{delay_ms}:all=1"));
        }
        steps.push(format!(
            "aformat=sample_rates={}:channel_layouts=stereo",
            self.p.sample_rate.max(8_000)
        ));
        let label = self.label("a");
        self.audio_chains
            .push(format!("[{}:a]{}[{}]", input, steps.join(","), label));
        let _ = info;
        Some(label)
    }

    fn burn_captions(&mut self, timeline: &Timeline, base: String) -> String {
        if !self.p.burn_captions {
            return base;
        }
        let captions: Vec<&CaptionTrack> = self.project.captions.iter().collect();
        if captions.is_empty() {
            return base;
        }
        let mut current = base;
        let range_start = self.p.range_start.as_secs_f64();
        let range_end = range_start + self.p.duration.as_secs_f64();
        for track in captions {
            for cue in &track.cues {
                let start = cue.start.as_secs_f64();
                let end = cue.end.as_secs_f64();
                if end <= range_start || start >= range_end {
                    continue;
                }
                let text = if track.style.uppercase {
                    cue.text.to_uppercase()
                } else {
                    cue.text.clone()
                };
                if text.trim().is_empty() {
                    continue;
                }
                let (text, size) = layout_text(
                    &text,
                    ((track.style.size as f64) * self.sf).round().max(8.0) as u32,
                    self.p.width,
                    0.85,
                );
                let draw = drawtext(
                    &text,
                    size,
                    &track.style.color,
                    &track.style.outline_color,
                    track.style.outline_width,
                    track.style.background.as_deref(),
                    track.style.position,
                    TextAlign::Center,
                    track.style.font.as_deref(),
                    Some((start - range_start, end - range_start)),
                    &TextMotion::default(),
                    self.sf,
                );
                let out = self.label("cap");
                self.chains.push(format!("[{current}]{draw}[{out}]"));
                current = out;
            }
        }
        let _ = timeline;
        current
    }
}

fn tempo_chain(speed: f64) -> Vec<String> {
    let mut steps = Vec::new();
    let mut remaining = speed.max(0.01);
    while remaining > 2.0 {
        steps.push("atempo=2.0".to_string());
        remaining /= 2.0;
    }
    while remaining < 0.5 {
        steps.push("atempo=0.5".to_string());
        remaining /= 0.5;
    }
    steps.push(format!("atempo={:.6}", remaining.clamp(0.5, 2.0)));
    steps
}

pub(super) fn build(
    project: &Project,
    store: &ProjectStore,
    timeline: &Timeline,
    params: &GraphParams,
) -> Result<Graph> {
    if params.duration.nanos <= 0 {
        return Err(Error::invalid("the render range is empty"));
    }
    let mut builder = Builder {
        project,
        store,
        p: params,
        inputs: Vec::new(),
        input_args: Vec::new(),
        chains: Vec::new(),
        audio_chains: Vec::new(),
        warnings: Vec::new(),
        counter: 0,
        audio_inputs: BTreeMap::new(),
        error: None,
        sf: params.width as f64 / (timeline.width.max(1) as f64),
    };

    // Base canvas.
    let base = builder.label("vbase");
    builder.chains.push(format!(
        "color=c={}:s={}x{}:r={}:d={}[{}]",
        color_arg(&params.background),
        params.width,
        params.height,
        params.fps.ffprobe_fraction(),
        secs(params.duration.as_secs_f64()),
        base
    ));
    let mut current = base;

    // Collect visual layers in z order.
    let range_end = params.range_start + params.duration;
    let mut layers: Vec<Layer> = Vec::new();
    for track in &timeline.tracks {
        if track.hidden || track.kind == TrackKind::Audio {
            continue;
        }
        for clip in &track.clips {
            if clip.timeline_end() <= params.range_start || clip.timeline_start >= range_end {
                continue;
            }
            layers.push(Layer {
                z: track.z,
                start: clip.timeline_start.nanos,
                clip,
            });
        }
    }
    layers.sort_by(|a, b| a.z.cmp(&b.z).then(a.start.cmp(&b.start)));

    for layer in &layers {
        let clip = layer.clip;
        if let Some(overlay) = &clip.text {
            if let Some(segment) = builder.text_segment(clip, overlay) {
                current = builder.overlay_onto(current, segment);
            }
            continue;
        }
        if clip.asset.is_none() {
            builder.warn(format!("clip '{}' has no asset and was skipped", clip.name));
            continue;
        }
        let (path, info, image) = match builder.clip_info(clip) {
            Some(found) => found,
            None => {
                let name = clip
                    .asset
                    .as_ref()
                    .and_then(|id| project.asset(id))
                    .map(|a| a.display_name())
                    .unwrap_or_else(|| clip.name.clone());
                builder.warn(format!(
                    "clip '{}' skipped because '{name}' is missing",
                    clip.name
                ));
                continue;
            }
        };
        if !info.has_video {
            continue;
        }
        if let Some(segment) = builder.video_segment(clip, &info, &path, image) {
            current = builder.overlay_onto(current, segment);
        }
    }

    let video_label = builder.burn_captions(timeline, current);

    // Audio. Clips flagged duck_under_speech are compressed by a sidechain fed with all other audio.
    let mut audio_labels: Vec<String> = Vec::new();
    let mut ducked_labels: Vec<String> = Vec::new();
    for track in &timeline.tracks {
        if track.muted {
            continue;
        }
        let audio_track = track.kind == TrackKind::Audio;
        for clip in track.sorted_clips() {
            if clip.text.is_some() || clip.audio.muted {
                continue;
            }
            let (path, info, image) = match builder.clip_info(clip) {
                Some(found) => found,
                None => continue,
            };
            if !info.has_audio || image {
                if audio_track {
                    builder.warn(format!(
                        "audio clip '{}' has no audio stream and was skipped",
                        clip.name
                    ));
                }
                continue;
            }
            if let Some(label) = builder.audio_segment(track, clip, &info, &path) {
                if clip.audio.duck_under_speech {
                    ducked_labels.push(label);
                } else {
                    audio_labels.push(label);
                }
            }
        }
    }
    if !ducked_labels.is_empty() {
        if audio_labels.is_empty() {
            builder.warn(
                "clips are set to duck under speech but no other audio is playing, so nothing was ducked".to_string(),
            );
            audio_labels.append(&mut ducked_labels);
        } else {
            let speech = if audio_labels.len() == 1 {
                audio_labels[0].clone()
            } else {
                let inputs: String = audio_labels.iter().map(|l| format!("[{l}]")).collect();
                let out = builder.label("speech");
                builder.audio_chains.push(format!(
                    "{inputs}amix=inputs={}:normalize=0:duration=longest[{out}]",
                    audio_labels.len()
                ));
                out
            };
            // One copy continues into the mix, one drives each ducked clip's compressor.
            let copies: Vec<String> = (0..=ducked_labels.len())
                .map(|_| builder.label("sc"))
                .collect();
            let outs: String = copies.iter().map(|c| format!("[{c}]")).collect();
            // The sidechain filter stops when its sidechain input ends, so pad speech to full length.
            builder.audio_chains.push(format!(
                "[{speech}]apad=whole_dur={},asplit={}{outs}",
                secs(params.duration.as_secs_f64()),
                copies.len()
            ));
            audio_labels = vec![copies[0].clone()];
            for (n, ducked) in ducked_labels.iter().enumerate() {
                let out = builder.label("duck");
                builder.audio_chains.push(format!(
                    "[{ducked}][{}]sidechaincompress=threshold=0.02:ratio=12:attack=15:release=500:makeup=1[{out}]",
                    copies[n + 1]
                ));
                audio_labels.push(out);
            }
        }
    }

    let mut audio_label = None;
    let mut audio_measure = None;
    if !audio_labels.is_empty() {
        let mixed = if audio_labels.len() == 1 {
            audio_labels[0].clone()
        } else {
            let inputs: String = audio_labels
                .iter()
                .map(|label| format!("[{label}]"))
                .collect();
            let out = builder.label("amix");
            builder.audio_chains.push(format!(
                "{inputs}amix=inputs={}:normalize=0:duration=longest[{}]",
                audio_labels.len(),
                out
            ));
            out
        };
        let out = builder.label("aout");
        let single = format!("loudnorm=I={:.1}:TP=-1.5:LRA=11", params.loudness_target);
        audio_measure = Some(AudioMeasure {
            graph: builder.audio_chains.join(";"),
            mix_label: mixed.clone(),
            single_pass: single.clone(),
        });
        builder.audio_chains.push(format!(
            "[{mixed}]{single},aresample={}[{}]",
            params.sample_rate.max(8_000),
            out
        ));
        audio_label = Some(out);
    }

    if let Some(err) = builder.error.take() {
        return Err(err);
    }
    let warnings = builder.warnings.clone();
    Ok(Graph {
        inputs: builder.inputs,
        input_args: builder.input_args,
        filtergraph: builder
            .chains
            .iter()
            .chain(builder.audio_chains.iter())
            .cloned()
            .collect::<Vec<_>>()
            .join(";"),
        video_label,
        audio_label,
        audio_measure,
        warnings,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escapes_drawtext_specials() {
        assert_eq!(escape_text("a:b,c"), "a\\:b\\,c");
        assert_eq!(escape_text("it's"), "it'\\\\\\''s");
        // ...and ffmpeg really accepts it (an apostrophe used to break the whole render).
        let has_drawtext = cfg!(target_os = "linux")
            && crate::util::proc::which("ffmpeg").is_some()
            && std::process::Command::new("ffmpeg")
                .args(["-hide_banner", "-filters"])
                .output()
                .map(|o| String::from_utf8_lossy(&o.stdout).contains(" drawtext "))
                .unwrap_or(false);
        if has_drawtext {
            let filter = format!(
                "drawtext=text='{}':fontsize=20",
                escape_text("it's: [ok], \\yes;")
            );
            let out = std::process::Command::new("ffmpeg")
                .args([
                    "-v",
                    "error",
                    "-f",
                    "lavfi",
                    "-i",
                    "color=c=black:s=64x64:d=1",
                    "-vf",
                    &filter,
                    "-frames:v",
                    "1",
                    "-f",
                    "null",
                    "-",
                ])
                .output()
                .unwrap();
            assert!(
                out.status.success(),
                "{}",
                String::from_utf8_lossy(&out.stderr)
            );
        }
        assert_eq!(escape_text("100%"), "100\\%");
        assert_eq!(escape_text("line\nbreak"), "line\nbreak");
    }

    #[test]
    fn normalises_colours() {
        assert_eq!(color_arg("#101010"), "0x101010");
        assert_eq!(color_arg("black"), "black");
        assert_eq!(color_arg(""), "black");
        assert_eq!(sanitize_color("", "white"), "white");
    }

    #[test]
    fn shrinks_oversized_text() {
        assert_eq!(fit_font_size(48, "short", 1920, 0.85), 48);
        assert!(
            fit_font_size(
                200,
                "an extremely long caption line that will not fit",
                320,
                0.85
            ) < 200
        );
    }

    #[test]
    fn chains_atempo_for_extremes() {
        assert_eq!(tempo_chain(1.0), vec!["atempo=1.000000".to_string()]);
        for speed in [0.25, 0.5, 0.99, 1.0, 2.0, 4.0, 8.0, 20.0] {
            let chain = tempo_chain(speed);
            let mut product = 1.0_f64;
            for step in &chain {
                let factor: f64 = step.trim_start_matches("atempo=").parse().unwrap();
                assert!(
                    (0.5..=2.0).contains(&factor),
                    "each atempo stage stays inside 0.5-2.0, got {factor}"
                );
                product *= factor;
            }
            assert!(
                (product - speed).abs() < 1e-4,
                "stages multiply back to {speed}: {chain:?}"
            );
        }
    }
}
