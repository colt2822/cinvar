//! Anthropic Messages API adapter.
//!
//! POST {base}/v1/messages with the x-api-key header and anthropic-version
//! 2023-06-01. The system prompt is a top-level string, tools are
//! {name, description, input_schema}, assistant tool calls become tool_use
//! blocks and our tool results become tool_result blocks inside a user message.

use serde_json::{json, Value};

use crate::config::ModelProfile;
use crate::error::{Error, ErrorCode, Result};
use crate::providers::common::{self, Http};
use crate::providers::openai::arguments_of;
use crate::providers::{
    Capabilities, ChatRequest, ChatResponse, Message, Part, Provider, Role, ToolCall, Usage,
};
use crate::util::ids;

const DEFAULT_BASE: &str = "https://api.anthropic.com";
const ANTHROPIC_VERSION: &str = "2023-06-01";
const DEFAULT_MAX_TOKENS: u32 = 8_192;

pub(crate) struct AnthropicProvider {
    id: String,
    base_url: String,
    api_key: String,
    model: String,
    context_window: u32,
    supports_tools: Option<bool>,
    supports_images: Option<bool>,
    temperature: Option<f64>,
    max_tokens: u32,
    http: Http,
}

impl AnthropicProvider {
    pub(crate) fn new(profile: &ModelProfile, api_key: String) -> AnthropicProvider {
        let base = profile
            .base_url
            .as_deref()
            .unwrap_or(DEFAULT_BASE)
            .trim_end_matches('/');
        let base = base
            .strip_suffix("/v1")
            .unwrap_or(base)
            .trim_end_matches('/')
            .to_string();
        AnthropicProvider {
            id: "anthropic".to_string(),
            base_url: base,
            api_key,
            model: profile.model.clone(),
            context_window: profile.context_window,
            supports_tools: profile.supports_tools,
            supports_images: profile.supports_images,
            temperature: if profile.temperature > 0.0 {
                Some(profile.temperature)
            } else {
                None
            },
            max_tokens: if profile.max_output_tokens > 0 {
                profile.max_output_tokens
            } else {
                DEFAULT_MAX_TOKENS
            },
            http: Http::new(),
        }
    }

    fn headers(&self) -> Vec<(String, String)> {
        vec![
            ("x-api-key".to_string(), self.api_key.clone()),
            (
                "anthropic-version".to_string(),
                ANTHROPIC_VERSION.to_string(),
            ),
        ]
    }
}

impl Provider for AnthropicProvider {
    fn id(&self) -> &str {
        &self.id
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities {
            tool_calling: self.supports_tools.unwrap_or(true),
            structured_output: false,
            images: self.supports_images.unwrap_or(true),
            video: false,
            audio: false,
            streaming: false,
            context_window: if self.context_window > 0 {
                self.context_window
            } else {
                32_000
            },
        }
    }

    fn chat(&self, request: &ChatRequest) -> Result<ChatResponse> {
        let body = build_body(request, self.max_tokens, self.temperature);
        let url = format!("{}/v1/messages", self.base_url);
        let raw = self
            .http
            .request("POST", &url, &self.headers(), Some(&body))?;
        let value = common::decode_ok(&raw, "messages", &url)?;
        parse_response(&value, &request.model)
    }

    fn health(&self) -> Result<()> {
        let url = format!("{}/v1/messages", self.base_url);
        let body = json!({
            "model": self.model,
            "max_tokens": 1,
            "messages": [{"role": "user", "content": [{"type": "text", "text": "ping"}]}],
        });
        let raw = self
            .http
            .request("POST", &url, &self.headers(), Some(&body))?;
        common::health_status(&raw, "messages endpoint", &url)
    }
}

/// Build the Messages request body.
pub fn build_body(request: &ChatRequest, max_tokens: u32, temperature: Option<f64>) -> Value {
    let mut system = request.system.trim().to_string();
    let mut messages: Vec<Value> = Vec::new();
    for message in &request.messages {
        match message.role {
            Role::System => {
                let text = message.text();
                if !text.trim().is_empty() {
                    if !system.is_empty() {
                        system.push_str("\n\n");
                    }
                    system.push_str(&text);
                }
            }
            Role::User => push_blocks(&mut messages, "user", user_blocks(message)),
            Role::Assistant => push_blocks(&mut messages, "assistant", assistant_blocks(message)),
            Role::Tool => push_blocks(&mut messages, "user", tool_result_blocks(message)),
        }
    }

    let mut body = json!({
        "model": request.model,
        "max_tokens": max_tokens,
        "messages": messages,
    });
    if !system.is_empty() {
        body["system"] = json!(system);
    }
    if let Some(temp) = request.temperature.or(temperature) {
        body["temperature"] = json!(temp);
    }
    if !request.tools.is_empty() {
        body["tools"] = Value::Array(
            request
                .tools
                .iter()
                .map(|schema| {
                    json!({
                        "name": schema.name,
                        "description": schema.description,
                        "input_schema": schema.input_schema,
                    })
                })
                .collect(),
        );
    }
    body
}

