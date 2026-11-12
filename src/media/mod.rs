use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::{Error, ErrorCode, Result};
use crate::project::model::{MediaInfo, Orientation};
use crate::util::paths;
use crate::util::proc::{self, ProcOpts};
use crate::util::time::{parse_fps, Fps, Time};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SceneCut {
    pub at: Time,
    pub score: f32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Span {
    pub start: Time,
    pub end: Time,
    pub detail: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProbeSummary {
    pub path: String,
    pub info: MediaInfo,
    pub orientation: Orientation,
}

const PROBE_TIMEOUT_SECS: u64 = 120;
const ANALYZE_TIMEOUT_SECS: u64 = 900;
const PRODUCE_TIMEOUT_SECS: u64 = 1_800;

fn ffmpeg_bin() -> Result<PathBuf> {
    proc::require("ffmpeg")
        .map_err(|e| e.with_hint("install ffmpeg (version 6 or newer) and run: cinvar setup"))
}

fn ffprobe_bin() -> Result<PathBuf> {
    proc::require("ffprobe")
        .map_err(|e| e.with_hint("ffprobe ships with ffmpeg; install ffmpeg and run: cinvar setup"))
}

/// Common ffmpeg flags. Verbose mode is required by the detection filters, which
/// report their findings through the ffmpeg logger at info level.
fn ffmpeg_argv(extra: Vec<String>, verbose: bool) -> Vec<String> {
    let level = if verbose { "info" } else { "error" };
    let mut args: Vec<String> = vec![
        "-hide_banner".into(),
        "-nostdin".into(),
        "-nostats".into(),
        "-loglevel".into(),
        level.into(),
    ];
    args.extend(extra);
    args
}

fn run_ffmpeg(extra: Vec<String>, verbose: bool, timeout_secs: u64) -> Result<proc::ProcOutput> {
    let bin = ffmpeg_bin()?;
    let opts = ProcOpts::default().with_timeout(timeout_secs);
    proc::run(&bin, &ffmpeg_argv(extra, verbose), &opts)
}

fn run_ffprobe(extra: Vec<String>) -> Result<proc::ProcOutput> {
    let bin = ffprobe_bin()?;
    let opts = ProcOpts::default().with_timeout(PROBE_TIMEOUT_SECS);
    proc::run(&bin, &extra, &opts)
}

fn require_file(path: &Path, what: &str) -> Result<()> {
    if path.is_file() {
        Ok(())
    } else {
        Err(Error::not_found(format!(
            "{what} not found: {}",
            path.display()
        )))
    }
}

fn tool_failure(code: ErrorCode, what: &str, out: &proc::ProcOutput) -> Error {
    let tail = out.tail_stderr(12);
    let detail = crate::secrets::redact(tail.trim());
    let message = if detail.is_empty() {
        let exit = out
            .code
            .map(|c| c.to_string())
            .unwrap_or_else(|| "?".into());
        format!("{what} failed (exit {exit})")
    } else {
        format!("{what} failed: {detail}")
    };
    Error::new(code, message)
}

fn verified_output(out: &Path) -> Result<PathBuf> {
    match std::fs::metadata(out) {
        Ok(meta) if meta.len() > 0 => Ok(out.to_path_buf()),
        Ok(_) => Err(Error::new(
            ErrorCode::Render,
            format!("ffmpeg wrote an empty file: {}", out.display()),
        )),
        Err(_) => Err(Error::new(
            ErrorCode::Render,
            format!("ffmpeg did not write {}", out.display()),
        )
        .with_hint("check that the output folder exists and is writable")),
    }
}

fn prepare_out(out: &Path) -> Result<()> {
    if let Some(parent) = out.parent() {
        if !parent.as_os_str().is_empty() {
            paths::ensure_dir(parent)?;
        }
    }
    Ok(())
}

/// Read the number that follows a key in a log line, e.g. "silence_end: 12.5".
fn number_after(hay: &str, key: &str) -> Option<f64> {
    let idx = hay.find(key)?;
    let rest = &hay[idx + key.len()..];
    let rest = rest.trim_start_matches(|c: char| c == ':' || c == '=' || c.is_whitespace());
    let end = rest
        .find(|c: char| {
            !(c.is_ascii_digit() || c == '.' || c == '-' || c == '+' || c == 'e' || c == 'E')
        })
        .unwrap_or(rest.len());
    if end == 0 {
        return None;
    }
    rest[..end].parse::<f64>().ok()
}

fn json_str<'a>(value: &'a serde_json::Value, key: &str) -> Option<&'a str> {
    value.get(key).and_then(|v| v.as_str())
}

