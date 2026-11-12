use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::error::{Error, Result};
use crate::project::model::{Clip, Project, Timeline, Track, TrackKind};
use crate::project::store::PROJECT_FILE;
use crate::util::paths;
use crate::util::time::Time;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InterchangeReport {
    pub path: String,
    pub warnings: Vec<String>,
}

const MAX_ROOT_WALK: usize = 6;

fn project_root_hint(out: &Path) -> Option<PathBuf> {
    let start = out.parent()?;
    let mut current = Some(start);
    for _ in 0..MAX_ROOT_WALK {
        let dir = current?;
        if dir.join(PROJECT_FILE).is_file() {
            return Some(dir.to_path_buf());
        }
        current = dir.parent();
    }
    None
}

fn media_target(relative: &str, root: Option<&Path>) -> (String, bool) {
    let path = Path::new(relative);
    if path.is_absolute() {
        return (relative.to_string(), true);
    }
    let Some(root) = root else {
        let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        return (cwd.join(path).to_string_lossy().to_string(), false);
    };
    let joined = root.join(path);
    match std::fs::canonicalize(&joined) {
        Ok(canonical) => (canonical.to_string_lossy().to_string(), true),
        Err(_) => (joined.to_string_lossy().to_string(), false),
    }
}

fn round3(value: f64) -> f64 {
    (value * 1000.0).round() / 1000.0
}

fn rational_time(seconds: f64, rate: f64) -> Value {
    json!({
        "OTIO_SCHEMA": "RationalTime.1",
        "value": round3(seconds.max(0.0) * rate),
        "rate": rate,
    })
}

fn time_range(start_secs: f64, duration_secs: f64, rate: f64) -> Value {
    json!({
        "OTIO_SCHEMA": "TimeRange.1",
        "start_time": rational_time(start_secs, rate),
        "duration": rational_time(duration_secs, rate),
    })
}

fn clip_rate(project: &Project, clip: &Clip, timeline: &Timeline) -> f64 {
    let asset_rate = clip
        .asset
        .as_deref()
        .and_then(|id| project.asset(id))
        .and_then(|asset| asset.info.as_ref())
        .and_then(|info| info.fps)
        .map(|fps| fps.as_f64());
    match asset_rate {
        Some(rate) if rate > 0.0 => rate,
        _ => timeline.fps.as_f64(),
    }
}

fn describe(clip: &Clip) -> String {
    if !clip.name.trim().is_empty() {
        clip.name.clone()
    } else if let Some(asset) = &clip.asset {
        asset.clone()
    } else {
        clip.id.clone()
    }
}

fn media_reference(
    project: &Project,
    clip: &Clip,
    root: Option<&Path>,
    warnings: &mut Vec<String>,
) -> Value {
    let name = describe(clip);
    let Some(asset_id) = clip.asset.as_deref() else {
        warnings.push(format!(
            "clip '{name}' has no media asset (text or generated content); exported as a missing reference"
        ));
        return json!({
            "OTIO_SCHEMA": "MissingReference.1",
            "name": name,
            "metadata": {},
            "available_range": null,
        });
    };
    let Some(asset) = project.asset(asset_id) else {
        warnings.push(format!(
            "clip '{name}' references unknown asset {asset_id}; exported as a missing reference"
        ));
        return json!({
            "OTIO_SCHEMA": "MissingReference.1",
            "name": name,
            "metadata": {},
            "available_range": null,
        });
    };
    let (target, resolved) = media_target(&asset.path, root);
    if !resolved {
        warnings.push(format!(
            "clip '{name}': could not resolve {} to an absolute path; exported the unresolved path",
            asset.path
        ));
    } else if !Path::new(&target).exists() {
        warnings.push(format!(
            "clip '{name}': media file is missing on disk ({target})"
        ));
    }
    json!({
        "OTIO_SCHEMA": "ExternalReference.1",
        "name": name,
        "metadata": {"cinvar": {"asset_id": asset.id, "sha256": asset.sha256}},
        "available_range": null,
        "target_url": target,
        "missing_media_reference": null,
    })
}

