//! Blender MCP tools (Argon Phase 6).
//!
//! Edition gating is applied centrally by `crate::tools::dispatch` through
//! `crate::edition::tool_feature`, so each handler only does its job.
//!
//! Registered here: `cinvar.blender.preview_check` (Free) and `cinvar.blender.render_job`
//! (Pro). `inspect`, `scene_build` and `mockup_screen` are registered once the declarative
//! scene and mockup modules land; they are deliberately absent rather than stubbed.

use serde_json::{json, Value};

use crate::error::{Error, Result};
use crate::tools::{Registry, ToolCtx, ToolSpec};

fn s(desc: &str) -> Value {
    json!({"type": "string", "description": desc})
}
fn i(desc: &str) -> Value {
    json!({"type": "integer", "description": desc})
}
fn ts(desc: &str) -> Value {
    json!({"type": "number", "description": desc})
}
fn schema(props: Value, required: &[&str]) -> Value {
    json!({"type": "object", "properties": props, "required": required, "additionalProperties": false})
}
fn spec(name: &str, description: &str, input: Value) -> ToolSpec {
    ToolSpec {
        name: name.to_string(),
        group: "blender".to_string(),
        description: description.to_string(),
        input,
        destructive: false,
    }
}

fn require_blender(ctx: &ToolCtx) -> Result<()> {
    if !ctx.config.security.allow_blender {
        return Err(Error::new(
            crate::error::ErrorCode::PermissionDenied,
            "Blender integration is disabled in configuration",
        )
        .with_hint("set security.allow_blender = true in ~/.config/cinvar/config.toml"));
    }
    if crate::blender::available().is_none() {
        return Err(Error::not_found("Blender is not installed")
            .with_hint("install Blender 3.6 or newer"));
    }
    Ok(())
}

/// A .blend path that exists and stays inside the folders this agent may read.
fn scene_path(ctx: &ToolCtx, raw: &str) -> Result<std::path::PathBuf> {
    let path = crate::util::paths::absolutize(std::path::Path::new(raw), &ctx.store.root);
    if !path.is_file() {
        return Err(Error::invalid(format!("not a file: {}", path.display())));
    }
    let real = path
        .canonicalize()
        .map_err(|_| Error::not_found(format!("cannot read {}", path.display())))?;
    let mut allowed: Vec<std::path::PathBuf> = ctx.config.security.effective_media_roots();
    allowed.push(ctx.store.root.clone());
    let in_allowed = allowed
        .iter()
        .filter_map(|r| r.canonicalize().ok())
        .any(|r| real.starts_with(r));
    if !in_allowed {
        return Err(Error::new(
            crate::error::ErrorCode::PermissionDenied,
            format!("outside the folders this agent may read: {}", path.display()),
        ));
    }
    if path.extension().map(|e| e.eq_ignore_ascii_case("blend")) != Some(true) {
        return Err(Error::invalid(
            "this tool reads .blend scenes; import .glb/.fbx/.obj/.svg into a scene first",
        ));
    }
    Ok(path)
}

