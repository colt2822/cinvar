use crate::error::{Error, Result};
use crate::project::model::*;
use crate::util::ids;
use crate::util::time::{Fps, Time, TimeRange};

pub const SUPPORTED_EFFECTS: &[&str] = &[
    "brightness",
    "contrast",
    "saturation",
    "gamma",
    "eq",
    "blur",
    "sharpen",
    "vignette",
    "grayscale",
    "sepia",
    "zoom",
    "crop",
    "rotate",
    "speed",
    "stabilize",
    "denoise",
    "letterbox",
    "lut",
];

/// Transitions the renderer actually produces. Names that would silently degrade to
/// a plain fade (wipe, slide, circle) are deliberately absent: the tool rejects them
/// instead of rendering something else and calling it a wipe.
pub const SUPPORTED_TRANSITIONS: &[&str] = &[
    "fade",
    "fadeblack",
    "fadewhite",
    "crossfade",
    "dissolve",
    "none",
];

fn require_timeline_mut<'a>(
    project: &'a mut Project,
    timeline_id: Option<&str>,
) -> Result<&'a mut Timeline> {
    let id = match timeline_id {
        Some(id) => id.to_string(),
        None => project
            .active_timeline
            .clone()
            .or_else(|| project.timelines.first().map(|t| t.id.clone()))
            .ok_or_else(|| {
                Error::not_found("this project has no timeline yet")
                    .with_hint("create one with cinvar.timeline.create")
            })?,
    };
    project
        .timelines
        .iter_mut()
        .find(|t| t.id == id)
        .ok_or_else(|| Error::not_found(format!("timeline {id} not found")))
}

fn overlap_check(track: &Track, range: TimeRange, ignore: Option<&str>) -> Result<()> {
    for clip in &track.clips {
        if Some(clip.id.as_str()) == ignore {
            continue;
        }
        if clip.range().overlaps(range) {
            return Err(Error::new(
                crate::error::ErrorCode::Conflict,
                format!(
                    "clip would overlap existing clip '{}' on track '{}' ({}-{})",
                    if clip.name.is_empty() {
                        clip.id.as_str()
                    } else {
                        clip.name.as_str()
                    },
                    track.name,
                    clip.timeline_start,
                    clip.timeline_end()
                ),
            )
            .with_hint(
                "move or trim the existing clip first, or place this clip on another track",
            ));
        }
    }
    Ok(())
}

/// (property, [(local seconds, value, easing)])
type PlanStep<'a> = (&'a str, Vec<(f64, f64, &'a str)>);

pub fn create_timeline(
    project: &mut Project,
    name: &str,
    width: u32,
    height: u32,
    fps: Fps,
) -> Result<String> {
    if width < 16 || height < 16 || width > 8192 || height > 8192 {
        return Err(Error::invalid(format!(
            "unsupported canvas size {width}x{height}"
        )));
    }
    if !fps.is_valid() {
        return Err(Error::invalid("invalid frame rate"));
    }
    let mut timeline = Timeline {
        id: ids::new_id("timeline"),
        name: name.to_string(),
        width,
        height,
        fps,
        ..Timeline::default()
    };
    timeline.width = width;
    timeline.height = height;
    timeline.fps = fps;
    let video_track = Track {
        name: "Video 1".into(),
        kind: TrackKind::Video,
        z: 0,
        ..Track::default()
    };
    let audio_track = Track {
        name: "Audio 1".into(),
        kind: TrackKind::Audio,
        z: 0,
        ..Track::default()
    };
    timeline.tracks.push(video_track);
    timeline.tracks.push(audio_track);
    let id = timeline.id.clone();
    project.timelines.push(timeline);
    project.active_timeline = Some(id.clone());
    Ok(id)
}

pub fn set_active_timeline(project: &mut Project, timeline_id: &str) -> Result<()> {
    if !project.timelines.iter().any(|t| t.id == timeline_id) {
        return Err(Error::not_found(format!(
            "timeline {timeline_id} not found"
        )));
    }
    project.active_timeline = Some(timeline_id.to_string());
    Ok(())
}

pub fn add_track(
    project: &mut Project,
    timeline_id: Option<&str>,
    kind: TrackKind,
    name: &str,
) -> Result<String> {
    let timeline = require_timeline_mut(project, timeline_id)?;
    let z = timeline.tracks.iter().map(|t| t.z).max().unwrap_or(0) + 1;
    let track = Track {
        kind,
        name: if name.is_empty() {
            format!("{:?} {}", kind, z)
        } else {
            name.to_string()
        },
        z,
        ..Track::default()
    };
    let id = track.id.clone();
    timeline.tracks.push(track);
    Ok(id)
}

pub fn default_track_for(timeline: &Timeline, kind: TrackKind) -> Option<&Track> {
    timeline.tracks.iter().find(|t| t.kind == kind)
}

pub fn ensure_track(
    project: &mut Project,
    timeline_id: Option<&str>,
    kind: TrackKind,
) -> Result<String> {
    let timeline = require_timeline_mut(project, timeline_id)?;
    if let Some(track) = timeline.tracks.iter().find(|t| t.kind == kind) {
        return Ok(track.id.clone());
    }
    let z = timeline.tracks.iter().map(|t| t.z).max().unwrap_or(0) + 1;
    let track = Track {
        kind,
        name: format!("{:?} {}", kind, z),
        z,
        ..Track::default()
    };
    let id = track.id.clone();
    timeline.tracks.push(track);
    Ok(id)
}

pub struct ClipRequest<'a> {
    pub asset_id: &'a str,
    pub source_in: Option<Time>,
    pub source_out: Option<Time>,
    pub at: Option<Time>,
    pub name: Option<&'a str>,
}

