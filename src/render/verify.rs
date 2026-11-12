//! Deep output verification: dimensions, exact frame count, monotonic timestamps and a clean
//! full decode. Used as acceptance evidence for large (8K) exports.

use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::error::{Error, ErrorCode, Result};
use crate::util::proc::{self, ProcOpts};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeepReport {
    pub width: u32,
    pub height: u32,
    pub codec: String,
    pub pix_fmt: String,
    pub frames_counted: u64,
    pub first_pts: f64,
    pub last_pts: f64,
    pub pts_strictly_increasing: bool,
    pub decode_errors: Vec<String>,
    pub issues: Vec<String>,
    pub passed: bool,
}

/// Verifies `path` by decoding every frame. `expect_frames` makes the frame count exact.
pub fn deep_verify(
    path: &Path,
    expect_size: Option<(u32, u32)>,
    expect_frames: Option<u64>,
) -> Result<DeepReport> {
    let ffprobe = proc::require("ffprobe")?;
    let ffmpeg = proc::require("ffmpeg")?;
    let p = path.to_string_lossy().to_string();

    let probe = proc::run(
        &ffprobe,
        &[
            "-v",
            "error",
            "-select_streams",
            "v:0",
            "-show_entries",
            "stream=codec_name,width,height,pix_fmt",
            "-of",
            "json",
            &p,
        ]
        .map(String::from),
        &ProcOpts::default().with_timeout(120),
    )?;
    if !probe.success {
        return Err(Error::new(
            ErrorCode::Render,
            format!("ffprobe failed: {}", probe.tail_stderr(3)),
        ));
    }
    let meta: serde_json::Value = serde_json::from_str(&probe.stdout)
        .map_err(|e| Error::new(ErrorCode::Render, format!("ffprobe output unreadable: {e}")))?;
    let stream = meta["streams"][0].clone();
    let width = stream["width"].as_u64().unwrap_or(0) as u32;
    let height = stream["height"].as_u64().unwrap_or(0) as u32;
    let codec = stream["codec_name"].as_str().unwrap_or("").to_string();
    let pix_fmt = stream["pix_fmt"].as_str().unwrap_or("").to_string();

    // One timestamp per decoded frame (-show_frames decodes), giving count and monotonicity.
    let frames = proc::run(
        &ffprobe,
        &[
            "-v",
            "error",
            "-select_streams",
            "v:0",
            "-show_entries",
            "frame=best_effort_timestamp_time",
            "-of",
            "csv=p=0",
            &p,
        ]
        .map(String::from),
        &ProcOpts::default().with_timeout(1800),
    )?;
    let mut stamps: Vec<f64> = Vec::new();
    for line in frames.stdout.lines() {
        if let Ok(v) = line.trim().trim_end_matches(',').parse::<f64>() {
            stamps.push(v);
        }
    }
    let increasing = stamps.windows(2).all(|w| w[1] > w[0]);

    let decode = proc::run(
        &ffmpeg,
        &["-v", "error", "-nostdin", "-i", &p, "-f", "null", "-"].map(String::from),
        &ProcOpts::default().with_timeout(1800),
    )?;
    let decode_errors: Vec<String> = decode
        .stderr
        .lines()
        .map(|l| l.trim().to_string())
        .filter(|l| !l.is_empty())
        .collect();

    let mut issues = Vec::new();
    if let Some((w, h)) = expect_size {
        if (width, height) != (w, h) {
            issues.push(format!("dimensions {width}x{height}, expected {w}x{h}"));
        }
    }
    if let Some(n) = expect_frames {
        if stamps.len() as u64 != n {
            issues.push(format!("{} frames decoded, expected {n}", stamps.len()));
        }
    }
    if stamps.is_empty() {
        issues.push("no video frames decoded".to_string());
    }
    if !increasing {
        issues.push("frame timestamps are not strictly increasing".to_string());
    }
    if !decode.success || !decode_errors.is_empty() {
        issues.push(format!(
            "full decode reported {} error line(s)",
            decode_errors.len()
        ));
    }
    Ok(DeepReport {
        width,
        height,
        codec,
        pix_fmt,
        frames_counted: stamps.len() as u64,
        first_pts: stamps.first().copied().unwrap_or(0.0),
        last_pts: stamps.last().copied().unwrap_or(0.0),
        pts_strictly_increasing: increasing,
        decode_errors,
        passed: issues.is_empty(),
        issues,
    })
}