fn json_f64(value: &serde_json::Value, key: &str) -> Option<f64> {
    match value.get(key) {
        Some(serde_json::Value::Number(n)) => n.as_f64(),
        Some(serde_json::Value::String(s)) => s.trim().parse::<f64>().ok(),
        _ => None,
    }
}

fn json_u64(value: &serde_json::Value, key: &str) -> Option<u64> {
    match value.get(key) {
        Some(serde_json::Value::Number(n)) => n.as_u64(),
        Some(serde_json::Value::String(s)) => s.trim().parse::<u64>().ok(),
        _ => None,
    }
}

fn stream_fps(stream: &serde_json::Value) -> Option<Fps> {
    for key in ["r_frame_rate", "avg_frame_rate"] {
        if let Some(raw) = json_str(stream, key) {
            let raw = raw.trim();
            if raw.is_empty() || raw.starts_with("0/") || raw == "0" {
                continue;
            }
            if let Ok(fps) = parse_fps(raw) {
                if fps.is_valid() {
                    return Some(fps);
                }
            }
        }
    }
    None
}

fn stream_rotation(stream: &serde_json::Value) -> i32 {
    if let Some(list) = stream.get("side_data_list").and_then(|v| v.as_array()) {
        for entry in list {
            if let Some(rot) = json_f64(entry, "rotation") {
                return rot.round() as i32;
            }
        }
    }
    if let Some(tags) = stream.get("tags") {
        if let Some(raw) = json_str(tags, "rotate") {
            if let Ok(rot) = raw.trim().parse::<f64>() {
                return rot.round() as i32;
            }
        }
    }
    0
}

fn parse_probe(text: &str) -> Result<MediaInfo> {
    let root: serde_json::Value = serde_json::from_str(text)
        .map_err(|e| Error::invalid(format!("ffprobe returned unreadable json: {e}")))?;
    let streams = root
        .get("streams")
        .and_then(|v| v.as_array())
        .ok_or_else(|| Error::invalid("ffprobe reported no streams for this file"))?;
    if streams.is_empty() {
        return Err(Error::invalid("ffprobe reported no streams for this file"));
    }
    let video = streams
        .iter()
        .find(|s| json_str(s, "codec_type") == Some("video"))
        .cloned();
    let audio = streams
        .iter()
        .find(|s| json_str(s, "codec_type") == Some("audio"))
        .cloned();
    let format = root
        .get("format")
        .cloned()
        .unwrap_or(serde_json::Value::Null);

    let mut info = MediaInfo {
        format: json_str(&format, "format_name").unwrap_or("").to_string(),
        has_video: video.is_some(),
        has_audio: audio.is_some(),
        ..MediaInfo::default()
    };

    if let Some(stream) = &video {
        info.width = json_u64(stream, "width").unwrap_or(0) as u32;
        info.height = json_u64(stream, "height").unwrap_or(0) as u32;
        info.video_codec = json_str(stream, "codec_name").map(|s| s.to_string());
        info.fps = stream_fps(stream);
        info.rotation = stream_rotation(stream);
        info.nb_frames = json_u64(stream, "nb_frames").filter(|n| *n > 0);
    }
    if let Some(stream) = &audio {
        info.audio_codec = json_str(stream, "codec_name").map(|s| s.to_string());
        info.sample_rate = json_u64(stream, "sample_rate").unwrap_or(0) as u32;
        info.channels = json_u64(stream, "channels").unwrap_or(0) as u16;
    }

    let mut duration = json_f64(&format, "duration").filter(|d| *d > 0.0);
    if duration.is_none() {
        duration = video
            .as_ref()
            .and_then(|s| json_f64(s, "duration"))
            .filter(|d| *d > 0.0);
    }
    if duration.is_none() {
        duration = audio
            .as_ref()
            .and_then(|s| json_f64(s, "duration"))
            .filter(|d| *d > 0.0);
    }
    info.duration = Time::from_secs_f64(duration.unwrap_or(0.0));

    let mut bitrate = json_u64(&format, "bit_rate").unwrap_or(0);
    if bitrate == 0 {
        bitrate = video
            .as_ref()
            .and_then(|s| json_u64(s, "bit_rate"))
            .unwrap_or(0);
    }
    if bitrate == 0 {
        bitrate = audio
            .as_ref()
            .and_then(|s| json_u64(s, "bit_rate"))
            .unwrap_or(0);
    }
    info.bitrate = bitrate;

    Ok(info)
}

