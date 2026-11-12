//! 8K (7680x4320) through CINVAR's own render path, verified by decoding every frame.
//! Native 8K *rendering* (generated synthetic canvas), 8K *encoding*, and AI upscaling are
//! reported separately; this file establishes the first two for a synthetic fixture on THIS
//! machine only. No claim about other hardware.

use cinvar::project::model::TrackKind;
use cinvar::project::ops::{self, ClipRequest};
use cinvar::project::store::ProjectStore;
use cinvar::render::{self, capability, verify, RenderRequest};
use cinvar::util::proc;
use cinvar::util::time::{Rational, Time};

fn ffmpeg_ok() -> bool {
    proc::which("ffmpeg").is_some() && proc::which("ffprobe").is_some()
}

fn build_project(
    dir: &std::path::Path,
    w: u32,
    h: u32,
    secs: f64,
) -> (ProjectStore, cinvar::project::model::Project) {
    let src = dir.join("src.mp4");
    let status = std::process::Command::new("ffmpeg")
        .args([
            "-y",
            "-v",
            "error",
            "-f",
            "lavfi",
            "-i",
            "testsrc2=size=1280x720:rate=24:duration=2",
            "-c:v",
            "libx264",
            "-pix_fmt",
            "yuv420p",
        ])
        .arg(&src)
        .status()
        .unwrap();
    assert!(status.success());
    let (store, mut project) = ProjectStore::create(dir, "8k").unwrap();
    let asset = store.import_asset(&mut project, &src, "8k").unwrap();
    let tl = ops::create_timeline(&mut project, "Main", w, h, Rational::new(24, 1)).unwrap();
    let track = project
        .timeline_by_id(&tl)
        .unwrap()
        .tracks_of(TrackKind::Video)[0]
        .id
        .clone();
    ops::insert_clip(
        &mut project,
        Some(&tl),
        Some(&track),
        ClipRequest {
            asset_id: &asset.id,
            source_in: Some(Time::ZERO),
            source_out: Some(Time::from_secs_f64(secs)),
            at: Some(Time::ZERO),
            name: None,
        },
    )
    .unwrap();
    (store, project)
}

