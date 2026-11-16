//! Long video -> N finished vertical Shorts.
//!
//! Pipeline (each stage is an existing CINVAR engine, composed here):
//! transcript (WhisperX word timings) -> highlight scoring -> transcript-aware segment
//! selection -> pacing/silence trim -> subject-tracking 9:16 reframe -> ASS word-highlight
//! captions -> vertical render -> output QA.
//!
//! generate is the single entry point used by the CLI (cinvar shorts), the MCP tool
//! cinvar.shorts.generate and the Studio UI.
//!
//! Stages and the engines they compose:
//!
//! | stage      | engine                                                |
//! |------------|-------------------------------------------------------|
//! | transcript | captions::whisperx (word timings) or captions::run_engine |
//! | scoring    | shorts::selection (see its module doc for the formula) |
//! | pacing     | analysis::autoeditor keep-spans, ffmpeg silencedetect fallback |
//! | reframe    | reframe::reframe (subject tracking) or a centre cover-crop |
//! | captions   | captions::ass karaoke ASS, burned with libass           |
//! | delivery   | render::render on a 1080x1920 scratch timeline          |
//! | QA         | qa::probe measurements                                 |
//!
//! The same input and options always produce the same selection and the same file names:
//! every stage is deterministic, and the selection is a ranking over a fixed candidate set.
//!
//! generate_with_diagnostics reports which engine actually ran (transcript engine, whether the
//! word times came from forced alignment, the pacing measurement, the reframe path per Short),
//! so a caller can tell a tracked reframe from a centre crop and an aligned transcript from an
//! interpolated one without reading this source.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use crate::util::time::Time;

mod captions;
mod quality;
mod selection;
mod track;
mod transcript;
mod trim;
mod vertical;

use vertical::{OUT_HEIGHT, OUT_WIDTH};

/// A request to build Shorts from one long source file.
#[derive(Debug, Clone)]
pub struct ShortsRequest {
    pub input: PathBuf,
    /// How many Shorts to produce.
    pub count: u32,
    /// Inclusive bounds for each Short's duration, in seconds.
    pub min_duration: f64,
    pub max_duration: f64,
    /// Directory for the rendered files (default: <project>/shorts).
    pub output_dir: Option<PathBuf>,
    /// Custom vocabulary forwarded to the transcription engine's prompt/hotword list.
    pub vocab: Vec<String>,
}

/// One finished Short.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Short {
    pub index: usize,
    pub path: PathBuf,
    /// Source range this Short was cut from, in seconds.
    pub start: f64,
    pub end: f64,
    /// Highlight score the selector assigned (higher is better).
    pub score: f64,
    pub width: u32,
    pub height: u32,
    /// Automated output QA verdict for this file.
    pub qa_passed: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ShortsReport {
    pub source: PathBuf,
    pub shorts: Vec<Short>,
    pub elapsed_ms: u64,
}

/// What actually ran for one generation: which transcription engine answered (or why none
/// did), whether the word times came from forced alignment, which pacing measurement trimmed
/// the windows, and how each Short was reframed and captioned. The report above carries only
/// the deliverables; this carries the honesty about how they were produced.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ShortsDiagnostics {
    /// Engine that produced the transcript, or a note that none was configured.
    pub transcript_engine: String,
    /// True when word times were interpolated inside phrase cues instead of aligned.
    pub words_estimated: bool,
    pub word_count: usize,
    /// Silence measurement used for pacing ("auto-editor keep plan", "ffmpeg silencedetect ...").
    pub pacing_source: String,
    /// Per Short: the reframe path that produced the vertical master.
    pub reframes: Vec<String>,
    /// Per Short: whether a karaoke caption track was burned in.
    pub captions_burned: Vec<bool>,
    /// Per Short: the QA measurement detail.
    pub qa_detail: Vec<String>,
}

/// Parse a --duration range such as 30-60 or 45.
pub fn parse_duration_range(raw: &str) -> Result<(f64, f64)> {
    let raw = raw.trim();
    let (lo, hi) = match raw.split_once('-') {
        Some((a, b)) => (a.trim(), b.trim()),
        None => (raw, raw),
    };
    let parse = |s: &str| -> Result<f64> {
        s.parse::<f64>()
            .map_err(|_| Error::invalid(format!("--duration must be seconds or MIN-MAX, got {raw:?}")))
    };
    let (lo, hi) = (parse(lo)?, parse(hi)?);
    if !(lo.is_finite() && hi.is_finite()) || lo <= 0.0 || hi <= 0.0 {
        return Err(Error::invalid("--duration bounds must be positive seconds"));
    }
    if lo > hi {
        return Err(Error::invalid(format!("--duration low bound {lo} exceeds high bound {hi}")));
    }
    Ok((lo, hi))
}