pub fn probe(path: &Path) -> Result<MediaInfo> {
    require_file(path, "media file")?;
    let args = vec![
        "-v".to_string(),
        "error".to_string(),
        "-print_format".to_string(),
        "json".to_string(),
        // Local files only: a crafted playlist (HLS/concat/SDP) must not make the probe
        // open network URLs or other protocols.
        "-protocol_whitelist".to_string(),
        "file".to_string(),
        "-show_format".to_string(),
        "-show_streams".to_string(),
        path.to_string_lossy().to_string(),
    ];
    let out = run_ffprobe(args)?;
    if !out.success {
        return Err(tool_failure(
            ErrorCode::InvalidInput,
            &format!("probing {}", path.display()),
            &out,
        ));
    }
    let info = parse_probe(&out.stdout)?;
    if info.format.is_empty() && !info.has_video && !info.has_audio {
        return Err(Error::invalid(format!(
            "{} is not a media file this build can read",
            path.display()
        )));
    }
    Ok(info)
}

/// Container/image formats accepted from untrusted references. Everything else ffprobe can
/// parse (concat, HLS, DASH, SDP, ffmetadata, text-like demuxers) is refused: those formats
/// can point at other files or URLs.
const SAFE_FORMATS: &[&str] = &[
    "mov",
    "mp4",
    "m4a",
    "3gp",
    "3g2",
    "mj2",
    "matroska",
    "webm",
    "avi",
    "mpegts",
    "mpeg",
    "mpegvideo",
    "wav",
    "mp3",
    "flac",
    "ogg",
    "aac",
    "aiff",
    "opus",
    "image2",
    "png_pipe",
    "jpeg_pipe",
    "webp_pipe",
    "bmp_pipe",
    "tiff_pipe",
    "gif",
    "flv",
    "asf",
    "mxf",
    "ac3",
    "eac3",
    "caf",
];

/// `probe` plus a content-based allow-list on the detected container. The result describes
/// what the bytes are, not what the file is called.
pub fn probe_untrusted(path: &Path) -> Result<MediaInfo> {
    let info = probe(path)?;
    let tokens: Vec<&str> = info
        .format
        .split(',')
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .collect();
    if tokens.is_empty() || !tokens.iter().all(|t| SAFE_FORMATS.contains(t)) {
        return Err(Error::new(
            ErrorCode::InvalidInput,
            format!("'{}' is detected as container '{}', which is not accepted from imported references", path.display(), info.format),
        ));
    }
    if !info.has_video && !info.has_audio {
        return Err(Error::invalid(format!(
            "{} contains no audio or video stream",
            path.display()
        )));
    }
    Ok(info)
}

