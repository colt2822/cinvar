//! Plugin registry: one inventory for built-in integrations and external `plugin.json`
//! plugins, with compatibility, dependency, health, permission and cost/cloud disclosure
//! reporting, plus persisted enable/disable and version pinning.
//!
//! Safety rules: install instructions are text only (never executed); invalid plugins are
//! reported, not fatal; a disabled or pin-mismatched plugin never registers tools; health
//! commands of external plugins only run when subprocess plugins are explicitly allowed.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};

use super::{platform_supported, version_at_least, version_parts, PluginManifest, MANIFEST_FILE};
use crate::error::{Error, Result};
use crate::util::paths;
use crate::util::proc::{self, ProcOpts};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct PluginState {
    /// Missing = enabled for built-ins, DISABLED for external plugins until the user opts in.
    pub enabled: BTreeMap<String, bool>,
    /// id -> exact version that must be installed.
    pub pinned: BTreeMap<String, String>,
}

pub fn default_state_path() -> PathBuf {
    paths::config_dir().join("plugins-state.json")
}

impl PluginState {
    pub fn load(path: &Path) -> PluginState {
        std::fs::read_to_string(path)
            .ok()
            .and_then(|t| serde_json::from_str(&t).ok())
            .unwrap_or_default()
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        paths::write_atomic(path, &serde_json::to_vec_pretty(self)?)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    Builtin,
    External,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Health {
    Ok,
    /// Optional dependency/service not found; CINVAR works without it.
    Unavailable,
    /// Declared in the registry as a contract; no adapter code exists yet.
    ContractOnly,
    NotChecked,
    Invalid,
}

#[derive(Debug, Clone, Serialize)]
pub struct PluginInfo {
    pub id: String,
    pub name: String,
    pub version: String,
    pub kind: Kind,
    pub enabled: bool,
    pub health: Health,
    pub health_detail: String,
    pub compatible: bool,
    pub problems: Vec<String>,
    pub missing_dependencies: Vec<String>,
    pub capabilities: Vec<String>,
    pub permissions: Vec<String>,
    pub costs_money: bool,
    pub uses_cloud: bool,
    pub sends_files_off_device: bool,
    pub pinned_to: Option<String>,
    pub install_help: String,
    pub license: String,
}

struct Builtin {
    id: &'static str,
    name: &'static str,
    caps: &'static [&'static str],
    perms: &'static [&'static str],
    costs: bool,
    cloud: bool,
    off_device: bool,
    install: &'static str,
    license: &'static str,
    health: fn() -> (Health, String),
}

fn h_blender() -> (Health, String) {
    match crate::blender::detect() {
        Ok(i) => (
            Health::Ok,
            format!("Blender {} at {}", i.version, i.path.display()),
        ),
        Err(e) => (Health::Unavailable, e.message),
    }
}
fn h_runpod() -> (Health, String) {
    match crate::secrets::runpod_key() {
        Some(_) => (
            Health::Ok,
            "API key configured (not validated; use cinvar.cloud.status)".into(),
        ),
        None => (Health::Unavailable, "no RunPod API key configured".into()),
    }
}
fn h_comfy() -> (Health, String) {
    match crate::comfy::ComfyClient::from_env().and_then(|c| c.system_stats()) {
        Ok(_) => (Health::Ok, "ComfyUI reachable".into()),
        Err(e) => (Health::Unavailable, e.message),
    }
}
fn h_otio() -> (Health, String) {
    (
        Health::Ok,
        "built-in OpenTimelineIO JSON export/import".into(),
    )
}
fn h_ocio() -> (Health, String) {
    match crate::color::available() {
        Ok(v) => (Health::Ok, v),
        Err(e) => (Health::Unavailable, e),
    }
}
fn h_ffmpeg() -> (Health, String) {
    match crate::media::ffmpeg_version() {
        Ok(v) => (Health::Ok, v),
        Err(e) => (Health::Unavailable, e.message),
    }
}
fn h_whisper() -> (Health, String) {
    match crate::captions::detect_engine() {
        Some(e) => (Health::Ok, format!("transcription engine: {}", e.name())),
        None => (
            Health::Unavailable,
            "no transcription engine on PATH".into(),
        ),
    }
}
fn h_scenedetect() -> (Health, String) {
    match proc::which("scenedetect") {
        Some(p) => (Health::Ok, format!("scenedetect at {}", p.display())),
        None => (
            Health::Unavailable,
            "scenedetect not on PATH (pip install scenedetect[opencv])".into(),
        ),
    }
}
fn h_storage() -> (Health, String) {
    (
        Health::ContractOnly,
        "S3-compatible storage is a documented contract; no adapter is implemented".into(),
    )
}

