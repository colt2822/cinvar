//! Ollama adapter.
//!
//! Which transport is used depends on how the profile's base_url is written:
//!
//! * base_url ending in ":11434" (the native Ollama daemon port, e.g.
//!   "http://localhost:11434") selects Ollama's native API at {base}/api/chat,
//!   with role/content messages, an images array per message, tools in the
//!   OpenAI function shape, and a message.tool_calls reply.
//! * anything else - including the default "http://localhost:11434/v1" - is
//!   served by the OpenAI-compatible adapter ({base}/chat/completions), which is
//!   what Ollama exposes under /v1 and what proxies and gateways expect.
//!
//! Keeping the native port as the discriminator means a profile that names the
//! daemon root gets the daemon API, while the documented default keeps working
//! through the compatible surface.

use serde_json::{json, Value};

use crate::config::ModelProfile;
use crate::error::{Error, ErrorCode, Result};
use crate::providers::common::{self, Http};
use crate::providers::openai::{arguments_of, first_text};
use crate::providers::{
    Capabilities, ChatRequest, ChatResponse, Message, Part, Provider, Role, ToolCall, Usage,
};
use crate::util::ids;

/// The native Ollama daemon port.
pub const NATIVE_PORT: &str = ":11434";

/// True when the base URL names the native daemon rather than its /v1 surface.
pub fn is_native(base_url: &str) -> bool {
    base_url.trim_end_matches('/').ends_with(NATIVE_PORT)
}

pub(crate) struct OllamaProvider {
    id: String,
    base_url: String,
    api_key: Option<String>,
    context_window: u32,
    supports_tools: Option<bool>,
    temperature: Option<f64>,
    max_output_tokens: Option<u32>,
    http: Http,
}

impl OllamaProvider {
    pub(crate) fn new(
        profile: &ModelProfile,
        base_url: &str,
        api_key: Option<String>,
    ) -> OllamaProvider {
        OllamaProvider {
            id: "ollama".to_string(),
            base_url: base_url.trim_end_matches('/').to_string(),
            api_key,
            context_window: profile.context_window,
            supports_tools: profile.supports_tools,
            temperature: if profile.temperature > 0.0 {
                Some(profile.temperature)
            } else {
                None
            },
            max_output_tokens: if profile.max_output_tokens > 0 {
                Some(profile.max_output_tokens)
            } else {
                None
            },
            http: Http::new(),
        }
    }

    fn headers(&self) -> Vec<(String, String)> {
        match &self.api_key {
            Some(key) => vec![("authorization".to_string(), format!("Bearer {key}"))],
            None => Vec::new(),
        }
    }
}

impl Provider for OllamaProvider {
    fn id(&self) -> &str {
        &self.id
    }

    /// Ollama's API accepts images and tools, but whether a given pulled model
    /// honours them is a property of the model, not the server, so images are
    /// only claimed when the profile says so.
    fn capabilities(&self) -> Capabilities {
        Capabilities {
            tool_calling: self.supports_tools.unwrap_or(true),
            structured_output: false,
            images: false,
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
        let body = build_body(request, self.temperature, self.max_output_tokens);
        let url = format!("{}/api/chat", self.base_url);
        let raw = self
            .http
            .request("POST", &url, &self.headers(), Some(&body))?;
        let value = common::decode_ok(&raw, "chat", &url)?;
        parse_response(&value, &request.model)
    }

    fn health(&self) -> Result<()> {
        let url = format!("{}/api/tags", self.base_url);
        let raw = self.http.request("GET", &url, &self.headers(), None)?;
        common::health_status(&raw, "ollama daemon", &url)
    }
}

/// Build the native /api/chat request body.
pub fn build_body(
    request: &ChatRequest,
    temperature: Option<f64>,
    max_output_tokens: Option<u32>,
) -> Value {
    let mut messages: Vec<Value> = Vec::new();
    if !request.system.trim().is_empty() {
        messages.push(json!({"role": "system", "content": request.system}));
    }
    for message in &request.messages {
        messages.push(convert_message(message));
    }

    let mut body = json!({"model": request.model, "messages": messages, "stream": false});
    if !request.tools.is_empty() {
        body["tools"] = Value::Array(
            request
                .tools
                .iter()
                .map(|schema| {
                    json!({
                        "type": "function",
                        "function": {
                            "name": schema.name,
                            "description": schema.description,
                            "parameters": schema.input_schema,
                        }
                    })
                })
                .collect(),
        );
    }
    let mut options = json!({});
    if let Some(temp) = request.temperature.or(temperature) {
        options["temperature"] = json!(temp);
    }
    if let Some(limit) = request.max_tokens.or(max_output_tokens) {
        options["num_predict"] = json!(limit);
    }
    if !options.as_object().map(|o| o.is_empty()).unwrap_or(true) {
        body["options"] = options;
    }
    body
}

fn convert_message(message: &Message) -> Value {
    match message.role {
        Role::System => json!({"role": "system", "content": message.text()}),
        Role::User => {
            let images: Vec<Value> = message
                .parts
                .iter()
                .filter_map(|part| match part {
                    Part::Image { data_base64, .. } => Some(json!(data_base64)),
                    Part::Text { .. } => None,
                })
                .collect();
            if images.is_empty() {
                json!({"role": "user", "content": message.text()})
            } else {
                json!({"role": "user", "content": message.text(), "images": images})
            }
        }
        Role::Assistant => {
            if message.tool_calls.is_empty() {
                json!({"role": "assistant", "content": message.text()})
            } else {
                let calls: Vec<Value> = message
                    .tool_calls
                    .iter()
                    .map(|call| json!({"function": {"name": call.name, "arguments": call.arguments}}))
                    .collect();
                json!({"role": "assistant", "content": message.text(), "tool_calls": calls})
            }
        }
        Role::Tool => json!({
            "role": "tool",
            "content": message.text(),
            "tool_call_id": message.tool_call_id.clone().unwrap_or_default(),
        }),
    }
}

/// Parse a native /api/chat response into our ChatResponse.
pub fn parse_response(value: &Value, fallback_model: &str) -> Result<ChatResponse> {
    let message = value
        .get("message")
        .cloned()
        .ok_or_else(|| Error::new(ErrorCode::Protocol, "provider response has no message"))?;
    let text = first_text(&message);
    let mut tool_calls: Vec<ToolCall> = Vec::new();
    if let Some(items) = message.get("tool_calls").and_then(|v| v.as_array()) {
        for item in items {
            let function = item.get("function").cloned().unwrap_or_else(|| json!({}));
            let name = function
                .get("name")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string();
            if name.is_empty() {
                continue;
            }
            tool_calls.push(ToolCall {
                id: ids::new_id("call"),
                name,
                arguments: arguments_of(function.get("arguments")),
            });
        }
    }
    let usage = Usage {
        prompt_tokens: value
            .get("prompt_eval_count")
            .and_then(|v| v.as_u64())
            .unwrap_or(0),
        completion_tokens: value
            .get("eval_count")
            .and_then(|v| v.as_u64())
            .unwrap_or(0),
    };
    let stop_reason = value
        .get("done_reason")
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
