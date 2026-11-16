//! Practical caption workflows that need no mandatory dependency: SRT/VTT import, timing a script
//! against a span of audio, and an optional external transcription engine.

use std::path::{Path, PathBuf};

use crate::error::{Error, ErrorCode, Result};
use crate::project::model::Cue;
use crate::util::paths::HelperOrigin;
use crate::util::proc::{self, ProcOpts};
use crate::util::time::Time;

pub mod ass;
pub mod vocab;
pub mod whisperx;

const MAX_CAPTION_FILE: u64 = 4 * 1024 * 1024;

/// Parse an SRT or WebVTT timestamp such as `00:00:01,500`, `00:01.500` or `1.5`.
fn parse_stamp(raw: &str) -> Option<f64> {
    let cleaned = raw.trim().replace(',', ".");
    let parts: Vec<&str> = cleaned.split(':').collect();
    let nums: Option<Vec<f64>> = parts.iter().map(|p| p.parse::<f64>().ok()).collect();
    let nums = nums?;
    match nums.as_slice() {
        [s] => Some(*s),
        [m, s] => Some(m * 60.0 + s),
        [h, m, s] => Some(h * 3600.0 + m * 60.0 + s),
        _ => None,
    }
}

/// Parse SRT or WebVTT text into cues. Markup tags are stripped; invalid blocks are skipped
/// and counted so the caller can report them.
pub fn parse_subtitles(text: &str) -> (Vec<Cue>, usize) {
    // A byte-order mark survives into the text on some paths; it would break the first index.
    let text = text.trim_start_matches('\u{feff}');
    let normalised = text.replace("\r\n", "\n").replace('\r', "\n");
    let mut cues = Vec::new();
    let mut skipped = 0;
    for block in normalised.split("\n\n") {
        let lines: Vec<&str> = block
            .lines()
            .map(|l| l.trim_end())
            .filter(|l| !l.is_empty())
            .collect();
        let Some(arrow) = lines.iter().position(|l| l.contains("-->")) else {
            if lines.iter().any(|l| {
                !l.starts_with("WEBVTT") && !l.starts_with("NOTE") && l.parse::<u32>().is_err()
            }) {
                skipped += 1;
            }
            continue;
        };
        let (a, b) = lines[arrow].split_once("-->").unwrap();
        let end_token = b.split_whitespace().next().unwrap_or("");
        let (Some(start), Some(end)) = (parse_stamp(a), parse_stamp(end_token)) else {
            skipped += 1;
            continue;
        };
        let body = lines[arrow + 1..].join(" ");
        let body = strip_tags(&body);
        if end <= start || body.trim().is_empty() {
            skipped += 1;
            continue;
        }
        cues.push(Cue {
            start: Time::from_secs_f64(start),
            end: Time::from_secs_f64(end),
            text: body.trim().to_string(),
        });
    }
    (cues, skipped)
}

fn strip_tags(text: &str) -> String {
    let mut out = String::new();
    let mut in_tag = false;
    for c in text.chars() {
        match c {
            '<' => in_tag = true,
            '>' => in_tag = false,
            _ if !in_tag => out.push(c),
            _ => {}
        }
    }
    out.replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("{\\an8}", "")
}

/// Decodes caption text. Windows tools write UTF-16 with a byte-order mark and "UTF-8 with BOM",
/// and neither is valid UTF-8 for `read_to_string`; the mark must also be stripped from a UTF-8
/// file or the first cue's index no longer parses.
fn decode_caption_bytes(bytes: &[u8]) -> Option<String> {
    let le = bytes.starts_with(&[0xFF, 0xFE]);
    let be = bytes.starts_with(&[0xFE, 0xFF]);
    if le || be {
        let body = bytes.get(2..)?;
        if body.len() % 2 != 0 {
            return None;
        }
        let units: Vec<u16> = body
            .chunks_exact(2)
            .map(|pair| {
                if le {
                    u16::from_le_bytes([pair[0], pair[1]])
                } else {
                    u16::from_be_bytes([pair[0], pair[1]])
                }
            })
            .collect();
        return String::from_utf16(&units).ok();
    }
    Some(
        String::from_utf8_lossy(bytes)
            .trim_start_matches('\u{feff}')
            .to_string(),
    )
}