fn clip_json(
    project: &Project,
    timeline: &Timeline,
    clip: &Clip,
    root: Option<&Path>,
    warnings: &mut Vec<String>,
) -> Value {
    let name = describe(clip);
    let rate = clip_rate(project, clip, timeline);
    let duration = clip.duration().as_secs_f64();

    if clip.speed != 1.0 {
        warnings.push(format!(
            "clip '{name}': speed {}x has no faithful OTIO mapping; source_range holds the timeline duration",
            round3(clip.speed)
        ));
    }
    if clip.reversed {
        warnings.push(format!(
            "clip '{name}': reversed playback is not represented in OTIO"
        ));
    }
    if clip.freeze_at.is_some() {
        warnings.push(format!(
            "clip '{name}': freeze frame is not represented in OTIO"
        ));
    }
    for effect in &clip.effects {
        if effect.enabled {
            warnings.push(format!(
                "clip '{name}': effect '{}' has no OTIO equivalent and was not exported",
                effect.kind
            ));
        }
    }
    if let Some(transition) = &clip.transition_in {
        warnings.push(format!(
            "clip '{name}': '{}' transition in ({:.2} s) has no OTIO equivalent",
            transition.kind,
            transition.duration.as_secs_f64()
        ));
    }
    if let Some(transition) = &clip.transition_out {
        warnings.push(format!(
            "clip '{name}': '{}' transition out ({:.2} s) has no OTIO equivalent",
            transition.kind,
            transition.duration.as_secs_f64()
        ));
    }
    for track in &clip.keyframes {
        if !track.keyframes.is_empty() {
            warnings.push(format!(
                "clip '{name}': {} keyframe(s) on '{}' were not exported",
                track.keyframes.len(),
                track.property
            ));
        }
    }
    if let Some(text) = &clip.text {
        warnings.push(format!(
            "clip '{name}': text overlay '{}' has no OTIO equivalent; only its timing is exported",
            crate::util::text::truncate(&text.text, 40)
        ));
    }
    if clip.audio.muted {
        warnings.push(format!(
            "clip '{name}': muted clip audio is not represented in OTIO"
        ));
    }
    if clip.audio.gain_db != 0.0 || clip.audio.fade_in.nanos > 0 || clip.audio.fade_out.nanos > 0 {
        warnings.push(format!(
            "clip '{name}': audio gain/fades are not represented in OTIO"
        ));
    }

    json!({
        "OTIO_SCHEMA": "Clip.2",
        "name": name,
        "metadata": {"cinvar": {"clip_id": clip.id, "asset_id": clip.asset, "speed": clip.speed}},
        "source_range": time_range(clip.source_in.as_secs_f64(), duration, rate),
        "effects": [],
        "markers": [],
        "enabled": true,
        "media_references": {"DEFAULT_MEDIA": media_reference(project, clip, root, warnings)},
        "active_media_reference_key": "DEFAULT_MEDIA",
    })
}

fn gap_json(seconds: f64, rate: f64) -> Value {
    json!({
        "OTIO_SCHEMA": "Gap.1",
        "name": "gap",
        "metadata": {},
        "source_range": time_range(0.0, seconds, rate),
        "effects": [],
        "markers": [],
        "enabled": true,
    })
}

fn track_json(
    project: &Project,
    timeline: &Timeline,
    track: &Track,
    root: Option<&Path>,
    warnings: &mut Vec<String>,
) -> Value {
    let rate = timeline.fps.as_f64();
    let mut children: Vec<Value> = Vec::new();
    let mut cursor = 0.0f64;
    for clip in track.sorted_clips() {
        let start = clip.timeline_start.as_secs_f64();
        if start > cursor + 1e-6 {
            children.push(gap_json(start - cursor, rate));
        }
        children.push(clip_json(project, timeline, clip, root, warnings));
        cursor = track_clip_end(start, clip);
    }
    if track.hidden || track.muted {
        warnings.push(format!(
            "track '{}' is {} in CINVAR; OTIO marks it disabled",
            track.name,
            if track.hidden { "hidden" } else { "muted" }
        ));
    }
    json!({
        "OTIO_SCHEMA": "Track.1",
        "name": track.name,
        // Required by OpenTimelineIO ("Video" | "Audio"); overlay/caption layers are picture.
        "kind": if track.kind == TrackKind::Audio { "Audio" } else { "Video" },
        "metadata": {"cinvar": {"track_id": track.id, "kind": track_kind(track.kind), "z": track.z}},
        "source_range": null,
        "effects": [],
        "markers": [],
        "enabled": !(track.hidden || track.muted),
        "children": children,
    })
}

