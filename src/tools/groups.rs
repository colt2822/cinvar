use serde_json::{json, Value};

use crate::error::{Error, Result};
use crate::project::model::*;
use crate::tools::{Registry, ToolCtx, ToolSpec};
use crate::util::text;
use crate::util::time::{parse_fps, Time};

fn schema(props: Value, required: &[&str]) -> Value {
    json!({
        "type": "object",
        "properties": props,
        "required": required,
        "additionalProperties": false
    })
}

fn s(desc: &str) -> Value {
    json!({"type": "string", "description": desc})
}

fn ts(desc: &str) -> Value {
    json!({"type": "string", "description": format!("{desc} — accepts seconds (12.5), MM:SS (01:30) or HH:MM:SS.mmm")})
}

fn n(desc: &str) -> Value {
    json!({"type": "number", "description": desc})
}

fn i(desc: &str) -> Value {
    json!({"type": "integer", "description": desc})
}

fn b(desc: &str) -> Value {
    json!({"type": "boolean", "description": desc})
}

fn str_list(desc: &str) -> Value {
    json!({"type": "array", "items": {"type": "string"}, "description": desc})
}

pub(crate) fn req_str(args: &Value, key: &str) -> Result<String> {
    match args.get(key).and_then(|v| v.as_str()) {
        Some(v) if !v.trim().is_empty() => Ok(v.to_string()),
        Some(_) => Err(Error::invalid(format!("'{key}' must not be empty"))),
        None => Err(Error::invalid(format!("missing required field '{key}'"))),
    }
}

pub(crate) fn opt_str(args: &Value, key: &str) -> Option<String> {
    args.get(key)
        .and_then(|v| v.as_str())
        .filter(|v| !v.trim().is_empty())
        .map(|v| v.to_string())
}

pub(crate) fn req_time(args: &Value, key: &str) -> Result<Time> {
    match args.get(key) {
        Some(Value::String(text)) => text.parse::<Time>(),
        Some(Value::Number(num)) => Ok(Time::from_secs_f64(num.as_f64().unwrap_or(0.0))),
        _ => Err(Error::invalid(format!(
            "missing required time field '{key}'"
        ))),
    }
}

pub(crate) fn opt_time(args: &Value, key: &str) -> Result<Option<Time>> {
    match args.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(text)) => Ok(Some(text.parse::<Time>()?)),
        Some(Value::Number(num)) => Ok(Some(Time::from_secs_f64(num.as_f64().unwrap_or(0.0)))),
        _ => Err(Error::invalid(format!("field '{key}' must be a time"))),
    }
}

pub(crate) fn opt_num(args: &Value, key: &str) -> Option<f64> {
    args.get(key).and_then(|v| v.as_f64())
}

pub(crate) fn opt_int(args: &Value, key: &str) -> Option<i64> {
    args.get(key).and_then(|v| v.as_i64())
}

pub(crate) fn opt_bool(args: &Value, key: &str) -> Option<bool> {
    args.get(key).and_then(|v| v.as_bool())
}

fn spec(name: &str, group: &str, description: &str, input: Value, destructive: bool) -> ToolSpec {
    ToolSpec {
        name: name.to_string(),
        group: group.to_string(),
        description: description.to_string(),
        input,
        destructive,
    }
}

fn project_summary(project: &Project) -> Value {
    json!({
        "id": project.id,
        "name": project.name,
        "revision": project.revision,
        "assets": project.assets.iter().map(|a| json!({
            "id": a.id,
            "name": a.display_name(),
            "kind": format!("{:?}", a.kind).to_lowercase(),
            "duration": a.info.as_ref().map(|i| i.duration.as_secs_f64()),
            "width": a.info.as_ref().map(|i| i.width),
            "height": a.info.as_ref().map(|i| i.height),
            "missing": a.missing,
        })).collect::<Vec<_>>(),
        "timelines": project.timelines.iter().map(|t| json!({
            "id": t.id,
            "name": t.name,
            "duration": t.duration().as_secs_f64(),
            "width": t.width,
            "height": t.height,
            "fps": t.fps.as_f64(),
            "tracks": t.tracks.iter().map(|tr| json!({
                "id": tr.id,
                "name": tr.name,
                "kind": format!("{:?}", tr.kind).to_lowercase(),
                "clips": tr.sorted_clips().iter().map(|c| json!({
                    "id": c.id,
                    "name": c.name,
                    "asset_id": c.asset,
                    "start": c.timeline_start.as_secs_f64(),
                    "end": c.timeline_end().as_secs_f64(),
                    "duration": c.duration().as_secs_f64(),
                    "source_in": c.source_in.as_secs_f64(),
                    "source_out": c.source_out.as_secs_f64(),
                    "speed": c.speed,
                    "text": c.text.as_ref().map(|t| t.text.clone()),
                    "effects": c.effects.iter().map(|e| e.kind.clone()).collect::<Vec<_>>(),
                    "transition_in": c.transition_in.as_ref().map(|t| t.kind.clone()),
                    "transition_out": c.transition_out.as_ref().map(|t| t.kind.clone()),
                })).collect::<Vec<_>>(),
            })).collect::<Vec<_>>(),
        })).collect::<Vec<_>>(),
        "active_timeline": project.active_timeline,
        "captions": project.captions.iter().map(|c| json!({"id": c.id, "cues": c.cues.len(), "language": c.language})).collect::<Vec<_>>(),
        "issues": crate::project::ops::validate(project),
    })
}

pub fn register_project(registry: &mut Registry) {
    registry.add(
        spec(
            "cinvar.project.status",
            "project",
            "Report the current project: assets, timelines, tracks, captions and validation issues. Call this first to understand the state before editing.",
            schema(json!({}), &[]),
            false,
        ),
        |ctx: &mut ToolCtx, _args: Value| Ok(json!({"project": project_summary(ctx.project), "path": ctx.store.root.to_string_lossy()})),
    );

    registry.add(
        spec(
            "cinvar.project.rename",
            "project",
            "Rename the project.",
            schema(json!({"name": s("New project name")}), &["name"]),
            false,
        ),
        |ctx: &mut ToolCtx, args: Value| {
            let name = req_str(&args, "name")?;
            ctx.record_history();
            ctx.project.name = name;
            ctx.persist()?;
            Ok(json!({"ok": true, "name": ctx.project.name}))
        },
    );

    registry.add(
        spec(
            "cinvar.project.history",
            "project",
            "List saved revisions of this project so an edit can be rolled back.",
            schema(json!({}), &[]),
            false,
        ),
        |ctx: &mut ToolCtx, _args: Value| {
            let revisions = ctx.store.revisions()?;
            Ok(json!({"revisions": revisions}))
        },
    );

    registry.add(
        spec(
            "cinvar.project.undo",
            "project",
            "Undo the last edit made in this session.",
            schema(json!({}), &[]),
            false,
        ),
        |ctx: &mut ToolCtx, _args: Value| {
            let undone = ctx.history.undo(ctx.project);
            if undone {
                ctx.persist()?;
            }
            Ok(json!({"undone": undone, "revision": ctx.project.revision}))
        },
    );

    registry.add(
        spec(
            "cinvar.project.redo",
            "project",
            "Redo the last undone edit.",
            schema(json!({}), &[]),
            false,
        ),
        |ctx: &mut ToolCtx, _args: Value| {
            let redone = ctx.history.redo(ctx.project);
            if redone {
                ctx.persist()?;
            }
            Ok(json!({"redone": redone, "revision": ctx.project.revision}))
        },
    );

    registry.add(
        spec(
            "cinvar.project.rollback",
            "project",
            "Restore the project to a previously saved revision.",
            schema(
                json!({"revision": i("Revision number from cinvar.project.history")}),
                &["revision"],
            ),
            true,
        ),
        |ctx: &mut ToolCtx, args: Value| {
            let revision = opt_int(&args, "revision")
                .ok_or_else(|| Error::invalid("missing 'revision'"))?
                as u64;
            ctx.record_history();
            ctx.store.rollback(ctx.project, revision)?;
            Ok(json!({"ok": true, "revision": ctx.project.revision}))
        },
    );

    registry.add(
        spec(
            "cinvar.project.relink",
            "project",
            "Point a missing asset at a new file on disk.",
            schema(json!({"asset_id": s("Asset id"), "path": s("Absolute path of the replacement file")}), &["asset_id", "path"]),
            false,
        ),
        |ctx: &mut ToolCtx, args: Value| {
            let asset_id = req_str(&args, "asset_id")?;
            let path = req_str(&args, "path")?;
            within_agent_roots(ctx, std::path::Path::new(&path))?;
            ctx.store.relink(ctx.project, &asset_id, std::path::Path::new(&path))?;
            ctx.persist()?;
            Ok(json!({"ok": true}))
        },
    );
}