/// Convenience: is path a usable source for Shorts generation?
pub fn source_usable(path: &Path) -> bool {
    path.is_file() && path.metadata().map(|m| m.len() > 0).unwrap_or(false)
}

/// Where Shorts land when the request names no directory: <source folder>/shorts.
fn default_output_dir(input: &Path) -> PathBuf {
    let parent = input
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| PathBuf::from("."));
    parent.join("shorts")
}

fn file_name_for(index: usize) -> String {
    format!("short_{index:02}.mp4")
}

/// Build count Shorts from req.input.
pub fn generate(req: &ShortsRequest) -> Result<ShortsReport> {
    generate_with_diagnostics(req).map(|(report, _)| report)
}

/// As generate, also returning what ran (see ShortsDiagnostics).
pub fn generate_with_diagnostics(req: &ShortsRequest) -> Result<(ShortsReport, ShortsDiagnostics)> {
    let started = std::time::Instant::now();
    if req.count == 0 {
        return Err(Error::invalid("--count must be at least 1"));
    }
    if !source_usable(&req.input) {
        return Err(Error::invalid(format!(
            "input not found or empty: {}",
            req.input.display()
        )));
    }
    if !(req.min_duration.is_finite() && req.min_duration > 0.0) {
        return Err(Error::invalid("min_duration must be a positive number of seconds"));
    }
    if !req.max_duration.is_finite() || req.max_duration < req.min_duration {
        return Err(Error::invalid(format!(
            "max_duration {} must be at least min_duration {}",
            req.max_duration, req.min_duration
        )));
    }

    let info = crate::qa::probe::probe(&req.input)?;
    if !info.has_video {
        return Err(Error::invalid(format!(
            "{} carries no video stream, so it cannot be cut into Shorts",
            req.input.display()
        )));
    }
    if !info.has_audio {
        return Err(Error::invalid(format!(
            "{} carries no audio stream; Shorts are selected from speech and measured audio energy, so there is nothing to score",
            req.input.display()
        )));
    }
    let total = info.duration.as_secs_f64();
    if total < req.min_duration {
        return Err(Error::invalid(format!(
            "the source is {total:.2}s long, shorter than the requested minimum of {:.2}s",
            req.min_duration
        )));
    }

    let output_dir = req
        .output_dir
        .clone()
        .unwrap_or_else(|| default_output_dir(&req.input));
    std::fs::create_dir_all(&output_dir)
        .map_err(|e| Error::io(format!("cannot create {}: {e}", output_dir.display())))?;
    let work = output_dir.join(".shorts-work");
    let _ = std::fs::remove_dir_all(&work);
    std::fs::create_dir_all(&work)
        .map_err(|e| Error::io(format!("cannot create {}: {e}", work.display())))?;

    // 1. Speech. Empty words are allowed (no engine, or an instrumental source): the
    // selection then runs on measured audio energy instead of fabricating a transcript.
    let speech = transcript::transcribe(&req.input, &work, &req.vocab)?;

    // 2. Pacing: the silence the trim pass may drop.
    let (gaps, pacing_source) = trim::measure(&req.input, total, &work);

    // 3. Score and select. Energy is measured per candidate window, over that window only.
    let energy_at = |start: f64, end: f64| -> Option<f64> {
        if !info.has_audio || end <= start {
            return None;
        }
        crate::qa::probe::volume_stats(
            &req.input,
            Some(Time::from_secs_f64(start)),
            Some(Time::from_secs_f64(end - start)),
        )
        .ok()
        .and_then(|stats| stats.mean_db)
    };
    let candidates = if speech.words.is_empty() {
        selection::grid_candidates(total, req.min_duration, req.max_duration, &energy_at)
    } else {
        selection::candidates(
            &speech.words,
            total,
            req.min_duration,
            req.max_duration,
            &energy_at,
        )
    };
    if candidates.is_empty() {
        return Err(Error::invalid(format!(
            "no candidate window of {:.1}-{:.1}s could be built from {}",
            req.min_duration,
            req.max_duration,
            req.input.display()
        )));
    }
    let chosen = selection::choose(&candidates, req.count as usize);
    if chosen.is_empty() {
        return Err(Error::invalid("the selector found no non-overlapping windows"));
    }

    // 4. Render each Short.
    let mut diagnostics = ShortsDiagnostics {
        transcript_engine: speech.engine.clone(),
        words_estimated: speech.estimated_words,
        word_count: speech.words.len(),
        pacing_source: pacing_source.clone(),
        reframes: Vec::with_capacity(chosen.len()),
        captions_burned: Vec::with_capacity(chosen.len()),
        qa_detail: Vec::with_capacity(chosen.len()),
    };
    let mut shorts: Vec<Short> = Vec::with_capacity(chosen.len());
    for (position, candidate) in chosen.iter().enumerate() {
        let index = position + 1;
        let (start, end) = trim::trim_window(
            candidate.start,
            candidate.end,
            &gaps,
            req.min_duration,
        );
        let stem = format!("short_{index:02}");
        let cut = work.join(format!("{stem}.cut.mp4"));
        vertical::cut_range(&req.input, start, end, &cut)?;
        let master = work.join(format!("{stem}.vertical.mp4"));
        let mode = vertical::to_vertical(&cut, &master, &work.join("reframe"))?;
        diagnostics.reframes.push(mode.label().to_string());

        let lines = captions::lines_for_window(&speech.words, start, end);
        diagnostics.captions_burned.push(!lines.is_empty());
        captions::burn_in(&master, &lines, OUT_WIDTH, OUT_HEIGHT)?;

        let out = output_dir.join(file_name_for(index));
        let _ = std::fs::remove_file(&out);
        track::render_vertical(&master, &out, &work.join(format!("{stem}.render")))?;

        let verdict = quality::check(
            &out,
            end - start,
            req.min_duration,
            req.max_duration,
            info.has_audio,
        );
        diagnostics.qa_detail.push(verdict.detail.clone());
        shorts.push(Short {
            index,
            path: out,
            start: (start * 1000.0).round() / 1000.0,
            end: (end * 1000.0).round() / 1000.0,
            score: candidate.score,
            width: verdict.width,
            height: verdict.height,
            qa_passed: verdict.passed,
        });
    }

    // Intermediate artefacts (the cut, the vertical master, the karaoke ASS) are scratch and
    // are removed on success. CINVAR_SHORTS_KEEP_WORK=1 keeps them for inspection, which is
    // how a caption or reframe problem is diagnosed after the fact.
    if std::env::var("CINVAR_SHORTS_KEEP_WORK").as_deref() != Ok("1") {
        let _ = std::fs::remove_dir_all(&work);
    }
    Ok((
        ShortsReport {
            source: req.input.clone(),
            shorts,
            elapsed_ms: started.elapsed().as_millis() as u64,
        },
        diagnostics,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn duration_ranges_parse_both_shapes() {
        assert_eq!(parse_duration_range("30-60").unwrap(), (30.0, 60.0));
        assert_eq!(parse_duration_range("45").unwrap(), (45.0, 45.0));
        assert_eq!(parse_duration_range(" 12.5 - 20 ").unwrap(), (12.5, 20.0));
        assert!(parse_duration_range("60-30").is_err());
        assert!(parse_duration_range("0-30").is_err());
        assert!(parse_duration_range("abc").is_err());
    }

    #[test]
    fn bad_requests_are_refused_before_any_work() {
        let req = ShortsRequest {
            input: PathBuf::from("/nonexistent.mp4"),
            count: 3,
            min_duration: 10.0,
            max_duration: 30.0,
            output_dir: None,
            vocab: Vec::new(),
        };
        let err = generate(&req).unwrap_err();
        assert_eq!(err.code, crate::error::ErrorCode::InvalidInput);

        let empty = ShortsRequest {
            input: PathBuf::from("/nonexistent.mp4"),
            count: 0,
            min_duration: 10.0,
            max_duration: 30.0,
            output_dir: None,
            vocab: Vec::new(),
        };
        assert_eq!(generate(&empty).unwrap_err().code, crate::error::ErrorCode::InvalidInput);
    }

    #[test]
    fn default_output_dir_sits_next_to_the_source() {
        assert_eq!(
            default_output_dir(Path::new("/videos/talk.mp4")),
            PathBuf::from("/videos/shorts")
        );
        assert_eq!(default_output_dir(Path::new("talk.mp4")), PathBuf::from("./shorts"));
    }

    #[test]
    fn file_names_are_zero_padded() {
        assert_eq!(file_name_for(1), "short_01.mp4");
        assert_eq!(file_name_for(12), "short_12.mp4");
    }
}
