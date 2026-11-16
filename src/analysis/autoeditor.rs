//! Silence-aware pacing through Auto-Editor (github.com/WyattBlue/auto-editor), run in an
//! external Python environment named by `CINVAR_AUTOEDITOR_PYTHON` (a venv with
//! `auto-editor` installed; the PATH `auto-editor` binary works too). The adapter exports
//! Auto-Editor's `v1` plan (keep/cut chunks), converts it to keep-spans in seconds via the
//! source fps, and hands it to the autoedit planner as an additive pacing pass — the
//! default pipeline is unchanged when the flag is off.

use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::error::{Error, Result};
use crate::secrets;
use crate::util::proc::{self, ProcOpts};

pub const ENV_PYTHON: &str = "CINVAR_AUTOEDITOR_PYTHON";
const TIMEOUT_SECS: u64 = 3600;
/// Auto-Editor marks cut chunks with this speed value.
pub const CUT_SPEED: f64 = 99999.0;

#[derive(Debug, Clone, PartialEq)]
pub struct KeepSpan {
    pub start: f64,
    pub end: f64,
}

#[derive(Deserialize)]
struct V1Plan {
    chunks: Vec<(f64, f64, f64)>,
}

pub fn tool_available() -> bool {
    std::env::var(ENV_PYTHON).map(|p| PathBuf::from(p).is_file()).unwrap_or(false)
        || proc::which("auto-editor").is_some()
}

fn command_for(video: &Path, work_dir: &Path) -> Result<(PathBuf, Vec<String>)> {
    let plan_path = work_dir.join("autoeditor-plan.json");
    let args = vec![
        video.to_string_lossy().to_string(),
        "--export".to_string(),
        "v1".to_string(),
        "-o".to_string(),
        plan_path.to_string_lossy().to_string(),
    ];
    if let Ok(py) = std::env::var(ENV_PYTHON) {
        let py = PathBuf::from(py);
        if !py.is_file() {
            return Err(Error::invalid(format!(
                "{ENV_PYTHON} does not point to a python binary: {}",
                py.display()
            )));
        }
        let mut a = vec!["-I".to_string(), "-m".to_string(), "auto_editor".to_string()];
        a.extend(args);
        Ok((py, a))
    } else if let Some(bin) = proc::which("auto-editor") {
        Ok((bin, args))
    } else {
        Err(Error::invalid("Auto-Editor is not configured").with_hint(format!(
            "install it into a venv and set {ENV_PYTHON}, or put `auto-editor` on PATH"
        )))
    }
}

/// Runs Auto-Editor's `--export v1` on `video` and returns the keep-spans (speed 1.0
/// chunks) in seconds. Cut (speed 99999) chunks are dropped.
pub fn keep_spans(video: &Path, work_dir: &Path) -> Result<Vec<KeepSpan>> {
    std::fs::create_dir_all(work_dir)?;
    let (program, args) = command_for(video, work_dir)?;
    let out = proc::run(&program, &args, &ProcOpts::default().with_timeout(TIMEOUT_SECS))?;
    let plan_path = work_dir.join("autoeditor-plan.json");
    if !plan_path.is_file() {
        return Err(Error::internal(format!(
            "auto-editor produced no plan: {}",
            secrets::redact(&out.tail_stderr(3))
        )));
    }
    let text = std::fs::read_to_string(&plan_path)?;
    parse_plan(&text, video)
}

fn parse_plan(text: &str, video: &Path) -> Result<Vec<KeepSpan>> {
    let plan: V1Plan = serde_json::from_str(text)
        .map_err(|e| Error::internal(format!("auto-editor plan unreadable: {e}")))?;
    let fps = source_fps(video)?;
    if fps <= 0.0 {
        return Err(Error::internal("source reported a non-positive fps"));
    }
    let mut spans = Vec::new();
    for (start, end, speed) in plan.chunks {
        if (speed - CUT_SPEED).abs() < f64::EPSILON {
            continue;
        }
        let start = start / fps;
        let end = end / fps;
        if end > start {
            spans.push(KeepSpan { start, end });
        }
    }
    if spans.is_empty() {
        return Err(Error::invalid(
            "auto-editor's plan kept nothing (fully silent source?)",
        ));
    }
    Ok(spans)
}