pub fn register_media(registry: &mut Registry) {
    registry.add(
        spec(
            "cinvar.media.import",
            "media",
            "Import a video, audio or image file into the project. Copies the file into the project and probes it. The original is never modified.",
            schema(json!({"path": s("Path to the media file (absolute, or relative to the working directory)")}), &["path"]),
            false,
        ),
        |ctx: &mut ToolCtx, args: Value| {
            let path = req_str(&args, "path")?;
            let source = crate::util::paths::absolutize(std::path::Path::new(&path), &std::env::current_dir().unwrap_or_default());
            // Work on the resolved path from here on, so a link swapped in after the check is not followed.
            let source = within_agent_roots(ctx, &source)?;
            // Agent-facing import: only real media is copied into the project, judged by content.
            crate::media::probe_untrusted(&source).map_err(|e| {
                Error::new(crate::error::ErrorCode::InvalidInput, format!("not importable as media: {}", e.message))
            })?;
            let asset = ctx.store.import_asset(ctx.project, &source, "tool:import")?;
            ctx.persist()?;
            Ok(json!({
                "asset_id": asset.id,
                "path": asset.path,
                "duration": asset.info.as_ref().map(|i| i.duration.as_secs_f64()),
                "width": asset.info.as_ref().map(|i| i.width),
                "height": asset.info.as_ref().map(|i| i.height),
                "has_audio": asset.info.as_ref().map(|i| i.has_audio),
            }))
        },
    );

    registry.add(
        spec(
            "cinvar.media.import_folder",
            "media",
            "Import every supported video, audio and image file in a folder. Returns one entry per imported asset.",
            schema(json!({"path": s("Folder path"), "recursive": b("Descend into subfolders (default false)")}), &["path"]),
            false,
        ),
        |ctx: &mut ToolCtx, args: Value| {
            let path = req_str(&args, "path")?;
            let recursive = opt_bool(&args, "recursive").unwrap_or(false);
            let dir = crate::util::paths::absolutize(std::path::Path::new(&path), &std::env::current_dir().unwrap_or_default());
            within_agent_roots(ctx, &dir)?;
            let mut imported = Vec::new();
            let mut files: Vec<std::path::PathBuf> = Vec::new();
            collect_media(&dir, recursive, &mut files)?;
            files.sort();
            for file in files {
                if let Err(err) = crate::media::probe_untrusted(&file) {
                    imported.push(json!({"file": file.to_string_lossy(), "error": format!("skipped, not real media: {}", err.message)}));
                    continue;
                }
                match ctx.store.import_asset(ctx.project, &file, "tool:import_folder") {
                    Ok(asset) => imported.push(json!({
                        "asset_id": asset.id,
                        "file": asset.display_name(),
                        "duration": asset.info.as_ref().map(|i| i.duration.as_secs_f64()),
                    })),
                    Err(err) => imported.push(json!({"file": file.to_string_lossy(), "error": err.message})),
                }
            }
            ctx.persist()?;
            Ok(json!({"imported": imported}))
        },
    );

    registry.add(
        spec(
            "cinvar.media.inspect",
            "media",
            "Probe a media file without importing it: duration, resolution, frame rate, codecs, audio channels.",
            schema(json!({"path": s("Path to the media file")}), &["path"]),
            false,
        ),
        |ctx: &mut ToolCtx, args: Value| {
            let path = req_str(&args, "path")?;
            let real = within_agent_roots(ctx, std::path::Path::new(&path))?;
            let info = crate::media::probe(&real)?;
            Ok(json!({"info": info, "orientation": format!("{:?}", info.orientation()).to_lowercase()}))
        },
    );

    registry.add(
        spec(
            "cinvar.media.frames",
            "media",
            "Extract frames at given timestamps as PNG files inside the project cache, from an imported asset or from a rendered file. Use the returned paths to actually look at the footage or the render instead of assuming it came out right.",
            schema(json!({
                "asset_id": s("Asset id (use this or path)"),
                "path": s("Path inside the project, e.g. renders/out.mp4 (use this or asset_id)"),
                "times": str_list("Timestamps to capture, e.g. [\"0\",\"2.5\",\"7\"]"),
                "width": i("Output width in pixels (default 960)")
            }), &["times"]),
            false,
        ),
        |ctx: &mut ToolCtx, args: Value| {
            let times: Vec<String> = args
                .get("times")
                .and_then(|v| v.as_array())
                .map(|a| a.iter().filter_map(|v| v.as_str().map(|s| s.to_string())).collect())
                .unwrap_or_default();
            if times.is_empty() {
                return Err(Error::invalid("provide at least one timestamp in 'times'"));
            }
            let (source, stem) = match opt_str(&args, "path") {
                Some(raw) => {
                    let resolved = crate::util::paths::ensure_within(&ctx.store.root, std::path::Path::new(&raw))?;
                    let stem = format!("frame-{}", crate::util::text::slugify(&raw));
                    (resolved, stem)
                }
                None => {
                    let asset_id = req_str(&args, "asset_id")?;
                    let asset = ctx
                        .project
                        .asset(&asset_id)
                        .ok_or_else(|| Error::not_found(format!("asset {asset_id} not found")))?
                        .clone();
                    (ctx.store.asset_path(&asset)?, asset.id.clone())
                }
            };
            if !source.exists() {
                return Err(Error::not_found(format!("{} does not exist", source.display())));
            }
            let width = opt_int(&args, "width").map(|w| w as u32).or(Some(960));
            let mut out = Vec::new();
            for (idx, raw) in times.iter().enumerate() {
                let at: Time = raw.parse()?;
                let target = ctx.store.cache_dir().join(format!("frame-{}-{}.png", stem, idx));
                crate::util::paths::unlink_if_symlink(&target);
                let written = crate::media::extract_frame(&source, at, &target, width)?;
                let bytes = std::fs::metadata(&written).map(|m| m.len()).unwrap_or(0);
                out.push(json!({"at": raw, "file": written.to_string_lossy(), "bytes": bytes}));
            }
            Ok(json!({"frames": out, "note": "open these image files to review the actual result"}))
        },
    );

    registry.add(
        spec(
            "cinvar.media.contact_sheet",
            "media",
            "Build a contact sheet (grid of thumbnails) for an asset so a model can review the footage in one image.",
            schema(json!({"asset_id": s("Asset id"), "count": i("Number of thumbnails (default 9)"), "columns": i("Grid columns (default 3)")}), &["asset_id"]),
            false,
        ),
        |ctx: &mut ToolCtx, args: Value| {
            let asset_id = req_str(&args, "asset_id")?;
            let count = opt_int(&args, "count").unwrap_or(9).clamp(1, 64) as usize;
            let columns = opt_int(&args, "columns").unwrap_or(3).clamp(1, 8) as usize;
            let asset = ctx.project.asset(&asset_id).ok_or_else(|| Error::not_found(format!("asset {asset_id} not found")))?.clone();
            let source = ctx.store.asset_path(&asset)?;
            let out = ctx.store.cache_dir().join(format!("sheet-{}.jpg", asset.id));
            crate::util::paths::unlink_if_symlink(&out);
            let written = crate::media::contact_sheet(&source, count, columns, &out, 320)?;
            Ok(json!({"file": written.to_string_lossy()}))
        },
    );

    registry.add(
        spec(
            "cinvar.media.scenes",
            "media",
            "Detect shot boundaries and return scene start timestamps. Use this before deciding cut points.",
            schema(json!({"asset_id": s("Asset id"), "threshold": n("Scene change threshold 0.1-0.9 (default 0.35)")}), &["asset_id"]),
            false,
        ),
        |ctx: &mut ToolCtx, args: Value| {
            let asset_id = req_str(&args, "asset_id")?;
            let threshold = opt_num(&args, "threshold").unwrap_or(0.35) as f32;
            let asset = ctx.project.asset(&asset_id).ok_or_else(|| Error::not_found(format!("asset {asset_id} not found")))?.clone();
            let source = ctx.store.asset_path(&asset)?;
            let cuts = crate::media::detect_scenes(&source, threshold)?;
            Ok(json!({"scenes": cuts.iter().map(|c| json!({"at": c.at.as_secs_f64(), "score": c.score})).collect::<Vec<_>>()}))
        },
    );

    registry.add(
        spec(
            "cinvar.media.silence",
            "media",
            "Detect silent spans in an asset's audio. Use this to trim dead air from podcasts or interviews.",
            schema(json!({"asset_id": s("Asset id"), "noise_db": n("Noise floor in dB (default -35)"), "min_secs": n("Minimum silence length in seconds (default 0.7)")}), &["asset_id"]),
            false,
        ),
        |ctx: &mut ToolCtx, args: Value| {
            let asset_id = req_str(&args, "asset_id")?;
            let noise_db = opt_num(&args, "noise_db").unwrap_or(-35.0);
            let min_secs = opt_num(&args, "min_secs").unwrap_or(0.7);
            let asset = ctx.project.asset(&asset_id).ok_or_else(|| Error::not_found(format!("asset {asset_id} not found")))?.clone();
            let source = ctx.store.asset_path(&asset)?;
            let spans = crate::media::detect_silence(&source, noise_db, min_secs)?;
            Ok(json!({"spans": spans.iter().map(|s| json!({"start": s.start.as_secs_f64(), "end": s.end.as_secs_f64()})).collect::<Vec<_>>()}))
        },
    );

    registry.add(
        spec(
            "cinvar.media.black",
            "media",
            "Detect black frames or black spans in a video asset.",
            schema(json!({"asset_id": s("Asset id"), "min_secs": n("Minimum span in seconds (default 0.5)")}), &["asset_id"]),
            false,
        ),
        |ctx: &mut ToolCtx, args: Value| {
            let asset_id = req_str(&args, "asset_id")?;
            let min_secs = opt_num(&args, "min_secs").unwrap_or(0.5);
            let asset = ctx.project.asset(&asset_id).ok_or_else(|| Error::not_found(format!("asset {asset_id} not found")))?.clone();
            let source = ctx.store.asset_path(&asset)?;
            let spans = crate::media::detect_black(&source, min_secs, 0.10)?;
            Ok(json!({"spans": spans.iter().map(|s| json!({"start": s.start.as_secs_f64(), "end": s.end.as_secs_f64()})).collect::<Vec<_>>()}))
        },
    );

    registry.add(
        spec(
            "cinvar.media.proxy",
            "media",
            "Create a low-resolution proxy file for expensive footage so previews render fast.",
            schema(
                json!({"asset_id": s("Asset id"), "height": i("Proxy height (default 480)")}),
                &["asset_id"],
            ),
            false,
        ),
        |ctx: &mut ToolCtx, args: Value| {
            let asset_id = req_str(&args, "asset_id")?;
            let height = opt_int(&args, "height").unwrap_or(480).clamp(120, 1080) as u32;
            let asset = ctx
                .project
                .asset(&asset_id)
                .ok_or_else(|| Error::not_found(format!("asset {asset_id} not found")))?
                .clone();
            let source = ctx.store.asset_path(&asset)?;
            let out = ctx
                .store
                .cache_dir()
                .join(format!("proxy-{}-{}p.mp4", asset.id, height));
            crate::util::paths::unlink_if_symlink(&out);
            let written = crate::media::make_proxy(&source, &out, height)?;
            Ok(json!({"file": written.to_string_lossy()}))
        },
    );
}

fn collect_media(
    dir: &std::path::Path,
    recursive: bool,
    out: &mut Vec<std::path::PathBuf>,
) -> Result<()> {
    if !dir.is_dir() {
        return Err(Error::not_found(format!(
            "{} is not a folder",
            dir.display()
        )));
    }
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.is_dir() {
            if recursive {
                collect_media(&path, recursive, out)?;
            }
            continue;
        }
        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_ascii_lowercase();
        if matches!(
            ext.as_str(),
            "mp4"
                | "mov"
                | "mkv"
                | "webm"
                | "avi"
                | "m4v"
                | "wav"
                | "mp3"
                | "m4a"
                | "flac"
                | "png"
                | "jpg"
                | "jpeg"
                | "webp"
        ) {
            out.push(path);
        }
    }
    Ok(())
}

