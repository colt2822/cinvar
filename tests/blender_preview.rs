//! Real preview_check / preview_correct evidence (NOT mocked). Ignored by default; run:
//!   CINVAR_BLENDER=/usr/bin/blender cargo test --release --test blender_preview -- --ignored --nocapture
//!
//! The scene is built with real Blender and contains two deliberate defects: a prop parked
//! outside the camera frame and a box sunk through the floor plane.

use std::path::{Path, PathBuf};
use std::process::Command;

use cinvar::blender::preview::{self, FrameChoice, PreviewRequest, Severity};
use cinvar::blender::{self};

const SCENE_BUILDER: &str = r#"
import math
import sys
import bpy

out = sys.argv[sys.argv.index("--") + 1]
sunken_x = float(sys.argv[sys.argv.index("--") + 2])
bpy.ops.wm.read_factory_settings(use_empty=True)
scene = bpy.context.scene
scene.name = "CINVAR_PREVIEW_TEST"
scene.frame_start = 1
scene.frame_end = 90
scene.render.resolution_x = 960
scene.render.resolution_y = 540
scene.render.fps = 30

bpy.ops.object.camera_add(location=(0.0, -6.0, 1.0), rotation=(math.radians(87.0), 0.0, 0.0))
cam = bpy.context.active_object
cam.name = "CINVAR_CAMERA"
cam.data.lens = 50.0
scene.camera = cam

bpy.ops.object.light_add(type="AREA", location=(3.0, -5.0, 4.0))
bpy.context.active_object.data.energy = 900.0

bpy.ops.object.text_add(location=(0.0, 0.0, 0.0))
text = bpy.context.active_object
text.name = "CINVAR_TEXT"
text.data.body = "CINVAR"
text.data.align_x = "CENTER"
text.rotation_euler = (math.radians(90.0), 0.0, 0.0)

# Deliberate defect 1: parked well outside the camera frame.
bpy.ops.mesh.primitive_cube_add(size=1.0, location=(7.0, 0.0, 0.5))
cube = bpy.context.active_object
cube.name = "OFFSCREEN_CUBE"

# Deliberate defect 2: sunk through the floor plane at z=0.
bpy.ops.mesh.primitive_cube_add(size=1.0, location=(sunken_x, 0.0, -0.35))
sunken = bpy.context.active_object
sunken.name = "SUNKEN_BOX"

bpy.ops.wm.save_as_mainfile(filepath=out)
print("CINVAR_SCENE_SAVED " + out)
"#;

fn blender_path() -> PathBuf {
    blender::available().expect("Blender not found: set CINVAR_BLENDER")
}

