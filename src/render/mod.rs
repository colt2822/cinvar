use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::error::{Error, ErrorCode, Result};
use crate::project::model::{HwMode, Project};
use crate::project::store::ProjectStore;
use crate::util::paths;
use crate::util::proc;
use crate::util::time::{Fps, Time};

mod anim;
mod encode;

pub use anim::{
    check_easing as check_keyframe_easing, check_property as check_keyframe_property,
    ANIMATABLE as ANIMATABLE_PROPERTIES, EASINGS as KEYFRAME_EASINGS,
};
pub mod capability;
mod graph;
pub mod verify;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RenderRequest {
    pub output: PathBuf,
    pub timeline_id: Option<String>,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub fps: Option<Fps>,
    pub crf: Option<u8>,
    pub codec: Option<String>,
    pub format: Option<String>,
    pub hardware: Option<HwMode>,
    pub range: Option<(Time, Time)>,
    pub preview: bool,
    pub burn_captions: bool,
    /// Named size preset ("720p", "1080p", "1440p", "4k", "8k"); explicit width/height win.
    #[serde(default)]
    pub size_preset: Option<String>,
    /// 8 (default) or 10. Validated against the encoder before the render is accepted.
    #[serde(default)]
    pub bit_depth: Option<u8>,
}

impl RenderRequest {
    pub fn for_output(output: impl Into<PathBuf>) -> RenderRequest {
        RenderRequest {
            output: output.into(),
            timeline_id: None,
            width: None,
            height: None,
            fps: None,
            crf: None,
            codec: None,
            format: None,
            hardware: None,
            range: None,
            preview: false,
            burn_captions: true,
            size_preset: None,
            bit_depth: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RenderPlan {
    pub args: Vec<String>,
    pub inputs: Vec<PathBuf>,
    pub filtergraph: String,
    pub output: PathBuf,
    /// ffmpeg writes here; it is renamed to `output` only after the file validates, so an
    /// interrupted or failed render never leaves a truncated file at the final name.
    #[serde(default)]
    pub partial_output: PathBuf,
    pub width: u32,
    pub height: u32,
    pub fps: Fps,
    pub duration: Time,
    pub estimated_frames: u64,
    pub encoder: String,
    pub warnings: Vec<String>,
    /// Audio-only ffmpeg arguments that measure the mix loudness for a two-pass normalisation.
    #[serde(default)]
    pub measure_args: Option<Vec<String>>,
    /// The single-pass loudnorm filter text inside `args` that the measured version replaces.
    #[serde(default)]
    pub single_pass_loudnorm: Option<String>,
    /// Resolution/encoder/memory/disk preflight for this plan (errors make `render` refuse).
    #[serde(default)]
    pub preflight: Option<capability::Preflight>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RenderOutcome {
    pub output: PathBuf,
    pub frames: u64,
    pub duration: Time,
    pub width: u32,
    pub height: u32,
    pub elapsed_secs: f64,
    pub bytes: u64,
    pub encoder: String,
    /// Features that could not be rendered faithfully (animated keyframes, unsupported
    /// transitions). Reported so nothing is silently dropped.
    #[serde(default)]
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OutputValidation {
    pub valid: bool,
    pub duration: Time,
    pub width: u32,
    pub height: u32,
    pub has_video: bool,
    pub has_audio: bool,
    pub issues: Vec<String>,
}

/// Format a duration for an ffmpeg argument.
pub(crate) fn secs(value: f64) -> String {
    format!("{:.4}", value.max(0.0))
}

fn even(value: u32) -> u32 {
    value & !1
}

fn duration_out_of_tolerance(actual: Time, expected: Time) -> bool {
    let tolerance = (expected.as_secs_f64().abs() * 0.02).max(0.5);
    (actual.as_secs_f64() - expected.as_secs_f64()).abs() > tolerance
}

fn dedupe(mut warnings: Vec<String>) -> Vec<String> {
    let mut seen: Vec<String> = Vec::new();
    warnings.retain(|w| {
        if seen.contains(w) {
            false
        } else {
            seen.push(w.clone());
            true
        }
    });
    warnings
}

fn partial_path(output: &Path) -> PathBuf {
    let name = output
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("render");
    let ext = output.extension().and_then(|e| e.to_str()).unwrap_or("mp4");
    let stem = output.file_stem().and_then(|n| n.to_str()).unwrap_or(name);
    output.with_file_name(format!(".{stem}.partial.{ext}"))
}

/// Deletes the partial file unless the render completed and renamed it.
struct PartialGuard(PathBuf);

impl Drop for PartialGuard {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

pub fn plan_render(
    project: &Project,
    store: &ProjectStore,
    request: &RenderRequest,
) -> Result<RenderPlan> {
    let timeline = match request.timeline_id.as_deref() {
        Some(id) => project
            .timeline_by_id(id)
            .ok_or_else(|| Error::not_found(format!("timeline {id} not found")))?,
        None => project.timeline().ok_or_else(|| {
            Error::invalid("this project has no timeline to render")
                .with_hint("create a timeline and add clips before rendering")
        })?,
    };
    let settings = &project.render;

    // The timeline canvas is authoritative; the project render settings are the
    // fallback for a timeline that never had a size of its own.
    let canvas_width = if timeline.width > 0 {
        timeline.width
    } else {
        settings.width.max(16)
    };
    let canvas_height = if timeline.height > 0 {
        timeline.height
    } else {
        settings.height.max(16)
    };
    let preset_size = match request.size_preset.as_deref() {
        Some(name) => Some(capability::preset(name).ok_or_else(|| {
            Error::invalid(format!("unknown size preset '{name}'"))
                .with_hint("use one of: 720p, 1080p, 1440p, 4k, 8k, or pass width and height")
        })?),
        None => None,
    };
    let width_hint = request
        .width
        .filter(|w| *w >= 16)
        .or(preset_size.map(|p| p.0))
        .unwrap_or_else(|| {
            if request.preview {
                640
            } else {
                canvas_width.max(16)
            }
        });
    let height_hint = request
        .height
        .filter(|h| *h >= 16)
        .or(preset_size.map(|p| p.1))
        .or({
            if request.preview {
                None
            } else {
                Some(canvas_height)
            }
        })
        .filter(|h| *h >= 16);
    let height = match height_hint {
        Some(h) => h,
        None => {
            let ratio = if timeline.width > 0 {
                timeline.height as f64 / timeline.width as f64
            } else {
                9.0 / 16.0
            };
            (width_hint as f64 * ratio).round().max(16.0) as u32
        }
    };
    // No silent clamping: a size the engine cannot honour is refused with the reason.
    let (width, height) = (even(width_hint), even(height));
    if width < 16
        || height < 16
        || width > capability::MAX_DIMENSION
        || height > capability::MAX_DIMENSION
    {
        return Err(
            Error::invalid(format!("unsupported render size {width}x{height}")).with_hint(format!(
                "each side must be 16..={} pixels",
                capability::MAX_DIMENSION
            )),
        );
    }

    let fps = request.fps.unwrap_or(timeline.fps);
    if !fps.is_valid() || fps.as_f64() > 240.0 {
        return Err(Error::invalid(format!(
            "unsupported frame rate {}/{}",
            fps.num, fps.den
        )));
    }

    let (range_start, range_end) = request
        .range
        .map(|(a, b)| (a.clamp_non_negative(), b.clamp_non_negative()))
        .unwrap_or((Time::ZERO, timeline.duration()));
    if range_end <= range_start {
        return Err(Error::invalid("the render range is empty").with_hint(
            "pass a range whose end is after its start, or add clips to the timeline first",
        ));
    }
    let duration = range_end - range_start;

    let crf = request
        .crf
        .unwrap_or(if request.preview { 30 } else { settings.crf })
        .min(51);
    let preset = if request.preview {
        "veryfast".to_string()
    } else if settings.preset.trim().is_empty() {
        "medium".to_string()
    } else {
        settings.preset.clone()
    };
    let loudness_target = if settings.loudness_target.is_finite() {
        settings.loudness_target
    } else {
        -14.0
    };
    let sample_rate = if timeline.sample_rate >= 8_000 {
        timeline.sample_rate
    } else {
        48_000
    };

    let params = graph::GraphParams {
        width,
        height,
        fps,
        duration,
        range_start,
        background: timeline.background.clone(),
        burn_captions: request.burn_captions,
        loudness_target,
        sample_rate,
    };
    let built = graph::build(project, store, timeline, &params)?;
    let enc = encode::choose(
        project,
        request,
        request.preview,
        crf,
        &preset,
        built.audio_label.is_some(),
        (width, height),
    );

    let mut args: Vec<String> = vec![
        "-hide_banner".to_string(),
        "-nostdin".to_string(),
        "-nostats".to_string(),
        "-loglevel".to_string(),
        "error".to_string(),
        "-progress".to_string(),
        "pipe:1".to_string(),
        "-y".to_string(),
    ];
    // Bound filter-graph parallelism on big frames: every extra worker holds whole frames.
    if (width as u64) * (height as u64) > 3840 * 2160 {
        args.extend(
            ["-filter_threads", "2", "-filter_complex_threads", "2"]
                .iter()
                .map(|v| v.to_string()),
        );
    }
    for input in &built.input_args {
        args.extend(input.clone());
    }
    args.push("-filter_complex".to_string());
    args.push(built.filtergraph.clone());
    args.push("-map".to_string());
    args.push(format!("[{}]", built.video_label));
    if let Some(audio) = &built.audio_label {
        args.push("-map".to_string());
        args.push(format!("[{audio}]"));
    }
    args.push("-t".to_string());
    args.push(secs(duration.as_secs_f64()));
    let video_args_at = args.len();
    args.extend(enc.video_args.clone());
    args.extend(enc.audio_args.clone());
    if let Some(muxer) = &enc.muxer {
        args.push("-f".to_string());
        args.push(muxer.clone());
    }
    let partial_output = partial_path(&request.output);
    // The partial name is predictable: never let a symlink (or stale file) planted there be the
    // thing ffmpeg opens with -y. remove_file removes a link itself, not its target.
    let _ = std::fs::remove_file(&partial_output);
    args.push(partial_output.to_string_lossy().to_string());

    // Bound memory: shrink encoder worker threads until the estimate fits half of free RAM.
    let cpu_threads = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(1);
    let mut threads = cpu_threads;
    let mut thread_warning = None;
    if (width as u64) * (height as u64) > 3840 * 2160 {
        let probe_input = capability::EstimateInput {
            width,
            height,
            fps: fps.as_f64(),
            duration_secs: duration.as_secs_f64(),
            crf,
            hevc: enc.encoder.contains("265") || enc.encoder.contains("hevc"),
            bit_depth: enc.bit_depth,
            layers: capability::max_video_layers(timeline),
            threads,
        };
        if let Some(fit) = capability::threads_within_memory(probe_input, cpu_threads, 0.5) {
            if fit < cpu_threads {
                thread_warning = Some(format!(
                    "limited encoder threads to {fit} (of {cpu_threads}) to keep estimated peak memory within half of free RAM"
                ));
            }
            threads = fit;
        }
        if !enc.encoder.contains("nvenc") {
            args.splice(
                video_args_at..video_args_at,
                ["-threads".to_string(), threads.to_string()],
            );
        }
    }
    let preflight = capability::preflight(&capability::PreflightInput {
        project,
        timeline,
        width,
        height,
        fps: fps.as_f64(),
        duration_secs: duration.as_secs_f64(),
        crf,
        encoder: &enc.encoder,
        pix_fmt: &enc.pix_fmt,
        bit_depth: enc.bit_depth,
        threads,
        output_dir: request.output.parent().unwrap_or(Path::new(".")),
    });
    let mut warnings = built.warnings.clone();
    warnings.extend(enc.warnings.clone());
    warnings.extend(preflight.warnings.clone());
    warnings.extend(thread_warning);
    if let Some(ext) = request.output.extension().and_then(|e| e.to_str()) {
        let ext = ext.to_ascii_lowercase();
        if !ext.is_empty() && ext != enc.container {
            warnings.push(format!(
                "output extension '{ext}' does not match the {} container resolved for this render; ffmpeg muxes by extension",
                enc.container
            ));
        }
    }
    let warnings = dedupe(warnings);

    let (measure_args, single_pass_loudnorm) = match &built.audio_measure {
        Some(m) => {
            let mut a: Vec<String> = [
                "-hide_banner",
                "-nostdin",
                "-nostats",
                "-loglevel",
                "info",
                "-y",
            ]
            .iter()
            .map(|v| v.to_string())
            .collect();
            for input in &built.input_args {
                a.extend(input.clone());
            }
            a.push("-filter_complex".to_string());
            a.push(format!(
                "{};[{}]{}:print_format=json[lnmeasure]",
                m.graph, m.mix_label, m.single_pass
            ));
            a.extend([
                "-map".to_string(),
                "[lnmeasure]".to_string(),
                "-t".to_string(),
                secs(duration.as_secs_f64()),
            ]);
            a.extend(["-f".to_string(), "null".to_string(), "-".to_string()]);
            (Some(a), Some(m.single_pass.clone()))
        }
        None => (None, None),
    };

    Ok(RenderPlan {
        args,
        inputs: built.inputs,
        filtergraph: built.filtergraph,
        output: request.output.clone(),
        partial_output,
        width,
        height,
        fps,
        duration,
        estimated_frames: duration.to_frames(fps).max(0) as u64,
        encoder: enc.encoder,
        warnings,
        measure_args,
        single_pass_loudnorm,
        preflight: Some(preflight),
    })
}

/// Two-pass loudness: measure the mix, then swap the single-pass loudnorm for a linear one with
/// the measured values. If measurement fails the single-pass filter stays and a warning says so.
fn apply_measured_loudness(plan: &mut RenderPlan, ffmpeg: &Path) {
    let (Some(measure), Some(single)) =
        (plan.measure_args.clone(), plan.single_pass_loudnorm.clone())
    else {
        return;
    };
    let result = proc::run(
        ffmpeg,
        &measure,
        &proc::ProcOpts::default().with_timeout(600),
    );
    let text = match result {
        Ok(out) if out.success => out.stderr,
        Ok(out) => {
            plan.warnings.push(format!(
                "loudness measurement failed, used single-pass normalisation: {}",
                out.tail_stderr(2)
            ));
            return;
        }
        Err(err) => {
            plan.warnings.push(format!(
                "loudness measurement could not run, used single-pass normalisation: {err}"
            ));
            return;
        }
    };
    let Some(start) = text.rfind('{') else {
        plan.warnings.push(
            "loudness measurement produced no result, used single-pass normalisation".to_string(),
        );
        return;
    };
    let Some(end) = text[start..].find('}') else {
        return;
    };
    let Ok(json) = serde_json::from_str::<serde_json::Value>(&text[start..start + end + 1]) else {
        plan.warnings.push(
            "loudness measurement was unreadable, used single-pass normalisation".to_string(),
        );
        return;
    };
    let get = |key: &str| {
        json.get(key)
            .and_then(|v| v.as_str())
            .and_then(|v| v.parse::<f64>().ok())
    };
    let (Some(i), Some(tp), Some(lra), Some(thresh), Some(offset)) = (
        get("input_i"),
        get("input_tp"),
        get("input_lra"),
        get("input_thresh"),
        get("target_offset"),
    ) else {
        return;
    };
    if !i.is_finite() || i < -69.0 {
        plan.warnings.push(
            "the mix is effectively silent, so loudness normalisation was skipped".to_string(),
        );
        return;
    }
    let measured = format!(
        "{single}:measured_I={i:.2}:measured_TP={tp:.2}:measured_LRA={lra:.2}:measured_thresh={thresh:.2}:offset={offset:.2}:linear=true"
    );
    for arg in plan.args.iter_mut() {
        if arg.contains(&single) {
            *arg = arg.replace(&single, &measured);
        }
    }
}

pub fn render(
    project: &Project,
    store: &ProjectStore,
    request: &RenderRequest,
    progress: &dyn Fn(f32, &str),
    cancelled: &dyn Fn() -> bool,
) -> Result<RenderOutcome> {
    let mut plan = plan_render(project, store, request)?;
    if let Some(pre) = &plan.preflight {
        if !pre.ok {
            return Err(Error::new(
                ErrorCode::Unsupported,
                format!(
                    "render refused by preflight ({}x{} {}): {}",
                    pre.width,
                    pre.height,
                    pre.encoder,
                    pre.errors.join("; ")
                ),
            )
            .with_hint("lower the resolution, pick another codec/encoder, free memory or disk, or render on a larger backend"));
        }
    }
    if let Some(parent) = plan.output.parent() {
        if !parent.as_os_str().is_empty() {
            paths::ensure_dir(parent)?;
        }
    }
    let ffmpeg = proc::require("ffmpeg")
        .map_err(|e| e.with_hint("install ffmpeg (version 6 or newer) and run: cinvar setup"))?;
    if cancelled() {
        return Err(Error::cancelled("render cancelled before it started"));
    }

    let _partial_guard = PartialGuard(plan.partial_output.clone());
    let started = Instant::now();
    progress(0.0, "measuring loudness");
    apply_measured_loudness(&mut plan, &ffmpeg);
    progress(
        0.0,
        &format!(
            "encoding {}x{} at {:.3} fps with {}",
            plan.width,
            plan.height,
            plan.fps.as_f64(),
            plan.encoder
        ),
    );

    let mut child = Command::new(&ffmpeg)
        .args(&plan.args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| {
            Error::new(ErrorCode::Render, format!("could not start ffmpeg: {e}"))
                .with_hint("run cinvar doctor to check the ffmpeg installation")
        })?;

    let position = Arc::new(AtomicI64::new(0));
    let frame_count = Arc::new(AtomicI64::new(0));
    let tail: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let total_us = (plan.duration.as_secs_f64() * 1_000_000.0).max(1.0);

    let stdout_handle = child.stdout.take().map(|pipe| {
        let position = Arc::clone(&position);
        let frame_count = Arc::clone(&frame_count);
        std::thread::spawn(move || {
            let reader = BufReader::new(pipe);
            for line in reader.lines() {
                let Ok(line) = line else { break };
                let line = line.trim();
                if let Some(value) = line.strip_prefix("out_time_us=") {
                    if let Ok(us) = value.trim().parse::<i64>() {
                        position.store(us, Ordering::Relaxed);
                    }
                } else if let Some(value) = line.strip_prefix("out_time_ms=") {
                    if let Ok(us) = value.trim().parse::<i64>() {
                        if position.load(Ordering::Relaxed) < us {
                            position.store(us, Ordering::Relaxed);
                        }
                    }
                } else if let Some(value) = line.strip_prefix("frame=") {
                    if let Ok(frames) = value.trim().parse::<i64>() {
                        frame_count.store(frames, Ordering::Relaxed);
                    }
                }
            }
        })
    });
    let stderr_handle = child.stderr.take().map(|pipe| {
        let tail = Arc::clone(&tail);
        std::thread::spawn(move || {
            let reader = BufReader::new(pipe);
            for line in reader.lines() {
                let Ok(line) = line else { break };
                let mut buffer = match tail.lock() {
                    Ok(buffer) => buffer,
                    Err(poisoned) => poisoned.into_inner(),
                };
                buffer.push(line);
                if buffer.len() > 80 {
                    let excess = buffer.len() - 80;
                    buffer.drain(0..excess);
                }
            }
        })
    });

    let timeout = Duration::from_secs((plan.duration.as_secs_f64() * 30.0) as u64 + 900);
    let mut last_report = Instant::now();
    let status = loop {
        if cancelled() {
            let _ = child.kill();
            let _ = child.wait();
            if let Some(handle) = stdout_handle {
                let _ = handle.join();
            }
            if let Some(handle) = stderr_handle {
                let _ = handle.join();
            }
            return Err(Error::cancelled("render cancelled"));
        }
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => {}
            Err(e) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(Error::io(format!("waiting for ffmpeg failed: {e}")));
            }
        }
        if started.elapsed() > timeout {
            let _ = child.kill();
            let _ = child.wait();
            return Err(Error::new(
                ErrorCode::Render,
                format!("render timed out after {}s", timeout.as_secs()),
            )
            .with_hint("reduce the render range or check the machine load"));
        }
        if last_report.elapsed() >= Duration::from_millis(150) {
            let us = position.load(Ordering::Relaxed) as f64;
            let fraction = (us / total_us).clamp(0.0, 0.99) as f32;
            let frames = frame_count.load(Ordering::Relaxed).max(0);
            progress(fraction, &format!("frame {frames}"));
            last_report = Instant::now();
        }
        std::thread::sleep(Duration::from_millis(40));
    };

    if let Some(handle) = stdout_handle {
        let _ = handle.join();
    }
    if let Some(handle) = stderr_handle {
        let _ = handle.join();
    }

    if !status.success() {
        let lines = match tail.lock() {
            Ok(buffer) => buffer.clone(),
            Err(poisoned) => poisoned.into_inner().clone(),
        };
        let start = lines.len().saturating_sub(30);
        let detail = crate::secrets::redact(&lines[start..].join("\n"));
        let exit = status
            .code()
            .map(|c| c.to_string())
            .unwrap_or_else(|| "signal".to_string());
        let hint = if detail.contains("No such filter: 'drawtext'") {
            "this FFmpeg build has no 'drawtext' filter (needs libfreetype), so titles and burned-in captions cannot render. Install a full FFmpeg build (e.g. Homebrew: brew install homebrew-ffmpeg/ffmpeg/ffmpeg --with-freetype; Windows: the gyan.dev full build) or remove text from the timeline. `cinvar doctor` shows drawtext support"
        } else {
            "check the render plan for unsupported filters or a bad input file"
        };
        return Err(Error::new(
            ErrorCode::Render,
            format!("ffmpeg exited with {exit}\n{detail}"),
        )
        .with_hint(hint));
    }

    progress(0.97, "validating output");
    let validation = validate_output(&plan.partial_output)?;
    if !validation.valid {
        return Err(Error::new(
            ErrorCode::Render,
            format!(
                "render produced an unusable file: {}",
                validation.issues.join("; ")
            ),
        ));
    }
    if duration_out_of_tolerance(validation.duration, plan.duration) {
        return Err(Error::new(
            ErrorCode::Render,
            format!(
                "rendered duration {:.3}s does not match the {:.3}s range",
                validation.duration.as_secs_f64(),
                plan.duration.as_secs_f64()
            ),
        )
        .with_hint("the render may have been truncated; check the ffmpeg log"));
    }

    std::fs::rename(&plan.partial_output, &plan.output).map_err(|e| {
        Error::io(format!(
            "could not move the finished render into place at {}: {e}",
            plan.output.display()
        ))
    })?;
    let bytes = std::fs::metadata(&plan.output)
        .map(|m| m.len())
        .unwrap_or(0);
    let elapsed_secs = started.elapsed().as_secs_f64();
    progress(1.0, "render complete");
    Ok(RenderOutcome {
        output: plan.output.clone(),
        frames: validation.duration.to_frames(plan.fps).max(0) as u64,
        duration: validation.duration,
        width: validation.width,
        height: validation.height,
        elapsed_secs,
        bytes,
        encoder: plan.encoder,
        warnings: plan.warnings.clone(),
    })
}

pub fn validate_output(path: &Path) -> Result<OutputValidation> {
    if !path.is_file() {
        return Err(Error::not_found(format!(
            "rendered file not found: {}",
            path.display()
        )));
    }
    let mut issues: Vec<String> = Vec::new();
    let size = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
    if size == 0 {
        issues.push("the file is empty".to_string());
    }
    let info = match crate::media::probe(path) {
        Ok(info) => info,
        Err(e) => {
            issues.push(format!("the output is not readable media: {}", e.message));
            return Ok(OutputValidation {
                valid: false,
                duration: Time::ZERO,
                width: 0,
                height: 0,
                has_video: false,
                has_audio: false,
                issues,
            });
        }
    };
    if info.duration.nanos <= 0 {
        issues.push("the output has no measurable duration".to_string());
    }
    if !info.has_video {
        issues.push("the output has no video stream".to_string());
    }
    if info.has_video && (info.width < 16 || info.height < 16) {
        issues.push(format!(
            "the output frame size {}x{} is unusable",
            info.width, info.height
        ));
    }
    Ok(OutputValidation {
        valid: issues.is_empty(),
        duration: info.duration,
        width: info.width,
        height: info.height,
        has_video: info.has_video,
        has_audio: info.has_audio,
        issues,
    })
}

pub fn available_encoders() -> Vec<String> {
    encode::available()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn duration_tolerance_is_the_greater_of_half_a_second_and_two_percent() {
        let expected = Time::from_secs_f64(10.0);
        assert!(!duration_out_of_tolerance(
            Time::from_secs_f64(10.4),
            expected
        ));
        assert!(duration_out_of_tolerance(
            Time::from_secs_f64(10.6),
            expected
        ));

        let long = Time::from_secs_f64(120.0);
        assert!(!duration_out_of_tolerance(Time::from_secs_f64(122.0), long));
        assert!(duration_out_of_tolerance(Time::from_secs_f64(123.0), long));
    }

    #[test]
    fn dedupes_warnings_in_order() {
        let warnings = dedupe(vec!["a".to_string(), "b".to_string(), "a".to_string()]);
        assert_eq!(warnings, vec!["a".to_string(), "b".to_string()]);
    }

    #[test]
    fn validates_a_missing_output() {
        let err = validate_output(Path::new("/nonexistent/cinvar-render.mp4")).unwrap_err();
        assert_eq!(err.code, ErrorCode::NotFound);
    }

    #[test]
    fn request_defaults_are_sane() {
        let request = RenderRequest::for_output("/tmp/x.mp4");
        assert_eq!(request.codec, None);
        assert!(request.burn_captions);
        assert!(!request.preview);
    }
}