pub fn insert_clip(
    project: &mut Project,
    timeline_id: Option<&str>,
    track_id: Option<&str>,
    req: ClipRequest,
) -> Result<String> {
    let asset = project
        .asset(req.asset_id)
        .ok_or_else(|| Error::not_found(format!("asset {} is not in this project", req.asset_id)))?
        .clone();
    let asset_duration = asset
        .info
        .as_ref()
        .map(|i| i.duration)
        .unwrap_or(Time::ZERO);
    let source_in = req.source_in.unwrap_or(Time::ZERO).clamp_non_negative();
    let source_out = match req.source_out {
        Some(t) => t,
        None => {
            if asset_duration.nanos > 0 {
                asset_duration
            } else {
                return Err(Error::invalid(format!(
                    "asset {} has no measurable duration; pass source_out explicitly",
                    asset.display_name()
                )));
            }
        }
    };
    if source_out <= source_in {
        return Err(Error::invalid("source_out must be greater than source_in"));
    }
    if asset_duration.nanos > 0 && source_out.nanos > asset_duration.nanos + 1_000_000 {
        return Err(Error::invalid(format!(
            "source range {}..{} exceeds asset duration {}",
            source_in, source_out, asset_duration
        )));
    }

    let kind = match asset.kind {
        AssetKind::Audio => TrackKind::Audio,
        _ => TrackKind::Video,
    };
    let timeline = require_timeline_mut(project, timeline_id)?;
    let track_idx = match track_id {
        Some(id) => timeline
            .tracks
            .iter()
            .position(|t| t.id == id)
            .ok_or_else(|| Error::not_found(format!("track {id} not found")))?,
        None => match timeline.tracks.iter().position(|t| t.kind == kind) {
            Some(idx) => idx,
            None => {
                let track = Track {
                    kind,
                    name: format!("{:?} auto", kind),
                    ..Track::default()
                };
                timeline.tracks.push(track);
                timeline.tracks.len() - 1
            }
        },
    };

    let start = req
        .at
        .unwrap_or_else(|| timeline.tracks[track_idx].end_time());
    let clip = Clip {
        asset: Some(asset.id.clone()),
        name: req
            .name
            .map(|n| n.to_string())
            .unwrap_or_else(|| asset.display_name()),
        source_in,
        source_out,
        timeline_start: start,
        ..Clip::default()
    };
    overlap_check(&timeline.tracks[track_idx], clip.range(), None)?;
    let id = clip.id.clone();
    timeline.tracks[track_idx].clips.push(clip);
    Ok(id)
}

pub fn append_clip(
    project: &mut Project,
    timeline_id: Option<&str>,
    track_id: Option<&str>,
    asset_id: &str,
) -> Result<String> {
    insert_clip(
        project,
        timeline_id,
        track_id,
        ClipRequest {
            asset_id,
            source_in: None,
            source_out: None,
            at: None,
            name: None,
        },
    )
}

pub fn remove_clip(project: &mut Project, clip_id: &str) -> Result<()> {
    if project.remove_clip_by_id(clip_id) {
        Ok(())
    } else {
        Err(Error::not_found(format!("clip {clip_id} not found")))
    }
}

pub fn ripple_delete(project: &mut Project, clip_id: &str) -> Result<()> {
    let removed = project
        .find_clip(clip_id)
        .map(|(_, _, c)| (c.timeline_start, c.duration(), c.timeline_start))
        .ok_or_else(|| Error::not_found(format!("clip {clip_id} not found")))?;
    let gap = removed.1;
    let from = removed.0;
    project.remove_clip_by_id(clip_id);
    for timeline in project.timelines.iter_mut() {
        for track in timeline.tracks.iter_mut() {
            for clip in track.clips.iter_mut() {
                if clip.timeline_start >= from {
                    clip.timeline_start = (clip.timeline_start - gap).clamp_non_negative();
                }
            }
        }
    }
    Ok(())
}