pub fn register_timeline(registry: &mut Registry) {
    registry.add(
        spec(
            "cinvar.timeline.create",
            "timeline",
            "Create a new timeline (canvas) with a video and an audio track and make it active.",
            schema(
                json!({"name": s("Timeline name"), "width": i("Canvas width (default 1920)"), "height": i("Canvas height (default 1080)"), "fps": s("Frame rate, e.g. 30, 25, 30000/1001 (default 30)")}),
                &[],
            ),
            false,
        ),
        |ctx: &mut ToolCtx, args: Value| {
            let name = opt_str(&args, "name").unwrap_or_else(|| "Main".into());
            let width = opt_int(&args, "width").unwrap_or(1920) as u32;
            let height = opt_int(&args, "height").unwrap_or(1080) as u32;
            let fps = match opt_str(&args, "fps") {
                Some(v) => parse_fps(&v)?,
                None => parse_fps("30")?,
            };
            ctx.record_history();
            let id = crate::project::ops::create_timeline(ctx.project, &name, width, height, fps)?;
            ctx.persist()?;
            Ok(json!({"timeline_id": id, "width": width, "height": height}))
        },
    );

    registry.add(
        spec(
            "cinvar.timeline.add_track",
            "timeline",
            "Add a track (video, audio, overlay or caption) to a timeline.",
            schema(
                json!({"kind": s("video | audio | overlay | caption"), "name": s("Track name")}),
                &["kind"],
            ),
            false,
        ),
        |ctx: &mut ToolCtx, args: Value| {
            let kind = match req_str(&args, "kind")?.to_lowercase().as_str() {
                "video" => TrackKind::Video,
                "audio" => TrackKind::Audio,
                "overlay" => TrackKind::Overlay,
                "caption" | "captions" => TrackKind::Caption,
                other => return Err(Error::invalid(format!("unknown track kind '{other}'"))),
            };
            let name = opt_str(&args, "name").unwrap_or_default();
            ctx.record_history();
            let id = crate::project::ops::add_track(ctx.project, None, kind, &name)?;
            ctx.persist()?;
            Ok(json!({"track_id": id}))
        },
    );

    registry.add(
        spec(
            "cinvar.timeline.insert_clip",
            "timeline",
            "Place an asset on a track at a specific time, with optional source in/out points. Clips may not overlap on the same track.",
            schema(
                json!({
                    "asset_id": s("Asset id to place"),
                    "at": ts("Timeline position; omit to append at the end of the track"),
                    "source_in": ts("Source in point (default 0)"),
                    "source_out": ts("Source out point (default: full asset duration)"),
                    "track_id": s("Target track id; omit for the default track of the asset kind")
                }),
                &["asset_id"],
            ),
            false,
        ),
        |ctx: &mut ToolCtx, args: Value| {
            let asset_id = req_str(&args, "asset_id")?;
            let track_id = opt_str(&args, "track_id");
            let request = crate::project::ops::ClipRequest {
                asset_id: &asset_id,
                source_in: opt_time(&args, "source_in")?,
                source_out: opt_time(&args, "source_out")?,
                at: opt_time(&args, "at")?,
                name: None,
            };
            ctx.record_history();
            let id = crate::project::ops::insert_clip(ctx.project, None, track_id.as_deref(), request)?;
            ctx.persist()?;
            Ok(json!({"clip_id": id, "timeline_duration": ctx.project.timeline().map(|t| t.duration().as_secs_f64())}))
        },
    );

    registry.add(
        spec(
            "cinvar.timeline.append_clip",
            "timeline",
            "Append an asset to the end of a track. This is the normal way to assemble a rough cut in order.",
            schema(json!({"asset_id": s("Asset id"), "track_id": s("Optional track id")}), &["asset_id"]),
            false,
        ),
        |ctx: &mut ToolCtx, args: Value| {
            let asset_id = req_str(&args, "asset_id")?;
            let track_id = opt_str(&args, "track_id");
            ctx.record_history();
            let id = crate::project::ops::append_clip(ctx.project, None, track_id.as_deref(), &asset_id)?;
            ctx.persist()?;
            Ok(json!({"clip_id": id, "timeline_duration": ctx.project.timeline().map(|t| t.duration().as_secs_f64())}))
        },
    );

    registry.add(
        spec(
            "cinvar.timeline.trim",
            "timeline",
            "Change a clip's source in and/or out point. Trimming never touches the original file. Pass ripple=true to close or open the gap by shifting every later clip on the same track.",
            schema(json!({"clip_id": s("Clip id"), "source_in": ts("New source in point"), "source_out": ts("New source out point"), "ripple": b("Shift later clips on the track by the same amount (default false)")}), &["clip_id"]),
            false,
        ),
        |ctx: &mut ToolCtx, args: Value| {
            let clip_id = req_str(&args, "clip_id")?;
            let ripple = opt_bool(&args, "ripple").unwrap_or(false);
            ctx.record_history();
            let (source_in, source_out) = (opt_time(&args, "source_in")?, opt_time(&args, "source_out")?);
            let shift = if ripple {
                Some(crate::project::ops::trim_clip_ripple(ctx.project, &clip_id, source_in, source_out)?)
            } else {
                crate::project::ops::trim_clip(ctx.project, &clip_id, source_in, source_out)?;
                None
            };
            ctx.persist()?;
            let (_, _, clip) = ctx.project.find_clip(&clip_id).ok_or_else(|| Error::not_found("clip vanished"))?;
            Ok(json!({
                "clip_id": clip_id,
                "duration": clip.duration().as_secs_f64(),
                "ripple_shift": shift.map(|d| d.as_secs_f64()),
            }))
        },
    );

    registry.add(
        spec(
            "cinvar.timeline.split",
            "timeline",
            "Split a clip at a timeline position, producing two adjacent clips.",
            schema(
                json!({"clip_id": s("Clip id"), "at": ts("Timeline position to split at")}),
                &["clip_id", "at"],
            ),
            false,
        ),
        |ctx: &mut ToolCtx, args: Value| {
            let clip_id = req_str(&args, "clip_id")?;
            let at = req_time(&args, "at")?;
            ctx.record_history();
            let (left, right) = crate::project::ops::split_clip(ctx.project, &clip_id, at)?;
            ctx.persist()?;
            Ok(json!({"left_clip_id": left, "right_clip_id": right}))
        },
    );

    registry.add(
        spec(
            "cinvar.timeline.move",
            "timeline",
            "Move a clip to a new timeline position, optionally onto another track.",
            schema(json!({"clip_id": s("Clip id"), "at": ts("New timeline start"), "track_id": s("Optional destination track")}), &["clip_id", "at"]),
            false,
        ),
        |ctx: &mut ToolCtx, args: Value| {
            let clip_id = req_str(&args, "clip_id")?;
            let at = req_time(&args, "at")?;
            let track_id = opt_str(&args, "track_id");
            ctx.record_history();
            crate::project::ops::move_clip(ctx.project, &clip_id, track_id.as_deref(), at)?;
            ctx.persist()?;
            Ok(json!({"ok": true, "clip_id": clip_id}))
        },
    );

    registry.add(
        spec(
            "cinvar.timeline.remove",
            "timeline",
            "Remove a clip from the timeline. Pass ripple=true to close the gap it leaves.",
            schema(
                json!({"clip_id": s("Clip id"), "ripple": b("Close the gap (default false)")}),
                &["clip_id"],
            ),
            true,
        ),
        |ctx: &mut ToolCtx, args: Value| {
            let clip_id = req_str(&args, "clip_id")?;
            let ripple = opt_bool(&args, "ripple").unwrap_or(false);
            ctx.record_history();
            if ripple {
                crate::project::ops::ripple_delete(ctx.project, &clip_id)?;
            } else {
                crate::project::ops::remove_clip(ctx.project, &clip_id)?;
            }
            ctx.persist()?;
            Ok(json!({"ok": true, "removed": clip_id}))
        },
    );

    registry.add(
        spec(
            "cinvar.timeline.duplicate",
            "timeline",
            "Duplicate a clip, offset in time on the same track.",
            schema(json!({"clip_id": s("Clip id"), "offset": ts("Time offset (default: the clip duration)")}), &["clip_id"]),
            false,
        ),
        |ctx: &mut ToolCtx, args: Value| {
            let clip_id = req_str(&args, "clip_id")?;
            let offset = match opt_time(&args, "offset")? {
                Some(t) => t,
                None => ctx.project.find_clip(&clip_id).map(|(_, _, c)| c.duration()).unwrap_or(Time::ZERO),
            };
            ctx.record_history();
            let new_id = crate::project::ops::duplicate_clip(ctx.project, &clip_id, offset)?;
            ctx.persist()?;
            Ok(json!({"clip_id": new_id}))
        },
    );

    registry.add(
        spec(
            "cinvar.timeline.speed",
            "timeline",
            "Change clip playback speed. 2.0 is twice as fast, 0.5 is slow motion.",
            schema(
                json!({"clip_id": s("Clip id"), "speed": n("Speed multiplier 0.05-20")}),
                &["clip_id", "speed"],
            ),
            false,
        ),
        |ctx: &mut ToolCtx, args: Value| {
            let clip_id = req_str(&args, "clip_id")?;
            let speed = opt_num(&args, "speed").ok_or_else(|| Error::invalid("missing 'speed'"))?;
            ctx.record_history();
            crate::project::ops::set_speed(ctx.project, &clip_id, speed)?;
            ctx.persist()?;
            Ok(json!({"ok": true}))
        },
    );

    registry.add(
        spec(
            "cinvar.timeline.freeze",
            "timeline",
            "Hold a single source frame for the whole clip, creating a freeze frame.",
            schema(
                json!({"clip_id": s("Clip id"), "at": ts("Source timestamp to freeze on")}),
                &["clip_id", "at"],
            ),
            false,
        ),
        |ctx: &mut ToolCtx, args: Value| {
            let clip_id = req_str(&args, "clip_id")?;
            let at = req_time(&args, "at")?;
            ctx.record_history();
            crate::project::ops::freeze_frame(ctx.project, &clip_id, at)?;
            ctx.persist()?;
            Ok(json!({"ok": true}))
        },
    );

    registry.add(
        spec(
            "cinvar.timeline.reverse",
            "timeline",
            "Play a clip backwards.",
            schema(json!({"clip_id": s("Clip id"), "reversed": b("true to reverse, false to restore (default true)")}), &["clip_id"]),
            false,
        ),
        |ctx: &mut ToolCtx, args: Value| {
            let clip_id = req_str(&args, "clip_id")?;
            let reversed = opt_bool(&args, "reversed").unwrap_or(true);
            ctx.record_history();
            crate::project::ops::set_reversed(ctx.project, &clip_id, reversed)?;
            ctx.persist()?;
            Ok(json!({"ok": true, "reversed": reversed}))
        },
    );

    registry.add(
        spec(
            "cinvar.timeline.transition",
            "timeline",
            "Set an in or out transition on a clip. Supported: fade, fadeblack, fadewhite, crossfade, dissolve, none. Use them sparingly - hard cuts are the default and most cuts should stay hard.",
            schema(json!({"clip_id": s("Clip id"), "side": s("in | out"), "kind": s("Transition kind"), "duration": ts("Transition length in seconds, e.g. 0.5")}), &["clip_id", "kind", "duration"]),
            false,
        ),
        |ctx: &mut ToolCtx, args: Value| {
            let clip_id = req_str(&args, "clip_id")?;
            let kind = req_str(&args, "kind")?;
            let duration = req_time(&args, "duration")?;
            let side = match opt_str(&args, "side").unwrap_or_else(|| "in".into()).to_lowercase().as_str() {
                "in" => TransitionSide::In,
                "out" => TransitionSide::Out,
                other => return Err(Error::invalid(format!("unknown transition side '{other}'"))),
            };
            ctx.record_history();
            crate::project::ops::set_transition(ctx.project, &clip_id, side, &kind, duration)?;
            ctx.persist()?;
            Ok(json!({"ok": true}))
        },
    );

    registry.add(
        spec(
            "cinvar.timeline.effect",
            "timeline",
            "Add a visual effect to a clip. Params are effect specific, e.g. brightness: {\"value\": 0.1}, blur: {\"sigma\": 3}, zoom: {\"factor\": 1.15}.",
            schema(json!({"clip_id": s("Clip id"), "kind": s("Effect kind"), "params": json!({"type": "object", "description": "Effect parameters"})}), &["clip_id", "kind"]),
            false,
        ),
        |ctx: &mut ToolCtx, args: Value| {
            let clip_id = req_str(&args, "clip_id")?;
            let kind = req_str(&args, "kind")?;
            let params = args.get("params").cloned().unwrap_or_else(|| json!({}));
            ctx.record_history();
            let effect_id = crate::project::ops::add_effect(ctx.project, &clip_id, &kind, params)?;
            ctx.persist()?;
            Ok(json!({"effect_id": effect_id}))
        },
    );

    registry.add(
        spec(
            "cinvar.timeline.keyframe",
            "timeline",
            "Add one keyframe to animate a clip property over time. Properties: x and y (pixel offset from centre; +x moves right, +y moves down), scale (1 = fit), opacity (0-1), rotation (degrees; not for text). Two or more keyframes on a property produce real animation, interpolated with the easing of the keyframe that starts each segment; before the first and after the last keyframe the value holds. `at` is a timeline time that must fall inside the clip. For common moves (push-in, pan, fades, slides) prefer cinvar.timeline.animate.",
            schema(json!({"clip_id": s("Clip id"), "property": s("Property name"), "at": ts("Timeline position"), "value": n("Value at that position"), "easing": s("linear | ease_in | ease_out | ease_in_out | hold")}), &["clip_id", "property", "at", "value"]),
            false,
        ),
        |ctx: &mut ToolCtx, args: Value| {
            let clip_id = req_str(&args, "clip_id")?;
            let property = req_str(&args, "property")?;
            let at = req_time(&args, "at")?;
            let value = opt_num(&args, "value").ok_or_else(|| Error::invalid("missing 'value'"))?;
            let easing = opt_str(&args, "easing").unwrap_or_else(|| "linear".into());
            ctx.record_history();
            crate::project::ops::add_keyframe(ctx.project, &clip_id, &property, at, value, &easing)?;
            ctx.persist()?;
            Ok(json!({"ok": true}))
        },
    );

    registry.add(
        spec(
            "cinvar.timeline.animate",
            "timeline",
            "Apply a named motion preset to a clip (replaces keyframes on the properties it sets). Presets: push_in, pull_out, pan_left, pan_right, drift (slow cinematic move on footage); fade_in, fade_out, fade_in_out; slide_in_left, slide_in_right, slide_in_up, slide_in_down, pop (for titles, logos and overlays). For text clips use text animate_in/animate_out instead of scale presets.",
            schema(json!({"clip_id": s("Clip id"), "preset": s("Preset name"), "amount": n("Strength multiplier, 1.0 default (0.1-4)"), "duration": ts("Ramp length for fades, slides and pop (default 0.5s)")}), &["clip_id", "preset"]),
            false,
        ),
        |ctx: &mut ToolCtx, args: Value| {
            let clip_id = req_str(&args, "clip_id")?;
            let preset = req_str(&args, "preset")?;
            let amount = opt_num(&args, "amount").unwrap_or(1.0);
            let duration = opt_time(&args, "duration")?;
            ctx.record_history();
            let written = crate::project::ops::apply_animation_preset(ctx.project, &clip_id, &preset, amount, duration)?;
            ctx.persist()?;
            let summary: Vec<Value> = written.iter().map(|(p, t, v)| json!({"property": p, "at_clip_seconds": t, "value": v})).collect();
            Ok(json!({"preset": preset, "keyframes": summary}))
        },
    );

    registry.add(
        spec(
            "cinvar.timeline.clear_keyframes",
            "timeline",
            "Remove keyframes from a clip, for one property or all of them.",
            schema(
                json!({"clip_id": s("Clip id"), "property": s("Property to clear (omit for all)")}),
                &["clip_id"],
            ),
            false,
        ),
        |ctx: &mut ToolCtx, args: Value| {
            let clip_id = req_str(&args, "clip_id")?;
            let property = opt_str(&args, "property");
            ctx.record_history();
            let removed =
                crate::project::ops::clear_keyframes(ctx.project, &clip_id, property.as_deref())?;
            ctx.persist()?;
            Ok(json!({"removed": removed}))
        },
    );

    registry.add(
        spec(
            "cinvar.timeline.text",
            "timeline",
            "Add a text overlay (title, lower third, caption card). Position is top, center, bottom or lower_third.",
            schema(
                json!({
                    "text": s("Text to display"),
                    "at": ts("Timeline start (default 0)"),
                    "duration": ts("How long the text is on screen (default 3s)"),
                    "size": i("Font size in pixels (default 64)"),
                    "color": s("CSS colour, e.g. white or #ffcc00"),
                    "position": s("top | center | bottom | lower_third"),
                    "outline": b("Draw an outline for readability (default true)"),
                    "template": s("Template name, e.g. title, subtitle, lower_third"),
                    "align": s("left | center | right (default center)"),
                    "background": s("Box colour behind the text for readability over busy footage, e.g. black@0.55"),
                    "font": s("Font family name or path to a .ttf/.otf file"),
                    "max_width": n("Fraction of the frame width text may use before wrapping (0.1-1, default 0.85)"),
                    "animate_in": s("none | fade | slide_up | slide_down"),
                    "animate_out": s("none | fade | slide_up | slide_down")
                }),
                &["text"],
            ),
            false,
        ),
        |ctx: &mut ToolCtx, args: Value| {
            let text_value = req_str(&args, "text")?;
            let at = opt_time(&args, "at")?.unwrap_or(Time::ZERO);
            let duration = opt_time(&args, "duration")?.unwrap_or_else(|| Time::from_secs_f64(3.0));
            let mut overlay = TextOverlay::default();
            if let Some(size) = opt_int(&args, "size") {
                overlay.size = size.clamp(8, 400) as u32;
            }
            if let Some(color) = opt_str(&args, "color") {
                overlay.color = color;
            }
            if let Some(position) = opt_str(&args, "position") {
                overlay.position = match position.to_lowercase().as_str() {
                    "top" => TextPosition::Top,
                    "center" => TextPosition::Center,
                    "bottom" => TextPosition::Bottom,
                    "lower_third" | "lowerthird" => TextPosition::LowerThird,
                    other => return Err(Error::invalid(format!("unknown position '{other}'"))),
                };
            }
            if let Some(outline) = opt_bool(&args, "outline") {
                overlay.outline_width = if outline { 3 } else { 0 };
            }
            if let Some(template) = opt_str(&args, "template") {
                overlay.template = template;
            }
            if let Some(align) = opt_str(&args, "align") {
                overlay.align = match align.to_lowercase().as_str() {
                    "left" => TextAlign::Left,
                    "center" | "centre" => TextAlign::Center,
                    "right" => TextAlign::Right,
                    other => return Err(Error::invalid(format!("unknown align '{other}'; use left, center or right"))),
                };
            }
            if let Some(bg) = opt_str(&args, "background") {
                overlay.bg_color = Some(bg);
            }
            if let Some(font) = opt_str(&args, "font") {
                overlay.font = Some(font);
            }
            if let Some(width) = opt_num(&args, "max_width") {
                overlay.max_width_pct = width.clamp(0.1, 1.0);
            }
            for (key, slot) in [("animate_in", &mut overlay.animate_in), ("animate_out", &mut overlay.animate_out)] {
                if let Some(kind) = opt_str(&args, key) {
                    let kind = kind.to_lowercase();
                    if !["none", "fade", "slide_up", "slide_down"].contains(&kind.as_str()) {
                        return Err(Error::invalid(format!("unknown {key} '{kind}'; use none, fade, slide_up or slide_down")));
                    }
                    *slot = Some(kind);
                }
            }
            ctx.record_history();
            let clip_id = crate::project::ops::add_text(ctx.project, None, &text_value, at, duration, Some(overlay))?;
            ctx.persist()?;
            Ok(json!({"clip_id": clip_id, "text": text::truncate(&text_value, 60)}))
        },
    );

    registry.add(
        spec(
            "cinvar.timeline.captions",
            "timeline",
            "Attach a caption track. Cues are rendered on export when burn_captions is enabled.",
            schema(
                json!({
                    "cues": json!({"type": "array", "items": json!({"type": "object", "properties": json!({"start": ts("Cue start"), "end": ts("Cue end"), "text": s("Cue text")}), "required": json!(["start", "end", "text"])}), "description": "Caption cues" }),
                    "language": s("Language code (default en)"),
                    "size": i("Font size (default 48)"),
                    "position": s("top | center | bottom")
                }),
                &["cues"],
            ),
            false,
        ),
        |ctx: &mut ToolCtx, args: Value| {
            let raw = args.get("cues").and_then(|v| v.as_array()).cloned().unwrap_or_default();
            if raw.is_empty() {
                return Err(Error::invalid("provide at least one cue"));
            }
            let mut cues = Vec::new();
            for cue in raw {
                cues.push(Cue {
                    start: req_time(&cue, "start")?,
                    end: req_time(&cue, "end")?,
                    text: req_str(&cue, "text")?,
                });
            }
            let mut style = CaptionStyle::default();
            if let Some(size) = opt_int(&args, "size") {
                style.size = size.clamp(8, 200) as u32;
            }
            if let Some(position) = opt_str(&args, "position") {
                style.position = match position.to_lowercase().as_str() {
                    "top" => TextPosition::Top,
                    "center" => TextPosition::Center,
                    _ => TextPosition::Bottom,
                };
            }
            let language = opt_str(&args, "language").unwrap_or_else(|| "en".into());
            ctx.record_history();
            let id = crate::project::ops::set_captions(ctx.project, cues, &language, Some(style), "tool")?;
            ctx.persist()?;
            Ok(json!({"caption_track_id": id}))
        },
    );

    registry.add(
        spec(
            "cinvar.timeline.clip_audio",
            "timeline",
            "Adjust a clip's audio: gain, fades, normalisation, mute and music ducking. Set duck_under_speech=true on music or ambience clips so they automatically drop in level whenever any other (non-ducked) audio such as dialogue or narration is playing, and recover afterwards.",
            schema(json!({"clip_id": s("Clip id"), "gain_db": n("Gain in dB"), "fade_in": ts("Fade in length"), "fade_out": ts("Fade out length"), "normalize": b("Apply loudness normalisation"), "muted": b("Mute the clip"), "duck_under_speech": b("Lower this clip while other audio (speech) plays; use for music beds")}), &["clip_id"]),
            false,
        ),
        |ctx: &mut ToolCtx, args: Value| {
            let clip_id = req_str(&args, "clip_id")?;
            ctx.record_history();
            crate::project::ops::set_clip_audio(
                ctx.project,
                &clip_id,
                opt_num(&args, "gain_db"),
                opt_time(&args, "fade_in")?,
                opt_time(&args, "fade_out")?,
                opt_bool(&args, "normalize"),
                opt_bool(&args, "muted"),
                opt_bool(&args, "duck_under_speech"),
            )?;
            ctx.persist()?;
            Ok(json!({"ok": true}))
        },
    );
    registry.add(
        spec(
            "cinvar.timeline.transform",
            "timeline",
            "Reframe a clip: scale, position, rotation, opacity and how it fits the canvas (cover crops to fill, contain letterboxes, stretch distorts). This is how vertical reframing and picture-in-picture are done.",
            schema(
                json!({
                    "clip_id": s("Clip id"),
                    "scale": n("Scale 0.1-4.0"),
                    "x": i("Horizontal offset in pixels"),
                    "y": i("Vertical offset in pixels"),
                    "rotation": n("Rotation in degrees"),
                    "opacity": n("Opacity 0-1"),
                    "fit": s("cover | contain | stretch")
                }),
                &["clip_id"],
            ),
            false,
        ),
        |ctx: &mut ToolCtx, args: Value| {
            let clip_id = req_str(&args, "clip_id")?;
            let mut transform = ctx
                .project
                .clip(&clip_id)
                .ok_or_else(|| Error::not_found(format!("clip {clip_id} not found")))?
                .transform
                .clone();
            if let Some(v) = opt_num(&args, "scale") {
                transform.scale = v.clamp(0.1, 4.0);
            }
            if let Some(v) = opt_int(&args, "x") {
                transform.x = v as i32;
            }
            if let Some(v) = opt_int(&args, "y") {
                transform.y = v as i32;
            }
            if let Some(v) = opt_num(&args, "rotation") {
                transform.rotation_deg = v;
            }
            if let Some(v) = opt_num(&args, "opacity") {
                transform.opacity = v.clamp(0.0, 1.0);
            }
            if let Some(fit) = opt_str(&args, "fit") {
                transform.fit = match fit.to_lowercase().as_str() {
                    "cover" => Fit::Cover,
                    "contain" => Fit::Contain,
                    "stretch" => Fit::Stretch,
                    other => return Err(Error::invalid(format!("unknown fit '{other}'"))),
                };
            }
            ctx.record_history();
            crate::project::ops::set_transform(ctx.project, &clip_id, transform)?;
            ctx.persist()?;
            Ok(json!({"ok": true}))
        },
    );

    registry.add(
        spec(
            "cinvar.timeline.reorder",
            "timeline",
            "Reorder a track: pass the clip ids in the order they should play and the track is rebuilt back to back from time zero. Use this instead of moving clips one at a time when the whole sequence is changing.",
            schema(json!({"track_id": s("Track id"), "clip_ids": str_list("Clip ids in playback order")}), &["track_id", "clip_ids"]),
            false,
        ),
        |ctx: &mut ToolCtx, args: Value| {
            let track_id = req_str(&args, "track_id")?;
            let ids: Vec<String> = args
                .get("clip_ids")
                .and_then(|v| v.as_array())
                .map(|a| a.iter().filter_map(|v| v.as_str().map(|s| s.to_string())).collect())
                .unwrap_or_default();
            if ids.is_empty() {
                return Err(Error::invalid("clip_ids must not be empty"));
            }
            ctx.record_history();
            crate::project::ops::reorder_track(ctx.project, &track_id, &ids)?;
            ctx.persist()?;
            Ok(json!({"ok": true, "count": ids.len()}))
        },
    );
}

