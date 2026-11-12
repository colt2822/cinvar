use std::collections::BTreeMap;
use std::path::{Component, Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::error::{Error, ErrorCode, Result};
use crate::tools::{Registry, ToolCtx, ToolSpec};
use crate::util::paths;
use crate::util::proc::{self, ProcOpts};

pub mod registry;

pub const MANIFEST_FILE: &str = "plugin.json";
pub const MAX_CAPTURE: usize = 1024 * 1024;
pub const DEFAULT_TIMEOUT_SECS: u64 = 60;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct PluginPermissions {
    pub spawn_subprocess: bool,
    pub network: bool,
    pub write_paths: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginTool {
    pub name: String,
    pub description: String,
    pub input: serde_json::Value,
    pub command: Vec<String>,
    #[serde(default = "default_timeout")]
    pub timeout_secs: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginManifest {
    pub name: String,
    pub version: String,
    pub description: String,
    pub cinvar_min: String,
    #[serde(default)]
    pub capabilities: Vec<String>,
    #[serde(default)]
    pub platforms: Vec<String>,
    #[serde(default)]
    pub requires: Vec<String>,
    #[serde(default)]
    pub permissions: PluginPermissions,
    #[serde(default)]
    pub tools: Vec<PluginTool>,
    /// Stable identifier (reverse-DNS style, e.g. `org.example.denoise`); defaults to `name`.
    #[serde(default)]
    pub id: String,
    /// Manifest format version (1 = this layout).
    #[serde(default)]
    pub schema_version: u32,
    /// Newest CINVAR this plugin is known to work with (inclusive, optional).
    #[serde(default)]
    pub cinvar_max: String,
    #[serde(default)]
    pub license: String,
    #[serde(default)]
    pub inputs: Vec<String>,
    #[serde(default)]
    pub outputs: Vec<String>,
    /// Free-text hardware needs, e.g. "NVIDIA GPU, 8 GB VRAM".
    #[serde(default)]
    pub hardware: Vec<String>,
    /// HUMAN-READABLE install instructions. CINVAR never executes install scripts.
    #[serde(default)]
    pub install: String,
    /// Health check argv, run (only when subprocess plugins are allowed) with a short timeout.
    #[serde(default)]
    pub health: Vec<String>,
    #[serde(default)]
    pub costs_money: bool,
    #[serde(default)]
    pub uses_cloud: bool,
    #[serde(default)]
    pub sends_files_off_device: bool,
}

impl PluginManifest {
    pub fn plugin_id(&self) -> String {
        if self.id.trim().is_empty() {
            self.name.clone()
        } else {
            self.id.clone()
        }
    }
}

fn default_timeout() -> u64 {
    DEFAULT_TIMEOUT_SECS
}

/// Discover plugins under `dir`, one plugin per `dir/<name>/plugin.json`.
/// A missing directory is not an error: it means no plugins are installed.
pub fn discover(dir: &Path) -> Result<Vec<PluginManifest>> {
    let mut out = Vec::new();
    if !dir.exists() {
        return Ok(out);
    }
    if !dir.is_dir() {
        return Err(Error::invalid(format!(
            "{} is not a plugin directory",
            dir.display()
        )));
    }
    let mut entries: Vec<PathBuf> = std::fs::read_dir(dir)
        .map_err(|e| Error::io(format!("could not read {}: {e}", dir.display())))?
        .filter_map(|entry| entry.ok().map(|e| e.path()))
        .collect();
    entries.sort();
    for root in entries {
        if !root.is_dir() {
            continue;
        }
        let manifest_path = root.join(MANIFEST_FILE);
        if !manifest_path.is_file() {
            continue;
        }
        let text = std::fs::read_to_string(&manifest_path)?;
        let manifest: PluginManifest = serde_json::from_str(&text)
            .map_err(|e| Error::invalid(format!("{}: {e}", manifest_path.display())))?;
        validate(&manifest)?;
        if !platform_supported(&manifest) {
            diagnostics(&format!(
                "skipping plugin '{}' v{}: supports {:?}, this host is {}",
                manifest.name,
                manifest.version,
                manifest.platforms,
                std::env::consts::OS
            ));
            continue;
        }
        remember_dir(&manifest.name, &root);
        out.push(manifest);
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(out)
}

/// Like [`discover`] but skips unreadable/invalid manifests instead of failing the whole scan.
pub fn discover_lenient(dir: &Path) -> Vec<PluginManifest> {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut roots: Vec<PathBuf> = rd.filter_map(|e| e.ok().map(|e| e.path())).collect();
    roots.sort();
    let mut out = Vec::new();
    for root in roots.into_iter().filter(|r| r.is_dir()) {
        let Ok(text) = std::fs::read_to_string(root.join(MANIFEST_FILE)) else {
            continue;
        };
        let Ok(manifest) = serde_json::from_str::<PluginManifest>(&text) else {
            continue;
        };
        if manifest.name.trim().is_empty() {
            continue;
        }
        remember_dir(&manifest.name, &root);
        out.push(manifest);
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

/// Register one tool per declared plugin tool as `plugin.<plugin>.<tool>`.
/// Every manifest is validated first, so a refusal leaves the registry untouched.
pub fn register(
    registry: &mut Registry,
    manifests: &[PluginManifest],
    allow_subprocess: bool,
) -> Result<usize> {
    let mut accepted: Vec<&PluginManifest> = Vec::new();
    for manifest in manifests {
        validate(manifest)?;
        if !platform_supported(manifest) {
            diagnostics(&format!(
                "skipping plugin '{}': not supported on {}",
                manifest.name,
                std::env::consts::OS
            ));
            continue;
        }
        if manifest.permissions.spawn_subprocess && !allow_subprocess {
            return Err(Error::new(
                ErrorCode::PermissionDenied,
                format!(
                    "plugin '{}' requests permission to spawn subprocesses and subprocess execution is disabled",
                    manifest.name
                ),
            )
            .with_hint("set security.allow_plugin_subprocess = true in the CINVAR configuration to allow it"));
        }
        for tool in &manifest.tools {
            if tool.name.trim().is_empty() {
                return Err(Error::invalid(format!(
                    "plugin '{}' declares a tool with an empty name",
                    manifest.name
                )));
            }
            let program = tool.command.first().ok_or_else(|| {
                Error::invalid(format!(
                    "plugin '{}' tool '{}' declares an empty command",
                    manifest.name, tool.name
                ))
            })?;
            check_program(program).map_err(|err| {
                Error::new(
                    err.code,
                    format!(
                        "plugin '{}' tool '{}': {}",
                        manifest.name, tool.name, err.message
                    ),
                )
            })?;
        }
        accepted.push(manifest);
    }

    let mut count = 0;
    for manifest in accepted {
        let root = known_dir(&manifest.name);
        for tool in &manifest.tools {
            let name = format!("plugin.{}.{}", manifest.name, tool.name);
            let description = if tool.description.trim().is_empty() {
                format!(
                    "Run the '{}' tool provided by the {} plugin.",
                    tool.name, manifest.name
                )
            } else {
                tool.description.clone()
            };
            let input = if tool.input.is_null() {
                serde_json::json!({"type": "object", "properties": {}})
            } else {
                tool.input.clone()
            };
            let argv = tool.command.clone();
            let timeout = tool.timeout_secs.max(1);
            let plugin_root = root.clone();
            let perms = manifest.permissions.clone();
            registry.add(
                ToolSpec {
                    name,
                    group: "plugin".to_string(),
                    description,
                    input,
                    destructive: false,
                },
                move |ctx: &mut ToolCtx, args: serde_json::Value| {
                    run_tool(plugin_root.as_deref(), &argv, timeout, &perms, ctx, &args)
                },
            );
            count += 1;
        }
    }
    Ok(count)
}

fn run_tool(
    plugin_root: Option<&Path>,
    argv: &[String],
    timeout_secs: u64,
    perms: &PluginPermissions,
    ctx: &mut ToolCtx,
    args: &serde_json::Value,
) -> Result<serde_json::Value> {
    let (program, rest) = argv
        .split_first()
        .ok_or_else(|| Error::invalid("plugin tool declares an empty command"))?;
    let program = resolve_program(plugin_root, program)?;
    // proc::run clears the environment and restores only the handful of
    // variables a well behaved child needs (PATH, HOME, temp and locale).
    let opts = ProcOpts {
        cwd: Some(ctx.store.root.clone()),
        timeout: Some(Duration::from_secs(timeout_secs)),
        stdin: Some(serde_json::to_vec(args)?),
        env: Vec::new(),
        max_capture: MAX_CAPTURE,
    };
    // Enforce the host switch at call time too: a manifest that omits `spawn_subprocess` must
    // not be able to run a command while subprocess plugins are disabled.
    if !ctx.config.security.allow_plugin_subprocess {
        return Err(Error::new(
            ErrorCode::PermissionDenied,
            "plugin commands are disabled (security.allow_plugin_subprocess is false)",
        )
        .with_hint(
            "set security.allow_plugin_subprocess = true in the CINVAR configuration to allow them",
        ));
    }
    if !(sandbox_available() || ctx.config.security.allow_unsandboxed_plugins) {
        return Err(Error::new(
            ErrorCode::PermissionDenied,
            "no sandbox (bubblewrap) is available, so plugin commands are refused",
        )
        .with_hint("install bubblewrap, or set security.allow_unsandboxed_plugins = true to accept running plugins with your full privileges"));
    }
    let (program, rest, sandboxed) =
        sandbox_command(&program, rest, plugin_root, perms, Some(&ctx.store.root));
    let output = proc::run(&program, &rest, &opts)?;
    Ok(serde_json::json!({
        "sandboxed": sandboxed,
        "stdout": crate::secrets::redact(&output.stdout),
        "stderr": crate::secrets::redact(&output.stderr),
        "exit_code": output.code,
    }))
}

/// True when a sandbox is available on this machine (bubblewrap on Linux).
pub fn sandbox_available() -> bool {
    cfg!(target_os = "linux") && proc::which("bwrap").is_some()
}

/// Plugin commands may run only when subprocess plugins are allowed AND either a sandbox exists
/// or the user explicitly accepted unsandboxed execution.
pub fn execution_allowed(security: &crate::config::Security) -> bool {
    execution_allowed_with(security, sandbox_available())
}

pub fn execution_allowed_with(security: &crate::config::Security, sandbox: bool) -> bool {
    security.allow_plugin_subprocess && (sandbox || security.allow_unsandboxed_plugins)
}

/// Wraps a plugin command in bubblewrap: read-only view of the system, an empty home directory
/// and an empty CINVAR config directory (so `credentials.toml` is invisible even when it lives
/// outside `$HOME`), write access only to `writable` (the project; `None` for health checks),
/// and no network unless the manifest asked for it. Returns `sandboxed == false` when no
/// sandbox exists; callers must refuse unless the user opted in (`execution_allowed`).
pub fn sandbox_command(
    program: &Path,
    rest: &[String],
    plugin_root: Option<&Path>,
    perms: &PluginPermissions,
    writable: Option<&Path>,
) -> (PathBuf, Vec<String>, bool) {
    #[cfg(target_os = "linux")]
    if let Some(bwrap) = proc::which("bwrap") {
        let s = |p: &Path| p.to_string_lossy().to_string();
        let mut a: Vec<String> = [
            "--ro-bind",
            "/",
            "/",
            "--dev",
            "/dev",
            "--proc",
            "/proc",
            "--tmpfs",
            "/tmp",
            "--die-with-parent",
            "--new-session",
            "--unshare-pid",
            "--unshare-ipc",
            "--unshare-uts",
            "--cap-drop",
            "ALL",
        ]
        .iter()
        .map(|x| x.to_string())
        .collect();
        // A read-only bind does not stop connect() on host Unix sockets (user systemd/D-Bus,
        // gpg-agent, docker...). Hide /run (and /var/run, a symlink to it); with network
        // allowed keep the rest of /run (DNS) but mask the known control sockets.
        if !perms.network {
            a.extend(["--tmpfs".into(), "/run".into()]);
        } else {
            for dir in ["/run/user", "/run/dbus"] {
                if Path::new(dir).is_dir() {
                    a.extend(["--tmpfs".into(), dir.into()]);
                }
            }
            for sock in ["/run/docker.sock", "/run/podman/podman.sock"] {
                if Path::new(sock).exists() {
                    a.extend(["--ro-bind".into(), "/dev/null".into(), sock.into()]);
                }
            }
        }
        a.extend(["--tmpfs".into(), s(&crate::util::paths::home_dir())]);
        let cfg = crate::util::paths::config_dir();
        if cfg.is_dir() {
            a.extend(["--tmpfs".into(), s(&cfg)]);
        }
        if let Some(root) = plugin_root {
            a.extend(["--ro-bind".into(), s(root), s(root)]);
        }
        if let Some(w) = writable {
            a.extend(["--bind".into(), s(w), s(w)]);
        }
        if !perms.network {
            a.push("--unshare-net".into());
        }
        if let Some(dir) = writable.or(plugin_root) {
            a.extend(["--chdir".into(), s(dir)]);
        }
        a.extend(["--".into(), s(program)]);
        a.extend(rest.iter().cloned());
        return (bwrap, a, true);
    }
    let _ = (plugin_root, perms, writable);
    (program.to_path_buf(), rest.to_vec(), false)
}

fn resolve_program(plugin_root: Option<&Path>, program: &str) -> Result<PathBuf> {
    check_program(program)?;
    let relative = Path::new(program);
    if !program.contains('/') && !program.contains('\\') {
        if let Some(found) = proc::which(program) {
            return Ok(found);
        }
    }
    let Some(root) = plugin_root else {
        return Ok(relative.to_path_buf());
    };
    let root = paths::normalize(root);
    let resolved = paths::normalize(&root.join(relative));
    if !resolved.starts_with(&root) {
        return Err(Error::new(
            ErrorCode::PermissionDenied,
            format!("plugin command '{program}' resolves outside the plugin directory"),
        ));
    }
    Ok(resolved)
}

fn check_program(program: &str) -> Result<()> {
    if program.trim().is_empty() {
        return Err(Error::invalid("plugin command must not be empty"));
    }
    let path = Path::new(program);
    let escapes = path.is_absolute()
        || path.components().any(|c| {
            matches!(
                c,
                Component::ParentDir | Component::RootDir | Component::Prefix(_)
            )
        });
    if escapes {
        return Err(Error::new(
            ErrorCode::PermissionDenied,
            format!(
                "plugin command '{program}' must be a relative path inside the plugin directory"
            ),
        )
        .with_hint("a plugin may only run programs shipped inside its own directory"));
    }
    Ok(())
}

fn validate(manifest: &PluginManifest) -> Result<()> {
    if manifest.name.trim().is_empty() {
        return Err(Error::invalid("plugin manifest is missing 'name'"));
    }
    if manifest.version.trim().is_empty() {
        return Err(Error::invalid(format!(
            "plugin '{}' is missing 'version'",
            manifest.name
        )));
    }
    let required = version_parts(&manifest.cinvar_min).ok_or_else(|| {
        Error::invalid(format!(
            "plugin '{}' has an invalid cinvar_min '{}': expected a dotted numeric version",
            manifest.name, manifest.cinvar_min
        ))
    })?;
    let have = version_parts(crate::VERSION).unwrap_or_default();
    if !version_at_least(&have, &required) {
        return Err(Error::new(
            ErrorCode::Unsupported,
            format!(
                "plugin '{}' requires CINVAR {} or newer, this build is {}",
                manifest.name,
                manifest.cinvar_min,
                crate::VERSION
            ),
        )
        .with_hint("upgrade CINVAR, or install a release of the plugin that supports this build"));
    }
    Ok(())
}

pub(crate) fn platform_supported(manifest: &PluginManifest) -> bool {
    manifest.platforms.is_empty()
        || manifest
            .platforms
            .iter()
            .any(|p| p.trim().eq_ignore_ascii_case(std::env::consts::OS))
}

pub(crate) fn version_parts(version: &str) -> Option<Vec<u64>> {
    if version.trim().is_empty() {
        return None;
    }
    let mut parts = Vec::new();
    for part in version.trim().split('.') {
        parts.push(part.trim().parse::<u64>().ok()?);
    }
    Some(parts)
}

pub(crate) fn version_at_least(have: &[u64], need: &[u64]) -> bool {
    for index in 0..have.len().max(need.len()) {
        let a = *have.get(index).unwrap_or(&0);
        let b = *need.get(index).unwrap_or(&0);
        if a != b {
            return a > b;
        }
    }
    true
}

/// discover() records where each plugin lives so register() can resolve a
/// relative command against the plugin's own directory rather than the cwd.
fn dirs() -> &'static Mutex<BTreeMap<String, PathBuf>> {
    static DIRS: OnceLock<Mutex<BTreeMap<String, PathBuf>>> = OnceLock::new();
    DIRS.get_or_init(|| Mutex::new(BTreeMap::new()))
}

fn remember_dir(name: &str, dir: &Path) {
    if let Ok(mut guard) = dirs().lock() {
        guard.insert(name.to_string(), dir.to_path_buf());
    }
}

fn known_dir(name: &str) -> Option<PathBuf> {
    dirs()
        .lock()
        .ok()
        .and_then(|guard| guard.get(name).cloned())
}

fn diagnostics(message: &str) {
    use std::io::Write;
    let _ = writeln!(std::io::stderr(), "cinvar: {message}");
}

#[cfg(test)]
mod tests {
    use super::*;

    fn manifest_with(command: &str, spawn: bool, cinvar_min: &str) -> PluginManifest {
        PluginManifest {
            name: "demo".into(),
            version: "1.0.0".into(),
            description: "demo".into(),
            cinvar_min: cinvar_min.into(),
            capabilities: Vec::new(),
            platforms: Vec::new(),
            requires: Vec::new(),
            permissions: PluginPermissions {
                spawn_subprocess: spawn,
                ..Default::default()
            },
            tools: vec![PluginTool {
                name: "echo".into(),
                description: "echo".into(),
                input: serde_json::json!({"type": "object"}),
                command: vec![command.into()],
                timeout_secs: 5,
            }],
            id: String::new(),
            schema_version: 1,
            cinvar_max: String::new(),
            license: String::new(),
            inputs: Vec::new(),
            outputs: Vec::new(),
            hardware: Vec::new(),
            install: String::new(),
            health: Vec::new(),
            costs_money: false,
            uses_cloud: false,
            sends_files_off_device: false,
        }
    }

    #[test]
    fn missing_directory_is_empty() {
        let dir = tempfile::tempdir().unwrap();
        assert!(discover(&dir.path().join("absent")).unwrap().is_empty());
    }

    #[test]
    fn compares_dotted_versions() {
        assert!(version_at_least(
            &version_parts("0.1.0").unwrap(),
            &version_parts("0.1.0").unwrap()
        ));
        assert!(version_at_least(
            &version_parts("0.2.0").unwrap(),
            &version_parts("0.1.9").unwrap()
        ));
        assert!(version_at_least(
            &version_parts("0.1.1").unwrap(),
            &version_parts("0.1").unwrap()
        ));
        assert!(!version_at_least(
            &version_parts("0.1.0").unwrap(),
            &version_parts("0.2").unwrap()
        ));
        assert!(version_parts("0.x.0").is_none());
        assert!(version_parts("  ").is_none());
    }

    #[test]
    fn rejects_escaping_commands() {
        assert!(check_program("/usr/bin/env").is_err());
        assert!(check_program("../escape.sh").is_err());
        assert!(check_program("../../bin/tool").is_err());
        assert!(check_program("").is_err());
        assert!(check_program("./run.sh").is_ok());
        assert!(check_program("bin/tool").is_ok());
    }

    #[test]
    fn refuses_subprocess_without_permission() {
        let manifest = manifest_with("./run.sh", true, "0.1.0");
        let mut registry = Registry::new();
        let err = register(&mut registry, std::slice::from_ref(&manifest), false).unwrap_err();
        assert_eq!(err.code, ErrorCode::PermissionDenied);
        assert_eq!(registry.len(), 0);
    }

    #[test]
    fn refuses_manifest_for_newer_cinvar() {
        let manifest = manifest_with("./run.sh", false, "99.0.0");
        let mut registry = Registry::new();
        assert_eq!(
            register(&mut registry, &[manifest], true).unwrap_err().code,
            ErrorCode::Unsupported
        );
    }

    #[test]
    fn refuses_escaping_command_at_registration() {
        let manifest = manifest_with("../escape.sh", false, "0.1.0");
        let mut registry = Registry::new();
        assert_eq!(
            register(&mut registry, &[manifest], true).unwrap_err().code,
            ErrorCode::PermissionDenied
        );
        assert_eq!(registry.len(), 0);
    }
}

#[cfg(all(test, target_os = "linux"))]
mod sandbox_tests {
    use super::*;

    fn run_sh(script: &str, project: &Path, network: bool) -> (bool, String) {
        let perms = PluginPermissions {
            network,
            ..Default::default()
        };
        let (prog, args, sandboxed) = sandbox_command(
            Path::new("/bin/sh"),
            &["-c".to_string(), script.to_string()],
            None,
            &perms,
            Some(project),
        );
        let opts = proc::ProcOpts {
            cwd: Some(project.to_path_buf()),
            timeout: Some(Duration::from_secs(20)),
            stdin: None,
            env: Vec::new(),
            max_capture: 65536,
        };
        let out = proc::run(&prog, &args, &opts).unwrap();
        (sandboxed, format!("{}{}", out.stdout, out.stderr))
    }

    #[test]
    fn without_a_sandbox_plugins_are_refused_unless_explicitly_accepted() {
        let mut sec = crate::config::Security::default();
        assert!(
            !execution_allowed_with(&sec, true),
            "subprocess plugins are off by default"
        );
        sec.allow_plugin_subprocess = true;
        assert!(execution_allowed_with(&sec, true));
        assert!(
            !execution_allowed_with(&sec, false),
            "fail closed without a sandbox"
        );
        sec.allow_unsandboxed_plugins = true;
        assert!(execution_allowed_with(&sec, false), "explicit opt-in");
    }

    #[test]
    fn plugin_cannot_read_home_write_outside_project_or_use_network() {
        if proc::which("bwrap").is_none() {
            eprintln!("bwrap not installed; skipping");
            return;
        }
        let home = crate::util::paths::home_dir();
        let canary = home.join(format!(".cinvar-sandbox-canary-{}", std::process::id()));
        std::fs::write(&canary, "secret").unwrap();
        let project = tempfile::tempdir().unwrap();
        let script = format!(
            "cat '{}' 2>&1 && echo HOME_READ; echo x > /var/tmp/cinvar-sbx-{} 2>&1 && echo OUTSIDE_WRITE; echo ok > inside.txt && echo INSIDE_WRITE; grep -c : /proc/net/dev",
            canary.display(), std::process::id()
        );
        let (sandboxed, text) = run_sh(&script, project.path(), false);
        let _ = std::fs::remove_file(&canary);
        let _ = std::fs::remove_file(format!("/var/tmp/cinvar-sbx-{}", std::process::id()));
        assert!(sandboxed);
        assert!(!text.contains("HOME_READ"), "{text}");
        assert!(!text.contains("OUTSIDE_WRITE"), "{text}");
        assert!(text.contains("INSIDE_WRITE"), "{text}");
        assert!(project.path().join("inside.txt").exists());
        // only the loopback interface exists without the network permission
        assert!(text.lines().any(|l| l.trim() == "1"), "{text}");
    }
}