pub fn trim_clip(
    project: &mut Project,
    clip_id: &str,
    new_in: Option<Time>,
    new_out: Option<Time>,
) -> Result<()> {
    let mut candidate = project
        .clip(clip_id)
        .ok_or_else(|| Error::not_found(format!("clip {clip_id} not found")))?
        .clone();
    if let Some(t) = new_in {
        candidate.source_in = t.clamp_non_negative();
    }
    if let Some(t) = new_out {
        candidate.source_out = t;
    }
    if candidate.source_out <= candidate.source_in {
        return Err(Error::invalid("trim would produce a zero-length clip"));
    }
    {
        let track = project
            .track_of_clip(clip_id)
            .ok_or_else(|| Error::not_found(format!("clip {clip_id} not found")))?;
        overlap_check(track, candidate.range(), Some(clip_id))?;
    }
    let clip = project
        .clip_mut(clip_id)
        .ok_or_else(|| Error::not_found(format!("clip {clip_id} not found")))?;
    clip.source_in = candidate.source_in;
    clip.source_out = candidate.source_out;
    Ok(())
}

/// Trim and close (or open) the gap by shifting every later clip on the same track.
/// Returns the applied shift: negative when the sequence got shorter.
pub fn trim_clip_ripple(
    project: &mut Project,
    clip_id: &str,
    new_in: Option<Time>,
    new_out: Option<Time>,
) -> Result<Time> {
    let (track_id, old_end, old_duration) = {
        let track = project
            .track_of_clip(clip_id)
            .ok_or_else(|| Error::not_found(format!("clip {clip_id} not found")))?;
        let clip = project
            .clip(clip_id)
            .ok_or_else(|| Error::not_found(format!("clip {clip_id} not found")))?;
        (track.id.clone(), clip.timeline_end(), clip.duration())
    };
    trim_clip(project, clip_id, new_in, new_out)?;
    let new_duration = project
        .clip(clip_id)
        .ok_or_else(|| Error::not_found(format!("clip {clip_id} not found")))?
        .duration();
    let delta = new_duration - old_duration;
    if delta.nanos == 0 {
        return Ok(delta);
    }
    for timeline in project.timelines.iter_mut() {
        if let Some(track) = timeline.tracks.iter_mut().find(|t| t.id == track_id) {
            for clip in track.clips.iter_mut() {
                if clip.id != clip_id && clip.timeline_start >= old_end {
                    clip.timeline_start = (clip.timeline_start + delta).clamp_non_negative();
                }
            }
        }
    }
    Ok(delta)
}

pub fn split_clip(project: &mut Project, clip_id: &str, at: Time) -> Result<(String, String)> {
    let (ti, ri, ci) = project
        .clip_location(clip_id)
        .ok_or_else(|| Error::not_found(format!("clip {clip_id} not found")))?;
    let original = project.timelines[ti].tracks[ri].clips[ci].clone();
    let clip_range = original.range();
    if !clip_range.contains(at) || at == clip_range.start {
        return Err(Error::invalid(format!(
            "split point {at} is outside clip range {}-{}",
            clip_range.start, clip_range.end
        )));
    }
    let offset = at - original.timeline_start;
    let source_cut = original.source_in + offset.scale(original.speed);
    if source_cut <= original.source_in || source_cut >= original.source_out {
        return Err(Error::invalid("split point maps outside the source range"));
    }
    let mut left = original.clone();
    left.source_out = source_cut;
    left.transition_out = None;
    let mut right = original;
    right.id = ids::new_id("clip");
    right.source_in = source_cut;
    right.timeline_start = at;
    right.transition_in = None;
    let left_id = left.id.clone();
    let right_id = right.id.clone();
    project.timelines[ti].tracks[ri].clips[ci] = left;
    project.timelines[ti].tracks[ri].clips.insert(ci + 1, right);
    Ok((left_id, right_id))
}

pub fn move_clip(
    project: &mut Project,
    clip_id: &str,
    target_track: Option<&str>,
    new_start: Time,
) -> Result<()> {
    let new_start = new_start.clamp_non_negative();
    let (current_track, mut candidate) = {
        let track = project
            .track_of_clip(clip_id)
            .ok_or_else(|| Error::not_found(format!("clip {clip_id} not found")))?;
        let clip = project
            .clip(clip_id)
            .ok_or_else(|| Error::not_found(format!("clip {clip_id} not found")))?
            .clone();
        (track.id.clone(), clip)
    };
    candidate.timeline_start = new_start;
    let dest_track_id = target_track
        .map(|s| s.to_string())
        .unwrap_or_else(|| current_track.clone());
    let destination = project
        .timelines
        .iter()
        .flat_map(|t| t.tracks.iter())
        .find(|t| t.id == dest_track_id)
        .ok_or_else(|| Error::not_found(format!("track {dest_track_id} not found")))?;
    overlap_check(destination, candidate.range(), Some(clip_id))?;
    if dest_track_id != current_track {
        project.remove_clip_by_id(clip_id);
        for timeline in project.timelines.iter_mut() {
            if let Some(track) = timeline.tracks.iter_mut().find(|t| t.id == dest_track_id) {
                track.clips.push(candidate);
                return Ok(());
            }
        }
        return Err(Error::not_found("destination track disappeared"));
    }
    let clip = project
        .clip_mut(clip_id)
        .ok_or_else(|| Error::not_found(format!("clip {clip_id} not found")))?;
    clip.timeline_start = new_start;
    Ok(())
}

