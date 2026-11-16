//! Video upscaling through Real-ESRGAN (ncnn/Vulkan), run in an external Python environment
//! named by `CINVAR_ESRGAN_PYTHON` (a venv with `realesrgan-ncnn-py==2.0.0`). The variable wins;
//! failing that the path `cinvar setup --pro-ai` recorded in `Config.helpers` is used, so an
//! installed helper runs without the customer exporting anything. The ncnn
//! wrapper needs `libomp.so.5`; the adapter prepends `<venv>/lib-shim` (symlink to
//! `libgomp.so.1`, created at install time) to `LD_LIBRARY_PATH` for the child.
//!
//! Vulkan GPU ids are the device enumeration order (0 = 3060 Ti, 1 = Tesla P40 on this
//! machine). `CINVAR_ESRGAN_GPU` picks the device; the default is 1 to keep renders off
//! the display GPU. Nothing is downloaded at runtime: the wheel bundles the model weights.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use crate::secrets;
use crate::util::paths::HelperOrigin;
use crate::util::proc::{self, ProcOpts};

const HELPER: &str = include_str!("esrgan_helper.py");
pub const ENV_PYTHON: &str = "CINVAR_ESRGAN_PYTHON";
pub const ENV_GPU: &str = "CINVAR_ESRGAN_GPU";
const TIMEOUT_SECS: u64 = 7200;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EnhanceReport {
    pub count: u32,
    pub gpuid: i32,
    pub scale: u32,
    pub seconds: f64,
}

/// The Real-ESRGAN interpreter: the exported variable (user authority), else the path
/// `cinvar setup --pro-ai` recorded in the config. Nothing else is a source, so an
/// unconfigured install still fails with a message instead of guessing.
pub fn python_from_env() -> Result<PathBuf> {
    let Some((path, origin)) = crate::util::paths::helper_path(ENV_PYTHON) else {
        return Err(Error::invalid("Real-ESRGAN is not configured")
            .with_hint(format!("set {ENV_PYTHON} to the python inside your realesrgan venv, or run cinvar setup --pro-ai")));
    };
    if path.is_file() {
        return Ok(path);
    }
    match origin {
        HelperOrigin::Environment => Err(Error::invalid(format!(
            "{ENV_PYTHON} does not point to a python binary: {}",
            path.display()
        ))),
        HelperOrigin::Config => Err(Error::invalid(format!(
            "{ENV_PYTHON} recorded by cinvar setup --pro-ai is not a file: {}",
            path.display()
        ))
        .with_hint("re-run: cinvar setup --pro-ai")),
    }
}

/// `LD_LIBRARY_PATH` entries for the child: the venv-relative `lib-shim` (libomp.so.5)
/// plus any existing value, so a custom toolkit install still resolves.
fn shim_env(python: &Path) -> Option<(String, String)> {
    let shim = python
        .parent()
        .and_then(|bin| bin.parent())
        .map(|venv| venv.join("lib-shim"))?;
    if !shim.is_dir() {
        return None;
    }
    let mut val = shim.to_string_lossy().to_string();
    if let Ok(existing) = std::env::var("LD_LIBRARY_PATH") {
        if !existing.is_empty() {
            val.push(':');
            val.push_str(&existing);
        }
    }
    Some(("LD_LIBRARY_PATH".to_string(), val))
}

/// Parses the helper stdout: progress chatter precedes a single JSON object line.
pub fn parse_report(text: &str) -> Result<EnhanceReport> {
    let json_line = text
        .lines()
        .rev()
        .find(|l| l.trim_start().starts_with('{'))
        .ok_or_else(|| Error::internal("esrgan helper printed no JSON result"))?;
    let report: EnhanceReport = serde_json::from_str(json_line.trim())
        .map_err(|e| Error::internal(format!("esrgan helper returned invalid JSON: {e}")))?;
    if report.count == 0 {
        return Err(Error::internal("esrgan helper upscaled 0 frames"));
    }
    Ok(report)
}

