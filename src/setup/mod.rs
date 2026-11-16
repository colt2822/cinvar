//! Managed installation of CINVAR Pro AI dependencies.
//!
//! `cinvar setup --pro-ai` creates one virtualenv per component under
//! `~/.local/share/cinvar/venvs/<component>/`, installs a pinned torch/torchaudio/torchvision
//! stack from a single CUDA channel and proves it by importing both wheels, creates the
//! `libomp.so.5` shim Real-ESRGAN's ncnn build needs (`<venv>/lib-shim`, the directory the
//! adapter prepends to `LD_LIBRARY_PATH`), downloads only the weights each component actually
//! loads (`/mnt/elyreon-data/cinvar-models/<component>/` when the data mount is present, else
//! `~/.local/share/cinvar/models/<component>/`) and records every resolved path in
//! `~/.config/cinvar/pro-ai.toml`.
//!
//! A component that fails reports its failing stage, the last output lines and the next step while
//! the other components still install. Progress goes to stderr, so `--json` on stdout stays
//! parseable.

mod install;
mod pins;

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::Result;

pub use pins::{Device, TorchPin};

/// One component this command can install.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Component {
    WhisperX,
    FasterWhisper,
    RealEsrgan,
    Demucs,
}

impl Component {
    pub const ALL: &'static [Component] = &[
        Component::WhisperX,
        Component::FasterWhisper,
        Component::RealEsrgan,
        Component::Demucs,
    ];

    /// The `CINVAR_*_PYTHON` variable this component is configured through, as named by the
    /// adapter that reads it.
    pub fn env_var(self) -> &'static str {
        match self {
            Component::WhisperX => "CINVAR_WHISPERX_PYTHON",
            Component::FasterWhisper => "CINVAR_FASTER_WHISPER_PYTHON",
            // The Real-ESRGAN adapter reads CINVAR_ESRGAN_PYTHON (src/enhance/mod.rs::ENV_PYTHON).
            Component::RealEsrgan => "CINVAR_ESRGAN_PYTHON",
            Component::Demucs => "CINVAR_DEMUCS_PYTHON",
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Component::WhisperX => "whisperx",
            Component::FasterWhisper => "faster-whisper",
            Component::RealEsrgan => "realesrgan",
            Component::Demucs => "demucs",
        }
    }

    /// pip requirements for the component itself. torch is never listed here: it is installed from
    /// the pinned channel first so pip cannot resolve an unpinned, mismatched wheel later.
    fn requirements(self) -> &'static [&'static str] {
        match self {
            Component::WhisperX => &["whisperx"],
            Component::FasterWhisper => &["faster-whisper"],
            Component::RealEsrgan => &["realesrgan-ncnn-py==2.0.0"],
            Component::Demucs => &["demucs==4.1.0"],
        }
    }

    /// The components that need the pinned torch stack.
    fn needs_torch(self) -> bool {
        matches!(self, Component::WhisperX | Component::Demucs)
    }

    /// The module imported to prove the component works.
    fn probe_module(self) -> &'static str {
        match self {
            Component::WhisperX => "whisperx",
            Component::FasterWhisper => "faster_whisper",
            Component::RealEsrgan => "realesrgan_ncnn_py",
            Component::Demucs => "demucs",
        }
    }
}

/// Per-component outcome.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ComponentReport {
    pub component: String,
    pub venv: String,
    pub installed: bool,
    /// Empty when the component is usable; otherwise why it is not.
    pub problem: String,
}

/// What `--pro-ai` did.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SetupReport {
    pub components: Vec<ComponentReport>,
    /// The file this run wrote: `~/.config/cinvar/pro-ai.toml`. `Config` has no field for
    /// helper paths yet (src/config.rs), so the resolved interpreters live in that manifest.
    pub config_path: String,
    /// True when every component is installed and configured.
    pub complete: bool,
}

/// What one install resolved, recorded in the manifest so an adapter can consume it.
#[derive(Debug, Clone, Default)]
struct Facts {
    torch_pair: String,
    shim: Option<PathBuf>,
    env: Vec<(String, String)>,
}

/// Install and configure every Pro AI component, then record where everything landed.
pub fn pro_ai(json_mode: bool) -> Result<SetupReport> {
    // Progress is written to stderr, so --json output on stdout stays parseable either way.
    let _ = json_mode;
    let layout = install::Layout::resolve()?;
    let device = Device::detect();
    let pin = device.pin();
    eprintln!(
        "cinvar setup --pro-ai: {} torch stack ({}), venvs under {}, weights under {}",
        device.as_str(),
        pin.torch,
        layout.venvs.display(),
        layout.models.display()
    );
    let mut entries: Vec<(ComponentReport, Facts)> = Vec::new();
    for (index, component) in Component::ALL.iter().enumerate() {
        let step = format!("[{}/{}]", index + 1, Component::ALL.len());
        let mut facts = Facts::default();
        let report = match install_component(*component, &layout, device, &step, &mut facts) {
            Ok(()) => ComponentReport {
                component: component.as_str().into(),
                venv: layout.venv(*component).display().to_string(),
                installed: true,
                problem: String::new(),
            },
            Err(e) => {
                eprintln!("{step} {}: {e}", component.as_str());
                ComponentReport {
                    component: component.as_str().into(),
                    venv: layout.venv(*component).display().to_string(),
                    installed: false,
                    problem: e.to_string(),
                }
            }
        };
        eprintln!(
            "{step} {}: {}",
            component.as_str(),
            if report.installed { "ready" } else { "FAILED" }
        );
        entries.push((report, facts));
    }
    let manifest = install::write_manifest(&layout, device, &entries)?;
    print_adapter_env(&manifest, &entries);
    Ok(SetupReport {
        complete: entries.iter().all(|(report, _)| report.installed),
        components: entries.into_iter().map(|(report, _)| report).collect(),
        config_path: manifest.display().to_string(),
    })
}


