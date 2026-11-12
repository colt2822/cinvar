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
}

impl Default for UiSettings {
    fn default() -> Self {
        UiSettings {
            port: 8420,
            open_browser: false,
        }
    }
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
        }
    }
}

impl Config {
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
