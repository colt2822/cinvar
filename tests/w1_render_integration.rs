//! W1 integration evidence: a real ffmpeg render of a two-clip timeline with a
//! title overlay, validated with ffprobe.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use cinvar::error::ErrorCode;
use cinvar::media;
use cinvar::project::model::TransitionSide;
use cinvar::project::model::{Cue, Fit, HwMode, TrackKind, Transform};
use cinvar::project::ops;
use cinvar::project::store::ProjectStore;
use cinvar::render::{self, RenderRequest};
use cinvar::util::proc::{self, ProcOpts};
use cinvar::util::time::{parse_fps, Time};

fn ffmpeg_app() -> Option<PathBuf> {
    proc::which("ffmpeg")
}

fn have_media_tools() -> bool {
    proc::which("ffmpeg").is_some() && proc::which("ffprobe").is_some()
}

fn run_ffmpeg(args: &[String]) -> bool {
    let bin = match proc::require("ffmpeg") {
        Ok(bin) => bin,
        Err(_) => return false,
    };
    match proc::run(&bin, args, &ProcOpts::default().with_timeout(180)) {
        Ok(out) if out.success => true,
        Ok(out) => {
            eprintln!("ffmpeg failed: {}", out.tail_stderr(10));
            false
        }
        Err(err) => {
            eprintln!("ffmpeg could not start: {err}");
            false
        }
    }
}

fn synth_clip(path: &Path, source: &str, frequency: u32) -> bool {
    let args: Vec<String> = vec![
        "-hide_banner".to_string(),
        "-nostdin".to_string(),
        "-loglevel".to_string(),
        "error".to_string(),
        "-y".to_string(),
        "-f".to_string(),
        "lavfi".to_string(),
        "-i".to_string(),
        format!("{source}=size=640x360:rate=30:duration=3"),
        "-f".to_string(),
        "lavfi".to_string(),
        "-i".to_string(),
        format!("sine=frequency={frequency}:sample_rate=48000:duration=3"),
        "-c:v".to_string(),
        "libx264".to_string(),
        "-pix_fmt".to_string(),
        "yuv420p".to_string(),
        "-c:a".to_string(),
        "aac".to_string(),
        "-shortest".to_string(),
        path.to_string_lossy().to_string(),
    ];
    run_ffmpeg(&args)
}

struct Fixture {
    store: ProjectStore,
    project: cinvar::project::model::Project,
    output: PathBuf,
}

fn build_project(dir: &Path) -> Option<Fixture> {
    let source_a = dir.join("clip-a.mp4");
    let source_b = dir.join("clip-b.mp4");
    if !synth_clip(&source_a, "testsrc2", 440) {
        return None;
    }
    if !synth_clip(&source_b, "smptebars", 880) {
        return None;
    }

    let projects_dir = dir.join("projects");
    std::fs::create_dir_all(&projects_dir).ok()?;
    let (store, mut project) = ProjectStore::create(&projects_dir, "W1 Render").ok()?;

    let asset_a = store
        .import_asset(&mut project, &source_a, "test:w1")
        .expect("import first clip");
    let asset_b = store
        .import_asset(&mut project, &source_b, "test:w1")
        .expect("import second clip");
    assert!(asset_a.info.is_some(), "import should probe the asset");
    let info_a = asset_a.info.as_ref().unwrap();
    assert!(info_a.has_video && info_a.has_audio);
    assert_eq!((info_a.width, info_a.height), (640, 360));

    ops::create_timeline(&mut project, "Main", 320, 180, parse_fps("30").unwrap())
        .expect("timeline");
    ops::append_clip(&mut project, None, None, &asset_a.id).expect("append first clip");
    ops::append_clip(&mut project, None, None, &asset_b.id).expect("append second clip");
    ops::add_text(
        &mut project,
        None,
        "CUT: one",
        Time::from_secs_f64(1.0),
        Time::from_secs_f64(2.5),
        None,
    )
    .expect("title overlay");
    store.save(&mut project).expect("save project");

    let output = dir.join("render-out.mp4");
    Some(Fixture {
        store,
        project,
        output,
    })
}

