use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::error::{Error, ErrorCode, Result};
use crate::util::proc::{self, ProcOpts};
use crate::util::time::Time;

const PROBE_TIMEOUT_SECS: u64 = 120;

/// Measurements taken straight from ffprobe. Nothing here is estimated.
#[derive(Debug, Clone, Default)]
pub struct MediaProbe {
    pub format: String,
    pub duration: Time,
    pub width: u32,
    pub height: u32,
    pub fps: Option<f64>,
    pub video_codec: Option<String>,
    pub audio_codec: Option<String>,
    pub sample_rate: u32,
    pub channels: u16,
    pub has_video: bool,
    pub has_audio: bool,
}

impl MediaProbe {
    pub fn resolution(&self) -> String {
        format!("{}x{}", self.width, self.height)
    }
}

#[derive(Debug, Clone, Copy, Default)]
pub struct VolumeStats {
    pub mean_db: Option<f64>,
    pub max_db: Option<f64>,
}

#[derive(Debug, Clone, Copy)]
pub struct BlackSpan {
    pub start: f64,
    pub end: f64,
    pub duration: f64,
}

pub fn ffprobe() -> Result<PathBuf> {
    proc::require("ffprobe").map_err(|_| missing("ffprobe"))
}

pub fn ffmpeg() -> Result<PathBuf> {
    proc::require("ffmpeg").map_err(|_| missing("ffmpeg"))
}

fn missing(tool: &str) -> Error {
    Error::new(
        ErrorCode::DependencyMissing,
        format!("{tool} is not installed"),
    )
    .with_hint("install ffmpeg (it ships ffmpeg and ffprobe): https://ffmpeg.org/download.html")
}

fn redacted(text: &str) -> String {
    crate::secrets::redact(text).trim().to_string()
}

pub fn probe(path: &Path) -> Result<MediaProbe> {
    if !path.exists() {
        return Err(Error::new(
            ErrorCode::NotFound,
            format!("no such file: {}", path.display()),
        ));
    }
    let bin = ffprobe()?;
    let args = vec![
        "-v".to_string(),
        "error".to_string(),
        "-print_format".to_string(),
        "json".to_string(),
        "-show_format".to_string(),
        "-show_streams".to_string(),
        path.to_string_lossy().to_string(),
    ];
    let out = proc::run(
        &bin,
        &args,
        &ProcOpts::default().with_timeout(PROBE_TIMEOUT_SECS),
    )?;
    if !out.success {
        return Err(Error::new(
            ErrorCode::Render,
            format!(
                "ffprobe could not read {}: {}",
                path.display(),
                redacted(&out.tail_stderr(3))
            ),
        )
        .with_hint("the file may be empty, truncated, or not a media container"));
    }
    parse_probe_json(&out.stdout)
}

pub fn parse_probe_json(text: &str) -> Result<MediaProbe> {
    let value: Value = serde_json::from_str(text).map_err(|e| {
        Error::new(
            ErrorCode::Render,
            format!("ffprobe emitted unreadable json: {e}"),
        )
    })?;
    let mut probe = MediaProbe::default();
    let mut stream_duration = 0.0f64;

    if let Some(format) = value.get("format") {
        probe.format = format
            .get("format_name")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        if let Some(seconds) = number(format.get("duration")) {
            probe.duration = Time::from_secs_f64(seconds);
        }
    }

    if let Some(streams) = value.get("streams").and_then(|v| v.as_array()) {
        for stream in streams {
            let kind = stream
                .get("codec_type")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let attached_pic = stream
                .get("disposition")
                .and_then(|d| d.get("attached_pic"))
                .and_then(|v| v.as_i64())
                .unwrap_or(0)
                == 1;
            if let Some(seconds) = number(stream.get("duration")) {
                stream_duration = stream_duration.max(seconds);
            }
            match kind {
                "video" if !attached_pic => {
                    probe.has_video = true;
                    probe.width = number(stream.get("width")).unwrap_or(0.0) as u32;
                    probe.height = number(stream.get("height")).unwrap_or(0.0) as u32;
                    probe.video_codec = stream
                        .get("codec_name")
                        .and_then(|v| v.as_str())
                        .map(|s| s.to_string());
                    probe.fps = stream
                        .get("r_frame_rate")
                        .and_then(|v| v.as_str())
                        .and_then(parse_rate)
                        .or_else(|| {
                            stream
                                .get("avg_frame_rate")
                                .and_then(|v| v.as_str())
                                .and_then(parse_rate)
                        });
                }
                "audio" => {
                    probe.has_audio = true;
                    probe.audio_codec = stream
                        .get("codec_name")
                        .and_then(|v| v.as_str())
                        .map(|s| s.to_string());
                    probe.sample_rate = number(stream.get("sample_rate")).unwrap_or(0.0) as u32;
                    probe.channels = number(stream.get("channels")).unwrap_or(0.0) as u16;
                }
                _ => {}
            }
        }
    }

    if probe.duration.nanos == 0 && stream_duration > 0.0 {
        probe.duration = Time::from_secs_f64(stream_duration);
    }
    if !probe.has_video && !probe.has_audio {
        return Err(Error::new(
            ErrorCode::Render,
            "container holds no audio or video stream".to_string(),
        )
        .with_hint("check that the file finished rendering and is not a fragment"));
    }
    Ok(probe)
}

