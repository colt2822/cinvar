use std::path::Path;
use std::sync::OnceLock;

use crate::project::model::{HwMode, Project};
use crate::util::proc::{self, ProcOpts};

use super::RenderRequest;

/// Encoders worth reporting to the user; the raw ffmpeg list is hundreds of
/// entries and useless for routing decisions.
const CURATED: &[&str] = &[
    "libx264",
    "libx265",
    "libvpx-vp9",
    "libvpx",
    "prores_ks",
    "dnxhd",
    "h264_nvenc",
    "hevc_nvenc",
    "av1_nvenc",
    "h264_qsv",
    "hevc_qsv",
    "h264_amf",
    "hevc_amf",
    "h264_videotoolbox",
    "hevc_videotoolbox",
    "aac",
    "libfdk_aac",
    "libopus",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Codec {
    H264,
    Hevc,
    Vp9,
    /// Apple ProRes 422 HQ (mezzanine/mastering; CINVAR Pro).
    ProRes,
    /// Avid DNxHR HQ (mezzanine/editing interchange; CINVAR Pro).
    Dnxhr,
    /// AV1 (smaller files for the same quality; CINVAR Pro).
    Av1,
}

impl Codec {
    pub(super) fn as_str(self) -> &'static str {
        match self {
            Codec::H264 => "h264",
            Codec::Hevc => "hevc",
            Codec::Vp9 => "vp9",
            Codec::ProRes => "prores",
            Codec::Dnxhr => "dnxhr",
            Codec::Av1 => "av1",
        }
    }
}

#[derive(Debug, Clone)]
pub(super) struct EncoderPlan {
    pub encoder: String,
    pub container: String,
    pub video_args: Vec<String>,
    pub audio_args: Vec<String>,
    pub muxer: Option<String>,
    pub warnings: Vec<String>,
    pub pix_fmt: String,
    pub bit_depth: u8,
}

/// `8M`, `5000k`, `2500000`: digits with an optional k/K/m/M suffix, 100 kbit/s .. 1 Gbit/s.
pub fn valid_bitrate(b: &str) -> bool {
    let (digits, mult) = match b.chars().last() {
        Some('k') | Some('K') => (&b[..b.len() - 1], 1_000u64),
        Some('m') | Some('M') => (&b[..b.len() - 1], 1_000_000u64),
        _ => (b, 1u64),
    };
    digits.len() <= 9
        && !digits.is_empty()
        && digits.bytes().all(|c| c.is_ascii_digit())
        && digits.parse::<u64>().map(|n| (100_000..=1_000_000_000).contains(&(n * mult))).unwrap_or(false)
}

fn args(values: &[&str]) -> Vec<String> {
    values.iter().map(|v| v.to_string()).collect()
}

pub(super) fn available() -> Vec<String> {
    static CACHE: OnceLock<Vec<String>> = OnceLock::new();
    CACHE.get_or_init(detect).clone()
}

fn detect() -> Vec<String> {
    let bin = match proc::require("ffmpeg") {
        Ok(bin) => bin,
        Err(_) => return Vec::new(),
    };
    let argv = args(&["-hide_banner", "-loglevel", "error", "-encoders"]);
    let opts = ProcOpts::default().with_timeout(60);
    let out = match proc::run(&bin, &argv, &opts) {
        Ok(out) if out.success => out,
        _ => return Vec::new(),
    };
    let mut present: Vec<String> = Vec::new();
    for line in out.stdout.lines() {
        let mut tokens = line.split_whitespace();
        let flags = tokens.next().unwrap_or("");
        if flags.len() != 6 || !flags.chars().all(|c| c.is_ascii_uppercase() || c == '.') {
            continue;
        }
        if let Some(name) = tokens.next() {
            present.push(name.to_string());
        }
    }
    CURATED
        .iter()
        .filter(|name| present.iter().any(|p| p == *name))
        .map(|name| (*name).to_string())
        .collect()
}