fn h_contract_upscale() -> (Health, String) {
    (Health::ContractOnly, "AI upscaling is a documented contract (ComfyUI workflow or external tool); no adapter is implemented".into())
}
fn h_contract_openfx() -> (Health, String) {
    (
        Health::ContractOnly,
        "OpenFX hosting is a documented contract; no host is implemented".into(),
    )
}

const BUILTINS: &[Builtin] = &[
    Builtin { id: "cinvar.ai-upscale", name: "AI upscaling", caps: &["upscale"], perms: &["spawn_subprocess"], costs: false, cloud: false, off_device: false,
        install: "Not implemented. Plan: register an upscaling ComfyUI workflow (cinvar.ai.*) and import the result; scale-filter resizing is NOT AI upscaling.", license: "per model", health: h_contract_upscale },
    Builtin { id: "cinvar.openfx", name: "OpenFX effects", caps: &["effects"], perms: &["spawn_subprocess"], costs: false, cloud: false, off_device: false,
        install: "Not implemented (contract only).", license: "BSD-3-Clause (OFX API); per effect", health: h_contract_openfx },
    Builtin { id: "cinvar.ffmpeg", name: "FFmpeg", caps: &["decode", "encode", "filters"], perms: &["spawn_subprocess"], costs: false, cloud: false, off_device: false,
        install: "Install FFmpeg (https://ffmpeg.org/download.html) and make sure ffmpeg/ffprobe are on PATH.", license: "LGPL-2.1+ or GPL-2.0+ depending on the build (external process; check `ffmpeg -version`)", health: h_ffmpeg },
    Builtin { id: "cinvar.ocio", name: "OpenColorIO colour transforms", caps: &["color-spaces", "lut-bake"], perms: &["spawn_subprocess"], costs: false, cloud: false, off_device: false,
        install: "python3 -m venv ocio-venv && ocio-venv/bin/pip install opencolorio, then set CINVAR_OCIO_PYTHON to ocio-venv/bin/python.", license: "BSD-3-Clause (external process)", health: h_ocio },
    Builtin { id: "cinvar.blender", name: "Blender", caps: &["3d-title", "logo-reveal", "background-render"], perms: &["spawn_subprocess"], costs: false, cloud: false, off_device: false,
        install: "Install Blender 3.6+ (https://www.blender.org/download/) or set CINVAR_BLENDER to the executable.", license: "GPL-3.0-or-later (external process; not linked)", health: h_blender },
    Builtin { id: "cinvar.runpod", name: "RunPod cloud render", caps: &["cloud-render", "gpu-render"], perms: &["network"], costs: true, cloud: true, off_device: true,
        install: "Create a RunPod API key, set CINVAR_RUNPOD_API_KEY, and enable security.allow_cloud_render.", license: "service (RunPod terms)", health: h_runpod },
    Builtin { id: "cinvar.comfyui", name: "ComfyUI workflows", caps: &["ai-workflow", "generate-media"], perms: &["network"], costs: false, cloud: false, off_device: false,
        install: "Run ComfyUI locally (default http://127.0.0.1:8188) or set CINVAR_COMFYUI_URL.", license: "GPL-3.0 (external service)", health: h_comfy },
    Builtin { id: "cinvar.otio", name: "OpenTimelineIO interchange", caps: &["timeline-export", "timeline-import"], perms: &[], costs: false, cloud: false, off_device: false,
        install: "Built in.", license: "Apache-2.0 (format)", health: h_otio },
    Builtin { id: "cinvar.transcribe", name: "Speech transcription (whisper family)", caps: &["transcribe", "captions"], perms: &["spawn_subprocess"], costs: false, cloud: false, off_device: false,
        install: "pip install faster-whisper into a venv and set CINVAR_FASTER_WHISPER_PYTHON (+ CINVAR_FASTER_WHISPER_MODEL/_CACHE; local models only), or install whisper.cpp / openai-whisper.", license: "per engine/model", health: h_whisper },
    Builtin { id: "cinvar.scenedetect", name: "PySceneDetect", caps: &["scene-detection"], perms: &["spawn_subprocess"], costs: false, cloud: false, off_device: false,
        install: "pip install scenedetect[opencv]", license: "BSD-3-Clause (external process)", health: h_scenedetect },
    Builtin { id: "cinvar.cloud-storage", name: "Cloud object storage", caps: &["asset-sync"], perms: &["network"], costs: true, cloud: true, off_device: true,
        install: "Not implemented yet (contract only).", license: "n/a", health: h_storage },
];