/// Reads a caption file in any of those encodings.
fn read_caption_file(path: &Path) -> Result<String> {
    let meta = std::fs::metadata(path)
        .map_err(|e| Error::io(format!("cannot read {}: {e}", path.display())))?;
    if meta.len() > MAX_CAPTION_FILE {
        return Err(Error::invalid("caption file is larger than 4 MB"));
    }
    let bytes = std::fs::read(path)
        .map_err(|e| Error::io(format!("cannot read {}: {e}", path.display())))?;
    decode_caption_bytes(&bytes)
        .ok_or_else(|| Error::invalid(format!("{} is not valid UTF-8 or UTF-16", path.display())))
}

pub fn read_subtitle_file(path: &Path) -> Result<(Vec<Cue>, usize)> {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    if ext != "srt" && ext != "vtt" {
        return Err(Error::invalid(format!(
            "{} is not a .srt or .vtt file",
            path.display()
        )));
    }
    let text = read_caption_file(path)?;
    let (cues, skipped) = parse_subtitles(&text);
    if cues.is_empty() {
        return Err(Error::invalid(
            "no usable cues were found in the caption file",
        ));
    }
    Ok((cues, skipped))
}

fn is_weak_ending(word: &str) -> bool {
    const WEAK: &[&str] = &[
        "a", "an", "the", "your", "my", "our", "its", "their", "of", "to", "and", "or", "but",
        "in", "on", "at", "for", "with", "as", "is", "it", "that", "this",
    ];
    let w: String = word
        .chars()
        .filter(|c| c.is_alphanumeric())
        .collect::<String>()
        .to_lowercase();
    !word.ends_with(['.', '!', '?', ',', ';', ':']) && WEAK.contains(&w.as_str())
}

/// Split a script into caption-sized phrases (at most `max_chars`), breaking at sentence ends
/// and commas first, then at word boundaries.
pub fn split_phrases(script: &str, max_chars: usize) -> Vec<String> {
    let max_chars = max_chars.max(8);
    let mut phrases: Vec<String> = Vec::new();
    let mut current = String::new();
    let flush = |current: &mut String, phrases: &mut Vec<String>| {
        let t = current.trim();
        if !t.is_empty() {
            phrases.push(t.to_string());
        }
        current.clear();
    };
    for word in script.split_whitespace() {
        let projected =
            current.chars().count() + usize::from(!current.is_empty()) + word.chars().count();
        if !current.is_empty() && projected > max_chars {
            // Never end a caption on a dangling function word ("LIGHT HAS A / BEGINNING.").
            let mut carry: Vec<String> = Vec::new();
            while current.split_whitespace().count() > 1
                && current
                    .split_whitespace()
                    .last()
                    .map(is_weak_ending)
                    .unwrap_or(false)
            {
                let cut = current.rfind(' ').unwrap_or(0);
                carry.insert(0, current[cut + 1..].to_string());
                current.truncate(cut);
            }
            flush(&mut current, &mut phrases);
            current = carry.join(" ");
        }
        if !current.is_empty() {
            current.push(' ');
        }
        current.push_str(word);
        if word.ends_with(['.', '!', '?'])
            || (word.ends_with([',', ';', ':']) && current.chars().count() >= max_chars / 2)
        {
            flush(&mut current, &mut phrases);
        }
    }
    flush(&mut current, &mut phrases);
    phrases
}

/// Spread phrases across `[start, end)` in proportion to their length, with a minimum on-screen
/// time. This is an estimate, not forced alignment: say so when using it.
pub fn time_script(script: &str, start: Time, end: Time, max_chars: usize) -> Result<Vec<Cue>> {
    let span = (end - start).as_secs_f64();
    if span <= 0.0 {
        return Err(Error::invalid(
            "the caption span must have a positive length",
        ));
    }
    let phrases = split_phrases(script, max_chars);
    if phrases.is_empty() {
        return Err(Error::invalid("the script has no words"));
    }
    let weights: Vec<f64> = phrases
        .iter()
        .map(|p| p.chars().count().max(3) as f64)
        .collect();
    let total: f64 = weights.iter().sum();
    let mut cursor = start.as_secs_f64();
    let mut cues = Vec::new();
    for (phrase, weight) in phrases.iter().zip(&weights) {
        let len = span * weight / total;
        let stop = (cursor + len).min(end.as_secs_f64());
        if stop - cursor > 0.01 {
            cues.push(Cue {
                start: Time::from_secs_f64(cursor),
                end: Time::from_secs_f64(stop),
                text: phrase.clone(),
            });
        }
        cursor = stop;
    }
    Ok(cues)
}

