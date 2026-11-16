//! Pacing: silence trim of a selected window.
//!
//! The keep-span plan comes from the repository's existing Auto-Editor adapter
//! (crate::analysis::autoeditor), inverted into the gaps Auto-Editor cut. When Auto-Editor is
//! not configured the same gaps are measured with ffmpeg silencedetect, so a Short is always
//! trimmed by measured silence and never by a fixed assumption.
//!
//! Only the head and tail of a window are trimmed: a Short is rendered as one continuous
//! source range (one clip in the delivery timeline), so a cut in the middle of the window
//! would desynchronise the word-timed captions, which are laid out against the source range.

use std::path::Path;

use crate::analysis::autoeditor::{self, KeepSpan};

/// Level treated as silence when Auto-Editor is unavailable.
pub const SILENCE_DB: f64 = -35.0;
/// Shortest silence worth trimming, in seconds.
pub const SILENCE_MIN_SECS: f64 = 0.25;
/// Breathing room kept next to the first and last word of the window.
pub const EDGE_KEEP_SECS: f64 = 0.08;

/// A region of the source to drop, in seconds.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Gap {
    pub start: f64,
    pub end: f64,
}

impl Gap {
    fn covers_start(&self, t: f64) -> bool {
        self.start <= t + 1e-6 && self.end > t
    }
    fn covers_end(&self, t: f64) -> bool {
        self.start < t && self.end >= t - 1e-6
    }
}

/// Complement of the kept spans within [0, total): what the pacing pass removes.
pub fn gaps_from_keep_spans(spans: &[KeepSpan], total: f64) -> Vec<Gap> {
    let mut gaps = Vec::new();
    let mut cursor = 0.0f64;
    for span in spans {
        let start = span.start.max(0.0);
        let end = span.end.min(total.max(span.end));
        if start > cursor + 1e-6 {
            gaps.push(Gap {
                start: cursor,
                end: start,
            });
        }
        cursor = cursor.max(end);
    }
    if total > cursor + 1e-6 {
        gaps.push(Gap {
            start: cursor,
            end: total,
        });
    }
    gaps
}

/// Silence gaps of the source, preferring Auto-Editor's own plan.
///
/// Returns the gaps and the name of the measurement used, so a caller can report honestly
/// which pacing source produced the Short.
pub fn measure(source: &Path, total: f64, work: &Path) -> (Vec<Gap>, String) {
    if autoeditor::tool_available() {
        if let Ok(spans) = autoeditor::keep_spans(source, work) {
            let gaps = gaps_from_keep_spans(&spans, total);
            return (gaps, "auto-editor keep plan".to_string());
        }
    }
    match crate::media::detect_silence(source, SILENCE_DB, SILENCE_MIN_SECS) {
        Ok(spans) => (
            spans
                .iter()
                .map(|s| Gap {
                    start: s.start.as_secs_f64(),
                    end: s.end.as_secs_f64(),
                })
                .collect(),
            format!("ffmpeg silencedetect ({SILENCE_DB:.0} dB/{SILENCE_MIN_SECS:.2} s)"),
        ),
        Err(err) => (
            Vec::new(),
            format!("no pacing source ({})", crate::secrets::redact(&err.message)),
        ),
    }
}

fn round3(value: f64) -> f64 {
    (value * 1000.0).round() / 1000.0
}

/// Trim the silence at the head and tail of [start, end), then restore enough of the
/// window to honour min_duration. The original window is the floor: a Short is never
/// shorter than the request allows because a trim went too far.
pub fn trim_window(start: f64, end: f64, gaps: &[Gap], min_duration: f64) -> (f64, f64) {
    if !(end > start) {
        return (start, end);
    }
    let mut head = start;
    let mut tail = end;
    // Head: skip every silence the window opens inside (silencedetect can emit neighbours).
    for _ in 0..gaps.len() {
        let before = head;
        for gap in gaps {
            if gap.covers_start(head) && gap.end + EDGE_KEEP_SECS < tail {
                head = gap.end + EDGE_KEEP_SECS;
            }
        }
        if (head - before).abs() < 1e-9 {
            break;
        }
    }
    // Tail: the same at the other end.
    for _ in 0..gaps.len() {
        let before = tail;
        for gap in gaps {
            if gap.covers_end(tail) && gap.start - EDGE_KEEP_SECS > head {
                tail = gap.start - EDGE_KEEP_SECS;
            }
        }
        if (tail - before).abs() < 1e-9 {
            break;
        }
    }
    let keep = min_duration.max(0.0);
    if tail - head < keep {
        // Give the video back first from the tail, then from the head.
        let mut need = keep - (tail - head);
        let room_tail = end - tail;
        let take_tail = need.min(room_tail);
        tail += take_tail;
        need -= take_tail;
        let room_head = head - start;
        let take_head = need.min(room_head);
        head -= take_head;
        need -= take_head;
        if need > 1e-6 {
            // The window itself is shorter than the floor; hand back the original range.
            return (round3(start), round3(end));
        }
    }
    if tail - head <= 0.0 {
        return (round3(start), round3(end));
    }
    (round3(head), round3(tail))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keep_spans_invert_into_gaps() {
        let spans = vec![
            KeepSpan { start: 1.0, end: 4.0 },
            KeepSpan { start: 6.0, end: 10.0 },
        ];
        let gaps = gaps_from_keep_spans(&spans, 12.0);
        assert_eq!(gaps.len(), 3);
        assert_eq!(gaps[0], Gap { start: 0.0, end: 1.0 });
        assert_eq!(gaps[1], Gap { start: 4.0, end: 6.0 });
        assert_eq!(gaps[2], Gap { start: 10.0, end: 12.0 });
        assert!(gaps_from_keep_spans(&[], 0.0).is_empty());
    }

    #[test]
    fn head_and_tail_silence_are_trimmed() {
        let gaps = vec![
            Gap { start: 0.0, end: 2.0 },
            Gap { start: 18.0, end: 25.0 },
        ];
        let (start, end) = trim_window(0.0, 20.0, &gaps, 6.0);
        assert!((start - (2.0 + EDGE_KEEP_SECS)).abs() < 1e-6, "head {start}");
        assert!((end - (18.0 - EDGE_KEEP_SECS)).abs() < 1e-6, "tail {end}");
    }

    #[test]
    fn trimming_never_goes_below_the_requested_duration() {
        // Speech only runs 5 s to 7 s inside a 20 s window; the floor is 10 s.
        let gaps = vec![
            Gap { start: 0.0, end: 5.0 },
            Gap { start: 7.0, end: 20.0 },
        ];
        let (start, end) = trim_window(0.0, 20.0, &gaps, 10.0);
        assert!(end - start >= 10.0 - 1e-6, "{start}..{end}");
        assert!(start >= 0.0 && end <= 20.0 + 1e-9, "{start}..{end}");
    }

    #[test]
    fn interior_silence_is_kept_so_captions_stay_in_sync() {
        let gaps = vec![Gap { start: 8.0, end: 10.0 }];
        let (start, end) = trim_window(2.0, 16.0, &gaps, 5.0);
        assert!((start - 2.0).abs() < 1e-9 && (end - 16.0).abs() < 1e-9);
    }

    #[test]
    fn degenerate_windows_are_returned_unchanged() {
        assert_eq!(trim_window(5.0, 5.0, &[], 4.0), (5.0, 5.0));
        assert_eq!(trim_window(5.0, 1.0, &[], 4.0), (5.0, 1.0));
        let (s, e) = trim_window(0.0, 3.0, &[], 4.0);
        assert_eq!((s, e), (0.0, 3.0));
    }
}
