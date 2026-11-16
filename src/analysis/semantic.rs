//! Semantic scene intelligence: segment footage, describe segments with a
//! local vision model (Ollama), extract keywords, and build a searchable
//! index. Falls back to deterministic descriptors (scene cuts, audio level)
//! when no vision model is reachable — those carry `source: "deterministic"`
//! and are never presented as model understanding.
//!
//! Everything runs locally; nothing leaves the machine.

use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::error::{Error, ErrorCode, Result};
use crate::util::proc::{self, ProcOpts};

const DEFAULT_OLLAMA_URL: &str = "http://localhost:11434";
const DEFAULT_MODEL: &str = "moondream";
const MIN_SEG_SECS: f64 = 2.0;
const MAX_SEG_SECS: f64 = 8.0;
const FRAME_WIDTH: u32 = 512;
const VISION_TIMEOUT_SECS: u64 = 60;
/// Words too common to be useful keywords.
const STOPWORDS: &[&str] = &[
    "a", "an", "the", "and", "or", "but", "in", "on", "at", "to", "of", "with", "for", "from",
    "is", "are", "was", "were", "be", "been", "being", "it", "its", "this", "that", "these",
    "those", "there", "here", "as", "by", "into", "over", "under", "about", "than", "then",
    "so", "such", "not", "no", "can", "could", "will", "would", "should", "may", "might",
    "one", "two", "some", "any", "each", "very", "also", "just", "have", "has", "had", "do",
    "does", "did", "you", "they", "he", "she", "we", "his", "her", "their", "our", "my",
    "video", "frame", "scene", "shot", "segment", "image", "picture", "shows", "showing",
    "shown", "appears", "looks", "like", "which", "what", "who", "whom", "whose", "while",
    "during", "through", "between", "both", "all", "more", "most", "other", "another",
    "because", "if", "when", "where", "how", "out", "up", "down", "back", "around",
];

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SceneSegment {
    pub id: String,
    /// Source file path as indexed (outside the project; provenance).
    pub file: String,
    pub file_name: String,
    pub start: f64,
    pub end: f64,
    /// Vision-model description, or a deterministic descriptor when no model ran.
    pub description: String,
    pub keywords: Vec<String>,
    pub subjects: Vec<String>,
    /// Mean audio level in dBFS over the segment, when measurable.
    pub audio_db: Option<f64>,
    /// Scene cuts per minute inside the segment.
    pub cut_density: f32,
    /// 0..1 heuristic interest (audio energy + cut density), same family as highlight scores.
    pub activity: f32,
    /// "vision:<model>" when a real model produced the description, "deterministic" otherwise.
    pub source: String,
    /// 0..1; deterministic descriptors carry lower confidence by definition.
    pub confidence: f32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SceneIndex {
    pub model: String,
    pub vision_available: bool,
    pub segments: Vec<SceneSegment>,
}

impl SceneIndex {
    pub fn load(path: &Path) -> Result<SceneIndex> {
        let raw = std::fs::read_to_string(path)
            .map_err(|e| Error::not_found(format!("no scene index at {}: {e}", path.display())))?;
        serde_json::from_str(&raw)
            .map_err(|e| Error::invalid(format!("corrupt scene index at {}: {e}", path.display())))
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| Error::new(ErrorCode::Internal, format!("cannot create {}: {e}", parent.display())))?;
        }
        std::fs::write(path, serde_json::to_vec(self).map_err(|e| Error::internal(e.to_string()))?)
            .map_err(|e| Error::internal(format!("cannot write {}: {e}", path.display())))
    }
}

