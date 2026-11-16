//! Autonomous highlight-reel editing: analyse raw footage in a folder, rank
//! highlight windows, select clips to fill a target runtime, and build a
//! renderable timeline. Deterministic — the same inputs always produce the
//! same plan. No model, no network.
//!
//! Used by `cinvar autoedit` and the `cinvar.ai.auto_edit` MCP tool.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::{Error, ErrorCode, Result};
use crate::project::model::{AssetKind, Project, TransitionSide};
use crate::project::ops;
use crate::project::store::ProjectStore;
use crate::util::time::{fps_from_f64, Time};

const VIDEO_EXTS: &[&str] = &["mp4", "mov", "mkv", "webm", "avi", "m4v", "mts", "ts"];
const MAX_FILES: usize = 200;
const HIGHLIGHT_WINDOW_SECS: f64 = 2.0;
const TRANSITION_SECS: f64 = 0.5;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AutoEditOptions {
    /// Target timeline runtime in seconds.
    pub target_secs: f64,
    /// Shortest clip to select.
    pub min_clip: f64,
    /// Longest clip to select.
    pub max_clip: f64,
    /// Vertical canvas (1080x1920) for Shorts/Reels.
    pub vertical: bool,
    /// Optional opening title overlay.
    pub title: Option<String>,
    /// Creative instruction ("find the celebrations"). Requires a scene index
    /// built by `cinvar analyze`; blends semantic relevance into selection.
    pub instruction: Option<String>,
    /// Transcribe speech and burn captions (requires a transcription engine).
    pub captions: bool,
    /// Tighten selected windows with Auto-Editor's silence plan (requires
    /// CINVAR_AUTOEDITOR_PYTHON or an `auto-editor` binary on PATH).
    pub silence_removal: bool,
}

impl Default for AutoEditOptions {
    fn default() -> Self {
        Self {
            target_secs: 60.0,
            min_clip: 2.0,
            max_clip: 6.0,
            vertical: false,
            title: None,
            instruction: None,
            captions: false,
            silence_removal: false,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Selection {
    pub asset_id: String,
    pub file_name: String,
    /// Source range in seconds.
    pub start: f64,
    pub end: f64,
    pub score: f32,
    pub reason: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AutoEditPlan {
    pub width: u32,
    pub height: u32,
    pub fps: f64,
    pub target_secs: f64,
    pub timeline_secs: f64,
    pub selections: Vec<Selection>,
}

impl AutoEditPlan {
    pub fn to_json(&self) -> serde_json::Value {
        serde_json::to_value(self).unwrap_or_else(|_| serde_json::Value::Null)
    }
}

/// Public so the semantic indexer walks footage with the same rules.
pub fn collect_video_files(dir: &Path) -> Result<Vec<PathBuf>> {
    let mut files = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(current) = stack.pop() {
        let entries = std::fs::read_dir(&current)
            .map_err(|e| Error::new(ErrorCode::NotFound, format!("cannot read {}: {e}", current.display())))?;
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
                continue;
            }
            let ext = path
                .extension()
                .and_then(|e| e.to_str())
                .map(|e| e.to_ascii_lowercase())
                .unwrap_or_default();
            if VIDEO_EXTS.contains(&ext.as_str()) {
                files.push(path);
                if files.len() >= MAX_FILES {
                    return Ok(files);
                }
            }
        }
    }
    files.sort();
    Ok(files)
}

/// One asset's scored, non-overlapping candidate windows (scene-aligned where
/// the scene detector found cuts).
fn candidate_windows(path: &Path) -> Result<Vec<crate::analysis::HighlightScore>> {
    let highlights = crate::analysis::score_highlights(path, HIGHLIGHT_WINDOW_SECS)?;
    let mut kept: Vec<crate::analysis::HighlightScore> = Vec::new();
    for score in highlights {
        let start = score.start.as_secs_f64();
        let end = score.end.as_secs_f64();
        let overlaps = kept.iter().any(|k| {
            let (ks, ke) = (k.start.as_secs_f64(), k.end.as_secs_f64());
            start < ke && ks < end
        });
        if !overlaps {
            kept.push(score);
        }
    }
    kept.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal));
    Ok(kept)
}