fn clock(t: Time, comma: bool) -> String {
    let ms = (t.as_secs_f64() * 1000.0).round().max(0.0) as u64;
    format!(
        "{:02}:{:02}:{:02}{}{:03}",
        ms / 3_600_000,
        ms / 60_000 % 60,
        ms / 1000 % 60,
        if comma { ',' } else { '.' },
        ms % 1000
    )
}

/// Serialises cues as SRT (`vtt == false`) or WebVTT.
pub fn to_subtitles(cues: &[crate::project::model::Cue], vtt: bool) -> String {
    let mut out = String::new();
    if vtt {
        out.push_str("WEBVTT\n\n");
    }
    for (i, c) in cues.iter().enumerate() {
        if !vtt {
            out.push_str(&format!("{}\n", i + 1));
        }
        out.push_str(&format!(
            "{} --> {}\n{}\n\n",
            clock(c.start, !vtt),
            clock(c.end, !vtt),
            c.text.replace("\n\n", "\n")
        ));
    }
    out
}

/// A transcription engine that can be run on a WAV file to produce SRT.
#[derive(Debug, Clone)]
pub enum Engine {
    /// `CINVAR_TRANSCRIBE_CMD` template with `{audio}` and `{srt}` placeholders.
    Custom(String),
    /// openai-whisper CLI (`whisper`).
    Whisper(PathBuf),
    /// whisper.cpp CLI (`whisper-cli`) with a model from `CINVAR_WHISPER_MODEL`.
    WhisperCpp(PathBuf, PathBuf),
    /// faster-whisper in a Python environment named by `CINVAR_FASTER_WHISPER_PYTHON`. CPU int8,
    /// local model only (`CINVAR_FASTER_WHISPER_MODEL` / `_CACHE`); never downloads unless
    /// `CINVAR_FASTER_WHISPER_ALLOW_DOWNLOAD=1`.
    FasterWhisper(PathBuf),
    /// WhisperX (word-level alignment) in a Python environment named by `CINVAR_WHISPERX_PYTHON`.
    /// Local models only unless `CINVAR_WHISPERX_ALLOW_DOWNLOAD=1`. Chosen explicitly
    /// (`--engine whisperx`), never by auto-detection.
    WhisperX(PathBuf),
}

const FW_HELPER: &str = include_str!("fw_helper.py");

impl Engine {
    pub fn name(&self) -> &'static str {
        match self {
            Engine::Custom(_) => "custom (CINVAR_TRANSCRIBE_CMD)",
            Engine::Whisper(_) => "whisper",
            Engine::WhisperCpp(..) => "whisper.cpp",
            Engine::FasterWhisper(_) => "faster-whisper",
            Engine::WhisperX(_) => "whisperx",
        }
    }
}

pub fn detect_engine() -> Option<Engine> {
    if let Ok(cmd) = std::env::var("CINVAR_TRANSCRIBE_CMD") {
        if cmd.contains("{audio}") && cmd.contains("{srt}") {
            return Some(Engine::Custom(cmd));
        }
    }
    // The exported variable wins; otherwise the path cinvar setup --pro-ai recorded is used.
    if let Some((py, _)) = crate::util::paths::helper_path("CINVAR_FASTER_WHISPER_PYTHON") {
        if py.is_file() {
            return Some(Engine::FasterWhisper(py));
        }
    }
    if let (Some(bin), Ok(model)) = (
        proc::which("whisper-cli"),
        std::env::var("CINVAR_WHISPER_MODEL"),
    ) {
        let model = PathBuf::from(model);
        if model.is_file() {
            return Some(Engine::WhisperCpp(bin, model));
        }
    }
    proc::which("whisper").map(Engine::Whisper)
}

