//! Real-Blender integration (NOT mocked). Ignored by default; run explicitly:
//!   CINVAR_BLENDER=/path/to/blender cargo test --test blender_real -- --ignored --nocapture
//! If Blender is absent the test FAILS (it never silently passes).

use cinvar::blender::{self, AnimationSpec};
use cinvar::util::time::Time;

/// The native-8K test holds several GB; run it alone so parallel Blender tests are not starved.
static HEAVY: std::sync::RwLock<()> = std::sync::RwLock::new(());

#[test]
#[ignore = "requires a real Blender install (set CINVAR_BLENDER)"]
fn real_blender_renders_a_clip() {
    let _shared = HEAVY.read().unwrap();
    let blender_path = blender::available().expect("Blender not found: set CINVAR_BLENDER");
    eprintln!("blender: {}", blender_path.display());
    let dir = tempfile::tempdir().unwrap();
    let spec = AnimationSpec {
        template: "logo_reveal".into(),
        text: "CINVAR".into(),
        duration: Time::from_secs_f64(0.5),
        width: 320,
        height: 180,
        fps: 24,
        color: "#4f8cff".into(),
    };
    let clip = blender::render_animation(&spec, dir.path()).expect("real render");
    assert!(clip.metadata().unwrap().len() > 0);
    eprintln!("clip: {}", clip.display());
}