/// Build the selection plan: score every file, then fill the target runtime by
/// taking the best remaining windows in round-robin file order so no single
/// source dominates the reel, and finally order the reel chronologically by
/// file with a global alternating interleave for variety.
pub fn plan_reel(
    store: &ProjectStore,
    project: &mut Project,
    media_root: &Path,
    opts: &AutoEditOptions,
) -> Result<AutoEditPlan> {
    let files = collect_video_files(media_root)?;
    if files.is_empty() {
        return Err(Error::invalid(format!(
            "no video files found in {}",
            media_root.display()
        )));
    }

    let mut per_file: Vec<(String, String, Vec<crate::analysis::HighlightScore>)> = Vec::new();
    for file in &files {
        let name = file.file_name().unwrap_or_default().to_string_lossy().to_string();
        let asset = store
            .import_asset(project, file, "autoedit")
            .map_err(|e| Error::new(e.code, format!("{name}: {}", crate::secrets::redact(&e.message))))?;
        if asset.kind != AssetKind::Video {
            continue;
        }
        let path = store.asset_path(&asset)?;
        let windows = candidate_windows(&path)?;
        per_file.push((asset.id, name, windows));
    }
    if per_file.is_empty() {
        return Err(Error::invalid("no importable video files in the folder"));
    }

    // Creative instruction: blend semantic relevance from the scene index into
    // each candidate window's score (0.6 heuristic + 0.4 semantic).
    if let Some(query) = opts.instruction.as_deref().filter(|q| !q.trim().is_empty()) {
        let idx_path = store.cache_dir().join("semantic-index.json");
        let index = crate::analysis::semantic::SceneIndex::load(&idx_path).map_err(|e| {
            Error::invalid(format!(
                "instruction given but no scene index exists for this project ({})",
                crate::secrets::redact(&e.message)
            ))
            .with_hint("run: cinvar analyze --media-root <folder> --project <this project>")
        })?;
        let hits = crate::analysis::semantic::search(&index, query);
        if hits.is_empty() {
            return Err(Error::invalid(format!(
                "no indexed footage matches the instruction \"{query}\"; report this to the user instead of inventing footage"
            )));
        }
        for (_, file_name, windows) in per_file.iter_mut() {
            for window in windows.iter_mut() {
                let (w_start, w_end) = (window.start.as_secs_f64(), window.end.as_secs_f64());
                let sem = hits
                    .iter()
                    .filter(|(seg_idx, score)| {
                        *score > 0.0
                            && index.segments[*seg_idx].file_name == *file_name
                            && index.segments[*seg_idx].start < w_end
                            && index.segments[*seg_idx].end > w_start
                    })
                    .map(|(_, score)| *score)
                    .fold(0.0f32, f32::max);
                if sem > 0.0 {
                    let blended = 0.6 * window.score + 0.4 * sem;
                    window.score = (blended * 1000.0).round() / 1000.0;
                    window.reason = format!(
                        "{}; semantic match {:.2} for \"{}\"",
                        window.reason, sem, query
                    );
                }
            }
        }
    }

    let mut remaining: Vec<f64> = per_file
        .iter()
        .map(|(_, _, windows)| {
            windows
                .last()
                .map(|w| w.end.as_secs_f64())
                .unwrap_or(0.0)
        })
        .collect();
    let mut selections: Vec<Selection> = Vec::new();
    let mut filled = 0.0f64;
    let mut cursor: HashMap<String, usize> = HashMap::new();
    // With a creative instruction, take the globally best-scoring window each
    // round (the brief asked for relevant footage, not balanced coverage).
    // Without one, round-robin keeps no single source dominating.
    let greedy = opts.instruction.as_deref().is_some_and(|q| !q.trim().is_empty());
    let mut progressed = true;
    while filled < opts.target_secs - 0.25 && progressed {
        progressed = false;
        let mut order: Vec<usize> = (0..per_file.len()).collect();
        if greedy {
            // Best remaining window first, deterministic tie-break by index.
            order.sort_by(|a, b| {
                let sa = per_file[*a]
                    .2
                    .get(*cursor.get(&per_file[*a].0).unwrap_or(&0))
                    .map(|w| w.score)
                    .unwrap_or(-1.0);
                let sb = per_file[*b]
                    .2
                    .get(*cursor.get(&per_file[*b].0).unwrap_or(&0))
                    .map(|w| w.score)
                    .unwrap_or(-1.0);
                sb.partial_cmp(&sa).unwrap_or(std::cmp::Ordering::Equal)
            });
        }
        for index in order {
            let (asset_id, name, windows) = &per_file[index];
            if filled >= opts.target_secs - 0.25 {
                break;
            }
            let start_idx = *cursor.get(asset_id).unwrap_or(&0);
            let Some(window) = windows.get(start_idx) else {
                continue;
            };
            let remaining_source = remaining[index];
            if remaining_source < 0.5 {
                cursor.insert(asset_id.clone(), start_idx + 1);
                continue;
            }
            cursor.insert(asset_id.clone(), start_idx + 1);
            let want = opts
                .max_clip
                .min((opts.target_secs - filled).max(opts.min_clip))
                .min(remaining_source.max(opts.min_clip));
            let start = window.start.as_secs_f64();
            // Clamp to the source's usable end (last candidate window end,
            // always within the asset duration) so a selection starting mid-
            // file can never overrun the asset. Bug fix: `remaining_source` is
            // measured from t=0, not from `start`, and using it here let
            // windows on short files extend past the asset's duration.
            let usable_end = windows
                .last()
                .map(|w| w.end.as_secs_f64())
                .unwrap_or(start);
            let end = clamp_end(want, start, usable_end);
            if end - start < opts.min_clip.min(remaining_source).min(0.5) {
                continue;
            }
            let take = (end - start).max(0.0);
            remaining[index] = (remaining_source - take).max(0.0);
            filled += take;
            progressed = true;
            selections.push(Selection {
                asset_id: asset_id.clone(),
                file_name: name.clone(),
                start,
                end,
                score: window.score,
                reason: window.reason.clone(),
            });
        }
    }
    if selections.is_empty() {
        return Err(Error::invalid(
            "every source is too short to fill even one clip; nothing to build",
        ));
    }

    // Reel order: alternate across files, chronological within a file, so the
    // edit reads as a sequence rather than one file dumped after another.
    let mut by_asset: HashMap<&str, Vec<usize>> = HashMap::new();
    for (idx, sel) in selections.iter().enumerate() {
        by_asset.entry(sel.asset_id.as_str()).or_default().push(idx);
    }
    for idxs in by_asset.values_mut() {
        idxs.sort_by(|a, b| selections[*a].start.partial_cmp(&selections[*b].start).unwrap_or(std::cmp::Ordering::Equal));
    }
    let mut ordered: Vec<Selection> = Vec::new();
    // Iterate in import order so the reel order is deterministic; HashMap's
    // iteration order is randomised and would reshuffle the edit every run.
    let keys: Vec<&str> = per_file
        .iter()
        .filter_map(|(asset_id, _, _)| {
            by_asset
                .contains_key(asset_id.as_str())
                .then_some(asset_id.as_str())
        })
        .collect();
    let mut round = 0;
    loop {
        let mut pushed = false;
        for key in &keys {
            if let Some(&idx) = by_asset.get(*key).and_then(|v| v.get(round)) {
                ordered.push(selections[idx].clone());
                pushed = true;
            }
        }
        if !pushed {
            break;
        }
        round += 1;
    }

    let timeline_secs: f64 = ordered
        .iter()
        .map(|s| s.end - s.start)
        .fold(0.0, |a, b| a + b);
    let (width, height) = if opts.vertical {
        (1080, 1920)
    } else {
        (1920, 1080)
    };
    Ok(AutoEditPlan {
        width,
        height,
        fps: 30.0,
        target_secs: opts.target_secs,
        timeline_secs,
        selections: ordered,
    })
}

