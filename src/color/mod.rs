//! Focused OpenColorIO integration.
//!
//! CINVAR does not colour-manage by default and never reinterprets media on its own: with no
//! colour step in a project, pixels pass through exactly as before. This module offers explicit,
//! inspectable steps only:
//!
//! * `inspect`  – discover a config and list its colour spaces and roles;
//! * `bake_lut` – turn one named source -> destination transform into a `.cube` 3D LUT inside
//!   the project, which the renderer applies through FFmpeg's `lut3d` (the `lut` clip effect);
//! * `validate_cube` – check an existing `.cube` before it is accepted into a project.
//!
//! OCIO itself runs in a separate Python process (`CINVAR_OCIO_PYTHON`, default `python3`).

use std::path::{Path, PathBuf};

use serde::Serialize;
use serde_json::{json, Value};

use crate::error::{Error, ErrorCode, Result};
use crate::util::proc::{self, ProcOpts};

const HELPER: &str = include_str!("ocio_helper.py");

#[derive(Debug, Clone, Serialize)]
pub struct ConfigChoice {
    /// Value passed to OCIO: a file path or an `ocio://` builtin name.
    pub spec: String,
    /// Where the choice came from, so the user can see why this config was used.
    pub source: String,
}

/// Config discovery order: explicit argument, project `color/config.ocio`, `$OCIO`, builtin.
pub fn discover_config(explicit: Option<&Path>, project_root: &Path) -> ConfigChoice {
    if let Some(p) = explicit {
        return ConfigChoice {
            spec: p.to_string_lossy().into(),
            source: "explicit".into(),
        };
    }
    let local = project_root.join("color").join("config.ocio");
    if local.is_file() {
        return ConfigChoice {
            spec: local.to_string_lossy().into(),
            source: "project color/config.ocio".into(),
        };
    }
    if let Ok(env) = std::env::var("OCIO") {
        if !env.trim().is_empty() {
            return ConfigChoice {
                spec: env,
                source: "$OCIO".into(),
            };
        }
    }
    ConfigChoice {
        spec: "ocio://default".into(),
        source: "OpenColorIO builtin default (ACES CG config)".into(),
    }
}

fn python() -> PathBuf {
    std::env::var("CINVAR_OCIO_PYTHON")
        .ok()
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
        .or_else(|| proc::which("python3"))
        .unwrap_or_else(|| PathBuf::from("python3"))
}

fn call(request: &Value) -> Result<Value> {
    let tmp = std::env::temp_dir().join(format!(
        "cinvar-ocio-{}-{}.py",
        std::process::id(),
        crate::util::ids::new_id("h")
    ));
    std::fs::write(&tmp, HELPER)?;
    let opts = ProcOpts::default()
        .with_timeout(120)
        .with_stdin(request.to_string());
    let out = proc::run(
        &python(),
        &["-I".into(), tmp.to_string_lossy().into()],
        &opts,
    );
    let _ = std::fs::remove_file(&tmp);
    let out = out.map_err(|e| {
        Error::new(
            ErrorCode::DependencyMissing,
            format!("OpenColorIO helper could not start: {}", e.message),
        )
        .with_hint(
            "pip install opencolorio into a virtualenv and set CINVAR_OCIO_PYTHON to its python",
        )
    })?;
    let v: Value = serde_json::from_str(out.stdout.trim()).map_err(|_| {
        Error::new(
            ErrorCode::DependencyMissing,
            format!(
                "OpenColorIO helper failed: {}",
                out.stderr.lines().last().unwrap_or("no output")
            ),
        )
        .with_hint(
            "pip install opencolorio into a virtualenv and set CINVAR_OCIO_PYTHON to its python",
        )
    })?;
    if v["ok"] == true {
        Ok(v)
    } else {
        let mut msg = v["error"]
            .as_str()
            .unwrap_or("OpenColorIO error")
            .to_string();
        if let Some(s) = v["suggestions"].as_array().filter(|s| !s.is_empty()) {
            msg.push_str(&format!(
                " (did you mean: {})",
                s.iter()
                    .filter_map(|x| x.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
        Err(Error::new(ErrorCode::InvalidInput, msg))
    }
}

pub fn available() -> std::result::Result<String, String> {
    call(&json!({"op": "inspect"}))
        .map(|v| format!("OpenColorIO {}", v["ocio_version"].as_str().unwrap_or("?")))
        .map_err(|e| e.message)
}

pub fn inspect(config: &ConfigChoice) -> Result<Value> {
    let mut v = call(&json!({"op": "inspect", "config": config.spec}))?;
    v["config_source"] = json!(config.source);
    Ok(v)
}

/// Bakes `src` -> `dst` into `out` (a `.cube`). Both names must exist in the config; nothing is
/// guessed or substituted.
pub fn bake_lut(
    config: &ConfigChoice,
    src: &str,
    dst: &str,
    size: u32,
    out: &Path,
) -> Result<Value> {
    if let Some(parent) = out.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut v = call(
        &json!({"op": "bake_lut", "config": config.spec, "src": src, "dst": dst, "size": size, "out": out}),
    )?;
    validate_cube(out)?;
    v["config_source"] = json!(config.source);
    Ok(v)
}

/// Structural check of a Resolve/Adobe `.cube` 3D LUT: size header, finite triples, exact count.
pub fn validate_cube(path: &Path) -> Result<u32> {
    let meta = std::fs::metadata(path)
        .map_err(|_| Error::not_found(format!("LUT not found: {}", path.display())))?;
    if meta.len() > 256 << 20 {
        return Err(Error::invalid("LUT file is unreasonably large"));
    }
    let text = std::fs::read_to_string(path)
        .map_err(|_| Error::invalid("LUT is not a text .cube file"))?;
    let mut size = 0u32;
    let mut rows = 0u64;
    for line in text
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
    {
        let mut it = line.split_whitespace();
        match it.next() {
            Some("LUT_3D_SIZE") => size = it.next().and_then(|n| n.parse().ok()).unwrap_or(0),
            Some("TITLE") | Some("DOMAIN_MIN") | Some("DOMAIN_MAX") | Some("LUT_1D_SIZE") => {
                if line.starts_with("LUT_1D_SIZE") {
                    return Err(Error::unsupported(
                        "1D LUTs are not supported; use a 3D .cube",
                    ));
                }
            }
            Some(first) => {
                let vals: Vec<f64> = std::iter::once(first)
                    .chain(it)
                    .filter_map(|t| t.parse().ok())
                    .collect();
                if vals.len() != 3 || vals.iter().any(|v| !v.is_finite()) {
                    return Err(Error::invalid(format!(
                        "malformed LUT row: '{}'",
                        line.chars().take(60).collect::<String>()
                    )));
                }
                rows += 1;
            }
            None => {}
        }
    }
    if !(2..=129).contains(&size) {
        return Err(Error::invalid("missing or implausible LUT_3D_SIZE"));
    }
    if rows != (size as u64).pow(3) {
        return Err(Error::invalid(format!(
            "LUT has {rows} rows, expected {}",
            (size as u64).pow(3)
        )));
    }
    Ok(size)
}
