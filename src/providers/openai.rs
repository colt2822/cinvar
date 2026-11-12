//! OpenAI-compatible chat adapter.
//!
//! Used for openai, deepseek, xai/grok, openrouter, llamacpp/local and any other
//! server that speaks POST {base}/chat/completions with bearer auth.
//!
//! Wire shape sent for a tool round (this exact body is asserted in
//! tests/w2_providers.rs):
//!
//! {"messages":[
//!    {"content":"system prompt","role":"system"},
//!    {"content":"make a vertical cut","role":"user"},
//!    {"content":"","role":"assistant","tool_calls":[
//!        {"function":{"arguments":"{\"path\":\"/media/clips\"}","name":"cinvar.media.import_folder"},"id":"call_1","type":"function"}]},
//!    {"content":"{\"imported\":[]}","role":"tool","tool_call_id":"call_1"}],
//!  "model":"gpt-4o-mini",
//!  "tool_choice":"auto",
//!  "tools":[{"function":{"description":"Import a folder","name":"cinvar.media.import_folder",
//!            "parameters":{"properties":{},"required":[],"type":"object"}},"type":"function"}]}

use serde_json::{json, Value};

use crate::config::ModelProfile;
use crate::error::{Error, ErrorCode, Result};
use crate::providers::common::{self, Http};
use crate::providers::{
    Capabilities, ChatRequest, ChatResponse, Message, Part, Provider, Role, ToolCall, Usage,
};
use crate::util::ids;

pub(crate) struct OpenAiProvider {
    id: String,
    base_url: String,
    api_key: Option<String>,
    context_window: u32,
    supports_tools: Option<bool>,
    supports_images: Option<bool>,
    temperature: Option<f64>,
    max_output_tokens: Option<u32>,
    http: Http,
}