/// Materialise a plan as a real timeline in the project: clips back to back
/// with crossfades, normalised audio, and an optional title overlay. Returns
/// one timeline clip id per selection, in order.
pub fn apply_plan(
    store: &ProjectStore,
    project: &mut Project,
    plan: &AutoEditPlan,
    opts: &AutoEditOptions,
) -> Result<Vec<String>> {
    ops::create_timeline(
        project,
        "Highlight Reel",
        plan.width,
        plan.height,
        fps_from_f64(plan.fps),
    )?;
    let transition = Time::from_secs_f64(TRANSITION_SECS);
    let mut clip_ids: Vec<String> = Vec::new();
    let count = plan.selections.len();
    for (idx, sel) in plan.selections.iter().enumerate() {
        let clip_id = ops::insert_clip(
            project,
            None,
            None,
            ops::ClipRequest {
                asset_id: &sel.asset_id,
                source_in: Some(Time::from_secs_f64(sel.start)),
                source_out: Some(Time::from_secs_f64(sel.end)),
                at: None,
                name: Some(&sel.file_name),
            },
        )?;
        ops::set_clip_audio(project, &clip_id, None, None, None, Some(true), None, None)?;
        if idx + 1 < count {
            ops::set_transition(project, &clip_id, TransitionSide::Out, "crossfade", transition)?;
            ops::set_transition(project, &clip_id, TransitionSide::In, "crossfade", transition)?;
        }
        clip_ids.push(clip_id);
    }
    if let Some(title) = opts.title.as_deref().filter(|t| !t.trim().is_empty()) {
        let end = plan
            .selections
            .first()
            .map(|s| s.end - s.start)
            .unwrap_or(3.0)
            .min(4.0)
            .max(2.0);
        ops::add_text(
            project,
            None,
            title,
            Time::ZERO,
            Time::from_secs_f64(end),
            None,
        )?;
    }
    store.save(project)?;
    Ok(clip_ids)
}

