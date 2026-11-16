//! Subprocess plumbing for the Pro AI installer: virtualenvs, pip, import verification and model
//! weights.
//!
//! Every failure becomes a crate::error::Error whose message names the stage and command that
//! failed and whose hint carries the last output lines, the next step and the full log path, so one
//! broken component reports a fixable problem without stopping the others.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use super::{Component, Device, Facts, TorchPin};
use crate::error::{Error, ErrorCode, Result};
use crate::util::paths;
use crate::util::proc::{self, ProcOpts, ProcOutput};

const VENV_TIMEOUT_SECS: u64 = 300;
const PIP_TIMEOUT_SECS: u64 = 7200;
const PROBE_TIMEOUT_SECS: u64 = 600;
const WEIGHTS_TIMEOUT_SECS: u64 = 3600;
const TAIL_LINES: usize = 6;

/// Directories the installer owns.
pub struct Layout {
    pub venvs: PathBuf,
    pub models: PathBuf,
    pub logs: PathBuf,
    pub pip_cache: PathBuf,
}

impl Layout {
    pub fn resolve() -> Result<Self> {
        let data = paths::data_dir();
        let layout = Layout {
            venvs: data.join("venvs"),
            models: super::pins::model_root(),
            logs: data.join("logs"),
            pip_cache: data.join("pip-cache"),
        };
        for dir in [&layout.venvs, &layout.models, &layout.logs, &layout.pip_cache] {
            paths::ensure_dir(dir)?;
        }
        Ok(layout)
    }

    pub fn venv(&self, c: Component) -> PathBuf {
        self.venvs.join(c.as_str())
    }

    pub fn models_for(&self, c: Component) -> PathBuf {
        self.models.join(c.as_str())
    }

    fn log_path(&self, c: Component) -> PathBuf {
        self.logs.join(format!("setup-{}.log", c.as_str()))
    }
}

fn say(step: &str, message: &str) {
    eprintln!("{step} {message}");
}

fn one_line(text: &str, max: usize) -> String {
    let flat = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() <= max {
        flat
    } else {
        let mut out: String = flat.chars().take(max.saturating_sub(1)).collect();
        out.push('…');
        out
    }
}

fn command_line(program: &Path, args: &[String]) -> String {
    let name = program
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| program.display().to_string());
    format!("{name} {}", args.join(" "))
}

fn log_block(layout: &Layout, c: Component, stage: &str, command: &str, out: &ProcOutput, elapsed: Duration) {
    let path = layout.log_path(c);
    let Ok(mut file) = std::fs::OpenOptions::new().create(true).append(true).open(&path) else {
        return;
    };
    let _ = writeln!(
        file,
        "=== {stage} ({}s, exit {}) $ {command}\n--- stdout\n{}\n--- stderr\n{}",
        elapsed.as_secs(),
        out.code.map(|code| code.to_string()).unwrap_or_else(|| "signal".into()),
        out.stdout.trim_end(),
        out.stderr.trim_end()
    );
}

/// Run one stage, log it, and turn a non-zero exit into an error that names the command.
fn run_stage(
    layout: &Layout,
    c: Component,
    program: &Path,
    args: &[String],
    opts: &ProcOpts,
    stage: &str,
    next: &str,
) -> Result<ProcOutput> {
    let started = Instant::now();
    let command = command_line(program, args);
    let out = proc::run(program, args, opts).map_err(|e| {
        e.clone()
            .with_hint(format!("{stage} could not start; next: {next} | command: {command}"))
    })?;
    log_block(layout, c, stage, &command, &out, started.elapsed());
    if !out.success {
        let stderr = out.tail_stderr(TAIL_LINES);
        let detail = if stderr.trim().is_empty() {
            one_line(out.stdout.trim(), 400)
        } else {
            one_line(stderr.trim(), 600)
        };
        return Err(Error::new(
            ErrorCode::DependencyMissing,
            format!(
                "{stage} failed (exit {}) after {}s",
                out.code.map(|code| code.to_string()).unwrap_or_else(|| "signal".into()),
                started.elapsed().as_secs()
            ),
        )
        .with_hint(format!(
            "{detail} | next: {next} | command: {} | log: {}",
            one_line(&command, 240),
            layout.log_path(c).display()
        )));
    }
    Ok(out)
}