pub fn duplicate_clip(project: &mut Project, clip_id: &str, offset: Time) -> Result<String> {
    let track_id = project
        .track_of_clip(clip_id)
        .map(|t| t.id.clone())
        .ok_or_else(|| Error::not_found(format!("clip {clip_id} not found")))?;
    let mut copy = project
        .clip(clip_id)
        .ok_or_else(|| Error::not_found(format!("clip {clip_id} not found")))?
        .clone();
    copy.id = ids::new_id("clip");
    copy.timeline_start = (copy.timeline_start + offset).clamp_non_negative();
    {
        let track = project
            .timelines
            .iter()
            .flat_map(|t| t.tracks.iter())
            .find(|t| t.id == track_id)
            .ok_or_else(|| Error::not_found("track not found"))?;
        overlap_check(track, copy.range(), None)?;
    }
    let id = copy.id.clone();
    for timeline in project.timelines.iter_mut() {
        if let Some(track) = timeline.tracks.iter_mut().find(|t| t.id == track_id) {
            track.clips.push(copy);
            return Ok(id);
        }
    }
    Err(Error::not_found("track disappeared"))
}

pub fn reorder_track(project: &mut Project, track_id: &str, ordered: &[String]) -> Result<()> {
    for timeline in project.timelines.iter_mut() {
        if let Some(track) = timeline.tracks.iter_mut().find(|t| t.id == track_id) {
            let mut cursor = Time::ZERO;
            let mut rebuilt: Vec<Clip> = Vec::new();
            for id in ordered {
                let mut clip = track
                    .clips
                    .iter()
                    .find(|c| &c.id == id)
                    .ok_or_else(|| Error::not_found(format!("clip {id} not found on this track")))?
                    .clone();
                clip.timeline_start = cursor;
                cursor = clip.timeline_end();
                rebuilt.push(clip);
            }
            track.clips = rebuilt;
            return Ok(());
        }
    }
    Err(Error::not_found(format!("track {track_id} not found")))
}

pub fn set_speed(project: &mut Project, clip_id: &str, speed: f64) -> Result<()> {
    if !(0.05..=20.0).contains(&speed) {
        return Err(Error::invalid("speed must be between 0.05 and 20"));
    }
    let mut candidate = project
        .clip(clip_id)
        .ok_or_else(|| Error::not_found(format!("clip {clip_id} not found")))?
        .clone();
    candidate.speed = speed;
    {
        let track = project
            .track_of_clip(clip_id)
            .ok_or_else(|| Error::not_found(format!("clip {clip_id} not found")))?;
        overlap_check(track, candidate.range(), Some(clip_id))?;
    }
    project
        .clip_mut(clip_id)
        .ok_or_else(|| Error::not_found(format!("clip {clip_id} not found")))?
        .speed = speed;
    Ok(())
}

pub fn freeze_frame(project: &mut Project, clip_id: &str, source_time: Time) -> Result<()> {
    let clip = project
        .clip(clip_id)
        .ok_or_else(|| Error::not_found(format!("clip {clip_id} not found")))?;
    if source_time < clip.source_in || source_time >= clip.source_out {
        return Err(Error::invalid(format!(
            "freeze point {source_time} is outside the clip source range {}-{}",
            clip.source_in, clip.source_out
        )));
    }
    project
        .clip_mut(clip_id)
        .ok_or_else(|| Error::not_found(format!("clip {clip_id} not found")))?
        .freeze_at = Some(source_time);
    Ok(())
}

pub fn clear_freeze(project: &mut Project, clip_id: &str) -> Result<()> {
    project
        .clip_mut(clip_id)
        .ok_or_else(|| Error::not_found(format!("clip {clip_id} not found")))?
        .freeze_at = None;
    Ok(())
}

pub fn set_reversed(project: &mut Project, clip_id: &str, reversed: bool) -> Result<()> {
    project
        .clip_mut(clip_id)
        .ok_or_else(|| Error::not_found(format!("clip {clip_id} not found")))?
        .reversed = reversed;
    Ok(())
}