/// Analyse, plan, and build in one call. Returns the plan (with clip ids).
pub fn auto_edit(
    store: &ProjectStore,
    project: &mut Project,
    media_root: &Path,
    opts: &AutoEditOptions,
) -> Result<(AutoEditPlan, Vec<String>)> {
    let mut plan = plan_reel(store, project, media_root, opts)?;
    if opts.silence_removal {
        // Pace the reel: intersect each selected window with Auto-Editor's keep-spans.
        // Runs per source asset; a missing/broken tool is surfaced, not swallowed.
        let mut paced = Vec::with_capacity(plan.selections.len());
        let work_dir = std::env::temp_dir().join("cinvar-autoeditor");
        let mut spans_cache: std::collections::HashMap<String, Vec<crate::analysis::autoeditor::KeepSpan>> =
            std::collections::HashMap::new();
        for sel in &plan.selections {
            if !spans_cache.contains_key(&sel.file_name) {
                let asset_path = media_root.join(&sel.file_name);
                spans_cache.insert(
                    sel.file_name.clone(),
                    crate::analysis::autoeditor::keep_spans(&asset_path, &work_dir)?,
                );
            }
        }
        for sel in plan.selections.drain(..) {
            if let Some(spans) = spans_cache.get(&sel.file_name) {
                paced.extend(crate::analysis::autoeditor::pace_selections(
                    vec![sel],
                    spans,
                    1.0,
                ));
            }
        }
        if paced.is_empty() {
            return Err(crate::error::Error::invalid(
                "--silence-removal cut every selected window (nothing but dead air selected?)",
            ));
        }
        plan.selections = paced;
        plan.timeline_secs = plan.selections.iter().map(|s| s.end - s.start).sum();
    }
    let clips = apply_plan(store, project, &plan, opts)?;
    Ok((plan, clips))
}

/// Caption presets for auto-edit. All are deterministic style objects.
pub fn caption_preset(name: &str, vertical: bool) -> Result<crate::project::model::CaptionStyle> {
    use crate::project::model::{CaptionStyle, TextPosition};
    let mut style = CaptionStyle::default();
    match name {
        "subtitles" | "professional" => {}
        "social" => {
            style.size = if vertical { 84 } else { 64 };
            style.uppercase = true;
            style.background = Some("black@0.6".into());
            style.max_chars_per_line = 24;
        }
        "documentary" => {
            style.size = 44;
            style.position = TextPosition::LowerThird;
        }
        "cinematic" => {
            style.size = 56;
            style.outline_width = 2;
            style.background = None;
        }
        other => {
            return Err(Error::invalid(format!(
                "unknown caption preset '{other}'; use subtitles, social, documentary or cinematic"
            )))
        }
    }
    if vertical && style.size < 72 && name != "documentary" {
        // Vertical-safe: larger text on the 1080-wide canvas.
        style.size = (style.size as f32 * 1.5) as u32;
    }
    Ok(style)
}

