//! Frame and final-media validation for distributed rendering.
//!
//! File existence is not completion. A frame only counts when it is a complete PNG (8-byte signature,
//! IEND chunk last), which is the same test src/blender/mod.rs applies locally before resuming; it is
//! repeated here because frames collected from a worker must be validated *before* they are placed in
//! the job directory, not after. The delivered clip is then measured with ffprobe instead of being
//! assumed to match the request.

use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::error::{Error, ErrorCode, Result};
use crate::util::proc::{self, ProcOpts};

const PNG_SIGNATURE: [u8; 8] = [0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];

/// File name of one frame in a job's frames directory (4-digit padding, as Blender writes it).
pub fn frame_path(frames_dir: &Path, frame: u32) -> PathBuf {
    frames_dir.join(format!("frame_{frame:04}.png"))
}

/// True when these bytes are a complete PNG. Collected frames are checked in memory, before they
/// are allowed anywhere near the job directory.
pub fn png_complete_bytes(bytes: &[u8]) -> bool {
    if bytes.len() < 20 || bytes[..8] != PNG_SIGNATURE {
        return false;
    }
    &bytes[bytes.len() - 8..bytes.len() - 4] == b"IEND"
}

/// SHA-256 of every frame in the range, in order. Two independent renders of the same scene have to
/// agree here, which is how reassembly is proven to be in the right order and complete.
pub fn frame_digests(frames_dir: &Path, start: u32, end: u32) -> Result<Vec<(u32, String)>> {
    use sha2::{Digest, Sha256};
    let mut out = Vec::new();
    for frame in start..=end {
        let path = frame_path(frames_dir, frame);
        let bytes = std::fs::read(&path).map_err(|e| {
            Error::new(
                ErrorCode::Render,
                format!("frame {frame} is not readable ({}): {e}", path.display()),
            )
        })?;
        if !png_complete_bytes(&bytes) {
            return Err(Error::new(
                ErrorCode::Render,
                format!("frame {frame} is not a complete PNG"),
            ));
        }
        let digest = Sha256::digest(&bytes);
        out.push((
            frame,
            digest.iter().map(|b| format!("{b:02x}")).collect::<String>(),
        ));
    }
    Ok(out)
}

/// True when the file is a complete PNG: valid signature and an IEND chunk in the last 12 bytes.
pub fn png_complete(path: &Path) -> bool {
    use std::io::{Read, Seek, SeekFrom};
    let Ok(mut file) = std::fs::File::open(path) else {
        return false;
    };
    let mut head = [0u8; 8];
    if file.read_exact(&mut head).is_err() || head != PNG_SIGNATURE {
        return false;
    }
    let mut tail = [0u8; 12];
    if file.seek(SeekFrom::End(-12)).is_err() || file.read_exact(&mut tail).is_err() {
        return false;
    }
    &tail[4..8] == b"IEND"
}

/// Number of complete frames present in a frames directory.
pub fn count_valid_frames(frames_dir: &Path) -> usize {
    let Ok(entries) = std::fs::read_dir(frames_dir) else {
        return 0;
    };
    entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().and_then(|x| x.to_str()) == Some("png"))
        .filter(|p| png_complete(p))
        .count()
}

/// Frames in start..=end that are missing or incomplete on disk.
pub fn missing_frames(frames_dir: &Path, start: u32, end: u32) -> Vec<u32> {
    (start..=end)
        .filter(|frame| !png_complete(&frame_path(frames_dir, *frame)))
        .collect()
}

/// One measured property of the delivered clip.
#[derive(Debug, Clone, Serialize)]
pub struct MediaCheck {
    pub clip: PathBuf,
    pub codec: String,
    pub width: u32,
    pub height: u32,
    pub frames: u64,
    pub duration: f64,
    /// Every requirement the file was measured against, and how it went.
    pub findings: Vec<String>,
}

impl MediaCheck {
    pub fn ok(&self) -> bool {
        self.findings.is_empty()
    }
}

fn probe_json(clip: &Path) -> Result<String> {
    let ffprobe = proc::require("ffprobe")?;
    let args: Vec<String> = [
        "-v",
        "error",
        "-select_streams",
        "v:0",
        "-count_frames",
        "-show_entries",
        "stream=codec_name,width,height,nb_read_frames,nb_frames,avg_frame_rate",
        "-show_entries",
        "format=duration",
        "-of",
        "json",
    ]
    .iter()
    .map(|s| s.to_string())
    .chain(std::iter::once(clip.to_string_lossy().to_string()))
    .collect();
    let out = proc::run(&ffprobe, &args, &ProcOpts::default().with_timeout(120))?;
    if !out.success {
        return Err(Error::new(
            ErrorCode::Render,
            format!(
                "ffprobe could not read {}: {}",
                clip.display(),
                out.tail_stderr(6)
            ),
        ));
    }
    Ok(out.stdout)
}

#[derive(Debug, Clone, Copy)]
pub struct MediaExpect {
    pub frames: u64,
    pub width: u32,
    pub height: u32,
    pub fps: u32,
}

/// Measures the reassembled clip with ffprobe and compares it with the request.
pub fn verify_media(clip: &Path, expect: MediaExpect) -> Result<MediaCheck> {
    let json = probe_json(clip)?;
    parse_media(&json, clip, expect)
}

