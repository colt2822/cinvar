//! Scene detection through PySceneDetect, run in an external Python environment named by
//! `CINVAR_SCENEDETECT_PYTHON` (a venv python with `scenedetect==0.7.3` installed). Nothing is
//! bundled or downloaded: when the variable is unset, the python is missing, or the helper
//! fails, the error says so and names the fix.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::{Error, ErrorCode, Result};
use crate::secrets;
use crate::util::proc::{self, ProcOpts};

/// Embedded copy of the helper so a bare binary can still run it (same pattern as captions).
const HELPER: &str = include_str!("scenedetect_helper.py");
pub const ENV_PYTHON: &str = "CINVAR_SCENEDETECT_PYTHON";
const TIMEOUT_SECS: u64 = 1800;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Detector {
    /// `ContentDetector`: frame-to-frame HSV difference against a fixed threshold (default 27).
    Content,
    /// `AdaptiveDetector`: content score relative to a rolling average (default 3.0).
    Adaptive,
}

impl Detector {
    pub fn parse(raw: &str) -> Result<Self> {
        match raw {
            "content" => Ok(Detector::Content),
            "adaptive" => Ok(Detector::Adaptive),
            other => Err(Error::invalid(format!(
                "unknown detector '{other}' (use content or adaptive)"
            ))),
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Detector::Content => "content",
            Detector::Adaptive => "adaptive",
        }
    }
}

/// One scene as emitted by the helper. `end_*` is exclusive (PySceneDetect's convention).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Scene {
    pub start_frame: u64,
    pub start_sec: f64,
    pub end_frame: u64,
    pub end_sec: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SceneReport {
    pub detector: String,
    #[serde(default)]
    pub threshold: Option<f64>,
    pub fps: f64,
    pub width: u32,
    pub height: u32,
    pub scenes: Vec<Scene>,
}

/// Maps the env value to a python path. Pure so it can be tested without touching the process env.
fn python_from_value(value: Option<&str>) -> Result<PathBuf> {
    let raw = value.map(str::trim).unwrap_or("");
    if raw.is_empty() {
        return Err(Error::new(
            ErrorCode::DependencyMissing,
            format!("{ENV_PYTHON} is not set; PySceneDetect scene detection is unavailable"),
        )
        .with_hint(
            "create a venv, `pip install scenedetect==0.7.3`, then set CINVAR_SCENEDETECT_PYTHON to its python",
        ));
    }
    let path = PathBuf::from(raw);
    if !path.is_file() {
        return Err(Error::new(
            ErrorCode::DependencyMissing,
            format!("{ENV_PYTHON} points to {raw}, which is not a file"),
        )
        .with_hint("set CINVAR_SCENEDETECT_PYTHON to the python inside your scenedetect venv"));
    }
    Ok(path)
}

pub fn python_from_env() -> Result<PathBuf> {
    python_from_value(std::env::var(ENV_PYTHON).ok().as_deref())
}

/// Parses and validates the helper's stdout JSON.
pub fn parse_report(text: &str) -> Result<SceneReport> {
    let report: SceneReport = serde_json::from_str(text.trim()).map_err(|e| {
        Error::internal(format!("scenedetect helper returned invalid JSON: {e}"))
    })?;
    if !(report.fps.is_finite() && report.fps > 0.0) {
        return Err(Error::internal("scenedetect helper reported a non-positive fps"));
    }
    let mut prev_start = f64::NEG_INFINITY;
    for (i, s) in report.scenes.iter().enumerate() {
        let ok = s.start_sec.is_finite()
            && s.end_sec.is_finite()
            && s.end_sec >= s.start_sec
            && s.start_sec >= prev_start
            && s.end_frame >= s.start_frame;
        if !ok {
            return Err(Error::internal(format!(
                "scenedetect helper returned malformed scene #{}",
                i + 1
            )));
        }
        prev_start = s.start_sec;
    }
    Ok(report)
}

