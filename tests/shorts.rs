//! Shorts generation end to end: a real source is synthesised with ffmpeg, generate() runs the
//! whole pipeline, and the delivered files are measured with ffprobe. No ASR engine is needed
//! for the default tests (the selector falls back to measured audio energy and no captions are
//! burned in); the ignored test exercises the speech path on a real speech recording.

use std::path::{Path, PathBuf};
use std::process::Command;

use cinvar::shorts::{self, ShortsRequest};

fn ffmpeg(args: &[String]) {
    let out = Command::new("ffmpeg")
        .args(args)
        .output()
        .expect("ffmpeg must be installed");
    assert!(
        out.status.success(),
        "ffmpeg failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// (width, height, duration) measured by ffprobe.
fn probe(path: &Path) -> (u32, u32, f64) {
    let out = Command::new("ffprobe")
        .args([
            "-v",
            "error",
            "-select_streams",
            "v:0",
            "-show_entries",
            "stream=width,height",
            "-show_entries",
            "format=duration",
            "-of",
            "csv=p=0",
        ])
        .arg(path)
        .output()
        .expect("ffprobe must be installed");
    assert!(out.status.success(), "ffprobe failed on {}", path.display());
    let text = String::from_utf8_lossy(&out.stdout);
    let numbers: Vec<f64> = text
        .split(['\n', ','])
        .filter_map(|token| token.trim().parse::<f64>().ok())
        .collect();
    assert!(numbers.len() >= 3, "unreadable probe output: {text:?}");
    (numbers[0] as u32, numbers[1] as u32, numbers[2])
}

/// A 30 s 1280x720 source whose audio is loud only in four speech-like bands.
fn source(dir: &Path, seconds: f64) -> PathBuf {
    let path = dir.join("source.mp4");
    let audio = format!(
        "aevalsrc='0.5*sin(2*PI*440*t)*(between(t,2,4)+between(t,8,11)+between(t,15,20)+between(t,24,27))':s=48000:d={seconds}"
    );
    ffmpeg(&[
        "-y".into(),
        "-v".into(),
        "error".into(),
        "-f".into(),
        "lavfi".into(),
        "-i".into(),
        format!("testsrc2=s=1280x720:r=30:d={seconds}"),
        "-f".into(),
        "lavfi".into(),
        "-i".into(),
        audio,
        "-c:v".into(),
        "libx264".into(),
        "-preset".into(),
        "veryfast".into(),
        "-crf".into(),
        "32".into(),
        "-pix_fmt".into(),
        "yuv420p".into(),
        "-c:a".into(),
        "aac".into(),
        "-b:a".into(),
        "128k".into(),
        "-shortest".into(),
        path.to_string_lossy().to_string(),
    ]);
    path
}

fn request(input: &Path, output: &Path, count: u32, min: f64, max: f64) -> ShortsRequest {
    ShortsRequest {
        input: input.to_path_buf(),
        count,
        min_duration: min,
        max_duration: max,
        output_dir: Some(output.to_path_buf()),
        vocab: Vec::new(),
    }
}

#[test]
fn generate_delivers_finished_vertical_shorts_that_pass_qa() {
    let dir = tempfile::tempdir().unwrap();
    let input = source(dir.path(), 30.0);
    let output = dir.path().join("shorts");
    let report = shorts::generate(&request(&input, &output, 2, 5.0, 9.0)).expect("generate");

    assert_eq!(report.shorts.len(), 2, "asked for two Shorts: {report:?}");
    assert_eq!(report.source, input);
    assert!(report.elapsed_ms > 0);

    for (position, short) in report.shorts.iter().enumerate() {
        let index = position + 1;
        assert_eq!(index, short.index);
        assert_eq!(short.path, output.join(format!("short_{index:02}.mp4")));
        assert!(short.path.is_file(), "{} was not written", short.path.display());
        // Measured, not copied from the report.
        let (width, height, duration) = probe(&short.path);
        assert_eq!((width, height), (1080, 1920), "{} is not vertical 1080x1920", short.path.display());
        assert_eq!((short.width, short.height), (1080, 1920), "the report must carry the measured size");
        assert!(
            duration >= 5.0 - 0.6 && duration <= 9.0 + 0.6,
            "short {index} measured {duration:.3}s, outside the requested 5-9s"
        );
        assert!(
            short.end - short.start >= 5.0 - 0.01 && short.end - short.start <= 9.0 + 0.01,
            "short {index} covers {:.3}-{:.3}s",
            short.start,
            short.end
        );
        assert!(short.start >= 0.0 && short.end <= 30.5, "inside the source: {short:?}");
        assert!(short.qa_passed, "short {index} failed its own QA");
        assert!(short.score > 0.0, "short {index} carries no score");
    }
    assert!(
        report.shorts[0].end <= report.shorts[1].start + 1e-6,
        "Shorts must not share source: {:?}",
        report.shorts
    );
    if std::env::var("CINVAR_SHORTS_KEEP_WORK").as_deref() != Ok("1") {
        assert!(
            !output.join(".shorts-work").exists(),
            "the scratch directory must be cleaned up"
        );
    }
}

#[test]
fn the_same_source_and_options_select_the_same_ranges() {
    let dir = tempfile::tempdir().unwrap();
    let input = source(dir.path(), 24.0);
    let first = dir.path().join("a");
    let second = dir.path().join("b");
    let a = shorts::generate(&request(&input, &first, 1, 5.0, 8.0)).expect("first run");
    let b = shorts::generate(&request(&input, &second, 1, 5.0, 8.0)).expect("second run");
    assert_eq!(a.shorts.len(), 1);
    assert_eq!(b.shorts.len(), 1);
    assert_eq!((a.shorts[0].start, a.shorts[0].end), (b.shorts[0].start, b.shorts[0].end));
    assert_eq!(a.shorts[0].score, b.shorts[0].score);
    let (_, _, da) = probe(&a.shorts[0].path);
    let (_, _, db) = probe(&b.shorts[0].path);
    assert!((da - db).abs() < 0.2, "same ranges, same length: {da:.3} vs {db:.3}");
}

#[test]
fn a_source_shorter_than_the_minimum_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let input = source(dir.path(), 8.0);
    let output = dir.path().join("shorts");
    let err = shorts::generate(&request(&input, &output, 1, 20.0, 30.0)).unwrap_err();
    assert_eq!(err.code, cinvar::error::ErrorCode::InvalidInput);
    assert!(err.message.contains("shorter"), "{}", err.message);
}

/// Real speech path: needs a speech recording and a configured transcription engine.
///   CINVAR_FASTER_WHISPER_PYTHON=/tmp/fw-venv/bin/python CINVAR_FASTER_WHISPER_MODEL=tiny \
///   CINVAR_FASTER_WHISPER_ALLOW_DOWNLOAD=1 CINVAR_E2E_SOURCE=/tmp/fx/speech_source.mp4 \
///   cargo test --release --test shorts -- --ignored e2e
#[test]
#[ignore = "needs CINVAR_E2E_SOURCE and a configured transcription engine"]
fn e2e_a_speech_recording_becomes_captioned_shorts() {
    let input = PathBuf::from(
        std::env::var("CINVAR_E2E_SOURCE").expect("set CINVAR_E2E_SOURCE to a speech video"),
    );
    let output = match std::env::var("CINVAR_E2E_OUT") {
        Ok(dir) => PathBuf::from(dir),
        Err(_) => std::env::temp_dir()
            .join(format!("cinvar-shorts-e2e-{}", std::process::id()))
            .join("shorts"),
    };
    std::env::set_var("CINVAR_SHORTS_KEEP_WORK", "1");
    let (report, diagnostics) =
        shorts::generate_with_diagnostics(&request(&input, &output, 2, 6.0, 14.0))
            .expect("generate");
    println!("DIAGNOSTICS {}", serde_json::to_string(&diagnostics).unwrap());
    assert_eq!(report.shorts.len(), 2, "{report:?}");
    assert_eq!(diagnostics.reframes.len(), 2);
    assert_eq!(diagnostics.qa_detail.len(), 2);
    for short in &report.shorts {
        let (width, height, duration) = probe(&short.path);
        assert_eq!((width, height), (1080, 1920));
        assert!(duration >= 6.0 - 0.6 && duration <= 14.0 + 0.6, "{duration:.3}s");
        assert!(short.qa_passed, "{} failed QA", short.path.display());
        // The karaoke ASS the burn-in used is still on disk for inspection.
        let stem = short.path.file_stem().unwrap().to_string_lossy().to_string();
        let work = output.join(".shorts-work");
        let ass = std::fs::read_dir(&work)
            .expect("the scratch directory is kept when CINVAR_SHORTS_KEEP_WORK=1")
            .filter_map(|entry| entry.ok().map(|e| e.path()))
            .find(|path| {
                let name = path.file_name().unwrap_or_default().to_string_lossy().to_string();
                name.starts_with(&stem) && name.ends_with(".captions.ass")
            })
            .unwrap_or_else(|| panic!("no karaoke track for {stem} in {}", work.display()));
        let script = std::fs::read_to_string(&ass).unwrap();
        assert!(script.contains("Dialogue:"), "the karaoke track is empty");
        assert!(script.contains("\\k"), "no word-level karaoke timing: {script}");
    }
    // The engine must also accept the custom-vocabulary channels on a real run: the helper
    // falls back when a build does not support a kwarg, so a silent drop would go unnoticed.
    let engine = cinvar::captions::detect_engine_named("auto").expect("a transcription engine");
    let wav = output.join(".shorts-work").join("speech.wav");
    let srt = cinvar::captions::run_engine_with_vocab(
        &engine,
        &wav,
        &output.join(".shorts-work").join("vocab-check"),
        None,
        &["ZBrush".to_string(), "Octane Render".to_string()],
    )
    .expect("a vocab run against the real engine");
    assert!(srt.contains("-->"), "the vocab run returned no subtitles: {srt}");
    println!("VOCAB_SRT_ENTRIES {}", srt.matches("-->").count());
    println!("SHORTS {}", serde_json::to_string(&report).unwrap());
}