pub fn probe_video(video: &Path) -> Result<(f64, f64, u32, u32, bool)> {
    let ffprobe = proc::which("ffprobe")
        .ok_or_else(|| Error::invalid("ffprobe not found on PATH"))?;
    let out = proc::run(
        &ffprobe,
        &[
            "-v".into(),
            "error".into(),
            "-select_streams".into(),
            "v:0".into(),
            "-show_entries".into(),
            "stream=width,height,avg_frame_rate".into(),
            "-show_entries".into(),
            "format=duration".into(),
            "-of".into(),
            "json".into(),
            video.to_string_lossy().to_string(),
        ],
        &ProcOpts::default(),
    )?;
    if !out.success {
        return Err(Error::internal(format!(
            "ffprobe failed on {}: {}",
            video.display(),
            secrets::redact(&out.tail_stderr(3))
        )));
    }
    #[derive(Deserialize)]
    struct Stream {
        width: u32,
        height: u32,
        #[serde(default)]
        avg_frame_rate: String,
    }
    #[derive(Deserialize)]
    struct Probe {
        streams: Vec<Stream>,
        #[serde(default)]
        format: serde_json::Value,
    }
    let probe: Probe = serde_json::from_str(&out.stdout)
        .map_err(|e| Error::internal(format!("ffprobe JSON unreadable: {e}")))?;
    let s = probe
        .streams
        .first()
        .ok_or_else(|| Error::invalid("no video stream"))?;
    let fps = parse_frame_rate(&s.avg_frame_rate)?;
    let duration = probe
        .format
        .get("duration")
        .and_then(|d| d.as_str())
        .and_then(|d| d.parse::<f64>().ok())
        .ok_or_else(|| Error::internal("ffprobe reported no duration"))?;
    let audio = has_audio(video)?;
    Ok((duration, fps, s.width, s.height, audio))
}

fn parse_frame_rate(rate: &str) -> Result<f64> {
    let (num, den) = rate
        .split_once('/')
        .ok_or_else(|| Error::internal(format!("bad frame rate '{rate}'")))?;
    let n: f64 = num
        .parse()
        .map_err(|_| Error::internal(format!("bad frame rate '{rate}'")))?;
    let d: f64 = den
        .parse()
        .map_err(|_| Error::internal(format!("bad frame rate '{rate}'")))?;
    if d <= 0.0 || n <= 0.0 {
        return Err(Error::internal(format!("bad frame rate '{rate}'")));
    }
    Ok(n / d)
}

fn has_audio(video: &Path) -> Result<bool> {
    let ffprobe = proc::which("ffprobe")
        .ok_or_else(|| Error::invalid("ffprobe not found on PATH"))?;
    let out = proc::run(
        &ffprobe,
        &[
            "-v".into(),
            "error".into(),
            "-select_streams".into(),
            "a".into(),
            "-show_entries".into(),
            "stream=index".into(),
            "-of".into(),
            "csv=p=0".into(),
            video.to_string_lossy().to_string(),
        ],
        &ProcOpts::default(),
    )?;
    Ok(out.success && !out.stdout.trim().is_empty())
}

/// Upscales `video` by `scale` (2 or 4) into `output`, preserving the original audio
/// stream untouched. Frame pipeline: extract PNG -> batch upscale (helper) -> reassemble
/// with `-c:a copy`. Work happens under `work_dir`; frames are removed afterwards.
pub fn enhance_video(
    video: &Path,
    output: &Path,
    scale: u32,
    tilesize: u32,
    work_dir: &Path,
) -> Result<EnhanceReport> {
    enhance_video_with(video, output, scale, tilesize, work_dir, false)
}

