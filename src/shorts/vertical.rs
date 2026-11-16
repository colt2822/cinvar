//! 9:16 vertical delivery for one Short.
//!
//! Two stages, both existing engines:
//!   1. cut the selected source range (ffmpeg, frame-accurate re-encode);
//!   2. reframe it to 1080x1920 with crate::reframe's subject tracker when that helper is
//!      configured, and otherwise with a deterministic centre cover-crop.
//!
//! The tracker needs CINVAR_TRACK_PYTHON (torch + torchvision). When it is absent, or when
//! it finds no subject, the centre crop is used and the caller is told which one ran: a
//! Short is still delivered, and nothing pretends the subject was tracked.

use std::path::Path;

use crate::error::{Error, Result};
use crate::secrets;
use crate::util::proc::{self, ProcOpts};

/// Output canvas of every Short.
pub const OUT_WIDTH: u32 = 1080;
pub const OUT_HEIGHT: u32 = 1920;
const TIMEOUT_SECS: u64 = 3600;

/// Which reframe path produced the vertical master.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VerticalMode {
    /// crate::reframe tracked a person and cropped around them.
    SubjectTracked,
    /// Centre cover-crop (no tracker configured, or no subject found).
    CentreCrop,
}

impl VerticalMode {
    pub fn label(self) -> &'static str {
        match self {
            VerticalMode::SubjectTracked => "subject-tracking reframe",
            VerticalMode::CentreCrop => "centre cover-crop",
        }
    }
}

/// Is a subject tracker configured on this machine?
pub fn tracker_configured() -> bool {
    std::env::var(crate::reframe::ENV_PYTHON)
        .map(|p| !p.trim().is_empty() && Path::new(&p).is_file())
        .unwrap_or(false)
}

fn ffmpeg() -> Result<std::path::PathBuf> {
    proc::which("ffmpeg").ok_or_else(|| Error::invalid("ffmpeg not found on PATH"))
}

fn run(args: Vec<String>, what: &str) -> Result<()> {
    let bin = ffmpeg()?;
    let out = proc::run(&bin, &args, &ProcOpts::default().with_timeout(TIMEOUT_SECS))?;
    if !out.success {
        return Err(Error::new(
            crate::error::ErrorCode::Render,
            format!("{what} failed: {}", secrets::redact(&out.tail_stderr(4))),
        ));
    }
    Ok(())
}

/// Cut [start, end) out of source into out, keeping the source frame size and audio.
pub fn cut_range(source: &Path, start: f64, end: f64, out: &Path) -> Result<()> {
    if !(end > start) {
        return Err(Error::invalid("the Short's source range is empty"));
    }
    let duration = end - start;
    let args: Vec<String> = vec![
        "-hide_banner".into(),
        "-loglevel".into(),
        "error".into(),
        "-y".into(),
        // Before -i is the fast input seek; with a re-encode ffmpeg still decodes and drops
        // up to the exact requested frame, so the cut is accurate.
        "-ss".into(),
        format!("{start:.4}"),
        "-i".into(),
        source.to_string_lossy().to_string(),
        "-t".into(),
        format!("{duration:.4}"),
        "-map".into(),
        "0:v:0".into(),
        "-map".into(),
        "0:a:0?".into(),
        "-c:v".into(),
        "libx264".into(),
        "-preset".into(),
        "medium".into(),
        "-crf".into(),
        "16".into(),
        "-pix_fmt".into(),
        "yuv420p".into(),
        "-c:a".into(),
        "aac".into(),
        "-b:a".into(),
        "192k".into(),
        "-ar".into(),
        "48000".into(),
        out.to_string_lossy().to_string(),
    ];
    run(args, "cutting the Short range")
}

/// Deterministic centre cover-crop to 1080x1920 (fills the frame, crops what does not fit).
fn centre_crop(input: &Path, out: &Path) -> Result<()> {
    let filter = format!(
        "scale={OUT_WIDTH}:{OUT_HEIGHT}:force_original_aspect_ratio=increase,crop={OUT_WIDTH}:{OUT_HEIGHT},setsar=1"
    );
    let args: Vec<String> = vec![
        "-hide_banner".into(),
        "-loglevel".into(),
        "error".into(),
        "-y".into(),
        "-i".into(),
        input.to_string_lossy().to_string(),
        "-vf".into(),
        filter,
        "-c:v".into(),
        "libx264".into(),
        "-preset".into(),
        "medium".into(),
        "-crf".into(),
        "16".into(),
        "-pix_fmt".into(),
        "yuv420p".into(),
        "-c:a".into(),
        "copy".into(),
        out.to_string_lossy().to_string(),
    ];
    run(args, "the centre cover-crop reframe")
}

/// Reframe an already-cut clip to 1080x1920.
pub fn to_vertical(input: &Path, out: &Path, work: &Path) -> Result<VerticalMode> {
    if tracker_configured() {
        match crate::reframe::reframe(input, out, "vertical", work, 2.0) {
            Ok(_) => return Ok(VerticalMode::SubjectTracked),
            Err(err) => {
                // A tracker that cannot run (no person in frame, helper crash) must not lose
                // the Short: fall through to the deterministic crop.
                let _ = err;
            }
        }
    }
    centre_crop(input, out)?;
    Ok(VerticalMode::CentreCrop)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mode_labels_are_honest() {
        assert!(VerticalMode::SubjectTracked.label().contains("tracking"));
        assert!(VerticalMode::CentreCrop.label().contains("crop"));
    }

    #[test]
    fn an_empty_range_is_refused_before_ffmpeg_runs() {
        let err = cut_range(Path::new("/nonexistent.mp4"), 4.0, 4.0, Path::new("/tmp/x.mp4"))
            .unwrap_err();
        assert_eq!(err.code, crate::error::ErrorCode::InvalidInput);
    }
}