fn compat_problems(m: &PluginManifest) -> Vec<String> {
    let mut p = Vec::new();
    match version_parts(&m.cinvar_min) {
        Some(req) => {
            let have = version_parts(crate::VERSION).unwrap_or_default();
            if !version_at_least(&have, &req) {
                p.push(format!(
                    "needs CINVAR {} or newer (this is {})",
                    m.cinvar_min,
                    crate::VERSION
                ));
            }
        }
        None => p.push(format!("invalid cinvar_min '{}'", m.cinvar_min)),
    }
    if !m.cinvar_max.trim().is_empty() {
        match version_parts(&m.cinvar_max) {
            Some(max) => {
                let have = version_parts(crate::VERSION).unwrap_or_default();
                if !version_at_least(&max, &have) {
                    p.push(format!(
                        "only tested up to CINVAR {} (this is {})",
                        m.cinvar_max,
                        crate::VERSION
                    ));
                }
            }
            None => p.push(format!("invalid cinvar_max '{}'", m.cinvar_max)),
        }
    }
    if !platform_supported(m) {
        p.push(format!(
            "supports {:?}, this host is {}",
            m.platforms,
            std::env::consts::OS
        ));
    }
    if m.schema_version > 1 {
        p.push(format!(
            "manifest schema {} is newer than this CINVAR understands",
            m.schema_version
        ));
    }
    p
}

fn perms_of(m: &PluginManifest) -> Vec<String> {
    let mut v = Vec::new();
    if m.permissions.spawn_subprocess {
        v.push("spawn_subprocess".to_string());
    }
    if m.permissions.network {
        v.push("network".to_string());
    }
    for w in &m.permissions.write_paths {
        v.push(format!("write:{w}"));
    }
    v
}

/// Full inventory. Never fails because of one bad plugin.
pub fn inventory(
    plugin_dir: &Path,
    state: &PluginState,
    allow_subprocess: bool,
) -> Vec<PluginInfo> {
    let mut out = Vec::new();
    for b in BUILTINS {
        let (health, detail) = (b.health)();
        out.push(PluginInfo {
            id: b.id.into(),
            name: b.name.into(),
            version: crate::VERSION.into(),
            kind: Kind::Builtin,
            enabled: state.enabled.get(b.id).copied().unwrap_or(true),
            health,
            health_detail: detail,
            compatible: true,
            problems: vec![],
            missing_dependencies: vec![],
            capabilities: b.caps.iter().map(|s| s.to_string()).collect(),
            permissions: b.perms.iter().map(|s| s.to_string()).collect(),
            costs_money: b.costs,
            uses_cloud: b.cloud,
            sends_files_off_device: b.off_device,
            pinned_to: None,
            install_help: b.install.into(),
            license: b.license.into(),
        });
    }
    let builtin_ids: Vec<&str> = BUILTINS.iter().map(|b| b.id).collect();
    let mut dirs: Vec<PathBuf> = std::fs::read_dir(plugin_dir)
        .map(|rd| rd.filter_map(|e| e.ok().map(|e| e.path())).collect())
        .unwrap_or_default();
    dirs.sort();
    let mut seen: Vec<String> = Vec::new();
    for root in dirs.into_iter().filter(|p| p.is_dir()) {
        let manifest_path = root.join(MANIFEST_FILE);
        if !manifest_path.is_file() {
            continue;
        }
        let dir_name = root
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default();
        let parsed = std::fs::read_to_string(&manifest_path)
            .map_err(|e| e.to_string())
            .and_then(|t| serde_json::from_str::<PluginManifest>(&t).map_err(|e| e.to_string()));
        let m = match parsed {
            Ok(m) => m,
            Err(e) => {
                out.push(invalid_entry(
                    &dir_name,
                    format!("invalid plugin.json: {e}"),
                ));
                continue;
            }
        };
        let id = m.plugin_id();
        let mut problems = compat_problems(&m);
        if m.name.trim().is_empty() || m.version.trim().is_empty() {
            problems.push("name and version are required".into());
        }
        if builtin_ids.contains(&id.as_str()) {
            problems.push(format!("id '{id}' collides with a built-in integration"));
        }
        if seen.contains(&id) {
            problems.push(format!("duplicate plugin id '{id}'"));
        }
        seen.push(id.clone());
        let pinned = state.pinned.get(&id).cloned();
        if let Some(p) = &pinned {
            if p != &m.version {
                problems.push(format!("pinned to {p} but {} is installed", m.version));
            }
        }
        let missing: Vec<String> = m
            .requires
            .iter()
            .filter(|r| proc::which(r).is_none())
            .cloned()
            .collect();
        let (health, detail) = if !problems.is_empty() {
            (Health::Invalid, problems.join("; "))
        } else if m.health.is_empty() {
            (Health::NotChecked, "plugin declares no health check".into())
        } else if !allow_subprocess {
            (
                Health::NotChecked,
                "health check skipped: plugin subprocesses are disabled".into(),
            )
        } else if !state.enabled.get(&id).copied().unwrap_or(false) {
            // Never run code from a plugin the user has not enabled.
            (
                Health::NotChecked,
                "health check skipped: plugin is not enabled".into(),
            )
        } else {
            run_health(&root, &m.health)
        };
        out.push(PluginInfo {
            id: id.clone(),
            name: m.name.clone(),
            version: m.version.clone(),
            kind: Kind::External,
            enabled: state.enabled.get(&id).copied().unwrap_or(false),
            health,
            health_detail: detail,
            compatible: problems.is_empty(),
            problems,
            missing_dependencies: missing,
            capabilities: m.capabilities.clone(),
            permissions: perms_of(&m),
            costs_money: m.costs_money,
            uses_cloud: m.uses_cloud,
            sends_files_off_device: m.sends_files_off_device,
            pinned_to: pinned,
            install_help: m.install.clone(),
            license: m.license.clone(),
        });
    }
    out
}