pub fn set_transition(
    project: &mut Project,
    clip_id: &str,
    side: TransitionSide,
    kind: &str,
    duration: Time,
) -> Result<()> {
    if !SUPPORTED_TRANSITIONS.contains(&kind) {
        return Err(Error::invalid(format!(
            "unknown transition '{kind}' (supported: {})",
            SUPPORTED_TRANSITIONS.join(", ")
        )));
    }
    if duration.nanos < 0 {
        return Err(Error::invalid("transition duration must not be negative"));
    }
    let transition = if kind == "none" {
        None
    } else {
        Some(Transition {
            kind: kind.to_string(),
            duration,
        })
    };
    let clip = project
        .clip_mut(clip_id)
        .ok_or_else(|| Error::not_found(format!("clip {clip_id} not found")))?;
    match side {
        TransitionSide::In => clip.transition_in = transition,
        TransitionSide::Out => clip.transition_out = transition,
    }
    Ok(())
}

pub fn add_effect(
    project: &mut Project,
    clip_id: &str,
    kind: &str,
    params: serde_json::Value,
) -> Result<String> {
    if !SUPPORTED_EFFECTS.contains(&kind) {
        return Err(Error::invalid(format!(
            "unknown effect '{kind}' (supported: {})",
            SUPPORTED_EFFECTS.join(", ")
        )));
    }
    let effect = Effect {
        id: ids::new_id("fx"),
        kind: kind.to_string(),
        params,
        enabled: true,
    };
    let id = effect.id.clone();
    project
        .clip_mut(clip_id)
        .ok_or_else(|| Error::not_found(format!("clip {clip_id} not found")))?
        .effects
        .push(effect);
    Ok(id)
}

pub fn remove_effect(project: &mut Project, clip_id: &str, effect_id: &str) -> Result<()> {
    let clip = project
        .clip_mut(clip_id)
        .ok_or_else(|| Error::not_found(format!("clip {clip_id} not found")))?;
    let before = clip.effects.len();
    clip.effects.retain(|e| e.id != effect_id);
    if clip.effects.len() == before {
        Err(Error::not_found(format!(
            "effect {effect_id} not found on clip {clip_id}"
        )))
    } else {
        Ok(())
    }
}

pub fn add_keyframe(
    project: &mut Project,
    clip_id: &str,
    property: &str,
    at: Time,
    value: f64,
    easing: &str,
) -> Result<()> {
    let duration = project
        .clip(clip_id)
        .map(|c| (c.timeline_start, c.duration()))
        .ok_or_else(|| Error::not_found(format!("clip {clip_id} not found")))?;
    crate::render::check_keyframe_property(property)?;
    let easing = if easing.is_empty() { "linear" } else { easing };
    crate::render::check_keyframe_easing(easing)?;
    let local = at - duration.0;
    if local.is_negative() || local.nanos > duration.1.nanos {
        return Err(Error::invalid("keyframe time must fall inside the clip"));
    }
    let clip = project
        .clip_mut(clip_id)
        .ok_or_else(|| Error::not_found(format!("clip {clip_id} not found")))?;
    let easing = if easing.is_empty() {
        "linear".to_string()
    } else {
        easing.to_string()
    };
    if !clip.keyframes.iter().any(|k| k.property == property) {
        clip.keyframes.push(KeyframeTrack {
            property: property.to_string(),
            keyframes: Vec::new(),
        });
    }
    let track = clip
        .keyframes
        .iter_mut()
        .find(|k| k.property == property)
        .unwrap();
    track.keyframes.retain(|k| k.at.nanos != local.nanos);
    track.keyframes.push(Keyframe {
        at: local,
        value,
        easing,
    });
    track.keyframes.sort_by_key(|k| k.at.nanos);
    Ok(())
}

pub fn set_transform(project: &mut Project, clip_id: &str, transform: Transform) -> Result<()> {
    project
        .clip_mut(clip_id)
        .ok_or_else(|| Error::not_found(format!("clip {clip_id} not found")))?
        .transform = transform;
    Ok(())
}

#[allow(clippy::too_many_arguments)]
pub fn set_clip_audio(
    project: &mut Project,
    clip_id: &str,
    gain_db: Option<f64>,
    fade_in: Option<Time>,
    fade_out: Option<Time>,
    normalize: Option<bool>,
    muted: Option<bool>,
    duck_under_speech: Option<bool>,
) -> Result<()> {
    let clip = project
        .clip_mut(clip_id)
        .ok_or_else(|| Error::not_found(format!("clip {clip_id} not found")))?;
    if let Some(gain) = gain_db {
        if !(-60.0..=24.0).contains(&gain) {
            return Err(Error::invalid("gain_db must be between -60 and 24"));
        }
        clip.audio.gain_db = gain;
    }
    if let Some(t) = fade_in {
        clip.audio.fade_in = t.clamp_non_negative();
    }
    if let Some(t) = fade_out {
        clip.audio.fade_out = t.clamp_non_negative();
    }
    if let Some(n) = normalize {
        clip.audio.normalize = n;
    }
    if let Some(m) = muted {
        clip.audio.muted = m;
    }
    if let Some(d) = duck_under_speech {
        clip.audio.duck_under_speech = d;
    }
    Ok(())
}