/// Two frames through the encoder: the only reliable way to know hardware
/// encoding actually works on this machine.
fn probe_encode(encoder: &str) -> bool {
    let bin = match proc::require("ffmpeg") {
        Ok(bin) => bin,
        Err(_) => return false,
    };
    let argv = vec![
        "-hide_banner".to_string(),
        "-nostdin".to_string(),
        "-loglevel".to_string(),
        "error".to_string(),
        "-f".to_string(),
        "lavfi".to_string(),
        "-i".to_string(),
        "color=c=black:s=256x144:r=25:d=0.4".to_string(),
        "-frames:v".to_string(),
        "2".to_string(),
        "-c:v".to_string(),
        encoder.to_string(),
        "-f".to_string(),
        "null".to_string(),
        "-".to_string(),
    ];
    let opts = ProcOpts::default().with_timeout(30);
    matches!(proc::run(&bin, &argv, &opts), Ok(out) if out.success)
}

fn working_hardware() -> &'static Vec<String> {
    static CACHE: OnceLock<Vec<String>> = OnceLock::new();
    CACHE.get_or_init(|| {
        let listed = available();
        let mut working = Vec::new();
        for candidate in [
            "h264_nvenc",
            "hevc_nvenc",
            "av1_nvenc",
            "h264_qsv",
            "hevc_qsv",
        ] {
            if listed.iter().any(|name| name == candidate) && probe_encode(candidate) {
                working.push(candidate.to_string());
            }
        }
        working
    })
}

fn nvenc_preset(preset: &str) -> &'static str {
    match preset.to_ascii_lowercase().as_str() {
        "ultrafast" => "p1",
        "superfast" => "p2",
        "veryfast" => "p3",
        "faster" | "fast" => "p4",
        "medium" | "default" => "p5",
        "slow" => "p6",
        "slower" | "veryslow" => "p7",
        _ => "p5",
    }
}

fn parse_codec(raw: &str) -> Option<Codec> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "h264" | "avc" | "libx264" | "x264" | "h.264" => Some(Codec::H264),
        "hevc" | "h265" | "h.265" | "libx265" | "x265" => Some(Codec::Hevc),
        "vp9" | "libvpx-vp9" | "webm" => Some(Codec::Vp9),
        "prores" | "prores_ks" | "prores422" | "prores-422" | "apple prores" => Some(Codec::ProRes),
        "dnxhr" | "dnxhd" | "avcintra" => Some(Codec::Dnxhr),
        "av1" | "libsvtav1" | "svt-av1" => Some(Codec::Av1),
        _ => None,
    }
}

/// The codec this render will actually ask for: the request's, else the project setting.
pub(super) fn requested_codec<'a>(project: &'a Project, request: &'a RenderRequest) -> &'a str {
    request
        .codec
        .as_deref()
        .filter(|c| !c.trim().is_empty())
        .unwrap_or_else(|| project.render.video_codec.as_str())
}

/// Name of the Pro feature a requested codec needs, or `None` for the free codecs. This is the one
/// place the codec -> edition mapping lives, so the render gate and the CLI report agree.
pub(super) fn pro_codec(raw: &str) -> Option<&'static str> {
    match parse_codec(raw)? {
        Codec::ProRes => Some("ProRes"),
        Codec::Dnxhr => Some("DNxHR"),
        Codec::Av1 => Some("AV1"),
        _ => None,
    }
}

fn container_of(request: &RenderRequest) -> String {
    let from_request = request
        .format
        .as_ref()
        .map(|f| f.trim().to_ascii_lowercase())
        .filter(|f| !f.is_empty());
    let from_path = Path::new(&request.output)
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase());
    let raw = from_request
        .or(from_path)
        .unwrap_or_else(|| "mp4".to_string());
    match raw.as_str() {
        "mp4" | "m4v" | "mov" => raw,
        "webm" => "webm".to_string(),
        "mkv" | "matroska" => "mkv".to_string(),
        other => other.to_string(),
    }
}