fn extract_frame(path: &Path, at_secs: f64, out: &Path) -> Result<()> {
    let bin = crate::qa::probe::ffmpeg()?;
    let args = vec![
        "-hide_banner".to_string(),
        "-nostdin".to_string(),
        "-loglevel".to_string(),
        "error".to_string(),
        "-ss".to_string(),
        format!("{at_secs:.3}"),
        "-i".to_string(),
        path.to_string_lossy().to_string(),
        "-frames:v".to_string(),
        "1".to_string(),
        "-vf".to_string(),
        format!("scale={FRAME_WIDTH}:-2"),
        "-q:v".to_string(),
        "5".to_string(),
        "-y".to_string(),
        out.to_string_lossy().to_string(),
    ];
    let outp = proc::run(&bin, &args, &ProcOpts::default().with_timeout(60))?;
    if !outp.success || !out.exists() {
        return Err(Error::new(
            ErrorCode::Render,
            format!("frame extraction failed: {}", crate::secrets::redact(&outp.tail_stderr(2))),
        ));
    }
    Ok(())
}

fn segment_audio_db(path: &Path, start: f64, end: f64) -> Option<f64> {
    let bin = crate::qa::probe::ffmpeg().ok()?;
    let args = vec![
        "-hide_banner".to_string(),
        "-nostdin".to_string(),
        "-ss".to_string(),
        format!("{start:.3}"),
        "-t".to_string(),
        format!("{:.3}", (end - start).max(0.5)),
        "-i".to_string(),
        path.to_string_lossy().to_string(),
        "-af".to_string(),
        "volumedetect".to_string(),
        "-f".to_string(),
        "null".to_string(),
        "-".to_string(),
    ];
    let outp = proc::run(&bin, &args, &ProcOpts::default().with_timeout(60)).ok()?;
    crate::qa::probe::parse_volume_stats(&outp.stderr)
        .ok()
        .and_then(|s| s.mean_db)
}

/// Split [start, end) on scene cuts into MIN_SEG..MAX_SEG windows.
fn segment_bounds(cuts: &[f64], duration: f64) -> Vec<(f64, f64)> {
    let mut bounds: Vec<f64> = vec![0.0];
    bounds.extend(cuts.iter().copied().filter(|c| *c > 0.0 && *c < duration));
    bounds.push(duration);
    let mut segs = Vec::new();
    let mut open: Option<f64> = None;
    for (i, &b) in bounds.iter().enumerate() {
        let last = i + 1 == bounds.len();
        match open {
            None => open = Some(b),
            Some(start) => {
                let span = b - start;
                if span >= MAX_SEG_SECS || last {
                    // Close at the cut (or extend to duration if still too short).
                    let end = if span < MIN_SEG_SECS && last { b } else { b };
                    let _ = end;
                    segs.push((start, b));
                    open = None;
                }
            }
        }
        if last {
            if let Some(start) = open.take() {
                if b - start >= 0.5 {
                    segs.push((start, b));
                }
            }
        }
    }
    // Merge tails shorter than MIN_SEG into the previous segment.
    let mut merged: Vec<(f64, f64)> = Vec::new();
    for seg in segs {
        let short = seg.1 - seg.0 < MIN_SEG_SECS;
        if short && !merged.is_empty() {
            let prev = merged.pop().unwrap();
            merged.push((prev.0, seg.1));
        } else {
            merged.push(seg);
        }
    }
    merged
}

/// Additive seam for PySceneDetect output: each detected scene becomes one (start, end) window in
/// the same shape `segment_bounds` produces. Scenes shorter than MIN_SEG_SECS fold into the
/// previous window, matching how the indexer treats short tails.
pub fn windows_from_scenes(
    scenes: &[crate::analysis::pyscenedetect::Scene],
) -> Vec<(f64, f64)> {
    let mut windows: Vec<(f64, f64)> = Vec::new();
    for scene in scenes.iter().filter(|s| s.end_sec > s.start_sec) {
        let (start, end) = (scene.start_sec, scene.end_sec);
        if end - start < MIN_SEG_SECS {
            if let Some(prev) = windows.last_mut() {
                prev.1 = end;
                continue;
            }
        }
        windows.push((start, end));
    }
    windows
}