fn source_fps(video: &Path) -> Result<f64> {
    let ffprobe =
        proc::which("ffprobe").ok_or_else(|| Error::invalid("ffprobe not found on PATH"))?;
    let out = proc::run(
        &ffprobe,
        &[
            "-v".into(),
            "error".into(),
            "-select_streams".into(),
            "v:0".into(),
            "-show_entries".into(),
            "stream=avg_frame_rate".into(),
            "-of".into(),
            "csv=p=0".into(),
            video.to_string_lossy().to_string(),
        ],
        &ProcOpts::default(),
    )?;
    let rate = out.stdout.trim();
    let (n, d) = rate
        .split_once('/')
        .ok_or_else(|| Error::internal(format!("bad frame rate '{rate}'")))?;
    let n: f64 = n.parse().map_err(|_| Error::internal("bad frame rate"))?;
    let d: f64 = d.parse().map_err(|_| Error::internal("bad frame rate"))?;
    Ok(n / d)
}

/// Intersects each selection with the keep-spans and drops fully-cut windows.
/// Windows are kept whole when overlap is above `min_part` seconds (avoid slivers).
pub fn pace_selections(
    selections: Vec<crate::analysis::autoedit::Selection>,
    spans: &[KeepSpan],
    min_part: f64,
) -> Vec<crate::analysis::autoedit::Selection> {
    let mut out = Vec::new();
    for sel in selections {
        for span in spans {
            let s = sel.start.max(span.start);
            let e = sel.end.min(span.end);
            if e - s >= min_part {
                let mut part = sel.clone();
                part.start = s;
                part.end = e;
                out.push(part);
            }
        }
        // selection fully inside a cut span → dropped
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_v1_chunks() {
        let text = r#"{"version":"1","source":"x.mp4","chunks":[[0,246,1.0],[246,354,99999.0],[354,546,1.0]]}"#;
        let fps = 30.0;
        let plan: V1Plan = serde_json::from_str(text).unwrap();
        let mut spans = Vec::new();
        for (s, e, sp) in &plan.chunks {
            if (*sp - CUT_SPEED).abs() >= f64::EPSILON {
                spans.push(KeepSpan { start: s / fps, end: e / fps });
            }
        }
        assert_eq!(spans.len(), 2);
        assert!((spans[0].end - 246.0 / 30.0).abs() < 1e-9);
    }

    #[test]
    fn pacing_drops_cut_windows_and_splits_keeps() {
        let mk = |s: f64, e: f64| crate::analysis::autoedit::Selection {
            asset_id: "a".into(),
            file_name: "f.mp4".into(),
            start: s,
            end: e,
            score: 1.0,
            reason: "test".into(),
        };
        let spans = vec![
            KeepSpan { start: 0.0, end: 8.0 },
            KeepSpan { start: 12.0, end: 22.0 },
        ];
        let paced = pace_selections(
            vec![mk(2.0, 6.0), mk(9.0, 11.0), mk(14.0, 20.0)],
            &spans,
            1.0,
        );
        assert_eq!(paced.len(), 2);
        assert!((paced[0].start - 2.0).abs() < 1e-9 && (paced[0].end - 6.0).abs() < 1e-9);
        assert!((paced[1].start - 14.0).abs() < 1e-9 && (paced[1].end - 20.0).abs() < 1e-9);
    }

    #[test]
    fn pacing_respects_min_part() {
        let mk = |s: f64, e: f64| crate::analysis::autoedit::Selection {
            asset_id: "a".into(),
            file_name: "f.mp4".into(),
            start: s,
            end: e,
            score: 1.0,
            reason: "test".into(),
        };
        let spans = vec![KeepSpan { start: 0.0, end: 5.0 }];
        // overlap 0.5 s < min_part 1.0 → dropped
        let paced = pace_selections(vec![mk(4.5, 10.0)], &spans, 1.0);
        assert!(paced.is_empty());
    }
}
