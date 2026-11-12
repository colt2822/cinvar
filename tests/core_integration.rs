//! Integration coverage for the behaviours the lead wired across modules:
//! per-clip audio normalisation in the render graph, and the blocking render-status
//! contract the agent harness relies on.

use cinvar::jobs::{JobStatus, Jobs};
use cinvar::project::model::*;
use cinvar::project::{ops, store::ProjectStore};
use cinvar::util::time::{parse_fps, Time};

fn have_ffmpeg() -> bool {
    cinvar::util::proc::which("ffmpeg").is_some() && cinvar::util::proc::which("ffprobe").is_some()
}

fn project_with_clip(dir: &std::path::Path) -> (ProjectStore, Project, String, String) {
    let (store, mut project) = ProjectStore::create(dir, "audio-normalise").unwrap();
    let ffmpeg = cinvar::util::proc::which("ffmpeg").unwrap();
    let source = dir.join("source.mp4");
    let args: Vec<String> = [
        "-hide_banner",
        "-loglevel",
        "error",
        "-y",
        "-f",
        "lavfi",
        "-i",
        "color=c=0x203040:s=320x180:d=2:r=30",
        "-f",
        "lavfi",
        "-i",
        "sine=frequency=440:duration=2",
        "-c:v",
        "libx264",
        "-preset",
        "ultrafast",
        "-pix_fmt",
        "yuv420p",
        "-c:a",
        "aac",
        "-shortest",
        source.to_str().unwrap(),
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    cinvar::util::proc::run(
        &ffmpeg,
        &args,
        &cinvar::util::proc::ProcOpts::default().with_timeout(120),
    )
    .unwrap();
    let asset = store.import_asset(&mut project, &source, "test").unwrap();
    let timeline_id =
        ops::create_timeline(&mut project, "Main", 320, 180, parse_fps("30").unwrap()).unwrap();
    let clip_id = ops::append_clip(&mut project, Some(&timeline_id), None, &asset.id).unwrap();
    ops::set_clip_audio(
        &mut project,
        &clip_id,
        None,
        None,
        None,
        Some(true),
        None,
        None,
    )
    .unwrap();
    (store, project, timeline_id, clip_id)
}

#[test]
fn per_clip_normalisation_reaches_the_audio_chain() {
    let dir = tempfile::tempdir().unwrap();
    let (store, project, _timeline, _clip) = project_with_clip(dir.path());
    let request = cinvar::render::RenderRequest::for_output(dir.path().join("out.mp4"));
    let plan = cinvar::render::plan_render(&project, &store, &request).unwrap();
    assert!(
        plan.filtergraph.contains("loudnorm=I=-16"),
        "per-clip normalisation must appear in the filtergraph: {}",
        plan.filtergraph
    );
}

#[test]
fn normalised_clip_renders_and_validates() {
    if !have_ffmpeg() {
        eprintln!("skipping: ffmpeg not installed");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let (store, project, _timeline, _clip) = project_with_clip(dir.path());
    let output = dir.path().join("out.mp4");
    let request = cinvar::render::RenderRequest::for_output(&output);
    let outcome =
        cinvar::render::render(&project, &store, &request, &|_, _| {}, &|| false).unwrap();
    assert!(
        outcome.bytes > 1024,
        "render produced {} bytes",
        outcome.bytes
    );
    let info = cinvar::media::probe(&output).unwrap();
    assert!(info.has_audio);
    let qa = cinvar::qa::validate_render(&project, &store, &output).unwrap();
    assert!(qa.passed, "qa must pass: {:?}", qa.checks);
}

#[test]
fn render_status_can_block_until_the_job_settles() {
    let jobs = Jobs::new();
    let id = jobs.spawn("test", |job| {
        std::thread::sleep(std::time::Duration::from_millis(120));
        job.progress(1.0, "done");
        Ok(serde_json::json!({"ok": true}))
    });
    let snapshot = jobs.wait(&id, std::time::Duration::from_secs(10)).unwrap();
    assert_eq!(snapshot.status, JobStatus::Succeeded);
    assert!(snapshot.output.is_some());
    assert_eq!(Time::from_secs_f64(1.0).as_secs_f64(), 1.0);
}

#[test]
fn ripple_trim_closes_the_gap_it_creates() {
    let dir = tempfile::tempdir().unwrap();
    let (store, mut project, timeline_id, _clip) = project_with_clip(dir.path());
    // a second clip after the first one
    let asset_id = project.assets[0].id.clone();
    let second = ops::append_clip(&mut project, Some(&timeline_id), None, &asset_id).unwrap();
    let before = project.clip(&second).unwrap().timeline_start;
    let first = project.timeline_by_id(&timeline_id).unwrap().tracks[0].clips[0]
        .id
        .clone();
    let shift =
        ops::trim_clip_ripple(&mut project, &first, None, Some(Time::from_secs_f64(1.0))).unwrap();
    assert!(
        shift.is_negative(),
        "shortening must report a negative shift: {shift:?}"
    );
    let after = project.clip(&second).unwrap().timeline_start;
    assert!(
        after < before,
        "later clip must move earlier: {before:?} -> {after:?}"
    );
    assert_eq!(
        project
            .timeline_by_id(&timeline_id)
            .unwrap()
            .duration()
            .as_secs_f64(),
        3.0
    );
    let _ = store;
}

#[test]
fn keyframes_render_without_a_fallback_warning() {
    if !have_ffmpeg() {
        eprintln!("skipping: ffmpeg not installed");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let (store, mut project, _timeline, clip) = project_with_clip(dir.path());
    // Keyframes are rendered for real now, so they produce no warning.
    ops::add_keyframe(
        &mut project,
        &clip,
        "scale",
        Time::from_secs_f64(0.0),
        1.0,
        "linear",
    )
    .unwrap();
    ops::add_keyframe(
        &mut project,
        &clip,
        "scale",
        Time::from_secs_f64(1.0),
        1.3,
        "linear",
    )
    .unwrap();
    // Unknown effects are refused when added, not dropped at render time.
    assert!(ops::add_effect(&mut project, &clip, "hologram", serde_json::json!({})).is_err());
    let output = dir.path().join("warn.mp4");
    let request = cinvar::render::RenderRequest::for_output(&output);
    let outcome =
        cinvar::render::render(&project, &store, &request, &|_, _| {}, &|| false).unwrap();
    assert!(
        !outcome.warnings.iter().any(|w| w.contains("keyframe")),
        "keyframes animate, so no keyframe warning: {:?}",
        outcome.warnings
    );
}