fn have(tool: &str) -> bool {
    std::process::Command::new(tool)
        .arg("-version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// project -> Blender (via the MCP tool registry) -> clip on the timeline -> FFmpeg export -> QA.
#[test]
#[ignore = "requires a real Blender install (set CINVAR_BLENDER)"]
fn real_blender_to_timeline_to_final_export() {
    let _shared = HEAVY.read().unwrap();
    use cinvar::project::model::TrackKind;
    use cinvar::project::ops::{self, ClipRequest};
    use cinvar::project::store::ProjectStore;
    use cinvar::util::time::Rational;

    blender::available().expect("Blender not found: set CINVAR_BLENDER");
    assert!(have("ffmpeg"), "ffmpeg required");
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("src.mp4");
    let ok = std::process::Command::new("ffmpeg")
        .args([
            "-y",
            "-v",
            "error",
            "-f",
            "lavfi",
            "-i",
            "testsrc2=size=640x360:rate=24:duration=3",
            "-f",
            "lavfi",
            "-i",
            "sine=frequency=330:duration=3",
            "-c:v",
            "libx264",
            "-pix_fmt",
            "yuv420p",
            "-c:a",
            "aac",
            "-shortest",
        ])
        .arg(&src)
        .status()
        .unwrap()
        .success();
    assert!(ok);

    let (store, mut project) = ProjectStore::create(dir.path(), "blender chain").unwrap();
    let asset = store.import_asset(&mut project, &src, "chain").unwrap();
    let tl = ops::create_timeline(&mut project, "Main", 640, 360, Rational::new(24, 1)).unwrap();
    let vtrack = project
        .timeline_by_id(&tl)
        .unwrap()
        .tracks_of(TrackKind::Video)[0]
        .id
        .clone();
    ops::insert_clip(
        &mut project,
        Some(&tl),
        Some(&vtrack),
        ClipRequest {
            asset_id: &asset.id,
            source_in: None,
            source_out: None,
            at: Some(Time::ZERO),
            name: None,
        },
    )
    .unwrap();

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
    let out = registry
        .call(
            "cinvar.blender.animation",
            &mut ctx,
            serde_json::json!({"template": "title_3d", "text": "CINVAR", "duration": 1.0, "width": 320, "height": 180, "fps": 24, "place_at": 1.0}),
        )
        .expect("blender tool");
    eprintln!("tool result: {out}");
    assert!(
        out["placed"]["clip_id"].is_string(),
        "clip must be on the timeline"
    );
    assert_eq!(out["frames"], 24);

    let output = store.renders_dir().join("final.mp4");
    let outcome = cinvar::render::render(
        &project,
        &store,
        &cinvar::render::RenderRequest::for_output(&output),
        &|_, _| {},
        &|| false,
    )
    .expect("final ffmpeg export");
    assert!(outcome.output.exists());
    let report = cinvar::qa::validate_render(&project, &store, &output).unwrap();
    eprintln!("{}", serde_json::to_string(&report).unwrap());
    assert!(report.passed, "QA failed on final export");
}

/// Cancel mid-render, then re-run the identical request: it must resume from the kept frames.
#[test]
#[ignore = "requires a real Blender install (set CINVAR_BLENDER)"]
fn real_blender_interrupt_and_resume() {
    let _shared = HEAVY.read().unwrap();
    use std::cell::Cell;
    blender::available().expect("Blender not found: set CINVAR_BLENDER");
    let dir = tempfile::tempdir().unwrap();
    let spec = AnimationSpec {
        template: "title_3d".into(),
        text: "RESUME".into(),
        duration: Time::from_secs_f64(3.0),
        width: 480,
        height: 270,
        fps: 24,
        color: "#ff8c4f".into(),
    };
    let seen = Cell::new(0usize);
    let progress = |done: usize, _t: usize| seen.set(done);
    let cancelled = || seen.get() >= 8;
    let ctl = blender::RenderControl {
        progress: &progress,
        cancelled: &cancelled,
    };
    let err = blender::render_animation_job(&spec, dir.path(), &ctl).unwrap_err();
    assert_eq!(err.code, cinvar::error::ErrorCode::Cancelled, "{err}");
    let kept = std::fs::read_dir(blender::job_dir(dir.path(), &spec).join("frames"))
        .unwrap()
        .count();
    assert!(
        kept >= 8,
        "cancelled render must keep finished frames, kept {kept}"
    );

    let done = blender::render_animation_job(&spec, dir.path(), &blender::RenderControl::none())
        .expect("resume");
    eprintln!("resumed {} of {} frames", done.resumed_frames, done.frames);
    assert!(
        done.resumed_frames >= 8,
        "second run must reuse the kept frames"
    );
    assert_eq!(done.frames, 72);
}

/// Native 8K: Blender itself renders 7680x4320 frames (not a scaled-up lower resolution).
#[test]
#[ignore = "requires a real Blender install (set CINVAR_BLENDER); needs several GB of RAM"]
fn real_blender_renders_native_8k() {
    let _exclusive = HEAVY.write().unwrap();
    blender::available().expect("Blender not found: set CINVAR_BLENDER");
    let dir = tempfile::tempdir().unwrap();
    let spec = AnimationSpec {
        template: "title_3d".into(),
        text: "8K".into(),
        duration: Time::from_secs_f64(0.125),
        width: 7680,
        height: 4320,
        fps: 24,
        color: "#4f8cff".into(),
    };
    let started = std::time::Instant::now();
    let clip = blender::render_animation(&spec, dir.path()).expect("native 8K render");
    eprintln!(
        "8K Blender render took {:.1}s",
        started.elapsed().as_secs_f64()
    );
    let probe = std::process::Command::new("ffprobe")
        .args([
            "-v",
            "error",
            "-select_streams",
            "v:0",
            "-show_entries",
            "stream=width,height,nb_frames",
            "-of",
            "csv=p=0",
        ])
        .arg(&clip)
        .output()
        .unwrap();
    let dims = String::from_utf8_lossy(&probe.stdout).trim().to_string();
    eprintln!("probe: {dims}");
    assert!(
        dims.starts_with("7680,4320"),
        "clip is not native 8K: {dims}"
    );
    let decode = std::process::Command::new("ffmpeg")
        .args(["-v", "error", "-i"])
        .arg(&clip)
        .args(["-f", "null", "-"])
        .output()
        .unwrap();
    assert!(
        decode.status.success() && decode.stderr.is_empty(),
        "{}",
        String::from_utf8_lossy(&decode.stderr)
    );
}
