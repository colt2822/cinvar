//! Director's Room: audio features are measured on the rendered file.

use std::path::Path;
use std::process::Command;

use cinvar::project::model::TrackKind;
use cinvar::project::ops;
use cinvar::project::store::ProjectStore;
use cinvar::render::{self, RenderRequest};
use cinvar::util::time::{parse_fps, Time};

fn have_ffmpeg() -> bool {
    Command::new("ffmpeg")
        .arg("-version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

fn ff(args: &[&str]) -> bool {
    Command::new("ffmpeg")
        .args(["-hide_banner", "-nostdin", "-loglevel", "error", "-y"])
        .args(args)
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// RMS level in dBFS of `[start, start+len)` after an optional ffmpeg audio filter.
fn rms_db(file: &Path, start: f64, len: f64, filter: &str) -> f64 {
    let chain = format!(
        "atrim=start={start}:end={},{filter}astats=metadata=1:reset=0",
        start + len
    );
    let out = Command::new("ffmpeg")
        .args([
            "-hide_banner",
            "-nostdin",
            "-i",
            file.to_str().unwrap(),
            "-vn",
            "-af",
            &chain,
            "-f",
            "null",
            "-",
        ])
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&out.stderr).to_string();
    // Last "RMS level dB" line is the overall figure.
    text.lines()
        .rfind(|l| l.contains("RMS level dB"))
        .and_then(|l| l.rsplit(':').next())
        .and_then(|v| v.trim().parse::<f64>().ok())
        .unwrap_or(-120.0)
}

struct Rig {
    _dir: tempfile::TempDir,
    root: std::path::PathBuf,
    store: ProjectStore,
    project: cinvar::project::model::Project,
}

/// A black 8 s video, 200 Hz "music" for 8 s, and 1 kHz "speech" for seconds 0-8 in a clip that
/// is silent until t=3 (speech appears at 3-6).
fn rig() -> Option<Rig> {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().to_path_buf();
    let video = root.join("v.mp4");
    let music = root.join("music.wav");
    let speech = root.join("speech.wav");
    let ok = ff(&[
        "-f",
        "lavfi",
        "-i",
        "color=c=black:s=320x180:r=30:d=8",
        "-pix_fmt",
        "yuv420p",
        video.to_str().unwrap(),
    ]) && ff(&[
        "-f",
        "lavfi",
        "-i",
        "sine=frequency=200:sample_rate=48000:duration=8",
        "-ac",
        "2",
        music.to_str().unwrap(),
    ]) && ff(&[
        "-f",
        "lavfi",
        "-i",
        "sine=frequency=1000:sample_rate=48000:duration=3",
        "-ac",
        "2",
        speech.to_str().unwrap(),
    ]);
    if !ok {
        return None;
    }
    let projects = root.join("projects");
    std::fs::create_dir_all(&projects).unwrap();
    let (store, mut project) = ProjectStore::create(&projects, "audio").unwrap();
    let v = store.import_asset(&mut project, &video, "t").unwrap();
    let m = store.import_asset(&mut project, &music, "t").unwrap();
    let sp = store.import_asset(&mut project, &speech, "t").unwrap();
    ops::create_timeline(&mut project, "Main", 320, 180, parse_fps("30").unwrap()).unwrap();
    ops::append_clip(&mut project, None, None, &v.id).unwrap();
    let music_track = ops::add_track(&mut project, None, TrackKind::Audio, "Music").unwrap();
    let speech_track = ops::add_track(&mut project, None, TrackKind::Audio, "Dialogue").unwrap();
    let music_clip = ops::append_clip(&mut project, None, Some(&music_track), &m.id).unwrap();
    ops::insert_clip(
        &mut project,
        None,
        Some(&speech_track),
        ops::ClipRequest {
            asset_id: &sp.id,
            source_in: None,
            source_out: None,
            at: Some(Time::from_secs_f64(3.0)),
            name: None,
        },
    )
    .unwrap();
    let _ = music_clip;
    Some(Rig {
        _dir: dir,
        root,
        store,
        project,
    })
}

fn render_it(r: &Rig, name: &str) -> std::path::PathBuf {
    let out = r.root.join(name);
    let mut req = RenderRequest::for_output(&out);
    req.width = Some(320);
    req.height = Some(180);
    render::render(&r.project, &r.store, &req, &|_, _| {}, &|| false).expect("render");
    out
}

fn music_clip_id(r: &Rig) -> String {
    r.project
        .timeline()
        .unwrap()
        .tracks
        .iter()
        .find(|t| t.name == "Music")
        .unwrap()
        .clips[0]
        .id
        .clone()
}

#[test]
fn ducking_lowers_music_only_while_speech_plays() {
    if !have_ffmpeg() {
        return;
    }
    let Some(mut r) = rig() else { return };
    let plain = render_it(&r, "plain.mp4");
    let id = music_clip_id(&r);
    ops::set_clip_audio(
        &mut r.project,
        &id,
        None,
        None,
        None,
        None,
        None,
        Some(true),
    )
    .unwrap();
    let ducked = render_it(&r, "ducked.mp4");

    let low = "lowpass=f=400,";
    // Music band while speech is playing (4-5.5 s) versus after speech ends (6.8-7.8 s).
    let during_plain = rms_db(&plain, 4.0, 1.5, low);
    let during_duck = rms_db(&ducked, 4.0, 1.5, low);
    let after_duck = rms_db(&ducked, 6.8, 1.0, low);
    let before_duck = rms_db(&ducked, 0.5, 1.5, low);
    eprintln!("music band: plain-during {during_plain:.1}  ducked-during {during_duck:.1}  ducked-after {after_duck:.1} ducked-before {before_duck:.1}");
    assert!(
        during_plain - during_duck >= 6.0,
        "music is at least 6 dB lower under speech: {during_plain} vs {during_duck}"
    );
    assert!(
        (before_duck - after_duck).abs() < 3.0,
        "music recovers to its normal level after speech: {before_duck} vs {after_duck}"
    );
    let speech_plain = rms_db(&plain, 4.0, 1.5, "highpass=f=600,");
    let speech_duck = rms_db(&ducked, 4.0, 1.5, "highpass=f=600,");
    assert!(
        (speech_plain - speech_duck).abs() < 2.0,
        "speech itself is not ducked: {speech_plain} vs {speech_duck}"
    );
}

#[test]
fn ducking_with_nothing_to_duck_under_is_reported() {
    if !have_ffmpeg() {
        return;
    }
    let Some(mut r) = rig() else { return };
    // Flag every audio clip: there is then no speech bus.
    let ids: Vec<String> = r
        .project
        .timeline()
        .unwrap()
        .tracks
        .iter()
        .flat_map(|t| t.clips.iter().map(|c| c.id.clone()))
        .collect();
    for id in ids {
        ops::set_clip_audio(
            &mut r.project,
            &id,
            None,
            None,
            None,
            None,
            None,
            Some(true),
        )
        .unwrap();
    }
    let out = r.root.join("none.mp4");
    let mut req = RenderRequest::for_output(&out);
    req.width = Some(320);
    req.height = Some(180);
    let outcome = render::render(&r.project, &r.store, &req, &|_, _| {}, &|| false).unwrap();
    assert!(
        outcome.warnings.iter().any(|w| w.contains("duck")),
        "{:?}",
        outcome.warnings
    );
}

fn integrated_lufs(file: &Path) -> f64 {
    let out = Command::new("ffmpeg")
        .args([
            "-hide_banner",
            "-nostdin",
            "-i",
            file.to_str().unwrap(),
            "-vn",
            "-af",
            "ebur128=peak=true",
            "-f",
            "null",
            "-",
        ])
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&out.stderr).to_string();
    let summary = text.rsplit("Summary:").next().unwrap_or("");
    summary
        .lines()
        .find(|l| l.trim_start().starts_with("I:"))
        .and_then(|l| l.split_whitespace().nth(1))
        .and_then(|v| v.parse::<f64>().ok())
        .expect("integrated loudness is reported")
}

#[test]
fn two_pass_loudness_lands_on_the_target() {
    if !have_ffmpeg() {
        return;
    }
    let Some(r) = rig() else { return };
    let out = render_it(&r, "loud.mp4");
    let lufs = integrated_lufs(&out);
    eprintln!("integrated loudness {lufs:.2} LUFS (target -14)");
    assert!((lufs + 14.0).abs() <= 0.7, "within 0.7 LU of -14: {lufs}");
}