fn base_python() -> Result<PathBuf> {
    if let Ok(value) = std::env::var("CINVAR_PRO_AI_PYTHON") {
        if !value.trim().is_empty() {
            let path = PathBuf::from(&value);
            return if path.is_file() {
                Ok(path)
            } else {
                Err(Error::invalid(format!(
                    "CINVAR_PRO_AI_PYTHON points at {}, which is not a file",
                    path.display()
                )))
            };
        }
    }
    proc::which("python3").or_else(|| proc::which("python")).ok_or_else(|| {
        Error::new(ErrorCode::DependencyMissing, "no python3 interpreter found on PATH").with_hint(
            "install Python 3.10+ (Debian/Ubuntu: sudo apt install python3 python3-venv), then re-run cinvar setup --pro-ai",
        )
    })
}

fn venv_python(venv: &Path) -> PathBuf {
    if cfg!(windows) {
        venv.join("Scripts").join("python.exe")
    } else {
        venv.join("bin").join("python")
    }
}

fn pip_env(layout: &Layout) -> Vec<(String, String)> {
    vec![
        ("PIP_CACHE_DIR".into(), layout.pip_cache.display().to_string()),
        ("PIP_DISABLE_PIP_VERSION_CHECK".into(), "1".into()),
        ("PIP_NO_INPUT".into(), "1".into()),
    ]
}

fn pip_args() -> Vec<String> {
    vec!["-m".into(), "pip".into(), "install".into(), "--no-input".into()]
}

/// Create the component's virtualenv unless it already exists; returns its interpreter.
pub fn ensure_venv(layout: &Layout, c: Component, step: &str) -> Result<PathBuf> {
    let venv = layout.venv(c);
    let python = venv_python(&venv);
    if python.is_file() {
        say(step, &format!("reusing virtualenv {}", venv.display()));
        return Ok(python);
    }
    let base = base_python()?;
    say(step, &format!("creating virtualenv {}", venv.display()));
    paths::ensure_dir(&layout.venvs)?;
    let args = vec!["-m".to_string(), "venv".to_string(), venv.display().to_string()];
    let opts = ProcOpts::default().with_timeout(VENV_TIMEOUT_SECS);
    run_stage(
        layout,
        c,
        &base,
        &args,
        &opts,
        "virtualenv creation",
        "install python3-venv (Debian/Ubuntu: sudo apt install python3-venv), then re-run cinvar setup --pro-ai",
    )?;
    if !python.is_file() {
        return Err(Error::new(
            ErrorCode::DependencyMissing,
            format!(
                "{} was created but has no interpreter at {}",
                venv.display(),
                python.display()
            ),
        )
        .with_hint("install python3-venv (Debian/Ubuntu: sudo apt install python3-venv) and re-run"));
    }
    Ok(python)
}

/// Install the pinned torch stack (torch, torchaudio and the matching torchvision) from one wheel
/// channel, so pip can never mix two CUDA builds in one environment.
pub fn install_torch(layout: &Layout, c: Component, python: &Path, device: Device, step: &str) -> Result<()> {
    let pin = device.pin();
    say(
        step,
        &format!(
            "installing torch {} + torchaudio {} from {} (multi-GB; later components reuse the pip cache)",
            pin.torch, pin.torchaudio, pin.index
        ),
    );
    let mut args = pip_args();
    args.push("--index-url".into());
    args.push(pin.index.into());
    args.push("--extra-index-url".into());
    args.push("https://pypi.org/simple".into());
    for requirement in pin.requirements() {
        args.push(requirement);
    }
    let mut opts = ProcOpts::default().with_timeout(PIP_TIMEOUT_SECS);
    opts.env = pip_env(layout);
    let next = format!(
        "check that this Python has wheels for torch {} (set CINVAR_PRO_AI_DEVICE=cpu to use the CPU build)",
        pin.torch
    );
    run_stage(layout, c, python, &args, &opts, "torch install", &next)?;
    Ok(())
}