#[test]
fn renders_two_clips_with_a_title_overlay_and_audio() {
    if !have_media_tools() {
        eprintln!("skipping: ffmpeg/ffprobe are not installed");
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    let fixture = match build_project(temp.path()) {
        Some(fixture) => fixture,
        None => {
            eprintln!("skipping: could not synthesise test clips");
            return;
        }
    };
    let Fixture {
        store,
        project,
        output,
    } = fixture;

    let timeline = project.timeline().expect("timeline exists");
    assert_eq!(timeline.duration(), Time::from_secs_f64(6.0));

    let mut request = RenderRequest::for_output(&output);
    request.width = Some(320);
    request.height = Some(180);
    request.crf = Some(28);
    request.burn_captions = true;

    let plan = render::plan_render(&project, &store, &request).expect("plan builds");
    assert_eq!((plan.width, plan.height), (320, 180));
    assert_eq!(plan.duration, Time::from_secs_f64(6.0));
    assert!(plan.args.contains(&"-filter_complex".to_string()));
    assert!(plan.args.contains(&"-progress".to_string()));
    assert!(
        plan.filtergraph.contains("drawtext"),
        "title overlay is burned in: {}",
        plan.filtergraph
    );
    assert!(plan.filtergraph.contains("overlay"));
    assert!(plan.filtergraph.contains("loudnorm"));
    assert!(
        plan.filtergraph.contains("amix"),
        "two clips are mixed: {}",
        plan.filtergraph
    );
    assert_eq!(plan.inputs.len(), 2);
    for warning in &plan.warnings {
        eprintln!("plan warning: {warning}");
    }

    let progress_log: Mutex<Vec<(f32, String)>> = Mutex::new(Vec::new());
    let progress = |p: f32, message: &str| {
        progress_log.lock().unwrap().push((p, message.to_string()));
    };
    let cancelled = || false;
    let outcome =
        render::render(&project, &store, &request, &progress, &cancelled).expect("render succeeds");

    assert!(output.is_file(), "output file exists");
    assert!(outcome.bytes > 0, "output file is not empty");
    assert!(
        outcome.encoder == "libx264" || outcome.encoder == "h264_nvenc",
        "h264 was encoded by the CPU or a probed hardware encoder, got {}",
        outcome.encoder
    );
    assert_eq!((outcome.width, outcome.height), (320, 180));
    eprintln!("encoder used: {}", outcome.encoder);

    let info = media::probe(&output).expect("ffprobe reads the output");
    assert_eq!((info.width, info.height), (320, 180));
    assert!(info.has_video, "output has a video stream");
    assert!(info.has_audio, "output has an audio stream");
    assert_eq!(info.video_codec.as_deref(), Some("h264"));
    assert_eq!(info.audio_codec.as_deref(), Some("aac"));
    assert_eq!(
        info.sample_rate, 48_000,
        "audio is resampled back to the project rate"
    );
    let delta = (info.duration.as_secs_f64() - 6.0).abs();
    assert!(
        delta <= 0.2,
        "duration {} is within 0.2s of 6.0",
        info.duration.as_secs_f64()
    );
    assert!(
        outcome.frames >= 170,
        "expected about 180 frames, got {}",
        outcome.frames
    );

    let validation = render::validate_output(&output).expect("validation runs");
    assert!(validation.valid, "issues: {:?}", validation.issues);
    assert!(validation.has_video && validation.has_audio);
    assert_eq!((validation.width, validation.height), (320, 180));

    let cuts = media::detect_scenes(&output, 0.1).expect("scene detection");
    assert!(
        !cuts.is_empty(),
        "the two clips meet at 3s, so at least one cut exists"
    );
    for cut in &cuts {
        assert!(
            cut.at.as_secs_f64() >= 0.0 && cut.at.as_secs_f64() <= 6.5,
            "cut at {} is inside the file",
            cut.at.as_secs_f64()
        );
    }
    eprintln!(
        "scene cuts: {:?}",
        cuts.iter()
            .map(|c| (c.at.as_secs_f64(), c.score))
            .collect::<Vec<_>>()
    );

    let black = media::detect_black(&output, 0.1, 0.1).expect("black detection");
    eprintln!(
        "black spans: {:?}",
        black
            .iter()
            .map(|s| (s.start.as_secs_f64(), s.end.as_secs_f64()))
            .collect::<Vec<_>>()
    );

    let logged = progress_log.lock().unwrap();
    assert!(!logged.is_empty(), "progress was reported");
    let last = logged.last().unwrap().0;
    assert!(
        (last - 1.0).abs() < 1e-6,
        "progress ends at 1.0, got {last}"
    );

    let evidence = Path::new(env!("CARGO_TARGET_TMPDIR")).join("render-2clips-title.mp4");
    let _ = std::fs::create_dir_all(evidence.parent().unwrap());
    if std::fs::copy(&output, &evidence).is_ok() {
        eprintln!("evidence copy kept at {}", evidence.display());
    }

    eprintln!(
        "rendered {} ({} bytes): {}x{} {} fps, video {} audio {} sample_rate {} duration {:.3}s",
        output.display(),
        outcome.bytes,
        info.width,
        info.height,
        info.fps.map(|f| f.as_f64()).unwrap_or(0.0),
        info.video_codec.clone().unwrap_or_default(),
        info.audio_codec.clone().unwrap_or_default(),
        info.sample_rate,
        info.duration.as_secs_f64()
    );
}

#[test]
fn render_plan_uses_preview_defaults() {
    if !have_media_tools() {
        eprintln!("skipping: ffmpeg/ffprobe are not installed");
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    let fixture = match build_project(temp.path()) {
        Some(fixture) => fixture,
        None => {
            eprintln!("skipping: could not synthesise test clips");
            return;
        }
    };
    let Fixture { store, project, .. } = fixture;
    let mut request = RenderRequest::for_output(temp.path().join("preview.mp4"));
    request.preview = true;
    request.hardware = Some(HwMode::Off);
    let plan = render::plan_render(&project, &store, &request).expect("preview plan");
    assert_eq!(plan.width, 640, "preview is 640 wide");
    assert_eq!(plan.height, 360, "preview keeps the aspect ratio");
    assert_eq!(plan.encoder, "libx264");
    assert!(
        plan.args.iter().any(|a| a == "veryfast"),
        "preview preset is veryfast: {:?}",
        plan.args
    );
    assert!(plan.args.iter().any(|a| a == "30"), "preview crf is 30");
    assert!(
        !plan.args.iter().any(|a| a == "-an"),
        "the timeline has audio"
    );

    let encoders = render::available_encoders();
    assert!(
        encoders.iter().any(|e| e == "libx264"),
        "libx264 is available: {encoders:?}"
    );

    // Auto hardware mode must resolve to an encoder this machine can actually run.
    let mut auto = RenderRequest::for_output(temp.path().join("auto.mp4"));
    auto.preview = true;
    let auto_plan = render::plan_render(&project, &store, &auto).expect("auto plan");
    assert!(
        auto_plan.encoder == "libx264" || auto_plan.encoder == "h264_nvenc",
        "auto picked {}",
        auto_plan.encoder
    );
}

#[test]
fn cancelling_a_render_returns_cancelled() {
    if !have_media_tools() {
        eprintln!("skipping: ffmpeg/ffprobe are not installed");
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    let fixture = match build_project(temp.path()) {
        Some(fixture) => fixture,
        None => {
            eprintln!("skipping: could not synthesise test clips");
            return;
        }
    };
    let Fixture { store, project, .. } = fixture;

    // Already cancelled before the encode starts.
    let request = RenderRequest::for_output(temp.path().join("cancelled-early.mp4"));
    let progress = |_: f32, _: &str| {};
    let always_cancelled = || true;
    let err = render::render(&project, &store, &request, &progress, &always_cancelled)
        .expect_err("cancelled render fails");
    assert_eq!(err.code, ErrorCode::Cancelled);
    assert!(!request.output.exists(), "no partial output is left behind");

    // Cancelled while ffmpeg is running.
    let request = RenderRequest::for_output(temp.path().join("cancelled-late.mp4"));
    let deadline = Instant::now() + Duration::from_millis(120);
    let checks = AtomicUsize::new(0);
    let cancelled_later = || {
        checks.fetch_add(1, Ordering::Relaxed);
        Instant::now() >= deadline
    };
    let err = render::render(&project, &store, &request, &progress, &cancelled_later)
        .expect_err("interrupted render fails");
    assert_eq!(err.code, ErrorCode::Cancelled);
    assert!(
        checks.load(Ordering::Relaxed) > 1,
        "the cancel callback is polled while encoding"
    );
}

#[test]
fn media_helpers_write_real_artifacts() {
    if ffmpeg_app().is_none() {
        eprintln!("skipping: ffmpeg is not installed");
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    let source = temp.path().join("source.mp4");
    if !synth_clip(&source, "testsrc2", 440) {
        eprintln!("skipping: could not synthesise a clip");
        return;
    }

    let info = media::probe(&source).expect("probe reads the clip");
    assert_eq!((info.width, info.height), (640, 360));
    assert_eq!(
        info.fps.map(|f| f.ffprobe_fraction()),
        Some("30/1".to_string())
    );
    assert_eq!(
        info.orientation(),
        cinvar::project::model::Orientation::Landscape
    );

    let frame = media::extract_frame(
        &source,
        Time::from_secs_f64(1.5),
        &temp.path().join("frames/f1.png"),
        Some(160),
    )
    .expect("frame written");
    assert!(frame.is_file());
    assert!(std::fs::metadata(&frame).unwrap().len() > 0);

    let sheet = media::contact_sheet(&source, 4, 2, &temp.path().join("sheet.jpg"), 160)
        .expect("contact sheet");
    assert!(sheet.is_file());
    assert!(std::fs::metadata(&sheet).unwrap().len() > 0);

    let audio =
        media::extract_audio(&source, &temp.path().join("audio.wav")).expect("audio extracted");
    assert!(audio.is_file());

    let proxy =
        media::make_proxy(&source, &temp.path().join("proxy.mp4"), 180).expect("proxy written");
    assert!(proxy.is_file());
    let proxy_info = media::probe(&proxy).expect("proxy probes");
    assert_eq!(proxy_info.height, 180);

    let version = media::ffmpeg_version().expect("ffmpeg version");
    assert!(version.starts_with("ffmpeg version"), "got: {version}");

    let loudness = cinvar::audio::analyze_loudness(&source).expect("loudness");
    assert!(loudness.integrated_lufs.is_finite());
    assert!(
        loudness.integrated_lufs <= 0.0,
        "a -6 dBFS sine is below 0 LUFS"
    );

    let clipping = cinvar::audio::detect_clipping(&source).expect("clipping report");
    assert!(clipping.peak_db.is_finite());
    assert!(
        !clipping.clipped,
        "a -6 dBFS sine is not clipped: {clipping:?}"
    );

    let plan = cinvar::audio::silence_plan(&source, -50.0, 0.3, 0.1).expect("silence plan");
    for (start, end) in plan {
        assert!(end > start);
        assert!(end.as_secs_f64() <= 3.2);
    }
}

#[test]
fn renders_effects_transitions_captions_and_freeze_frames() {
    if !have_media_tools() {
        eprintln!("skipping: ffmpeg/ffprobe are not installed");
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    let fixture = match build_project(temp.path()) {
        Some(fixture) => fixture,
        None => {
            eprintln!("skipping: could not synthesise test clips");
            return;
        }
    };
    let Fixture {
        store, mut project, ..
    } = fixture;

    let mut video_clips: Vec<(i64, String)> = project
        .timeline()
        .unwrap()
        .all_clips()
        .into_iter()
        .filter(|(track, clip)| track.kind == TrackKind::Video && clip.text.is_none())
        .map(|(_, clip)| (clip.timeline_start.nanos, clip.id.clone()))
        .collect();
    video_clips.sort_by_key(|(start, _)| *start);
    let (clip_a, clip_b) = (video_clips[0].1.clone(), video_clips[1].1.clone());

    ops::freeze_frame(&mut project, &clip_a, Time::from_secs_f64(1.0)).unwrap();
    ops::add_effect(
        &mut project,
        &clip_a,
        "brightness",
        serde_json::json!({"value": 0.05}),
    )
    .unwrap();
    ops::add_effect(
        &mut project,
        &clip_a,
        "blur",
        serde_json::json!({"sigma": 1.5}),
    )
    .unwrap();
    ops::add_effect(&mut project, &clip_a, "vignette", serde_json::json!({})).unwrap();
    ops::add_effect(&mut project, &clip_a, "grayscale", serde_json::json!({})).unwrap();

    ops::set_reversed(&mut project, &clip_b, true).unwrap();
    ops::set_speed(&mut project, &clip_b, 1.5).unwrap();
    let transform = Transform {
        fit: Fit::Cover,
        rotation_deg: 8.0,
        opacity: 0.8,
        scale: 1.1,
        x: 6,
        y: -4,
    };
    ops::set_transform(&mut project, &clip_b, transform).unwrap();
    ops::set_transition(
        &mut project,
        &clip_a,
        TransitionSide::Out,
        "crossfade",
        Time::from_secs_f64(0.5),
    )
    .unwrap();
    ops::set_transition(
        &mut project,
        &clip_b,
        TransitionSide::In,
        "crossfade",
        Time::from_secs_f64(0.5),
    )
    .unwrap();
    ops::set_captions(
        &mut project,
        vec![Cue {
            start: Time::from_secs_f64(0.5),
            end: Time::from_secs_f64(2.5),
            text: "burned: captions".into(),
        }],
        "en",
        None,
        "manual",
    )
    .unwrap();
    store.save(&mut project).unwrap();

    let mut request = RenderRequest::for_output(temp.path().join("features.mp4"));
    request.width = Some(320);
    request.height = Some(180);
    request.crf = Some(30);
    request.hardware = Some(HwMode::Off);
    let plan = render::plan_render(&project, &store, &request).expect("feature plan");
    for needle in [
        "reverse",
        "tpad=stop_mode=clone",
        "setpts=PTS/1.500000",
        "eq=brightness=",
        "gblur=sigma=",
        "vignette",
        "hue=s=0",
        "rotate=",
        "colorchannelmixer=aa=0.8000",
        "fade=t=out",
        "alpha=1",
        "enable=between(t",
        "crop=320:180",
    ] {
        assert!(
            plan.filtergraph.contains(needle),
            "filtergraph is missing {needle}:\n{}",
            plan.filtergraph
        );
    }
    assert!(
        (plan.duration.as_secs_f64() - 5.0).abs() < 0.01,
        "a 1.5x clip shortens the timeline to 5s, got {}",
        plan.duration.as_secs_f64()
    );

    let progress = |_: f32, _: &str| {};
    let cancelled = || false;
    let outcome = render::render(&project, &store, &request, &progress, &cancelled)
        .expect("feature render succeeds");
    let info = media::probe(&temp.path().join("features.mp4")).expect("probe");
    assert!(
        (info.duration.as_secs_f64() - 5.0).abs() <= 0.2,
        "duration {}",
        info.duration.as_secs_f64()
    );
    assert_eq!((outcome.width, outcome.height), (320, 180));
    assert!(
        render::validate_output(&temp.path().join("features.mp4"))
            .unwrap()
            .valid
    );
}

#[test]
fn encodes_vp9_webm_and_hevc() {
    if !have_media_tools() {
        eprintln!("skipping: ffmpeg/ffprobe are not installed");
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    let fixture = match build_project(temp.path()) {
        Some(fixture) => fixture,
        None => {
            eprintln!("skipping: could not synthesise test clips");
            return;
        }
    };
    let Fixture { store, project, .. } = fixture;
    let progress = |_: f32, _: &str| {};
    let cancelled = || false;

    let mut webm = RenderRequest::for_output(temp.path().join("alt.webm"));
    webm.width = Some(320);
    webm.height = Some(180);
    webm.codec = Some("vp9".into());
    webm.format = Some("webm".into());
    webm.crf = Some(45);
    webm.hardware = Some(HwMode::Off);
    webm.range = Some((Time::ZERO, Time::from_secs_f64(1.5)));
    let plan = render::plan_render(&project, &store, &webm).expect("vp9 plan");
    assert_eq!(plan.encoder, "libvpx-vp9");
    assert!(
        plan.args.iter().any(|a| a == "libopus"),
        "webm audio is opus: {:?}",
        plan.args
    );
    render::render(&project, &store, &webm, &progress, &cancelled).expect("vp9 render");
    let info = media::probe(&temp.path().join("alt.webm")).expect("probe webm");
    assert_eq!(info.video_codec.as_deref(), Some("vp9"));
    assert!(info.format.contains("webm"), "container: {}", info.format);

    let mut hevc = RenderRequest::for_output(temp.path().join("alt-hevc.mp4"));
    hevc.width = Some(320);
    hevc.height = Some(180);
    hevc.codec = Some("hevc".into());
    hevc.crf = Some(32);
    hevc.hardware = Some(HwMode::Off);
    hevc.range = Some((Time::ZERO, Time::from_secs_f64(1.5)));
    let plan = render::plan_render(&project, &store, &hevc).expect("hevc plan");
    assert_eq!(plan.encoder, "libx265");
    render::render(&project, &store, &hevc, &progress, &cancelled).expect("hevc render");
    let info = media::probe(&temp.path().join("alt-hevc.mp4")).expect("probe hevc");
    assert_eq!(info.video_codec.as_deref(), Some("hevc"));
}

#[test]
fn renders_a_video_only_timeline_without_audio() {
    if !have_media_tools() {
        eprintln!("skipping: ffmpeg/ffprobe are not installed");
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    let silent = temp.path().join("silent.mp4");
    let args: Vec<String> = vec![
        "-hide_banner".to_string(),
        "-nostdin".to_string(),
        "-loglevel".to_string(),
        "error".to_string(),
        "-y".to_string(),
        "-f".to_string(),
        "lavfi".to_string(),
        "-i".to_string(),
        "testsrc2=size=320x180:rate=30:duration=2".to_string(),
        "-an".to_string(),
        "-c:v".to_string(),
        "libx264".to_string(),
        "-pix_fmt".to_string(),
        "yuv420p".to_string(),
        silent.to_string_lossy().to_string(),
    ];
    if !run_ffmpeg(&args) {
        eprintln!("skipping: could not synthesise a silent clip");
        return;
    }

    let (store, mut project) = ProjectStore::create(temp.path(), "W1 Silent").unwrap();
    let asset = store
        .import_asset(&mut project, &silent, "test:w1")
        .unwrap();
    assert!(asset.info.as_ref().map(|i| !i.has_audio).unwrap_or(false));
    ops::create_timeline(&mut project, "Main", 320, 180, parse_fps("30").unwrap()).unwrap();
    ops::append_clip(&mut project, None, None, &asset.id).unwrap();
    store.save(&mut project).unwrap();

    let output = temp.path().join("silent-render.mp4");
    let mut request = RenderRequest::for_output(&output);
    request.hardware = Some(HwMode::Off);
    let plan = render::plan_render(&project, &store, &request).expect("silent plan");
    assert!(
        plan.args.iter().any(|a| a == "-an"),
        "a silent timeline gets -an: {:?}",
        plan.args
    );
    assert!(!plan.filtergraph.contains("amix"));
    assert!(!plan.filtergraph.contains("loudnorm"));

    let progress = |_: f32, _: &str| {};
    let cancelled = || false;
    render::render(&project, &store, &request, &progress, &cancelled).expect("silent render");
    let info = media::probe(&output).expect("probe silent render");
    assert!(info.has_video);
    assert!(!info.has_audio, "no audio stream is invented");
    assert!(render::validate_output(&output).unwrap().valid);
}