impl OpenAiProvider {
    pub(crate) fn new(
        id: &str,
        base_url: &str,
        api_key: Option<String>,
        profile: &ModelProfile,
    ) -> OpenAiProvider {
        OpenAiProvider {
            id: id.to_string(),
            base_url: base_url.trim_end_matches('/').to_string(),
            api_key,
            context_window: profile.context_window,
            supports_tools: profile.supports_tools,
            supports_images: profile.supports_images,
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

impl Provider for OpenAiProvider {
    fn id(&self) -> &str {
        &self.id
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities {
            tool_calling: self.supports_tools.unwrap_or(true),
            structured_output: true,
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
        let body = chat_body(request, self.temperature, self.max_output_tokens);
        let url = format!("{}/chat/completions", self.base_url);
        let raw = self
            .http
            .request("POST", &url, &self.headers(), Some(&body))?;
        let value = common::decode_ok(&raw, "chat completion", &url)?;
        parse_chat_response(&value, &request.model)
    }

    fn health(&self) -> Result<()> {
        let url = format!("{}/models", self.base_url);
        let raw = self.http.request("GET", &url, &self.headers(), None)?;
        common::health_status(&raw, "model list", &url)
    }
}

/// Build the request body for one chat round.
pub fn chat_body(
    request: &ChatRequest,
    temperature: Option<f64>,
    max_tokens: Option<u32>,
) -> Value {
    let mut messages: Vec<Value> = Vec::new();
    if !request.system.trim().is_empty() {
        messages.push(json!({"role": "system", "content": request.system}));
    }
    for message in &request.messages {
        messages.push(convert_message(message));
    }

    let mut body = json!({"model": request.model, "messages": messages});
    if let Some(temp) = request.temperature.or(temperature) {
        body["temperature"] = json!(temp);
    }
    if let Some(limit) = request.max_tokens.or(max_tokens) {
        body["max_tokens"] = json!(limit);
    }
    if !request.tools.is_empty() {
        body["tools"] = Value::Array(request.tools.iter().map(tool_json).collect());
        body["tool_choice"] = json!("auto");
    }
    if request.json_mode {
        body["response_format"] = json!({"type": "json_object"});
    }
    body
}

fn tool_json(schema: &crate::providers::ToolSchema) -> Value {
    json!({
        "type": "function",
        "function": {
            "name": schema.name,
            "description": schema.description,
            "parameters": schema.input_schema,
        }
    })
}

fn convert_message(message: &Message) -> Value {
    match message.role {
        Role::System => json!({"role": "system", "content": message.text()}),
        Role::User => {
            let has_image = message
                .parts
                .iter()
                .any(|p| matches!(p, Part::Image { .. }));
            if has_image {
                let mut blocks: Vec<Value> = Vec::new();
                for part in &message.parts {
                    match part {
                        Part::Text { text } => {
                            if !text.is_empty() {
                                blocks.push(json!({"type": "text", "text": text}));
                            }
                        }
                        Part::Image { mime, data_base64 } => blocks.push(json!({
                            "type": "image_url",
                            "image_url": {"url": format!("data:{mime};base64,{data_base64}")}
                        })),
                    }
                }
                json!({"role": "user", "content": blocks})
            } else {
                json!({"role": "user", "content": message.text()})
            }
        }
        Role::Assistant => {
            if message.tool_calls.is_empty() {
                json!({"role": "assistant", "content": message.text()})
            } else {
                let calls: Vec<Value> = message
                    .tool_calls
                    .iter()
                    .map(|call| {
                        json!({
                            "id": call.id,
                            "type": "function",
                            "function": {
                                "name": call.name,
                                "arguments": serde_json::to_string(&call.arguments).unwrap_or_else(|_| "{}".into()),
                            }
                        })
                    })
                    .collect();
                json!({"role": "assistant", "content": message.text(), "tool_calls": calls})
            }
        }
        Role::Tool => json!({
            "role": "tool",
            "tool_call_id": message.tool_call_id.clone().unwrap_or_default(),
            "content": message.text(),
        }),
    }
}

/// Parse a chat completion. Unparsable tool arguments never fail the turn; they
/// become a tool call carrying the raw text under "__malformed".
pub fn parse_chat_response(value: &Value, fallback_model: &str) -> Result<ChatResponse> {
    let choice = value
        .get("choices")
        .and_then(|c| c.as_array())
        .and_then(|items| items.first())
        .ok_or_else(|| Error::new(ErrorCode::Protocol, "provider response has no choices"))?;
    let message = choice.get("message").cloned().unwrap_or_else(|| json!({}));
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
            let id = item
                .get("id")
                .and_then(|v| v.as_str())
                .filter(|v| !v.is_empty())
                .map(|v| v.to_string())
                .unwrap_or_else(|| ids::new_id("call"));
            tool_calls.push(ToolCall {
                id,
                name,
                arguments: arguments_of(function.get("arguments")),
            });
        }
    }

    let usage = Usage {
        prompt_tokens: usage_field(value, "prompt_tokens"),
        completion_tokens: usage_field(value, "completion_tokens"),
    };
    let stop_reason = choice
        .get("finish_reason")
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

pub(crate) fn first_text(message: &Value) -> String {
    match message.get("content") {
        Some(Value::String(text)) => text.clone(),
        Some(Value::Array(items)) => items
            .iter()
            .find_map(|item| {
                if item.get("type").and_then(|t| t.as_str()) == Some("text") {
                    item.get("text")
                        .and_then(|t| t.as_str())
                        .map(|s| s.to_string())
                } else {
                    None
                }
            })
            .unwrap_or_default(),
        _ => String::new(),
    }
}

pub(crate) fn usage_field(value: &Value, key: &str) -> u64 {
    value
        .get("usage")
        .and_then(|usage| usage.get(key))
        .and_then(|v| v.as_u64())
        .unwrap_or(0)
}

/// Normalise tool arguments. A JSON string is parsed, a JSON object is kept, and
/// anything unparsable is wrapped rather than dropped.
pub(crate) fn arguments_of(raw: Option<&Value>) -> Value {
    match raw {
        Some(Value::String(text)) => {
            let trimmed = text.trim();
            if trimmed.is_empty() {
                json!({})
            } else {
                serde_json::from_str::<Value>(trimmed)
                    .unwrap_or_else(|_| json!({"__malformed": trimmed}))
            }
        }
        Some(Value::Null) | None => json!({}),
        Some(other) => other.clone(),
    }
}
