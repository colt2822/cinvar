//! NOTE: faster-whisper 1.2.1 needs PyAV < 17 (av 19 breaks `av.open(metadata_errors=...)`).
//! Real faster-whisper transcription (CPU, tiny.en, local model only). Ignored by default:
//!   CINVAR_FASTER_WHISPER_PYTHON=~/cinvar-deps/fw-venv/bin/python \
//!   CINVAR_FASTER_WHISPER_MODEL=tiny.en CINVAR_FASTER_WHISPER_CACHE=~/cinvar-deps/fw-models \
//!   CINVAR_TEST_SPEECH=/path/to/speech.wav cargo test --test transcribe_real -- --ignored

use cinvar::project::model::TrackKind;
use cinvar::project::ops::{self, ClipRequest};
use cinvar::project::store::ProjectStore;
use cinvar::util::time::{Rational, Time};

#[test]
#[ignore = "requires faster-whisper, a local model and CINVAR_TEST_SPEECH"]
fn real_transcription_places_editable_captions_and_exports_srt() {
    let speech =
        std::env::var("CINVAR_TEST_SPEECH").expect("set CINVAR_TEST_SPEECH to a short speech wav");
    let d = tempfile::tempdir().unwrap();
    let src = d.path().join("speech.mp4");
    assert!(std::process::Command::new("ffmpeg")
        .args([
            "-y",
            "-v",
            "error",
            "-f",
            "lavfi",
            "-i",
            "color=c=black:s=320x180:r=24",
            "-i",
            &speech,
            "-t",
            "8",
            "-c:v",
            "libx264",
            "-pix_fmt",
            "yuv420p",
            "-c:a",
            "aac",
            "-shortest"
        ])
        .arg(&src)
        .status()
        .unwrap()
        .success());
    let (store, mut project) = ProjectStore::create(d.path(), "tr").unwrap();
    let asset = store.import_asset(&mut project, &src, "t").unwrap();
    let tl = ops::create_timeline(&mut project, "Main", 320, 180, Rational::new(24, 1)).unwrap();
    let track = project
        .timeline_by_id(&tl)
        .unwrap()
        .tracks_of(TrackKind::Video)[0]
        .id
        .clone();
    let clip = ops::insert_clip(
        &mut project,
        Some(&tl),
        Some(&track),
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
            "cinvar.captions.transcribe",
            &mut ctx,
            serde_json::json!({"clip_id": clip, "language": "en"}),
        )
        .expect("transcribe");
    eprintln!("{out}");
    assert!(out["cue_count"].as_u64().unwrap_or(0) >= 1, "{out}");
    let exp = registry
        .call(
            "cinvar.captions.export",
            &mut ctx,
            serde_json::json!({"output": "exports/captions.srt"}),
        )
        .expect("export");
    let text = std::fs::read_to_string(exp["path"].as_str().unwrap()).unwrap();
    let (cues, skipped) = cinvar::captions::parse_subtitles(&text);
    assert_eq!(skipped, 0);
    assert!(!cues.is_empty());
    assert!(cues
        .iter()
        .all(|c| c.end > c.start && !c.text.trim().is_empty()));
    eprintln!("first cue: {:?}", cues[0].text);
}

#[test]
fn caption_export_refuses_paths_outside_the_project_and_bad_extensions() {
    let d = tempfile::tempdir().unwrap();
    let (store, mut project) = ProjectStore::create(d.path(), "ce").unwrap();
    ops::set_captions(
        &mut project,
        vec![cinvar::project::model::Cue {
            start: Time::from_secs_f64(0.0),
            end: Time::from_secs_f64(1.0),
            text: "hi".into(),
        }],
        "en",
        None,
        "test",
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
    assert!(registry
        .call(
            "cinvar.captions.export",
            &mut ctx,
            serde_json::json!({"output": "/tmp/cinvar-escape.srt"})
        )
        .is_err());
    assert!(registry
        .call(
            "cinvar.captions.export",
            &mut ctx,
            serde_json::json!({"output": "../x.srt"})
        )
        .is_err());
    assert!(registry
        .call(
            "cinvar.captions.export",
            &mut ctx,
            serde_json::json!({"output": "exports/x.txt"})
        )
        .is_err());
    let ok = registry
        .call(
            "cinvar.captions.export",
            &mut ctx,
            serde_json::json!({"output": "exports/x.vtt"}),
        )
        .unwrap();
    assert_eq!(ok["cues"], 1);
}