/// Runs the helper with the python named by `CINVAR_SCENEDETECT_PYTHON`.
/// `threshold` of `None` uses the detector's default.
pub fn detect(
    video: &Path,
    detector: Detector,
    threshold: Option<f64>,
    work_dir: &Path,
) -> Result<SceneReport> {
    if let Some(t) = threshold {
        if !(t.is_finite() && t > 0.0) {
            return Err(Error::invalid(format!(
                "threshold must be a positive number, got {t}"
            )));
        }
    }
    let python = python_from_env()?;
    std::fs::create_dir_all(work_dir)
        .map_err(|e| Error::io(format!("cannot create {}: {e}", work_dir.display())))?;
    let script = work_dir.join("scenedetect_helper.py");
    std::fs::write(&script, HELPER)
        .map_err(|e| Error::io(format!("cannot write {}: {e}", script.display())))?;
    let args = vec![
        "-I".to_string(),
        script.to_string_lossy().to_string(),
        video.to_string_lossy().to_string(),
        detector.name().to_string(),
        threshold.map(|t| t.to_string()).unwrap_or_default(),
    ];
    let out = proc::run(&python, &args, &ProcOpts::default().with_timeout(TIMEOUT_SECS))?;
    if !out.success {
        return Err(Error::internal(format!(
            "PySceneDetect helper failed: {}",
            secrets::redact(&out.tail_stderr(3))
        ))
        .with_hint("check that the CINVAR_SCENEDETECT_PYTHON venv has scenedetect==0.7.3"));
    }
    parse_report(&out.stdout)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"{"detector":"content","threshold":27.0,"fps":25.0,"width":480,"height":270,
        "scenes":[{"start_frame":0,"start_sec":0.0,"end_frame":75,"end_sec":3.0},
                  {"start_frame":75,"start_sec":3.0,"end_frame":163,"end_sec":6.52}]}"#;

    #[test]
    fn parses_helper_json() {
        let r = parse_report(SAMPLE).unwrap();
        assert_eq!(r.detector, "content");
        assert_eq!(r.width, 480);
        assert_eq!(r.height, 270);
        assert_eq!(r.fps, 25.0);
        assert_eq!(r.scenes.len(), 2);
        assert_eq!(r.scenes[1].start_frame, 75);
        assert_eq!(r.scenes[1].end_sec, 6.52);
    }

    #[test]
    fn threshold_is_optional() {
        let text = r#"{"detector":"adaptive","fps":25,"width":2,"height":2,"scenes":[]}"#;
        let r = parse_report(text).unwrap();
        assert_eq!(r.threshold, None);
        assert!(r.scenes.is_empty());
    }

    #[test]
    fn rejects_non_json_output() {
        let err = parse_report("Detected: 3 | Progress: 100%").unwrap_err();
        assert_eq!(err.code, ErrorCode::Internal);
        assert!(err.message.contains("invalid JSON"));
    }

    #[test]
    fn rejects_missing_required_fields() {
        let err = parse_report(r#"{"detector":"content","scenes":[]}"#).unwrap_err();
        assert!(err.message.contains("invalid JSON"));
    }

    #[test]
    fn rejects_non_monotonic_scenes() {
        let text = r#"{"detector":"content","fps":25,"width":2,"height":2,"scenes":[
            {"start_frame":75,"start_sec":3.0,"end_frame":80,"end_sec":3.2},
            {"start_frame":0,"start_sec":0.0,"end_frame":75,"end_sec":3.0}]}"#;
        let err = parse_report(text).unwrap_err();
        assert!(err.message.contains("malformed scene #2"));
    }

    #[test]
    fn rejects_bad_fps() {
        let text = r#"{"detector":"content","fps":0,"width":2,"height":2,"scenes":[]}"#;
        assert!(parse_report(text).is_err());
    }

    #[test]
    fn unset_env_is_an_honest_dependency_error() {
        let err = python_from_value(None).unwrap_err();
        assert_eq!(err.code, ErrorCode::DependencyMissing);
        assert!(err.message.contains("CINVAR_SCENEDETECT_PYTHON is not set"));
        assert!(python_from_value(Some("   ")).is_err());
    }

    #[test]
    fn env_pointing_at_missing_file_is_rejected() {
        let err = python_from_value(Some("/nonexistent/venv/bin/python")).unwrap_err();
        assert_eq!(err.code, ErrorCode::DependencyMissing);
    }

    #[test]
    fn detector_names_round_trip() {
        assert_eq!(Detector::parse("content").unwrap(), Detector::Content);
        assert_eq!(Detector::parse("adaptive").unwrap().name(), "adaptive");
        assert_eq!(Detector::parse("bogus").unwrap_err().code, ErrorCode::InvalidInput);
    }

    #[test]
    fn helper_is_embedded() {
        assert!(HELPER.contains("scenedetect.detect("));
    }
}
