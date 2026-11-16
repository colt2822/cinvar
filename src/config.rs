use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use crate::util::paths;

pub const CONFIG_VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ModelProfile {
    pub provider: String,
    pub model: String,
    pub base_url: Option<String>,
    pub api_key_env: Option<String>,
    pub context_window: u32,
    pub max_output_tokens: u32,
    pub temperature: f64,
    pub supports_tools: Option<bool>,
    pub supports_images: Option<bool>,
    pub label: Option<String>,
}

impl Default for ModelProfile {
    fn default() -> Self {
        ModelProfile {
            provider: "mock".into(),
            model: "mock-planner".into(),
            base_url: None,
            api_key_env: None,
            context_window: 128_000,
            max_output_tokens: 4_096,
            temperature: 0.2,
            supports_tools: None,
            supports_images: None,
            label: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Budget {
    pub max_iterations: u32,
    pub max_model_calls: u32,
    pub max_tokens: u64,
    pub max_task_seconds: u64,
    pub max_usd: f64,
}

impl Default for Budget {
    fn default() -> Self {
        Budget {
            max_iterations: 30,
            max_model_calls: 60,
            max_tokens: 400_000,
            max_task_seconds: 3_600,
            max_usd: 5.0,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Security {
    pub allow_network: bool,
    pub allow_media_upload: bool,
    pub allow_plugin_subprocess: bool,
    /// Run plugin commands even when no sandbox (bubblewrap) is available. Off by default: with
    /// no sandbox, enabled plugins would inherit the user's full file and network access.
    pub allow_unsandboxed_plugins: bool,
    pub allow_blender: bool,
    /// Sending a project to a paid cloud GPU. Off until the user turns it on.
    pub allow_cloud_render: bool,
    /// Extra directories (besides the project) that AI agents may read media from, and that an
    /// imported timeline may reference. Empty by default: agents are confined to the project.
    pub media_roots: Vec<std::path::PathBuf>,
}

impl Security {
    /// Folders (besides the project) that AI agents may read media from: `security.media_roots`
    /// plus the `CINVAR_MEDIA_ROOTS` path list / `--media-root` flag chosen by the human who
    /// launched the process. Empty means project-only (the default).
    pub fn effective_media_roots(&self) -> Vec<std::path::PathBuf> {
        let mut roots = self.media_roots.clone();
        if let Some(v) = std::env::var_os("CINVAR_MEDIA_ROOTS") {
            roots.extend(std::env::split_paths(&v).filter(|p| !p.as_os_str().is_empty()));
        }
        roots
    }
}

impl Default for Security {
    fn default() -> Self {
        Security {
            allow_network: true,
            allow_media_upload: false,
            allow_plugin_subprocess: false,
            allow_unsandboxed_plugins: false,
            allow_blender: true,
            allow_cloud_render: false,
            media_roots: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct RenderDefaults {
    pub width: u32,
    pub height: u32,
    pub fps: String,
    pub video_codec: String,
    pub crf: u8,
    pub preset: String,
    pub audio_codec: String,
    pub audio_bitrate: String,
    pub hardware: String,
}

impl Default for RenderDefaults {
    fn default() -> Self {
        RenderDefaults {
            width: 1920,
            height: 1080,
            fps: "30".into(),
            video_codec: "h264".into(),
            crf: 20,
            preset: "medium".into(),
            audio_codec: "aac".into(),
            audio_bitrate: "192k".into(),
            hardware: "auto".into(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct UiSettings {
    pub port: u16,
    pub open_browser: bool,
    /// Pick skills (podcast, gaming, short-form, blender...) from what the user asks for.
    pub auto_skills: bool,
}

impl Default for UiSettings {
    fn default() -> Self {
        UiSettings {
            port: 8420,
            open_browser: false,
            auto_skills: true,
        }
    }
}

/// Free/Pro edition behaviour. See `crate::edition`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct EditionSettings {
    /// Can only tighten: `true` enforces Free limits even in a build whose default is observe-only.
    /// `false` does not turn enforcement off in release builds.
    pub enforce: bool,
    /// Endpoint that exchanges an order key for a signed license envelope
    /// (`cinvar activate <order-key>`). Empty means "use `CINVAR_LICENSE_REDEEM_URL`, then the
    /// built-in default", which ships empty because no service is deployed yet — see
    /// `crate::edition::pick_redeem_url` and docs/LICENSE_OPERATIONS.md.
    pub redeem_url: String,
}

/// Structured editing-action journal (see `crate::actions`). Local only, off by default.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct ActionSettings {
    pub record: bool,
}

/// Paid hosted video generation. Off by default; limits are in seconds of video so they hold whatever the price.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct VideoGenSettings {
    pub enabled: bool,
    pub max_job_seconds: u32,
    pub max_day_seconds: u32,
}

impl Default for VideoGenSettings {
    fn default() -> Self {
        VideoGenSettings { enabled: false, max_job_seconds: 8, max_day_seconds: 40 }
    }
}

/// A model chosen for one job in the harness: which connection (provider) and which model id.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct RoleBinding {
    pub provider: String,
    pub model: String,
}

/// What each part of the editor uses. `assistant` drives the agent chat, `script` writes scripts/narration
/// (falls back to the assistant), `video_workflow` is the default generative-video workflow (ComfyUI).
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Roles {
    pub assistant: Option<RoleBinding>,
    pub script: Option<RoleBinding>,
    /// Designs Blender animations (falls back to the assistant).
    pub blender: Option<RoleBinding>,
    /// Designs motion graphics: titles, lower thirds, keyframes (falls back to the assistant).
    pub motion: Option<RoleBinding>,
    pub video_workflow: Option<String>,
    /// Hosted video backend (provider = fal|gemini|runway|luma). Mutually exclusive with `video_workflow` in the UI.
    pub video_backend: Option<RoleBinding>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub version: u32,
    pub active_model: Option<String>,
    pub models: BTreeMap<String, ModelProfile>,
    pub budget: Budget,
    pub security: Security,
    pub render: RenderDefaults,
    pub ui: UiSettings,
    pub edition: EditionSettings,
    pub actions: ActionSettings,
    pub roles: Roles,
    pub videogen: VideoGenSettings,
    /// Resolved helper paths written by `cinvar setup --pro-ai`, keyed by the environment
    /// variable the adapter reads (for example `CINVAR_WHISPERX_PYTHON`). Persisting them here
    /// means a customer never exports five `CINVAR_*_PYTHON` variables by hand.
    #[serde(default)]
    pub helpers: BTreeMap<String, String>,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            version: CONFIG_VERSION,
            active_model: None,
            models: BTreeMap::new(),
            budget: Budget::default(),
            security: Security::default(),
            render: RenderDefaults::default(),
            ui: UiSettings::default(),
            edition: EditionSettings::default(),
            actions: ActionSettings::default(),
            roles: Roles::default(),
            videogen: VideoGenSettings::default(),
            helpers: BTreeMap::new(),
        }
    }
}

impl Config {
    /// A helper path recorded by `cinvar setup --pro-ai`.
    pub fn helper(&self, key: &str) -> Option<&str> {
        self.helpers.get(key).map(String::as_str)
    }

    /// Record (or replace) a helper path.
    pub fn set_helper(&mut self, key: &str, value: &str) {
        self.helpers.insert(key.to_string(), value.to_string());
    }

    /// Every helper as `(name, value)` pairs, for building a child-process environment.
    pub fn helper_env(&self) -> Vec<(String, String)> {
        self.helpers
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect()
    }

    pub fn path() -> PathBuf {
        paths::config_dir().join("config.toml")
    }

    pub fn load() -> Result<Config> {
        Self::load_from(&Self::path())
    }

    pub fn load_from(path: &Path) -> Result<Config> {
        if !path.exists() {
            return Ok(Config::default());
        }
        let text = std::fs::read_to_string(path)?;
        let mut cfg: Config = toml::from_str(&text)?;
        if cfg.version == 0 {
            cfg.version = CONFIG_VERSION;
        }
        Ok(cfg)
    }

    pub fn save(&self) -> Result<()> {
        self.save_to(&Self::path())
    }

    pub fn save_to(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            paths::ensure_dir(parent)?;
        }
        let text = toml::to_string_pretty(self)?;
        paths::write_atomic(path, format!("# CINVAR configuration\n{text}").as_bytes())
    }

    pub fn active_profile(&self) -> Result<&ModelProfile> {
        let name = self.active_model.as_ref().ok_or_else(|| {
            Error::invalid("no active model configured").with_hint(
                "run: cinvar models add local --provider ollama --model qwen2.5:7b, then cinvar models use local",
            )
        })?;
        self.profile(name)
    }

    /// Resolve a role to (connection name, profile with the role's model applied).
    /// Connections are the per-provider profiles; roles only pick a model on one of them.
    pub fn role_profile(&self, role: &str) -> Result<(String, ModelProfile)> {
        let own = match role {
            "script" => self.roles.script.clone(),
            "blender" => self.roles.blender.clone(),
            "motion" => self.roles.motion.clone(),
            _ => None,
        };
        let binding = own.or_else(|| self.roles.assistant.clone());
        if let Some(b) = binding.filter(|b| !b.provider.is_empty()) {
            let mut profile = self.models.get(&b.provider).cloned().unwrap_or_default();
            profile.provider = b.provider.clone();
            if !b.model.is_empty() {
                profile.model = b.model.clone();
            }
            return Ok((b.provider, profile));
        }
        let name = self.active_model.clone().ok_or_else(|| {
            Error::invalid("no model configured").with_hint("open Settings and pick a model for the assistant")
        })?;
        Ok((name.clone(), self.profile(&name)?.clone()))
    }

    pub fn profile(&self, name: &str) -> Result<&ModelProfile> {
        self.models.get(name).ok_or_else(|| {
            Error::not_found(format!("model profile '{name}' is not configured"))
                .with_hint("run cinvar models list to see available profiles")
        })
    }

    pub fn upsert_profile(&mut self, name: &str, profile: ModelProfile) {
        self.models.insert(name.to_string(), profile);
        if self.active_model.is_none() {
            self.active_model = Some(name.to_string());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn old_config_without_roles_loads_and_roles_resolve() {
        let c: Config = toml::from_str("version = 1\nactive_model = \"x\"\n[models.x]\nprovider = \"mock\"\nmodel = \"m\"\n").unwrap();
        assert_eq!(c.roles, Roles::default());
        assert_eq!(c.role_profile("assistant").unwrap().0, "x");
        let mut c = c;
        c.roles.script = Some(RoleBinding { provider: "deepseek".into(), model: "deepseek-chat".into() });
        let (n, p) = c.role_profile("script").unwrap();
        assert_eq!((n.as_str(), p.model.as_str(), p.provider.as_str()), ("deepseek", "deepseek-chat", "deepseek"));
        assert_eq!(c.role_profile("assistant").unwrap().0, "x");
    }

    #[test]
    fn roundtrips_toml() {
        let mut cfg = Config::default();
        cfg.upsert_profile(
            "local",
            ModelProfile {
                provider: "ollama".into(),
                model: "qwen2.5:7b".into(),
                ..Default::default()
            },
        );
        let text = toml::to_string_pretty(&cfg).unwrap();
        let back: Config = toml::from_str(&text).unwrap();
        assert_eq!(back.active_model.as_deref(), Some("local"));
        assert_eq!(back.models["local"].model, "qwen2.5:7b");
        assert_eq!(back.version, CONFIG_VERSION);
    }
}
