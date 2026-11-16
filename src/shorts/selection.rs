//! Transcript-aware highlight scoring and segment selection.
//!
//! A Short is chosen from speech, not from a time grid: every candidate window is
//! anchored on a word boundary, and its score is a measured function of what happens in
//! that window.
//!
//! ~~~text
//! score = (0.45 * S + 0.30 * E + 0.25 * C) / (combined weight)     clamped to 0..=1
//!
//! S  speech density      sum of word durations inside the window / window length
//!                        (1.0 = continuous talking, 0.0 = no speech at all)
//! E  audio energy        (mean dBFS - (-60)) / ((-6) - (-60)), clamped to 0..=1
//!                        measured with ffmpeg volumedetect over the window only
//! C  sentence completeness
//!                        0.55 * opens_cleanly + 0.45 * closes_cleanly
//!                        opens_cleanly: 1.0 when the window starts the source or the word
//!                        before it ends a sentence, 0.5 when the first word is capitalised
//!                        (an utterance opens), otherwise 0.0
//!                        closes_cleanly: 1.0 for . ! ?, 0.6 for , ; :, 1.0 when the
//!                        window's last word is the source's last word, otherwise 0.0
//! ~~~
//!
//! When the source carries no audio stream, E cannot be measured: its weight moves to S
//! and the score says so in its reason (the same convention crate::analysis::combine
//! uses). Windowing is two-phase so a two-hour source costs a bounded number of ffmpeg
//! probes: windows are ranked on S and C first, and only the MAX_SCORED best-disjoint windows
//! pay for an energy measurement before the final ranking.

use crate::captions::whisperx::WordStamp;

/// Weight of speech density in the highlight score.
pub const WEIGHT_SPEECH_DENSITY: f64 = 0.45;
/// Weight of measured audio energy in the highlight score.
pub const WEIGHT_AUDIO_ENERGY: f64 = 0.30;
/// Weight of sentence completeness in the highlight score.
pub const WEIGHT_COMPLETENESS: f64 = 0.25;
/// Mean dBFS treated as silence for the energy component.
const SILENT_DB: f64 = -60.0;
/// Mean dBFS treated as full-scale loudness for the energy component.
const LOUD_DB: f64 = -6.0;
/// Hard ceiling on candidate windows, so a long source stays a bounded job.
const MAX_CANDIDATES: usize = 400;
/// Windows that pay for a volumedetect probe in the second scoring phase.
const MAX_SCORED: usize = 24;
/// Silence kept before the first word and after the last one, in seconds.
const EDGE_PAD: f64 = 0.12;
/// A candidate needs at least this many words to be worth scoring.
const MIN_WORDS: usize = 4;

/// One candidate window, anchored on word indices.
#[derive(Debug, Clone, PartialEq)]
pub struct Candidate {
    /// Inclusive indices into the word list.
    pub first: usize,
    pub last: usize,
    /// Source range in seconds.
    pub start: f64,
    pub end: f64,
    pub score: f64,
    /// Human-readable breakdown of the score components.
    pub reason: String,
}

impl Candidate {
    pub fn duration(&self) -> f64 {
        (self.end - self.start).max(0.0)
    }
}

/// True when the raw token closes a sentence.
fn ends_sentence(raw: &str) -> bool {
    raw.trim_end().ends_with(['.', '!', '?'])
}

/// True when the raw token closes a clause.
fn ends_clause(raw: &str) -> bool {
    raw.trim_end().ends_with([',', ';', ':'])
}

/// Fraction of [start, end) actually covered by spoken words.
pub fn speech_density(words: &[WordStamp], first: usize, last: usize, start: f64, end: f64) -> f64 {
    let span = end - start;
    if !(span > 0.0) || last >= words.len() || first > last {
        return 0.0;
    }
    let spoken: f64 = words[first..=last]
        .iter()
        .map(|w| (w.end.min(end) - w.start.max(start)).max(0.0))
        .sum();
    (spoken / span).clamp(0.0, 1.0)
}