pub fn ffmpeg_version() -> Result<String> {
    let bin = ffmpeg_bin()?;
    let opts = ProcOpts::default().with_timeout(30);
    let out = proc::run(&bin, &["-version".to_string()], &opts)?;
    if !out.success {
        return Err(tool_failure(
            ErrorCode::DependencyMissing,
            "ffmpeg -version",
            &out,
        ));
    }
    out.stdout
        .lines()
        .next()
        .map(|line| line.trim().to_string())
        .filter(|line| !line.is_empty())
        .ok_or_else(|| {
            Error::new(
                ErrorCode::DependencyMissing,
                "ffmpeg -version printed nothing",
            )
        })
}

pub fn extract_frame(input: &Path, at: Time, out: &Path, width: Option<u32>) -> Result<PathBuf> {
    require_file(input, "media file")?;
    prepare_out(out)?;
    let mut args: Vec<String> = vec![
        "-y".into(),
        "-ss".into(),
        at.ffmpeg_timestamp(),
        "-i".into(),
        input.to_string_lossy().to_string(),
        "-frames:v".into(),
        "1".into(),
    ];
    if let Some(w) = width.filter(|w| *w >= 2) {
        let w = w & !1;
        args.push("-vf".into());
        args.push(format!("scale={w}:-2"));
    }
    args.push(out.to_string_lossy().to_string());
    let run = run_ffmpeg(args, false, PRODUCE_TIMEOUT_SECS)?;
    if !run.success {
        return Err(tool_failure(ErrorCode::Render, "frame extraction", &run));
    }
    verified_output(out)
}

pub fn contact_sheet(
    input: &Path,
    count: usize,
    columns: usize,
    out: &Path,
    tile_width: u32,
) -> Result<PathBuf> {
    require_file(input, "media file")?;
    if count == 0 {
        return Err(Error::invalid("contact sheet needs at least one thumbnail"));
    }
    if columns == 0 {
        return Err(Error::invalid("contact sheet needs at least one column"));
    }
    let info = probe(input)?;
    if info.duration.nanos <= 0 {
        return Err(Error::invalid(format!(
            "{} has no measurable duration; cannot lay out a contact sheet",
            input.display()
        )));
    }
    let fps = (count as f64 / info.duration.as_secs_f64()).clamp(0.01, 1_000.0);
    let rows = count.div_ceil(columns);
    let tile_width = tile_width.max(16) & !1;
    prepare_out(out)?;
    let filter = format!("fps={fps:.6},scale={tile_width}:-2,tile={columns}x{rows}");
    let args = vec![
        "-y".to_string(),
        "-i".to_string(),
        input.to_string_lossy().to_string(),
        "-vf".to_string(),
        filter,
        "-frames:v".to_string(),
        "1".to_string(),
        out.to_string_lossy().to_string(),
    ];
    let run = run_ffmpeg(args, false, ANALYZE_TIMEOUT_SECS)?;
    if !run.success {
        return Err(tool_failure(ErrorCode::Render, "contact sheet", &run));
    }
    verified_output(out)
}

pub fn detect_scenes(input: &Path, threshold: f32) -> Result<Vec<SceneCut>> {
    require_file(input, "media file")?;
    if !(threshold.is_finite() && threshold > 0.0 && threshold < 1.0) {
        return Err(Error::invalid(format!(
            "scene threshold must be between 0 and 1, got {threshold}"
        )));
    }
    let filter = format!("select=gt(scene\\,{threshold:.4}),metadata=mode=print,showinfo");
    let args = vec![
        "-i".to_string(),
        input.to_string_lossy().to_string(),
        "-vf".to_string(),
        filter,
        "-an".to_string(),
        "-f".to_string(),
        "null".to_string(),
        "-".to_string(),
    ];
    let run = run_ffmpeg(args, true, ANALYZE_TIMEOUT_SECS)?;
    if !run.success {
        return Err(tool_failure(
            ErrorCode::InvalidInput,
            "scene detection",
            &run,
        ));
    }
    Ok(parse_scenes(&run.stderr, threshold))
}