fn invalid_entry(name: &str, why: String) -> PluginInfo {
    PluginInfo {
        id: name.to_string(),
        name: name.to_string(),
        version: String::new(),
        kind: Kind::External,
        enabled: false,
        health: Health::Invalid,
        health_detail: why.clone(),
        compatible: false,
        problems: vec![why],
        missing_dependencies: vec![],
        capabilities: vec![],
        permissions: vec![],
        costs_money: false,
        uses_cloud: false,
        sends_files_off_device: false,
        pinned_to: None,
        install_help: String::new(),
        license: String::new(),
    }
}

fn run_health(root: &Path, argv: &[String]) -> (Health, String) {
    let Some((prog, rest)) = argv.split_first() else {
        return (Health::NotChecked, "empty health command".into());
    };
    let p = Path::new(prog);
    let escapes = p.is_absolute()
        || p.components()
            .any(|c| matches!(c, std::path::Component::ParentDir));
    if escapes {
        return (
            Health::Invalid,
            "health command must stay inside the plugin directory".into(),
        );
    }
    let program = proc::which(prog).unwrap_or_else(|| root.join(prog));
    // Health checks are plugin code too: same sandbox, read-only plugin directory, no network.
    let (program, rest, _sandboxed) = super::sandbox_command(
        &program,
        rest,
        Some(root),
        &super::PluginPermissions::default(),
        None,
    );
    let opts = ProcOpts {
        cwd: Some(root.to_path_buf()),
        timeout: Some(Duration::from_secs(10)),
        stdin: None,
        env: vec![],
        max_capture: 16 * 1024,
    };
    match proc::run(&program, &rest, &opts) {
        Ok(o) if o.success => (
            Health::Ok,
            o.stdout.lines().next().unwrap_or("healthy").to_string(),
        ),
        Ok(o) => (
            Health::Unavailable,
            format!("health check exited {:?}: {}", o.code, o.tail_stderr(2)),
        ),
        Err(e) => (Health::Unavailable, e.message),
    }
}

pub fn set_enabled(state_path: &Path, id: &str, enabled: bool) -> Result<()> {
    let mut st = PluginState::load(state_path);
    st.enabled.insert(id.to_string(), enabled);
    st.save(state_path)
}

pub fn set_pin(state_path: &Path, id: &str, version: Option<&str>) -> Result<()> {
    let mut st = PluginState::load(state_path);
    match version {
        Some(v) if !v.trim().is_empty() => {
            st.pinned.insert(id.to_string(), v.to_string());
        }
        _ => {
            st.pinned.remove(id);
        }
    }
    st.save(state_path)
}

/// External manifests that are allowed to register tools: enabled, compatible, pin satisfied.
/// Returns `(runnable, skipped-with-reason)`.
pub fn runnable(
    plugin_dir: &Path,
    state: &PluginState,
) -> (Vec<PluginManifest>, Vec<(String, String)>) {
    let infos = inventory(plugin_dir, state, false);
    let mut ok = Vec::new();
    let mut skipped = Vec::new();
    let manifests = super::discover_lenient(plugin_dir);
    for m in manifests {
        let id = m.plugin_id();
        let Some(info) = infos
            .iter()
            .find(|i| i.id == id && i.kind == Kind::External)
        else {
            continue;
        };
        if !info.enabled {
            skipped.push((id, "disabled".to_string()));
        } else if !info.compatible {
            skipped.push((id, info.problems.join("; ")));
        } else if !info.missing_dependencies.is_empty() {
            skipped.push((
                id,
                format!(
                    "missing dependencies: {}",
                    info.missing_dependencies.join(", ")
                ),
            ));
        } else {
            ok.push(m);
        }
    }
    (ok, skipped)
}

pub fn require_known(plugin_dir: &Path, id: &str) -> Result<()> {
    let known = inventory(plugin_dir, &PluginState::default(), false);
    if known.iter().any(|i| i.id == id) {
        Ok(())
    } else {
        Err(Error::not_found(format!("no plugin with id '{id}'")))
    }
}
