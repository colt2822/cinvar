//! Per-Short output QA: dimensions, duration and stream presence, measured with the
//! repository's ffprobe wrapper (crate::qa). Nothing is inferred from the request.

use std::path::Path;

use crate::shorts::vertical::{OUT_HEIGHT, OUT_WIDTH};

/// Duration slack allowed for container rounding and AAC priming.
pub const DURATION_TOLERANCE_SECS: f64 = 0.6;
/// A file smaller than this cannot be a usable Short.
pub const MIN_BYTES: u64 = 4096;

/// The QA verdict recorded on a Short.
#[derive(Debug, Clone, PartialEq)]
pub struct Verdict {
    pub passed: bool,
    pub width: u32,
    pub height: u32,
    /// Measured duration in seconds, 0.0 when it could not be read.
    pub duration: f64,
    pub detail: String,
}

/// Measure one rendered Short against the request.
///
/// expected: the length the Short was cut to; min/max: the requested bounds.
/// wants_audio: whether the source carried audio (an audio stream is then required).
pub fn check(
    path: &Path,
    expected: f64,
    min_duration: f64,
    max_duration: f64,
    wants_audio: bool,
) -> Verdict {
    let bytes = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
    if bytes < MIN_BYTES {
        return Verdict {
            passed: false,
            width: 0,
            height: 0,
            duration: 0.0,
            detail: format!("measured {bytes} bytes at {} (need >= {MIN_BYTES})", path.display()),
        };
    }
    let probe = match crate::qa::probe::probe(path) {
        Ok(probe) => probe,
        Err(err) => {
            return Verdict {
                passed: false,
                width: 0,
                height: 0,
                duration: 0.0,
                detail: format!("the output is not readable media: {}", crate::secrets::redact(&err.message)),
            }
        }
    };
    let duration = probe.duration.as_secs_f64();
    let tol = (expected * 0.02).max(DURATION_TOLERANCE_SECS);
    let mut problems: Vec<String> = Vec::new();
    if !probe.has_video {
        problems.push("no video stream".to_string());
    }
    if probe.width != OUT_WIDTH || probe.height != OUT_HEIGHT {
        problems.push(format!(
            "frame is {}x{}, expected {OUT_WIDTH}x{OUT_HEIGHT}",
            probe.width, probe.height
        ));
    }
    if wants_audio && !probe.has_audio {
        problems.push("no audio stream, but the source has audio".to_string());
    }
    if (duration - expected).abs() > tol {
        problems.push(format!("duration {duration:.3}s misses the cut length {expected:.3}s by more than {tol:.2}s"));
    }
    if duration < min_duration - tol || duration > max_duration + tol {
        problems.push(format!(
            "duration {duration:.3}s is outside the requested {min_duration:.2}-{max_duration:.2}s"
        ));
    }
    let passed = problems.is_empty();
    let detail = if passed {
        format!(
            "measured {}x{} {duration:.3}s, video={} audio={}",
            probe.width, probe.height, probe.has_video, probe.has_audio
        )
    } else {
        problems.join("; ")
    };
    Verdict {
        passed,
        width: probe.width,
        height: probe.height,
        duration,
        detail,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_missing_file_never_passes() {
        let verdict = check(Path::new("/nonexistent/short_01.mp4"), 10.0, 5.0, 30.0, true);
        assert!(!verdict.passed);
        assert_eq!(verdict.width, 0);
        assert!(verdict.detail.contains("bytes"), "{}", verdict.detail);
    }
}