fn parse_scenes(stderr: &str, threshold: f32) -> Vec<SceneCut> {
    let mut cuts: Vec<SceneCut> = Vec::new();
    let mut pending: Option<Time> = None;
    let push = |at: Time, score: f32, cuts: &mut Vec<SceneCut>| {
        if cuts.last().map(|c| c.at != at).unwrap_or(true) {
            cuts.push(SceneCut { at, score });
        }
    };
    for line in stderr.lines() {
        if line.contains("Parsed_showinfo") {
            continue;
        }
        if line.contains("pts_time:") {
            if let Some(t) = number_after(line, "pts_time") {
                pending = Some(Time::from_secs_f64(t.max(0.0)));
            }
            continue;
        }
        if line.contains("lavfi.scene_score") {
            if let Some(time) = pending.take() {
                let score =
                    number_after(line, "lavfi.scene_score").unwrap_or(threshold as f64) as f32;
                push(time, score, &mut cuts);
            }
        }
    }
    if let Some(time) = pending {
        push(time, threshold, &mut cuts);
    }
    cuts.sort_by_key(|c| c.at.nanos);
    cuts.dedup_by_key(|c| c.at.nanos);
    cuts
}

pub fn detect_silence(input: &Path, noise_db: f64, min_secs: f64) -> Result<Vec<Span>> {
    require_file(input, "media file")?;
    if !noise_db.is_finite() || !(-120.0..=0.0).contains(&noise_db) {
        return Err(Error::invalid(format!(
            "noise floor must be between -120 and 0 dB, got {noise_db}"
        )));
    }
    if !min_secs.is_finite() || min_secs <= 0.0 {
        return Err(Error::invalid(format!(
            "minimum silence length must be positive, got {min_secs}"
        )));
    }
    let info = probe(input)?;
    if !info.has_audio {
        return Err(Error::invalid(format!(
            "{} has no audio stream to analyse",
            input.display()
        )));
    }
    let args = vec![
        "-i".to_string(),
        input.to_string_lossy().to_string(),
        "-af".to_string(),
        format!("silencedetect=noise={noise_db:.1}dB:d={min_secs:.3}"),
        "-vn".to_string(),
        "-f".to_string(),
        "null".to_string(),
        "-".to_string(),
    ];
    let run = run_ffmpeg(args, true, ANALYZE_TIMEOUT_SECS)?;
    if !run.success {
        return Err(tool_failure(
            ErrorCode::InvalidInput,
            "silence detection",
            &run,
        ));
    }
    Ok(parse_silence(&run.stderr, info.duration))
}

fn parse_silence(stderr: &str, duration: Time) -> Vec<Span> {
    let mut spans: Vec<Span> = Vec::new();
    let mut open: Option<Time> = None;
    for line in stderr.lines() {
        if !line.contains("silence_") {
            continue;
        }
        if line.contains("silence_start") {
            if let Some(v) = number_after(line, "silence_start") {
                open = Some(Time::from_secs_f64(v.max(0.0)));
            }
        }
        if line.contains("silence_end") {
            if let Some(v) = number_after(line, "silence_end") {
                let end = Time::from_secs_f64(v.max(0.0));
                if let Some(start) = open.take() {
                    if end > start {
                        spans.push(Span {
                            start,
                            end,
                            detail: format!("silence {:.2}s", (end - start).as_secs_f64()),
                        });
                    }
                }
            }
        }
    }
    if let Some(start) = open {
        let end = if duration > start { duration } else { start };
        if end > start {
            spans.push(Span {
                start,
                end,
                detail: format!(
                    "silence {:.2}s (to end of file)",
                    (end - start).as_secs_f64()
                ),
            });
        }
    }
    spans.sort_by_key(|s| s.start.nanos);
    spans
}