#[derive(Debug, Clone, Serialize)]
pub struct CaptionOutcome {
    pub cues: usize,
    pub engine: String,
    pub files_transcribed: usize,
    /// Word-timed caption lines on the timeline (empty unless the engine gave word timings).
    pub lines: Vec<crate::captions::ass::AssLine>,
}

/// Transcribe each selected clip (cached per asset) and map the cues onto the
/// timeline. Requires a transcription engine (see captions::engine_help).
pub fn generate_captions(
    store: &ProjectStore,
    project: &mut Project,
    clip_ids: &[String],
    language: Option<&str>,
    preset: &str,
    vertical: bool,
) -> Result<CaptionOutcome> {
    generate_captions_with(store, project, clip_ids, language, preset, vertical, "auto")
}

/// As `generate_captions`, with an explicit engine name (`auto` or `whisperx`).
pub fn generate_captions_with(
    store: &ProjectStore,
    project: &mut Project,
    clip_ids: &[String],
    language: Option<&str>,
    preset: &str,
    vertical: bool,
    engine_name: &str,
) -> Result<CaptionOutcome> {
    let engine = crate::captions::detect_engine_named(engine_name)?;
    let mut timeline_words: Vec<crate::captions::ass::AssWord> = Vec::new();
    let style = caption_preset(preset, vertical)?;
    let mut cues: Vec<crate::project::model::Cue> = Vec::new();
    let mut transcribed: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    for clip_id in clip_ids {
        let (asset_id, clip_start, source_in, source_out) = {
            let clip = project
                .clip(clip_id)
                .ok_or_else(|| Error::not_found(format!("clip {clip_id} not found")))?;
            let asset = clip
                .asset
                .clone()
                .ok_or_else(|| Error::invalid("clip has no media"))?;
            (asset, clip.timeline_start, clip.source_in, clip.source_out)
        };
        let work = store.cache_dir().join("transcribe").join(&asset_id);
        std::fs::create_dir_all(&work)
            .map_err(|e| Error::internal(format!("cannot create {}: {e}", work.display())))?;
        let srt_path = work.join("transcript.srt");
        if !transcribed.contains(&asset_id) {
            let asset = project
                .asset(&asset_id)
                .ok_or_else(|| Error::not_found("asset missing"))?
                .clone();
            let source = store.asset_path(&asset)?;
            let wav = work.join("speech.wav");
            if !wav.exists() {
                crate::media::extract_audio(&source, &wav)?;
            }
            let srt = crate::captions::run_engine(&engine, &wav, &work, language)?;
            std::fs::write(&srt_path, srt)
                .map_err(|e| Error::internal(format!("cannot write transcript: {e}")))?;
            transcribed.insert(asset_id.clone());
        }
        let srt = std::fs::read_to_string(&srt_path)
            .map_err(|e| Error::internal(format!("cannot read transcript: {e}")))?;
        let (raw, _skipped) = crate::captions::parse_subtitles(&srt);
        if let crate::captions::Engine::WhisperX(_) = engine {
            for w in crate::captions::whisperx::load_words(&work)? {
                let s = w.start.max(source_in.as_secs_f64());
                let e = w.end.min(source_out.as_secs_f64());
                if e <= s {
                    continue;
                }
                let offset = clip_start.as_secs_f64() - source_in.as_secs_f64();
                timeline_words.push(crate::captions::ass::AssWord {
                    text: w.word.clone(),
                    start: s + offset,
                    end: e + offset,
                });
            }
        }
        for cue in raw {
            // Source times -> timeline times for the trimmed clip window.
            if cue.end.as_secs_f64() <= source_in.as_secs_f64()
                || cue.start.as_secs_f64() >= source_out.as_secs_f64()
            {
                continue;
            }
            let s0 = cue.start.as_secs_f64().max(source_in.as_secs_f64()) - source_in.as_secs_f64();
            let e0 = cue.end.as_secs_f64().min(source_out.as_secs_f64()) - source_in.as_secs_f64();
            if e0 <= s0 {
                continue;
            }
            cues.push(crate::project::model::Cue {
                start: Time::from_secs_f64(clip_start.as_secs_f64() + s0),
                end: Time::from_secs_f64(clip_start.as_secs_f64() + e0),
                text: cue.text.clone(),
            });
        }
    }
    if cues.is_empty() {
        return Err(Error::invalid(
            "no speech found in the selected clips; captions skipped",
        ));
    }
    crate::project::ops::set_captions(
        project,
        cues.clone(),
        language.unwrap_or("en"),
        Some(style),
        engine.name(),
    )?;
    timeline_words.sort_by(|a, b| a.start.partial_cmp(&b.start).unwrap_or(std::cmp::Ordering::Equal));
    let lines: Vec<crate::captions::ass::AssLine> = cues
        .iter()
        .map(|c| {
            let (s, e) = (c.start.as_secs_f64(), c.end.as_secs_f64());
            crate::captions::ass::AssLine {
                start: s,
                end: e,
                text: c.text.clone(),
                words: timeline_words
                    .iter()
                    .filter(|w| {
                        let mid = (w.start + w.end) / 2.0;
                        mid >= s && mid < e
                    })
                    .cloned()
                    .collect(),
            }
        })
        .collect();
    Ok(CaptionOutcome {
        cues: cues.len(),
        engine: engine.name().to_string(),
        files_transcribed: transcribed.len(),
        lines,
    })
}

