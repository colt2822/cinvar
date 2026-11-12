//! Google Gemini (generative language) adapter.
//!
//! POST {base}/v1beta/models/{model}:generateContent with an x-goog-api-key header (never in the URL) and user/model
//! contents, functionDeclarations for tools, and functionCall / functionResponse
//! parts mapped from our tool calls and tool results. The key travels in the
//! query string, so every message that mentions a URL goes through
//! common::public_url first.

use serde_json::{json, Value};

use crate::config::ModelProfile;
use crate::error::{Error, ErrorCode, Result};
use crate::providers::common::{self, Http};
use crate::providers::openai::arguments_of;
use crate::providers::{
    Capabilities, ChatRequest, ChatResponse, Message, Part, Provider, Role, ToolCall, Usage,
};
use crate::util::ids;

const DEFAULT_BASE: &str = "https://generativelanguage.googleapis.com";

pub(crate) struct GeminiProvider {
    id: String,
    base_url: String,
    api_key: String,
    model: String,
    context_window: u32,
    supports_tools: Option<bool>,
    supports_images: Option<bool>,
    temperature: Option<f64>,
    max_output_tokens: Option<u32>,
    http: Http,
}

impl GeminiProvider {
    pub(crate) fn new(profile: &ModelProfile, api_key: String) -> GeminiProvider {
        let base = profile
            .base_url
            .as_deref()
            .unwrap_or(DEFAULT_BASE)
            .trim_end_matches('/');
        let base = base
            .strip_suffix("/v1beta")
            .unwrap_or(base)
            .trim_end_matches('/')
            .to_string();
        GeminiProvider {
            id: "gemini".to_string(),
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
            max_output_tokens: if profile.max_output_tokens > 0 {
                Some(profile.max_output_tokens)
            } else {
                None
            },
            http: Http::new(),
        }
    }

    fn endpoint(&self, model: &str) -> String {
        format!("{}/v1beta/models/{model}:generateContent", self.base_url)
    }
}

impl Provider for GeminiProvider {
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
        let model = if request.model.trim().is_empty() {
            self.model.clone()
        } else {
            request.model.clone()
        };
        let body = build_body(request, self.temperature, self.max_output_tokens);
        let url = self.endpoint(&model);
        let raw = self.http.request(
            "POST",
            &url,
            &[("x-goog-api-key".to_string(), self.api_key.clone())],
            Some(&body),
        )?;
        let value = common::decode_ok(&raw, "generateContent", &url)?;
        parse_response(&value, &model)
    }

    fn health(&self) -> Result<()> {
        let url = self.endpoint(&self.model);
        let body = json!({
            "contents": [{"role": "user", "parts": [{"text": "ping"}]}],
            "generationConfig": {"maxOutputTokens": 1},
        });
        let raw = self.http.request(
            "POST",
            &url,
            &[("x-goog-api-key".to_string(), self.api_key.clone())],
            Some(&body),
        )?;
        common::health_status(&raw, "generateContent", &url)
    }
}

/// Build the generateContent request body.
pub fn build_body(
    request: &ChatRequest,
    temperature: Option<f64>,
    max_output_tokens: Option<u32>,
) -> Value {
    let mut system = request.system.trim().to_string();
    let mut contents: Vec<Value> = Vec::new();
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
            Role::User => push_parts(&mut contents, "user", user_parts(message)),
            Role::Assistant => push_parts(&mut contents, "model", assistant_parts(message)),
            Role::Tool => push_parts(&mut contents, "user", tool_response_parts(request, message)),
        }
    }

    let mut body = json!({"contents": contents});
    if !system.is_empty() {
        body["systemInstruction"] = json!({"parts": [{"text": system}]});
    }
    if !request.tools.is_empty() {
        let declarations: Vec<Value> = request
            .tools
            .iter()
            .map(|schema| {
                json!({
                    "name": schema.name,
                    "description": schema.description,
                    "parameters": schema.input_schema,
                })
            })
            .collect();
        body["tools"] = json!([{"functionDeclarations": declarations}]);
    }
    let mut generation = json!({});
    if let Some(temp) = request.temperature.or(temperature) {
        generation["temperature"] = json!(temp);
    }
    if let Some(limit) = request.max_tokens.or(max_output_tokens) {
        generation["maxOutputTokens"] = json!(limit);
    }
    if !generation.as_object().map(|o| o.is_empty()).unwrap_or(true) {
        body["generationConfig"] = generation;
    }
    body
}