pub fn keywords_from(text: &str) -> Vec<String> {
    let mut seen = std::collections::BTreeSet::new();
    for word in text
        .to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| w.len() > 2 && !STOPWORDS.contains(w))
    {
        seen.insert(word.to_string());
    }
    seen.into_iter().take(12).collect()
}

/// One call to a local Ollama vision model. Returns (description, subjects).
fn vision_describe(base_url: &str, model: &str, frame: &Path) -> Result<(String, Vec<String>)> {
    let bytes = std::fs::read(frame)
        .map_err(|e| Error::internal(format!("cannot read frame {}: {e}", frame.display())))?;
    let json = serde_json::json!({
        "model": model,
        "prompt": "Describe what this video frame shows in one factual sentence, then on a second line list notable subjects or objects separated by commas. Only describe what is visible.",
        "images": [base64_encode(&bytes)],
        "stream": false,
    });
    let response = ureq::post(&format!("{base_url}/api/generate"))
        .timeout(std::time::Duration::from_secs(VISION_TIMEOUT_SECS))
        .send_json(json)
        .map_err(|e| Error::new(ErrorCode::Internal, format!("ollama call failed: {e}")))?;
    let value: serde_json::Value = response
        .into_json()
        .map_err(|e| Error::internal(format!("ollama response unreadable: {e}")))?;
    let text = value
        .get("response")
        .and_then(|r| r.as_str())
        .unwrap_or("")
        .trim()
        .to_string();
    if text.is_empty() {
        return Err(Error::internal("ollama returned an empty description"));
    }
    let mut lines = text.lines().map(|l| l.trim()).filter(|l| !l.is_empty());
    let description = lines.next().unwrap_or("").to_string();
    let subjects = lines
        .next()
        .map(|l| {
            l.trim_start_matches(['-', '*', ' '])
                .split(',')
                .map(|s| s.trim().to_lowercase())
                .filter(|s| !s.is_empty())
                .take(8)
                .collect()
        })
        .unwrap_or_default();
    Ok((description, subjects))
}

