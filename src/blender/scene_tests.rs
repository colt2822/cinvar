//! Tests for the declarative scene system.
//!
//! The version-compat and document tests are pure: no Blender, no ffmpeg, no bpy. The live
//! acceptance scenes (a real SVG logo and a real imported .glb) run only when
//! CINVAR_SCENE_LIVE=1 is set, because each one starts Blender twice and renders on the CPU.

use std::path::{Path, PathBuf};

use super::*;

// ---------------------------------------------------------------------------
// Version-compat shim: the socket names two Blender generations actually expose
// ---------------------------------------------------------------------------

/// Blender 3.6 Principled BSDF inputs (pre-rename).
const SOCKETS_36: &[&str] = &[
    "Base Color",
    "Subsurface",
    "Subsurface Color",
    "Metallic",
    "Specular",
    "Roughness",
    "IOR",
    "Alpha",
    "Transmission",
    "Emission",
    "Emission Strength",
];

/// Blender 4.0-5.x Principled BSDF inputs (4.0 renamed four of them).
const SOCKETS_4X: &[&str] = &[
    "Base Color",
    "Metallic",
    "Roughness",
    "IOR",
    "Alpha",
    "Subsurface Weight",
    "Specular IOR Level",
    "Transmission Weight",
    "Emission Color",
    "Emission Strength",
];

#[test]
fn socket_shim_maps_blender_36_sockets() {
    assert_eq!(principled_socket(SOCKETS_36, "base_color"), Some("Base Color"));
    assert_eq!(principled_socket(SOCKETS_36, "metallic"), Some("Metallic"));
    assert_eq!(principled_socket(SOCKETS_36, "roughness"), Some("Roughness"));
    // The renamed roles resolve to their 3.x names, not to their 4.x ones.
    assert_eq!(principled_socket(SOCKETS_36, "emission_color"), Some("Emission"));
    assert_eq!(
        principled_socket(SOCKETS_36, "emission_strength"),
        Some("Emission Strength")
    );
    assert_eq!(principled_socket(SOCKETS_36, "transmission"), Some("Transmission"));
    assert_eq!(principled_socket(SOCKETS_36, "specular"), Some("Specular"));
    assert_eq!(principled_socket(SOCKETS_36, "subsurface"), Some("Subsurface"));
}

#[test]
fn socket_shim_maps_blender_4x_and_5x_sockets() {
    assert_eq!(
        principled_socket(SOCKETS_4X, "emission_color"),
        Some("Emission Color")
    );
    assert_eq!(principled_socket(SOCKETS_4X, "transmission"), Some("Transmission Weight"));
    assert_eq!(principled_socket(SOCKETS_4X, "specular"), Some("Specular IOR Level"));
    assert_eq!(principled_socket(SOCKETS_4X, "subsurface"), Some("Subsurface Weight"));
    assert_eq!(principled_socket(SOCKETS_4X, "alpha"), Some("Alpha"));
    assert_eq!(principled_socket(SOCKETS_4X, "ior"), Some("IOR"));
    // No role may resolve a 4.x-only name against the 3.6 socket set.
    for role in ["emission_color", "transmission", "specular", "subsurface"] {
        let resolved = principled_socket(SOCKETS_36, role).expect("3.6 socket");
        assert!(
            !SOCKETS_4X.contains(&resolved),
            "{resolved} is a 4.x-only socket name"
        );
    }
}

#[test]
fn socket_shim_reports_a_missing_role_instead_of_guessing() {
    let sockets: &[&str] = &["Base Color", "Metallic"];
    assert_eq!(principled_socket(sockets, "emission_strength"), None);
    assert_eq!(principled_socket(sockets, "not_a_role"), None);
    assert!(principled_candidates("not_a_role").is_empty());
}

#[test]
fn socket_alias_table_is_the_json_map_the_sdk_reads() {
    let table = socket_alias_table();
    let object = table.as_object().expect("alias table is an object");
    assert_eq!(object.len(), PRINCIPLED_ALIASES.len());
    let emission = object["emission_color"].as_array().expect("array");
    assert_eq!(emission[0], "Emission Color");
    assert_eq!(emission[1], "Emission");
}

