use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::{Error, ErrorCode, Result};
use crate::util::paths;
use crate::util::proc::{self, ProcOpts};
use crate::util::time::Time;

const DEFAULT_ACCENT: (u8, u8, u8) = (0x4f, 0x8c, 0xff);
const MAX_STDERR_LINES: usize = 20;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnimationSpec {
    pub template: String,
    pub text: String,
    pub duration: Time,
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    pub color: String,
}

impl Default for AnimationSpec {
    fn default() -> Self {
        AnimationSpec {
            template: "logo_reveal".into(),
            text: "CINVAR".into(),
            duration: Time::from_secs_f64(5.0),
            width: 1920,
            height: 1080,
            fps: 30,
            color: "#4f8cff".into(),
        }
    }
}

impl AnimationSpec {
    /// Frame span used by the script, the encoder and the timeout estimate.
    fn frame_start(&self) -> u32 {
        1
    }

    fn frame_end(&self) -> u32 {
        let fps = self.fps.max(1) as f64;
        let frames = (self.duration.as_secs_f64() * fps).round() as i64;
        frames.clamp(2, 100_000) as u32
    }

    fn canvas(&self) -> (u32, u32) {
        let even = |v: u32| ((v.max(64)) / 2) * 2;
        (even(self.width), even(self.height))
    }

    fn frame_count(&self) -> u32 {
        self.frame_end().saturating_sub(self.frame_start()) + 1
    }
}

