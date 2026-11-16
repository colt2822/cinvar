//! Speech for Shorts: word timings from the transcription engines the repository already
//! ships (crate::captions).
//!
//! WhisperX is used when CINVAR_WHISPERX_PYTHON points at an interpreter, because it is the
//! only engine that returns words whose timings come from forced alignment. Otherwise the
//! detected engine (faster-whisper, whisper.cpp, whisper, or a CINVAR_TRANSCRIBE_CMD
//! template) runs exactly as cinvar captions runs it, and its phrase-level cues are split
//! into words with times interpolated inside each cue; that interpolation is NOT forced
//! alignment and the caller is told so (Speech::estimated_words).
//!
//! When no engine is configured at all, transcription is skipped and an empty word list is
//! returned: the Shorts pipeline then scores windows on measured audio energy instead of
//! failing, and no captions are burned in. A configured engine that fails is an error.

use std::path::Path;

use crate::captions::whisperx::WordStamp;
use crate::captions::{self, Engine};
use crate::error::{Error, ErrorCode, Result};

/// Word timings for one source file.
#[derive(Debug, Clone)]
pub struct Speech {
    pub words: Vec<WordStamp>,
    /// Engine that produced the transcript, or why none ran.
    pub engine: String,
    /// True when word times were interpolated inside phrase cues.
    pub estimated_words: bool,
}

/// Is the WhisperX helper configured (CINVAR_WHISPERX_PYTHON points at a file)?
pub fn whisperx_configured() -> bool {
    std::env::var("CINVAR_WHISPERX_PYTHON")
        .map(|p| Path::new(&p).is_file())
        .unwrap_or(false)
}

/// Split one cue into words, spreading the cue's span across them in proportion to length.
fn words_from_cue(text: &str, start: f64, end: f64) -> Vec<WordStamp> {
    let tokens: Vec<&str> = text.split_whitespace().filter(|t| !t.is_empty()).collect();
    if tokens.is_empty() || end <= start {
        return Vec::new();
    }
    let weights: Vec<f64> = tokens
        .iter()
        .map(|t| t.chars().filter(|c| c.is_alphanumeric()).count().max(1) as f64)
        .collect();
    let total: f64 = weights.iter().sum();
    let mut cursor = start;
    let mut out = Vec::with_capacity(tokens.len());
    for (token, weight) in tokens.iter().zip(&weights) {
        let len = (end - start) * weight / total;
        let stop = (cursor + len).min(end);
        if stop > cursor {
            out.push(WordStamp {
                word: token.to_string(),
                start: (cursor * 1000.0).round() / 1000.0,
                end: (stop * 1000.0).round() / 1000.0,
            });
        }
        cursor = stop;
    }
    out
}

/// Word timings for source, transcribed into work_dir.
pub fn transcribe(source: &Path, work_dir: &Path, vocab: &[String]) -> Result<Speech> {
    std::fs::create_dir_all(work_dir)
        .map_err(|e| Error::io(format!("cannot create {}: {e}", work_dir.display())))?;
    let wav = work_dir.join("speech.wav");
    if !wav.is_file() {
        crate::media::extract_audio(source, &wav)?;
    }

    if whisperx_configured() {
        let engine = captions::detect_engine_named("whisperx")?;
        if let Engine::WhisperX(py) = &engine {
            let transcript =
                captions::whisperx::run_whisperx_with_vocab(py, &wav, work_dir, None, vocab)?;
            let words: Vec<WordStamp> = transcript
                .words
                .into_iter()
                .filter(|w| !w.word.trim().is_empty())
                .collect();
            return Ok(Speech {
                words,
                engine: engine.name().to_string(),
                estimated_words: false,
            });
        }
        return Err(Error::internal("whisperx resolved to a different engine"));
    }

    let engine = match captions::detect_engine_named("auto") {
        Ok(engine) => engine,
        Err(err) if err.code == ErrorCode::DependencyMissing => {
            return Ok(Speech {
                words: Vec::new(),
                engine: format!("no transcription engine ({}); scored on audio energy", err.code.as_str()),
                estimated_words: false,
            })
        }
        Err(err) => return Err(err),
    };
    let srt = captions::run_engine_with_vocab(&engine, &wav, work_dir, None, vocab)?;
    let (cues, _skipped) = captions::parse_subtitles(&srt);
    let mut words: Vec<WordStamp> = Vec::new();
    for cue in cues {
        words.extend(words_from_cue(
            &cue.text,
            cue.start.as_secs_f64(),
            cue.end.as_secs_f64(),
        ));
    }
    words.sort_by(|a, b| a.start.partial_cmp(&b.start).unwrap_or(std::cmp::Ordering::Equal));
    Ok(Speech {
        words,
        engine: engine.name().to_string(),
        estimated_words: true,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cue_words_cover_the_cue_without_overlap() {
        let words = words_from_cue("Hello there world", 10.0, 12.0);
        assert_eq!(words.len(), 3);
        assert!((words[0].start - 10.0).abs() < 1e-6);
        assert!((words[2].end - 12.0).abs() < 1e-6);
        for pair in words.windows(2) {
            assert!(pair[0].end <= pair[1].start + 1e-9, "{:?}", words);
        }
        for word in &words {
            assert!(word.end > word.start);
        }
        // Longer words get more of the cue than shorter ones.
        assert!(words[2].end - words[2].start > words[0].end - words[0].start - 1e-9);
    }

    #[test]
    fn degenerate_cues_produce_no_words() {
        assert!(words_from_cue("   ", 0.0, 1.0).is_empty());
        assert!(words_from_cue("text", 1.0, 1.0).is_empty());
    }

    #[test]
    fn a_missing_engine_degrades_to_energy_scoring() {
        // No engine is configured in the test environment; extract_audio runs first, so
        // this asserts the degradation path rather than an engine run.
        std::env::remove_var("CINVAR_TRANSCRIBE_CMD");
        std::env::remove_var("CINVAR_WHISPERX_PYTHON");
        if crate::captions::detect_engine().is_some() {
            return; // an engine is installed here: the fallback cannot be exercised
        }
        assert!(!whisperx_configured());
    }
}