fn have(tool: &str) -> bool {
    Command::new(tool)
        .arg("-version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

fn build_scene(dir: &Path, sunken_x: f64) -> PathBuf {
    let source = dir.join("build_scene.py");
    std::fs::write(&source, SCENE_BUILDER).unwrap();
    let blend = dir.join("preview_scene.blend");
    let out = Command::new(blender_path())
        .args(["-b", "-noaudio", "--factory-startup", "--python"])
        .arg(&source)
        .arg("--")
        .arg(&blend)
        .arg(format!("{sunken_x}"))
        .output()
        .expect("Blender runs");
    assert!(
        blend.is_file(),
        "scene builder failed: {}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    blend
}

fn ffprobe_size(path: &Path) -> String {
    let out = Command::new("ffprobe")
        .args([
            "-v",
            "error",
            "-select_streams",
            "v:0",
            "-show_entries",
            "stream=width,height,pix_fmt",
            "-of",
            "csv=p=0",
        ])
        .arg(path)
        .output()
        .expect("ffprobe runs");
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

#[test]
#[ignore = "requires a real Blender install (set CINVAR_BLENDER)"]
fn preview_check_names_the_off_frame_object_with_its_percentage() {
    blender_path();
    assert!(have("ffmpeg") && have("ffprobe"));
    let dir = tempfile::tempdir().unwrap();
    // The sunken box is parked across the logo here, so BVHTree.overlap has real work to do.
    let blend = build_scene(dir.path(), -0.6);
    let out_dir = dir.path().join("preview");

    let report = preview::preview_check(
        &PreviewRequest::new(&blend, &out_dir).frame(FrameChoice::Exact(45)),
    )
    .expect("preview_check on a real scene");

    eprintln!("==== diagnostics ====");
    eprintln!(
        "frame {} camera {} ({:.0}mm) objects {}",
        report.diagnostics.frame,
        report.diagnostics.camera.name,
        report.diagnostics.camera.lens,
        report.diagnostics.objects.len()
    );
    for object in &report.diagnostics.objects {
        eprintln!(
            "  {} [{}] inside={:.1}% outside={:.1}% floor={:.3} occluded={:.0}%",
            object.name,
            object.kind,
            object.inside_pct,
            object.outside_pct,
            object.floor_penetration,
            object.occluded_pct
        );
    }
    eprintln!(
        "image: coverage={:.4} luma={:.3} over={:.1}% under={:.1}% empty={} hist_bins={}",
        report.diagnostics.image.alpha_coverage,
        report.diagnostics.image.mean_luma,
        report.diagnostics.image.over_exposed_pct,
        report.diagnostics.image.under_exposed_pct,
        report.diagnostics.image.empty,
        report.diagnostics.image.luminance_hist.len()
    );
    eprintln!("==== findings ====");
    for finding in &report.findings {
        eprintln!(
            "  [{:?}] {}: {}{}",
            finding.severity,
            finding.code,
            finding.message,
            finding
                .suggestion
                .as_deref()
                .map(|s| format!(" -> {s}"))
                .unwrap_or_default()
        );
    }
    eprintln!(
        "sheet: {} ({}), preview latency {} ms, blender {}",
        report.sheet.display(),
        ffprobe_size(&report.sheet),
        report.elapsed_ms,
        report.blender_version
    );

    // (1) The off-frame object is named, with its exact measured percentage.
    let off_frame = report
        .findings
        .iter()
        .find(|f| f.code == "object_off_frame" && f.subject.as_deref() == Some("OFFSCREEN_CUBE"))
        .or_else(|| {
            report
                .findings
                .iter()
                .find(|f| f.code == "object_off_frame" && f.message.contains("OFFSCREEN_CUBE"))
        })
        .expect("the off-frame prop must be reported");
    assert!(
        off_frame.message.contains("OFFSCREEN_CUBE is")
            && off_frame
                .message
                .contains("% outside the camera frame at frame 45."),
        "unexpected message: {}",
        off_frame.message
    );
    let measured = off_frame
        .value
        .expect("the percentage is carried as a number");
    eprintln!("off-frame percentage: {measured}%");
    assert!(
        measured > 10.0,
        "expected a real excursion, got {measured}%"
    );
    assert!(report
        .findings
        .iter()
        .any(|f| f.code == "floor_penetration"));
    assert!(!report.passed, "a scene with two defects must not pass");

    // (2) The centred logo is not flagged as off-frame, and the frame is not empty.
    assert!(
        !report
            .findings
            .iter()
            .any(|f| f.code == "object_off_frame" && f.subject.as_deref() == Some("CINVAR_TEXT")),
        "the centred logo was wrongly reported as off-frame"
    );
    // The sunken box also overlaps the logo geometry, which BVHTree.overlap must catch.
    let collision = report
        .findings
        .iter()
        .find(|f| f.code == "mesh_collision")
        .expect("BVHTree.overlap must report the intersecting meshes");
    eprintln!(
        "collision message: {} (pairs={:?})",
        collision.message, collision.value
    );
    assert!(!report.diagnostics.image.empty);

    // (3) The contact sheet is a real 1024x1024 PNG.
    assert!(report.sheet.is_file());
    assert_eq!(ffprobe_size(&report.sheet), "1024,1024,rgba");
    assert!(report.diagnostics_path.is_file());

    // (4) Latency, measured honestly (see the printed number above).
    assert!(
        report.elapsed_ms < 120_000,
        "preview took {} ms",
        report.elapsed_ms
    );
    eprintln!("MEASURED preview_check latency: {} ms", report.elapsed_ms);
}

#[test]
#[ignore = "requires a real Blender install (set CINVAR_BLENDER)"]
fn preview_correct_reaches_an_accepted_frame_and_keeps_the_history() {
    blender_path();
    assert!(have("ffmpeg"));
    let dir = tempfile::tempdir().unwrap();
    // Only the two autofixable defects here: a prop out of frame and a box under the floor
    // that clears the logo (no collision, so the loop can actually converge).
    let blend = build_scene(dir.path(), -2.6);
    let out_dir = dir.path().join("corrected");
    let request = PreviewRequest::new(&blend, &out_dir)
        .frame(FrameChoice::Exact(45))
        .max_rounds(4);
    let report = preview::preview_correct(&request).expect("correction loop runs");

    eprintln!("==== correction history ====");
    for round in &report.rounds {
        eprintln!(
            "round {} frame {} passed={} findings={} corrections={}",
            round.round,
            round.frame,
            round.diagnostics.passed(),
            round.findings().len(),
            round.corrections.len()
        );
        for correction in &round.corrections {
            eprintln!("  {}", correction.describe());
        }
    }
    eprintln!(
        "accepted={} latency={} ms sheet={} history={}",
        report.accepted,
        report.elapsed_ms,
        report.sheet.display(),
        report.history_path.display()
    );

    assert!(
        report.rounds.len() >= 2,
        "the loop must iterate at least once"
    );
    assert!(
        report.rounds[0]
            .corrections
            .iter()
            .any(|c| c.op == "camera_dolly"),
        "an off-frame object must pull the camera back"
    );
    assert!(
        report.rounds[0]
            .corrections
            .iter()
            .any(|c| c.op == "raise_object"),
        "a sunken object must be raised"
    );
    assert!(
        report
            .rounds
            .iter()
            .flat_map(|r| r.corrections.iter())
            .all(|c| c.op == "camera_dolly" || c.op == "raise_object"),
        "the loop must not move geometry it cannot fix numerically"
    );
    assert!(report.accepted, "the bounded loop must reach a clean frame");
    assert!(report
        .findings
        .iter()
        .all(|f| f.severity != Severity::Error));
    assert_eq!(ffprobe_size(&report.sheet), "1024,1024,rgba");

    // The history is persisted as JSON and carries every round.
    let history: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&report.history_path).unwrap()).unwrap();
    let rounds = history["rounds"].as_array().expect("rounds array");
    assert_eq!(rounds.len(), report.rounds.len());
    assert_eq!(history["accepted"], serde_json::json!(true));
    assert!(rounds[0]["corrections"]
        .as_array()
        .unwrap()
        .iter()
        .any(|c| c.as_str().unwrap_or_default().contains("camera")));
    assert!(!rounds[0]["findings"].as_array().unwrap().is_empty());
    eprintln!("history rounds persisted: {}", rounds.len());
}
