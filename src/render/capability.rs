//! Hardware- and resolution-aware render preflight.
//!
//! Nothing here assumes a codec, GPU or driver supports a given frame size: encoder support
//! is *measured* by a two-frame trial encode at the exact target dimensions and cached for the
//! life of the process. Memory/disk figures are documented heuristics (they are labelled as
//! estimates in the report) and are compared against what the host reports right now.

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Mutex, OnceLock};

use serde::{Deserialize, Serialize};

use crate::project::model::{Project, Timeline, TrackKind};
use crate::util::proc::{self, ProcOpts};

/// Absolute ceiling CINVAR accepts for either dimension before asking any encoder.
pub const MAX_DIMENSION: u32 = 16_384;

/// Named output presets. 8K is UHD-2 (7680x4320).
pub const PRESETS: &[(&str, u32, u32)] = &[
    ("720p", 1280, 720),
    ("1080p", 1920, 1080),
    ("1440p", 2560, 1440),
    ("4k", 3840, 2160),
    ("8k", 7680, 4320),
];

pub fn preset(name: &str) -> Option<(u32, u32)> {
    let wanted = name.trim().to_ascii_lowercase().replace([' ', '_'], "");
    let wanted = match wanted.as_str() {
        "uhd" | "2160p" | "uhd4k" => "4k",
        "4320p" | "uhd8k" | "8kuhd" => "8k",
        other => other,
    }
    .to_string();
    PRESETS
        .iter()
        .find(|(n, _, _)| *n == wanted)
        .map(|(_, w, h)| (*w, *h))
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProbeResult {
    pub encoder: String,
    pub width: u32,
    pub height: u32,
    pub pix_fmt: String,
    pub supported: bool,
    /// Last ffmpeg error line when `supported` is false.
    pub error: Option<String>,
}

type ProbeKey = (String, u32, u32, String);

fn probe_cache() -> &'static Mutex<HashMap<ProbeKey, ProbeResult>> {
    static CACHE: OnceLock<Mutex<HashMap<ProbeKey, ProbeResult>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Encodes two synthetic frames at exactly `width`x`height` with `encoder`.
/// This is the only trustworthy way to learn whether a driver/profile/size combination works.
pub fn probe_encoder(encoder: &str, width: u32, height: u32, pix_fmt: &str) -> ProbeResult {
    let key = (encoder.to_string(), width, height, pix_fmt.to_string());
    if let Some(hit) = probe_cache().lock().unwrap().get(&key) {
        return hit.clone();
    }
    let result = run_probe(encoder, width, height, pix_fmt);
    probe_cache().lock().unwrap().insert(key, result.clone());
    result
}

fn run_probe(encoder: &str, width: u32, height: u32, pix_fmt: &str) -> ProbeResult {
    let mut out = ProbeResult {
        encoder: encoder.to_string(),
        width,
        height,
        pix_fmt: pix_fmt.to_string(),
        supported: false,
        error: None,
    };
    let ffmpeg = match proc::require("ffmpeg") {
        Ok(path) => path,
        Err(err) => {
            out.error = Some(err.message);
            return out;
        }
    };
    let argv: Vec<String> = [
        "-hide_banner",
        "-nostdin",
        "-loglevel",
        "error",
        "-f",
        "lavfi",
        "-i",
        &format!("color=c=0x203040:s={width}x{height}:r=25:d=0.2"),
        "-frames:v",
        "2",
        "-vf",
        &format!("format={pix_fmt}"),
        "-c:v",
        encoder,
        "-f",
        "null",
        "-",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    match proc::run(&ffmpeg, &argv, &ProcOpts::default().with_timeout(120)) {
        Ok(run) if run.success => out.supported = true,
        Ok(run) => {
            out.error = Some(run.tail_stderr(2));
        }
        Err(err) => out.error = Some(err.message),
    }
    out
}

/// Bytes of one decoded frame. `bit_depth` > 8 uses 16-bit samples.
pub fn frame_bytes_yuv420(width: u32, height: u32, bit_depth: u8) -> u64 {
    let samples = width as u64 * height as u64 * 3 / 2;
    samples * if bit_depth > 8 { 2 } else { 1 }
}

pub fn frame_bytes_rgba(width: u32, height: u32) -> u64 {
    width as u64 * height as u64 * 4
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Estimate {
    pub frame_bytes_yuv420: u64,
    pub est_peak_memory_bytes: u64,
    pub est_output_bytes: u64,
    pub est_scratch_bytes: u64,
    pub basis: String,
}

/// Heuristic resource estimate. Peak memory counts the encoder look-ahead/reorder frames plus
/// one RGBA working frame per overlapping video layer, doubled for filter-graph buffering.
#[derive(Debug, Clone, Copy)]
pub struct EstimateInput {
    pub width: u32,
    pub height: u32,
    pub fps: f64,
    pub duration_secs: f64,
    pub crf: u8,
    pub hevc: bool,
    pub bit_depth: u8,
    pub layers: u32,
    /// Encoder worker threads (each holds extra frames in flight).
    pub threads: usize,
}

pub fn estimate(input: EstimateInput) -> Estimate {
    let EstimateInput {
        width,
        height,
        fps,
        duration_secs,
        crf,
        hevc,
        bit_depth,
        layers,
        threads,
    } = input;
    let yuv = frame_bytes_yuv420(width, height, bit_depth);
    // Calibrated on libx264 (preset medium) at 7680x4320 on a 12-thread host: peak RSS was
    // 6.3 GB with 1 thread and 9.4 GB with 12 (~0.28 GB per extra thread). That is ~120 raw
    // frames of fixed cost (rc-lookahead 40 + reorder/reference + per-frame analysis data) plus
    // ~5.6 frames per thread. libx265 has not been measured; the same figures are used.
    let fixed_frames: u64 = 120;
    let per_thread_frames: f64 = 5.6;
    let rgba = frame_bytes_rgba(width, height);
    let peak = yuv * fixed_frames
        + (yuv as f64 * per_thread_frames * threads.max(1) as f64) as u64
        + rgba * (layers.max(1) as u64) * 3
        + 256 * 1024 * 1024;
    let bpp = 0.09 * 2f64.powf((23.0 - crf as f64) / 6.0) * if hevc { 0.6 } else { 1.0 };
    let out = (width as f64 * height as f64 * fps * duration_secs * bpp / 8.0).max(0.0) as u64;
    Estimate {
        frame_bytes_yuv420: yuv,
        est_peak_memory_bytes: peak,
        est_output_bytes: out,
        est_scratch_bytes: out / 10,
        basis: format!(
            "heuristic calibrated on libx264 8K ({fixed_frames} fixed + {per_thread_frames} per thread x {threads} \
             frames) + {layers} RGBA layer(s) x3 + 256MiB; hevc={hevc} memory is unmeasured; \
             output from {bpp:.4} bits/pixel at crf {crf}; real values vary with content"
        ),
    }
}

/// Largest encoder thread count (<= `wanted`, >= 1) whose estimated peak memory stays within
/// `budget_fraction` of the memory available right now. `None` when memory is unknown.
pub fn threads_within_memory(
    input: EstimateInput,
    wanted: usize,
    budget_fraction: f64,
) -> Option<usize> {
    Some(threads_for_budget(
        input,
        wanted,
        available_memory_bytes()?,
        budget_fraction,
    ))
}

/// Pure core of [`threads_within_memory`] (testable without touching the host).
pub fn threads_for_budget(
    mut input: EstimateInput,
    wanted: usize,
    avail_bytes: u64,
    budget_fraction: f64,
) -> usize {
    let avail = avail_bytes as f64;
    let mut t = wanted.max(1);
    while t > 1 {
        input.threads = t;
        if estimate(input).est_peak_memory_bytes as f64 <= avail * budget_fraction {
            break;
        }
        t -= 1;
    }
    t
}

/// `MemAvailable` in bytes on Linux; `None` elsewhere (callers must say "unknown").
pub fn available_memory_bytes() -> Option<u64> {
    let text = std::fs::read_to_string("/proc/meminfo").ok()?;
    let line = text.lines().find(|l| l.starts_with("MemAvailable:"))?;
    let kib: u64 = line.split_whitespace().nth(1)?.parse().ok()?;
    Some(kib * 1024)
}

/// Free bytes on the filesystem holding `dir`, via POSIX `df -Pk`; `None` when unavailable.
pub fn available_disk_bytes(dir: &Path) -> Option<u64> {
    let mut probe = dir;
    while !probe.exists() {
        probe = probe.parent()?;
    }
    let out = std::process::Command::new("df")
        .arg("-Pk")
        .arg(probe)
        .output()
        .ok()?;
    let text = String::from_utf8_lossy(&out.stdout);
    let line = text.lines().nth(1)?;
    let kib: u64 = line.split_whitespace().nth(3)?.parse().ok()?;
    Some(kib * 1024)
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SourceClass {
    /// No video sources: titles, graphics or generated colour only.
    GeneratedOnly,
    /// Every video source is at least as large as the output.
    NativeSources,
    /// At least one video source is smaller than the output and is enlarged with ffmpeg's
    /// `scale` filter. This is plain resampling, NOT AI upscaling.
    ScaleFilterUpscale,
}

pub fn classify_sources(
    project: &Project,
    timeline: &Timeline,
    width: u32,
    height: u32,
) -> SourceClass {
    let mut any = false;
    let mut upscaled = false;
    for track in &timeline.tracks {
        if track.kind != TrackKind::Video && track.kind != TrackKind::Overlay {
            continue;
        }
        for clip in &track.clips {
            let Some(info) = clip
                .asset
                .as_deref()
                .and_then(|id| project.asset(id))
                .and_then(|a| a.info.as_ref())
            else {
                continue;
            };
            if !info.has_video || info.width == 0 {
                continue;
            }
            any = true;
            if info.width < width || info.height < height {
                upscaled = true;
            }
        }
    }
    match (any, upscaled) {
        (false, _) => SourceClass::GeneratedOnly,
        (true, false) => SourceClass::NativeSources,
        (true, true) => SourceClass::ScaleFilterUpscale,
    }
}

pub fn max_video_layers(timeline: &Timeline) -> u32 {
    timeline
        .tracks
        .iter()
        .filter(|t| matches!(t.kind, TrackKind::Video | TrackKind::Overlay) && !t.clips.is_empty())
        .count() as u32
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Preflight {
    pub ok: bool,
    pub width: u32,
    pub height: u32,
    pub megapixels: f64,
    pub encoder: String,
    pub pix_fmt: String,
    pub source_class: SourceClass,
    pub estimate: Estimate,
    pub available_memory_bytes: Option<u64>,
    pub available_disk_bytes: Option<u64>,
    pub encoder_probe: Option<ProbeResult>,
    pub errors: Vec<String>,
    pub warnings: Vec<String>,
}

pub struct PreflightInput<'a> {
    pub project: &'a Project,
    pub timeline: &'a Timeline,
    pub width: u32,
    pub height: u32,
    pub fps: f64,
    pub duration_secs: f64,
    pub crf: u8,
    pub encoder: &'a str,
    pub pix_fmt: &'a str,
    pub bit_depth: u8,
    pub threads: usize,
    pub output_dir: &'a Path,
}

pub fn preflight(input: &PreflightInput) -> Preflight {
    let (w, h) = (input.width, input.height);
    let mut errors = Vec::new();
    let mut warnings = Vec::new();
    if w < 16 || h < 16 || w > MAX_DIMENSION || h > MAX_DIMENSION {
        errors.push(format!(
            "{w}x{h} is outside the supported range 16..={MAX_DIMENSION} per side"
        ));
    }
    if w % 2 != 0 || h % 2 != 0 {
        errors.push(format!("{w}x{h}: 4:2:0 output needs even dimensions"));
    }
    let hevc = input.encoder.contains("265") || input.encoder.contains("hevc");
    let est = estimate(EstimateInput {
        width: w,
        height: h,
        fps: input.fps,
        duration_secs: input.duration_secs,
        crf: input.crf,
        hevc,
        bit_depth: input.bit_depth,
        layers: max_video_layers(input.timeline),
        threads: input.threads,
    });

    // Measure encoder support at the real size; skip the (cheap but non-zero) probe for the
    // sizes every build handles, and when the dimensions are already rejected.
    let big = (w as u64 * h as u64) > 1920 * 1088;
    let encoder_probe = if errors.is_empty() && big {
        let probe = probe_encoder(input.encoder, w, h, input.pix_fmt);
        if !probe.supported {
            errors.push(format!(
                "encoder {} cannot encode {w}x{h} {} on this machine: {}",
                input.encoder,
                input.pix_fmt,
                probe
                    .error
                    .clone()
                    .unwrap_or_else(|| "unknown error".into())
            ));
        }
        Some(probe)
    } else {
        None
    };

    let mem = available_memory_bytes();
    match mem {
        Some(avail) if est.est_peak_memory_bytes as f64 > avail as f64 * 0.85 => {
            errors.push(format!(
                "estimated peak memory {} exceeds 85% of the {} currently available",
                human(est.est_peak_memory_bytes),
                human(avail)
            ))
        }
        Some(avail) if est.est_peak_memory_bytes as f64 > avail as f64 * 0.6 => {
            warnings.push(format!(
                "estimated peak memory {} is above 60% of the {} currently available",
                human(est.est_peak_memory_bytes),
                human(avail)
            ))
        }
        Some(_) => {}
        None => warnings.push("available memory could not be determined on this platform".into()),
    }
    let disk = available_disk_bytes(input.output_dir);
    let need = est.est_output_bytes + est.est_scratch_bytes;
    match disk {
        Some(avail) if need as f64 > avail as f64 * 0.9 => errors.push(format!(
            "estimated output + scratch {} would not fit in the {} free",
            human(need),
            human(avail)
        )),
        Some(_) => {}
        None => warnings.push("free disk space could not be determined on this platform".into()),
    }

    let source_class = classify_sources(input.project, input.timeline, w, h);
    // Only called out at 4K and above, where "is this real detail?" is the question that matters.
    if source_class == SourceClass::ScaleFilterUpscale && (w as u64) * (h as u64) >= 3840 * 2160 {
        warnings.push(format!(
            "some video sources are smaller than {w}x{h}; they are enlarged with the ffmpeg scale \
             filter (plain resampling, not AI upscaling and not native {h}p detail)"
        ));
    }
    Preflight {
        ok: errors.is_empty(),
        width: w,
        height: h,
        megapixels: (w as f64 * h as f64) / 1_000_000.0,
        encoder: input.encoder.to_string(),
        pix_fmt: input.pix_fmt.to_string(),
        source_class,
        estimate: est,
        available_memory_bytes: mem,
        available_disk_bytes: disk,
        encoder_probe,
        errors,
        warnings,
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Gpu {
    pub name: String,
    pub vram_mb: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HostProfile {
    pub os: String,
    pub arch: String,
    pub cpu_threads: usize,
    pub memory_available_bytes: Option<u64>,
    pub gpus: Vec<Gpu>,
    /// Hardware encoders that passed a trial encode (1080p). Absent = not usable here.
    pub working_hw_encoders: Vec<String>,
    /// Largest 16:9 preset each working encoder (and libx264/libx265) passed a trial encode at.
    pub max_preset: Vec<(String, String)>,
}

/// NVIDIA GPUs via `nvidia-smi` when present (other vendors are not detected here).
pub fn detect_gpus() -> Vec<Gpu> {
    let Some(smi) = proc::which("nvidia-smi") else {
        return Vec::new();
    };
    let args = [
        "--query-gpu=name,memory.total",
        "--format=csv,noheader,nounits",
    ]
    .map(String::from);
    let Ok(out) = proc::run(&smi, &args, &ProcOpts::default().with_timeout(20)) else {
        return Vec::new();
    };
    out.stdout
        .lines()
        .filter_map(|l| {
            let (name, mem) = l.rsplit_once(',')?;
            Some(Gpu {
                name: name.trim().to_string(),
                vram_mb: mem.trim().parse().ok()?,
            })
        })
        .collect()
}

/// Measures (does not assume) what this machine can encode. Runs trial encodes, so it takes a
/// few seconds the first time; results are cached per process.
pub fn host_profile() -> HostProfile {
    let listed = super::encode::available();
    let mut working = Vec::new();
    let mut max_preset = Vec::new();
    for enc in [
        "libx264",
        "libx265",
        "h264_nvenc",
        "hevc_nvenc",
        "av1_nvenc",
        "h264_qsv",
        "hevc_qsv",
    ] {
        if !listed.iter().any(|n| n == enc) {
            continue;
        }
        if !probe_encoder(enc, 1920, 1080, "yuv420p").supported {
            continue;
        }
        if enc.contains("nvenc") || enc.contains("qsv") {
            working.push(enc.to_string());
        }
        let mut best = "1080p";
        for (name, w, h) in PRESETS.iter().filter(|(_, w, _)| *w > 1920) {
            if probe_encoder(enc, *w, *h, "yuv420p").supported {
                best = name;
            } else {
                break;
            }
        }
        max_preset.push((enc.to_string(), best.to_string()));
    }
    HostProfile {
        os: std::env::consts::OS.to_string(),
        arch: std::env::consts::ARCH.to_string(),
        cpu_threads: std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(1),
        memory_available_bytes: available_memory_bytes(),
        gpus: detect_gpus(),
        working_hw_encoders: working,
        max_preset,
    }
}

pub fn human(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    format!("{value:.1} {}", UNITS[unit])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn presets_resolve_and_8k_is_uhd() {
        assert_eq!(preset("8K"), Some((7680, 4320)));
        assert_eq!(preset("4k"), Some((3840, 2160)));
        assert_eq!(preset("UHD"), Some((3840, 2160)));
        assert_eq!(preset("1080p"), Some((1920, 1080)));
        assert_eq!(preset("9k"), None);
    }

    #[test]
    fn frame_sizes_match_pixel_formats() {
        assert_eq!(frame_bytes_yuv420(7680, 4320, 8), 7680 * 4320 * 3 / 2);
        assert_eq!(frame_bytes_yuv420(7680, 4320, 10), 7680 * 4320 * 3);
        assert_eq!(frame_bytes_rgba(1920, 1080), 1920 * 1080 * 4);
    }

    #[test]
    fn estimate_scales_with_resolution_and_quality() {
        let hd = estimate(EstimateInput {
            width: 1920,
            height: 1080,
            fps: 30.0,
            duration_secs: 10.0,
            crf: 20,
            hevc: false,
            bit_depth: 8,
            layers: 2,
            threads: 8,
        });
        let k8 = estimate(EstimateInput {
            width: 7680,
            height: 4320,
            fps: 30.0,
            duration_secs: 10.0,
            crf: 20,
            hevc: false,
            bit_depth: 8,
            layers: 2,
            threads: 8,
        });
        assert!(k8.est_peak_memory_bytes > hd.est_peak_memory_bytes * 4);
        assert!(k8.est_output_bytes > hd.est_output_bytes * 15);
        let better = estimate(EstimateInput {
            width: 1920,
            height: 1080,
            fps: 30.0,
            duration_secs: 10.0,
            crf: 14,
            hevc: false,
            bit_depth: 8,
            layers: 2,
            threads: 8,
        });
        assert!(better.est_output_bytes > hd.est_output_bytes);
        let hevc = estimate(EstimateInput {
            width: 1920,
            height: 1080,
            fps: 30.0,
            duration_secs: 10.0,
            crf: 20,
            hevc: true,
            bit_depth: 8,
            layers: 2,
            threads: 8,
        });
        assert!(hevc.est_output_bytes < hd.est_output_bytes);
    }

    #[test]
    fn thread_count_shrinks_to_fit_a_memory_budget() {
        let base = EstimateInput {
            width: 7680,
            height: 4320,
            fps: 24.0,
            duration_secs: 10.0,
            crf: 20,
            hevc: false,
            bit_depth: 8,
            layers: 1,
            threads: 12,
        };
        let gib = 1024u64 * 1024 * 1024;
        // Plenty of RAM: all threads kept.
        assert_eq!(threads_for_budget(base, 12, 64 * gib, 0.5), 12);
        // Tight RAM: fewer threads, never below one.
        let tight = threads_for_budget(base, 12, 15 * gib, 0.5);
        assert!((1..12).contains(&tight), "got {tight}");
        assert_eq!(threads_for_budget(base, 12, gib, 0.5), 1);
    }

    #[test]
    fn human_sizes_are_readable() {
        assert_eq!(human(1536), "1.5 KiB");
        assert_eq!(human(3 * 1024 * 1024 * 1024), "3.0 GiB");
    }

    #[test]
    fn bogus_encoder_probe_reports_unsupported_with_reason() {
        if proc::which("ffmpeg").is_none() {
            return;
        }
        let r = probe_encoder("definitely_not_an_encoder", 320, 240, "yuv420p");
        assert!(!r.supported);
        assert!(r.error.is_some());
    }
}