pub fn register_audio(registry: &mut Registry) {
    registry.add(
        spec(
            "cinvar.audio.analyze",
            "audio",
            "Measure loudness (integrated LUFS, true peak, LRA) of an asset. Use it to decide gain and normalisation.",
            schema(json!({"asset_id": s("Asset id")}), &["asset_id"]),
            false,
        ),
        |ctx: &mut ToolCtx, args: Value| {
            let asset_id = req_str(&args, "asset_id")?;
            let asset = ctx.project.asset(&asset_id).ok_or_else(|| Error::not_found(format!("asset {asset_id} not found")))?.clone();
            let source = ctx.store.asset_path(&asset)?;
            let loudness = crate::audio::analyze_loudness(&source)?;
            Ok(json!(loudness))
        },
    );

    registry.add(
        spec(
            "cinvar.audio.clipping",
            "audio",
            "Detect whether an asset's audio is clipping.",
            schema(json!({"asset_id": s("Asset id")}), &["asset_id"]),
            false,
        ),
        |ctx: &mut ToolCtx, args: Value| {
            let asset_id = req_str(&args, "asset_id")?;
            let asset = ctx
                .project
                .asset(&asset_id)
                .ok_or_else(|| Error::not_found(format!("asset {asset_id} not found")))?
                .clone();
            let source = ctx.store.asset_path(&asset)?;
            let report = crate::audio::detect_clipping(&source)?;
            Ok(json!(report))
        },
    );

    registry.add(
        spec(
            "cinvar.audio.silence_plan",
            "audio",
            "Compute the cut list that removes silence from an asset while keeping a small natural lead-in around speech.",
            schema(json!({"asset_id": s("Asset id"), "noise_db": n("Noise floor dB (default -35)"), "min_secs": n("Minimum silence to cut (default 0.6)"), "keep_secs": n("Silence to keep around speech (default 0.15)")}), &["asset_id"]),
            false,
        ),
        |ctx: &mut ToolCtx, args: Value| {
            let asset_id = req_str(&args, "asset_id")?;
            let asset = ctx.project.asset(&asset_id).ok_or_else(|| Error::not_found(format!("asset {asset_id} not found")))?.clone();
            let source = ctx.store.asset_path(&asset)?;
            let ranges = crate::audio::silence_plan(
                &source,
                opt_num(&args, "noise_db").unwrap_or(-35.0),
                opt_num(&args, "min_secs").unwrap_or(0.6),
                opt_num(&args, "keep_secs").unwrap_or(0.15),
            )?;
            Ok(json!({"ranges": ranges.iter().map(|(a, b)| json!({"start": a.as_secs_f64(), "end": b.as_secs_f64()})).collect::<Vec<_>>()}))
        },
    );
}