/// The acceptance probe: both wheels import together and report the pinned pair.
pub fn verify_torch_pair(layout: &Layout, c: Component, python: &Path, pin: TorchPin) -> Result<String> {
    const PROBE: &str = "import torch, torchaudio; print(torch.__version__, torchaudio.__version__)";
    let args = vec!["-c".to_string(), PROBE.to_string()];
    let mut opts = ProcOpts::default().with_timeout(PROBE_TIMEOUT_SECS);
    opts.env = vec![("PYTHONNOUSERSITE".into(), "1".into())];
    let out = run_stage(
        layout,
        c,
        python,
        &args,
        &opts,
        "torch/torchaudio import",
        "delete the venv and re-run cinvar setup --pro-ai so the pair is installed in one pass",
    )?;
    let pair = out.stdout.trim().to_string();
    for expected in [pin.torch, pin.torchaudio] {
        if !pair.contains(expected) {
            return Err(Error::new(
                ErrorCode::DependencyMissing,
                format!("torch/torchaudio imported but reported '{pair}' instead of the pinned pair"),
            )
            .with_hint(format!(
                "expected {} and {}; delete {} and re-run cinvar setup --pro-ai",
                pin.torch,
                pin.torchaudio,
                layout.venv(c).display()
            )));
        }
    }
    Ok(pair)
}

/// Install the component's own requirements into the venv.
pub fn install_requirements(layout: &Layout, c: Component, python: &Path, step: &str) -> Result<()> {
    let requirements = c.requirements();
    say(step, &format!("installing {}", requirements.join(" ")));
    let mut args = pip_args();
    args.extend(requirements.iter().map(|r| r.to_string()));
    let mut opts = ProcOpts::default().with_timeout(PIP_TIMEOUT_SECS);
    opts.env = pip_env(layout);
    run_stage(
        layout,
        c,
        python,
        &args,
        &opts,
        &format!("pip install {}", requirements.join(" ")),
        "re-run cinvar setup --pro-ai to resume from the cached wheels",
    )?;
    Ok(())
}

/// Import the module the adapter runs, so "installed" means "importable", not "pip exited 0".
pub fn probe_module(layout: &Layout, c: Component, python: &Path, step: &str) -> Result<()> {
    let module = c.probe_module();
    say(step, &format!("verifying import {module}"));
    let args = vec!["-c".to_string(), format!("import {module}; print('{module} ok')")];
    let mut opts = ProcOpts::default().with_timeout(PROBE_TIMEOUT_SECS);
    opts.env = vec![("PYTHONNOUSERSITE".into(), "1".into())];
    if c == Component::RealEsrgan {
        // Importing the ncnn wrapper loads libomp.so.5, exactly as the adapter's child process does.
        if let Some((key, value)) = shim_env(&layout.venv(c)) {
            opts.env.push((key, value));
        }
    }
    run_stage(
        layout,
        c,
        python,
        &args,
        &opts,
        &format!("import {module}"),
        "re-run cinvar setup --pro-ai after fixing the import error above",
    )?;
    Ok(())
}

/// LD_LIBRARY_PATH for the Real-ESRGAN child, matching what the adapter prepends.
fn shim_env(venv: &Path) -> Option<(String, String)> {
    let shim = venv.join("lib-shim");
    if !shim.is_dir() {
        return None;
    }
    let mut value = shim.display().to_string();
    if let Ok(existing) = std::env::var("LD_LIBRARY_PATH") {
        if !existing.trim().is_empty() {
            value.push(':');
            value.push_str(&existing);
        }
    }
    Some(("LD_LIBRARY_PATH".to_string(), value))
}

/// Link `libomp.so.5` (what the ncnn build links) to `libgomp.so.1` (what Linux ships) inside
/// `<venv>/lib-shim`, the directory the Real-ESRGAN adapter prepends to `LD_LIBRARY_PATH`.
pub fn omp_shim(venv: &Path, step: &str) -> Result<PathBuf> {
    let shim = venv.join("lib-shim");
    let link = shim.join("libomp.so.5");
    if std::fs::symlink_metadata(&link).is_ok() {
        say(step, &format!("reusing OpenMP shim {}", link.display()));
        return Ok(shim);
    }
    let source = find_libgomp().ok_or_else(|| {
        Error::new(
            ErrorCode::DependencyMissing,
            "libgomp.so.1 not found: Real-ESRGAN's ncnn build needs an OpenMP runtime",
        )
        .with_hint("install it (Debian/Ubuntu: sudo apt install libgomp1), then re-run cinvar setup --pro-ai")
    })?;
    make_omp_shim(&shim, &source)?;
    say(step, &format!("OpenMP shim {} -> {}", link.display(), source.display()));
    Ok(shim)
}