/// Does the window open on an utterance boundary and close on a pause?
pub fn sentence_completeness(
    words: &[WordStamp],
    first: usize,
    last: usize,
    source_end_word: usize,
) -> f64 {
    if last >= words.len() || first > last {
        return 0.0;
    }
    let opens: f64 = if first == 0 {
        1.0
    } else if ends_sentence(&words[first - 1].word) {
        1.0
    } else if words[first]
        .word
        .chars()
        .find(|c| c.is_alphanumeric())
        .map(|c| c.is_uppercase())
        .unwrap_or(false)
    {
        0.5
    } else {
        0.0
    };
    let closes: f64 = if ends_sentence(&words[last].word) {
        1.0
    } else if last == source_end_word {
        1.0
    } else if ends_clause(&words[last].word) {
        0.6
    } else {
        0.0
    };
    (0.55 * opens + 0.45 * closes).clamp(0.0, 1.0)
}

/// Energy component from a measured mean level, or None when nothing was measured.
pub fn energy_component(mean_db: Option<f64>) -> Option<f64> {
    mean_db.map(|mean| ((mean - SILENT_DB) / (LOUD_DB - SILENT_DB)).clamp(0.0, 1.0))
}

fn round3(value: f64) -> f64 {
    (value * 1000.0).round() / 1000.0
}

/// Score one window; returns the score and the component breakdown.
pub fn score_window(
    words: &[WordStamp],
    first: usize,
    last: usize,
    start: f64,
    end: f64,
    mean_db: Option<f64>,
) -> (f64, String) {
    let density = speech_density(words, first, last, start, end);
    let completeness = sentence_completeness(words, first, last, words.len().saturating_sub(1));
    let energy = energy_component(mean_db);
    // An unmeasurable energy component hands its weight to speech density rather than
    // scoring every window as if the source were silent.
    let mut w_density = WEIGHT_SPEECH_DENSITY;
    let w_energy = WEIGHT_AUDIO_ENERGY;
    if energy.is_none() {
        w_density += WEIGHT_AUDIO_ENERGY;
    }
    let total = w_density + w_energy + WEIGHT_COMPLETENESS;
    let weighted =
        w_density * density + w_energy * energy.unwrap_or(0.0) + WEIGHT_COMPLETENESS * completeness;
    let score = if total > 0.0 {
        (weighted / total).clamp(0.0, 1.0)
    } else {
        0.0
    };
    let audio = match (mean_db, energy) {
        (Some(db), Some(e)) => format!("audio {e:.2} ({db:.1} dBFS)"),
        _ => "audio unmeasurable".to_string(),
    };
    let reason = format!("speech {density:.2}, {audio}, sentence {completeness:.2}");
    (round3(score), reason)
}

/// Build every candidate window of the source, ranked best first.
///
/// energy_at(start, end) measures a window's mean level in dBFS; it is only called for
/// the windows that survive the first ranking pass.
pub fn candidates(
    words: &[WordStamp],
    source_total: f64,
    min_duration: f64,
    max_duration: f64,
    energy_at: &dyn Fn(f64, f64) -> Option<f64>,
) -> Vec<Candidate> {
    if words.is_empty() || !(source_total > 0.0) || !(min_duration > 0.0) || max_duration < min_duration
    {
        return Vec::new();
    }
    let step = (words.len() / MAX_CANDIDATES).max(1);
    let mut windows: Vec<Candidate> = Vec::new();
    for first in (0..words.len()).step_by(step) {
        let start = (words[first].start - EDGE_PAD).max(0.0);
        // Grow the window on its PADDED length: the edge pads are part of the Short, so a
        // window grown to max_duration and then padded would exceed the ceiling and be lost.
        let mut last = first;
        while last + 1 < words.len()
            && (words[last + 1].end + EDGE_PAD).min(source_total) - start <= max_duration
        {
            last += 1;
        }
        if last - first + 1 < MIN_WORDS {
            continue;
        }
        let mut end = (words[last].end + EDGE_PAD).min(source_total);
        if end - start < min_duration {
            // Too few words to fill the minimum: pad the tail out to the floor.
            end = (start + min_duration).min(source_total);
            if end - start < min_duration - 1e-6 {
                continue;
            }
        }
        if end - start < min_duration - 1e-6 || end - start > max_duration + 1e-6 {
            continue;
        }
        let (score, reason) = score_window(words, first, last, start, end, None);
        windows.push(Candidate {
            first,
            last,
            start: round3(start),
            end: round3(end),
            score,
            reason,
        });
    }
    // Phase one ranks on density + completeness to decide who pays for a measurement. The
    // budget is spent on a spread of windows, not on the 24 best ones: taking every candidate
    // in rank order would spend it on neighbours of the single densest passage, and a request
    // for several Shorts would then have no disjoint window left to choose.
    windows.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(a.start.partial_cmp(&b.start).unwrap_or(std::cmp::Ordering::Equal))
    });
    let mut spread: Vec<Candidate> = Vec::new();
    for candidate in windows {
        if spread.len() >= MAX_SCORED {
            break;
        }
        let disjoint = spread
            .iter()
            .all(|kept| candidate.start >= kept.end - 1e-6 || candidate.end <= kept.start + 1e-6);
        if disjoint {
            spread.push(candidate);
        }
    }
    let mut windows = spread;
    // Phase two measures the audio and re-scores with the full formula.
    for window in windows.iter_mut() {
        let mean_db = energy_at(window.start, window.end);
        let (score, reason) = score_window(
            words, window.first, window.last, window.start, window.end, mean_db,
        );
        window.score = score;
        window.reason = reason;
    }
    windows.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(a.start.partial_cmp(&b.start).unwrap_or(std::cmp::Ordering::Equal))
    });
    windows
}

