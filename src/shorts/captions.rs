//! Word-highlighted karaoke captions for one Short, burned in with the repository's ASS
//! engine (crate::captions::ass).
//!
//! Words are grouped into caption lines by length, by pauses and by sentence ends, so a line
//! never outlives the phrase it belongs to. Every word keeps its own timing, which is what
//! makes the karaoke sweep in the ASS script follow the speaker.

use std::path::Path;

use crate::captions::ass::{self, AssLine, AssStyle, AssWord};
use crate::captions::whisperx::WordStamp;
use crate::error::{Error, Result};

/// Most characters a caption line may hold.
pub const MAX_LINE_CHARS: usize = 34;
/// Longest a caption line may stay on screen, in seconds.
pub const MAX_LINE_SECS: f64 = 3.2;
/// A pause longer than this starts a new caption line.
pub const PAUSE_BREAK_SECS: f64 = 0.5;

fn ends_sentence(raw: &str) -> bool {
    raw.trim_end().ends_with(['.', '!', '?'])
}

/// Caption lines for the source range [start, end), re-based to the Short's own timeline.
///
/// Words whose midpoint falls outside the window are dropped: they belong to the
/// neighbouring Short. Words that straddle the edges are clamped, never shifted.
pub fn lines_for_window(words: &[WordStamp], start: f64, end: f64) -> Vec<AssLine> {
    let mut kept: Vec<AssWord> = Vec::new();
    for word in words {
        let text = word.word.trim();
        if text.is_empty() || word.end < word.start {
            continue;
        }
        let mid = (word.start + word.end) / 2.0;
        if mid < start || mid >= end {
            continue;
        }
        let s = word.start.max(start) - start;
        let e = word.end.min(end) - start;
        if e <= s {
            continue;
        }
        kept.push(AssWord {
            text: text.to_string(),
            start: s,
            end: e,
        });
    }
    let mut lines: Vec<AssLine> = Vec::new();
    let mut current: Vec<AssWord> = Vec::new();
    let flush = |current: &mut Vec<AssWord>, lines: &mut Vec<AssLine>| {
        if current.is_empty() {
            return;
        }
        let text = current
            .iter()
            .map(|w| w.text.as_str())
            .collect::<Vec<_>>()
            .join(" ");
        lines.push(AssLine {
            start: current[0].start,
            end: current[current.len() - 1].end,
            text,
            words: current.clone(),
        });
        current.clear();
    };
    for word in kept {
        if let Some(previous) = current.last() {
            let chars: usize = current.iter().map(|w| w.text.chars().count() + 1).sum();
            let pause = word.start - previous.end;
            let span = word.end - current[0].start;
            if chars + word.text.chars().count() > MAX_LINE_CHARS
                || pause > PAUSE_BREAK_SECS
                || span > MAX_LINE_SECS
                || ends_sentence(&previous.text)
            {
                flush(&mut current, &mut lines);
            }
        }
        current.push(word);
    }
    flush(&mut current, &mut lines);
    lines
}

/// Burn the karaoke track into the vertical master, in place.
pub fn burn_in(video: &Path, lines: &[AssLine], width: u32, height: u32) -> Result<()> {
    if lines.is_empty() {
        return Ok(());
    }
    let ass_path = video.with_extension("captions.ass");
    let script = ass::generate_ass(
        lines,
        width,
        height,
        ass::Layout::for_canvas(width, height),
        &AssStyle::default(),
    )?;
    std::fs::write(&ass_path, script)
        .map_err(|e| Error::io(format!("cannot write {}: {e}", ass_path.display())))?;
    let tmp = video.with_extension("captioned.mp4");
    ass::burn_subtitle_file(video, &ass_path, &tmp)?;
    std::fs::rename(&tmp, video)
        .map_err(|e| Error::io(format!("cannot replace {}: {e}", video.display())))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn w(text: &str, start: f64, end: f64) -> WordStamp {
        WordStamp {
            word: text.into(),
            start,
            end,
        }
    }

    #[test]
    fn words_outside_the_window_are_dropped_and_times_rebased() {
        let words = vec![
            w("before", 0.0, 0.5),
            w("Hello", 10.2, 10.6),
            w("world", 10.6, 11.0),
            w("after", 30.0, 30.5),
        ];
        let lines = lines_for_window(&words, 10.0, 20.0);
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].text, "Hello world");
        assert!((lines[0].start - 0.2).abs() < 1e-9);
        assert!((lines[0].end - 1.0).abs() < 1e-9);
        assert_eq!(lines[0].words.len(), 2);
    }

    #[test]
    fn lines_respect_length_pauses_and_sentence_ends() {
        let mut words = Vec::new();
        let mut t = 0.0;
        for token in "Hello there this is a first line that runs on. Second sentence starts now!".split_whitespace() {
            words.push(w(token, t, t + 0.3));
            t += 0.35;
        }
        // A long pause must break the line even without punctuation.
        words.push(w("Later", t + 2.0, t + 2.3));
        let lines = lines_for_window(&words, 0.0, 60.0);
        assert!(lines.len() >= 3, "{lines:?}");
        for line in &lines {
            assert!(line.text.chars().count() <= MAX_LINE_CHARS + 12, "{}", line.text);
            assert!(line.end - line.start <= MAX_LINE_SECS + 1e-9, "{:?}", line);
            assert!(!line.words.is_empty());
        }
        // No word is lost or duplicated by the grouping.
        let grouped: usize = lines.iter().map(|l| l.words.len()).sum();
        assert_eq!(grouped, words.len());
    }

    #[test]
    fn an_empty_window_produces_no_lines() {
        assert!(lines_for_window(&[], 0.0, 10.0).is_empty());
        assert!(lines_for_window(&[w("only", 30.0, 31.0)], 0.0, 10.0).is_empty());
    }
}