pub fn register_render(registry: &mut Registry) {
    registry.add(
        spec(
            "cinvar.render.preview",
            "render",
            "Render a fast low-resolution preview MP4 of the timeline. Returns a job id; poll cinvar.render.status until the job finishes.",
            schema(json!({"range_start": ts("Optional range start"), "range_end": ts("Optional range end")}), &[]),
            false,
        ),
        |ctx: &mut ToolCtx, args: Value| {
            let output = ctx.store.renders_dir().join(format!("preview-{}.mp4", ctx.project.revision));
            let mut request = crate::render::RenderRequest::for_output(output);
            request.preview = true;
            request.range = build_range(&args)?;
            let project = ctx.project.clone();
            let store = ctx.store.clone();
            let id = ctx.jobs.spawn("render_preview", move |job| {
                let progress = |p: f32, m: &str| job.progress(p, m);
                let cancelled = || job.cancelled();
                let outcome = crate::render::render(&project, &store, &request, &progress, &cancelled)?;
                Ok(json!(outcome))
            });
            Ok(json!({"job_id": id}))
        },
    );

    registry.add(
        spec(
            "cinvar.render.export",
            "render",
            "Export the finished video. Returns a job id; poll cinvar.render.status, then run cinvar.qa.render_check on the output.",
            schema(
                json!({
                    "output": s("Output file path (default renders/<project>-<revision>.mp4)"),
                    "width": i("Optional width override"),
                    "height": i("Optional height override"),
                    "fps": s("Optional frame rate override"),
                    "crf": i("Quality 0-51, lower is better (default from project settings)"),
                    "codec": s("h264 | hevc | vp9 (default h264)"),
                    "format": s("mp4 | mov | webm"),
                    "burn_captions": b("Burn in the caption track (default true)")
                }),
                &[],
            ),
            false,
        ),
        |ctx: &mut ToolCtx, args: Value| {
            let output = match opt_str(&args, "output") {
                Some(p) => {
                    // Relative outputs are relative to the PROJECT (as the hint says), not the cwd.
                    let requested = crate::util::paths::absolutize(std::path::Path::new(&p), &ctx.store.root);
                    crate::util::paths::ensure_within(&ctx.store.root, &requested).map_err(|_| {
                        Error::new(
                            crate::error::ErrorCode::PermissionDenied,
                            format!("refusing to render outside the project: {}", requested.display()),
                        )
                        .with_hint("renders must stay inside the project, e.g. \"output\": \"renders/my-video.mp4\"")
                    })?
                }
                None => ctx
                    .store
                    .renders_dir()
                    .join(format!("{}-r{}.mp4", text::slugify(&ctx.project.name), ctx.project.revision)),
            };
            let mut request = crate::render::RenderRequest::for_output(output);
            request.width = opt_int(&args, "width").map(|v| v as u32);
            request.height = opt_int(&args, "height").map(|v| v as u32);
            request.fps = match opt_str(&args, "fps") {
                Some(v) => Some(parse_fps(&v)?),
                None => None,
            };
            request.crf = opt_int(&args, "crf").map(|v| v.clamp(0, 51) as u8);
            request.codec = opt_str(&args, "codec");
            request.format = opt_str(&args, "format");
            request.burn_captions = opt_bool(&args, "burn_captions").unwrap_or(true);
            let project = ctx.project.clone();
            let store = ctx.store.clone();
            let output_for_reply = request.output.to_string_lossy().to_string();
            // Durable, idempotent record: an identical export of the same revision is never
            // started twice, and the record survives a crash as `interrupted`.
            let durable = crate::jobstore::JobStore::open(&ctx.store.jobs_dir())?;
            durable.recover()?;
            let manifest = json!({"request": request, "project": ctx.project.name, "revision": ctx.project.revision});
            let submission = durable.submit("render_export", manifest)?;
            // A re-queued (retried) record is adopted and executed; anything else is a duplicate.
            // Only a record re-queued on purpose (`jobs.retry`) is adopted; a never-retried Queued
            // record belongs to a submission still in flight, so this call is a duplicate.
            let adopt = !submission.is_new()
                && submission.record().state == crate::jobstore::JobState::Queued
                && submission.record().retried;
            if !submission.is_new() && !adopt {
                let r = submission.record();
                return Ok(json!({"duplicate_of": r.id, "state": r.state, "output": output_for_reply, "job_id": r.remote_ref, "note": "an identical render already exists; not starting a second one"}));
            }
            let durable_id = submission.record().id.clone();
            let durable_for_job = crate::jobstore::JobStore::open(&ctx.store.jobs_dir())?;
            let durable_id_job = durable_id.clone();
            let id = ctx.jobs.spawn("render_export", move |job| {
                durable_for_job.start(&durable_id_job)?;
                let progress = |p: f32, m: &str| {
                    job.progress(p, m);
                    let _ = durable_for_job.progress(&durable_id_job, p, m);
                };
                let cancelled = || job.cancelled();
                match crate::render::render(&project, &store, &request, &progress, &cancelled) {
                    Ok(outcome) => {
                        let value = json!(outcome);
                        let _ = durable_for_job.succeed(&durable_id_job, value.clone());
                        Ok(value)
                    }
                    Err(err) => {
                        if job.cancelled() {
                            let _ = durable_for_job.update(&durable_id_job, |r| r.cancel_requested = true);
                        }
                        let _ = durable_for_job.fail(&durable_id_job, &err.message);
                        Err(err)
                    }
                }
            });
            let _ = durable.update(&durable_id, |r| r.remote_ref = Some(id.clone()));
            Ok(json!({"job_id": id, "durable_job_id": durable_id, "output": output_for_reply}))
        },
    );

    registry.add(
        spec(
            "cinvar.render.status",
            "render",
            "Check a render job. Status is queued, running, succeeded, failed or cancelled; progress is 0-1 and output carries the finished file. Pass wait_seconds to block until the job settles instead of polling.",
            schema(json!({"job_id": s("Job id from render.preview / render.export"), "wait_seconds": i("Optional: block up to this many seconds (0-600) waiting for the job to finish")}), &["job_id"]),
            false,
        ),
        |ctx: &mut ToolCtx, args: Value| {
            let job_id = req_str(&args, "job_id")?;
            if !ctx.jobs.list().iter().any(|job| job.id == job_id) {
                return Err(Error::not_found(format!("job {job_id} not found")));
            }
            let wait_secs = opt_int(&args, "wait_seconds").unwrap_or(0).clamp(0, 600) as u64;
            let snapshot = if wait_secs > 0 {
                ctx.jobs.wait(&job_id, std::time::Duration::from_secs(wait_secs))
            } else {
                ctx.jobs.get(&job_id)
            };
            let snapshot = snapshot.ok_or_else(|| Error::not_found(format!("job {job_id} not found")))?;
            Ok(json!(snapshot))
        },
    );

    registry.add(
        spec(
            "cinvar.render.cancel",
            "render",
            "Cancel a running render job.",
            schema(json!({"job_id": s("Job id")}), &["job_id"]),
            false,
        ),
        |ctx: &mut ToolCtx, args: Value| {
            let job_id = req_str(&args, "job_id")?;
            Ok(json!({"cancelled": ctx.jobs.cancel(&job_id)}))
        },
    );

    registry.add(
        spec(
            "cinvar.render.list_jobs",
            "render",
            "List render jobs started in this session.",
            schema(json!({}), &[]),
            false,
        ),
        |ctx: &mut ToolCtx, _args: Value| Ok(json!({"jobs": ctx.jobs.list()})),
    );
}

fn build_range(args: &Value) -> Result<Option<(Time, Time)>> {
    match (opt_time(args, "range_start")?, opt_time(args, "range_end")?) {
        (Some(start), Some(end)) => Ok(Some((start, end))),
        (None, None) => Ok(None),
        _ => Err(Error::invalid(
            "provide both range_start and range_end, or neither",
        )),
    }
}

pub fn register_creative(registry: &mut Registry) {
    registry.add(
        spec(
            "cinvar.skills.list",
            "skills",
            "List the installed creative editing skills. Skills explain how to edit well; load one before planning a style-specific edit.",
            schema(json!({}), &[]),
            false,
        ),
        |_ctx: &mut ToolCtx, _args: Value| {
            let skills = crate::skills::load_all()?;
            Ok(json!({"skills": skills.iter().map(|s| json!({"name": s.name, "title": s.title, "summary": s.summary, "tags": s.tags})).collect::<Vec<_>>()}))
        },
    );

    registry.add(
        spec(
            "cinvar.skills.load",
            "skills",
            "Load the full guidance for one or more skills before planning an edit in that style.",
            schema(
                json!({"names": str_list("Skill names from cinvar.skills.list")}),
                &["names"],
            ),
            false,
        ),
        |_ctx: &mut ToolCtx, args: Value| {
            let names: Vec<String> = args
                .get("names")
                .and_then(|v| v.as_array())
                .map(|a| {
                    a.iter()
                        .filter_map(|v| v.as_str().map(|s| s.to_string()))
                        .collect()
                })
                .unwrap_or_default();
            let skills = crate::skills::load_all()?;
            let mut loaded = Vec::new();
            for name in names {
                if let Some(skill) = skills.iter().find(|s| s.name == name) {
                    loaded.push(
                        json!({"name": skill.name, "title": skill.title, "body": skill.body}),
                    );
                } else {
                    loaded.push(json!({"name": name, "error": "not installed"}));
                }
            }
            Ok(json!({"skills": loaded}))
        },
    );

    registry.add(
        spec(
            "cinvar.qa.project_check",
            "qa",
            "Check the project for structural problems: missing assets, overlaps, empty timelines, zero-length clips.",
            schema(json!({}), &[]),
            false,
        ),
        |ctx: &mut ToolCtx, _args: Value| {
            let report = crate::qa::validate_project(ctx.project, ctx.store);
            Ok(json!(report))
        },
    );

    registry.add(
        spec(
            "cinvar.qa.render_check",
            "qa",
            "Validate a rendered file with ffprobe: duration against the timeline, resolution, audio presence, black frames and audio levels. Never claim a render is good without this.",
            schema(json!({"path": s("Path to the rendered file"), "expected_duration": n("Optional expected duration in seconds")}), &["path"]),
            false,
        ),
        |ctx: &mut ToolCtx, args: Value| {
            let path = req_str(&args, "path")?;
            within_agent_roots(ctx, std::path::Path::new(&path))?;
            let report = crate::qa::validate_render(ctx.project, ctx.store, std::path::Path::new(&path))?;
            Ok(json!(report))
        },
    );

    registry.add(
        spec(
            "cinvar.blender.status",
            "blender",
            "Report whether Blender is installed and usable for 3D renders.",
            schema(json!({}), &[]),
            false,
        ),
        |_ctx: &mut ToolCtx, _args: Value| match crate::blender::available() {
            Some(path) => Ok(json!({"available": true, "path": path.to_string_lossy()})),
            None => {
                Ok(json!({"available": false, "hint": "install Blender to enable 3D animation"}))
            }
        },
    );

    registry.add(
        spec(
            "cinvar.blender.animation",
            "blender",
            "Render a 3D animation clip with Blender (controlled templates only), import it into the project, and optionally place it on the timeline. Requires Blender 3.6+. Re-running the same request resumes an interrupted render.",
            schema(json!({"template": s("logo_reveal | title_3d"), "text": s("Text for the animation"), "duration": ts("Clip length in seconds (default 5)"), "color": s("Accent colour"), "width": i("Render width (default 1920)"), "height": i("Render height (default 1080)"), "fps": i("Frames per second (default 30)"), "place_at": ts("If set, insert the rendered clip on an overlay track at this time"), "timeline_id": s("Timeline to place on (default active)")}), &[]),
            false,
        ),
        |ctx: &mut ToolCtx, args: Value| {
            if !ctx.config.security.allow_blender {
                return Err(Error::new(crate::error::ErrorCode::PermissionDenied, "Blender integration is disabled in configuration"));
            }
            let mut spec = crate::blender::AnimationSpec::default();
            if let Some(template) = opt_str(&args, "template") {
                spec.template = template;
            }
            if let Some(text_value) = opt_str(&args, "text") {
                spec.text = text_value;
            }
            if let Some(color) = opt_str(&args, "color") {
                spec.color = color;
            }
            if let Some(duration) = opt_time(&args, "duration")? {
                spec.duration = duration;
            }
            for (key, slot) in [("width", &mut spec.width), ("height", &mut spec.height), ("fps", &mut spec.fps)] {
                if let Some(v) = args.get(key).and_then(|v| v.as_u64()) {
                    if !(16..=16384).contains(&v) {
                        return Err(Error::invalid(format!("{key} must be between 16 and 16384")));
                    }
                    *slot = v as u32;
                }
            }
            let base = ctx.store.cache_dir().join("blender");
            let render = crate::blender::render_animation_job(&spec, &base, &crate::blender::RenderControl::none())?;
            let asset = ctx.store.import_asset(ctx.project, &render.clip, "blender")?;
            let mut placed = Value::Null;
            if let Some(at) = opt_time(&args, "place_at")? {
                let timeline_id = opt_str(&args, "timeline_id");
                let track = crate::project::ops::ensure_track(ctx.project, timeline_id.as_deref(), crate::project::model::TrackKind::Overlay)?;
                let clip_id = crate::project::ops::insert_clip(
                    ctx.project,
                    timeline_id.as_deref(),
                    Some(&track),
                    crate::project::ops::ClipRequest { asset_id: &asset.id, source_in: None, source_out: None, at: Some(at), name: Some("Blender animation") },
                )?;
                placed = json!({"clip_id": clip_id, "track_id": track});
            }
            Ok(json!({
                "file": render.clip.to_string_lossy(),
                "asset_id": asset.id,
                "frames": render.frames,
                "resumed_frames": render.resumed_frames,
                "blender_version": render.blender_version,
                "placed": placed,
            }))
        },
    );

    registry.add(
        spec(
            "cinvar.otio.import",
            "interchange",
            "Import an OpenTimelineIO (.otio JSON) file as a NEW timeline: video/audio tracks, clips with source ranges, gaps and dissolves. Missing media becomes a gap plus a warning; unsupported items are listed, never silently dropped.",
            schema(json!({"input": s("Path to the .otio file")}), &["input"]),
            false,
        ),
        |ctx: &mut ToolCtx, args: Value| {
            let input = req_str(&args, "input")?;
            let path = std::path::PathBuf::from(&input);
            if path.extension().and_then(|e| e.to_str()).map(|e| e.eq_ignore_ascii_case("otio")) != Some(true) {
                return Err(Error::invalid("only .otio files can be imported"));
            }
            // The .otio itself is also untrusted input from an agent: it has to live in the project
            // or in a configured media root, and symlinks are resolved before the check.
            let real = path.canonicalize().map_err(|_| Error::not_found(format!("cannot read {}", path.display())))?;
            let mut allowed: Vec<std::path::PathBuf> = ctx.config.security.effective_media_roots();
            allowed.push(ctx.store.root.clone());
            if !allowed.iter().filter_map(|r| r.canonicalize().ok()).any(|r| real.starts_with(r)) {
                return Err(Error::new(crate::error::ErrorCode::PermissionDenied, "the .otio file is outside the project and the configured media roots")
                    .with_hint("copy it into the project folder or add its folder to security.media_roots"));
            }
            let report = crate::otio::import_otio_with(ctx.project, ctx.store, &real, &ctx.config.security.effective_media_roots())?;
            Ok(json!(report))
        },
    );

    registry.add(
        spec(
            "cinvar.otio.export",
            "interchange",
            "Export the timeline as an OpenTimelineIO file for DaVinci Resolve, Kdenlive or other editors. Effects without an OTIO equivalent are reported as warnings.",
            schema(json!({"output": s("Output .otio path")}), &["output"]),
            false,
        ),
        |ctx: &mut ToolCtx, args: Value| {
            let out = req_str(&args, "output")?;
            let requested = crate::util::paths::absolutize(std::path::Path::new(&out), &ctx.store.root);
            let target = crate::util::paths::ensure_within(&ctx.store.root, &requested).map_err(|_| {
                Error::new(crate::error::ErrorCode::PermissionDenied, format!("refusing to write outside the project: {}", requested.display()))
                    .with_hint("export inside the project, e.g. \"output\": \"exports/cut.otio\"")
            })?;
            if target.extension().and_then(|e| e.to_str()).map(|e| e.eq_ignore_ascii_case("otio")) != Some(true) {
                return Err(Error::invalid("the output must end in .otio"));
            }
            let report = crate::otio::export_otio_in(ctx.project, None, &target, Some(&ctx.store.root))?;
            Ok(json!(report))
        },
    );
}