pub fn available() -> Option<PathBuf> {
    if let Ok(configured) = std::env::var("CINVAR_BLENDER") {
        let trimmed = configured.trim();
        if !trimmed.is_empty() {
            let candidate = paths::expand_tilde(trimmed);
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    proc::which("blender")
}

fn hex_pair(input: &str, at: usize) -> Option<u8> {
    u8::from_str_radix(input.get(at..at + 2)?, 16).ok()
}

fn parse_color(spec: &str) -> (u8, u8, u8) {
    let trimmed = spec.trim().trim_start_matches('#');
    match trimmed.len() {
        6 => match (
            hex_pair(trimmed, 0),
            hex_pair(trimmed, 2),
            hex_pair(trimmed, 4),
        ) {
            (Some(r), Some(g), Some(b)) => (r, g, b),
            _ => DEFAULT_ACCENT,
        },
        3 => {
            let expand = |c: Option<char>| -> Option<u8> {
                let c = c?;
                hex_pair(&format!("{c}{c}"), 0)
            };
            let mut chars = trimmed.chars();
            match (
                expand(chars.next()),
                expand(chars.next()),
                expand(chars.next()),
            ) {
                (Some(r), Some(g), Some(b)) => (r, g, b),
                _ => DEFAULT_ACCENT,
            }
        }
        _ => DEFAULT_ACCENT,
    }
}

fn srgb_to_linear(channel: u8) -> f64 {
    let c = channel as f64 / 255.0;
    if c <= 0.04045 {
        c / 12.92
    } else {
        ((c + 0.055) / 1.055).powf(2.4)
    }
}

fn rgba_literal(rgb: (u8, u8, u8)) -> String {
    format!(
        "({:.4}, {:.4}, {:.4}, 1.0)",
        srgb_to_linear(rgb.0),
        srgb_to_linear(rgb.1),
        srgb_to_linear(rgb.2)
    )
}

/// Escapes text for a double-quoted Python string literal.
fn py_string(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 2);
    out.push('"');
    for ch in text.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// Builds the Blender Python script for a spec. Kept public and side-effect free
/// so the generated Python can be unit-tested without Blender installed.
pub fn script_for(spec: &AnimationSpec) -> String {
    let (width, height) = spec.canvas();
    let rgb = parse_color(&spec.color);
    let template = if spec.template.trim() == "title_3d" {
        "title_3d"
    } else {
        "logo_reveal"
    };
    let mut out = String::new();
    out.push_str("# CINVAR Blender animation — generated automatically, edits are overwritten.\n");
    out.push_str("import math\nimport os\n\n");
    out.push_str("import bpy\n\n");
    out.push_str(
        "FRAMES_DIR = os.path.join(os.path.dirname(os.path.abspath(__file__)), \"frames\")\n",
    );
    out.push_str(&format!("TEMPLATE = {}\n", py_string(template)));
    out.push_str(&format!("TEXT = {}\n", py_string(&spec.text)));
    out.push_str(&format!("REQUESTED_COLOR = {}\n", py_string(&spec.color)));
    out.push_str(&format!(
        "COLOR_RGBA = {}  # linearised #{:02x}{:02x}{:02x}\n\n",
        rgba_literal(rgb),
        rgb.0,
        rgb.1,
        rgb.2
    ));
    out.push_str(&format!("FPS = {}\n", spec.fps.max(1)));
    out.push_str(&format!("RES_X = {width}\nRES_Y = {height}\n"));
    out.push_str(&format!("FRAME_START = {}\n", spec.frame_start()));
    out.push_str(&format!("FRAME_END = {}\n\n", spec.frame_end()));

    out.push_str(
        r#"def reset_scene():
    """Clear the factory scene so the clip contains only what CINVAR adds."""
    bpy.ops.wm.read_factory_settings(use_empty=True)


def make_emissive_material(name, rgba):
    mat = bpy.data.materials.new(name=name)
    mat.use_nodes = True
    bsdf = mat.node_tree.nodes.get("Principled BSDF")
    if bsdf is not None:
        inputs = bsdf.inputs
        if "Base Color" in inputs:
            inputs["Base Color"].default_value = rgba
        # Blender 4.x renamed the emission socket; support both names.
        if "Emission Color" in inputs:
            inputs["Emission Color"].default_value = rgba
        elif "Emission" in inputs:
            inputs["Emission"].default_value = rgba
        if "Emission Strength" in inputs:
            inputs["Emission Strength"].default_value = 3.0
        if "Roughness" in inputs:
            inputs["Roughness"].default_value = 0.3
    return mat


def add_text(body, material):
    bpy.ops.object.text_add(location=(0.0, 0.0, 0.0))
    obj = bpy.context.active_object
    obj.name = "CINVAR_TEXT"
    obj.data.body = body
    obj.data.align_x = "CENTER"
    obj.data.align_y = "CENTER"
    obj.data.extrude = 0.05
    obj.data.bevel_depth = 0.01
    obj.data.materials.append(material)
    obj.rotation_euler = (math.radians(90.0), 0.0, 0.0)
    return obj


def add_camera(distance):
    bpy.ops.object.camera_add(
        location=(0.0, -distance, 0.0),
        rotation=(math.radians(90.0), 0.0, 0.0),
    )
    cam = bpy.context.active_object
    cam.data.lens = 50.0
    bpy.context.scene.camera = cam
    return cam


def add_lights():
    bpy.ops.object.light_add(type="AREA", location=(4.0, -6.0, 5.0))
    key = bpy.context.active_object
    key.data.energy = 900.0
    key.data.size = 6.0
    bpy.ops.object.light_add(type="SUN", location=(-5.0, -4.0, 3.0))
    fill = bpy.context.active_object
    fill.data.energy = 2.5
    bpy.ops.object.light_add(type="AREA", location=(0.0, 6.0, 2.0))
    rim = bpy.context.active_object
    rim.data.energy = 400.0


def configure_render():
    scene = bpy.context.scene
    scene.render.resolution_x = RES_X
    scene.render.resolution_y = RES_Y
    scene.render.resolution_percentage = 100
    scene.render.fps = FPS
    scene.render.image_settings.file_format = "PNG"
    # Resume support: frames already on disk are skipped (CINVAR validates them first).
    scene.render.use_overwrite = False
    scene.render.use_placeholder = False
    scene.render.filepath = os.path.join(FRAMES_DIR, "frame_")
    scene.frame_start = FRAME_START
    scene.frame_end = FRAME_END
    if not os.path.isdir(FRAMES_DIR):
        os.makedirs(FRAMES_DIR)
    # Explicit world: a lit deep-navy backdrop, never pure black (edge-on frames of the
    # rotating title would otherwise be indistinguishable from a dropped/black frame).
    world = bpy.data.worlds.new("CinvarWorld")
    scene.world = world
    world.use_nodes = True
    background = world.node_tree.nodes.get("Background")
    if background is not None:
        background.inputs[0].default_value = (0.05, 0.07, 0.14, 1.0)
        background.inputs[1].default_value = 1.0
    return scene


def animate_logo_reveal(obj, cam):
    span = FRAME_END - FRAME_START
    settle = FRAME_START + int(span * 0.6)
    obj.scale = (0.01, 0.01, 0.01)
    obj.location = (0.0, 0.0, -0.4)
    obj.keyframe_insert(data_path="scale", frame=FRAME_START)
    obj.keyframe_insert(data_path="location", frame=FRAME_START)
    obj.scale = (1.0, 1.0, 1.0)
    obj.location = (0.0, 0.0, 0.0)
    obj.keyframe_insert(data_path="scale", frame=settle)
    obj.keyframe_insert(data_path="location", frame=settle)
    # camera dolly: pull back from a tight framing on the same curve
    cam.location = (0.0, -13.0, 0.0)
    cam.keyframe_insert(data_path="location", frame=FRAME_START)
    cam.location = (0.0, -8.0, 0.0)
    cam.keyframe_insert(data_path="location", frame=settle)


def animate_title_3d(obj, cam):
    mid = FRAME_START + int((FRAME_END - FRAME_START) * 0.5)
    obj.scale = (0.85, 0.85, 0.85)
    obj.keyframe_insert(data_path="scale", frame=FRAME_START)
    obj.scale = (1.0, 1.0, 1.0)
    obj.keyframe_insert(data_path="scale", frame=mid)
    # full turn around the vertical axis (euler Z is applied last -> world up)
    obj.rotation_euler = (math.radians(90.0), 0.0, 0.0)
    obj.keyframe_insert(data_path="rotation_euler", frame=FRAME_START)
    obj.rotation_euler = (math.radians(90.0), 0.0, math.pi)
    obj.keyframe_insert(data_path="rotation_euler", frame=mid)
    obj.rotation_euler = (math.radians(90.0), 0.0, 2.0 * math.pi)
    obj.keyframe_insert(data_path="rotation_euler", frame=FRAME_END)


def main():
    reset_scene()
    configure_render()
    material = make_emissive_material("CINVAR_ACCENT", COLOR_RGBA)
    obj = add_text(TEXT, material)
    cam = add_camera(8.0)
    add_lights()
    if TEMPLATE == "title_3d":
        animate_title_3d(obj, cam)
    else:
        animate_logo_reveal(obj, cam)
    bpy.ops.render.render(animation=True)
    print("CINVAR_BLENDER_DONE frames=%d color=%s" % (FRAME_END - FRAME_START + 1, REQUESTED_COLOR))


main()
"#,
    );
    out
}

fn render_error(message: String, stderr: String) -> Error {
    Error::new(
        ErrorCode::Render,
        format!("{message}: {}", crate::secrets::redact(&stderr)),
    )
    .with_hint(
        "check that Blender can render headless on this machine (blender -b --factory-startup)",
    )
}

fn count_frames(frames: &Path) -> usize {
    let Ok(entries) = std::fs::read_dir(frames) else {
        return 0;
    };
    entries
        .filter_map(|e| e.ok())
        .filter(|e| e.path().extension().and_then(|x| x.to_str()) == Some("png"))
        .count()
}

/// Minimum Blender version CINVAR's generated scripts are written for.
pub const MIN_BLENDER: (u32, u32) = (3, 6);

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct BlenderInfo {
    pub path: PathBuf,
    pub version: String,
    pub major: u32,
    pub minor: u32,
    pub compatible: bool,
}

/// Parses the first line of `blender --version` ("Blender 5.3.0").
pub fn parse_version(output: &str) -> Option<(u32, u32, String)> {
    let line = output
        .lines()
        .find(|l| l.trim_start().starts_with("Blender "))?;
    let version = line
        .trim()
        .trim_start_matches("Blender ")
        .trim()
        .to_string();
    let mut parts = version.split(|c: char| !c.is_ascii_digit());
    let major: u32 = parts.next()?.parse().ok()?;
    let minor: u32 = parts.next()?.parse().ok()?;
    Some((major, minor, version))
}

/// Runs the real executable (`--version`) and reports whether it satisfies [`MIN_BLENDER`].
pub fn inspect(path: &Path) -> Result<BlenderInfo> {
    let out = proc::run(
        path,
        &["--version".to_string()],
        &ProcOpts::default().with_timeout(60),
    )?;
    let (major, minor, version) = parse_version(&out.stdout).ok_or_else(|| {
        Error::new(
            ErrorCode::DependencyMissing,
            format!("{} did not report a Blender version", path.display()),
        )
    })?;
    Ok(BlenderInfo {
        path: path.to_path_buf(),
        version,
        major,
        minor,
        compatible: (major, minor) >= MIN_BLENDER,
    })
}

/// Detect an installed Blender and verify it is new enough.
pub fn detect() -> Result<BlenderInfo> {
    let path = available().ok_or_else(|| {
        Error::new(ErrorCode::DependencyMissing, "Blender is not installed or not detected")
            .with_hint("install Blender 3.6+ from https://www.blender.org/download/ or point CINVAR_BLENDER at the blender executable")
    })?;
    let info = inspect(&path)?;
    if !info.compatible {
        return Err(Error::new(
            ErrorCode::Unsupported,
            format!(
                "Blender {} is older than the required {}.{}",
                info.version, MIN_BLENDER.0, MIN_BLENDER.1
            ),
        ));
    }
    Ok(info)
}

/// Stable identity of a render request: the same spec always maps to the same job directory,
/// which is what makes interrupted renders resumable and duplicate renders free.
pub fn spec_hash(spec: &AnimationSpec) -> String {
    use sha2::{Digest, Sha256};
    let canonical = serde_json::to_string(spec).unwrap_or_default();
    let digest = Sha256::digest(canonical.as_bytes());
    digest.iter().take(8).map(|b| format!("{b:02x}")).collect()
}

pub fn job_dir(base: &Path, spec: &AnimationSpec) -> PathBuf {
    base.join(spec_hash(spec))
}

/// A PNG is complete only if it carries the 8-byte signature and ends with the IEND chunk.
fn png_complete(path: &Path) -> bool {
    use std::io::{Read, Seek, SeekFrom};
    let Ok(mut file) = std::fs::File::open(path) else {
        return false;
    };
    let mut head = [0u8; 8];
    if file.read_exact(&mut head).is_err()
        || head != [0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a]
    {
        return false;
    }
    let mut tail = [0u8; 12];
    if file.seek(SeekFrom::End(-12)).is_err() || file.read_exact(&mut tail).is_err() {
        return false;
    }
    &tail[4..8] == b"IEND"
}

/// Removes truncated frames left by an interrupted render; returns how many valid frames remain.
fn prune_invalid_frames(frames: &Path) -> usize {
    let Ok(entries) = std::fs::read_dir(frames) else {
        return 0;
    };
    let mut valid = 0;
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("png") {
            continue;
        }
        if png_complete(&path) {
            valid += 1;
        } else {
            let _ = std::fs::remove_file(&path);
        }
    }
    valid
}

/// Hooks for a long-running render: progress `(done, total)` and cooperative cancellation.
pub struct RenderControl<'a> {
    pub progress: &'a dyn Fn(usize, usize),
    pub cancelled: &'a dyn Fn() -> bool,
}