fn number(value: Option<&Value>) -> Option<f64> {
    let value = value?;
    if let Some(n) = value.as_f64() {
        return Some(n);
    }
    value.as_str().and_then(|s| s.trim().parse::<f64>().ok())
}

fn parse_rate(raw: &str) -> Option<f64> {
    let (num, den) = raw.split_once('/')?;
    let num: f64 = num.trim().parse().ok()?;
    let den: f64 = den.trim().parse().ok()?;
    if num <= 0.0 || den <= 0.0 {
        return None;
    }
    Some(num / den)
}

/// Runs volumedetect over the whole file, or over a slice when a range is given.
pub fn volume_stats(
    path: &Path,
    from: Option<Time>,
    duration: Option<Time>,
) -> Result<VolumeStats> {
    let bin = ffmpeg()?;
    let mut args: Vec<String> = vec!["-hide_banner".into(), "-nostdin".into()];
    if let Some(start) = from {
        if start.nanos > 0 {
            args.push("-ss".into());
            args.push(format!("{:.3}", start.as_secs_f64()));
        }
    }
    args.push("-i".into());
    args.push(path.to_string_lossy().to_string());
    if let Some(len) = duration {
        if len.nanos > 0 {
            args.push("-t".into());
            args.push(format!("{:.3}", len.as_secs_f64()));
        }
    }
    args.extend([
        "-vn".to_string(),
        "-af".to_string(),
        "volumedetect".to_string(),
        "-f".to_string(),
        "null".to_string(),
        "-".to_string(),
    ]);
    let out = proc::run(
        &bin,
        &args,
        &ProcOpts::default().with_timeout(PROBE_TIMEOUT_SECS),
    )?;
    if !out.success {
        return Err(Error::new(
            ErrorCode::Render,
            format!("volumedetect failed: {}", redacted(&out.tail_stderr(3))),
        ));
    }
    parse_volume_stats(&out.stderr)
}

pub fn parse_volume_stats(stderr: &str) -> Result<VolumeStats> {
    let mut stats = VolumeStats::default();
    for line in stderr.lines() {
        if let Some(value) = field_after(line, "mean_volume:") {
            stats.mean_db = value;
        }
        if let Some(value) = field_after(line, "max_volume:") {
            stats.max_db = value;
        }
    }
    if stats.mean_db.is_none() && stats.max_db.is_none() {
        return Err(Error::new(
            ErrorCode::Render,
            "volumedetect reported no levels".to_string(),
        )
        .with_hint("the file may carry no decodable audio stream"));
    }
    Ok(stats)
}

fn field_after(line: &str, marker: &str) -> Option<Option<f64>> {
    let idx = line.find(marker)?;
    let rest = line[idx + marker.len()..].trim();
    let token = rest.split_whitespace().next().unwrap_or("");
    let cleaned = token.trim_end_matches("dB").trim();
    Some(cleaned.parse::<f64>().ok())
}