pub fn register_system(registry: &mut Registry) {
    registry.add(
        spec(
            "cinvar.system.aliases",
            "system",
            "Map of logical capability names (cinvar.render, cinvar.detect_scenes, ...) to the tool that implements them, plus capabilities that are families of tools or not exposed over MCP.",
            schema(json!({}), &[]),
            false,
        ),
        |_ctx: &mut ToolCtx, _args: Value| {
            Ok(json!({
                "aliases": crate::mcp::compat::ALIASES.iter().map(|(a, n)| json!({"alias": a, "tool": n})).collect::<Vec<_>>(),
                "not_a_single_tool": crate::mcp::compat::NOT_A_SINGLE_TOOL.iter().map(|(a, n)| json!({"name": a, "note": n})).collect::<Vec<_>>(),
            }))
        },
    );

    registry.add(
        spec(
            "cinvar.plugins.list",
            "plugins",
            "Inventory of built-in integrations and external plugins: enabled state, compatibility, measured health, missing dependencies, permissions, and whether each costs money, uses a cloud service or sends files off this device. Optional integrations are never required for basic editing.",
            schema(json!({}), &[]),
            false,
        ),
        |ctx: &mut ToolCtx, _args: Value| {
            let dir = crate::util::paths::config_dir().join("plugins");
            let state = crate::plugins::registry::PluginState::load(&crate::plugins::registry::default_state_path());
            Ok(json!({
                "sandbox": if crate::plugins::sandbox_available() { "bubblewrap" } else { "none" },
                "plugin_commands_allowed": crate::plugins::execution_allowed(&ctx.config.security),
                "plugins": crate::plugins::registry::inventory(&dir, &state, crate::plugins::execution_allowed(&ctx.config.security)),
            }))
        },
    );

    registry.add(
        spec(
            "cinvar.plugins.set_enabled",
            "plugins",
            "Enable or disable a plugin by id. External plugins are disabled until enabled here. Takes effect the next time the tool server starts.",
            schema(json!({"id": s("Plugin id from cinvar.plugins.list"), "enabled": b("true to enable, false to disable")}), &["id", "enabled"]),
            false,
        ),
        |_ctx: &mut ToolCtx, args: Value| {
            let id = req_str(&args, "id")?;
            let dir = crate::util::paths::config_dir().join("plugins");
            crate::plugins::registry::require_known(&dir, &id)?;
            let enabled = opt_bool(&args, "enabled").unwrap_or(false);
            crate::plugins::registry::set_enabled(&crate::plugins::registry::default_state_path(), &id, enabled)?;
            Ok(json!({"id": id, "enabled": enabled, "applies": "on next tool-server start"}))
        },
    );

    registry.add(
        spec(
            "cinvar.plugins.pin",
            "plugins",
            "Pin a plugin to an exact version (a mismatching install is then refused) or clear the pin by omitting version.",
            schema(json!({"id": s("Plugin id"), "version": s("Exact version to require; omit to unpin")}), &["id"]),
            false,
        ),
        |_ctx: &mut ToolCtx, args: Value| {
            let id = req_str(&args, "id")?;
            let dir = crate::util::paths::config_dir().join("plugins");
            crate::plugins::registry::require_known(&dir, &id)?;
            let version = opt_str(&args, "version");
            crate::plugins::registry::set_pin(&crate::plugins::registry::default_state_path(), &id, version.as_deref())?;
            Ok(json!({"id": id, "pinned_to": version}))
        },
    );

    registry.add(
        spec(
            "cinvar.ai.workflows",
            "ai",
            "List the AI workflows (ComfyUI) registered on this machine by task, and whether the ComfyUI server is reachable. Workflows are user-supplied descriptors; no model is assumed.",
            schema(json!({}), &[]),
            false,
        ),
        |_ctx: &mut ToolCtx, _args: Value| {
            let defs = crate::comfy::load_registry(&crate::comfy::registry_dir());
            let reachable = crate::comfy::ComfyClient::from_env().and_then(|c| c.system_stats()).is_ok();
            Ok(json!({
                "server_reachable": reachable,
                "registry_dir": crate::comfy::registry_dir().to_string_lossy(),
                "tasks": crate::comfy::TASKS,
                "workflows": defs.iter().map(|d| json!({"name": d.name, "task": d.task, "description": d.description, "params": d.params.keys().collect::<Vec<_>>(), "file_params": d.file_params})).collect::<Vec<_>>(),
            }))
        },
    );

    registry.add(
        spec(
            "cinvar.ai.run_workflow",
            "ai",
            "Run a registered ComfyUI workflow as a job. Parameters bind to workflow inputs; file parameters take a project-relative path or \"asset:<id>\" and are uploaded first. The workflow is validated against the server (missing nodes/models fail clearly). Poll with cinvar.render.status; then import the result with cinvar.ai.import_outputs.",
            schema(json!({"workflow": s("Workflow name from cinvar.ai.workflows"), "params": {"type": "object", "description": "Parameter values"}, "timeout_seconds": i("Run limit (default 600, max 3600)")}), &["workflow"]),
            false,
        ),
        |ctx: &mut ToolCtx, args: Value| {
            if !ctx.config.security.allow_network {
                return Err(Error::new(crate::error::ErrorCode::PermissionDenied, "network access is disabled in configuration"));
            }
            let name = req_str(&args, "workflow")?;
            let def = crate::comfy::load_registry(&crate::comfy::registry_dir()).into_iter().find(|d| d.name == name)
                .ok_or_else(|| Error::not_found(format!("no registered workflow named '{name}'")).with_hint("see cinvar.ai.workflows"))?;
            let mut params = args.get("params").and_then(|v| v.as_object()).cloned().unwrap_or_default();
            let client = crate::comfy::ComfyClient::from_env()?;
            if !def.file_params.is_empty() && !client.is_local() && !ctx.config.security.allow_media_upload {
                return Err(Error::new(
                    crate::error::ErrorCode::PermissionDenied,
                    "this workflow would upload project media to a non-local ComfyUI server",
                )
                .with_hint("set security.allow_media_upload = true if you really intend to send media to that server"));
            }
            for key in &def.file_params {
                let Some(v) = params.get(key).and_then(|v| v.as_str()).map(String::from) else { continue };
                let local = match v.strip_prefix("asset:") {
                    Some(id) => {
                        let asset = ctx.project.asset(id).ok_or_else(|| Error::not_found(format!("asset {id} not in project")))?;
                        ctx.store.asset_path(asset)?
                    }
                    None => crate::util::paths::ensure_within(&ctx.store.root, std::path::Path::new(&v))?,
                };
                let remote_name = format!("cinvar_{}", local.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default());
                let uploaded = client.upload(&local, &remote_name)?;
                params.insert(key.clone(), json!(uploaded));
            }
            let workflow = crate::comfy::bind(&def, &params)?;
            let dest = ctx.store.cache_dir().join("comfy");
            let timeout = std::time::Duration::from_secs(opt_int(&args, "timeout_seconds").unwrap_or(600).clamp(10, 3600) as u64);
            let id = ctx.jobs.spawn("ai_workflow", move |job| {
                let progress = |p: f32, m: &str| job.progress(p, m);
                let cancelled = || job.cancelled();
                let r = crate::comfy::run(&client, &workflow, &dest, timeout, std::time::Duration::from_millis(500), &cancelled, &progress)?;
                Ok(json!({"prompt_id": r.prompt_id, "files": r.files.iter().map(|p| p.to_string_lossy().to_string()).collect::<Vec<_>>()}))
            });
            Ok(json!({"job_id": id}))
        },
    );

    registry.add(
        spec(
            "cinvar.ai.import_outputs",
            "ai",
            "Import the finished outputs of an AI workflow job into the project as assets, optionally placing the first video/image on the timeline.",
            schema(json!({"job_id": s("Job id from cinvar.ai.run_workflow"), "place_at": ts("Insert the first output at this time on a video track")}), &["job_id"]),
            false,
        ),
        |ctx: &mut ToolCtx, args: Value| {
            let job_id = req_str(&args, "job_id")?;
            let snap = ctx.jobs.get(&job_id).ok_or_else(|| Error::not_found(format!("job {job_id} not found")))?;
            if snap.status != crate::jobs::JobStatus::Succeeded {
                return Err(Error::new(crate::error::ErrorCode::Conflict, format!("job {job_id} is {}, not succeeded", snap.status.as_str())));
            }
            let files: Vec<String> = snap.output.as_ref().and_then(|o| o["files"].as_array()).map(|a| a.iter().filter_map(|v| v.as_str().map(String::from)).collect()).unwrap_or_default();
            let place = opt_time(&args, "place_at")?;
            let mut imported = Vec::new();
            let mut placed = Value::Null;
            for f in files {
                let asset = ctx.store.import_asset(ctx.project, std::path::Path::new(&f), "comfyui")?;
                if placed.is_null() {
                    if let (Some(at), Some(info)) = (place, asset.info.as_ref()) {
                        if info.has_video {
                            let clip = crate::project::ops::insert_clip(ctx.project, None, None, crate::project::ops::ClipRequest { asset_id: &asset.id, source_in: None, source_out: None, at: Some(at), name: Some("AI workflow") })?;
                            placed = json!({"clip_id": clip});
                        }
                    }
                }
                imported.push(asset.id);
            }
            Ok(json!({"assets": imported, "placed": placed}))
        },
    );

    registry.add(
        spec(
            "cinvar.cloud.status",
            "cloud",
            "Report whether cloud rendering is enabled, whether a RunPod key is configured and (if so) whether it authenticates. Makes one read-only API call; creates nothing and costs nothing.",
            schema(json!({}), &[]),
            false,
        ),
        |ctx: &mut ToolCtx, _args: Value| {
            let key = crate::secrets::runpod_key();
            let mut out = json!({"allowed_by_config": ctx.config.security.allow_cloud_render, "key_configured": key.is_some(), "provider": "runpod (pods)"});
            if let Some(key) = key {
                let client = crate::cloud::runpod::RunpodClient::new(&std::env::var("CINVAR_RUNPOD_BASE").unwrap_or_else(|_| crate::cloud::runpod::DEFAULT_BASE.into()), &key);
                out["key_valid"] = json!(client.validate_credentials().is_ok());
            }
            Ok(out)
        },
    );

    registry.add(
        spec(
            "cinvar.render.cloud",
            "cloud",
            "Render on a RunPod GPU pod. TWO STEPS. Step 1 (no confirm_quote): returns a quote with GPU candidates, your price ceiling, runtime limit and the worst-case cost; nothing is created or billed. Step 2: call again with confirm_quote set to the quote id to provision a billable pod, upload the project, render, download the verified result and terminate the pod. Requires security.allow_cloud_render=true and a RunPod key. The result is a job; poll with cinvar.render.status.",
            schema(json!({
                "max_price_per_hr": {"type": "number", "description": "REQUIRED ceiling in USD/hour; a pod priced above it is terminated immediately"},
                "max_runtime_minutes": i("REQUIRED runtime limit in minutes (max 1440)"),
                "image": s("Worker container image (see deploy/runpod/Dockerfile)"),
                "size_preset": s("720p | 1080p | 1440p | 4k | 8k"),
                "width": i("Custom width"), "height": i("Custom height"),
                "codec": s("h264 | hevc | vp9"), "bit_depth": i("8 or 10"),
                "min_vram_gb": i("Minimum GPU memory (default 24)"),
                "gpu_type_ids": {"type": "array", "items": {"type": "string"}, "description": "Explicit RunPod gpuTypeIds"},
                "output": s("Output path inside the project"),
                "confirm_quote": s("Quote id from step 1: explicit approval to spend"),
                "max_spend_usd": n("Optional personal spending limit in USD; quotes whose worst case exceeds it are refused"),
                "acknowledge_billing_risk": b("Required with confirm_quote: you accept that CINVAR cannot cap RunPod charges at the provider")
            }), &["max_price_per_hr", "max_runtime_minutes", "image"]),
            false,
        ),
        |ctx: &mut ToolCtx, args: Value| {
            if !ctx.config.security.allow_cloud_render {
                return Err(Error::new(crate::error::ErrorCode::PermissionDenied, "cloud rendering is disabled in configuration")
                    .with_hint("set security.allow_cloud_render = true in the CINVAR config to allow sending projects to a paid GPU"));
            }
            let key = crate::secrets::runpod_key().ok_or_else(|| Error::new(crate::error::ErrorCode::DependencyMissing, "no RunPod API key is configured")
                .with_hint("set CINVAR_RUNPOD_API_KEY or run: cinvar cloud connect"))?;
            let base = std::env::var("CINVAR_RUNPOD_BASE").unwrap_or_else(|_| crate::cloud::runpod::DEFAULT_BASE.into());
            let ceiling = args.get("max_price_per_hr").and_then(|v| v.as_f64()).unwrap_or(0.0);
            let minutes = opt_int(&args, "max_runtime_minutes").unwrap_or(0).max(0) as u64;
            let image = req_str(&args, "image")?;
            let mut opts = crate::cloud::orchestrator::CloudOptions::production(ceiling, minutes * 60, &image);
            opts.max_spend_usd = args.get("max_spend_usd").and_then(|v| v.as_f64());
            opts.acknowledge_billing_risk = opt_bool(&args, "acknowledge_billing_risk").unwrap_or(false);
            if let Some(v) = opt_int(&args, "min_vram_gb") { opts.min_vram_gb = v.clamp(1, 512) as u32; }
            if let Some(list) = args.get("gpu_type_ids").and_then(|v| v.as_array()) {
                opts.gpu_type_ids = list.iter().filter_map(|v| v.as_str().map(String::from)).collect();
            }
            let output = match opt_str(&args, "output") {
                Some(p) => crate::util::paths::ensure_within(&ctx.store.root, std::path::Path::new(&p))?,
                None => ctx.store.renders_dir().join(format!("{}-cloud-r{}.mp4", text::slugify(&ctx.project.name), ctx.project.revision)),
            };
            let mut request = crate::render::RenderRequest::for_output(&output);
            request.size_preset = opt_str(&args, "size_preset");
            request.width = opt_int(&args, "width").map(|v| v as u32);
            request.height = opt_int(&args, "height").map(|v| v as u32);
            request.codec = opt_str(&args, "codec");
            request.bit_depth = opt_int(&args, "bit_depth").map(|v| v as u8);
            let (manifest, files) = crate::cloud::manifest::build(ctx.project, ctx.store, &request)?;
            let quote = crate::cloud::orchestrator::quote(&manifest, &opts)?;
            let Some(confirm) = opt_str(&args, "confirm_quote") else {
                crate::cloud::approval::record_pending(&quote, &ctx.store.root)?;
                return Ok(json!({"quote": quote, "next": format!("show this quote to the user. Billable rendering starts only after THE USER approves it in a terminal with `cinvar cloud approve {}`; then call again with confirm_quote set to quote.id and acknowledge_billing_risk=true", quote.id), "billed": false, "needs_human_approval": true}));
            };
            // Refuse synchronously, before any background work exists, so the agent is told "no".
            if !opts.acknowledge_billing_risk {
                return Err(Error::new(
                    crate::error::ErrorCode::Budget,
                    "CINVAR cannot cap RunPod spending at the provider; nothing was started",
                )
                .with_hint("show the quote to the user and re-call with acknowledge_billing_risk=true only after they accept"));
            }
            if confirm != quote.id {
                return Err(Error::new(crate::error::ErrorCode::Budget, "confirm_quote does not match the current quote; nothing was started")
                    .with_hint(format!("current quote id: {}", quote.id)));
            }
            // The arguments above are the agent's words; this record is the human's.
            crate::cloud::approval::consume(&quote.id)?;
            let store_dir = ctx.store.jobs_dir();
            let job_id = ctx.jobs.spawn("render_cloud", move |job| {
                let client = crate::cloud::runpod::RunpodClient::new(&base, &key);
                let durable = crate::jobstore::JobStore::open(&store_dir)?;
                let progress = |p: f32, m: &str| job.progress(p, m);
                let cancelled = || job.cancelled();
                let outcome = crate::cloud::orchestrator::run(&crate::cloud::orchestrator::CloudRun {
                    client: &client, jobs: &durable, manifest: &manifest, files: &files, opts: &opts,
                    output: &output, confirm_quote_id: &confirm, cancelled: &cancelled, progress: &progress,
                })?;
                Ok(json!(outcome))
            });
            Ok(json!({"job_id": job_id, "billed": "possible: a RunPod pod may be created and billed once credentials validate; poll cinvar.render.status", "quote": quote}))
        },
    );

    registry.add(
        spec(
            "cinvar.cloud.cleanup",
            "cloud",
            "Terminate RunPod pods left behind by THIS project's interrupted, failed, cancelled or uncertain cloud jobs. Never touches pods that belong to other projects or running jobs.",
            schema(json!({}), &[]),
            true,
        ),
        |ctx: &mut ToolCtx, _args: Value| {
            if !ctx.config.security.allow_cloud_render {
                return Err(Error::new(crate::error::ErrorCode::PermissionDenied, "cloud rendering is disabled in configuration"));
            }
            let key = crate::secrets::runpod_key().ok_or_else(|| Error::new(crate::error::ErrorCode::DependencyMissing, "no RunPod API key is configured"))?;
            let base = std::env::var("CINVAR_RUNPOD_BASE").unwrap_or_else(|_| crate::cloud::runpod::DEFAULT_BASE.into());
            let client = crate::cloud::runpod::RunpodClient::new(&base, &key);
            let jobs = crate::jobstore::JobStore::open(&ctx.store.jobs_dir())?;
            jobs.recover()?;
            let released = crate::cloud::orchestrator::cleanup_orphans(&client, &jobs, std::time::Duration::from_millis(200))?;
            Ok(json!({"released_pods": released}))
        },
    );

    registry.add(
        spec(
            "cinvar.jobs.list",
            "render",
            "List durable job records for this project (survive restarts). States: queued, running, succeeded, failed, cancelled, interrupted (owner process died; never auto-restarted), uncertain (remote outcome unknown). Runs crash recovery first.",
            schema(json!({}), &[]),
            false,
        ),
        |ctx: &mut ToolCtx, _args: Value| {
            let store = crate::jobstore::JobStore::open(&ctx.store.jobs_dir())?;
            let recovered = store.recover()?;
            Ok(json!({"jobs": store.list()?, "recovered_as_interrupted": recovered.iter().map(|r| r.id.clone()).collect::<Vec<_>>()}))
        },
    );

    registry.add(
        spec(
            "cinvar.jobs.retry",
            "render",
            "Explicitly re-queue a failed, cancelled or interrupted durable job (bounded attempts). Uncertain remote jobs must be reconciled first. This only re-queues the record; run the render tool again to execute.",
            schema(json!({"job_id": s("Durable job id")}), &["job_id"]),
            false,
        ),
        |ctx: &mut ToolCtx, args: Value| {
            let store = crate::jobstore::JobStore::open(&ctx.store.jobs_dir())?;
            Ok(json!(store.retry(&req_str(&args, "job_id")?)?))
        },
    );

    registry.add(
        spec(
            "cinvar.render.preflight",
            "render",
            "Check, WITHOUT rendering, whether a render of this timeline is feasible on this machine: measured encoder support at the exact size (trial encode), estimated memory/disk against what is free, 8-bit/10-bit, and whether sources would be enlarged by the scale filter. Presets: 720p, 1080p, 1440p, 4k, 8k.",
            schema(json!({"size_preset": s("720p | 1080p | 1440p | 4k | 8k"), "width": i("Custom width"), "height": i("Custom height"), "codec": s("h264 | hevc | vp9"), "bit_depth": i("8 or 10"), "hardware": s("auto | off | force")}), &[]),
            false,
        ),
        |ctx: &mut ToolCtx, args: Value| {
            let mut req = crate::render::RenderRequest::for_output(ctx.store.renders_dir().join("preflight.mp4"));
            req.size_preset = opt_str(&args, "size_preset");
            req.width = args.get("width").and_then(|v| v.as_u64()).map(|v| v as u32);
            req.height = args.get("height").and_then(|v| v.as_u64()).map(|v| v as u32);
            req.codec = opt_str(&args, "codec");
            req.bit_depth = args.get("bit_depth").and_then(|v| v.as_u64()).map(|v| v as u8);
            if let Some(hw) = opt_str(&args, "hardware") {
                req.hardware = Some(match hw.as_str() {
                    "off" => crate::project::model::HwMode::Off,
                    "force" => crate::project::model::HwMode::Force,
                    _ => crate::project::model::HwMode::Auto,
                });
            }
            let plan = crate::render::plan_render(ctx.project, ctx.store, &req)?;
            Ok(json!({"width": plan.width, "height": plan.height, "encoder": plan.encoder, "preflight": plan.preflight, "warnings": plan.warnings}))
        },
    );

    registry.add(
        spec(
            "cinvar.system.hardware",
            "system",
            "Measure this machine for rendering: CPU threads, free memory, GPUs, which hardware encoders actually work and the largest size preset each passed a trial encode at.",
            schema(json!({}), &[]),
            false,
        ),
        |_ctx: &mut ToolCtx, _args: Value| Ok(json!(crate::render::capability::host_profile())),
    );

    registry.add(
        spec(
            "cinvar.system.capabilities",
            "system",
            "Report which external tools and integrations are available on this machine (ffmpeg, ffprobe, fonts, hardware encoders, Blender).",
            schema(json!({}), &[]),
            false,
        ),
        |_ctx: &mut ToolCtx, _args: Value| {
            let mut capabilities = serde_json::to_value(crate::cli::doctor_report()).unwrap_or(Value::Null);
            if let Value::Object(map) = &mut capabilities {
                let engine = crate::captions::detect_engine();
                map.insert(
                    "transcription".to_string(),
                    json!({
                        "available": engine.is_some(),
                        "engine": engine.as_ref().map(|e| e.name()),
                        "hint": if engine.is_none() { Some(crate::captions::engine_help()) } else { None },
                    }),
                );
            }
            Ok(capabilities)
        },
    );
}