pub fn detect_black(input: &Path, min_secs: f64, pixel_threshold: f64) -> Result<Vec<Span>> {
    require_file(input, "media file")?;
    if !min_secs.is_finite() || min_secs < 0.0 {
        return Err(Error::invalid("minimum black length must not be negative"));
    }
    if !pixel_threshold.is_finite() || !(0.0..=1.0).contains(&pixel_threshold) {
        return Err(Error::invalid(
            "black pixel threshold must be between 0 and 1",
        ));
    }
    let info = probe(input)?;
    if !info.has_video {
        return Err(Error::invalid(format!(
            "{} has no video stream to analyse",
            input.display()
        )));
    }
    let args = vec![
        "-i".to_string(),
        input.to_string_lossy().to_string(),
        "-vf".to_string(),
        format!("blackdetect=d={min_secs:.3}:pix_th={pixel_threshold:.3}"),
        "-an".to_string(),
        "-f".to_string(),
        "null".to_string(),
        "-".to_string(),
    ];
    let run = run_ffmpeg(args, true, ANALYZE_TIMEOUT_SECS)?;
    if !run.success {
        return Err(tool_failure(
            ErrorCode::InvalidInput,
            "black detection",
            &run,
        ));
    }
    Ok(parse_black(&run.stderr, info.duration))
}

fn parse_black(stderr: &str, duration: Time) -> Vec<Span> {
    let mut spans: Vec<Span> = Vec::new();
    let mut open: Option<Time> = None;
    for line in stderr.lines() {
        if !line.contains("black_") {
            continue;
        }
        if line.contains("black_start") {
            if let Some(v) = number_after(line, "black_start") {
                open = Some(Time::from_secs_f64(v.max(0.0)));
            }
        }
        if line.contains("black_end") {
            if let Some(v) = number_after(line, "black_end") {
                let end = Time::from_secs_f64(v.max(0.0));
                if let Some(start) = open.take() {
                    if end > start {
                        spans.push(Span {
                            start,
                            end,
                            detail: format!("black {:.2}s", (end - start).as_secs_f64()),
                        });
                    }
                }
            }
        }
    }
    if let Some(start) = open {
        let end = if duration > start { duration } else { start };
        if end > start {
            spans.push(Span {
                start,
                end,
                detail: format!("black {:.2}s (to end of file)", (end - start).as_secs_f64()),
            });
        }
    }
    spans.sort_by_key(|s| s.start.nanos);
    spans
}

pub fn extract_audio(input: &Path, out: &Path) -> Result<PathBuf> {
    require_file(input, "media file")?;
    let info = probe(input)?;
    if !info.has_audio {
        return Err(Error::invalid(format!(
            "{} has no audio stream to extract",
            input.display()
        )));
    }
    prepare_out(out)?;
    let ext = out
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("m4a")
        .to_ascii_lowercase();
    let mut args: Vec<String> = vec![
        "-y".into(),
        "-i".into(),
        input.to_string_lossy().to_string(),
        "-vn".into(),
    ];
    match ext.as_str() {
        "wav" => {
            args.push("-c:a".into());
            args.push("pcm_s16le".into());
            args.push("-ar".into());
            args.push(if info.sample_rate > 0 {
                info.sample_rate.to_string()
            } else {
                "48000".into()
            });
        }
        _ => {
            args.push("-c:a".into());
            args.push("aac".into());
            args.push("-b:a".into());
            args.push("192k".into());
        }
    }
    args.push(out.to_string_lossy().to_string());
    let run = run_ffmpeg(args, false, PRODUCE_TIMEOUT_SECS)?;
    if !run.success {
        return Err(tool_failure(ErrorCode::Render, "audio extraction", &run));
    }
    verified_output(out)
}

