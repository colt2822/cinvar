//! Director's Room: caption workflows.
//! NOTE: no real speech-to-text engine exists on the build machine. The engine adapter is
//! verified against a stub command that emits SRT; a real whisper run is NOT claimed.

use std::path::Path;
use std::process::Command;

use cinvar::captions;
use cinvar::project::model::{CaptionStyle, Cue, TextPosition};
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

#[test]
fn custom_engine_output_is_parsed_into_cues() {
    let dir = tempfile::tempdir().unwrap();
    let script = dir.path().join("stub engine.sh");
    // $1 = audio, $2 = srt output path
    std::fs::write(&script, "printf '1\\n00:00:00,500 --> 00:00:02,000\\nHello from the stub\\n\\n2\\n00:00:02,500 --> 00:00:04,000\\nSecond cue\\n' > \"$2\"\n").unwrap();
    let template = format!(
        "sh {} {{audio}} {{srt}}",
        script.to_string_lossy().replace(' ', "\\ ")
    );
    // split_whitespace cannot carry a space in the path, so use a space-free copy for the template.
    let plain = dir.path().join("stub.sh");
    std::fs::copy(&script, &plain).unwrap();
    let template = template.replace(
        &script.to_string_lossy().replace(' ', "\\ "),
        &plain.to_string_lossy(),
    );
    let wav = dir.path().join("in.wav");
    std::fs::write(&wav, b"RIFF").unwrap();
    let srt = captions::run_engine(
        &captions::Engine::Custom(template),
        &wav,
        &dir.path().join("work"),
        None,
    )
    .unwrap();
    let (cues, skipped) = captions::parse_subtitles(&srt);
    assert_eq!(skipped, 0);
    assert_eq!(cues.len(), 2);
    assert_eq!(cues[0].text, "Hello from the stub");
}

#[test]
fn a_failing_engine_reports_instead_of_inventing_captions() {
    let dir = tempfile::tempdir().unwrap();
    let wav = dir.path().join("in.wav");
    std::fs::write(&wav, b"RIFF").unwrap();
    let err = captions::run_engine(
        &captions::Engine::Custom("sh -c exit\\ 3 {audio} {srt}".into()),
        &wav,
        &dir.path().join("w"),
        None,
    );
    assert!(err.is_err());
}

fn gray(video: &Path, t: f64, w: usize, h: usize) -> Vec<u8> {
    Command::new("ffmpeg")
        .args([
            "-v",
            "error",
            "-ss",
            &format!("{t}"),
            "-i",
            video.to_str().unwrap(),
            "-frames:v",
            "1",
            "-f",
            "rawvideo",
            "-pix_fmt",
            "gray",
            "-",
        ])
        .output()
        .unwrap()
        .stdout
        .into_iter()
        .take(w * h)
        .collect()
}

#[test]
fn burned_captions_wrap_inside_the_frame_and_use_a_background_box() {
    if !have_ffmpeg() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let src = root.join("white.mp4");
    // A bright background: a box of darker colour behind the text is measurable.
    assert!(Command::new("ffmpeg")
        .args([
            "-v",
            "error",
            "-y",
            "-f",
            "lavfi",
            "-i",
            "color=c=white:s=360x640:r=30:d=3",
            "-pix_fmt",
            "yuv420p",
            src.to_str().unwrap()
        ])
        .status()
        .unwrap()
        .success());
    let projects = root.join("p");
    std::fs::create_dir_all(&projects).unwrap();
    let (store, mut project) = ProjectStore::create(&projects, "cap").unwrap();
    let asset = store.import_asset(&mut project, &src, "t").unwrap();
    ops::create_timeline(&mut project, "Vertical", 360, 640, parse_fps("30").unwrap()).unwrap();
    ops::append_clip(&mut project, None, None, &asset.id).unwrap();
    let style = CaptionStyle {
        size: 36,
        color: "white".into(),
        outline_width: 0,
        background: Some("black@0.85".into()),
        position: TextPosition::Center,
        ..CaptionStyle::default()
    };
    let cues = vec![Cue {
        start: Time::from_secs_f64(0.2),
        end: Time::from_secs_f64(2.8),
        text: "This caption is far too long for one line on a vertical phone screen".into(),
    }];
    ops::set_captions(&mut project, cues, "en", Some(style), "test").unwrap();
    let out = root.join("cap.mp4");
    let mut req = RenderRequest::for_output(&out);
    req.width = Some(360);
    req.height = Some(640);
    let outcome = render::render(&project, &store, &req, &|_, _| {}, &|| false).expect("render");
    // Ignore the expected notice from hosts that list a hardware encoder they cannot use (CI runners).
    let unexpected: Vec<_> = outcome
        .warnings
        .iter()
        .filter(|w| !w.contains("probe encode failed"))
        .collect();
    assert!(unexpected.is_empty(), "{:?}", unexpected);
    let frame = gray(&out, 1.0, 360, 640);
    // Rows containing dark pixels = the caption box.
    let dark_rows: Vec<usize> = (0..640)
        .filter(|y| frame[y * 360..(y + 1) * 360].iter().any(|v| *v < 60))
        .collect();
    assert!(!dark_rows.is_empty(), "caption box is visible");
    let height = dark_rows.last().unwrap() - dark_rows.first().unwrap();
    assert!(
        height > 70,
        "wrapped over several lines, box height {height}"
    );
    let dark_cols: Vec<usize> = (0..360)
        .filter(|x| (0..640).any(|y| frame[y * 360 + x] < 60))
        .collect();
    assert!(
        *dark_cols.first().unwrap() >= 8 && *dark_cols.last().unwrap() <= 352,
        "inside the frame: {:?}..{:?}",
        dark_cols.first(),
        dark_cols.last()
    );
}