fn cue_summary(cues: &[Cue]) -> Value {
    let preview: Vec<Value> = cues
        .iter()
        .take(3)
        .map(
            |c| json!({"start": c.start.as_secs_f64(), "end": c.end.as_secs_f64(), "text": c.text}),
        )
        .collect();
    json!({"cue_count": cues.len(), "first_cues": preview})
}

fn caption_style_from(args: &Value) -> Result<CaptionStyle> {
    let mut style = CaptionStyle::default();
    if let Some(size) = opt_int(args, "size") {
        style.size = size.clamp(8, 200) as u32;
    }
    if let Some(color) = opt_str(args, "color") {
        style.color = color;
    }
    if let Some(bg) = opt_str(args, "background") {
        style.background = Some(bg);
    }
    if let Some(upper) = opt_bool(args, "uppercase") {
        style.uppercase = upper;
    }
    if let Some(position) = opt_str(args, "position") {
        style.position = match position.to_lowercase().as_str() {
            "top" => TextPosition::Top,
            "center" => TextPosition::Center,
            "bottom" => TextPosition::Bottom,
            "lower_third" => TextPosition::LowerThird,
            other => {
                return Err(Error::invalid(format!(
                    "unknown position '{other}'; use top, center, bottom or lower_third"
                )))
            }
        };
    }
    Ok(style)
}

fn caption_style_props() -> Value {
    json!({
        "size": i("Font size (default 48)"),
        "color": s("Text colour"),
        "background": s("Box behind captions for readability, e.g. black@0.6"),
        "uppercase": b("Render captions in capitals"),
        "position": s("top | center | bottom | lower_third"),
        "language": s("Language code (default en)")
    })
}