fn base64_encode(bytes: &[u8]) -> String {
    const TABLE: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity((bytes.len() + 2) / 3 * 4);
    for chunk in bytes.chunks(3) {
        let b = [chunk[0], *chunk.get(1).unwrap_or(&0), *chunk.get(2).unwrap_or(&0)];
        let n = ((b[0] as u32) << 16) | ((b[1] as u32) << 8) | b[2] as u32;
        out.push(TABLE[(n >> 18) as usize & 63] as char);
        out.push(TABLE[(n >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 { TABLE[(n >> 6) as usize & 63] as char } else { '=' });
        out.push(if chunk.len() > 2 { TABLE[n as usize & 63] as char } else { '=' });
    }
    out
}

fn ollama_reachable(base_url: &str, model: &str) -> bool {
    let Ok(resp) = ureq::get(&format!("{base_url}/api/tags"))
        .timeout(std::time::Duration::from_secs(5))
        .call()
    else {
        return false;
    };
    let Ok(value) = resp.into_json::<serde_json::Value>() else {
        return false;
    };
    value["models"]
        .as_array()
        .map(|models| {
            models.iter().any(|m| {
                m["name"].as_str().map(|n| n.starts_with(model)).unwrap_or(false)
            })
        })
        .unwrap_or(false)
}

#[derive(Debug, Clone)]
pub struct IndexOptions {
    pub ollama_url: String,
    pub model: String,
}

impl Default for IndexOptions {
    fn default() -> Self {
        Self {
            ollama_url: DEFAULT_OLLAMA_URL.to_string(),
            model: DEFAULT_MODEL.to_string(),
        }
    }
}

/// Index every video in a folder into a searchable segment list.
pub fn index_media(media_root: &Path, opts: &IndexOptions) -> Result<SceneIndex> {
    let files = crate::analysis::autoedit::collect_video_files(media_root)?;
    if files.is_empty() {
        return Err(Error::invalid(format!(
            "no video files found in {}",
            media_root.display()
        )));
    }
    let vision = ollama_reachable(&opts.ollama_url, &opts.model);
    let mut segments: Vec<SceneSegment> = Vec::new();
    let mut scratch_dir = std::env::temp_dir().join("cinvar-semantic");
    scratch_dir.push(format!("idx-{}", std::process::id()));
    std::fs::create_dir_all(&scratch_dir)
        .map_err(|e| Error::internal(format!("cannot create scratch {}: {e}", scratch_dir.display())))?;

    for file in &files {
        let name = file.file_name().unwrap_or_default().to_string_lossy().to_string();
        let info = crate::qa::probe::probe(file)?;
        let duration = info.duration.as_secs_f64();
        let cuts: Vec<f64> = crate::media::detect_scenes(file, 0.30)
            .map(|sc| sc.iter().map(|c| c.at.as_secs_f64()).collect())
            .unwrap_or_default();
        for (seg_idx, (start, end)) in segment_bounds(&cuts, duration).into_iter().enumerate() {
            let activity = {
                let cuts_per_min = (cuts.iter().filter(|c| **c >= start && **c < end).count() as f32)
                    / (((end - start).max(0.1) / 60.0).max(0.0167) as f32);
                let cut_component = (cuts_per_min / 3.0).min(1.0);
                let audio = segment_audio_db(file, start, end);
                let audio_component = audio
                    .map(|m| (((m + 60.0) / 54.0)).clamp(0.0, 1.0) as f32)
                    .unwrap_or(0.2);
                (0.45 * cut_component + 0.55 * audio_component).clamp(0.0, 1.0)
            };
            let (description, subjects, source, confidence) = if vision {
                let frame_path = scratch_dir.join(format!("{name}-{seg_idx}.jpg"));
                match extract_frame(file, (start + end) / 2.0, &frame_path)
                    .and_then(|_| vision_describe(&opts.ollama_url, &opts.model, &frame_path))
                {
                    Ok((desc, subj)) => {
                        let _ = std::fs::remove_file(&frame_path);
                        (desc, subj, format!("vision:{}", opts.model), 0.8)
                    }
                    Err(_) => (
                        deterministic_description(activity, info.width, info.height),
                        Vec::new(),
                        "deterministic".to_string(),
                        0.3,
                    ),
                }
            } else {
                (
                    deterministic_description(activity, info.width, info.height),
                    Vec::new(),
                    "deterministic".to_string(),
                    0.3,
                )
            };
            let keywords = keywords_from(&format!("{description} {}", subjects.join(" ")));
            segments.push(SceneSegment {
                id: format!("seg_{}_{}_{}", &name[..name.len().saturating_sub(4).min(8)], seg_idx, segments.len()),
                file: file.to_string_lossy().to_string(),
                file_name: name.clone(),
                start,
                end,
                description,
                keywords,
                subjects,
                audio_db: segment_audio_db(file, start, end),
                cut_density: (cuts.iter().filter(|c| **c >= start && **c < end).count() as f32)
                    / (((end - start).max(0.1) / 60.0).max(0.0167) as f32),
                activity: (activity * 1000.0).round() / 1000.0,
                source: source.clone(),
                confidence,
            });
        }
    }
    let _ = std::fs::remove_dir_all(&scratch_dir);
    Ok(SceneIndex {
        model: if vision { opts.model.clone() } else { "none".to_string() },
        vision_available: vision,
        segments,
    })
}

fn deterministic_description(activity: f32, width: u32, height: u32) -> String {
    let level = if activity >= 0.6 {
        "high-activity"
    } else if activity >= 0.3 {
        "moderate-activity"
    } else {
        "low-activity"
    };
    format!(
        "No vision model available: deterministic descriptor only. {level} segment ({activity:.2}) from {width}x{height} source, measured by scene-cut density and audio level."
    )
}

/// Score every segment against a free-text instruction. Returns (segment, score 0..1)
/// sorted best first. Empty query returns nothing — callers fall back to activity.
pub fn search(index: &SceneIndex, query: &str) -> Vec<(usize, f32)> {
    let query_words: Vec<String> = keywords_from(query);
    if query_words.is_empty() {
        return Vec::new();
    }
    let mut scored: Vec<(usize, f32)> = index
        .segments
        .iter()
        .enumerate()
        .map(|(i, seg)| {
            let mut best = 0.0f32;
            for word in &query_words {
                // Exact keyword hits count fully; prefix hits (tense/plural) partially.
                let exact = seg.keywords.iter().any(|k| k == word);
                let partial = seg
                    .keywords
                    .iter()
                    .any(|k| k.starts_with(word.as_str()) || word.starts_with(k.as_str()));
                let word_score = if exact {
                    1.0
                } else if partial {
                    0.5
                } else {
                    0.0
                };
                best += word_score;
            }
            (i, (best / query_words.len() as f32).min(1.0))
        })
        .collect();
    scored.retain(|(_, s)| *s > 0.0);
    scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    scored
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn windows_follow_scene_boundaries() {
        use crate::analysis::pyscenedetect::Scene;
        let scene = |s: u64, a: f64, e: u64, b: f64| Scene {
            start_frame: s,
            start_sec: a,
            end_frame: e,
            end_sec: b,
        };
        let scenes = vec![
            scene(0, 0.0, 75, 3.0),
            scene(75, 3.0, 163, 6.52),
            scene(163, 6.52, 164, 6.56), // 0.04 s flash: folds into the previous window
            scene(164, 6.56, 251, 10.04),
            scene(251, 10.04, 375, 15.0),
        ];
        assert_eq!(
            windows_from_scenes(&scenes),
            vec![(0.0, 3.0), (3.0, 6.56), (6.56, 10.04), (10.04, 15.0)]
        );
        assert!(windows_from_scenes(&[]).is_empty());
    }

    #[test]
    fn bounds_split_and_merge() {
        let cuts = vec![3.0, 9.0, 9.5, 20.0];
        let segs = segment_bounds(&cuts, 25.0);
        assert!(segs.first().unwrap().0 == 0.0);
        assert!(segs.iter().all(|(s, e)| e > s));
        // The 9.0-9.5 cut produces a tiny segment that must merge away.
        assert!(!segs.iter().any(|(s, e)| e - s < 0.5));
    }

    #[test]
    fn keywords_drop_stopwords() {
        let kws = keywords_from("A man runs fast through the crowded market with a dog");
        assert!(kws.contains(&"man".to_string()));
        assert!(kws.contains(&"runs".to_string()));
        assert!(!kws.contains(&"the".to_string()));
        assert!(!kws.contains(&"with".to_string()));
    }

    #[test]
    fn search_ranks_keyword_hits() {
        let mk = |id: &str, desc: &str, kw: &[&str]| SceneSegment {
            id: id.to_string(),
            file: "f.mp4".into(),
            file_name: "f.mp4".into(),
            start: 0.0,
            end: 3.0,
            description: desc.to_string(),
            keywords: kw.iter().map(|s| s.to_string()).collect(),
            subjects: Vec::new(),
            audio_db: Some(-30.0),
            cut_density: 1.0,
            activity: 0.5,
            source: "vision:test".into(),
            confidence: 0.8,
        };
        let index = SceneIndex {
            model: "test".into(),
            vision_available: true,
            segments: vec![
                mk("a", "a man celebrates", &["man", "celebrates", "crowd"]),
                mk("b", "empty parking lot", &["parking", "lot", "cars"]),
            ],
        };
        let hits = search(&index, "someone celebrates with a crowd");
        assert_eq!(hits[0].0, 0);
        assert!(hits[0].1 > 0.0);
        let hits2 = search(&index, "parking lot");
        assert_eq!(hits2[0].0, 1);
    }

    #[test]
    fn base64_roundtrip() {
        assert_eq!(base64_encode(b"any carnal pleasure"), "YW55IGNhcm5hbCBwbGVhc3VyZQ==");
    }
}