pub(super) fn choose(
    project: &Project,
    request: &RenderRequest,
    preview: bool,
    crf: u8,
    preset: &str,
    has_audio: bool,
    size: (u32, u32),
) -> EncoderPlan {
    let mut warnings: Vec<String> = Vec::new();
    let settings = &project.render;
    let container = container_of(request);

    let requested_codec = requested_codec(project, request);
    let mut codec = match parse_codec(requested_codec) {
        Some(codec) => codec,
        None => {
            warnings.push(format!(
                "unknown codec '{requested_codec}'; falling back to h264"
            ));
            Codec::H264
        }
    };
    if container == "webm" && !matches!(codec, Codec::Vp9 | Codec::Av1) {
        warnings.push(format!(
            "{} cannot be muxed into webm; switching to vp9",
            codec.as_str()
        ));
        codec = Codec::Vp9;
    }
    if codec == Codec::Vp9 && container != "webm" {
        warnings.push(format!(
            "vp9 in a {container} container has limited player support; use webm for broad compatibility"
        ));
    }

    let mut bit_depth = request.bit_depth.unwrap_or(8);
    if bit_depth != 8 && bit_depth != 10 {
        warnings.push(format!(
            "bit depth {bit_depth} is not supported; rendering 8-bit"
        ));
        bit_depth = 8;
    }
    if bit_depth == 10 && codec == Codec::Vp9 {
        warnings.push("10-bit vp9 is not wired up; rendering 8-bit".to_string());
        bit_depth = 8;
    }
    let (w, h) = size;
    let large = (w as u64) * (h as u64) > 1920 * 1088;
    let hardware = request.hardware.unwrap_or(settings.hardware);
    // ProRes, DNxHR and AV1 are mezzanine/delivery codecs with no NVENC-equivalent path wired up
    // here (av1_nvenc exists but its rate control differs), so they always run on the CPU.
    let hw_candidate = match codec {
        Codec::H264 => Some("h264_nvenc"),
        Codec::Hevc => Some("hevc_nvenc"),
        Codec::Vp9 | Codec::ProRes | Codec::Dnxhr | Codec::Av1 => None,
    };
    let mut encoder = match codec {
        Codec::H264 => "libx264".to_string(),
        Codec::Hevc => "libx265".to_string(),
        Codec::Vp9 => "libvpx-vp9".to_string(),
        Codec::ProRes => "prores_ks".to_string(),
        Codec::Dnxhr => "dnxhd".to_string(),
        Codec::Av1 => "libsvtav1".to_string(),
    };
    let mut use_hardware = false;
    if hardware != HwMode::Off {
        match hw_candidate {
            Some(candidate) => {
                let hw_pix = match (codec, bit_depth) {
                    (Codec::Hevc, 10) => "p010le",
                    _ => "yuv420p",
                };
                let mut working = working_hardware().iter().any(|name| name == candidate);
                if working && bit_depth == 10 && codec != Codec::Hevc {
                    working = false;
                    warnings.push(format!(
                        "{candidate} has no 10-bit mode; using the CPU encoder for 10-bit"
                    ));
                }
                // The 256x144 start-up probe says nothing about large frames: measure at the real size.
                if working && large {
                    let probe = super::capability::probe_encoder(candidate, w, h, hw_pix);
                    if !probe.supported {
                        working = false;
                        warnings.push(format!(
                            "{candidate} failed a trial encode at {w}x{h} ({}); using the CPU encoder",
                            probe.error.unwrap_or_else(|| "no detail".into())
                        ));
                    }
                }
                let listed = available().iter().any(|name| name == candidate);
                if working {
                    encoder = candidate.to_string();
                    use_hardware = true;
                } else if listed {
                    warnings.push(format!(
                        "ffmpeg advertises {candidate} but a probe encode failed, so this render uses the CPU encoder"
                    ));
                } else if hardware == HwMode::Force {
                    warnings.push(format!(
                        "hardware encoding was requested but {candidate} is not available here; rendering on the CPU"
                    ));
                }
            }
            None => {
                if hardware == HwMode::Force {
                    warnings.push(format!(
                        "hardware encoding was requested for {}, which has no hardware path here; rendering on the CPU",
                        codec.as_str()
                    ));
                }
            }
        }
    }

    let sw_pix = if bit_depth == 10 {
        "yuv420p10le"
    } else {
        "yuv420p"
    };
    let crf = crf.to_string();
    let bitrate = match request.video_bitrate.as_deref().map(str::trim).filter(|b| !b.is_empty()) {
        Some(b) if valid_bitrate(b) => Some(b.to_string()),
        Some(b) => {
            warnings.push(format!("invalid bitrate '{b}' (use e.g. 8M or 5000k); using CRF quality instead"));
            None
        }
        None => None,
    };
    let mut video_args: Vec<String> = Vec::new();
    if use_hardware {
        video_args.extend(args(&["-c:v", encoder.as_str()]));
        match codec {
            Codec::H264 | Codec::Hevc => {
                video_args.extend(args(&["-preset", nvenc_preset(preset)]));
                match bitrate.as_deref() {
                    Some(b) => video_args.extend(args(&["-rc", "vbr", "-b:v", b])),
                    None => video_args.extend(args(&["-rc", "vbr", "-cq", crf.as_str(), "-b:v", "0"])),
                }
                let hw_pix = if codec == Codec::Hevc && bit_depth == 10 {
                    "p010le"
                } else {
                    "yuv420p"
                };
                video_args.extend(args(&["-pix_fmt", hw_pix]));
                if codec == Codec::Hevc {
                    video_args.extend(args(&["-tag:v", "hvc1"]));
                }
            }
            Codec::Vp9 | Codec::ProRes | Codec::Dnxhr | Codec::Av1 => {}
        }
    } else {
        match codec {
            Codec::H264 => {
                video_args.extend(args(&["-c:v", "libx264"]));
                match bitrate.as_deref() {
                    Some(b) => video_args.extend(args(&["-preset", preset, "-b:v", b])),
                    None => video_args.extend(args(&["-preset", preset, "-crf", crf.as_str()])),
                }
                video_args.extend(args(&["-pix_fmt", sw_pix]));
            }
            Codec::Hevc => {
                video_args.extend(args(&["-c:v", "libx265"]));
                match bitrate.as_deref() {
                    Some(b) => video_args.extend(args(&["-preset", preset, "-b:v", b])),
                    None => video_args.extend(args(&["-preset", preset, "-crf", crf.as_str()])),
                }
                video_args.extend(args(&["-pix_fmt", sw_pix, "-tag:v", "hvc1"]));
            }
            Codec::ProRes => {
                // profile 3 = ProRes 422 HQ; apl0 marks the file as Apple-produced so editors
                // that key off the vendor tag (Resolve, FCP) treat it as a normal master.
                video_args.extend(args(&[
                    "-c:v", "prores_ks", "-profile:v", "3", "-pix_fmt", "yuv422p10le", "-vendor",
                    "apl0",
                ]));
            }
            Codec::Dnxhr => {
                video_args.extend(args(&["-c:v", "dnxhd", "-profile:v", "dnxhr_hq", "-pix_fmt", "yuv422p"]));
            }
            Codec::Av1 => {
                video_args.extend(args(&["-c:v", "libsvtav1"]));
                match bitrate.as_deref() {
                    Some(b) => video_args.extend(args(&["-b:v", b])),
                    None => video_args.extend(args(&["-crf", crf.as_str(), "-preset", "8"])),
                }
                video_args.extend(args(&["-pix_fmt", "yuv420p"]));
            }
            Codec::Vp9 => {
                video_args.extend(args(&["-c:v", "libvpx-vp9"]));
                match bitrate.as_deref() {
                    Some(b) => video_args.extend(args(&["-b:v", b, "-row-mt", "1"])),
                    None => video_args.extend(args(&["-crf", crf.as_str(), "-b:v", "0", "-row-mt", "1"])),
                }
                video_args.extend(args(&["-pix_fmt", "yuv420p"]));
            }
        }
    }
    if container == "mp4" || container == "mov" || container == "m4v" {
        video_args.extend(args(&["-movflags", "+faststart"]));
    }
    if preview && codec == Codec::Vp9 {
        video_args.extend(args(&["-deadline", "realtime"]));
    }

    let audio_args = if !has_audio {
        args(&["-an"])
    } else {
        let bitrate = if settings.audio_bitrate.trim().is_empty() {
            "192k".to_string()
        } else {
            settings.audio_bitrate.clone()
        };
        match container.as_str() {
            "webm" => args(&["-c:a", "libopus", "-b:a", "160k"]),
            _ => vec![
                "-c:a".to_string(),
                "aac".to_string(),
                "-b:a".to_string(),
                bitrate,
            ],
        }
    };

    let has_extension = Path::new(&request.output).extension().is_some();
    let muxer = if has_extension {
        None
    } else {
        Some(container.clone())
    };

    EncoderPlan {
        encoder,
        container,
        video_args,
        audio_args,
        muxer,
        warnings,
        pix_fmt: reported_pix_fmt(codec, use_hardware, bit_depth, sw_pix).0,
        bit_depth: reported_pix_fmt(codec, use_hardware, bit_depth, sw_pix).1,
    }
}