/// Pick count non-overlapping windows, best first, then in chronological order.
///
/// When the best-scoring windows collide, the losers are re-offered once with a relaxed
/// overlap budget so count Shorts can still be delivered from a short source; a window
/// that shares more than half of its length with an accepted one is never re-offered.
pub fn choose(candidates: &[Candidate], count: usize) -> Vec<Candidate> {
    let mut chosen: Vec<Candidate> = Vec::new();
    for relax in [0.0f64, 0.5] {
        for candidate in candidates {
            if chosen.len() >= count {
                break;
            }
            let acceptable = chosen.iter().all(|c| {
                let overlap = (c.end.min(candidate.end) - c.start.max(candidate.start)).max(0.0);
                overlap <= relax * candidate.duration().min(c.duration())
            });
            let duplicate = chosen
                .iter()
                .any(|c| c.start == candidate.start && c.end == candidate.end);
            if acceptable && !duplicate {
                chosen.push(candidate.clone());
            }
        }
    }
    chosen.sort_by(|a, b| a.start.partial_cmp(&b.start).unwrap_or(std::cmp::Ordering::Equal));
    chosen.truncate(count);
    chosen
}

/// Candidate windows for a source with no usable transcript (an instrumental, or a machine
/// with no transcription engine): a fixed grid over the source, ranked on measured audio
/// energy alone. Words indices are 0/0 because there are no words; the reason string says so,
/// and the score is the plain energy component. Windowing is capped so the grid never costs
/// more than MAX_SCORED level measurements.
pub fn grid_candidates(
    source_total: f64,
    min_duration: f64,
    max_duration: f64,
    energy_at: &dyn Fn(f64, f64) -> Option<f64>,
) -> Vec<Candidate> {
    if !(source_total > 0.0) || !(min_duration > 0.0) || max_duration < min_duration {
        return Vec::new();
    }
    let window = max_duration.min(source_total);
    let step = (source_total / MAX_SCORED as f64)
        .max(window / 2.0)
        .max(min_duration / 2.0)
        .max(0.25);
    let mut out: Vec<Candidate> = Vec::new();
    let mut start = 0.0f64;
    loop {
        let end = (start + window).min(source_total);
        if end - start >= min_duration - 1e-9 {
            let mean_db = energy_at(start, end);
            let energy = energy_component(mean_db).unwrap_or(0.0);
            let reason = match mean_db {
                Some(db) => format!("audio energy {energy:.2} ({db:.1} dBFS); no transcript"),
                None => "audio energy unmeasurable; no transcript".to_string(),
            };
            out.push(Candidate {
                first: 0,
                last: 0,
                start: round3(start),
                end: round3(end),
                score: round3(energy),
                reason,
            });
        }
        if end >= source_total - 1e-9 {
            break;
        }
        start += step;
    }
    out.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(a.start.partial_cmp(&b.start).unwrap_or(std::cmp::Ordering::Equal))
    });
    out.truncate(MAX_SCORED);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_energy_grid_finds_the_loud_region() {
        // Energy is only high between 60 s and 70 s, whatever the grid's window offsets.
        let energy = |start: f64, end: f64| -> Option<f64> {
            let mid = (start + end) / 2.0;
            Some(if (60.0..70.0).contains(&mid) { -8.0 } else { -50.0 })
        };
        let windows = grid_candidates(120.0, 8.0, 20.0, &energy);
        assert!(!windows.is_empty());
        assert!(windows.len() <= MAX_SCORED);
        let picked = choose(&windows, 1);
        assert_eq!(picked.len(), 1);
        assert!(picked[0].start >= 50.0, "loud region wins: {:?}", picked[0]);
        assert!(picked[0].reason.contains("no transcript"), "{:?}", picked[0].reason);
        // Deterministic and bounded by the requested duration.
        assert_eq!(grid_candidates(120.0, 8.0, 20.0, &energy), windows);
        for window in &windows {
            assert!(window.duration() <= 20.0 + 1e-6 && window.duration() >= 8.0 - 1e-6);
        }
        assert!(grid_candidates(0.0, 8.0, 20.0, &energy).is_empty());
        assert!(grid_candidates(10.0, 8.0, 20.0, &|_, _| None).len() == 1);
    }

    fn w(word: &str, start: f64, end: f64) -> WordStamp {
        WordStamp {
            word: word.into(),
            start,
            end,
        }
    }

    /// 40 s of speech in four sentences with a long pause between them.
    fn speech() -> Vec<WordStamp> {
        let mut words = Vec::new();
        let sentences = [
            "This is the first sentence about colour.",
            "Then we talk about light and shadow.",
            "Here comes the loud celebration moment!",
            "And finally a quiet closing line.",
        ];
        let mut t = 0.4;
        for (n, sentence) in sentences.iter().enumerate() {
            for token in sentence.split_whitespace() {
                words.push(w(token, t, t + 0.32));
                t += 0.36;
            }
            t += if n == 0 { 6.0 } else { 0.6 };
        }
        words
    }

    #[test]
    fn density_counts_only_spoken_time() {
        let words = speech();
        let dense = speech_density(&words, 0, 6, words[0].start, words[6].end);
        assert!(dense > 0.7, "dense window {dense}");
        let loose = speech_density(&words, 0, 6, words[0].start, words[6].end * 2.0);
        assert!(loose < dense, "{loose} < {dense}");
        assert_eq!(speech_density(&words, 0, 6, 5.0, 5.0), 0.0);
        assert_eq!(speech_density(&[], 0, 0, 0.0, 1.0), 0.0);
    }

    #[test]
    fn completeness_rewards_sentence_boundaries() {
        let words = speech();
        let first_end = words.iter().position(|w| w.word.ends_with('.')).unwrap();
        let complete = sentence_completeness(&words, 0, first_end, words.len() - 1);
        let partial = sentence_completeness(&words, 2, first_end - 2, words.len() - 1);
        assert!(complete > partial, "{complete} > {partial}");
        assert!((complete - 1.0).abs() < 1e-9, "{complete}");
        let no_punct = vec![w("hello", 0.0, 0.3), w("there", 0.3, 0.6)];
        assert!(sentence_completeness(&no_punct, 0, 1, 1) > 0.9);
        assert_eq!(sentence_completeness(&words, 5, 2, words.len() - 1), 0.0);
    }

    #[test]
    fn score_is_the_documented_weighted_formula() {
        let words = speech();
        let first_end = words.iter().position(|w| w.word.ends_with('.')).unwrap();
        let (start, end) = (words[0].start, words[first_end].end);
        let density = speech_density(&words, 0, first_end, start, end);
        let completeness = sentence_completeness(&words, 0, first_end, words.len() - 1);
        // -6 dBFS is the top of the energy scale: E = 1.0.
        let expected = (WEIGHT_SPEECH_DENSITY * density
            + WEIGHT_AUDIO_ENERGY
            + WEIGHT_COMPLETENESS * completeness)
            / (WEIGHT_SPEECH_DENSITY + WEIGHT_AUDIO_ENERGY + WEIGHT_COMPLETENESS);
        let (score, reason) = score_window(&words, 0, first_end, start, end, Some(-6.0));
        let expected = (expected * 1000.0).round() / 1000.0;
        assert!((score - expected).abs() < 1e-9, "{score} vs {expected}");
        assert!(
            reason.contains("speech") && reason.contains("audio") && reason.contains("sentence"),
            "{reason}"
        );

        let (loud, _) = score_window(&words, 0, first_end, start, end, Some(-9.0));
        let (quiet, _) = score_window(&words, 0, first_end, start, end, Some(-52.0));
        assert!(loud > quiet, "{loud} > {quiet}");
        let (silent, reason) = score_window(&words, 0, first_end, start, end, None);
        assert!(reason.contains("unmeasurable"), "{reason}");
        assert!(silent > quiet, "unmeasured audio must not be scored as silence");
    }

    #[test]
    fn candidates_are_word_anchored_and_inside_the_bounds() {
        let words = speech();
        let total = words.last().unwrap().end + 0.5;
        let windows = candidates(&words, total, 6.0, 12.0, &|_, _| Some(-20.0));
        assert!(!windows.is_empty());
        for window in &windows {
            assert!(window.end - window.start >= 6.0 - 1e-6, "{window:?}");
            assert!(window.end - window.start <= 12.0 + 1e-6, "{window:?}");
            assert!(window.start >= 0.0 && window.end <= total + 1e-9, "{window:?}");
            // The window opens on a real word onset (minus the fixed edge pad).
            let anchor = (words[window.first].start - EDGE_PAD).max(0.0);
            assert!(
                (window.start - anchor).abs() < 1e-6 || window.start <= anchor + 1e-6,
                "window {window:?} is not anchored on word {}",
                window.first
            );
        }
        assert!(windows.len() <= MAX_SCORED);
        for pair in windows.windows(2) {
            assert!(pair[0].score >= pair[1].score, "{:?}", (pair[0].score, pair[1].score));
        }
        assert!(candidates(&words, 0.0, 6.0, 12.0, &|_, _| None).is_empty());
        assert!(candidates(&[], 30.0, 6.0, 12.0, &|_, _| None).is_empty());
        assert!(candidates(&words, total, 12.0, 6.0, &|_, _| None).is_empty());
    }

    #[test]
    fn selection_is_deterministic_non_overlapping_and_chronological() {
        let words = speech();
        let total = words.last().unwrap().end + 0.5;
        let windows = candidates(&words, total, 4.0, 9.0, &|_, _| Some(-18.0));
        let a = choose(&windows, 2);
        let b = choose(&windows, 2);
        assert_eq!(a, b, "same input must give the same selection");
        assert_eq!(a.len(), 2);
        assert!(a[0].start < a[1].start, "chronological order");
        let overlap = (a[0].end.min(a[1].start) - a[0].start.max(a[1].start)).max(0.0);
        assert_eq!(overlap, 0.0, "no shared source range: {a:?}");
        assert!(choose(&windows, 0).is_empty());
        assert!(choose(&[], 3).is_empty());
        assert!(choose(&windows, 50).len() <= 50);
    }

    #[test]
    fn dense_speech_wins_over_evenly_spaced_slices() {
        // Only one region is dense talking; evenly spaced thirds would miss it.
        let mut words = Vec::new();
        for i in 0..10 {
            words.push(w("quiet", i as f64 * 6.0, i as f64 * 6.0 + 0.2));
        }
        for i in 0..40 {
            words.push(w("talk", 60.0 + i as f64 * 0.5, 60.0 + i as f64 * 0.5 + 0.45));
        }
        words.sort_by(|a, b| a.start.partial_cmp(&b.start).unwrap());
        let windows = candidates(&words, 120.0, 8.0, 20.0, &|_, _| None);
        let picked = choose(&windows, 1);
        assert_eq!(picked.len(), 1);
        assert!(picked[0].start >= 55.0, "the dense region wins: {:?}", picked[0]);
    }
}