pub fn black_spans(path: &Path, min_secs: f64, pixel_ratio: f64) -> Result<Vec<BlackSpan>> {
    let bin = ffmpeg()?;
    let filter = format!("blackdetect=d={min_secs}:pic_th=0.98:pix_th={pixel_ratio}");
    let args = vec![
        "-hide_banner".to_string(),
        "-nostdin".to_string(),
        "-i".to_string(),
        path.to_string_lossy().to_string(),
        "-vf".to_string(),
        filter,
        "-an".to_string(),
        "-f".to_string(),
        "null".to_string(),
        "-".to_string(),
    ];
    let out = proc::run(
        &bin,
        &args,
        &ProcOpts::default().with_timeout(PROBE_TIMEOUT_SECS),
    )?;
    if !out.success {
        return Err(Error::new(
            ErrorCode::Render,
            format!("blackdetect failed: {}", redacted(&out.tail_stderr(3))),
        ));
    }
    Ok(parse_black_spans(&out.stderr))
}

pub fn parse_black_spans(stderr: &str) -> Vec<BlackSpan> {
    let mut spans = Vec::new();
    for line in stderr.lines() {
        if !line.contains("black_start:") {
            continue;
        }
        let start = field_after(line, "black_start:").flatten();
        let end = field_after(line, "black_end:").flatten();
        let duration = field_after(line, "black_duration:").flatten();
        if let (Some(start), Some(end)) = (start, end) {
            spans.push(BlackSpan {
                start,
                end,
                duration: duration.unwrap_or(end - start),
            });
        }
    }
    spans.sort_by(|a, b| {
        a.start
            .partial_cmp(&b.start)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    spans
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_volume_levels() {
        let stderr = "[Parsed_volumedetect_0 @ 0x55] mean_volume: -21.3 dB\n\
                      [Parsed_volumedetect_0 @ 0x55] max_volume: -3.0 dB\n\
                      [Parsed_volumedetect_0 @ 0x55] histogram_0db: 0";
        let stats = parse_volume_stats(stderr).unwrap();
        assert_eq!(stats.mean_db, Some(-21.3));
        assert_eq!(stats.max_db, Some(-3.0));
    }

    #[test]
    fn rejects_volume_silence_of_measurement() {
        assert!(parse_volume_stats("no levels here").is_err());
    }

    #[test]
    fn parses_black_spans() {
        let stderr = "[blackdetect @ 0x1] black_start:1.500000 black_end:2.500000 black_duration:1.000000\n\
                      [blackdetect @ 0x1] black_start:0.000000 black_end:0.200000 black_duration:0.200000";
        let spans = parse_black_spans(stderr);
        assert_eq!(spans.len(), 2);
        assert_eq!(spans[0].start, 0.0);
        assert_eq!(spans[1].duration, 1.0);
    }

    #[test]
    fn parses_probe_payload() {
        let json = r#"{"format":{"format_name":"mov,mp4","duration":"4.000000"},
            "streams":[
              {"codec_type":"video","codec_name":"h264","width":320,"height":240,"r_frame_rate":"30/1","duration":"4.000000"},
              {"codec_type":"audio","codec_name":"aac","sample_rate":"48000","channels":2,"duration":"4.000000"}
            ]}"#;
        let probe = parse_probe_json(json).unwrap();
        assert!(probe.has_video && probe.has_audio);
        assert_eq!(probe.width, 320);
        assert_eq!(probe.height, 240);
        assert_eq!(probe.duration.nanos, 4_000_000_000);
        assert_eq!(probe.fps, Some(30.0));
        assert_eq!(probe.sample_rate, 48_000);
    }

    #[test]
    fn skips_attached_picture_streams() {
        let json = r#"{"format":{"format_name":"mp3","duration":"2.0"},
            "streams":[
              {"codec_type":"video","codec_name":"mjpeg","width":500,"height":500,"disposition":{"attached_pic":1}},
              {"codec_type":"audio","codec_name":"mp3","sample_rate":"44100","channels":1}
            ]}"#;
        let probe = parse_probe_json(json).unwrap();
        assert!(!probe.has_video);
        assert!(probe.has_audio);
    }

    #[test]
    fn rejects_empty_container() {
        let json = r#"{"format":{"format_name":"data"},"streams":[]}"#;
        assert!(parse_probe_json(json).is_err());
    }
}