fn make_omp_shim(shim: &Path, source: &Path) -> Result<PathBuf> {
    paths::ensure_dir(shim)?;
    let link = shim.join("libomp.so.5");
    let _ = std::fs::remove_file(&link);
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(source, &link).map_err(|e| {
            Error::io(format!("cannot link {} -> {}: {e}", link.display(), source.display()))
        })?;
    }
    #[cfg(not(unix))]
    {
        std::fs::copy(source, &link).map_err(|e| {
            Error::io(format!("cannot copy the OpenMP runtime to {}: {e}", link.display()))
        })?;
    }
    Ok(link)
}

/// Usual multiarch locations first, then the dynamic loader cache (covers exotic distros).
pub fn find_libgomp() -> Option<PathBuf> {
    for candidate in [
        "/usr/lib/x86_64-linux-gnu/libgomp.so.1",
        "/usr/lib/aarch64-linux-gnu/libgomp.so.1",
        "/usr/lib64/libgomp.so.1",
        "/usr/lib/libgomp.so.1",
        "/usr/local/lib/libgomp.so.1",
    ] {
        let path = PathBuf::from(candidate);
        if path.exists() {
            return Some(path);
        }
    }
    let exe = proc::which("ldconfig")?;
    let args = vec!["-p".to_string()];
    let out = proc::run(&exe, &args, &ProcOpts::default().with_timeout(30)).ok()?;
    out.stdout.lines().find_map(|line| {
        let line = line.trim();
        if !line.starts_with("libgomp.so.1 ") {
            return None;
        }
        let target = line.split("=>").nth(1)?.trim();
        let path = PathBuf::from(target);
        path.exists().then_some(path)
    })
}

/// Python that downloads exactly the weights this component loads, using the component's own
/// loader so a version bump cannot silently point at the wrong cache layout. `None` means the
/// wheel already bundles them.
fn weights_script(c: Component) -> Option<&'static str> {
    match c {
        Component::WhisperX => Some(
            r#"import inspect, os, whisperx
root = os.environ["CINVAR_WEIGHTS_ROOT"]
kw = {}
if "download_root" in inspect.signature(whisperx.load_model).parameters:
    kw["download_root"] = root
whisperx.load_model(os.environ.get("CINVAR_WHISPERX_MODEL", "base"), device="cpu", compute_type="int8", **kw)
akw = {}
if "model_dir" in inspect.signature(whisperx.load_align_model).parameters:
    akw["model_dir"] = os.path.join(root, "align")
whisperx.load_align_model(language_code=os.environ.get("CINVAR_WHISPERX_LANG", "en"), device="cpu", **akw)
print("whisperx weights ready")"#,
        ),
        Component::FasterWhisper => Some(
            r#"import os
from faster_whisper import WhisperModel
root = os.environ["CINVAR_WEIGHTS_ROOT"]
WhisperModel(os.environ.get("CINVAR_FASTER_WHISPER_MODEL", "tiny"), device="cpu", compute_type="int8", download_root=root)
print("faster-whisper weights ready")"#,
        ),
        Component::Demucs => Some(
            r#"import os
from demucs.pretrained import get_model
get_model(os.environ.get("CINVAR_DEMUCS_MODEL", "htdemucs"))
print("demucs weights ready")"#,
        ),
        Component::RealEsrgan => None,
    }
}