/// Anthropic wants alternating roles, so consecutive blocks of the same role are
/// merged instead of emitted as separate messages.
fn push_blocks(messages: &mut Vec<Value>, role: &str, blocks: Vec<Value>) {
    if blocks.is_empty() {
        return;
    }
    if let Some(last) = messages.last_mut() {
        if last.get("role").and_then(|r| r.as_str()) == Some(role) {
            if let Some(array) = last.get_mut("content").and_then(|c| c.as_array_mut()) {
                array.extend(blocks);
                return;
            }
        }
    }
    messages.push(json!({"role": role, "content": blocks}));
}

fn user_blocks(message: &Message) -> Vec<Value> {
    let mut blocks: Vec<Value> = Vec::new();
    for part in &message.parts {
        match part {
            Part::Text { text } => {
                if !text.is_empty() {
                    blocks.push(json!({"type": "text", "text": text}));
                }
            }
            Part::Image { mime, data_base64 } => blocks.push(json!({
                "type": "image",
                "source": {"type": "base64", "media_type": mime, "data": data_base64},
            })),
        }
    }
    blocks
}

fn assistant_blocks(message: &Message) -> Vec<Value> {
    let mut blocks: Vec<Value> = Vec::new();
    let text = message.text();
    if !text.is_empty() {
        blocks.push(json!({"type": "text", "text": text}));
    }
    for call in &message.tool_calls {
        blocks.push(json!({
            "type": "tool_use",
            "id": call.id,
            "name": call.name,
            "input": call.arguments,
        }));
    }
    blocks
}

fn tool_result_blocks(message: &Message) -> Vec<Value> {
    vec![json!({
        "type": "tool_result",
        "tool_use_id": message.tool_call_id.clone().unwrap_or_default(),
        "content": message.text(),
    })]
}

/// Parse a Messages response into our ChatResponse.
pub fn parse_response(value: &Value, fallback_model: &str) -> Result<ChatResponse> {
    let blocks = value
        .get("content")
        .and_then(|c| c.as_array())
        .cloned()
        .unwrap_or_default();
    let mut text = String::new();
    let mut tool_calls: Vec<ToolCall> = Vec::new();
    for block in &blocks {
        match block.get("type").and_then(|t| t.as_str()) {
            Some("text") => {
                if text.is_empty() {
                    text = block
                        .get("text")
                        .and_then(|v| v.as_str())
                        .unwrap_or_default()
                        .to_string();
                }
            }
            Some("tool_use") => {
                let name = block
                    .get("name")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default()
                    .to_string();
                if name.is_empty() {
                    continue;
                }
                let id = block
                    .get("id")
                    .and_then(|v| v.as_str())
                    .filter(|v| !v.is_empty())
                    .map(|v| v.to_string())
                    .unwrap_or_else(|| ids::new_id("call"));
                tool_calls.push(ToolCall {
                    id,
                    name,
                    arguments: arguments_of(block.get("input")),
                });
            }
            _ => {}
        }
    }
    if blocks.is_empty() {
        return Err(Error::new(
            ErrorCode::Protocol,
            "provider response has no content blocks",
        ));
    }
    let usage = Usage {
        prompt_tokens: common_usage(value, "input_tokens"),
        completion_tokens: common_usage(value, "output_tokens"),
    };
    let stop_reason = value
        .get("stop_reason")
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_string();
    let model = value
        .get("model")
        .and_then(|v| v.as_str())
        .filter(|v| !v.is_empty())
        .unwrap_or(fallback_model)
        .to_string();
    Ok(ChatResponse {
        text,
        tool_calls,
        usage,
        stop_reason,
        model,
        raw: Some(value.clone()),
    })
}

fn common_usage(value: &Value, key: &str) -> u64 {
    value
        .get("usage")
        .and_then(|usage| usage.get(key))
        .and_then(|v| v.as_u64())
        .unwrap_or(0)
}