fn track_clip_end(start: f64, clip: &Clip) -> f64 {
    start + clip.duration().as_secs_f64()
}

fn track_kind(kind: TrackKind) -> &'static str {
    match kind {
        TrackKind::Video => "video",
        TrackKind::Audio => "audio",
        TrackKind::Caption => "caption",
        TrackKind::Overlay => "overlay",
    }
}

pub fn export_otio(
    project: &Project,
    timeline_id: Option<&str>,
    out: &Path,
) -> Result<InterchangeReport> {
    export_otio_in(project, timeline_id, out, None)
}

/// Like [`export_otio`] with an explicit project root, so media paths resolve correctly even
/// when the `.otio` is written outside the project folder.
pub fn export_otio_in(
    project: &Project,
    timeline_id: Option<&str>,
    out: &Path,
    project_root: Option<&Path>,
) -> Result<InterchangeReport> {
    let timeline = match timeline_id {
        Some(id) => project
            .timeline_by_id(id)
            .ok_or_else(|| Error::not_found(format!("timeline {id} not found")))?,
        None => project.timeline().ok_or_else(|| {
            Error::invalid("this project has no timeline to export")
                .with_hint("create a timeline first: cinvar timeline create --name Main")
        })?,
    };

    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let target = paths::absolutize(out, &cwd);
    let root = project_root
        .map(Path::to_path_buf)
        .or_else(|| project_root_hint(&target));
    let mut warnings: Vec<String> = Vec::new();

    let tracks: Vec<Value> = timeline
        .tracks
        .iter()
        .map(|track| track_json(project, timeline, track, root.as_deref(), &mut warnings))
        .collect();

    if !project.captions.is_empty() {
        let cues: usize = project.captions.iter().map(|c| c.cues.len()).sum();
        warnings.push(format!(
            "{} caption track(s) with {} cue(s) are not exported; OTIO has no caption mapping in this build",
            project.captions.len(),
            cues
        ));
    }
    if !project.markers.is_empty() {
        warnings.push(format!(
            "{} timeline marker(s) were not exported",
            project.markers.len()
        ));
    }
    if !timeline.has_audio() {
        warnings.push("the timeline has no audible audio clips".to_string());
    }

    let stack = json!({
        "OTIO_SCHEMA": "Stack.1",
        "name": "tracks",
        "metadata": {},
        "source_range": null,
        "effects": [],
        "markers": [],
        "enabled": true,
        "children": tracks,
    });
    let document = json!({
        "OTIO_SCHEMA": "Timeline.1",
        "name": timeline.name,
        "metadata": {"cinvar": {"revision": project.revision, "project_id": project.id}},
        "global_start_time": null,
        "tracks": stack,
    });

    if let Some(parent) = target.parent() {
        paths::ensure_dir(parent)?;
    }
    let body = serde_json::to_string_pretty(&document)?;
    paths::write_atomic(&target, body.as_bytes())?;

    Ok(InterchangeReport {
        path: target.to_string_lossy().to_string(),
        warnings,
    })
}

#[derive(Debug, Clone, Serialize)]
pub struct ImportReport {
    pub timeline_id: String,
    pub clips: usize,
    pub tracks: usize,
    pub warnings: Vec<String>,
}

fn rt_seconds(v: &Value) -> Option<f64> {
    let value = v.get("value")?.as_f64()?;
    let rate = v.get("rate")?.as_f64()?;
    if rate <= 0.0 || !value.is_finite() {
        return None;
    }
    Some(value / rate)
}