/// Download this component's weights unless they are already there. One component's download
/// failing is that component's problem, never the run's.
pub fn ensure_models(layout: &Layout, c: Component, python: &Path, step: &str) -> Result<PathBuf> {
    let root = layout.models_for(c);
    paths::ensure_dir(&root)?;
    let marker = root.join(".cinvar-weights-ok");
    if marker.is_file() {
        say(step, &format!("weights already in {}", root.display()));
        return Ok(root);
    }
    let Some(script) = weights_script(c) else {
        say(step, "no weights to download (the wheel bundles them)");
        write_marker(&marker)?;
        return Ok(root);
    };
    say(step, &format!("downloading weights into {}", root.display()));
    let args = vec!["-c".to_string(), script.to_string()];
    let mut opts = ProcOpts::default().with_timeout(WEIGHTS_TIMEOUT_SECS).in_dir(&root);
    opts.env = vec![
        ("CINVAR_WEIGHTS_ROOT".into(), root.display().to_string()),
        ("HF_HOME".into(), root.display().to_string()),
        ("TORCH_HOME".into(), root.display().to_string()),
        ("XDG_CACHE_HOME".into(), root.display().to_string()),
        ("HF_HUB_DISABLE_TELEMETRY".into(), "1".into()),
    ];
    run_stage(
        layout,
        c,
        python,
        &args,
        &opts,
        "model weights download",
        "check access to huggingface.co (or place the weights in that directory by hand), then re-run cinvar setup --pro-ai",
    )?;
    write_marker(&marker)?;
    if c == Component::Demucs {
        publish_torch_hub(&root);
    }
    Ok(root)
}

fn write_marker(marker: &Path) -> Result<()> {
    std::fs::write(marker, b"downloaded by cinvar setup --pro-ai\n")
        .map_err(|e| Error::io(format!("cannot write {}: {e}", marker.display())))
}

/// Demucs resolves its checkpoint through `torch.hub`, whose directory follows `TORCH_HOME` or
/// `$HOME/.cache/torch`. The adapter clears the child environment down to `HOME`, so the cached
/// checkpoint is also linked into `$HOME/.cache/torch/hub/checkpoints` to work offline as-is.
fn publish_torch_hub(root: &Path) {
    let Ok(entries) = std::fs::read_dir(root.join("hub").join("checkpoints")) else {
        return;
    };
    let home = paths::home_dir().join(".cache").join("torch").join("hub").join("checkpoints");
    if paths::ensure_dir(&home).is_err() {
        return;
    }
    for entry in entries.flatten() {
        let source = entry.path();
        if !source.is_file() {
            continue;
        }
        let Some(name) = source.file_name() else {
            continue;
        };
        let link = home.join(name);
        if std::fs::symlink_metadata(&link).is_ok() {
            continue;
        }
        #[cfg(unix)]
        {
            let _ = std::os::unix::fs::symlink(&source, &link);
        }
        #[cfg(not(unix))]
        {
            let _ = std::fs::copy(&source, &link);
        }
    }
}