pub fn register_captions(registry: &mut Registry) {
    registry.add(
        spec(
            "cinvar.captions.import",
            "captions",
            "Import captions from an .srt or .vtt file (for example one exported by a transcription tool). Cue times are timeline times; use offset to shift them.",
            schema(
                {
                    let mut props = caption_style_props();
                    props["path"] = s("Path to the .srt or .vtt file");
                    props["offset"] = ts("Shift all cues by this much (default 0)");
                    props
                },
                &["path"],
            ),
            false,
        ),
        |ctx: &mut ToolCtx, args: Value| {
            let path = req_str(&args, "path")?;
            let offset = opt_time(&args, "offset")?.unwrap_or(Time::ZERO);
            within_agent_roots(ctx, std::path::Path::new(&path))?;
            let (mut cues, skipped) = crate::captions::read_subtitle_file(std::path::Path::new(&path))?;
            for cue in cues.iter_mut() {
                cue.start = cue.start + offset;
                cue.end = cue.end + offset;
            }
            let style = caption_style_from(&args)?;
            let language = opt_str(&args, "language").unwrap_or_else(|| "en".into());
            ctx.record_history();
            let id = crate::project::ops::set_captions(ctx.project, cues.clone(), &language, Some(style), "import")?;
            ctx.persist()?;
            let mut out = cue_summary(&cues);
            out["caption_track_id"] = json!(id);
            out["skipped_blocks"] = json!(skipped);
            Ok(out)
        },
    );

    registry.add(
        spec(
            "cinvar.captions.export",
            "captions",
            "Write the project's caption cues to an .srt or .vtt file inside the project (for upload to YouTube or other editors). The captions stay editable in the project.",
            schema(json!({"output": s("Output path inside the project ending in .srt or .vtt, e.g. exports/captions.srt"), "track_id": s("Caption track id (default: the first)")}), &["output"]),
            false,
        ),
        |ctx: &mut ToolCtx, args: Value| {
            let out = req_str(&args, "output")?;
            let target = crate::util::paths::ensure_within(&ctx.store.root, &crate::util::paths::absolutize(std::path::Path::new(&out), &ctx.store.root))?;
            let ext = target.extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase();
            if ext != "srt" && ext != "vtt" {
                return Err(Error::invalid("the output must end in .srt or .vtt"));
            }
            let track = match opt_str(&args, "track_id") {
                Some(id) => ctx.project.captions.iter().find(|c| c.id == id),
                None => ctx.project.captions.first(),
            }
            .ok_or_else(|| Error::not_found("no caption track in this project"))?;
            if let Some(parent) = target.parent() {
                crate::util::paths::ensure_dir(parent)?;
            }
            crate::util::paths::write_atomic(&target, crate::captions::to_subtitles(&track.cues, ext == "vtt").as_bytes())?;
            Ok(json!({"path": target.to_string_lossy(), "cues": track.cues.len(), "format": ext}))
        },
    );

    registry.add(
        spec(
            "cinvar.captions.from_script",
            "captions",
            "Make captions from a known script (narration text, voice-over, lyrics) by splitting it into readable phrases and spreading them across a time span in proportion to their length. This is an ESTIMATE of timing, not forced alignment: check a few frames against the audio and adjust start/end if speech begins late or ends early (cinvar.media.silence helps find where speech starts and stops).",
            schema(
                {
                    let mut props = caption_style_props();
                    props["script"] = s("The spoken text");
                    props["start"] = ts("When the speech starts on the timeline");
                    props["end"] = ts("When the speech ends on the timeline");
                    props["max_chars"] = i("Maximum characters per caption line (default 32)");
                    props
                },
                &["script", "start", "end"],
            ),
            false,
        ),
        |ctx: &mut ToolCtx, args: Value| {
            let script = req_str(&args, "script")?;
            let start = req_time(&args, "start")?;
            let end = req_time(&args, "end")?;
            let max_chars = opt_int(&args, "max_chars").unwrap_or(32).clamp(8, 80) as usize;
            let cues = crate::captions::time_script(&script, start, end, max_chars)?;
            let style = caption_style_from(&args)?;
            let language = opt_str(&args, "language").unwrap_or_else(|| "en".into());
            ctx.record_history();
            let id = crate::project::ops::set_captions(ctx.project, cues.clone(), &language, Some(style), "script")?;
            ctx.persist()?;
            let mut out = cue_summary(&cues);
            out["caption_track_id"] = json!(id);
            out["timing"] = json!("estimated from phrase length; verify against the audio");
            Ok(out)
        },
    );

    registry.add(
        spec(
            "cinvar.captions.transcribe",
            "captions",
            "Transcribe the speech of a timeline clip into captions using an OPTIONAL local engine (openai-whisper, whisper.cpp, or a custom command in CINVAR_TRANSCRIBE_CMD). If no engine is installed this returns an error explaining the options; fall back to cinvar.captions.from_script or cinvar.captions.import. Cue times are mapped onto the timeline using the clip's position and trim.",
            schema(
                {
                    let mut props = caption_style_props();
                    props["clip_id"] = s("Timeline clip whose audio should be transcribed");
                    props
                },
                &["clip_id"],
            ),
            false,
        ),
        |ctx: &mut ToolCtx, args: Value| {
            let clip_id = req_str(&args, "clip_id")?;
            let (asset_id, clip_start, source_in, source_out, speed) = {
                let clip = ctx.project.clip(&clip_id).ok_or_else(|| Error::not_found(format!("clip {clip_id} not found")))?;
                let asset = clip.asset.clone().ok_or_else(|| Error::invalid("that clip has no media to transcribe"))?;
                (asset, clip.timeline_start, clip.source_in, clip.source_out, clip.speed)
            };
            let engine = crate::captions::detect_engine()
                .ok_or_else(|| Error::new(crate::error::ErrorCode::DependencyMissing, crate::captions::engine_help()))?;
            let asset = ctx.project.asset(&asset_id).ok_or_else(|| Error::not_found("asset missing"))?.clone();
            let source = ctx.store.asset_path(&asset)?;
            let work = ctx.store.cache_dir().join("transcribe").join(&clip_id);
            std::fs::create_dir_all(&work).map_err(|e| Error::io(format!("cannot create {}: {e}", work.display())))?;
            let wav = work.join("speech.wav");
            crate::media::extract_audio(&source, &wav)?;
            let language = opt_str(&args, "language");
            let srt = crate::captions::run_engine(&engine, &wav, &work, language.as_deref())?;
            let (raw, skipped) = crate::captions::parse_subtitles(&srt);
            let mut cues = Vec::new();
            let speed = if speed.is_finite() && speed > 0.01 { speed } else { 1.0 };
            for cue in raw {
                // Keep only speech inside the trimmed window, then map source time to timeline time.
                if cue.end <= source_in || cue.start >= source_out {
                    continue;
                }
                let s0 = (cue.start.max(source_in) - source_in).scale(1.0 / speed);
                let e0 = (cue.end.min(source_out) - source_in).scale(1.0 / speed);
                cues.push(Cue { start: (clip_start + s0), end: (clip_start + e0), text: cue.text });
            }
            if cues.is_empty() {
                return Err(Error::invalid("the engine found no speech inside the trimmed part of that clip"));
            }
            let style = caption_style_from(&args)?;
            let lang = language.unwrap_or_else(|| "en".into());
            ctx.record_history();
            let id = crate::project::ops::set_captions(ctx.project, cues.clone(), &lang, Some(style), engine.name())?;
            ctx.persist()?;
            let mut out = cue_summary(&cues);
            out["caption_track_id"] = json!(id);
            out["engine"] = json!(engine.name());
            out["skipped_blocks"] = json!(skipped);
            Ok(out)
        },
    );

    registry.add(
        spec(
            "cinvar.captions.style",
            "captions",
            "Restyle the existing caption track (size, colour, background box, case, position) without retyping cues.",
            schema(caption_style_props(), &[]),
            false,
        ),
        |ctx: &mut ToolCtx, args: Value| {
            let style = caption_style_from(&args)?;
            let explicit = |key: &str| args.get(key).is_some();
            if ctx.project.captions.is_empty() {
                return Err(Error::invalid("there is no caption track yet; create one with cinvar.captions.import or from_script"));
            }
            ctx.record_history();
            for track in ctx.project.captions.iter_mut() {
                if explicit("size") { track.style.size = style.size; }
                if explicit("color") { track.style.color = style.color.clone(); }
                if explicit("background") { track.style.background = style.background.clone(); }
                if explicit("uppercase") { track.style.uppercase = style.uppercase; }
                if explicit("position") { track.style.position = style.position; }
            }
            ctx.persist()?;
            Ok(json!({"ok": true}))
        },
    );
}

pub fn register_color(registry: &mut Registry) {
    registry.add(
        spec(
            "cinvar.color.inspect",
            "color",
            "List the colour spaces and roles of an OpenColorIO config (project color/config.ocio, $OCIO, or the OCIO builtin) and report which one was used. CINVAR applies no colour management unless a colour step is added explicitly.",
            schema(json!({"config": s("Optional OCIO config path inside the project, or an ocio:// builtin name")}), &[]),
            false,
        ),
        |ctx: &mut ToolCtx, args: Value| {
            let choice = color_choice(ctx, &args)?;
            crate::color::inspect(&choice).map(|v| json!(v))
        },
    );

    registry.add(
        spec(
            "cinvar.color.apply_transform",
            "color",
            "Bake ONE explicit OpenColorIO transform (source colour space -> destination colour space, names exactly as listed by cinvar.color.inspect) into a .cube LUT inside the project and apply it to a clip as a visible 'lut' effect. Nothing is guessed: unknown names are an error. Remove the effect to undo.",
            schema(
                json!({
                    "clip_id": s("Clip to apply the transform to"),
                    "source": s("Source colour space name"),
                    "destination": s("Destination colour space name"),
                    "size": i("LUT cube size 5..65 (default 33)"),
                    "config": s("Optional OCIO config path inside the project, or an ocio:// builtin name")
                }),
                &["clip_id", "source", "destination"],
            ),
            false,
        ),
        |ctx: &mut ToolCtx, args: Value| {
            let clip_id = req_str(&args, "clip_id")?;
            let src = req_str(&args, "source")?;
            let dst = req_str(&args, "destination")?;
            let size = opt_int(&args, "size").unwrap_or(33).clamp(5, 65) as u32;
            if ctx.project.clip(&clip_id).is_none() {
                return Err(Error::not_found(format!("clip {clip_id} not found")));
            }
            let choice = color_choice(ctx, &args)?;
            let rel = format!("luts/{}-to-{}-{}.cube", text::slugify(&src), text::slugify(&dst), size);
            let out = ctx.store.root.join(&rel);
            crate::util::paths::unlink_if_symlink(&out);
            let baked = crate::color::bake_lut(&choice, &src, &dst, size, &out)?;
            ctx.record_history();
            let fx = crate::project::ops::add_effect(
                ctx.project,
                &clip_id,
                "lut",
                json!({"file": rel, "ocio_source": src, "ocio_destination": dst, "config": choice.source}),
            )?;
            ctx.persist()?;
            Ok(json!({"effect_id": fx, "lut": rel, "baked": baked, "note": "the clip's pixels are now transformed on render; remove the lut effect to undo"}))
        },
    );

    registry.add(
        spec(
            "cinvar.color.import_lut",
            "color",
            "Validate an existing .cube 3D LUT and copy it into the project's luts/ folder so a 'lut' effect can use it.",
            schema(json!({"path": s("Path to a .cube file")}), &["path"]),
            false,
        ),
        |ctx: &mut ToolCtx, args: Value| {
            let path = std::path::PathBuf::from(req_str(&args, "path")?);
            if path.extension().and_then(|e| e.to_str()).map(|e| e.eq_ignore_ascii_case("cube")) != Some(true) {
                return Err(Error::invalid("only .cube LUT files can be imported"));
            }
            within_agent_roots(ctx, &path)?;
            let size = crate::color::validate_cube(&path)?;
            let name = path.file_name().and_then(|n| n.to_str()).map(text::slugify).unwrap_or_else(|| "lut".into());
            let rel = format!("luts/{name}.cube");
            let dest = ctx.store.root.join(&rel);
            crate::util::paths::ensure_dir(&ctx.store.root.join("luts"))?;
            crate::util::paths::unlink_if_symlink(&dest);
            std::fs::copy(&path, &dest)?;
            Ok(json!({"lut": rel, "size": size}))
        },
    );
}

fn color_choice(ctx: &ToolCtx, args: &Value) -> Result<crate::color::ConfigChoice> {
    match opt_str(args, "config") {
        Some(c) if c.starts_with("ocio://") => Ok(crate::color::ConfigChoice {
            spec: c,
            source: "explicit builtin".into(),
        }),
        Some(c) => {
            let p = crate::util::paths::ensure_within(&ctx.store.root, std::path::Path::new(&c))?;
            Ok(crate::color::discover_config(Some(&p), &ctx.store.root))
        }
        None => Ok(crate::color::discover_config(None, &ctx.store.root)),
    }
}

/// Agent-facing file boundary: agents may read files inside the project plus the folders the
/// human configured (`security.media_roots`, `CINVAR_MEDIA_ROOTS`, `--media-root`). Symlinks are
/// resolved before the check. Footage elsewhere is brought in by the human (`cinvar media import`,
/// `cinvar connect --footage`) or by adding its folder as a media root.
fn within_agent_roots(ctx: &ToolCtx, path: &std::path::Path) -> Result<std::path::PathBuf> {
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
            format!(
                "{} is outside the project; agents may only read inside it and the folders you allow",
                path.display()
            ),
        )
        .with_hint("bring it in yourself with `cinvar media import FILE`, or allow its folder: `--media-root DIR`, CINVAR_MEDIA_ROOTS, or security.media_roots in the configuration"))
    }
}
