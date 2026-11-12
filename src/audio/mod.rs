use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::{Error, ErrorCode, Result};
use crate::util::proc::{self, ProcOpts};
use crate::util::time::Time;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Loudness {
    pub integrated_lufs: f64,
    pub true_peak_db: f64,
    pub lra: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClippingReport {
    pub clipped: bool,
    pub peak_db: f64,
    pub detail: String,
}

/// Peak level at or above this value counts as clipping.
pub const CLIP_CEILING_DB: f64 = -0.1;
/// Floor used when ffmpeg reports -inf (digital silence) so the value stays finite.
const SILENCE_FLOOR_DB: f64 = -120.0;

const ANALYZE_TIMEOUT_SECS: u64 = 900;

fn ffmpeg_bin() -> Result<PathBuf> {
    proc::require("ffmpeg")
        .map_err(|e| e.with_hint("install ffmpeg (version 6 or newer) and run: cinvar setup"))
}

fn run_analysis(args: Vec<String>) -> Result<proc::ProcOutput> {
    let bin = ffmpeg_bin()?;
    let mut argv: Vec<String> = vec![
        "-hide_banner".into(),
        "-nostdin".into(),
        "-nostats".into(),
        "-loglevel".into(),
        "info".into(),
    ];
    argv.extend(args);
    let opts = ProcOpts::default().with_timeout(ANALYZE_TIMEOUT_SECS);
    proc::run(&bin, &argv, &opts)
}

fn require_file(path: &Path) -> Result<()> {
    if path.is_file() {
        return Ok(());
    }
    Err(Error::not_found(format!(
        "audio file not found: {}",
        path.display()
    )))
}

fn tool_failure(what: &str, out: &proc::ProcOutput) -> Error {
    let tail = out.tail_stderr(12);
    let detail = crate::secrets::redact(tail.trim());
    if detail.is_empty() {
        Error::new(ErrorCode::InvalidInput, format!("{what} failed"))
    } else {
        Error::new(ErrorCode::InvalidInput, format!("{what} failed: {detail}"))
    }
}

/// Read the number that follows a key in a log line, e.g. "max_volume: -16.1 dB".
fn number_after(hay: &str, key: &str) -> Option<f64> {
    let idx = hay.find(key)?;
    let rest = &hay[idx + key.len()..];
    let rest = rest.trim_start_matches(|c: char| c == ':' || c == '=' || c.is_whitespace());
    let end = rest
        .find(|c: char| {
            !(c.is_ascii_digit() || c == '.' || c == '-' || c == '+' || c == 'e' || c == 'E')
        })
        .unwrap_or(rest.len());
    if end == 0 {
        return None;
    }
    rest[..end].parse::<f64>().ok()
}

/// Parse a loudnorm number, mapping the textual infinities onto the silence floor.
fn loudness_value(raw: &str) -> f64 {
    let raw = raw.trim();
    match raw {
        "-inf" | "-Infinity" | "inf" => SILENCE_FLOOR_DB,
        other => other.parse::<f64>().unwrap_or(SILENCE_FLOOR_DB),
    }
}

fn json_block(text: &str) -> Option<serde_json::Value> {
    let idx = text.rfind("\"input_i\"")?;
    let start = text[..idx].rfind('{')?;
    let rest = &text[idx..];
    let end = rest.find('}')?;
    let block = &text[start..idx + end + 1];
    serde_json::from_str::<serde_json::Value>(block).ok()
}

pub fn analyze_loudness(path: &Path) -> Result<Loudness> {
    require_file(path)?;
    let args = vec![
        "-i".to_string(),
        path.to_string_lossy().to_string(),
        "-af".to_string(),
        "loudnorm=print_format=json".to_string(),
        "-vn".to_string(),
        "-f".to_string(),
        "null".to_string(),
        "-".to_string(),
    ];
    let out = run_analysis(args)?;
    if !out.success {
        return Err(tool_failure("loudness analysis", &out));
    }
    let block = json_block(&out.stderr).ok_or_else(|| {
        Error::new(
            ErrorCode::InvalidInput,
            format!(
                "{} produced no loudness measurement (does it have audio?)",
                path.display()
            ),
        )
    })?;
    let field = |key: &str| -> f64 {
        block
            .get(key)
            .and_then(|v| v.as_str())
            .map(loudness_value)
            .unwrap_or(SILENCE_FLOOR_DB)
    };
    Ok(Loudness {
        integrated_lufs: field("input_i"),
        true_peak_db: field("input_tp"),
        lra: field("input_lra"),
    })
}

pub fn detect_clipping(path: &Path) -> Result<ClippingReport> {
    require_file(path)?;
    let mut peak: Option<f64> = None;
    let mut source = "volumedetect";

    let args = vec![
        "-i".to_string(),
        path.to_string_lossy().to_string(),
        "-af".to_string(),
        "volumedetect".to_string(),
        "-vn".to_string(),
        "-f".to_string(),
        "null".to_string(),
        "-".to_string(),
    ];
    let out = run_analysis(args)?;
    if !out.success {
        return Err(tool_failure("peak analysis", &out));
    }
    for line in out.stderr.lines() {
        if line.contains("max_volume") {
            if let Some(v) = number_after(line, "max_volume") {
                peak = Some(v);
            }
        }
    }

    if peak.is_none() {
        source = "astats";
        let args = vec![
            "-i".to_string(),
            path.to_string_lossy().to_string(),
            "-af".to_string(),
            "astats=metadata=0:measure_overall=Peak_level".to_string(),
            "-vn".to_string(),
            "-f".to_string(),
            "null".to_string(),
            "-".to_string(),
        ];
        let out = run_analysis(args)?;
        for line in out.stderr.lines() {
            if line.contains("Peak level dB") {
                if let Some(v) = number_after(line, "Peak level dB") {
                    peak = Some(peak.map(|p| p.max(v)).unwrap_or(v));
                }
            }
        }
    }

    let peak_db = peak.unwrap_or(SILENCE_FLOOR_DB);
    let clipped = peak_db >= CLIP_CEILING_DB;
    let detail = if peak.is_none() {
        "no peak level reported by ffmpeg; audio may be silent or empty".to_string()
    } else if clipped {
        format!(
            "peak {peak_db:.2} dB ({source}) is at or above the {CLIP_CEILING_DB:.1} dB ceiling; reduce gain or normalise"
        )
    } else {
        format!("peak {peak_db:.2} dB ({source}) leaves headroom below the {CLIP_CEILING_DB:.1} dB ceiling")
    };
    Ok(ClippingReport {
        clipped,
        peak_db,
        detail,
    })
}

pub fn silence_plan(
    path: &Path,
    noise_db: f64,
    min_secs: f64,
    keep_secs: f64,
) -> Result<Vec<(Time, Time)>> {
    require_file(path)?;
    if !keep_secs.is_finite() || keep_secs < 0.0 {
        return Err(Error::invalid("keep_secs must not be negative"));
    }
    let spans = crate::media::detect_silence(path, noise_db, min_secs)?;
    let total = crate::media::probe(path)
        .map(|info| info.duration)
        .unwrap_or_else(|_| spans.last().map(|s| s.end).unwrap_or(Time::ZERO));
    let keep = Time::from_secs_f64(keep_secs);
    Ok(keep_ranges(&spans, total, keep))
}

/// Turn silence spans into the ranges worth keeping: every silence is trimmed by
/// the lead-in/lead-out that should survive, and the gaps become cut points.
fn keep_ranges(spans: &[crate::media::Span], total: Time, keep: Time) -> Vec<(Time, Time)> {
    let mut out: Vec<(Time, Time)> = Vec::new();
    let mut cursor = Time::ZERO;
    for span in spans {
        let cut_start = (span.start + keep).min(span.end);
        let cut_end = (span.end - keep).max(span.start);
        if cut_end <= cut_start {
            continue;
        }
        if cut_start > cursor {
            out.push((cursor, cut_start));
        }
        cursor = cursor.max(cut_end);
    }
    if total > cursor {
        out.push((cursor, total));
    }
    out.into_iter().filter(|(a, b)| b > a).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::media::Span;

    fn span(start: f64, end: f64) -> Span {
        Span {
            start: Time::from_secs_f64(start),
            end: Time::from_secs_f64(end),
            detail: String::new(),
        }
    }

    #[test]
    fn keeps_lead_in_around_silence() {
        let spans = vec![span(2.0, 4.0)];
        let ranges = keep_ranges(&spans, Time::from_secs_f64(6.0), Time::from_secs_f64(0.5));
        assert_eq!(ranges.len(), 2);
        assert_eq!(ranges[0], (Time::ZERO, Time::from_secs_f64(2.5)));
        assert_eq!(
            ranges[1],
            (Time::from_secs_f64(3.5), Time::from_secs_f64(6.0))
        );
    }

    #[test]
    fn drops_silence_shorter_than_the_keep_window() {
        let spans = vec![span(1.0, 1.2)];
        let ranges = keep_ranges(&spans, Time::from_secs_f64(3.0), Time::from_secs_f64(0.5));
        assert_eq!(ranges, vec![(Time::ZERO, Time::from_secs_f64(3.0))]);
    }

    #[test]
    fn merged_silence_never_overlaps() {
        let spans = vec![span(1.0, 2.0), span(1.5, 2.5), span(2.2, 2.4)];
        let ranges = keep_ranges(&spans, Time::from_secs_f64(4.0), Time::from_secs_f64(0.1));
        let mut previous_end = Time::ZERO;
        for (start, end) in &ranges {
            assert!(end > start);
            assert!(*start >= previous_end, "ranges must not overlap");
            previous_end = *end;
        }
    }

    #[test]
    fn whole_file_silent_keeps_only_the_lead_windows() {
        let spans = vec![span(0.0, 5.0)];
        let keep = Time::from_secs_f64(0.1);
        let ranges = keep_ranges(&spans, Time::from_secs_f64(5.0), keep);
        assert_eq!(
            ranges,
            vec![
                (Time::ZERO, Time::from_secs_f64(0.1)),
                (Time::from_secs_f64(4.9), Time::from_secs_f64(5.0))
            ]
        );
        let kept: f64 = ranges.iter().map(|(a, b)| (*b - *a).as_secs_f64()).sum();
        assert!(
            kept <= 0.2 + 1e-9,
            "only the two keep windows survive, kept {kept}"
        );
    }

    #[test]
    fn parses_loudnorm_numbers() {
        assert_eq!(loudness_value("-23.02"), -23.02);
        assert_eq!(loudness_value("-inf"), SILENCE_FLOOR_DB);
        let text = "noise\n{\n\t\"input_i\" : \"-21.80\",\n\t\"input_tp\" : \"-16.08\",\n\t\"input_lra\" : \"0.10\",\n\t\"output_i\" : \"-23.99\"\n}\ntrailing\n";
        let block = json_block(text).unwrap();
        assert_eq!(
            block.get("input_i").and_then(|v| v.as_str()),
            Some("-21.80")
        );
        assert_eq!(
            block.get("input_lra").and_then(|v| v.as_str()),
            Some("0.10")
        );
    }
}