/// The pixel format and bit depth the plan really produces, for capability/memory estimates.
fn reported_pix_fmt(codec: Codec, use_hardware: bool, bit_depth: u8, sw_pix: &str) -> (String, u8) {
    match codec {
        Codec::ProRes => ("yuv422p10le".to_string(), 10),
        Codec::Dnxhr => ("yuv422p".to_string(), 8),
        Codec::Hevc if use_hardware && bit_depth == 10 => ("p010le".to_string(), bit_depth),
        _ if use_hardware => ("yuv420p".to_string(), bit_depth),
        _ => (sw_pix.to_string(), bit_depth),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_presets_for_nvenc() {
        assert_eq!(nvenc_preset("veryfast"), "p3");
        assert_eq!(nvenc_preset("medium"), "p5");
        assert_eq!(nvenc_preset("nonsense"), "p5");
    }

    #[test]
    fn parses_codec_names() {
        assert_eq!(parse_codec("h264"), Some(Codec::H264));
        assert_eq!(parse_codec("HEVC"), Some(Codec::Hevc));
        assert_eq!(parse_codec("libvpx-vp9"), Some(Codec::Vp9));
        assert_eq!(parse_codec("mpeg4"), None);
        assert_eq!(parse_codec("ProRes"), Some(Codec::ProRes));
        assert_eq!(parse_codec("dnxhr"), Some(Codec::Dnxhr));
        assert_eq!(parse_codec("libsvtav1"), Some(Codec::Av1));
    }

    #[test]
    fn pro_codecs_are_named_for_the_edition_gate() {
        assert_eq!(pro_codec("prores"), Some("ProRes"));
        assert_eq!(pro_codec("dnxhd"), Some("DNxHR"));
        assert_eq!(pro_codec("av1"), Some("AV1"));
        assert_eq!(pro_codec("h264"), None);
        assert_eq!(pro_codec("hevc"), None);
        assert_eq!(pro_codec("vp9"), None);
    }

    #[test]
    fn picks_container_from_extension() {
        let request = RenderRequest::for_output("/tmp/out.webm");
        assert_eq!(container_of(&request), "webm");
        let request = RenderRequest::for_output("/tmp/out.mp4");
        assert_eq!(container_of(&request), "mp4");
    }
}
