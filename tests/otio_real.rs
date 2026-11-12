//! Interchange checked against the REAL OpenTimelineIO Python library (not CINVAR's own
//! reader). Ignored by default; run with
//!   CINVAR_OTIO_PYTHON=~/cinvar-deps/otio-venv/bin/python cargo test --test otio_real -- --ignored
//! The test FAILS (never silently passes) if the interpreter or library is missing.

use std::path::PathBuf;
use std::process::Command;

use cinvar::project::model::TrackKind;
use cinvar::project::ops::{self, ClipRequest};
use cinvar::project::store::ProjectStore;
use cinvar::util::time::{Rational, Time};
use serde_json::Value;

fn python() -> String {
    std::env::var("CINVAR_OTIO_PYTHON")
        .expect("set CINVAR_OTIO_PYTHON to a python with opentimelineio installed")
}

fn script(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/py")
        .join(name)
}

fn run_py(script_name: &str, args: &[&str]) -> Value {
    let out = Command::new(python())
        .arg(script(script_name))
        .args(args)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "python failed ({:?}): {}",
        out.status,
        String::from_utf8_lossy(&out.stderr)
    );
    serde_json::from_slice(&out.stdout).unwrap_or(Value::Null)
}

fn media(dir: &std::path::Path) -> PathBuf {
    let p = dir.join("m.mp4");
    assert!(Command::new("ffmpeg")
        .args([
            "-y",
            "-v",
            "error",
            "-f",
            "lavfi",
            "-i",
            "testsrc2=size=320x180:rate=24:duration=4",
            "-f",
            "lavfi",
            "-i",
            "sine=frequency=300:duration=4",
            "-c:v",
            "libx264",
            "-pix_fmt",
            "yuv420p",
            "-c:a",
            "aac",
            "-shortest"
        ])
        .arg(&p)
        .status()
        .unwrap()
        .success());
    p
}

#[test]
#[ignore = "needs python with opentimelineio (set CINVAR_OTIO_PYTHON)"]
fn cinvar_export_is_readable_by_real_otio_and_round_trips() {
    let dir = tempfile::tempdir().unwrap();
    let src = media(dir.path());
    let (store, mut project) = ProjectStore::create(dir.path(), "otio").unwrap();
    let asset = store.import_asset(&mut project, &src, "t").unwrap();
    let tl = ops::create_timeline(&mut project, "Main", 320, 180, Rational::new(24, 1)).unwrap();
    let v = project
        .timeline_by_id(&tl)
        .unwrap()
        .tracks_of(TrackKind::Video)[0]
        .id
        .clone();
    let a = project
        .timeline_by_id(&tl)
        .unwrap()
        .tracks_of(TrackKind::Audio)[0]
        .id
        .clone();
    for (start, s_in, s_out) in [(0.0, 0.5, 1.5), (2.0, 0.0, 1.0)] {
        ops::insert_clip(
            &mut project,
            Some(&tl),
            Some(&v),
            ClipRequest {
                asset_id: &asset.id,
                source_in: Some(Time::from_secs_f64(s_in)),
                source_out: Some(Time::from_secs_f64(s_out)),
                at: Some(Time::from_secs_f64(start)),
                name: None,
            },
        )
        .unwrap();
    }
    ops::insert_clip(
        &mut project,
        Some(&tl),
        Some(&a),
        ClipRequest {
            asset_id: &asset.id,
            source_in: Some(Time::ZERO),
            source_out: Some(Time::from_secs_f64(3.0)),
            at: Some(Time::ZERO),
            name: None,
        },
    )
    .unwrap();
    store.save(&mut project).unwrap();

    let otio_path = store.root.join("export.otio");
    let report =
        cinvar::otio::export_otio_in(&project, None, &otio_path, Some(&store.root)).unwrap();
    eprintln!("export warnings: {:?}", report.warnings);

    let round = store.root.join("roundtrip.otio");
    let seen = run_py(
        "otio_inspect.py",
        &[otio_path.to_str().unwrap(), round.to_str().unwrap()],
    );
    eprintln!("{seen}");
    let tracks = seen["tracks"]
        .as_array()
        .expect("real OTIO parsed the tracks");
    assert_eq!(tracks.len(), 2);
    let vt = &tracks[0];
    assert_eq!(vt["kind"], "Video");
    let items = vt["items"].as_array().unwrap();
    assert_eq!(
        items
            .iter()
            .map(|i| i["type"].as_str().unwrap())
            .collect::<Vec<_>>(),
        vec!["clip", "gap", "clip"]
    );
    assert!((items[0]["start"].as_f64().unwrap() - 0.5).abs() < 0.01);
    assert!((items[0]["duration"].as_f64().unwrap() - 1.0).abs() < 0.01);
    assert!(
        (items[1]["duration"].as_f64().unwrap() - 1.0).abs() < 0.01,
        "gap between 1.0s and 2.0s"
    );
    assert!(
        items[0]["target"].as_str().unwrap().ends_with(".mp4"),
        "media reference must be preserved"
    );
    assert_eq!(tracks[1]["kind"], "Audio");

    // The file rewritten by the real library imports back into CINVAR with the same structure.
    let (store2, mut project2) = ProjectStore::create(dir.path(), "otio-back").unwrap();
    let imp = cinvar::otio::import_otio(&mut project2, &store2, &round).unwrap();
    eprintln!("import: {imp:?}");
    let t2 = project2.timeline_by_id(&imp.timeline_id).unwrap();
    let vclips: Vec<_> = t2
        .tracks_of(TrackKind::Video)
        .into_iter()
        .flat_map(|t| t.sorted_clips())
        .collect();
    assert_eq!(vclips.len(), 2);
    assert!((vclips[0].source_in.as_secs_f64() - 0.5).abs() < 0.01);
    assert!((vclips[0].duration().as_secs_f64() - 1.0).abs() < 0.01);
    assert!(
        (vclips[1].timeline_start.as_secs_f64() - 2.0).abs() < 0.01,
        "gap preserved on import"
    );
    assert_eq!(
        t2.tracks_of(TrackKind::Audio)
            .iter()
            .map(|t| t.clips.len())
            .sum::<usize>(),
        1
    );
}

#[test]
#[ignore = "needs python with opentimelineio (set CINVAR_OTIO_PYTHON)"]
fn timeline_authored_by_real_otio_imports_into_cinvar() {
    let dir = tempfile::tempdir().unwrap();
    let src = media(dir.path());
    let authored = dir.path().join("authored.otio");
    run_py(
        "otio_author.py",
        &[authored.to_str().unwrap(), src.to_str().unwrap()],
    );
    let (store, mut project) = ProjectStore::create(dir.path(), "from-otio").unwrap();
    let imp = cinvar::otio::import_otio(&mut project, &store, &authored).unwrap();
    let t = project.timeline_by_id(&imp.timeline_id).unwrap();
    let v = t.tracks_of(TrackKind::Video);
    let clips = v[0].sorted_clips();
    assert_eq!(clips.len(), 2);
    assert!((clips[0].source_in.as_secs_f64() - 0.5).abs() < 0.01);
    assert!((clips[0].duration().as_secs_f64() - 1.0).abs() < 0.01);
    assert!(
        (clips[1].timeline_start.as_secs_f64() - 1.5).abs() < 0.01,
        "1.0s clip + 0.5s gap"
    );
    assert_eq!(t.tracks_of(TrackKind::Audio)[0].clips.len(), 1);
    assert_eq!(
        project.assets.len(),
        1,
        "same media file must dedupe to one asset"
    );
}