#[test]
fn eight_k_export_is_decodable_with_exact_frames() {
    if !ffmpeg_ok() {
        eprintln!("SKIP: ffmpeg/ffprobe missing");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let (store, project) = build_project(dir.path(), 7680, 4320, 0.5);
    let output = store.renders_dir().join("eight_k.mp4");
    let mut req = RenderRequest::for_output(&output);
    req.size_preset = Some("8k".into());
    req.hardware = Some(cinvar::project::model::HwMode::Off);
    let plan = render::plan_render(&project, &store, &req).unwrap();
    let pre = plan.preflight.as_ref().unwrap();
    eprintln!("{}", serde_json::to_string_pretty(pre).unwrap());
    assert!(pre.ok, "preflight refused 8K here: {:?}", pre.errors);
    assert_eq!((plan.width, plan.height), (7680, 4320));
    assert!(
        plan.args.iter().any(|a| a == "-filter_threads"),
        "large frames must bound filter parallelism"
    );
    assert_eq!(
        pre.source_class,
        capability::SourceClass::ScaleFilterUpscale,
        "720p source into an 8K canvas must be labelled as scale-filter upscaling"
    );
    let outcome = render::render(&project, &store, &req, &|_, _| {}, &|| false).expect("8K render");
    let report = verify::deep_verify(&outcome.output, Some((7680, 4320)), Some(12)).unwrap();
    eprintln!("{}", serde_json::to_string_pretty(&report).unwrap());
    assert!(report.passed, "{:?}", report.issues);
    assert_eq!(report.frames_counted, 12);
}

#[test]
fn oversized_and_odd_requests_are_refused_not_clamped() {
    if !ffmpeg_ok() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let (store, project) = build_project(dir.path(), 640, 360, 1.0);
    let mut req = RenderRequest::for_output(store.renders_dir().join("x.mp4"));
    req.width = Some(30000);
    req.height = Some(20000);
    let err = render::plan_render(&project, &store, &req).unwrap_err();
    assert_eq!(err.code, cinvar::error::ErrorCode::InvalidInput, "{err}");
    let mut bad = RenderRequest::for_output(store.renders_dir().join("y.mp4"));
    bad.size_preset = Some("9k".into());
    assert!(render::plan_render(&project, &store, &bad).is_err());
}

#[test]
fn ten_bit_output_is_requested_and_verified() {
    if !ffmpeg_ok() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let (store, project) = build_project(dir.path(), 640, 360, 1.0);
    let output = store.renders_dir().join("ten.mp4");
    let mut req = RenderRequest::for_output(&output);
    req.codec = Some("hevc".into());
    req.bit_depth = Some(10);
    req.hardware = Some(cinvar::project::model::HwMode::Off);
    render::render(&project, &store, &req, &|_, _| {}, &|| false).expect("10-bit render");
    let report = verify::deep_verify(&output, Some((640, 360)), None).unwrap();
    assert!(report.passed, "{:?}", report.issues);
    assert_eq!(report.pix_fmt, "yuv420p10le");
}

/// Hardware encoder choice must follow a *measured* result at the real size, never a table.
#[test]
fn hardware_choice_follows_a_measured_probe_at_8k() {
    if !ffmpeg_ok() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let (store, project) = build_project(dir.path(), 7680, 4320, 0.5);
    for (codec, hw) in [("h264", "h264_nvenc"), ("hevc", "hevc_nvenc")] {
        let mut req =
            RenderRequest::for_output(store.renders_dir().join(format!("hw_{codec}.mp4")));
        req.size_preset = Some("8k".into());
        req.codec = Some(codec.into());
        req.hardware = Some(cinvar::project::model::HwMode::Auto);
        let plan = render::plan_render(&project, &store, &req).unwrap();
        let listed = render::available_encoders().iter().any(|e| e == hw);
        let probe = capability::probe_encoder(hw, 7680, 4320, "yuv420p");
        eprintln!(
            "{codec}: planned encoder {} (hw listed={listed}, 8K probe supported={})",
            plan.encoder, probe.supported
        );
        if listed && probe.supported && cinvar_hw_working(hw) {
            assert_eq!(
                plan.encoder, hw,
                "a measured-good hardware encoder should be used"
            );
        } else {
            assert!(
                !plan.encoder.contains("nvenc"),
                "unsupported at 8K, yet {} was planned",
                plan.encoder
            );
        }
        if plan.encoder.contains("nvenc") {
            let outcome = render::render(&project, &store, &req, &|_, _| {}, &|| false)
                .expect("hardware 8K render");
            let report =
                verify::deep_verify(&outcome.output, Some((7680, 4320)), Some(12)).unwrap();
            eprintln!("{}", serde_json::to_string(&report).unwrap());
            assert!(report.passed, "{:?}", report.issues);
        }
    }
}

fn cinvar_hw_working(encoder: &str) -> bool {
    capability::probe_encoder(encoder, 256, 144, "yuv420p").supported
}

/// Durable job semantics through the real MCP tool registry: identical export is not run twice.
#[test]
fn duplicate_export_is_refused_and_job_survives_in_the_store() {
    if !ffmpeg_ok() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let (store, mut project) = build_project(dir.path(), 640, 360, 1.0);
    let registry = cinvar::tools::build_default();
    let config = cinvar::config::Config::default();
    let jobs = cinvar::jobs::Jobs::new();
    let mut history = cinvar::project::history::History::default();
    let mut ctx = cinvar::tools::ToolCtx {
        project: &mut project,
        store: &store,
        config: &config,
        jobs: &jobs,
        history: &mut history,
        dry_run: false,
    };
    let args = serde_json::json!({"output": store.renders_dir().join("dup.mp4").to_string_lossy()});
    let first = registry
        .call("cinvar.render.export", &mut ctx, args.clone())
        .expect("first export");
    let job = first["job_id"].as_str().unwrap().to_string();
    let second = registry
        .call("cinvar.render.export", &mut ctx, args)
        .expect("second export");
    assert!(
        second["duplicate_of"].is_string(),
        "identical export must be flagged as a duplicate: first={first} second={second}"
    );
    assert_eq!(second["duplicate_of"], first["durable_job_id"]);
    let done = jobs.wait(&job, std::time::Duration::from_secs(60)).unwrap();
    let listed0 = registry
        .call("cinvar.jobs.list", &mut ctx, serde_json::json!({}))
        .unwrap();
    assert_eq!(
        done.status,
        cinvar::jobs::JobStatus::Succeeded,
        "{:?} {}",
        done.error,
        listed0
    );
    let listed = registry
        .call("cinvar.jobs.list", &mut ctx, serde_json::json!({}))
        .unwrap();
    let rec = &listed["jobs"][0];
    assert_eq!(rec["state"], "succeeded");
    assert_eq!(rec["progress"], 1.0);
}