pub fn add_text(
    project: &mut Project,
    timeline_id: Option<&str>,
    text: &str,
    at: Time,
    duration: Time,
    overlay: Option<TextOverlay>,
) -> Result<String> {
    if text.trim().is_empty() {
        return Err(Error::invalid("overlay text must not be empty"));
    }
    if duration.nanos <= 0 {
        return Err(Error::invalid("overlay duration must be positive"));
    }
    let track_id = ensure_track(project, timeline_id, TrackKind::Overlay)?;
    let mut clip = Clip {
        name: crate::util::text::truncate(text, 40),
        timeline_start: at.clamp_non_negative(),
        source_in: Time::ZERO,
        source_out: duration,
        ..Clip::default()
    };
    let mut overlay = overlay.unwrap_or_default();
    overlay.text = text.to_string();
    clip.text = Some(overlay);
    let id = clip.id.clone();
    for timeline in project.timelines.iter_mut() {
        if let Some(track) = timeline.tracks.iter_mut().find(|t| t.id == track_id) {
            overlap_check(track, clip.range(), None)?;
            track.clips.push(clip);
            return Ok(id);
        }
    }
    Err(Error::not_found("overlay track disappeared"))
}

pub fn set_captions(
    project: &mut Project,
    cues: Vec<Cue>,
    language: &str,
    style: Option<CaptionStyle>,
    source: &str,
) -> Result<String> {
    for cue in &cues {
        if cue.end <= cue.start {
            return Err(Error::invalid(format!(
                "caption cue '{}' has a non-positive duration",
                cue.text
            )));
        }
    }
    let mut track = CaptionTrack {
        language: if language.is_empty() {
            "en".into()
        } else {
            language.to_string()
        },
        cues,
        ..CaptionTrack::default()
    };
    if let Some(style) = style {
        track.style = style;
    }
    track.source = source.to_string();
    let id = track.id.clone();
    project.captions = vec![track];
    Ok(id)
}

pub fn set_render_settings(project: &mut Project, render: RenderSettings) -> Result<()> {
    if render.width < 16 || render.height < 16 {
        return Err(Error::invalid("render dimensions are too small"));
    }
    if render.crf > 51 {
        return Err(Error::invalid("crf must be between 0 and 51"));
    }
    project.render = render;
    Ok(())
}

pub fn validate(project: &Project) -> Vec<String> {
    let mut issues = Vec::new();
    if project.timelines.is_empty() {
        issues.push("project has no timeline".to_string());
    }
    for asset in &project.assets {
        if asset.missing {
            issues.push(format!("missing asset: {}", asset.display_name()));
        }
    }
    for timeline in &project.timelines {
        if timeline.duration().nanos == 0 {
            issues.push(format!("timeline '{}' is empty", timeline.name));
        }
        for track in &timeline.tracks {
            let mut sorted: Vec<&Clip> = track.clips.iter().collect();
            sorted.sort_by_key(|c| c.timeline_start.nanos);
            for pair in sorted.windows(2) {
                if pair[0].range().overlaps(pair[1].range()) {
                    issues.push(format!(
                        "clips '{}' and '{}' overlap on track '{}'",
                        pair[0].name, pair[1].name, track.name
                    ));
                }
            }
            for clip in &track.clips {
                if clip.duration().nanos <= 0 {
                    issues.push(format!("clip '{}' has zero duration", clip.name));
                }
                if let Some(asset_id) = &clip.asset {
                    if project.asset(asset_id).is_none() {
                        issues.push(format!("clip '{}' references a missing asset", clip.name));
                    }
                }
            }
        }
    }
    issues
}

/// Motion presets that expand into keyframes. They replace any keyframes on the properties they set.
pub const ANIMATION_PRESETS: &[&str] = &[
    "push_in",
    "pull_out",
    "pan_left",
    "pan_right",
    "drift",
    "fade_in",
    "fade_out",
    "fade_in_out",
    "slide_in_left",
    "slide_in_right",
    "slide_in_up",
    "slide_in_down",
    "pop",
];

pub fn clear_keyframes(
    project: &mut Project,
    clip_id: &str,
    property: Option<&str>,
) -> Result<usize> {
    let clip = project
        .clip_mut(clip_id)
        .ok_or_else(|| Error::not_found(format!("clip {clip_id} not found")))?;
    let before: usize = clip.keyframes.iter().map(|k| k.keyframes.len()).sum();
    match property {
        Some(p) => clip.keyframes.retain(|k| k.property != p),
        None => clip.keyframes.clear(),
    }
    let after: usize = clip.keyframes.iter().map(|k| k.keyframes.len()).sum();
    Ok(before - after)
}