#[test]
fn eevee_engine_name_tracks_the_three_generations() {
    assert_eq!(eevee_engine(3, 6), "BLENDER_EEVEE");
    assert_eq!(eevee_engine(4, 0), "BLENDER_EEVEE");
    assert_eq!(eevee_engine(4, 1), "BLENDER_EEVEE");
    assert_eq!(eevee_engine(4, 2), "BLENDER_EEVEE_NEXT");
    assert_eq!(eevee_engine(4, 5), "BLENDER_EEVEE_NEXT");
    assert_eq!(eevee_engine(5, 0), "BLENDER_EEVEE");
    assert!(principled_renamed(4, 0));
    assert!(!principled_renamed(3, 6));
}

#[test]
fn the_sdk_carries_the_same_shim_tables() {
    let sdk = sdk_source();
    for needle in [
        "PRINCIPLED_ALIASES = {",
        "\"emission_color\": (\"Emission Color\", \"Emission\"),",
        "\"transmission\": (\"Transmission Weight\", \"Transmission\"),",
        "\"specular\": (\"Specular IOR Level\", \"Specular\"),",
        "\"subsurface\": (\"Subsurface Weight\", \"Subsurface\"),",
        "def eevee_engine_name(version):",
        "BLENDER_EEVEE_NEXT",
        "def principled_socket(bsdf, role, aliases):",
        "def sc_guarded_op(scene, name, kwargs, warnings, label):",
        "def sc_svg_extrude(scene, objects, depth, warnings):",
        "def sc_missing_textures(objects, warnings):",
        "def run_scene(job):",
    ] {
        assert!(sdk.contains(needle), "SDK is missing {needle}");
    }
}