/// Upscaling with an explicit `--pro-preview` grant. With `pro_preview` a Free edition may run the
/// real Real-ESRGAN upscale on the first `edition::PRO_PREVIEW_SECS` seconds with the preview
/// notice burned in; without it (or without the flag) the gate refuses as usual.
pub fn enhance_video_with(
    video: &Path,
    output: &Path,
    scale: u32,
    tilesize: u32,
    work_dir: &Path,
    pro_preview: bool,
) -> Result<EnhanceReport> {
    if scale != 2 && scale != 4 {
        return Err(Error::invalid(format!("scale must be 2 or 4, got {scale}")));
    }
    // The edition decides before the environment does: a Free user is told the upscaler is Pro,
    // not that their venv is missing.
    let entitlements = crate::edition::resolve_from_disk();
    let feature_grant =
        entitlements.gate_feature(crate::edition::Feature::EnhanceUpscale, pro_preview)?;
    let python = python_from_env()?;
    let (duration, fps, width, height, audio) = probe_video(video)?;

    // VRAM admission: each tile is tilesize^2 px upscaled at once. Without a fixed
    // tilesize the engine auto-sizes, which on small cards can exceed VRAM; force the
    // caller to pick one for unusually large upscales instead of failing mid-render.
    let out_w = width * scale;
    let out_h = height * scale;
    let export_grant = entitlements.gate_export(out_w, out_h, pro_preview)?;
    let preview_limited = feature_grant.is_preview() || export_grant.is_preview();
    let enhance_secs = if preview_limited {
        duration.min(crate::edition::PRO_PREVIEW_SECS)
    } else {
        duration
    };
    if tilesize == 0 && out_w as u64 * out_h as u64 > 7680 * 4320 {
        return Err(Error::invalid(format!(
            "{out_w}x{out_h} exceeds the safe no-tile budget (8K x4); pass --tile (e.g. 256)"
        )));
    }

    std::fs::create_dir_all(work_dir)?;
    let frames_dir = work_dir.join("frames");
    let upscaled_dir = work_dir.join("upscaled");
    let _ = std::fs::remove_dir_all(&frames_dir);
    let _ = std::fs::remove_dir_all(&upscaled_dir);
    std::fs::create_dir_all(&frames_dir)?;
    std::fs::create_dir_all(&upscaled_dir)?;

    let ffmpeg = proc::which("ffmpeg").ok_or_else(|| Error::invalid("ffmpeg not found on PATH"))?;

    // 1. extract frames (a preview only extracts what it is allowed to output)
    let mut extract_args: Vec<String> = vec![
        "-loglevel".into(),
        "error".into(),
        "-y".into(),
        "-i".into(),
        video.to_string_lossy().to_string(),
    ];
    if preview_limited && enhance_secs < duration {
        extract_args.push("-t".into());
        extract_args.push(format!("{enhance_secs:.3}"));
    }
    extract_args.push("-vsync".into());
    extract_args.push("0".into());
    extract_args.push(frames_dir.join("%09d.png").to_string_lossy().to_string());
    let extract = proc::run(
        &ffmpeg,
        &extract_args,
        &ProcOpts::default().with_timeout(TIMEOUT_SECS),
    )?;
    if !extract.success {
        return Err(Error::internal(format!(
            "frame extraction failed: {}",
            secrets::redact(&extract.tail_stderr(3))
        )));
    }

    // 2. batch upscale via helper
    let script = work_dir.join("esrgan_helper.py");
    std::fs::write(&script, HELPER)?;
    let gpuid = std::env::var(ENV_GPU)
        .ok()
        .and_then(|v| v.parse::<i32>().ok())
        .unwrap_or(1);
    let mut o = ProcOpts::default().with_timeout(TIMEOUT_SECS);
    if let Some(env) = shim_env(&python) {
        o.env.push(env);
    }
    let args = vec![
        "-I".to_string(),
        script.to_string_lossy().to_string(),
        frames_dir.to_string_lossy().to_string(),
        upscaled_dir.to_string_lossy().to_string(),
        scale.to_string(),
        gpuid.to_string(),
        if tilesize > 0 { tilesize.to_string() } else { "0".to_string() },
    ];
    let up_out = proc::run(&python, &args, &o)?;
    // The ncnn/Vulkan wrapper can segfault during teardown *after* all frames are
    // written and the result JSON printed. Accept the run when the JSON parses and the
    // upscaled frame count matches the input count; otherwise surface the real error.
    let input_frames = std::fs::read_dir(&frames_dir)
        .map(|d| d.filter(|e| e.as_ref().map(|e| e.path().extension().map_or(false, |x| x == "png")).unwrap_or(false)).count())
        .unwrap_or(0);
    let report = parse_report(&up_out.stdout).ok().filter(|r| r.count as usize == input_frames);
    let report = match report {
        Some(r) => r,
        None => {
            return Err(Error::internal(format!(
                "Real-ESRGAN helper failed (exit ok={}): {}",
                up_out.success,
                secrets::redact(&up_out.tail_stderr(3))
            )));
        }
    };

    // 3. reassemble: upscaled frames at the source frame rate, original audio copied
    let mut args = vec![
        "-loglevel".into(),
        "error".into(),
        "-y".into(),
        "-framerate".into(),
        format!("{fps:.6}"),
        "-i".into(),
        upscaled_dir.join("%09d.png").to_string_lossy().to_string(),
    ];
    if audio {
        args.push("-i".into());
        args.push(video.to_string_lossy().to_string());
        args.push("-map".into());
        args.push("0:v:0".into());
        args.push("-map".into());
        args.push("1:a:0".into());
        args.push("-c:a".into());
        args.push("copy".into());
    }
    if preview_limited {
        // Same burn-in the renderer uses, at the upscaled size.
        args.push("-vf".into());
        args.push(crate::render::preview::drawtext_spec(out_w, out_h));
    }
    args.push("-c:v".into());
    args.push("libx264".into());
    args.push("-crf".into());
    args.push("18".into());
    args.push("-pix_fmt".into());
    args.push("yuv420p".into());
    if preview_limited {
        args.push("-t".into());
        args.push(format!("{enhance_secs:.3}"));
    }
    args.push(output.to_string_lossy().to_string());
    let mux = proc::run(&ffmpeg, &args, &ProcOpts::default().with_timeout(TIMEOUT_SECS))?;
    if !mux.success {
        return Err(Error::internal(format!(
            "reassemble failed: {}",
            secrets::redact(&mux.tail_stderr(3))
        )));
    }

    let _ = std::fs::remove_dir_all(&frames_dir);
    let _ = std::fs::remove_dir_all(&upscaled_dir);
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_helper_json_after_progress_noise() {
        let raw = "0.00%\n50.00%\n{\"ok\": true, \"count\": 30, \"gpuid\": 1, \"scale\": 4, \"seconds\": 18.9}\n";
        let r = parse_report(raw).unwrap();
        assert_eq!(r.count, 30);
        assert_eq!(r.scale, 4);
    }

    #[test]
    fn rejects_empty_report() {
        assert!(parse_report("{\"count\":0,\"gpuid\":1,\"scale\":4,\"seconds\":0.0}").is_err());
        assert!(parse_report("no json here").is_err());
    }

    #[test]
    fn frame_rate_parsing() {
        assert_eq!(parse_frame_rate("25/1").unwrap(), 25.0);
        assert_eq!(parse_frame_rate("30000/1001").unwrap(), 30000.0 / 1001.0);
        assert!(parse_frame_rate("0/1").is_err());
    }

    #[test]
    fn scale_is_restricted() {
        // 3 is rejected without touching the filesystem (python probe happens first)
        std::env::remove_var(ENV_PYTHON);
        assert!(enhance_video(Path::new("/tmp/x.mp4"), Path::new("/tmp/y.mp4"), 3, 0, Path::new("/tmp/w")).is_err());
    }
}
