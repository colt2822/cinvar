use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use crate::util::proc::{self, ProcOpts};
use crate::util::time::Time;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HighlightScore {
    pub start: Time,
    pub end: Time,
    pub score: f32,
    pub reason: String,
}

/// Hard ceilings keep a two-hour source from turning into thousands of decoder
/// launches. Both are deterministic: the same input always yields the same grid.
const MAX_WINDOWS: usize = 120;
const MAX_HIGHLIGHTS: usize = 50;
const MIN_WINDOW_SECS: f64 = 0.25;
const MAX_WINDOW_SECS: f64 = 600.0;
const SCENE_THRESHOLD: f64 = 0.30;
const SILENT_DB: f64 = -60.0;
const LOUD_DB: f64 = -6.0;
const CUTS_FOR_FULL_MARK: f32 = 3.0;

/// Scene cuts measured with ffmpeg's scene filter, used when the media module
/// cannot supply them. Timestamps are decoded positions in seconds.
fn local_scene_cuts(path: &Path, threshold: f64) -> Result<Vec<f64>> {
    let bin = crate::qa::probe::ffmpeg()?;
    let filter = format!("select=gt(scene\\,{threshold:.3}),showinfo");
    let args = vec![
        "-hide_banner".to_string(),
        "-nostdin".to_string(),
        "-i".to_string(),
        path.to_string_lossy().to_string(),
        "-an".to_string(),
        "-vf".to_string(),
        filter,
        "-f".to_string(),
        "null".to_string(),
        "-".to_string(),
    ];
    let out = proc::run(&bin, &args, &ProcOpts::default().with_timeout(900))?;
    if !out.success {
        return Err(Error::new(
            crate::error::ErrorCode::Render,
            format!(
                "scene detection failed: {}",
                crate::secrets::redact(&out.tail_stderr(3))
            ),
        ));
    }
    Ok(parse_scene_times(&out.stderr))
}

fn parse_scene_times(stderr: &str) -> Vec<f64> {
    let mut cuts = Vec::new();
    for line in stderr.lines() {
        if !line.contains("pts_time:") {
            continue;
        }
        let Some(idx) = line.find("pts_time:") else {
            continue;
        };
        let rest = &line[idx + "pts_time:".len()..];
        let token = rest.split_whitespace().next().unwrap_or("");
        if let Ok(seconds) = token.parse::<f64>() {
            if seconds > 0.0 {
                cuts.push((seconds * 1000.0).round() / 1000.0);
            }
        }
    }
    cuts.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    cuts.dedup();
    cuts
}

fn scene_cuts(path: &Path) -> Result<Vec<f64>> {
    match crate::media::detect_scenes(path, SCENE_THRESHOLD as f32) {
        Ok(cuts) => Ok(cuts
            .iter()
            .map(|cut| cut.at.as_secs_f64())
            .filter(|seconds| *seconds > 0.0)
            .collect()),
        Err(_) => local_scene_cuts(path, SCENE_THRESHOLD),
    }
}

fn loudness_word(mean_db: f64) -> &'static str {
    if mean_db >= -18.0 {
        "loud"
    } else if mean_db >= -35.0 {
        "moderate"
    } else {
        "quiet"
    }
}

/// Combines the two measured signals into one deterministic 0..1 score.
fn combine(cuts: usize, mean_db: Option<f64>) -> (f32, String) {
    let cut_component = (cuts as f32 / CUTS_FOR_FULL_MARK).min(1.0);
    match mean_db {
        Some(mean) => {
            let audio_component =
                (((mean - SILENT_DB) / (LOUD_DB - SILENT_DB)).clamp(0.0, 1.0)) as f32;
            let score = (0.45 * cut_component + 0.55 * audio_component).clamp(0.0, 1.0);
            let reason = format!(
                "{cuts} scene cut(s) in window; audio mean {mean:.1} dBFS ({})",
                loudness_word(mean)
            );
            (round3f(score), reason)
        }
        None => {
            let score = cut_component.clamp(0.0, 1.0);
            (
                round3f(score),
                format!("{cuts} scene cut(s) in window; no audio stream to weigh"),
            )
        }
    }
}