/// Pure part of [verify_media], so the comparison logic is testable without ffmpeg.
pub fn parse_media(json: &str, clip: &Path, expect: MediaExpect) -> Result<MediaCheck> {
    let value: serde_json::Value = serde_json::from_str(json).map_err(|e| {
        Error::new(ErrorCode::Protocol, format!("unreadable ffprobe report: {e}"))
    })?;
    let stream = value
        .get("streams")
        .and_then(|s| s.as_array())
        .and_then(|s| s.first())
        .ok_or_else(|| {
            Error::new(
                ErrorCode::Render,
                format!("{} has no video stream", clip.display()),
            )
        })?;
    let number = |key: &str| stream.get(key).and_then(|v| v.as_u64()).unwrap_or(0);
    let codec = stream
        .get("codec_name")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown")
        .to_string();
    let frames = stream
        .get("nb_read_frames")
        .and_then(|v| v.as_str())
        .and_then(|v| v.parse::<u64>().ok())
        .or_else(|| stream.get("nb_frames").and_then(|v| v.as_str()).and_then(|v| v.parse::<u64>().ok()))
        .unwrap_or(0);
    let duration = value
        .get("format")
        .and_then(|f| f.get("duration"))
        .and_then(|v| v.as_str())
        .and_then(|v| v.parse::<f64>().ok())
        .unwrap_or(0.0);

    let mut findings = Vec::new();
    if frames != expect.frames {
        findings.push(format!(
            "frame count is {frames}, expected {}",
            expect.frames
        ));
    }
    if (number("width"), number("height")) != (expect.width as u64, expect.height as u64) {
        findings.push(format!(
            "resolution is {}x{}, expected {}x{}",
            number("width"),
            number("height"),
            expect.width,
            expect.height
        ));
    }
    let wanted = expect.frames as f64 / expect.fps.max(1) as f64;
    if (duration - wanted).abs() > (0.5 / expect.fps.max(1) as f64).max(0.05) {
        findings.push(format!(
            "duration is {duration:.3}s, expected ~{wanted:.3}s for {} frames at {} fps",
            expect.frames, expect.fps
        ));
    }

    Ok(MediaCheck {
        clip: clip.to_path_buf(),
        codec,
        width: number("width") as u32,
        height: number("height") as u32,
        frames,
        duration,
        findings,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_png(path: &Path, body_len: usize, iend: bool) {
        let mut bytes = PNG_SIGNATURE.to_vec();
        bytes.extend(std::iter::repeat(0x5A).take(body_len));
        if iend {
            // length(0) + type IEND + crc: the last 12 bytes the checker inspects.
            bytes.extend_from_slice(&[0, 0, 0, 0, b'I', b'E', b'N', b'D', 0xAE, 0x42, 0x60, 0x82]);
        }
        std::fs::write(path, bytes).unwrap();
    }

    #[test]
    fn png_completeness_rejects_truncation() {
        let dir = tempfile::tempdir().unwrap();
        let good = dir.path().join("frame_0001.png");
        write_png(&good, 100, true);
        assert!(png_complete(&good));

        // Blender killed mid-frame: header written, no IEND.
        let bad = dir.path().join("frame_0002.png");
        write_png(&bad, 5000, false);
        assert!(!png_complete(&bad), "a frame without IEND is not complete");

        // A .part / empty file is not complete either.
        let empty = dir.path().join("frame_0003.png");
        std::fs::write(&empty, b"").unwrap();
        assert!(!png_complete(&empty));
        assert!(!png_complete(&dir.path().join("does-not-exist.png")));

        // Not a PNG at all, even though it is named like one.
        let wrong = dir.path().join("frame_0004.png");
        std::fs::write(&wrong, b"<html>error</html>").unwrap();
        assert!(!png_complete(&wrong));
    }

    #[test]
    fn counts_and_gaps() {
        let dir = tempfile::tempdir().unwrap();
        write_png(&dir.path().join("frame_0001.png"), 10, true);
        write_png(&dir.path().join("frame_0002.png"), 10, false); // incomplete: must be rejected
        write_png(&dir.path().join("frame_0004.png"), 10, true);
        assert_eq!(count_valid_frames(dir.path()), 2);
        assert_eq!(missing_frames(dir.path(), 1, 4), vec![2, 3]);
        assert_eq!(frame_path(dir.path(), 42).file_name().unwrap(), "frame_0042.png");
    }

    /// Captured from ffprobe 6.1.1 on a 24-frame 960x540 h264 clip.
    const CAPTURED_FFPROBE: &str = r#"{
    "streams": [
        {
            "codec_name": "h264",
            "width": 960,
            "height": 540,
            "avg_frame_rate": "30/1",
            "nb_frames": "24",
            "nb_read_frames": "24"
        }
    ],
    "format": {
        "duration": "0.800000"
    }
}"#;

    #[test]
    fn ffprobe_report_is_measured_against_the_request() {
        let expect = MediaExpect { frames: 24, width: 960, height: 540, fps: 30 };
        let check = parse_media(CAPTURED_FFPROBE, Path::new("clip.mp4"), expect).unwrap();
        assert!(check.ok(), "unexpected findings: {:?}", check.findings);
        assert_eq!(check.codec, "h264");
        assert_eq!((check.width, check.height, check.frames), (960, 540, 24));
        assert!((check.duration - 0.8).abs() < 1e-6);
    }

    #[test]
    fn short_or_wrong_sized_clip_is_reported() {
        let expect = MediaExpect { frames: 24, width: 1920, height: 1080, fps: 30 };
        let check = parse_media(CAPTURED_FFPROBE, Path::new("clip.mp4"), expect).unwrap();
        assert!(!check.ok());
        assert!(check.findings.iter().any(|f| f.contains("frame count is 24, expected 24")) == false);
        assert!(check.findings.iter().any(|f| f.contains("resolution is 960x540")));
    }

    #[test]
    fn streams_are_required() {
        let expect = MediaExpect { frames: 1, width: 2, height: 2, fps: 30 };
        let err = parse_media("{\"streams\": []}", Path::new("x.mp4"), expect).unwrap_err();
        assert_eq!(err.code, ErrorCode::Render);
    }
}
