use serde::{Deserialize, Serialize};

use crate::config::{Config, ModelProfile};
use crate::error::{Error, ErrorCode, Result};

mod anthropic;
mod common;
mod gemini;
mod mock;
mod ollama;
mod openai;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Capabilities {
    pub tool_calling: bool,
    pub structured_output: bool,
    pub images: bool,
    pub video: bool,
    pub audio: bool,
    pub streaming: bool,
    pub context_window: u32,
}

impl Default for Capabilities {
    fn default() -> Self {
        Capabilities {
            tool_calling: true,
            structured_output: false,
            images: false,
            video: false,
            audio: false,
            streaming: false,
            context_window: 32_000,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    System,
    User,
    Assistant,
    Tool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Part {
    Text { text: String },
    Image { mime: String, data_base64: String },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub arguments: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    pub role: Role,
    #[serde(default)]
    pub parts: Vec<Part>,
    #[serde(default)]
    pub tool_calls: Vec<ToolCall>,
    #[serde(default)]
    pub tool_call_id: Option<String>,
}

impl Message {
    pub fn system(text: impl Into<String>) -> Message {
        Message {
            role: Role::System,
            parts: vec![Part::Text { text: text.into() }],
            tool_calls: Vec::new(),
            tool_call_id: None,
        }
    }
    pub fn user(text: impl Into<String>) -> Message {
        Message {
            role: Role::User,
            parts: vec![Part::Text { text: text.into() }],
            tool_calls: Vec::new(),
            tool_call_id: None,
        }
    }
    pub fn assistant(text: impl Into<String>) -> Message {
        Message {
            role: Role::Assistant,
            parts: vec![Part::Text { text: text.into() }],
            tool_calls: Vec::new(),
            tool_call_id: None,
        }
    }
    pub fn tool_result(call_id: impl Into<String>, text: impl Into<String>) -> Message {
        Message {
            role: Role::Tool,
            parts: vec![Part::Text { text: text.into() }],
            tool_calls: Vec::new(),
            tool_call_id: Some(call_id.into()),
        }
    }
    pub fn text(&self) -> String {
        let mut out = String::new();
        for part in &self.parts {
            match part {
                Part::Text { text } => {
                    if !out.is_empty() {
                        out.push('\n');
                    }
                    out.push_str(text);
                }
                Part::Image { .. } => out.push_str("[image]"),
            }
        }
        out
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolSchema {
    pub name: String,
    pub description: String,
    pub input_schema: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Usage {
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
}

impl Usage {
    pub fn total(&self) -> u64 {
        self.prompt_tokens + self.completion_tokens
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatRequest {
    pub model: String,
    pub system: String,
    pub messages: Vec<Message>,
    pub tools: Vec<ToolSchema>,
    pub temperature: Option<f64>,
    pub max_tokens: Option<u32>,
    pub json_mode: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatResponse {
    pub text: String,
    pub tool_calls: Vec<ToolCall>,
    pub usage: Usage,
    pub stop_reason: String,
    pub model: String,
    pub raw: Option<serde_json::Value>,
}

pub trait Provider: Send + Sync {
    fn id(&self) -> &str;
    fn capabilities(&self) -> Capabilities;
    fn chat(&self, request: &ChatRequest) -> Result<ChatResponse>;
    fn health(&self) -> Result<()>;
}

/// Every provider name build() accepts, for the hint of an unknown one.
const SUPPORTED_PROVIDERS: &str = "openai, openai-compatible, anthropic, gemini, google, deepseek, xai, grok, openrouter, ollama, llamacpp, local, mock";

/// Build the provider adapter for one model profile.
///
/// The provider name is matched case-insensitively. Unknown names are still
/// usable when the profile carries a base_url, because they are then assumed to
/// speak the OpenAI-compatible chat completions protocol; without a base_url
/// there is nothing to talk to, so the name is rejected with the supported list.
pub fn build(name: &str, profile: &ModelProfile, config: &Config) -> Result<Box<dyn Provider>> {
    let provider = profile.provider.trim().to_ascii_lowercase();
    if provider.is_empty() {
        return Err(
            Error::invalid(format!("model profile '{name}' has no provider"))
                .with_hint(format!("supported providers: {SUPPORTED_PROVIDERS}")),
        );
    }
    if provider == "mock" {
        return Ok(Box::new(mock::MockProvider::new(profile)));
    }
    if !config.security.allow_network {
        return Err(Error::new(
            ErrorCode::PermissionDenied,
            format!(
                "provider '{provider}' needs network access, which security.allow_network disables"
            ),
        )
        .with_hint("set security.allow_network = true, or use the offline mock provider"));
    }

    let default_base = match provider.as_str() {
        "openai" | "openai-compatible" => "https://api.openai.com/v1",
        "deepseek" => "https://api.deepseek.com/v1",
        "xai" | "grok" => "https://api.x.ai/v1",
        "openrouter" => "https://openrouter.ai/api/v1",
        "llamacpp" | "local" => "http://localhost:8080/v1",
        "ollama" => "http://localhost:11434/v1",
        _ => "",
    };
    let base_url = profile
        .base_url
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or(default_base);

    match provider.as_str() {
        "anthropic" => {
            let key = require_credential(name, profile, "ANTHROPIC_API_KEY")?;
            Ok(Box::new(anthropic::AnthropicProvider::new(profile, key)))
        }
        "gemini" | "google" => {
            let key = require_credential(name, profile, "GEMINI_API_KEY")?;
            Ok(Box::new(gemini::GeminiProvider::new(profile, key)))
        }
        "ollama" => {
            let key = optional_credential(name, profile, "CINVAR_API_KEY", base_url)?;
            if ollama::is_native(base_url) {
                Ok(Box::new(ollama::OllamaProvider::new(
                    profile, base_url, key,
                )))
            } else {
                Ok(Box::new(openai::OpenAiProvider::new(
                    "ollama", base_url, key, profile,
                )))
            }
        }
        "openai" => {
            let key = optional_credential(name, profile, "OPENAI_API_KEY", base_url)?;
            Ok(Box::new(openai::OpenAiProvider::new(
                "openai", base_url, key, profile,
            )))
        }
        "openai-compatible" => {
            let key = optional_credential(name, profile, "OPENAI_API_KEY", base_url)?;
            Ok(Box::new(openai::OpenAiProvider::new(
                "openai-compatible",
                base_url,
                key,
                profile,
            )))
        }
        "deepseek" => {
            let key = optional_credential(name, profile, "DEEPSEEK_API_KEY", base_url)?;
            Ok(Box::new(openai::OpenAiProvider::new(
                "deepseek", base_url, key, profile,
            )))
        }
        "xai" | "grok" => {
            let key = optional_credential(name, profile, "XAI_API_KEY", base_url)?;
            Ok(Box::new(openai::OpenAiProvider::new(
                &provider, base_url, key, profile,
            )))
        }
        "openrouter" => {
            let key = optional_credential(name, profile, "OPENROUTER_API_KEY", base_url)?;
            Ok(Box::new(openai::OpenAiProvider::new(
                "openrouter",
                base_url,
                key,
                profile,
            )))
        }
        "llamacpp" | "local" => {
            let key = optional_credential(name, profile, "CINVAR_API_KEY", base_url)?;
            Ok(Box::new(openai::OpenAiProvider::new(
                &provider, base_url, key, profile,
            )))
        }
        other => {
            if base_url.is_empty() {
                return Err(Error::invalid(format!("unknown provider '{other}'")).with_hint(format!(
                    "supported providers: {SUPPORTED_PROVIDERS}; any other server needs base_url in the profile"
                )));
            }
            let key = optional_credential(name, profile, "CINVAR_API_KEY", base_url)?;
            Ok(Box::new(openai::OpenAiProvider::new(
                other, base_url, key, profile,
            )))
        }
    }
}

/// A key that must be present: cloud APIs cannot be reached anonymously.
fn require_credential(name: &str, profile: &ModelProfile, default_env: &str) -> Result<String> {
    match crate::secrets::resolve(name, profile) {
        Some(key) => Ok(key),
        None => Err(missing_key(name, profile, default_env)),
    }
}

/// A key that may be absent: local servers such as Ollama, llama.cpp and vLLM
/// usually authenticate nothing, so an unset key is only an error when the URL
/// points at a remote host.
fn optional_credential(
    name: &str,
    profile: &ModelProfile,
    default_env: &str,
    base_url: &str,
) -> Result<Option<String>> {
    if let Some(key) = crate::secrets::resolve(name, profile) {
        return Ok(Some(key));
    }
    if is_local_url(base_url) {
        return Ok(None);
    }
    Err(missing_key(name, profile, default_env))
}

fn missing_key(name: &str, profile: &ModelProfile, default_env: &str) -> Error {
    let env_name = profile
        .api_key_env
        .clone()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| default_env.to_string());
    Error::new(
        ErrorCode::Provider,
        format!(
            "no API key found for provider '{}'",
            profile.provider.trim()
        ),
    )
    .with_hint(format!(
        "set the {env_name} environment variable, or store one with: cinvar models key {name}"
    ))
}

fn is_local_url(url: &str) -> bool {
    let lower = url.to_ascii_lowercase();
    lower.starts_with("http://localhost")
        || lower.starts_with("http://127.0.0.1")
        || lower.starts_with("http://[::1]")
        || lower.starts_with("http://0.0.0.0")
}

/// Request builders, response parsers and the retry classifier, exposed so the
/// integration tests can assert the exact wire format without a network.
#[doc(hidden)]
pub mod internals {
    pub use super::anthropic::{
        build_body as anthropic_request_body, parse_response as anthropic_parse_response,
    };
    pub use super::common::{
        backoff_schedule, is_retryable_status, redacted_error_message, shorten,
    };
    pub use super::gemini::{
        build_body as gemini_request_body, parse_response as gemini_parse_response,
    };
    pub use super::ollama::build_body as ollama_request_body;
    pub use super::openai::parse_chat_response as openai_parse_response;

    /// The OpenAI-compatible chat body using the adapter's own defaults.
    pub fn openai_request_body(request: &super::ChatRequest) -> serde_json::Value {
        super::openai::chat_body(request, None, None)
    }

    /// The compact JSON text the OpenAI adapter sends for a request.
    pub fn openai_request_json(request: &super::ChatRequest) -> String {
        serde_json::to_string(&super::openai::chat_body(request, None, None)).unwrap_or_default()
    }
}