pub fn register_blender(registry: &mut Registry) {
    registry.add(
        spec(
            "cinvar.blender.preview_check",
            "Diagnose a Blender scene before an expensive render. Renders a four-view contact sheet (active camera, top ortho, side ortho, wireframe) and returns structured spatial telemetry: per-object percentage inside the camera frame, off-screen objects, mesh intersections with BVH collision counts, floor penetration, occlusion, luminance histogram, over/under-exposure, alpha coverage and empty-frame detection. Every finding carries an actionable correction, so a scene can be fixed before any full animation render.",
            schema(
                json!({
                    "path": s("Path to a .blend file"),
                    "out_dir": s("Directory for the contact sheet and diagnostics JSON (default: the project folder)"),
                    "frame": s("first | middle | final, or an exact frame number (default: middle)"),
                    "size": i("Contact sheet size in pixels (default 1024)")
                }),
                &["path"],
            ),
        ),
        |ctx: &mut ToolCtx, args: Value| {
            require_blender(ctx)?;
            let raw = args
                .get("path")
                .and_then(|v| v.as_str())
                .ok_or_else(|| Error::invalid("path is required"))?;
            let path = scene_path(ctx, raw)?;
            let out_dir = match args.get("out_dir").and_then(|v| v.as_str()) {
                Some(d) => std::path::PathBuf::from(d),
                None => ctx.store.root.clone(),
            };
            let mut req = crate::blender::preview::PreviewRequest::new(&path, &out_dir);
            if let Some(size) = args.get("size").and_then(|v| v.as_u64()) {
                req = req.size(size.clamp(256, 4096) as u32);
            }
            req = req.frame(match args.get("frame").and_then(|v| v.as_str()) {
                Some("first") => crate::blender::preview::FrameChoice::First,
                Some("final") => crate::blender::preview::FrameChoice::Final,
                Some("middle") | None => crate::blender::preview::FrameChoice::Middle,
                Some(other) => crate::blender::preview::FrameChoice::Exact(
                    other
                        .parse::<u32>()
                        .map_err(|_| Error::invalid(format!(
                            "frame must be first, middle, final or a number, got {other:?}"
                        )))?,
                ),
            });
            let report = crate::blender::preview::preview_check(&req)?;
            Ok(serde_json::to_value(&report).unwrap_or(Value::Null))
        },
    );

    registry.add(
        spec(
            "cinvar.blender.render_job",
            "Render a deterministic Blender animation. Re-running the same request continues an interrupted render instead of starting over: verified PNG frames are never overwritten and incomplete frames are rejected. With transparent=true the delivered file keeps its alpha channel (ProRes 4444) so it can be overlaid on the timeline, and the alpha is verified on the encoded output rather than assumed from the PNG frames.",
            schema(
                json!({
                    "template": s("logo_reveal | title_3d"),
                    "text": s("Text for the animation"),
                    "duration": ts("Clip length in seconds (default 5)"),
                    "color": s("Accent colour"),
                    "width": i("Render width (default 1920)"),
                    "height": i("Render height (default 1080)"),
                    "fps": i("Frames per second (default 30)"),
                    "transparent": json!({"type": "boolean", "description": "Keep the alpha channel (transparent background)"}),
                    "out_dir": s("Directory for the render job (default: the project folder)")
                }),
                &[],
            ),
        ),
        |ctx: &mut ToolCtx, args: Value| {
            require_blender(ctx)?;
            let mut anim = crate::blender::AnimationSpec::default();
            if let Some(v) = args.get("template").and_then(|v| v.as_str()) {
                anim.template = v.to_string();
            }
            if let Some(v) = args.get("text").and_then(|v| v.as_str()) {
                anim.text = v.to_string();
            }
            if let Some(v) = args.get("color").and_then(|v| v.as_str()) {
                anim.color = v.to_string();
            }
            if let Some(v) = args.get("duration").and_then(|v| v.as_f64()) {
                if !(0.1..=600.0).contains(&v) {
                    return Err(Error::invalid("duration must be between 0.1 and 600 seconds"));
                }
                anim.duration = crate::util::time::Time::from_secs_f64(v);
            }
            for (key, slot) in [
                ("width", &mut anim.width),
                ("height", &mut anim.height),
                ("fps", &mut anim.fps),
            ] {
                if let Some(v) = args.get(key).and_then(|v| v.as_u64()) {
                    if !(16..=16384).contains(&v) {
                        return Err(Error::invalid(format!("{key} must be between 16 and 16384")));
                    }
                    *slot = v as u32;
                }
            }
            if let Some(v) = args.get("transparent").and_then(|v| v.as_bool()) {
                anim.transparent = v;
            }
            let ent = crate::edition::resolve(ctx.config);
            ent.check_export(anim.width, anim.height)?;
            let base = match args.get("out_dir").and_then(|v| v.as_str()) {
                Some(d) => std::path::PathBuf::from(d),
                None => ctx.store.root.clone(),
            };
            let render = crate::blender::render_animation_job(
                &anim,
                &base,
                &crate::blender::RenderControl::none(),
            )?;
            let mut out = json!({
                "clip": render.clip.to_string_lossy(),
                "frames": render.frames,
                "resumed": render.resumed_frames,
                "transparent": anim.transparent,
            });
            if anim.transparent {
                let verified = crate::blender::alpha::verify_alpha(&render.clip);
                if let Some(obj) = out.as_object_mut() {
                    obj.insert(
                        "alpha".into(),
                        match verified {
                            Ok(check) => serde_json::to_value(&check).unwrap_or(Value::Null),
                            Err(e) => json!({"verified": false, "error": e.message}),
                        },
                    );
                }
            }
            Ok(out)
        },
    );


    registry.add(
        spec(
            "cinvar.blender.inspect",
            "Read a .blend scene without rendering it: scene and frame range, render engine and resolution, every camera with lens and location, every material with the node types it uses, every mesh/text/curve object with vertex count and world bounding box, and any image whose file is missing from disk. Use this first so a plan is based on the real scene rather than on assumptions.",
            schema(
                json!({
                    "path": s("Path to a .blend file")
                }),
                &["path"],
            ),
        ),
        |ctx: &mut ToolCtx, args: Value| {
            require_blender(ctx)?;
            let raw = args
                .get("path")
                .and_then(|v| v.as_str())
                .ok_or_else(|| Error::invalid("path is required"))?;
            let path = scene_path(ctx, raw)?;
            let info = crate::blender::detect()?;
            let script = tempfile::Builder::new()
                .prefix("cinvar-inspect-")
                .suffix(".py")
                .tempfile()
                .map_err(|e| Error::new(crate::error::ErrorCode::Io, format!("temp script: {e}")))?;
            std::fs::write(script.path(), INSPECT_SCRIPT.as_bytes())
                .map_err(|e| Error::new(crate::error::ErrorCode::Io, format!("write script: {e}")))?;
            let output = std::process::Command::new(&info.path)
                .arg("-b")
                .arg(&path)
                .arg("--factory-startup")
                .arg("-P")
                .arg(script.path())
                .output()
                .map_err(|e| Error::new(crate::error::ErrorCode::Io, format!("run Blender: {e}")))?;
            let stdout = String::from_utf8_lossy(&output.stdout);
            let json_line = stdout
                .lines()
                .skip_while(|l| !l.contains("CINVAR_INSPECT_BEGIN"))
                .nth(1)
                .ok_or_else(|| {
                    Error::new(
                        crate::error::ErrorCode::Io,
                        format!(
                            "Blender produced no inspection output (exit {:?})",
                            output.status.code()
                        ),
                    )
                })?;
            serde_json::from_str::<Value>(json_line)
                .map_err(|e| Error::invalid(format!("unreadable inspection output: {e}")))
        },
    );

    registry.add(
        spec(
            "cinvar.blender.scene_build",
            "Build a .blend from a declarative scene description. Supports objects with materials, camera rigs (orbit, dolly_track, isometric_turntable), lighting presets (studio_three_point, cyberpunk_neon, product_cyclorama) and asset imports (.blend, .glb, .gltf, .fbx, .obj, .svg with extrusion). The scene is data: no caller python is executed. Returns the .blend path, verified object/camera/light counts and any import warnings. Pair with preview_check before rendering.",
            schema(
                json!({
                    "scene": json!({"type": "object", "description": "A CinvarBlenderScene document: name, materials[], objects[], assets[], camera, lighting, render"}),
                    "out_dir": s("Directory for the .blend (default: the project folder)")
                }),
                &["scene"],
            ),
        ),
        |ctx: &mut ToolCtx, args: Value| {
            require_blender(ctx)?;
            let scene_value = args
                .get("scene")
                .cloned()
                .ok_or_else(|| Error::invalid("scene is required"))?;
            let scene: crate::blender::scene::CinvarBlenderScene =
                serde_json::from_value(scene_value)
                    .map_err(|e| Error::invalid(format!("scene document: {e}")))?;
            let out_dir = match args.get("out_dir").and_then(|v| v.as_str()) {
                Some(d) => std::path::PathBuf::from(d),
                None => ctx.store.root.clone(),
            };
            let settings = &scene.render;
            crate::edition::resolve(ctx.config)
                .check_export(settings.width, settings.height)?;
            let built = crate::blender::scene::build(&scene, &out_dir)?;
            Ok(serde_json::to_value(&built).unwrap_or(Value::Null))
        },
    );

    registry.add(
        spec(
            "cinvar.blender.mockup_screen",
            "Place a real video (a timeline clip or a screen recording) into an animated 3D scene and render it back. Four scene kinds (floating screen, glass browser, laptop, phone), camera motion, depth of field, lighting presets, reflections and motion blur. The source is decoded to a frame-exact sequence and bound as an animated texture, so render frame N shows source frame N. Returns the rendered clip plus an overlay filtergraph for compositing it on the timeline.",
            schema(
                json!({
                    "video": s("Path to the source video (timeline clip or screen recording)"),
                    "kind": s("floating_screen | glass_browser | laptop | phone (default floating_screen)"),
                    "camera": s("static | orbit | dolly_in | pan_right | handheld (default static)"),
                    "lighting": s("studio | softbox | neon | daylight (default studio)"),
                    "engine": s("eevee | cycles (default eevee: this build has no denoiser for Cycles)"),
                    "width": i("Render width (default 1920)"),
                    "height": i("Render height (default 1080)"),
                    "fps": i("Frames per second (default 30)"),
                    "duration": ts("Seconds of the source to use (default 5)"),
                    "start": ts("Seconds into the source to start (default 0)"),
                    "samples": i("Render samples"),
                    "seed": i("Deterministic seed"),
                    "reflections": json!({"type": "boolean", "description": "Screen and glass reflections"}),
                    "motion_blur": ts("Shutter for motion blur (0 disables)"),
                    "f_stop": ts("Aperture; lower means shallower depth of field"),
                    "transparent": json!({"type": "boolean", "description": "Keep the alpha channel (ProRes 4444)"}),
                    "out_dir": s("Job directory (default: the project folder)")
                }),
                &["video"],
            ),
        ),
        |ctx: &mut ToolCtx, args: Value| {
            require_blender(ctx)?;
            let raw = args
                .get("video")
                .and_then(|v| v.as_str())
                .ok_or_else(|| Error::invalid("video is required"))?;
            let video = media_path(ctx, raw)?;
            let out_dir = match args.get("out_dir").and_then(|v| v.as_str()) {
                Some(d) => std::path::PathBuf::from(d),
                None => ctx.store.root.clone(),
            };
            // MockupRequest is #[serde(default)]: a caller sends only what it cares about, and the
            // unknown out_dir key is ignored rather than rejected.
            let mut req: crate::blender::mockup::MockupRequest =
                serde_json::from_value(args.clone())
                    .map_err(|e| Error::invalid(format!("mockup arguments: {e}")))?;
            req.video = video;
            crate::edition::resolve(ctx.config).check_export(req.width, req.height)?;
            let rendered = crate::blender::mockup::render_mockup(
                &req,
                &out_dir,
                &crate::blender::RenderControl::none(),
            )?;
            Ok(serde_json::to_value(&rendered).unwrap_or(Value::Null))
        },
    );
}
/// A media file the agent may read (any video container, not only .blend).
fn media_path(ctx: &ToolCtx, raw: &str) -> Result<std::path::PathBuf> {
    let path = crate::util::paths::absolutize(std::path::Path::new(raw), &ctx.store.root);
    if !path.is_file() {
        return Err(Error::invalid(format!("not a file: {}", path.display())));
    }
    let real = path
        .canonicalize()
        .map_err(|_| Error::not_found(format!("cannot read {}", path.display())))?;
    let mut allowed: Vec<std::path::PathBuf> = ctx.config.security.effective_media_roots();
    allowed.push(ctx.store.root.clone());
    if allowed
        .iter()
        .filter_map(|r| r.canonicalize().ok())
        .any(|r| real.starts_with(r))
    {
        Ok(real)
    } else {
        Err(Error::new(
            crate::error::ErrorCode::PermissionDenied,
            format!("outside the folders this agent may read: {}", path.display()),
        ))
    }
}