/// Expand a motion preset into keyframes. `amount` scales the strength (1.0 = default).
/// Returns the keyframes that were written as `(property, local_seconds, value)`.
pub fn apply_animation_preset(
    project: &mut Project,
    clip_id: &str,
    preset: &str,
    amount: f64,
    duration: Option<Time>,
) -> Result<Vec<(String, f64, f64)>> {
    let (start, total, canvas_w) = {
        let clip = project
            .clip(clip_id)
            .ok_or_else(|| Error::not_found(format!("clip {clip_id} not found")))?;
        let w = project.timeline().map(|t| t.width as f64).unwrap_or(1920.0);
        (clip.timeline_start, clip.duration().as_secs_f64(), w)
    };
    if total <= 0.0 {
        return Err(Error::invalid("clip has no duration to animate"));
    }
    let k = amount.clamp(0.1, 4.0);
    let ramp = duration
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.5)
        .clamp(0.05, total);
    // (property, [(local seconds, value, easing)])
    let mut plan: Vec<PlanStep> = Vec::new();
    match preset {
        "push_in" => plan.push((
            "scale",
            vec![(0.0, 1.0, "ease_in_out"), (total, 1.0 + 0.15 * k, "linear")],
        )),
        "pull_out" => plan.push((
            "scale",
            vec![(0.0, 1.0 + 0.15 * k, "ease_in_out"), (total, 1.0, "linear")],
        )),
        "pan_left" | "pan_right" => {
            let dx = 0.06 * canvas_w * k;
            let sign = if preset == "pan_left" { -1.0 } else { 1.0 };
            // Zoom just enough that the pan never reveals the canvas edge.
            plan.push((
                "scale",
                vec![(0.0, 1.0 + 2.0 * dx / canvas_w + 0.02, "linear")],
            ));
            plan.push((
                "x",
                vec![
                    (0.0, -sign * dx, "ease_in_out"),
                    (total, sign * dx, "linear"),
                ],
            ));
        }
        "drift" => {
            let dx = 0.02 * canvas_w * k;
            plan.push((
                "scale",
                vec![
                    (0.0, 1.06 + 0.04 * k, "ease_in_out"),
                    (total, 1.10 + 0.06 * k, "linear"),
                ],
            ));
            plan.push(("x", vec![(0.0, -dx, "ease_in_out"), (total, dx, "linear")]));
        }
        "fade_in" => plan.push((
            "opacity",
            vec![(0.0, 0.0, "ease_out"), (ramp, 1.0, "linear")],
        )),
        "fade_out" => plan.push((
            "opacity",
            vec![
                ((total - ramp).max(0.0), 1.0, "ease_in"),
                (total, 0.0, "linear"),
            ],
        )),
        "fade_in_out" => {
            let r = ramp.min(total / 2.0);
            plan.push((
                "opacity",
                vec![
                    (0.0, 0.0, "ease_out"),
                    (r, 1.0, "linear"),
                    (total - r, 1.0, "ease_in"),
                    (total, 0.0, "linear"),
                ],
            ));
        }
        "slide_in_left" | "slide_in_right" => {
            let from = canvas_w * 0.25 * k * if preset == "slide_in_left" { -1.0 } else { 1.0 };
            plan.push(("x", vec![(0.0, from, "ease_out"), (ramp, 0.0, "linear")]));
            plan.push((
                "opacity",
                vec![(0.0, 0.0, "ease_out"), (ramp, 1.0, "linear")],
            ));
        }
        "slide_in_up" | "slide_in_down" => {
            let from = 120.0 * k * if preset == "slide_in_up" { 1.0 } else { -1.0 };
            plan.push(("y", vec![(0.0, from, "ease_out"), (ramp, 0.0, "linear")]));
            plan.push((
                "opacity",
                vec![(0.0, 0.0, "ease_out"), (ramp, 1.0, "linear")],
            ));
        }
        "pop" => {
            plan.push((
                "scale",
                vec![(0.0, 0.85, "ease_out"), (ramp.min(0.4), 1.0, "linear")],
            ));
            plan.push((
                "opacity",
                vec![(0.0, 0.0, "ease_out"), (ramp.min(0.4), 1.0, "linear")],
            ));
        }
        other => {
            return Err(Error::invalid(format!(
                "unknown animation preset '{other}'; presets: {}",
                ANIMATION_PRESETS.join(", ")
            )))
        }
    }
    let mut written = Vec::new();
    for (property, frames) in plan {
        clear_keyframes(project, clip_id, Some(property))?;
        // A single keyframe holds a constant; nothing animates, so a lone point is only used for fixed scale.
        for (local, value, easing) in frames {
            add_keyframe(
                project,
                clip_id,
                property,
                start + Time::from_secs_f64(local),
                value,
                easing,
            )?;
            written.push((property.to_string(), local, value));
        }
    }
    Ok(written)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::project::model::{Asset, AssetKind, MediaInfo};
    use crate::util::time::parse_fps;

    fn project_with_clips() -> (Project, String, String) {
        let mut project = Project::new("test");
        let timeline_id =
            create_timeline(&mut project, "Main", 1920, 1080, parse_fps("30").unwrap()).unwrap();
        let info = MediaInfo {
            duration: Time::from_secs_f64(10.0),
            width: 1920,
            height: 1080,
            has_video: true,
            ..MediaInfo::default()
        };
        let asset = Asset {
            id: "asset_1".into(),
            path: "assets/a.mp4".into(),
            kind: AssetKind::Video,
            bytes: 1,
            imported_at: String::new(),
            sha256: "x".into(),
            info: Some(info),
            missing: false,
            provenance: None,
        };
        project.add_asset(asset);
        (project, timeline_id, "asset_1".into())
    }

    #[test]
    fn appends_clips_back_to_back() {
        let (mut project, timeline_id, asset_id) = project_with_clips();
        let a = append_clip(&mut project, Some(&timeline_id), None, &asset_id).unwrap();
        let b = append_clip(&mut project, Some(&timeline_id), None, &asset_id).unwrap();
        let (_, track, clip_a) = project.find_clip(&a).unwrap();
        let end = clip_a.timeline_end();
        let (_, _, clip_b) = project.find_clip(&b).unwrap();
        assert_eq!(clip_b.timeline_start, end);
        assert_eq!(track.clips.len(), 2);
    }

    #[test]
    fn rejects_overlap() {
        let (mut project, timeline_id, asset_id) = project_with_clips();
        let track_id = project.timeline_by_id(&timeline_id).unwrap().tracks[0]
            .id
            .clone();
        insert_clip(
            &mut project,
            Some(&timeline_id),
            Some(&track_id),
            ClipRequest {
                asset_id: &asset_id,
                source_in: Some(Time::ZERO),
                source_out: Some(Time::from_secs_f64(5.0)),
                at: Some(Time::ZERO),
                name: None,
            },
        )
        .unwrap();
        let err = insert_clip(
            &mut project,
            Some(&timeline_id),
            Some(&track_id),
            ClipRequest {
                asset_id: &asset_id,
                source_in: Some(Time::ZERO),
                source_out: Some(Time::from_secs_f64(5.0)),
                at: Some(Time::from_secs_f64(2.0)),
                name: None,
            },
        )
        .unwrap_err();
        assert_eq!(err.code, crate::error::ErrorCode::Conflict);
    }

    #[test]
    fn split_produces_contiguous_halves() {
        let (mut project, timeline_id, asset_id) = project_with_clips();
        let clip = append_clip(&mut project, Some(&timeline_id), None, &asset_id).unwrap();
        let (left, right) = split_clip(&mut project, &clip, Time::from_secs_f64(4.0)).unwrap();
        let (_, _, l) = project.find_clip(&left).unwrap();
        let (_, _, r) = project.find_clip(&right).unwrap();
        assert_eq!(l.source_out, r.source_in);
        assert_eq!(r.timeline_start, Time::from_secs_f64(4.0));
        let total = l.duration() + r.duration();
        assert!((total.as_secs_f64() - 10.0).abs() < 0.001);
        assert!(validate(&project).is_empty());
    }

    #[test]
    fn speed_halves_duration() {
        let (mut project, timeline_id, asset_id) = project_with_clips();
        let clip = append_clip(&mut project, Some(&timeline_id), None, &asset_id).unwrap();
        set_speed(&mut project, &clip, 2.0).unwrap();
        let (_, _, c) = project.find_clip(&clip).unwrap();
        assert!((c.duration().as_secs_f64() - 5.0).abs() < 0.001);
    }

    #[test]
    fn ripple_delete_closes_gap() {
        let (mut project, timeline_id, asset_id) = project_with_clips();
        let a = insert_clip(
            &mut project,
            Some(&timeline_id),
            None,
            ClipRequest {
                asset_id: &asset_id,
                source_in: Some(Time::ZERO),
                source_out: Some(Time::from_secs_f64(2.0)),
                at: Some(Time::ZERO),
                name: None,
            },
        )
        .unwrap();
        let b = insert_clip(
            &mut project,
            Some(&timeline_id),
            None,
            ClipRequest {
                asset_id: &asset_id,
                source_in: Some(Time::ZERO),
                source_out: Some(Time::from_secs_f64(3.0)),
                at: Some(Time::from_secs_f64(2.0)),
                name: None,
            },
        )
        .unwrap();
        ripple_delete(&mut project, &a).unwrap();
        let (_, _, clip_b) = project.find_clip(&b).unwrap();
        assert_eq!(clip_b.timeline_start, Time::ZERO);
    }
}