/// Gemini requires alternating roles, so equal-role turns are merged.
fn push_parts(contents: &mut Vec<Value>, role: &str, parts: Vec<Value>) {
    if parts.is_empty() {
        return;
    }
    if let Some(last) = contents.last_mut() {
        if last.get("role").and_then(|r| r.as_str()) == Some(role) {
            if let Some(array) = last.get_mut("parts").and_then(|p| p.as_array_mut()) {
                array.extend(parts);
                return;
            }
        }
    }
    contents.push(json!({"role": role, "parts": parts}));
}

fn user_parts(message: &Message) -> Vec<Value> {
    let mut parts: Vec<Value> = Vec::new();
    for part in &message.parts {
        match part {
            Part::Text { text } => {
                if !text.is_empty() {
                    parts.push(json!({"text": text}));
                }
            }
            Part::Image { mime, data_base64 } => {
                parts.push(json!({"inlineData": {"mimeType": mime, "data": data_base64}}))
            }
        }
    }
    parts
}

fn assistant_parts(message: &Message) -> Vec<Value> {
    let mut parts: Vec<Value> = Vec::new();
    let text = message.text();
    if !text.is_empty() {
        parts.push(json!({"text": text}));
    }
    for call in &message.tool_calls {
        parts.push(json!({"functionCall": {"name": call.name, "args": call.arguments}}));
    }
    parts
}

fn tool_response_parts(request: &ChatRequest, message: &Message) -> Vec<Value> {
    let text = message.text();
    let response = match serde_json::from_str::<Value>(text.trim()) {
        Ok(Value::Object(map)) => Value::Object(map),
        _ => json!({"result": text}),
    };
    let name = message
        .tool_call_id
        .as_deref()
        .map(|id| tool_name_for(&request.messages, id))
        .unwrap_or_else(|| "tool_result".to_string());
    vec![json!({"functionResponse": {"name": name, "response": response}})]
}

/// Gemini functionResponse blocks carry the function name, not the call id, so
/// the name is recovered from the assistant turn that requested the call.
fn tool_name_for(messages: &[Message], call_id: &str) -> String {
    for message in messages {
        for call in &message.tool_calls {
            if call.id == call_id {
                return call.name.clone();
            }
        }
    }
    "tool_result".to_string()
}

/// Parse a generateContent response into our ChatResponse.
pub fn parse_response(value: &Value, fallback_model: &str) -> Result<ChatResponse> {
    let candidate = value
        .get("candidates")
        .and_then(|c| c.as_array())
        .and_then(|items| items.first())
        .ok_or_else(|| Error::new(ErrorCode::Protocol, "provider response has no candidates"))?;
    let parts = candidate
        .get("content")
        .and_then(|c| c.get("parts"))
        .and_then(|p| p.as_array())
        .cloned()
        .unwrap_or_default();

    let mut text = String::new();
    let mut tool_calls: Vec<ToolCall> = Vec::new();
    for part in &parts {
        if let Some(call) = part.get("functionCall") {
            let name = call
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
                arguments: arguments_of(call.get("args")),
            });
            continue;
        }
        if text.is_empty() {
            if let Some(chunk) = part.get("text").and_then(|v| v.as_str()) {
                text = chunk.to_string();
            }
        }
    }

    let usage = Usage {
        prompt_tokens: metadata_count(value, "promptTokenCount"),
        completion_tokens: metadata_count(value, "candidatesTokenCount"),
    };
    let stop_reason = candidate
        .get("finishReason")
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_string();
    let model = value
        .get("modelVersion")
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

fn metadata_count(value: &Value, key: &str) -> u64 {
    value
        .get("usageMetadata")
        .and_then(|usage| usage.get(key))
        .and_then(|v| v.as_u64())
        .unwrap_or(0)
}