/// The bpy inspection driver. A constant: the caller supplies a path, never python.
const INSPECT_SCRIPT: &str = r##"
import bpy, json, os, sys, math
from mathutils import Vector

def main():
    scn = bpy.context.scene
    out = {"scene": scn.name, "frame_start": scn.frame_start, "frame_end": scn.frame_end,
           "fps": scn.render.fps, "engine": scn.render.engine,
           "resolution": [scn.render.resolution_x, scn.render.resolution_y],
           "cameras": [], "materials": [], "objects": [], "missing_textures": []}
    cam = scn.camera
    out["active_camera"] = cam.name if cam else None
    for o in bpy.data.objects:
        if o.type != "CAMERA": continue
        out["cameras"].append({"name": o.name, "lens_mm": round(o.data.lens, 3),
                               "location": [round(v, 4) for v in o.location]})
    for m in bpy.data.materials:
        entry = {"name": m.name, "nodes": []}
        if m.use_nodes and m.node_tree:
            entry["nodes"] = sorted({n.type for n in m.node_tree.nodes})
        out["materials"].append(entry)
    for img in bpy.data.images:
        fp = getattr(img, "filepath", "") or ""
        if not fp or img.packed_file: continue
        resolved = bpy.path.abspath(fp)
        if not os.path.exists(resolved):
            out["missing_textures"].append({"image": img.name, "filepath": fp})
    deps = bpy.context.evaluated_depsgraph_get()
    for o in bpy.data.objects:
        if o.type not in {"MESH", "FONT", "CURVE", "SURFACE", "META"}: continue
        try:
            ev = o.evaluated_get(deps)
            corners = [o.matrix_world @ Vector(c) for c in ev.bound_box]
        except Exception:
            continue
        if not corners: continue
        mn = [min(c[i] for c in corners) for i in range(3)]
        mx = [max(c[i] for c in corners) for i in range(3)]
        entry = {"name": o.name, "type": o.type,
                 "verts": len(o.data.vertices) if hasattr(o.data, "vertices") else 0,
                 "min": [round(v, 4) for v in mn], "max": [round(v, 4) for v in mx],
                 "materials": [ms.material.name for ms in o.material_slots if ms.material]}
        out["objects"].append(entry)
    print("CINVAR_INSPECT_BEGIN")
    print(json.dumps(out))
    print("CINVAR_INSPECT_END")

main()
"##;