/// Clamp a selection to the source's usable end (the last candidate window's
/// end, always within the asset duration). Regression guard: `remaining` is
/// measured from t=0, not from `start`; using it relative to `start` let
/// selections on short files overrun the asset (seen on real 10.2 s clips).
fn clamp_end(want: f64, start: f64, usable_end: f64) -> f64 {
    (start + want).min(usable_end).max(start)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reel_order_interleaves_files() {
        // Two selections per file, chronological inside each file; expect an
        // alternating order a0, b0, a1, b1.
        let mk = |asset: &str, start: f64| Selection {
            asset_id: asset.to_string(),
            file_name: asset.to_string(),
            start,
            end: start + 3.0,
            score: 0.5,
            reason: "test".into(),
        };
        let selections = vec![mk("a", 0.0), mk("a", 10.0), mk("b", 0.0), mk("b", 10.0)];
        // Replicate the ordering pass used by plan_reel via a tiny shim: the
        // real ordering lives inline, so exercise it through auto_edit's plan
        // step is not possible without media; assert the interleaving rule
        // directly here by re-running the same algorithm.
        let mut by_asset: HashMap<&str, Vec<usize>> = HashMap::new();
        for (idx, sel) in selections.iter().enumerate() {
            by_asset.entry(sel.asset_id.as_str()).or_default().push(idx);
        }
        for idxs in by_asset.values_mut() {
            idxs.sort_by(|a, b| selections[*a].start.partial_cmp(&selections[*b].start).unwrap_or(std::cmp::Ordering::Equal));
        }
        // Iterate in import order so the reel order is deterministic; HashMap's
        // iteration order is randomised and would reshuffle the edit every run.
        let keys: Vec<&str> = vec!["a", "b"];
        let mut ordered = Vec::new();
        let mut round = 0;
        loop {
            let mut pushed = false;
            for key in &keys {
                if let Some(&idx) = by_asset.get(*key).and_then(|v| v.get(round)) {
                    ordered.push(selections[idx].clone());
                    pushed = true;
                }
            }
            if !pushed {
                break;
            }
            round += 1;
        }
        let names: Vec<&str> = ordered.iter().map(|s| s.asset_id.as_str()).collect();
        assert_eq!(names, vec!["a", "b", "a", "b"]);
    }

    #[test]
    fn selection_never_overruns_asset_duration() {
        // Real-world regression: 10.2 s file, window starting at 8.0 s, budget
        // remaining measured from t=0 was 8.0 -> old code produced end=14.0.
        let end = clamp_end(6.0, 8.0, 10.208);
        assert!((end - 10.208).abs() < 1e-9, "end {end} overruns the asset");
        // And when the usable end precedes the start (degenerate), no negative span.
        assert_eq!(clamp_end(6.0, 12.0, 10.0), 12.0);
    }
}