pub fn make_proxy(input: &Path, out: &Path, height: u32) -> Result<PathBuf> {
    require_file(input, "media file")?;
    let info = probe(input)?;
    if !info.has_video {
        return Err(Error::invalid(format!(
            "{} has no video stream to proxy",
            input.display()
        )));
    }
    let height = height.clamp(64, 2160) & !1;
    prepare_out(out)?;
    let mut args: Vec<String> = vec![
        "-y".into(),
        "-i".into(),
        input.to_string_lossy().to_string(),
        "-vf".into(),
        format!("scale=-2:{height}"),
        "-c:v".into(),
        "libx264".into(),
        "-preset".into(),
        "ultrafast".into(),
        "-crf".into(),
        "26".into(),
        "-pix_fmt".into(),
        "yuv420p".into(),
    ];
    if info.has_audio {
        args.push("-c:a".into());
        args.push("aac".into());
        args.push("-b:a".into());
        args.push("128k".into());
    } else {
        args.push("-an".into());
    }
    args.push(out.to_string_lossy().to_string());
    let run = run_ffmpeg(args, false, PRODUCE_TIMEOUT_SECS)?;
    if !run.success {
        return Err(tool_failure(ErrorCode::Render, "proxy encode", &run));
    }
    verified_output(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_black_and_silence_lines() {
        let black = "[blackdetect @ 0x1] black_start:1.5 black_end:3 black_duration:1.5\n";
        let spans = parse_black(black, Time::from_secs_f64(10.0));
        assert_eq!(spans.len(), 1);
        assert_eq!(spans[0].start, Time::from_secs_f64(1.5));
        assert_eq!(spans[0].end, Time::from_secs_f64(3.0));

        let silence = "[silencedetect @ 0x2] silence_start: 0.5\n[silencedetect @ 0x2] silence_end: 2.25 | silence_duration: 1.75\n";
        let spans = parse_silence(silence, Time::from_secs_f64(10.0));
        assert_eq!(spans.len(), 1);
        assert_eq!(spans[0].start, Time::from_secs_f64(0.5));
        assert_eq!(spans[0].end, Time::from_secs_f64(2.25));
    }

    #[test]
    fn dangling_silence_runs_to_the_end() {
        let text = "[silencedetect @ 0x2] silence_start: 8\n";
        let spans = parse_silence(text, Time::from_secs_f64(12.0));
        assert_eq!(spans.len(), 1);
        assert_eq!(spans[0].end, Time::from_secs_f64(12.0));
    }

    #[test]
    fn parses_scene_metadata_block() {
        let text = "\
[Parsed_metadata_1 @ 0x1] frame:0    pts:30   pts_time:1
[Parsed_metadata_1 @ 0x1] lavfi.scene_score=0.500000
[Parsed_showinfo_2 @ 0x1] n:   0 pts:   30 pts_time:1       fmt:yuv420p
";
        let cuts = parse_scenes(text, 0.3);
        assert_eq!(cuts.len(), 1);
        assert_eq!(cuts[0].at, Time::from_secs_f64(1.0));
        assert!((cuts[0].score - 0.5).abs() < 1e-6);
    }

    #[test]
    fn reads_probe_json() {
        let text = r#"{
          "streams": [
            {"codec_type":"video","codec_name":"h264","width":1920,"height":1080,
             "r_frame_rate":"30000/1001","nb_frames":"90","side_data_list":[{"rotation":-90}]},
            {"codec_type":"audio","codec_name":"aac","sample_rate":"48000","channels":2}
          ],
          "format": {"duration":"3.000000","format_name":"mov,mp4","bit_rate":"800000"}
        }"#;
        let info = parse_probe(text).unwrap();
        assert_eq!(info.width, 1920);
        assert_eq!(info.height, 1080);
        assert_eq!(info.fps.unwrap().ffprobe_fraction(), "30000/1001");
        assert_eq!(info.video_codec.as_deref(), Some("h264"));
        assert_eq!(info.audio_codec.as_deref(), Some("aac"));
        assert_eq!(info.sample_rate, 48_000);
        assert_eq!(info.channels, 2);
        assert_eq!(info.rotation, -90);
        assert_eq!(info.nb_frames, Some(90));
        assert!(info.has_video && info.has_audio);
        assert_eq!(info.duration, Time::from_secs_f64(3.0));
        assert_eq!(info.orientation(), Orientation::Landscape);
    }

    #[test]
    fn rejects_garbage_json() {
        assert!(parse_probe("not json").is_err());
        assert!(parse_probe("{\"streams\":[]}").is_err());
    }
}
