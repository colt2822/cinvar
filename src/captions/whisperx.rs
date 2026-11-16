//! WhisperX engine adapter: word-level timings (from wav2vec2 forced alignment) for karaoke
//! captions, plus phrase search over the word list. The transcription itself runs in a Python
//! environment named by `CINVAR_WHISPERX_PYTHON` (or the path `cinvar setup --pro-ai`
//! recorded in the config); nothing is downloaded unless
//! `CINVAR_WHISPERX_ALLOW_DOWNLOAD=1`. Speaker diarization is not used (needs a Hugging Face token).

use std::path::Path;

use serde::{Deserialize, Serialize};

use super::to_subtitles;
use crate::error::{Error, ErrorCode, Result};
use crate::project::model::Cue;
use crate::util::proc::{self, ProcOpts};
use crate::util::time::Time;

const WX_HELPER: &str = include_str!("whisperx_helper.py");
const TRANSCRIPT_FILE: &str = "transcript.words.json";

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WordStamp {
    pub word: String,
    pub start: f64,
    pub end: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SegmentStamp {
    pub text: String,
    pub start: f64,
    pub end: f64,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Transcript {
    #[serde(default)]
    pub words: Vec<WordStamp>,
    #[serde(default)]
    pub segments: Vec<SegmentStamp>,
}

/// A phrase located in the word list. `first`/`last` are inclusive indices into the word list.
#[derive(Debug, Clone, PartialEq)]
pub struct PhraseMatch {
    pub first: usize,
    pub last: usize,
    pub start: f64,
    pub end: f64,
}

/// Parse and validate a transcript JSON document written by the helper.
pub fn parse_transcript(json: &str) -> Result<Transcript> {
    let t: Transcript = serde_json::from_str(json)
        .map_err(|e| Error::invalid(format!("unreadable whisperx transcript: {e}")))?;
    validate_transcript(&t)?;
    Ok(t)
}

/// Words must have finite, non-negative-length spans and non-decreasing start times.
pub fn validate_transcript(t: &Transcript) -> Result<()> {
    for w in &t.words {
        if !(w.start.is_finite() && w.end.is_finite() && w.start >= 0.0 && w.end >= w.start) {
            return Err(Error::invalid(format!(
                "whisperx word '{}' has an invalid span {:.3}-{:.3}",
                w.word, w.start, w.end
            )));
        }
    }
    for pair in t.words.windows(2) {
        if pair[1].start + 1e-3 < pair[0].start {
            return Err(Error::invalid(format!(
                "whisperx word timings are not monotonic at '{}' ({:.3} after {:.3})",
                pair[1].word, pair[1].start, pair[0].start
            )));
        }
    }
    Ok(())
}

/// Lower-case token with punctuation removed (keeps apostrophes so "don't" stays one token).
fn normalise(token: &str) -> String {
    token
        .chars()
        .filter(|c| c.is_alphanumeric() || *c == '\'')
        .collect::<String>()
        .to_lowercase()
}

/// Find every occurrence of `phrase` in the word list, ignoring case and punctuation. Tokens
/// with no letters or digits (a stray dash) are skipped rather than breaking the match.
pub fn find_phrase(words: &[WordStamp], phrase: &str) -> Vec<PhraseMatch> {
    let tokens: Vec<String> = phrase
        .split_whitespace()
        .map(normalise)
        .filter(|t| !t.is_empty())
        .collect();
    let indexed: Vec<(usize, String)> = words
        .iter()
        .enumerate()
        .map(|(i, w)| (i, normalise(&w.word)))
        .filter(|(_, n)| !n.is_empty())
        .collect();
    let mut out = Vec::new();
    if tokens.is_empty() || indexed.len() < tokens.len() {
        return out;
    }
    for s in 0..=indexed.len() - tokens.len() {
        if tokens
            .iter()
            .enumerate()
            .all(|(k, t)| &indexed[s + k].1 == t)
        {
            let first = indexed[s].0;
            let last = indexed[s + tokens.len() - 1].0;
            out.push(PhraseMatch {
                first,
                last,
                start: words[first].start,
                end: words[last].end,
            });
        }
    }
    out
}

/// SRT text from whisperx segments (the plain-caption path, identical in shape to other engines).
pub fn srt_from_segments(segments: &[SegmentStamp]) -> String {
    let cues: Vec<Cue> = segments
        .iter()
        .filter(|s| s.end > s.start && !s.text.trim().is_empty())
        .map(|s| Cue {
            start: Time::from_secs_f64(s.start),
            end: Time::from_secs_f64(s.end),
            text: s.text.trim().to_string(),
        })
        .collect();
    to_subtitles(&cues, false)
}

/// Run the WhisperX helper on `wav` with the Python interpreter `py`. Writes
/// `transcript.words.json` into `work_dir` (kept as a sidecar for word-highlight captions).
pub fn run_whisperx(
    py: &Path,
    wav: &Path,
    work_dir: &Path,
    language: Option<&str>,
) -> Result<Transcript> {
    run_whisperx_with_vocab(py, wav, work_dir, language, &[])
}

/// As `run_whisperx`, with a custom vocabulary passed to the decoder as its initial prompt.
/// An empty vocabulary adds no argument, so the helper runs exactly as before.
pub fn run_whisperx_with_vocab(
    py: &Path,
    wav: &Path,
    work_dir: &Path,
    language: Option<&str>,
    vocab: &[String],
) -> Result<Transcript> {
    std::fs::create_dir_all(work_dir)
        .map_err(|e| Error::io(format!("cannot create {}: {e}", work_dir.display())))?;
    let script = work_dir.join("whisperx_helper.py");
    std::fs::write(&script, WX_HELPER)?;
    let out_json = work_dir.join(TRANSCRIPT_FILE);
    let _ = std::fs::remove_file(&out_json);
    let mut args = vec![
        "-I".to_string(),
        script.to_string_lossy().to_string(),
        wav.to_string_lossy().to_string(),
        out_json.to_string_lossy().to_string(),
        language.unwrap_or("").to_string(),
    ];
    if let Some(prompt) = super::vocab::prompt_for(vocab) {
        args.push(prompt);
    }
    let mut opts = ProcOpts::default().with_timeout(3600);
    for key in [
        "CINVAR_WHISPERX_MODEL",
        "CINVAR_WHISPERX_CACHE",
        "CINVAR_WHISPERX_ALLOW_DOWNLOAD",
        "CINVAR_WHISPERX_DEVICE",
        "CUDA_VISIBLE_DEVICES",
        "HF_HOME",
        "TORCH_HOME",
    ] {
        // The exported variable wins; otherwise the value cinvar setup --pro-ai recorded in the
        // config is forwarded, so an installed helper runs without the customer exporting it.
        if let Some(v) = crate::util::paths::helper_value(key) {
            opts.env.push((key.to_string(), v));
        }
    }
    let out = proc::run(py, &args, &opts)?;
    if !out.success {
        return Err(Error::new(
            ErrorCode::DependencyMissing,
            format!(
                "the whisperx engine failed: {}",
                crate::secrets::redact(&out.tail_stderr(6))
            ),
        ));
    }
    let text = std::fs::read_to_string(&out_json).map_err(|_| {
        Error::new(
            ErrorCode::Render,
            "the whisperx engine finished but wrote no transcript".to_string(),
        )
    })?;
    parse_transcript(&text)
}

/// Load the word sidecar written by a previous `run_whisperx` in `work_dir`.
pub fn load_words(work_dir: &Path) -> Result<Vec<WordStamp>> {
    let path = work_dir.join(TRANSCRIPT_FILE);
    let text = std::fs::read_to_string(&path).map_err(|_| {
        Error::invalid(format!(
            "no whisperx word timings at {}; re-run the transcription",
            path.display()
        ))
    })?;
    Ok(parse_transcript(&text)?.words)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn w(word: &str, start: f64, end: f64) -> WordStamp {
        WordStamp {
            word: word.into(),
            start,
            end,
        }
    }

    #[test]
    fn phrase_search_maps_to_word_indices_and_times() {
        let words = vec![
            w("The", 0.0, 0.2),
            w("quick,", 0.2, 0.6),
            w("brown", 0.6, 1.0),
            w("fox!", 1.0, 1.4),
            w("The", 1.6, 1.8),
            w("lazy", 1.8, 2.2),
            w("dog.", 2.2, 2.6),
        ];
        let hits = find_phrase(&words, "quick brown fox");
        assert_eq!(hits.len(), 1);
        assert_eq!((hits[0].first, hits[0].last), (1, 3));
        assert!((hits[0].start - 0.2).abs() < 1e-9);
        assert!((hits[0].end - 1.4).abs() < 1e-9);

        // Case and punctuation are ignored; repeated phrases are all reported.
        let the = find_phrase(&words, "THE");
        assert_eq!(the.iter().map(|m| m.first).collect::<Vec<_>>(), vec![0, 4]);
        assert!(find_phrase(&words, "quick fox").is_empty(), "adjacency is required");
        assert!(find_phrase(&words, "   ").is_empty());
    }

    #[test]
    fn phrase_search_skips_tokens_without_letters() {
        let words = vec![w("the", 0.0, 0.1), w("—", 0.1, 0.2), w("lazy", 0.2, 0.5)];
        let hits = find_phrase(&words, "the lazy");
        assert_eq!(hits.len(), 1);
        assert_eq!((hits[0].first, hits[0].last), (0, 2));
    }

    #[test]
    fn transcript_validation_rejects_bad_timings() {
        let ok = r#"{"words":[{"word":"a","start":0.0,"end":0.5},{"word":"b","start":0.5,"end":0.9}],"segments":[]}"#;
        assert_eq!(parse_transcript(ok).unwrap().words.len(), 2);
        let reversed = r#"{"words":[{"word":"a","start":1.0,"end":0.5}],"segments":[]}"#;
        assert!(parse_transcript(reversed).is_err());
        let unordered = r#"{"words":[{"word":"a","start":2.0,"end":2.5},{"word":"b","start":1.0,"end":1.5}],"segments":[]}"#;
        assert!(parse_transcript(unordered).is_err());
        assert!(parse_transcript("not json").is_err());
    }

    #[test]
    fn segments_become_srt() {
        let segs = vec![SegmentStamp {
            text: " Hello there ".into(),
            start: 1.0,
            end: 2.5,
        }];
        let srt = srt_from_segments(&segs);
        assert!(srt.contains("00:00:01,000 --> 00:00:02,500"), "{srt}");
        assert!(srt.contains("Hello there"));
    }

    /// Real run: needs CINVAR_WHISPERX_PYTHON, CINVAR_E2E_WAV (speech), CINVAR_E2E_VIDEO (same
    /// length, video), CINVAR_E2E_PHRASE (exact words spoken), CINVAR_E2E_DURATION (seconds),
    /// CINVAR_E2E_OUT (directory). Run with `cargo test -- --ignored e2e_whisperx`.
    #[test]
    #[ignore = "needs a WhisperX environment and CINVAR_E2E_* inputs"]
    fn e2e_whisperx_words_and_burned_ass() {
        use crate::captions::ass::{burn_subtitle_file, generate_ass, AssLine, AssStyle, AssWord, Layout};
        let env = |k: &str| std::env::var(k).unwrap_or_else(|_| panic!("{k} is not set"));
        let py = std::path::PathBuf::from(env("CINVAR_WHISPERX_PYTHON"));
        let wav = std::path::PathBuf::from(env("CINVAR_E2E_WAV"));
        let video = std::path::PathBuf::from(env("CINVAR_E2E_VIDEO"));
        let phrase = env("CINVAR_E2E_PHRASE");
        let duration: f64 = env("CINVAR_E2E_DURATION").parse().expect("duration");
        let out = std::path::PathBuf::from(env("CINVAR_E2E_OUT"));
        std::fs::create_dir_all(&out).unwrap();

        let t = run_whisperx(&py, &wav, &out.join("work"), Some("en")).expect("whisperx run");
        let expected: Vec<&str> = phrase.split_whitespace().collect();
        assert_eq!(t.words.len(), expected.len(), "word count: {:?}", t.words);
        for (got, want) in t.words.iter().zip(&expected) {
            assert_eq!(normalise(&got.word), normalise(want));
        }
        assert!(t.words.iter().all(|w| w.end <= duration + 0.05), "timings exceed duration");
        assert!(find_phrase(&t.words, &phrase).len() == 1);

        let cues: Vec<Cue> = t
            .segments
            .iter()
            .map(|s| Cue {
                start: Time::from_secs_f64(s.start),
                end: Time::from_secs_f64(s.end),
                text: s.text.clone(),
            })
            .collect();
        std::fs::write(out.join("baseline.srt"), to_subtitles(&cues, false)).unwrap();
        let lines: Vec<AssLine> = t
            .segments
            .iter()
            .map(|s| AssLine {
                start: s.start,
                end: s.end,
                text: s.text.clone(),
                words: t
                    .words
                    .iter()
                    .filter(|w| {
                        let mid = (w.start + w.end) / 2.0;
                        mid >= s.start && mid < s.end
                    })
                    .map(|w| AssWord {
                        text: w.word.clone(),
                        start: w.start,
                        end: w.end,
                    })
                    .collect(),
            })
            .collect();
        let (w, h) = (1280, 720);
        let ass = generate_ass(&lines, w, h, Layout::for_canvas(w, h), &AssStyle::default()).unwrap();
        std::fs::write(out.join("karaoke.ass"), ass).unwrap();
        burn_subtitle_file(&video, &out.join("baseline.srt"), &out.join("baseline_srt.mp4")).unwrap();
        burn_subtitle_file(&video, &out.join("karaoke.ass"), &out.join("karaoke_ass.mp4")).unwrap();
        println!("WORDS {}", serde_json::to_string(&t.words).unwrap());
    }
}
