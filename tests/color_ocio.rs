//! OpenColorIO integration. The real-library tests are #[ignore] and need
//!   CINVAR_OCIO_PYTHON=/path/to/python-with-opencolorio   (pip install opencolorio)
//! cargo test --test color_ocio -- --ignored

use cinvar::project::model::TrackKind;
use cinvar::project::ops::{self, ClipRequest};
use cinvar::project::store::ProjectStore;
use cinvar::util::time::{Rational, Time};

fn write(dir: &std::path::Path, name: &str, text: &str) -> std::path::PathBuf {
    let p = dir.join(name);
    std::fs::write(&p, text).unwrap();
    p
}

fn identity_cube(size: u32) -> String {
    let mut s = format!("LUT_3D_SIZE {size}\n");
    let m = (size - 1) as f64;
    for b in 0..size {
        for g in 0..size {
            for r in 0..size {
                s.push_str(&format!(
                    "{} {} {}\n",
                    r as f64 / m,
                    g as f64 / m,
                    b as f64 / m
                ));
            }
        }
    }
    s
}

#[test]
fn cube_validation_accepts_good_and_rejects_bad_files() {
    let d = tempfile::tempdir().unwrap();
    assert_eq!(
        cinvar::color::validate_cube(&write(d.path(), "ok.cube", &identity_cube(3))).unwrap(),
        3
    );
    let mut short = identity_cube(3);
    short.truncate(short.len() - 12);
    assert!(cinvar::color::validate_cube(&write(d.path(), "short.cube", &short)).is_err());
    assert!(
        cinvar::color::validate_cube(&write(d.path(), "oned.cube", "LUT_1D_SIZE 4\n0 0 0\n"))
            .is_err()
    );
    assert!(cinvar::color::validate_cube(&write(
        d.path(),
        "junk.cube",
        "LUT_3D_SIZE 2\nnot numbers here\n"
    ))
    .is_err());
    assert!(cinvar::color::validate_cube(&write(
        d.path(),
        "nan.cube",
        "LUT_3D_SIZE 2\nnan 0 0\n0 0 0\n0 0 0\n0 0 0\n0 0 0\n0 0 0\n0 0 0\n"
    ))
    .is_err());
    assert!(cinvar::color::validate_cube(&d.path().join("missing.cube")).is_err());
}

fn make_clip_project(
    dir: &std::path::Path,
) -> (ProjectStore, cinvar::project::model::Project, String) {
    let src = dir.join("src.mp4");
    assert!(std::process::Command::new("ffmpeg")
        .args([
            "-y",
            "-v",
            "error",
            "-f",
            "lavfi",
            "-i",
            "testsrc2=size=320x180:rate=24:duration=1",
            "-pix_fmt",
            "yuv420p"
        ])
        .arg(&src)
        .status()
        .unwrap()
        .success());
    let (store, mut project) = ProjectStore::create(dir, "ocio").unwrap();
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
    (store, project, clip)
}

fn mean_luma(path: &std::path::Path) -> f64 {
    let out = std::process::Command::new("ffmpeg")
        .args(["-v", "error", "-i"])
        .arg(path)
        .args([
            "-vf",
            "scale=1:1,format=gray",
            "-f",
            "rawvideo",
            "-frames:v",
            "1",
            "-",
        ])
        .output()
        .unwrap();
    out.stdout[0] as f64
}

fn render(
    project: &cinvar::project::model::Project,
    store: &ProjectStore,
    name: &str,
) -> std::path::PathBuf {
    let out = store.renders_dir().join(name);
    cinvar::render::render(
        project,
        store,
        &cinvar::render::RenderRequest::for_output(&out),
        &|_, _| {},
        &|| false,
    )
    .expect("render");
    out
}

