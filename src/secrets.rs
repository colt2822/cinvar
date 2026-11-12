use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::error::Result;
use crate::util::paths;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CredentialStore {
    pub keys: BTreeMap<String, String>,
}

impl CredentialStore {
    pub fn path() -> PathBuf {
        paths::config_dir().join("credentials.toml")
    }

    pub fn load() -> Result<CredentialStore> {
        let path = Self::path();
        if !path.exists() {
            return Ok(CredentialStore::default());
        }
        let text = std::fs::read_to_string(&path)?;
        Ok(toml::from_str(&text).unwrap_or_default())
    }

    pub fn save(&self) -> Result<()> {
        let path = Self::path();
        if let Some(parent) = path.parent() {
            paths::ensure_dir(parent)?;
        }
        let text = toml::to_string_pretty(self)?;
        let body = format!("# CINVAR credentials — keep this file private\n{text}");
        // Create the temp file private from the first byte (no window with the default umask),
        // then rename over the target.
        let tmp = path.with_extension("toml.tmp");
        {
            use std::io::Write;
            let mut opts = std::fs::OpenOptions::new();
            opts.write(true).create(true).truncate(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                opts.mode(0o600);
            }
            let mut file = opts.open(&tmp)?;
            file.write_all(body.as_bytes())?;
            file.sync_all()?;
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600));
        }
        std::fs::rename(&tmp, &path)?;
        Ok(())
    }

    pub fn set_key(&mut self, profile: &str, key: &str) -> Result<()> {
        self.keys.insert(profile.to_string(), key.to_string());
        self.save()
    }

    pub fn get_key(&self, profile: &str) -> Option<String> {
        self.keys.get(profile).cloned()
    }
}

pub fn resolve(profile_name: &str, profile: &crate::config::ModelProfile) -> Option<String> {
    if let Some(env_name) = &profile.api_key_env {
        if let Ok(v) = std::env::var(env_name) {
            if !v.trim().is_empty() {
                return Some(v.trim().to_string());
            }
        }
    }
    if let Ok(v) = std::env::var("CINVAR_API_KEY") {
        if !v.trim().is_empty() {
            return Some(v.trim().to_string());
        }
    }
    let provider_env = match profile.provider.as_str() {
        "openai" | "openai-compatible" => Some("OPENAI_API_KEY"),
        "anthropic" => Some("ANTHROPIC_API_KEY"),
        "gemini" | "google" => Some("GEMINI_API_KEY"),
        "deepseek" => Some("DEEPSEEK_API_KEY"),
        "xai" | "grok" => Some("XAI_API_KEY"),
        "openrouter" => Some("OPENROUTER_API_KEY"),
        _ => None,
    };
    if let Some(name) = provider_env {
        if let Ok(v) = std::env::var(name) {
            if !v.trim().is_empty() {
                return Some(v.trim().to_string());
            }
        }
    }
    CredentialStore::load()
        .ok()
        .and_then(|store| store.get_key(profile_name))
}

pub fn redact(text: &str) -> String {
    let mut out = text.to_string();
    if let Ok(store) = CredentialStore::load() {
        for value in store.keys.values() {
            if value.len() >= 8 {
                out = out.replace(value.as_str(), "[redacted]");
            }
        }
    }
    for name in [
        "CINVAR_API_KEY",
        "OPENAI_API_KEY",
        "ANTHROPIC_API_KEY",
        "GEMINI_API_KEY",
        "DEEPSEEK_API_KEY",
        "XAI_API_KEY",
        "OPENROUTER_API_KEY",
        "CINVAR_RUNPOD_API_KEY",
    ] {
        if let Ok(value) = std::env::var(name) {
            if value.len() >= 8 {
                out = out.replace(&value, "[redacted]");
            }
        }
    }
    match regex::Regex::new(r"sk-[A-Za-z0-9_\-]{8,}") {
        Ok(re) => re.replace_all(&out, "[redacted]").to_string(),
        Err(_) => out,
    }
}

/// RunPod key: `CINVAR_RUNPOD_API_KEY`, else the private credentials file under profile "runpod".
/// It is never stored in a project, a manifest, an export or a job record.
pub fn runpod_key() -> Option<String> {
    if let Ok(v) = std::env::var("CINVAR_RUNPOD_API_KEY") {
        if !v.trim().is_empty() {
            return Some(v.trim().to_string());
        }
    }
    CredentialStore::load()
        .ok()
        .and_then(|s| s.get_key("runpod"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redacts_sk_keys() {
        let s = "auth header sk-abcdefghijklmnopqrstuvwxyz";
        assert!(!redact(s).contains("sk-abc"));
        assert!(redact(s).contains("[redacted]"));
    }
}