impl RenderControl<'_> {
    pub fn none() -> RenderControl<'static> {
        RenderControl {
            progress: &|_, _| {},
            cancelled: &|| false,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct BlenderRender {
    pub clip: PathBuf,
    pub frames: usize,
    pub resumed_frames: usize,
    pub blender_version: String,
    pub job_dir: PathBuf,
}

pub fn render_animation(spec: &AnimationSpec, out_dir: &Path) -> Result<PathBuf> {
    render_animation_job(spec, out_dir, &RenderControl::none()).map(|r| r.clip)
}

/// Renders `spec` into `<base>/<spec-hash>/`, resuming from valid frames already there,
/// reporting progress by counting finished PNGs and honouring cancellation (the Blender
/// process is killed; completed frames stay for the next attempt).
pub fn render_animation_job(
    spec: &AnimationSpec,
    base: &Path,
    ctl: &RenderControl,
) -> Result<BlenderRender> {
    let info = detect()?;
    let ffmpeg = proc::require("ffmpeg")?;
    let out_dir = job_dir(base, spec);
    paths::ensure_dir(&out_dir)?;
    let frames_dir = out_dir.join("frames");
    paths::ensure_dir(&frames_dir)?;
    let total = spec.frame_count() as usize;
    let resumed = prune_invalid_frames(&frames_dir);

    let script = script_for(spec);
    let script_path = out_dir.join("animation.py");
    paths::write_atomic(&script_path, script.as_bytes())?;

    if resumed < total {
        let timeout = std::time::Duration::from_secs(
            180u64
                .saturating_add(spec.frame_count() as u64 * 2)
                .min(3600),
        );
        let log_path = out_dir.join("blender.log");
        let log = std::fs::File::create(&log_path)?;
        let mut cmd = std::process::Command::new(&info.path);
        cmd.args(["-b", "--factory-startup", "--python"])
            .arg(&script_path)
            .current_dir(&out_dir)
            .stdin(std::process::Stdio::null())
            .stdout(log.try_clone()?)
            .stderr(log);
        let mut child = proc::spawn_retrying(&mut cmd)?;
        let started = std::time::Instant::now();
        let status = loop {
            if let Some(status) = child.try_wait()? {
                break status;
            }
            if (ctl.cancelled)() {
                let _ = child.kill();
                let _ = child.wait();
                return Err(Error::new(ErrorCode::Cancelled, "Blender render cancelled")
                    .with_hint("completed frames are kept; re-run the same request to resume"));
            }
            if started.elapsed() > timeout {
                let _ = child.kill();
                let _ = child.wait();
                return Err(render_error(
                    format!("Blender timed out after {}s", timeout.as_secs()),
                    tail_of(&log_path),
                ));
            }
            (ctl.progress)(count_frames(&frames_dir), total);
            std::thread::sleep(std::time::Duration::from_millis(150));
        };
        if !status.success() {
            return Err(render_error(
                format!("Blender exited with status {:?}", status.code()),
                tail_of(&log_path),
            ));
        }
    }

    let done = prune_invalid_frames(&frames_dir);
    (ctl.progress)(done, total);
    if done < total {
        return Err(render_error(
            format!("Blender produced {done} of {total} frames"),
            tail_of(&out_dir.join("blender.log")),
        ));
    }

    let clip = out_dir.join("clip.mp4");
    let sequence = frames_dir.join("frame_%04d.png");
    let encode_args = vec![
        "-y".to_string(),
        "-hide_banner".to_string(),
        "-nostdin".to_string(),
        "-framerate".to_string(),
        spec.fps.max(1).to_string(),
        "-start_number".to_string(),
        spec.frame_start().to_string(),
        "-i".to_string(),
        sequence.to_string_lossy().to_string(),
        "-c:v".to_string(),
        "libx264".to_string(),
        "-pix_fmt".to_string(),
        "yuv420p".to_string(),
        "-crf".to_string(),
        "18".to_string(),
        "-movflags".to_string(),
        "+faststart".to_string(),
        clip.to_string_lossy().to_string(),
    ];
    let encoded = proc::run(
        &ffmpeg,
        &encode_args,
        &ProcOpts::default().in_dir(&out_dir).with_timeout(600),
    )?;
    if !encoded.success {
        return Err(render_error(
            format!("ffmpeg could not encode the {done} frames"),
            encoded.tail_stderr(MAX_STDERR_LINES),
        ));
    }
    if std::fs::metadata(&clip).map(|m| m.len()).unwrap_or(0) == 0 {
        return Err(Error::new(
            ErrorCode::Render,
            format!("encoded clip is empty: {}", clip.display()),
        )
        .with_hint("check the frames directory for PNG output"));
    }
    Ok(BlenderRender {
        clip,
        frames: done,
        resumed_frames: resumed.min(total),
        blender_version: info.version,
        job_dir: out_dir,
    })
}

fn tail_of(log: &Path) -> String {
    let text = std::fs::read_to_string(log).unwrap_or_default();
    let lines: Vec<&str> = text.lines().collect();
    lines[lines.len().saturating_sub(MAX_STDERR_LINES)..].join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_blender_version_output() {
        let out = "Blender 5.3.0\n\tbuild date: 2026-11-10\n";
        assert_eq!(parse_version(out), Some((5, 3, "5.3.0".to_string())));
        assert_eq!(parse_version("Blender 3.6.23 LTS\n").unwrap().0, 3);
        assert_eq!(parse_version("not blender"), None);
    }

    #[test]
    fn spec_hash_is_stable_and_sensitive() {
        let a = AnimationSpec::default();
        let mut b = AnimationSpec::default();
        assert_eq!(spec_hash(&a), spec_hash(&b));
        b.text = "OTHER".into();
        assert_ne!(spec_hash(&a), spec_hash(&b));
    }

    #[test]
    fn truncated_png_is_rejected_and_pruned() {
        let dir = tempfile::tempdir().unwrap();
        let good = dir.path().join("frame_0001.png");
        let mut bytes = vec![0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];
        bytes.extend_from_slice(&[0, 0, 0, 0, b'I', b'E', b'N', b'D', 0xae, 0x42, 0x60, 0x82]);
        std::fs::write(&good, &bytes).unwrap();
        let bad = dir.path().join("frame_0002.png");
        std::fs::write(&bad, &bytes[..9]).unwrap();
        assert_eq!(prune_invalid_frames(dir.path()), 1);
        assert!(good.exists() && !bad.exists());
    }

    #[test]
    fn parses_hex_colours() {
        assert_eq!(parse_color("#4f8cff"), (0x4f, 0x8c, 0xff));
        assert_eq!(parse_color("4f8cff"), (0x4f, 0x8c, 0xff));
        assert_eq!(parse_color("#fff"), (0xff, 0xff, 0xff));
        assert_eq!(parse_color("nonsense"), DEFAULT_ACCENT);
    }

    #[test]
    fn linearises_srgb() {
        assert!((srgb_to_linear(0) - 0.0).abs() < 1e-9);
        assert!((srgb_to_linear(255) - 1.0).abs() < 1e-9);
        let mid = srgb_to_linear(128);
        assert!(mid > 0.2 && mid < 0.23, "mid grey was {mid}");
    }

    #[test]
    fn escapes_python_strings() {
        assert_eq!(py_string("a\"b\\c"), "\"a\\\"b\\\\c\"");
        assert_eq!(py_string("line\nbreak"), "\"line\\nbreak\"");
    }

    #[test]
    fn script_has_scene_camera_and_material_calls() {
        let script = script_for(&AnimationSpec::default());
        for needle in [
            "bpy.ops.wm.read_factory_settings(use_empty=True)",
            "bpy.ops.object.text_add",
            "bpy.data.materials.new",
            "Emission Strength",
            "bpy.ops.object.camera_add",
            "bpy.ops.object.light_add",
            "keyframe_insert",
            "scene.render.resolution_x = RES_X",
            "bpy.ops.render.render(animation=True)",
        ] {
            assert!(
                script.contains(needle),
                "missing {needle} in generated script"
            );
        }
        assert!(script.contains("TEMPLATE = \"logo_reveal\""));
        assert!(script.contains("FRAME_END = 150"));
        assert!(script.contains("RES_X = 1920"));
    }

    #[test]
    fn title_template_rotates_and_reveal_scales() {
        let spec = AnimationSpec {
            template: "title_3d".into(),
            ..AnimationSpec::default()
        };
        let script = script_for(&spec);
        assert!(script.contains("TEMPLATE = \"title_3d\""));
        assert!(script.contains("def animate_title_3d"));
        assert!(script.contains("2.0 * math.pi"));
        let reveal = script_for(&AnimationSpec::default());
        assert!(reveal.contains("obj.scale = (0.01, 0.01, 0.01)"));
        assert!(reveal.contains("cam.location = (0.0, -8.0, 0.0)"));
    }

    #[test]
    fn frame_span_follows_duration() {
        let spec = AnimationSpec {
            duration: Time::from_secs_f64(1.5),
            fps: 24,
            ..AnimationSpec::default()
        };
        assert_eq!(spec.frame_end(), 36);
        assert_eq!(spec.frame_count(), 36);
    }

    #[test]
    fn canvas_is_even_sized() {
        let spec = AnimationSpec {
            width: 1081,
            height: 1081,
            ..AnimationSpec::default()
        };
        assert_eq!(spec.canvas(), (1080, 1080));
    }

    #[test]
    fn available_honours_env_override() {
        let dir = tempfile::tempdir().unwrap();
        let fake = dir.path().join("blender");
        std::fs::write(&fake, b"#!/bin/sh\n").unwrap();
        let key = "CINVAR_BLENDER";
        let previous = std::env::var(key).ok();
        std::env::set_var(key, fake.to_string_lossy().to_string());
        assert_eq!(available(), Some(fake));
        match previous {
            Some(value) => std::env::set_var(key, value),
            None => std::env::remove_var(key),
        }
    }

    #[test]
    fn missing_blender_reports_dependency() {
        if available().is_some() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let err = render_animation(&AnimationSpec::default(), dir.path()).unwrap_err();
        assert_eq!(err.code, ErrorCode::DependencyMissing);
    }
}