#[test]
fn identity_lut_effect_renders_and_invalid_lut_is_skipped_with_a_warning() {
    let d = tempfile::tempdir().unwrap();
    let (store, mut project, clip) = make_clip_project(d.path());
    let before = render(&project, &store, "plain.mp4");
    std::fs::create_dir_all(store.root.join("luts")).unwrap();
    std::fs::write(store.root.join("luts/id.cube"), identity_cube(9)).unwrap();
    ops::add_effect(
        &mut project,
        &clip,
        "lut",
        serde_json::json!({"file": "luts/id.cube"}),
    )
    .unwrap();
    let after = render(&project, &store, "id.mp4");
    assert!(
        (mean_luma(&before) - mean_luma(&after)).abs() <= 2.0,
        "identity LUT must not change the picture"
    );
    // a LUT outside the project is skipped, never followed
    ops::add_effect(
        &mut project,
        &clip,
        "lut",
        serde_json::json!({"file": "/etc/passwd"}),
    )
    .unwrap();
    let still = render(&project, &store, "skip.mp4");
    assert!((mean_luma(&before) - mean_luma(&still)).abs() <= 2.0);
}

#[test]
#[ignore = "requires opencolorio in CINVAR_OCIO_PYTHON"]
fn real_ocio_inspect_bake_apply_and_render() {
    let d = tempfile::tempdir().unwrap();
    let (store, mut project, clip) = make_clip_project(d.path());
    let registry = cinvar::tools::build_default();
    let config = cinvar::config::Config::default();
    let jobs = cinvar::jobs::Jobs::new();
    let mut history = cinvar::project::history::History::default();
    let before = render(&project, &store, "plain.mp4");
    let mut ctx = cinvar::tools::ToolCtx {
        project: &mut project,
        store: &store,
        config: &config,
        jobs: &jobs,
        history: &mut history,
        dry_run: false,
    };

    let info = registry
        .call("cinvar.color.inspect", &mut ctx, serde_json::json!({}))
        .expect("inspect");
    let spaces: Vec<String> = info["color_spaces"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap().to_string())
        .collect();
    assert!(spaces.iter().any(|s| s == "ACEScg"), "{spaces:?}");
    assert!(info["config_source"].as_str().unwrap().contains("builtin"));

    // unknown names are errors with suggestions, never guesses
    let err = registry
        .call(
            "cinvar.color.apply_transform",
            &mut ctx,
            serde_json::json!({"clip_id": clip, "source": "sRGB", "destination": "NopeSpace"}),
        )
        .unwrap_err();
    assert!(err.message.contains("unknown colour space"), "{err}");

    let ok = registry
        .call(
            "cinvar.color.apply_transform",
            &mut ctx,
            serde_json::json!({"clip_id": clip, "source": "sRGB Encoded Rec.709 (sRGB)", "destination": "Linear Rec.709 (sRGB)", "size": 17}),
        )
        .expect("apply");
    assert!(ok["effect_id"].is_string());
    let lut = store.root.join(ok["lut"].as_str().unwrap());
    assert_eq!(cinvar::color::validate_cube(&lut).unwrap(), 17);

    let after = render(&project, &store, "linear.mp4");
    // sRGB -> linear darkens midtones: a real, visible, explicit transform.
    assert!(
        mean_luma(&after) + 4.0 < mean_luma(&before),
        "before {} after {}",
        mean_luma(&before),
        mean_luma(&after)
    );
}

#[test]
fn lut_path_with_spaces_commas_and_quotes_still_renders() {
    let d = tempfile::tempdir().unwrap();
    let awkward = d.path().join("a b,c'd [x]");
    std::fs::create_dir_all(&awkward).unwrap();
    let (store, mut project, clip) = make_clip_project(&awkward);
    let before = render(&project, &store, "plain.mp4");
    std::fs::create_dir_all(store.root.join("luts")).unwrap();
    // a clearly non-identity LUT (halves every channel) so a skipped LUT would be detected
    let mut inv = String::from("LUT_3D_SIZE 2\n");
    for b in 0..2 {
        for g in 0..2 {
            for r in 0..2 {
                inv.push_str(&format!(
                    "{} {} {}\n",
                    r as f64 * 0.5,
                    g as f64 * 0.5,
                    b as f64 * 0.5
                ));
            }
        }
    }
    std::fs::write(store.root.join("luts/invert.cube"), inv).unwrap();
    ops::add_effect(
        &mut project,
        &clip,
        "lut",
        serde_json::json!({"file": "luts/invert.cube"}),
    )
    .unwrap();
    let after = render(&project, &store, "inv.mp4");
    assert!(
        (mean_luma(&before) - mean_luma(&after)).abs() > 20.0,
        "LUT was not applied: {} vs {}",
        mean_luma(&before),
        mean_luma(&after)
    );
}