fn round3f(value: f32) -> f32 {
    (value * 1000.0).round() / 1000.0
}

pub fn score_highlights(path: &Path, window_secs: f64) -> Result<Vec<HighlightScore>> {
    if !(MIN_WINDOW_SECS..=MAX_WINDOW_SECS).contains(&window_secs) {
        return Err(Error::invalid(format!(
            "window_secs must be between {MIN_WINDOW_SECS} and {MAX_WINDOW_SECS}, got {window_secs}"
        )));
    }
    let info = crate::qa::probe::probe(path)?;
    let total = info.duration.as_secs_f64();
    if total <= 0.0 {
        return Err(Error::invalid(format!(
            "cannot score highlights: {} has no measurable duration",
            path.display()
        )));
    }

    // Coarsen the grid rather than dropping coverage when the window is tiny.
    let window = window_secs.max(total / MAX_WINDOWS as f64);
    let count = ((total / window).ceil() as usize).clamp(1, MAX_WINDOWS);
    let cuts = scene_cuts(path).unwrap_or_default();

    let mut scores: Vec<HighlightScore> = Vec::with_capacity(count);
    for index in 0..count {
        let start = index as f64 * window;
        if start >= total {
            break;
        }
        let end = (start + window).min(total);
        let in_window = cuts.iter().filter(|c| **c >= start && **c < end).count();
        let mean_db = if info.has_audio && end > start {
            crate::qa::probe::volume_stats(
                path,
                Some(Time::from_secs_f64(start)),
                Some(Time::from_secs_f64(end - start)),
            )
            .ok()
            .and_then(|stats| stats.mean_db)
        } else {
            None
        };
        let (score, reason) = combine(in_window, mean_db);
        scores.push(HighlightScore {
            start: Time::from_secs_f64(start),
            end: Time::from_secs_f64(end),
            score,
            reason,
        });
    }

    scores.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(a.start.nanos.cmp(&b.start.nanos))
    });
    scores.truncate(MAX_HIGHLIGHTS);
    Ok(scores)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_scene_times_from_showinfo() {
        let stderr = "[Parsed_showinfo_1 @ 0x1] n:0 pts:0 pts_time:0 type:I mean:[0 0 0]\n\
                      [Parsed_showinfo_1 @ 0x1] n:12 pts:12 pts_time:0.4 type:I mean:[10 10 10]\n\
                      [Parsed_showinfo_1 @ 0x1] n:61 pts:61 pts_time:2.033333 type:I mean:[20 20 20]";
        let cuts = parse_scene_times(stderr);
        assert_eq!(cuts, vec![0.4, 2.033]);
    }

    #[test]
    fn quiet_windows_score_lower_than_loud_ones() {
        let (quiet, quiet_reason) = combine(0, Some(-48.0));
        let (loud, _) = combine(0, Some(-9.0));
        assert!(quiet < loud);
        assert!(quiet_reason.contains("quiet"));
        assert!(quiet_reason.contains("scene cut"));
    }

    #[test]
    fn cut_density_raises_the_score() {
        let (few, _) = combine(0, Some(-30.0));
        let (many, _) = combine(4, Some(-30.0));
        assert!(many > few);
        assert!(many <= 1.0);
    }

    #[test]
    fn missing_audio_falls_back_to_cuts_only() {
        let (score, reason) = combine(3, None);
        assert_eq!(score, 1.0);
        assert!(reason.contains("no audio stream"));
    }

    #[test]
    fn rejects_bad_window() {
        let err = score_highlights(Path::new("nope.mp4"), 0.0).unwrap_err();
        assert_eq!(err.code, crate::error::ErrorCode::InvalidInput);
    }

    #[test]
    fn rounds_scores_to_three_places() {
        assert_eq!(round3f(0.12345), 0.123);
    }
}