/// Record every resolved path in `~/.config/cinvar/pro-ai.toml`.
pub fn write_manifest(layout: &Layout, device: Device, entries: &[(super::ComponentReport, Facts)]) -> Result<PathBuf> {
    let pin = device.pin();
    let mut root = toml::value::Table::new();
    root.insert("schema".into(), toml::Value::Integer(1));
    root.insert("device".into(), toml::Value::String(device.as_str().into()));
    root.insert("torch".into(), toml::Value::String(pin.torch.into()));
    root.insert("torchaudio".into(), toml::Value::String(pin.torchaudio.into()));
    root.insert("index".into(), toml::Value::String(pin.index.into()));
    root.insert("venvs".into(), toml::Value::String(layout.venvs.display().to_string()));
    root.insert("models".into(), toml::Value::String(layout.models.display().to_string()));
    let mut components = toml::value::Table::new();
    for (report, facts) in entries {
        let mut table = toml::value::Table::new();
        table.insert("venv".into(), toml::Value::String(report.venv.clone()));
        table.insert("installed".into(), toml::Value::Boolean(report.installed));
        table.insert("problem".into(), toml::Value::String(report.problem.clone()));
        if !facts.torch_pair.is_empty() {
            table.insert("torch_pair".into(), toml::Value::String(facts.torch_pair.clone()));
        }
        if let Some(shim) = &facts.shim {
            table.insert("lib_shim".into(), toml::Value::String(shim.display().to_string()));
        }
        let mut env = toml::value::Table::new();
        for (key, value) in &facts.env {
            env.insert(key.clone(), toml::Value::String(value.clone()));
        }
        if !env.is_empty() {
            table.insert("env".into(), toml::Value::Table(env));
        }
        components.insert(report.component.clone(), toml::Value::Table(table));
    }
    root.insert("components".into(), toml::Value::Table(components));
    let text = toml::to_string_pretty(&toml::Value::Table(root))
        .map_err(|e| Error::internal(format!("cannot render the setup manifest: {e}")))?;
    let path = paths::config_dir().join("pro-ai.toml");
    if let Some(parent) = path.parent() {
        paths::ensure_dir(parent)?;
    }
    paths::write_atomic(
        &path,
        format!("# CINVAR Pro AI setup, written by cinvar setup --pro-ai\n{text}").as_bytes(),
    )?;

    // Also record the same paths in the main configuration. This is what makes the promise real:
    // paths::resolve_helper reads Config.helpers, so a customer who ran setup needs no
    // CINVAR_*_PYTHON environment variables at all. pro-ai.toml stays the human-readable record.
    let mut config = crate::config::Config::load().unwrap_or_default();
    let mut recorded = 0usize;
    for (_report, facts) in entries {
        for (key, value) in &facts.env {
            if key.starts_with("CINVAR_") && !value.trim().is_empty() {
                config.set_helper(key, value);
                recorded += 1;
            }
        }
    }
    if recorded > 0 {
        config.save()?;
    }
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::setup::ComponentReport;

    fn layout_in(dir: &Path) -> Layout {
        Layout {
            venvs: dir.join("venvs"),
            models: dir.join("models"),
            logs: dir.join("logs"),
            pip_cache: dir.join("pip-cache"),
        }
    }

    #[test]
    fn omp_shim_links_libomp_to_the_system_openmp_runtime() {
        let dir = tempfile::tempdir().unwrap();
        let fake = dir.path().join("libgomp.so.1.0.0");
        std::fs::write(&fake, b"not really a library").unwrap();
        let shim = dir.path().join("venv").join("lib-shim");
        let link = make_omp_shim(&shim, &fake).unwrap();
        assert_eq!(link.file_name().unwrap(), "libomp.so.5");
        assert!(link.exists(), "shim target does not resolve");
        #[cfg(unix)]
        assert_eq!(std::fs::read_link(&link).unwrap(), fake);
    }

    #[test]
    fn shim_env_ignores_a_venv_without_a_shim() {
        let dir = tempfile::tempdir().unwrap();
        assert!(shim_env(dir.path()).is_none());
        std::fs::create_dir(dir.path().join("lib-shim")).unwrap();
        let (key, value) = shim_env(dir.path()).unwrap();
        assert_eq!(key, "LD_LIBRARY_PATH");
        assert!(value.starts_with(&dir.path().join("lib-shim").display().to_string()));
    }

    #[test]
    fn manifest_carries_per_component_env_and_parses_back() {
        let dir = tempfile::tempdir().unwrap();
        let layout = layout_in(dir.path());
        let entries = vec![(
            ComponentReport {
                component: "whisperx".into(),
                venv: "/tmp/venvs/whisperx".into(),
                installed: true,
                problem: String::new(),
            },
            Facts {
                torch_pair: "2.8.0+cu128 2.8.0+cu128".into(),
                shim: None,
                env: vec![(
                    "CINVAR_WHISPERX_PYTHON".into(),
                    "/tmp/venvs/whisperx/bin/python".into(),
                )],
            },
        )];
        let path = write_manifest(&layout, Device::Cpu, &entries).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        let parsed: toml::Value = toml::from_str(&text).unwrap();
        assert_eq!(parsed["torch"].as_str().unwrap(), "2.8.0+cpu");
        assert!(parsed["components"]["whisperx"]["installed"].as_bool().unwrap());
        assert_eq!(
            parsed["components"]["whisperx"]["env"]["CINVAR_WHISPERX_PYTHON"]
                .as_str()
                .unwrap(),
            "/tmp/venvs/whisperx/bin/python"
        );
        let _ = std::fs::remove_file(&path);
    }
}