#[test]
fn sdk_is_valid_python() {
    let Some(python) = proc::which("python3") else {
        eprintln!("python3 not installed; skipping the syntax check");
        return;
    };
    let dir = tempfile::tempdir().expect("temp dir");
    let path = dir.path().join("cinvar_bpy_sdk.py");
    std::fs::write(&path, sdk_source()).expect("write sdk");
    let output = std::process::Command::new(python)
        .arg("-c")
        .arg("import ast,sys; ast.parse(open(sys.argv[1],encoding='utf-8').read())")
        .arg(&path)
        .output()
        .expect("python3 runs");
    assert!(
        output.status.success(),
        "generated SDK is not valid python: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn sdk_socket_table_matches_the_rust_table() {
    let Some(python) = proc::which("python3") else {
        eprintln!("python3 not installed; skipping the drift check");
        return;
    };
    let dir = tempfile::tempdir().expect("temp dir");
    let path = dir.path().join("cinvar_bpy_sdk.py");
    std::fs::write(&path, sdk_source()).expect("write sdk");
    // The SDK imports bpy at module level, so read its table with ast instead of importing it.
    let script = r#"
import ast, json, sys
tree = ast.parse(open(sys.argv[1], encoding="utf-8").read())
for node in tree.body:
    if isinstance(node, ast.Assign) and node.targets[0].id == "PRINCIPLED_ALIASES":
        print(json.dumps({k: list(v) for k, v in ast.literal_eval(node.value).items()}))
"#;
    let output = std::process::Command::new(python)
        .arg("-c")
        .arg(script)
        .arg(&path)
        .output()
        .expect("python3 runs");
    assert!(
        output.status.success(),
        "could not read the SDK table: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let sdk_table: std::collections::BTreeMap<String, Vec<String>> =
        serde_json::from_slice(&output.stdout).expect("SDK table is JSON");
    for (role, candidates) in PRINCIPLED_ALIASES {
        let python_side = sdk_table
            .get(*role)
            .unwrap_or_else(|| panic!("the SDK has no {role} role"));
        assert_eq!(
            python_side,
            &candidates.iter().map(|c| c.to_string()).collect::<Vec<_>>(),
            "the SDK and Rust disagree about the {role} socket names"
        );
    }
    assert_eq!(sdk_table.len(), PRINCIPLED_ALIASES.len());
}

// ---------------------------------------------------------------------------
// The scene document
// ---------------------------------------------------------------------------

fn full_scene() -> CinvarBlenderScene {
    CinvarBlenderScene::new("DOC_TEST")
        .material(MaterialSpec::new("BODY", "#4f8cff").metallic(0.2).roughness(0.3))
        .material(MaterialSpec::new("GLOW", "#ff2d95").emission("#ff2d95", 4.0).alpha(0.9))
        .object(SceneObject::new("TITLE", ObjectKind::text("CINVAR")))
        .object(
            SceneObject::new("PUCK", ObjectKind::cylinder(0.4, 0.2))
                .transform(Transform::at([0.0, 0.0, 0.1]))
                .material("BODY"),
        )
        .asset(AssetImport::new("logo", "/tmp/logo.svg").fit(2.0).extrude(0.2).material("BODY"))
        .camera(CameraRig::orbit(Some([0.0, 0.0, 0.5]), 4.0, 20.0, -30.0, 30.0).with_lens(35.0))
        .lighting(Lighting::studio_three_point().with_key_intensity(750.0))
        .render(
            RenderSettings::size(1280, 720)
                .engine(RenderEngine::Cycles)
                .samples(32)
                .frames(1, 90)
                .transparent(),
        )
}

#[test]
fn scene_round_trips_through_json() {
    let scene = full_scene();
    let text = serde_json::to_string_pretty(&scene).expect("serializes");
    let parsed: CinvarBlenderScene = serde_json::from_str(&text).expect("parses");
    assert_eq!(parsed.name, "DOC_TEST");
    assert_eq!(parsed.objects.len(), 2);
    assert_eq!(parsed.materials.len(), 2);
    assert_eq!(parsed.assets.len(), 1);
    assert_eq!(parsed.camera.rig, RigKind::Orbit);
    assert_eq!(parsed.camera.radius, Some(4.0));
    assert_eq!(parsed.lighting.preset, LightPreset::StudioThreePoint);
    assert_eq!(parsed.render.engine, RenderEngine::Cycles);
    assert!(parsed.render.transparent);
    assert_eq!(parsed.render.frame_count(), 90);
    parsed.validate().expect("valid scene");
}

#[test]
fn every_rig_and_preset_round_trips() {
    let dolly = CameraRig::dolly_track(None, [-3.0, -3.0, 1.5], [0.0, -4.0, 1.0], 50.0, Some(1.8));
    let parsed: CameraRig =
        serde_json::from_str(&serde_json::to_string(&dolly).unwrap()).expect("dolly parses");
    assert_eq!(parsed.rig, RigKind::DollyTrack);
    assert_eq!(parsed.start_pos, Some([-3.0, -3.0, 1.5]));
    assert_eq!(parsed.dof_fstop, Some(1.8));

    let turntable = CameraRig::isometric_turntable(None, 0.15);
    let parsed: CameraRig =
        serde_json::from_str(&serde_json::to_string(&turntable).unwrap()).expect("turntable parses");
    assert_eq!(parsed.rig, RigKind::IsometricTurntable);
    assert_eq!(parsed.padding, Some(0.15));

    for lighting in [
        Lighting::studio_three_point(),
        Lighting::cyberpunk_neon(),
        Lighting::product_cyclorama(),
        Lighting::explicit(vec![
            LightSpec::new("KEY", LightKind::Area, 900.0, [-2.0, -2.0, 3.0])
                .color("#ffe8c0")
                .size(3.0),
        ]),
    ] {
        let text = serde_json::to_string(&lighting).expect("lighting serializes");
        let parsed: Lighting = serde_json::from_str(&text).expect("lighting parses");
        assert_eq!(parsed.preset, lighting.preset);
        assert_eq!(parsed.lights.len(), lighting.lights.len());
        parsed.validate().expect("lighting validates");
    }
}

#[test]
fn a_sparse_scene_fills_in_documented_defaults() {
    let scene: CinvarBlenderScene =
        serde_json::from_str(r#"{"name": "MINIMAL"}"#).expect("minimal scene parses");
    assert_eq!(scene.objects.len(), 0);
    assert_eq!(scene.render.width, 1920);
    assert_eq!(scene.render.height, 1080);
    assert_eq!(scene.render.fps, 30);
    assert_eq!(scene.render.samples, 64);
    assert_eq!(scene.render.engine, RenderEngine::Eevee);
    assert!(!scene.render.transparent);
    assert_eq!(scene.camera.rig, RigKind::Orbit);
    assert_eq!(scene.lighting.preset, LightPreset::StudioThreePoint);
    scene.validate().expect("minimal scene is valid");
}

#[test]
fn validation_rejects_dangling_references_and_empty_ranges() {
    let duplicate = full_scene().object(SceneObject::new("TITLE", ObjectKind::cube(1.0)));
    assert!(duplicate.validate().is_err(), "duplicate object name must fail");

    let undefined = CinvarBlenderScene::new("BAD")
        .object(SceneObject::new("BOX", ObjectKind::cube(1.0)).material("MISSING"));
    assert!(undefined.validate().is_err(), "undefined material must fail");

    let empty_range =
        CinvarBlenderScene::new("BAD").render(RenderSettings::default().frames(10, 10));
    assert!(empty_range.validate().is_err(), "empty frame range must fail");

    let no_padding = CinvarBlenderScene::new("BAD").camera(CameraRig {
        rig: RigKind::IsometricTurntable,
        ..CameraRig::default()
    });
    assert!(no_padding.validate().is_err(), "turntable without padding must fail");

    let no_light = CinvarBlenderScene::new("BAD").lighting(Lighting::explicit(Vec::new()));
    assert!(no_light.validate().is_err(), "empty explicit lighting must fail");

    let bad_asset = CinvarBlenderScene::new("BAD")
        .asset(AssetImport::new("logo", "/tmp/logo.svg").extrude(0.0));
    assert!(bad_asset.validate().is_err(), "an SVG without thickness must fail");
}

#[test]
fn transparent_and_frame_range_reach_the_animation_spec() {
    let scene = full_scene();
    let spec = scene.animation_spec();
    assert!(
        spec.transparent,
        "the scene transparent flag must reach AnimationSpec"
    );
    assert_eq!(spec.width, 1280);
    assert_eq!(spec.height, 720);
    assert_eq!(spec.fps, 30);
    assert!(
        (spec.duration.as_secs_f64() - 3.0).abs() < 1e-3,
        "90 frames at 30 fps is 3 s"
    );
    assert_eq!(spec.color, "#4f8cff");

    let opaque = CinvarBlenderScene::new("OPAQUE");
    assert!(!opaque.animation_spec().transparent);
}

// ---------------------------------------------------------------------------
// The driver is constant: the scene never becomes python source
// ---------------------------------------------------------------------------

#[test]
fn the_driver_is_a_constant_that_executes_no_scene_data() {
    assert!(DRIVER_SOURCE.contains("sdk.run_scene(JOB)"));
    assert!(DRIVER_SOURCE.contains("json.load(handle)"));
    // No interpolation, no formatting, no eval: the payload is only ever data.
    for forbidden in ["eval(", "exec(", "format(", "%s", "{", "f\""] {
        assert!(
            !DRIVER_SOURCE.contains(forbidden),
            "driver must not contain {forbidden}"
        );
    }
}

#[test]
fn a_hostile_scene_string_stays_data() {
    let hostile = "LOGO\"); import os; os.system('rm -rf /'); #\nprint(1)";
    let scene = CinvarBlenderScene::new(hostile)
        .object(SceneObject::new(hostile, ObjectKind::text(hostile)));
    let payload = serde_json::to_string(&scene).expect("serializes");
    // A JSON payload has no raw newline and escapes the quote, so it cannot break a literal.
    assert!(!payload.contains('\n'), "payload must stay on one line");
    // The hostile quote is escaped, so the text is inert even inside a python string literal.
    assert!(
        payload.contains("\\\"); import os"),
        "the payload quote must be escaped: {payload}"
    );
    assert!(!payload.contains("\"\"\""), "no triple-quote break-out");
    let parsed: CinvarBlenderScene = serde_json::from_str(&payload).expect("parses back");
    assert_eq!(parsed.name, hostile, "the string survives as data, unchanged");
    assert!(!DRIVER_SOURCE.contains("os.system"));
}

#[test]
fn the_job_payload_carries_the_socket_table_and_mode() {
    let scene = full_scene();
    let job = serde_json::json!({
        "mode": "build",
        "scene": scene,
        "socket_aliases": socket_alias_table(),
    });
    let text = serde_json::to_string_pretty(&job).expect("job serializes");
    assert!(text.contains("\"mode\": \"build\""));
    assert!(text.contains("\"socket_aliases\""));
    assert!(text.contains("Transmission Weight"));
    assert!(text.contains("\"rig\": \"orbit\""));
    assert!(text.contains("\"preset\": \"studio_three_point\""));
}

// ---------------------------------------------------------------------------
// Still measurement: real pixels decoded by ffmpeg, no Blender
// ---------------------------------------------------------------------------

#[test]
fn still_measurement_reads_luma_and_coverage_from_real_pixels() {
    let Some(ffmpeg) = proc::which("ffmpeg") else {
        eprintln!("ffmpeg not installed; skipping the pixel measurement check");
        return;
    };
    // A 160x90 image: black everywhere except a white box over a quarter of the frame.
    let dir = tempfile::tempdir().expect("temp dir");
    let png = dir.path().join("still.png");
    let status = std::process::Command::new(ffmpeg)
        .args(["-y", "-hide_banner", "-loglevel", "error", "-f", "lavfi", "-i"])
        .arg("color=c=black:s=160x90:d=1")
        .args([
            "-vf",
            "drawbox=x=40:y=22:w=80:h=45:color=white:t=fill",
            "-frames:v",
            "1",
        ])
        .arg(&png)
        .status()
        .expect("ffmpeg runs");
    assert!(status.success(), "ffmpeg could not build the fixture image");
    let stats = measure_still(&png).expect("measures the still");
    assert_eq!(stats.width, 160);
    assert_eq!(stats.height, 90);
    assert_eq!(stats.background_luma, 0.0, "the border is black");
    assert_eq!(stats.min_luma, 0);
    assert_eq!(stats.max_luma, 255);
    // The white box is 80x45 = 3600 of 14400 pixels = 25%.
    assert!(
        (stats.coverage - 0.25).abs() < 0.02,
        "coverage {:.3} should be about 0.25",
        stats.coverage
    );
    assert!(
        (stats.mean_luma - 0.25).abs() < 0.02,
        "mean luma {:.3} should be about 0.25",
        stats.mean_luma
    );
    assert!(measure_still(&dir.path().join("missing.png")).is_err());
}

#[test]
fn a_report_that_disagrees_with_the_saved_file_is_an_error() {
    let report = BuildReport {
        blend: "/tmp/scene.blend".into(),
        objects: 3,
        cameras: 1,
        lights: 3,
        vertices: 24,
        material_count: 2,
        missing_textures: Vec::new(),
        assets: Vec::new(),
        warnings: Vec::new(),
        engine: "BLENDER_EEVEE".into(),
        resolution: [800, 450],
        frame_start: 1,
        frame_end: 48,
        transparent: false,
        camera_rig: "orbit".into(),
        lighting: "studio_three_point".into(),
        bbox_min: Some([-1.0, -0.25, -0.9]),
        bbox_max: Some([1.0, 0.25, 0.9]),
    };
    let matching = VerifyReport {
        file: "/tmp/scene.blend".into(),
        objects: 3,
        cameras: 1,
        lights: 3,
        vertices: 24,
        material_count: 2,
        object_details: Vec::new(),
        engine: "BLENDER_EEVEE".into(),
        resolution: [800, 450],
        fps: 30,
        frame_start: 1,
        frame_end: 48,
        transparent: false,
        bbox_min: Some([-1.0, -0.25, -0.9]),
        bbox_max: Some([1.0, 0.25, 0.9]),
        camera_animated: true,
        blender_version: "4.0.2".into(),
    };
    check_verified(&report, &matching).expect("matching counts are accepted");

    for broken in [
        VerifyReport {
            objects: 2,
            ..matching.clone()
        },
        VerifyReport {
            cameras: 0,
            ..matching.clone()
        },
        VerifyReport {
            lights: 2,
            ..matching.clone()
        },
        VerifyReport {
            engine: "CYCLES".into(),
            ..matching.clone()
        },
        VerifyReport {
            frame_end: 96,
            ..matching.clone()
        },
        VerifyReport {
            transparent: true,
            ..matching.clone()
        },
    ] {
        assert!(
            check_verified(&report, &broken).is_err(),
            "a mismatched reopen must fail the build: {broken:?}"
        );
    }
}

// ---------------------------------------------------------------------------
// Live acceptance scenes: two real builds on this machine
// ---------------------------------------------------------------------------

fn live_enabled() -> bool {
    std::env::var("CINVAR_SCENE_LIVE")
        .map(|value| value == "1" || value.eq_ignore_ascii_case("true"))
        .unwrap_or(false)
}

fn live_dir(tag: &str) -> PathBuf {
    let root = std::env::var("CINVAR_SCENE_LIVE_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| std::env::temp_dir().join("cinvar-scene-live"));
    let dir = root.join(tag);
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("live dir");
    dir
}

fn write_logo_svg(dir: &Path) -> PathBuf {
    let path = dir.join("logo.svg");
    let svg = r##"<?xml version="1.0" encoding="UTF-8"?>
<svg xmlns="http://www.w3.org/2000/svg" width="210" height="210" viewBox="0 0 210 210">
  <path d="M 25 175 L 80 30 L 105 175 Z" fill="#4f8cff"/>
  <path d="M 115 175 L 150 85 L 190 175 Z" fill="#ffb347"/>
</svg>
"##;
    std::fs::write(&path, svg).expect("write svg");
    path
}

/// A small .glb made by this machine's Blender, so the import path runs on a real file.
fn write_glb(dir: &Path) -> PathBuf {
    let glb = dir.join("model.glb");
    if glb.is_file() {
        return glb;
    }
    let script = dir.join("make_glb.py");
    let body = format!(
        r#"import bpy

bpy.ops.wm.read_factory_settings(use_empty=True)
bpy.ops.mesh.primitive_torus_add(major_radius=0.6, minor_radius=0.2, major_segments=48, minor_segments=16)
torus = bpy.context.active_object
torus.name = "TORUS_BODY"
material = bpy.data.materials.new("GLB_WHITE")
material.use_nodes = True
torus.data.materials.append(material)
bpy.ops.mesh.primitive_cube_add(size=0.7, location=(0.0, 0.0, 0.9))
cube = bpy.context.active_object
cube.name = "CAP"
cube.data.materials.append(material)
bpy.ops.export_scene.gltf(filepath=r"{glb}", export_format="GLB")
print("WROTE", r"{glb}")
"#,
        glb = glb.display()
    );
    std::fs::write(&script, body).expect("write glb generator");
    let info = detect().expect("Blender is installed");
    let output = std::process::Command::new(&info.path)
        .arg("-b")
        .arg("--factory-startup")
        .arg("--python")
        .arg(&script)
        .output()
        .expect("blender runs");
    assert!(
        output.status.success() && glb.is_file(),
        "could not generate a .glb fixture: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    glb
}

#[test]
fn live_svg_logo_orbit_studio_scene() {
    if !live_enabled() {
        eprintln!("CINVAR_SCENE_LIVE is not set; skipping the live SVG logo build");
        return;
    }
    let dir = live_dir("svg-logo");
    let svg = write_logo_svg(&dir);
    let scene = CinvarBlenderScene::new("SVG_LOGO")
        // No material override: the SVG's own fills must survive the import and the extrusion.
        .asset(AssetImport::new("logo", &svg).fit(2.0).extrude(0.25))
        .camera(CameraRig::orbit(Some([0.0, 0.0, 0.0]), 5.5, 12.0, -30.0, 30.0).with_lens(50.0))
        .lighting(Lighting::studio_three_point().with_key_intensity(900.0))
        .render(
            RenderSettings::size(800, 450)
                .engine(RenderEngine::Eevee)
                .samples(40)
                .frames(1, 48),
        );

    let build = build(&scene, &dir).expect("the SVG logo scene builds");
    println!(
        "SVG build: objects {} cameras {} lights {} vertices {} -> {}",
        build.objects,
        build.cameras,
        build.lights,
        build.vertices,
        build.blend.display()
    );
    assert!(build.blend.is_file(), ".blend must exist");
    assert_eq!(build.cameras, 1);
    assert_eq!(build.lights, 3, "studio_three_point is key + fill + rim");
    assert!(build.objects >= 1, "the extruded logo must be geometry");
    assert!(build.verify.camera_animated, "the orbit rig must be keyframed");
    assert!(
        build.engine.contains("EEVEE"),
        "the requested EEVEE engine must resolve on this Blender, got {}",
        build.engine
    );
    assert_eq!(build.objects, build.verify.objects);
    assert_eq!(build.cameras, build.verify.cameras);
    assert_eq!(build.lights, build.verify.lights);

    // Real geometry, not merely "it did not error".
    let logo = build
        .asset_objects("logo")
        .into_iter()
        .find(|detail| detail.has_geometry())
        .expect("the extruded SVG must produce vertices and a non-zero bounding box");
    println!(
        "SVG geometry: {} vertices, {} triangles, dimensions {:?}",
        logo.vertices, logo.triangles, logo.dimensions
    );
    assert!(logo.vertices > 0);
    assert!(
        !logo.materials.is_empty(),
        "the SVG fill must survive as a material reference"
    );
    assert!(
        build.material_count >= 2,
        "both SVG fills must survive the import, got {} materials",
        build.material_count
    );
    let dims = logo.dimensions.expect("dimensions measured");
    assert!(dims.iter().all(|value| value.is_finite()));
    // An extruded path has real volume: every axis has extent, and the thin one is the depth.
    let thinnest = dims.iter().cloned().fold(f64::INFINITY, f64::min);
    assert!(
        thinnest > 0.2,
        "the extruded path must have thickness, thinnest axis {thinnest}"
    );
    // fit_size applies to the whole imported set, so the two marks together span 2.0 m.
    let low = build.bbox_min.expect("scene bbox measured");
    let high = build.bbox_max.expect("scene bbox measured");
    let span = (0..3).map(|axis| high[axis] - low[axis]).fold(0.0f64, f64::max);
    assert!(
        (span - 2.0).abs() < 0.25,
        "fit_size 2.0 must size the logo set, measured span {span}"
    );
    let marks: Vec<&ObjectMeasure> = build
        .asset_objects("logo")
        .into_iter()
        .filter(|detail| detail.has_geometry())
        .collect();
    assert_eq!(marks.len(), 2, "both SVG paths must become geometry");

    let still = render_still(&build, Some(24)).expect("the logo still renders");
    println!(
        "SVG still: {}x{} engine {} mean luma {:.4} background {:.4} coverage {:.4} range {}-{}",
        still.width,
        still.height,
        still.engine,
        still.mean_luma,
        still.background_luma,
        still.coverage,
        still.min_luma,
        still.max_luma
    );
    assert!(still.path.is_file());
    assert!(
        still.mean_luma > 0.01 && still.mean_luma < 0.99,
        "the frame is neither black nor white"
    );
    assert!(still.coverage > 0.02, "the subject must be visible in frame");
    assert!(
        still.max_luma > 200,
        "the key light produces highlights, got {}",
        still.max_luma
    );

    // The rig really animates: the arc changes the framing between the first and last frame.
    let opening = render_still(&build, Some(1)).expect("the opening frame renders");
    let closing = render_still(&build, Some(48)).expect("the closing frame renders");
    println!(
        "SVG motion: frame 1 centroid ({:.4}, {:.4}) coverage {:.4} -> frame 48 centroid ({:.4}, {:.4}) coverage {:.4}",
        opening.centroid_x,
        opening.centroid_y,
        opening.coverage,
        closing.centroid_x,
        closing.centroid_y,
        closing.coverage
    );
    // The arc sweeps -30 to +30 degrees: the subject's weight must move across the frame.
    assert!(
        (opening.centroid_x - closing.centroid_x).abs() > 0.005,
        "the orbit must move the subject across the frame: {:.4} vs {:.4}",
        opening.centroid_x,
        closing.centroid_x
    );
}

#[test]
fn live_product_shot_isometric_cyclorama_with_glb() {
    if !live_enabled() {
        eprintln!("CINVAR_SCENE_LIVE is not set; skipping the live product build");
        return;
    }
    let dir = live_dir("product");
    let glb = write_glb(&dir);
    let scene = CinvarBlenderScene::new("PRODUCT_SHOT")
        .material(MaterialSpec::new("CERAMIC", "#f3f1ec").roughness(0.35))
        .material(MaterialSpec::new("CHROME", "#c9ced6").metallic(1.0).roughness(0.2))
        .asset(AssetImport::new("product", &glb).fit(2.2).at([0.0, 0.0, 1.3]).material("CERAMIC"))
        .object(
            SceneObject::new("PLINTH", ObjectKind::cylinder(0.85, 0.3))
                .transform(Transform::at([0.0, 0.0, 0.15]))
                .material("CHROME"),
        )
        .camera(CameraRig::isometric_turntable(Some([0.0, 0.0, 1.2]), 0.18).with_turns(1.0))
        .lighting(Lighting::product_cyclorama().with_backdrop_scale(7.0))
        .render(
            RenderSettings::size(800, 450)
                .engine(RenderEngine::Eevee)
                .samples(40)
                .frames(1, 96),
        );

    let build = build(&scene, &dir).expect("the product scene builds");
    println!(
        "PRODUCT build: objects {} cameras {} lights {} vertices {} -> {}",
        build.objects,
        build.cameras,
        build.lights,
        build.vertices,
        build.blend.display()
    );
    assert_eq!(build.cameras, 1);
    assert_eq!(build.lights, 3, "product_cyclorama is key + fill + rim");
    assert_eq!(build.camera_rig, "isometric_turntable");
    assert_eq!(build.lighting, "product_cyclorama");
    assert!(build.objects >= 3, "glb parts, the plinth and the sweep are all geometry");
    assert!(build.verify.camera_animated);
    assert_eq!(build.objects, build.verify.objects);
    assert_eq!(build.cameras, build.verify.cameras);
    assert_eq!(build.lights, build.verify.lights);

    let imported = build.asset_objects("product");
    assert!(!imported.is_empty(), "the .glb must import objects");
    let measured = imported
        .iter()
        .find(|detail| detail.has_geometry())
        .expect("the imported .glb must carry real geometry");
    println!(
        "GLB geometry: {} vertices, dimensions {:?}, materials {:?}",
        measured.vertices, measured.dimensions, measured.materials
    );
    assert!(measured.vertices > 0);

    let still = render_still(&build, Some(48)).expect("the product still renders");
    println!(
        "PRODUCT still: {}x{} engine {} mean luma {:.4} background {:.4} coverage {:.4} range {}-{}",
        still.width,
        still.height,
        still.engine,
        still.mean_luma,
        still.background_luma,
        still.coverage,
        still.min_luma,
        still.max_luma
    );
    assert!(still.path.is_file());
    assert!(still.mean_luma > 0.01 && still.mean_luma < 0.99);
    assert!(still.coverage > 0.02, "the product must be visible in frame");
}