fn install_component(c: Component, layout: &install::Layout, device: Device, step: &str, facts: &mut Facts) -> Result<()> {
    let python = install::ensure_venv(layout, c, step)?;
    if c.needs_torch() {
        install::install_torch(layout, c, &python, device, step)?;
        facts.torch_pair = install::verify_torch_pair(layout, c, &python, device.pin())?;
        eprintln!("{step} {}: torch/torchaudio import ok: {}", c.as_str(), facts.torch_pair);
    }
    install::install_requirements(layout, c, &python, step)?;
    // The OpenMP shim has to exist before the import probe: realesrgan_ncnn_vulkan_wrapper.so
    // carries "NEEDED libomp.so.5", so without <venv>/lib-shim on LD_LIBRARY_PATH the module
    // cannot be imported at all.
    if c == Component::RealEsrgan {
        facts.shim = Some(install::omp_shim(&layout.venv(c), step)?);
    }
    install::probe_module(layout, c, &python, step)?;
    let models = install::ensure_models(layout, c, &python, step)?;
    facts.env = adapter_env(c, &python, &models);
    Ok(())
}

/// The environment the adapters read, with the values this run resolved.
fn adapter_env(c: Component, python: &Path, models: &Path) -> Vec<(String, String)> {
    let mut env = vec![(c.env_var().to_string(), python.display().to_string())];
    match c {
        Component::WhisperX => {
            // The helper passes this as download_root and never downloads on its own.
            env.push(("CINVAR_WHISPERX_CACHE".into(), models.display().to_string()));
            env.push(("CINVAR_WHISPERX_MODEL".into(), "base".into()));
        }
        Component::FasterWhisper => {
            env.push(("CINVAR_FASTER_WHISPER_CACHE".into(), models.display().to_string()));
            env.push(("CINVAR_FASTER_WHISPER_MODEL".into(), "tiny".into()));
        }
        // Real-ESRGAN needs nothing but the interpreter: the wheel bundles its weights and the
        // adapter adds <venv>/lib-shim to LD_LIBRARY_PATH itself.
        Component::RealEsrgan => {}
        Component::Demucs => {
            // demucs resolves htdemucs through torch.hub, not through HF_HOME.
            env.push(("TORCH_HOME".into(), models.display().to_string()));
            env.push(("CINVAR_DEMUCS_ALLOW_DOWNLOAD".into(), "1".into()));
        }
    }
    env
}

/// Print the resolved environment once, so the stack is usable before the adapters read the
/// manifest themselves.
fn print_adapter_env(manifest: &Path, entries: &[(ComponentReport, Facts)]) {
    let lines: Vec<String> = entries
        .iter()
        .flat_map(|(_, facts)| facts.env.iter())
        .map(|(key, value)| format!("  export {key}={value}"))
        .collect();
    if lines.is_empty() {
        return;
    }
    eprintln!("\nHelper environment (also written to {}):", manifest.display());
    for line in lines {
        eprintln!("{line}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn env_var_names_match_the_adapters_that_read_them() {
        assert_eq!(Component::WhisperX.env_var(), "CINVAR_WHISPERX_PYTHON");
        assert_eq!(Component::FasterWhisper.env_var(), "CINVAR_FASTER_WHISPER_PYTHON");
        assert_eq!(Component::RealEsrgan.env_var(), crate::enhance::ENV_PYTHON);
        assert_eq!(Component::Demucs.env_var(), crate::audio::demucs::ENV_PYTHON);
    }

    #[test]
    fn every_component_has_a_distinct_name_and_a_probe() {
        let names: Vec<&str> = Component::ALL.iter().map(|c| c.as_str()).collect();
        assert_eq!(names, vec!["whisperx", "faster-whisper", "realesrgan", "demucs"]);
        for c in Component::ALL {
            assert!(!c.probe_module().is_empty(), "{c:?} has no import probe");
            assert!(!c.requirements().is_empty(), "{c:?} has no pip requirements");
        }
        assert!(Component::WhisperX.needs_torch() && Component::Demucs.needs_torch());
        assert!(!Component::FasterWhisper.needs_torch() && !Component::RealEsrgan.needs_torch());
    }

    #[test]
    fn adapter_env_points_every_component_at_its_venv_python() {
        let python = Path::new("/venvs/x/bin/python");
        let models = Path::new("/models/x");
        for c in Component::ALL {
            let env = adapter_env(*c, python, models);
            assert_eq!(env[0].0, c.env_var());
            assert_eq!(env[0].1, python.display().to_string());
        }
        let demucs = adapter_env(Component::Demucs, python, models);
        assert!(demucs.iter().any(|(k, v)| k == "TORCH_HOME" && v == &models.display().to_string()));
        let whisperx = adapter_env(Component::WhisperX, python, models);
        assert!(whisperx.iter().any(|(k, _)| k == "CINVAR_WHISPERX_CACHE"));
    }
}