fn range_of(v: &Value) -> Option<(f64, f64)> {
    let r = v
        .get("source_range")
        .filter(|r| !r.is_null())
        .or_else(|| v.get("available_range").filter(|r| !r.is_null()))?;
    Some((
        rt_seconds(r.get("start_time")?)?,
        rt_seconds(r.get("duration")?)?,
    ))
}

fn rate_of(v: &Value) -> Option<f64> {
    v.get("source_range")?
        .get("duration")?
        .get("rate")?
        .as_f64()
}

fn fps_from_rate(rate: f64) -> crate::util::time::Rational {
    use crate::util::time::Rational;
    for (r, n, d) in [
        (23.976, 24000, 1001),
        (29.97, 30000, 1001),
        (59.94, 60000, 1001),
    ] {
        if (rate - r).abs() < 0.01 {
            return Rational::new(n, d);
        }
    }
    Rational::new(rate.round().max(1.0) as u32, 1)
}

fn media_path(clip: &Value, base: &Path) -> Option<PathBuf> {
    let refs = clip.get("media_references").and_then(|m| m.as_object());
    let key = clip
        .get("active_media_reference_key")
        .and_then(|k| k.as_str());
    let reference = match (refs, key) {
        (Some(map), Some(k)) => map.get(k),
        (Some(map), None) => map.values().next(),
        _ => clip.get("media_reference"),
    }?;
    let url = reference.get("target_url")?.as_str()?;
    let stripped = url.strip_prefix("file://").unwrap_or(url);
    let decoded = percent_decode(stripped);
    let p = PathBuf::from(decoded);
    Some(if p.is_absolute() { p } else { base.join(p) })
}

fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let Ok(v) = u8::from_str_radix(&s[i + 1..i + 3], 16) {
                out.push(v);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).to_string()
}

/// Imports an OpenTimelineIO JSON file as a NEW timeline. Supports tracks (video/audio), clips
/// with source ranges and external media references, gaps, and dissolve transitions. Everything
/// else is reported in `warnings`, never dropped silently. Media that cannot be found becomes a
/// warning and a gap of the right length so downstream timing is preserved.
pub fn import_otio(
    project: &mut Project,
    store: &crate::project::store::ProjectStore,
    path: &Path,
) -> Result<ImportReport> {
    import_otio_with(project, store, path, &[])
}

/// Maximum bytes of one referenced media file that an import will stage (64 GiB).
const MAX_IMPORT_BYTES: u64 = 64 << 30;