/// Resolve an engine by name: `auto`/empty uses detection, `whisperx` requires
/// `CINVAR_WHISPERX_PYTHON`.
pub fn detect_engine_named(name: &str) -> Result<Engine> {
    match name.trim().to_ascii_lowercase().as_str() {
        "" | "auto" => detect_engine()
            .ok_or_else(|| Error::new(ErrorCode::DependencyMissing, engine_help().to_string())),
        "whisperx" => {
            // The exported variable wins; otherwise the path cinvar setup --pro-ai recorded is
            // used, so an installed WhisperX needs no environment at all.
            let (py, origin) = crate::util::paths::helper_path("CINVAR_WHISPERX_PYTHON")
                .ok_or_else(|| {
                    Error::new(
                        ErrorCode::DependencyMissing,
                        "--engine whisperx needs CINVAR_WHISPERX_PYTHON set to a Python with whisperx installed (plus CINVAR_WHISPERX_MODEL for a local model), or cinvar setup --pro-ai to install one".to_string(),
                    )
                })?;
            if !py.is_file() {
                let message = match origin {
                    HelperOrigin::Environment => format!(
                        "CINVAR_WHISPERX_PYTHON points at {}, which is not a file",
                        py.display()
                    ),
                    HelperOrigin::Config => format!(
                        "CINVAR_WHISPERX_PYTHON recorded by cinvar setup --pro-ai points at {}, which is not a file",
                        py.display()
                    ),
                };
                return Err(Error::new(ErrorCode::DependencyMissing, message)
                    .with_hint("re-run: cinvar setup --pro-ai, or export the variable yourself"));
            }
            Ok(Engine::WhisperX(py))
        }
        other => Err(Error::invalid(format!(
            "unknown engine '{other}'; use auto or whisperx"
        ))),
    }
}

pub fn engine_help() -> &'static str {
    "No transcription engine found. Options (all optional): install openai-whisper so `whisper` is on PATH; or install whisper.cpp and set CINVAR_WHISPER_MODEL to a ggml model file; or set CINVAR_TRANSCRIBE_CMD to a command template containing {audio} (input wav) and {srt} (output file). Without an engine you can still use cinvar.captions.from_script or cinvar.captions.import."
}

/// Run `engine` on `audio_wav` and return the SRT text it produced.
pub fn run_engine(
    engine: &Engine,
    audio_wav: &Path,
    work_dir: &Path,
    language: Option<&str>,
) -> Result<String> {
    run_engine_with_vocab(engine, audio_wav, work_dir, language, &[])
}

