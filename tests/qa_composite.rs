//! Regression: a transparent overlay (ProRes 4444) over footage must keep the footage visible,
//! and CINVAR's QA must flag the render when the base is blacked out (the 7fa76aa alpha defect).
//! Needs only ffmpeg/ffprobe; no Blender.

use std::path::Path;
use std::process::Command;

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_cinvar")
}

fn ff(args: &[&str]) {
    let st = Command::new("ffmpeg").args(["-v", "error", "-y"]).args(args).status().expect("ffmpeg");
    assert!(st.success(), "ffmpeg {args:?}");
}

fn cinvar(project: &Path, media: &Path, args: &[&str]) -> String {
    let out = Command::new(bin())
        .args(["--json", "--project"])
        .arg(project)
        .arg("--media-root")
        .arg(media)
        .args(args)
        .output()
        .expect("cinvar");
    assert!(out.status.success(), "{args:?}: {}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout).to_string()
}

fn id(json: &str, key: &str) -> String {
    let v: serde_json::Value = serde_json::from_str(json).expect("json");
    v[key].as_str().unwrap_or_else(|| panic!("no {key} in {json}")).to_string()
}

fn composite_check(report: &str) -> (bool, String) {
    let v: serde_json::Value = serde_json::from_str(report).expect("qa json");
    let c = v["checks"].as_array().unwrap().iter().find(|c| c["name"] == "composite_visibility").expect("check present");
    (c["passed"].as_bool().unwrap(), c["detail"].as_str().unwrap().to_string())
}

#[test]
fn qa_flags_a_blacked_out_base_and_passes_a_correct_composite() {
    let dir = tempfile::tempdir().unwrap();
    let m = dir.path().join("media");
    std::fs::create_dir_all(&m).unwrap();
    let base = m.join("base.mp4");
    let overlay = m.join("overlay.mov");
    ff(&["-f", "lavfi", "-i", "testsrc2=s=320x180:r=24:d=3", "-pix_fmt", "yuv420p", base.to_str().unwrap()]);
    // Transparent canvas with one opaque white box in the middle.
    ff(&[
        "-f", "lavfi", "-i", "color=c=black@0.0:s=320x180:r=24:d=2,format=rgba",
        "-vf", "drawbox=x=100:y=50:w=120:h=80:color=white@1.0:t=fill,format=yuva444p10le",
        "-c:v", "prores_ks", "-profile:v", "4444", overlay.to_str().unwrap(),
    ]);

    let proj = dir.path().join("proj");
    cinvar(dir.path(), &m, &["project", "create", "--name", "qa", "--dir", proj.to_str().unwrap()]);
    let proj = proj.join("qa");
    cinvar(&proj, &m, &["tool", "call", "cinvar.timeline.create", "--args", r#"{"width":320,"height":180,"fps":"24"}"#]);
    let b = id(&cinvar(&proj, &m, &["tool", "call", "cinvar.media.import", "--args", &format!(r#"{{"path":"{}"}}"#, base.display())]), "asset_id");
    let o = id(&cinvar(&proj, &m, &["tool", "call", "cinvar.media.import", "--args", &format!(r#"{{"path":"{}"}}"#, overlay.display())]), "asset_id");
    let track = id(&cinvar(&proj, &m, &["tool", "call", "cinvar.timeline.add_track", "--args", r#"{"kind":"video"}"#]), "track_id");
    cinvar(&proj, &m, &["tool", "call", "cinvar.timeline.append_clip", "--args", &format!(r#"{{"asset_id":"{b}"}}"#)]);
    cinvar(&proj, &m, &["tool", "call", "cinvar.timeline.insert_clip", "--args", &format!(r#"{{"asset_id":"{o}","track_id":"{track}","at":0.5}}"#)]);
    let export = cinvar(&proj, &m, &["tool", "call", "cinvar.render.export", "--args", "{}", "--wait"]);
    let good = id(&export, "output");

    let (ok, detail) = composite_check(&cinvar(&proj, &m, &["qa", &good]));
    assert!(ok, "a correct composite must pass: {detail}");

    // The original failure: base footage flattened to black with only the overlay box visible.
    let bad = proj.join("renders").join("alpha_flattened.mp4");
    ff(&["-f", "lavfi", "-i", "color=c=black:s=320x180:r=24:d=3,drawbox=x=100:y=50:w=120:h=80:color=white:t=fill", "-pix_fmt", "yuv420p", bad.to_str().unwrap()]);
    let (ok, detail) = composite_check(&cinvar(&proj, &m, &["qa", bad.to_str().unwrap()]));
    assert!(!ok, "a blacked-out base must fail QA");
    assert!(detail.contains("hides the base"), "{detail}");
}