/// Resolves an untrusted media reference to a validated copy inside the project.
///
/// 1. symlinks and `..` are resolved with `canonicalize`, then the real path must sit under an
///    authorised root (the project, the .otio's folder, or configured `media_roots`);
/// 2. the file is opened once and everything after that works on the open handle; on Linux the
///    open descriptor is re-resolved through /proc/self/fd and containment is checked again, which
///    closes the canonicalize->open race. On other platforms only an identity (dev/inode) check
///    guards that window, and a swapped parent directory could fool it;
/// 3. the bytes are copied into the project's `tmp/` and probed there: the detected container
///    must be on the allow-list, whatever the extension says.
fn stage_reference(
    file: &Path,
    roots: &[PathBuf],
    store: &crate::project::store::ProjectStore,
) -> std::result::Result<PathBuf, String> {
    let real = file.canonicalize().map_err(|_| {
        format!(
            "media not found at {}; left as a gap (relink and re-import)",
            file.display()
        )
    })?;
    if !roots.iter().any(|r| real.starts_with(r)) {
        return Err(format!(
            "'{}' resolves outside the authorised folders; add its folder to security.media_roots or copy the media next to the .otio",
            file.display()
        ));
    }
    // Snapshot identity BEFORE opening, open once, then re-resolve the open descriptor itself.
    // On Linux /proc/self/fd/N names the file actually opened, so a path component swapped for a
    // symlink between canonicalize() and open() is caught by re-running the containment check.
    #[cfg(unix)]
    let before = {
        use std::os::unix::fs::MetadataExt;
        let m = std::fs::metadata(&real).map_err(|e| e.to_string())?;
        (m.dev(), m.ino())
    };
    let mut src =
        std::fs::File::open(&real).map_err(|e| format!("cannot open {}: {e}", real.display()))?;
    let meta = src.metadata().map_err(|e| e.to_string())?;
    if !meta.is_file() {
        return Err(format!("'{}' is not a regular file", real.display()));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if (meta.dev(), meta.ino()) != before {
            return Err(format!(
                "'{}' changed while it was being opened",
                real.display()
            ));
        }
    }
    #[cfg(target_os = "linux")]
    {
        use std::os::fd::AsRawFd;
        let opened = std::fs::read_link(format!("/proc/self/fd/{}", src.as_raw_fd()))
            .map_err(|e| format!("cannot verify the opened file: {e}"))?;
        if !roots.iter().any(|r| opened.starts_with(r)) {
            return Err(format!(
                "'{}' resolved outside the authorised folders at open time",
                file.display()
            ));
        }
    }
    if meta.len() > MAX_IMPORT_BYTES {
        return Err(format!(
            "'{}' is larger than the import limit",
            real.display()
        ));
    }
    let tmp_dir = store.root.join("tmp");
    crate::util::paths::ensure_dir(&tmp_dir).map_err(|e| e.to_string())?;
    let ext = real.extension().and_then(|e| e.to_str()).unwrap_or("bin");
    let staged = tmp_dir.join(format!(
        "otio-stage-{}.{}",
        crate::util::ids::new_id("s"),
        ext.chars()
            .filter(|c| c.is_ascii_alphanumeric())
            .take(8)
            .collect::<String>()
    ));
    {
        let mut dst = std::fs::File::create(&staged).map_err(|e| e.to_string())?;
        let mut limited = std::io::Read::take(&mut src, MAX_IMPORT_BYTES);
        std::io::copy(&mut limited, &mut dst).map_err(|e| e.to_string())?;
    }
    match crate::media::probe_untrusted(&staged) {
        Ok(_) => Ok(staged),
        Err(e) => {
            let _ = std::fs::remove_file(&staged);
            Err(format!("'{}' rejected: {}", file.display(), e.message))
        }
    }
}