/// As `run_engine`, with a custom vocabulary handed to the engine's prompt/hotword channel
/// (see `vocab`). An empty vocabulary is exactly `run_engine`: no argument is added.
pub fn run_engine_with_vocab(
    engine: &Engine,
    audio_wav: &Path,
    work_dir: &Path,
    language: Option<&str>,
    vocab: &[String],
) -> Result<String> {
    let prompt = vocab::prompt_for(vocab);
    std::fs::create_dir_all(work_dir)
        .map_err(|e| Error::io(format!("cannot create {}: {e}", work_dir.display())))?;
    let srt = work_dir.join("transcript.srt");
    let _ = std::fs::remove_file(&srt);
    if let Engine::WhisperX(py) = engine {
        let transcript = whisperx::run_whisperx_with_vocab(py, audio_wav, work_dir, language, vocab)?;
        if transcript.segments.is_empty() {
            return Err(Error::new(
                ErrorCode::Render,
                "the whisperx engine found no speech".to_string(),
            ));
        }
        return Ok(whisperx::srt_from_segments(&transcript.segments));
    }
    let opts = ProcOpts::default().with_timeout(3600);
    let output = match engine {
        Engine::Custom(template) => {
            // Split on whitespace; paths are substituted after splitting so spaces in them are safe.
            let mut parts = template.split_whitespace().map(|p| {
                p.replace("{audio}", &audio_wav.to_string_lossy())
                    .replace("{srt}", &srt.to_string_lossy())
                    // A template that wants the vocabulary opts in with {vocab}; it stays one
                    // argv element, so terms can never be read as flags.
                    .replace("{vocab}", prompt.as_deref().unwrap_or(""))
            });
            let program = parts
                .next()
                .ok_or_else(|| Error::invalid("CINVAR_TRANSCRIBE_CMD is empty"))?;
            let program = proc::which(&program).unwrap_or_else(|| PathBuf::from(program));
            let args: Vec<String> = parts.collect();
            proc::run(&program, &args, &opts)?
        }
        Engine::Whisper(bin) => {
            let mut args = vec![
                audio_wav.to_string_lossy().to_string(),
                "--model".into(),
                "base".into(),
                "--output_format".into(),
                "srt".into(),
                "--output_dir".into(),
                work_dir.to_string_lossy().to_string(),
            ];
            if let Some(lang) = language {
                args.push("--language".into());
                args.push(lang.into());
            }
            if let Some(prompt) = &prompt {
                args.push("--initial_prompt".into());
                args.push(prompt.clone());
            }
            let out = proc::run(bin, &args, &opts)?;
            // openai-whisper names the output after the input file.
            let produced = work_dir.join(format!(
                "{}.srt",
                audio_wav
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .unwrap_or("audio")
            ));
            if produced.is_file() {
                let _ = std::fs::rename(&produced, &srt);
            }
            out
        }
        Engine::FasterWhisper(py) => {
            let script = work_dir.join("fw_helper.py");
            std::fs::write(&script, FW_HELPER)?;
            let mut args = vec![
                "-I".to_string(),
                script.to_string_lossy().to_string(),
                audio_wav.to_string_lossy().to_string(),
                srt.to_string_lossy().to_string(),
                language.unwrap_or("").to_string(),
            ];
            if let Some(prompt) = &prompt {
                args.push(prompt.clone());
                args.push(vocab::hotwords_for(vocab).unwrap_or_default());
            }
            // -I ignores PYTHON* env vars but the model settings are plain env for the child.
            let mut o = ProcOpts::default().with_timeout(3600);
            for key in [
                "CINVAR_FASTER_WHISPER_MODEL",
                "CINVAR_FASTER_WHISPER_CACHE",
                "CINVAR_FASTER_WHISPER_ALLOW_DOWNLOAD",
                "HF_HOME",
            ] {
                // Exported variable first, then the value cinvar setup --pro-ai recorded.
                if let Some(v) = crate::util::paths::helper_value(key) {
                    o.env.push((key.to_string(), v));
                }
            }
            proc::run(py, &args, &o)?
        }
        Engine::WhisperX(_) => {
            return Err(Error::internal("whisperx is handled before the process runner"))
        }
        Engine::WhisperCpp(bin, model) => {
            let prefix = work_dir.join("transcript");
            let mut args = vec![
                "-m".into(),
                model.to_string_lossy().to_string(),
                "-f".into(),
                audio_wav.to_string_lossy().to_string(),
                "-osrt".into(),
                "-of".into(),
                prefix.to_string_lossy().to_string(),
            ];
            if let Some(lang) = language {
                args.push("-l".into());
                args.push(lang.into());
            }
            if let Some(prompt) = &prompt {
                args.push("--prompt".into());
                args.push(prompt.clone());
            }
            proc::run(bin, &args, &opts)?
        }
    };
    if !output.success {
        return Err(Error::new(
            ErrorCode::DependencyMissing,
            format!(
                "the {} engine failed: {}",
                engine.name(),
                crate::secrets::redact(&output.tail_stderr(4))
            ),
        ));
    }
    read_caption_file(&srt).map_err(|_| {
        Error::new(
            ErrorCode::Render,
            format!(
                "the {} engine finished but wrote no SRT file",
                engine.name()
            ),
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn subtitles_round_trip_through_the_parser() {
        let cues = vec![crate::project::model::Cue {
            start: Time::from_secs_f64(1.0),
            end: Time::from_secs_f64(2.5),
            text: "Hello".into(),
        }];
        for vtt in [false, true] {
            let (back, skipped) = parse_subtitles(&to_subtitles(&cues, vtt));
            assert_eq!(skipped, 0);
            assert_eq!(back.len(), 1);
            assert_eq!(back[0].text, "Hello");
            assert!((back[0].end.as_secs_f64() - 2.5).abs() < 1e-3);
        }
    }

    #[test]
    fn parses_srt_and_vtt() {
        let srt = "1\n00:00:01,000 --> 00:00:02,500\nHello <i>there</i>\n\n2\n00:00:03,000 --> 00:00:04,000\nSecond line\nwraps\n";
        let (cues, skipped) = parse_subtitles(srt);
        assert_eq!(skipped, 0);
        assert_eq!(cues.len(), 2);
        assert_eq!(cues[0].text, "Hello there");
        assert!((cues[0].end.as_secs_f64() - 2.5).abs() < 1e-6);
        assert_eq!(cues[1].text, "Second line wraps");

        let vtt = "WEBVTT\n\n00:01.000 --> 00:02.000 align:start\nHi\n\nNOTE comment\n\n00:03.000 --> 00:02.000\nbackwards\n";
        let (cues, skipped) = parse_subtitles(vtt);
        assert_eq!(cues.len(), 1);
        assert_eq!(skipped, 1, "the backwards cue is counted as skipped");
    }

    #[test]
    fn phrases_respect_the_limit_and_punctuation() {
        let phrases = split_phrases(
            "This is the first sentence. And here is a second, much longer one that must be split.",
            28,
        );
        assert!(
            phrases.iter().all(|p| p.chars().count() <= 34),
            "{phrases:?}"
        );
        assert_eq!(phrases[0], "This is the first sentence.");
        assert!(phrases.len() >= 3);
    }

    #[test]
    fn captions_never_end_on_a_dangling_function_word() {
        let phrases = split_phrases("Light has a beginning. Every colour you see started as one idea. Watch it take shape, and then make your own.", 16);
        for p in &phrases {
            let last = p.split_whitespace().last().unwrap();
            assert!(!is_weak_ending(last), "'{p}' ends on '{last}': {phrases:?}");
        }
        let words: usize = phrases.iter().map(|p| p.split_whitespace().count()).sum();
        assert_eq!(words, 21, "no words lost or duplicated: {phrases:?}");
    }

    #[test]
    fn script_timing_covers_the_span_without_overlap() {
        let cues = time_script(
            "One two three four. Five six seven eight nine ten.",
            Time::from_secs_f64(2.0),
            Time::from_secs_f64(12.0),
            30,
        )
        .unwrap();
        assert!(cues.len() >= 2);
        assert!((cues[0].start.as_secs_f64() - 2.0).abs() < 1e-6);
        assert!((cues.last().unwrap().end.as_secs_f64() - 12.0).abs() < 0.02);
        for pair in cues.windows(2) {
            assert!(pair[0].end <= pair[1].start);
        }
        assert!(time_script("   ", Time::ZERO, Time::from_secs_f64(1.0), 20).is_err());
    }

    #[test]
    fn rejects_non_subtitle_files() {
        assert!(read_subtitle_file(Path::new("/tmp/x.txt")).is_err());
    }

    #[test]
    fn windows_encoded_caption_files_are_decoded() {
        // "UTF-8 with BOM", as Notepad writes it.
        let mut utf8 = vec![0xEF, 0xBB, 0xBF];
        utf8.extend_from_slice(b"1\r\n00:00:01,000 --> 00:00:02,000\r\nHello\r\n");
        let text = decode_caption_bytes(&utf8).unwrap();
        let (cues, skipped) = parse_subtitles(&text);
        assert_eq!(skipped, 0);
        assert_eq!(cues.len(), 1);
        assert_eq!(cues[0].text, "Hello");

        // UTF-16 LE with a BOM, the other encoding Windows editors produce.
        let body = "1\r\n00:00:01,000 --> 00:00:02,000\r\nHello\r\n";
        let mut utf16 = vec![0xFF, 0xFE];
        for unit in body.encode_utf16() {
            utf16.extend_from_slice(&unit.to_le_bytes());
        }
        let text = decode_caption_bytes(&utf16).unwrap();
        let (cues, skipped) = parse_subtitles(&text);
        assert_eq!(skipped, 0);
        assert_eq!(cues.len(), 1);

        // A torn UTF-16 file is reported, not silently half-decoded.
        assert!(decode_caption_bytes(&[0xFF, 0xFE, 0x41]).is_none());
    }
}