pub fn import_otio_with(
    project: &mut Project,
    store: &crate::project::store::ProjectStore,
    path: &Path,
    extra_roots: &[PathBuf],
) -> Result<ImportReport> {
    let text = std::fs::read_to_string(path)
        .map_err(|e| Error::not_found(format!("cannot read {}: {e}", path.display())))?;
    let doc: Value = serde_json::from_str(&text)
        .map_err(|e| Error::invalid(format!("{} is not valid OTIO JSON: {e}", path.display())))?;
    let schema = doc
        .get("OTIO_SCHEMA")
        .and_then(|s| s.as_str())
        .unwrap_or("");
    if !schema.starts_with("Timeline.") {
        return Err(
            Error::invalid(format!("expected a Timeline, found '{schema}'")).with_hint(
                "export a single timeline (not a SerializableCollection) from the editor",
            ),
        );
    }
    let base = path.parent().map(Path::to_path_buf).unwrap_or_default();
    let mut roots: Vec<PathBuf> = Vec::new();
    for r in [store.root.as_path(), base.as_path()]
        .into_iter()
        .chain(extra_roots.iter().map(PathBuf::as_path))
    {
        if let Ok(c) = r.canonicalize() {
            roots.push(c);
        }
    }
    let tracks = doc
        .get("tracks")
        .and_then(|t| t.get("children"))
        .and_then(|c| c.as_array())
        .ok_or_else(|| Error::invalid("timeline has no tracks"))?;

    let mut warnings: Vec<String> = Vec::new();
    let rate = tracks
        .iter()
        .flat_map(|t| {
            t.get("children")
                .and_then(|c| c.as_array())
                .into_iter()
                .flatten()
        })
        .find_map(rate_of)
        .unwrap_or(24.0);
    let name = doc
        .get("name")
        .and_then(|n| n.as_str())
        .filter(|n| !n.is_empty())
        .unwrap_or("Imported");
    let timeline_id =
        crate::project::ops::create_timeline(project, name, 1920, 1080, fps_from_rate(rate))?;
    warnings.push("OTIO carries no canvas size; the timeline is 1920x1080 (change it with the timeline tools)".into());
    // create_timeline adds default tracks; the import creates its own.
    if let Some(tl) = project.timelines.iter_mut().find(|t| t.id == timeline_id) {
        tl.tracks.clear();
    }

    let mut clip_count = 0usize;
    let mut track_count = 0usize;
    for track in tracks {
        let kind_str = track
            .get("kind")
            .and_then(|k| k.as_str())
            .unwrap_or("Video");
        let kind = if kind_str.eq_ignore_ascii_case("audio") {
            TrackKind::Audio
        } else {
            TrackKind::Video
        };
        let tname = track
            .get("name")
            .and_then(|n| n.as_str())
            .unwrap_or("")
            .to_string();
        let track_id = crate::project::ops::add_track(project, Some(&timeline_id), kind, &tname)?;
        track_count += 1;
        let mut cursor = 0.0f64;
        let mut last_clip: Option<String> = None;
        let mut pending_in: Option<(String, f64)> = None;
        for item in track
            .get("children")
            .and_then(|c| c.as_array())
            .into_iter()
            .flatten()
        {
            let schema = item
                .get("OTIO_SCHEMA")
                .and_then(|s| s.as_str())
                .unwrap_or("");
            if schema.starts_with("Gap.") {
                if let Some((_, d)) = range_of(item) {
                    cursor += d;
                }
            } else if schema.starts_with("Clip.") {
                let Some((start, dur)) = range_of(item) else {
                    warnings.push(format!(
                        "clip '{}' has no time range; skipped",
                        item.get("name").and_then(|n| n.as_str()).unwrap_or("?")
                    ));
                    continue;
                };
                let cname = item
                    .get("name")
                    .and_then(|n| n.as_str())
                    .unwrap_or("clip")
                    .to_string();
                let Some(file) = media_path(item, &base) else {
                    warnings.push(format!(
                        "clip '{cname}': no usable media reference; left as a gap"
                    ));
                    cursor += dur;
                    continue;
                };
                let staged = match stage_reference(&file, &roots, store) {
                    Ok(p) => p,
                    Err(why) => {
                        warnings.push(format!("clip '{cname}': {why}; left as a gap"));
                        cursor += dur;
                        continue;
                    }
                };
                let imported = store.import_asset(project, &staged, "otio");
                let _ = std::fs::remove_file(&staged);
                let mut asset = imported?;
                if let Some(a) = project.assets.iter_mut().find(|a| a.id == asset.id) {
                    // The staged copy's name is internal; keep the original file name in the record.
                    a.provenance = Some(format!(
                        "otio:{}",
                        file.file_name()
                            .map(|n| n.to_string_lossy().to_string())
                            .unwrap_or_default()
                    ));
                    asset = a.clone();
                }
                let id = crate::project::ops::insert_clip(
                    project,
                    Some(&timeline_id),
                    Some(&track_id),
                    crate::project::ops::ClipRequest {
                        asset_id: &asset.id,
                        source_in: Some(Time::from_secs_f64(start)),
                        source_out: Some(Time::from_secs_f64(start + dur)),
                        at: Some(Time::from_secs_f64(cursor)),
                        name: Some(&cname),
                    },
                )?;
                if let Some((kind, d)) = pending_in.take() {
                    let _ = crate::project::ops::set_transition(
                        project,
                        &id,
                        crate::project::model::TransitionSide::In,
                        &kind,
                        Time::from_secs_f64(d),
                    );
                }
                cursor += dur;
                last_clip = Some(id);
                clip_count += 1;
            } else if schema.starts_with("Transition.") {
                let tt = item
                    .get("transition_type")
                    .and_then(|t| t.as_str())
                    .unwrap_or("");
                let d = item.get("in_offset").and_then(rt_seconds).unwrap_or(0.0)
                    + item.get("out_offset").and_then(rt_seconds).unwrap_or(0.0);
                if tt == "SMPTE_Dissolve"
                    || tt.eq_ignore_ascii_case("dissolve")
                    || tt == "Custom_Transition" && d > 0.0
                {
                    if let Some(prev) = &last_clip {
                        let _ = crate::project::ops::set_transition(
                            project,
                            prev,
                            crate::project::model::TransitionSide::Out,
                            "dissolve",
                            Time::from_secs_f64(d),
                        );
                    }
                    pending_in = Some(("dissolve".to_string(), d));
                } else {
                    warnings.push(format!(
                        "transition '{tt}' is not supported and was skipped"
                    ));
                }
            } else {
                warnings.push(format!(
                    "unsupported item '{schema}' on track '{tname}' skipped"
                ));
            }
        }
    }
    if tracks.iter().any(|t| {
        t.get("effects")
            .and_then(|e| e.as_array())
            .map(|e| !e.is_empty())
            .unwrap_or(false)
    }) {
        warnings.push("track effects were not imported".into());
    }
    Ok(ImportReport {
        timeline_id,
        clips: clip_count,
        tracks: track_count,
        warnings,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::project::model::{
        Asset, AssetKind, Effect, Keyframe, KeyframeTrack, MediaInfo, Transition,
    };
    use crate::util::time::Time;

    fn sample_project() -> Project {
        let mut project = Project::new("OTIO Test");
        project.revision = 7;
        let mut timeline = Timeline {
            name: "Main".into(),
            fps: crate::util::time::Rational::new(30, 1),
            ..Timeline::default()
        };
        let mut video = Track {
            name: "Video 1".into(),
            kind: TrackKind::Video,
            ..Track::default()
        };
        let first = Clip {
            name: "first".into(),
            asset: Some("asset-1".into()),
            source_in: Time::ZERO,
            source_out: Time::from_secs_f64(2.0),
            timeline_start: Time::ZERO,
            ..Clip::default()
        };
        let mut second = Clip {
            name: "second".into(),
            asset: Some("asset-1".into()),
            source_in: Time::from_secs_f64(0.5),
            source_out: Time::from_secs_f64(2.5),
            timeline_start: Time::from_secs_f64(3.0),
            ..Clip::default()
        };
        second.effects.push(Effect {
            id: "e1".into(),
            kind: "blur".into(),
            params: json!({}),
            enabled: true,
        });
        second.transition_in = Some(Transition {
            kind: "dissolve".into(),
            duration: Time::from_secs_f64(0.5),
        });
        second.keyframes.push(KeyframeTrack {
            property: "opacity".into(),
            keyframes: vec![Keyframe {
                at: Time::ZERO,
                value: 1.0,
                easing: "linear".into(),
            }],
        });
        video.clips.push(first);
        video.clips.push(second);
        timeline.tracks.push(video);
        project.assets.push(Asset {
            id: "asset-1".into(),
            path: "assets/a.mp4".into(),
            kind: AssetKind::Video,
            bytes: 1,
            imported_at: String::new(),
            sha256: "digest".into(),
            info: Some(MediaInfo {
                has_video: true,
                fps: Some(crate::util::time::Rational::new(24000, 1001)),
                ..MediaInfo::default()
            }),
            missing: false,
            provenance: None,
        });
        let id = timeline.id.clone();
        project.timelines.push(timeline);
        project.active_timeline = Some(id);
        project
    }

    #[test]
    fn export_uses_schemas_the_real_library_accepts() {
        // Regression: Clip.1 carrying `media_references` and Track.1 without `kind` made the
        // real OpenTimelineIO library crash (SIGSEGV) / refuse the file.
        let project = sample_project();
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("t.otio");
        export_otio(&project, None, &out).unwrap();
        let doc: Value = serde_json::from_str(&std::fs::read_to_string(&out).unwrap()).unwrap();
        let track = &doc["tracks"]["children"][0];
        assert!(matches!(
            track["kind"].as_str(),
            Some("Video") | Some("Audio")
        ));
        let clip = track["children"]
            .as_array()
            .unwrap()
            .iter()
            .find(|c| c["OTIO_SCHEMA"].as_str().unwrap_or("").starts_with("Clip."))
            .unwrap();
        assert_eq!(clip["OTIO_SCHEMA"], "Clip.2");
        assert!(
            clip["media_references"].is_object() && clip["active_media_reference_key"].is_string()
        );
    }

    #[test]
    fn writes_otio_with_tracks_gaps_and_warnings() {
        let dir = tempfile::tempdir().unwrap();
        let project = sample_project();
        let out = dir.path().join("timeline.otio");
        let report = export_otio(&project, None, &out).unwrap();
        assert!(out.exists());
        let value: Value = serde_json::from_str(&std::fs::read_to_string(&out).unwrap()).unwrap();
        assert_eq!(value["OTIO_SCHEMA"], "Timeline.1");
        assert!(value["global_start_time"].is_null());
        assert_eq!(value["metadata"]["cinvar"]["revision"], 7);
        assert_eq!(value["metadata"]["cinvar"]["project_id"], project.id);
        let children = value["tracks"]["children"].as_array().unwrap();
        assert_eq!(children.len(), 1);
        assert_eq!(children[0]["OTIO_SCHEMA"], "Track.1");
        let stack = children[0]["children"].as_array().unwrap();
        // clip, gap, clip
        assert_eq!(stack.len(), 3);
        assert_eq!(stack[0]["OTIO_SCHEMA"], "Clip.2");
        assert_eq!(stack[1]["OTIO_SCHEMA"], "Gap.1");
        assert_eq!(stack[2]["OTIO_SCHEMA"], "Clip.2");
        // asset fps wins over the timeline fps
        assert_eq!(
            stack[0]["source_range"]["duration"]["rate"]
                .as_f64()
                .unwrap(),
            24000.0 / 1001.0
        );
        assert!(report.warnings.iter().any(|w| w.contains("blur")));
        assert!(report.warnings.iter().any(|w| w.contains("transition")));
        assert!(report.warnings.iter().any(|w| w.contains("keyframe")));
    }

    #[test]
    fn unknown_timeline_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let project = sample_project();
        let err =
            export_otio(&project, Some("timeline-nope"), &dir.path().join("x.otio")).unwrap_err();
        assert_eq!(err.code, crate::error::ErrorCode::NotFound);
    }

    #[test]
    fn empty_project_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let project = Project::new("nothing");
        let err = export_otio(&project, None, &dir.path().join("x.otio")).unwrap_err();
        assert_eq!(err.code, crate::error::ErrorCode::InvalidInput);
    }

    #[test]
    fn text_clip_becomes_missing_reference() {
        let dir = tempfile::tempdir().unwrap();
        let mut project = sample_project();
        let timeline_id = project.active_timeline.clone().unwrap();
        let timeline = project.timeline_by_id_mut(&timeline_id).unwrap();
        let text_clip = Clip {
            name: "title".into(),
            text: Some(crate::project::model::TextOverlay::default()),
            timeline_start: Time::from_secs_f64(8.0),
            ..Clip::default()
        };
        timeline.tracks[0].clips.push(text_clip);
        let out = dir.path().join("text.otio");
        let report = export_otio(&project, None, &out).unwrap();
        let value: Value = serde_json::from_str(&std::fs::read_to_string(&out).unwrap()).unwrap();
        let stack = value["tracks"]["children"][0]["children"]
            .as_array()
            .unwrap();
        let last = stack.last().unwrap();
        assert_eq!(
            last["media_references"]["DEFAULT_MEDIA"]["OTIO_SCHEMA"],
            "MissingReference.1"
        );
        assert!(report.warnings.iter().any(|w| w.contains("text overlay")));
    }
}
