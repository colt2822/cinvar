//! Offline tests for the provider adapters (worker W2).
//!
//! Nothing here touches the network: every test either inspects a request body
//! built from our own types, parses a synthetic provider response, or drives the
//! deterministic mock planner over synthetic tool results.

use serde_json::{json, Value};

use cinvar::config::{Config, ModelProfile};
use cinvar::error::ErrorCode;
use cinvar::providers::{
    build, internals, ChatRequest, Message, Provider, Role, ToolCall, ToolSchema,
};

fn tool(name: &str) -> ToolSchema {
    ToolSchema {
        name: name.to_string(),
        description: format!("Run {name}"),
        input_schema: json!({"type": "object", "properties": {}, "required": []}),
    }
}

fn request(model: &str, messages: Vec<Message>, tools: Vec<ToolSchema>) -> ChatRequest {
    ChatRequest {
        model: model.to_string(),
        system: "system prompt".to_string(),
        messages,
        tools,
        temperature: None,
        max_tokens: None,
        json_mode: false,
    }
}

fn assistant_call(call: &ToolCall) -> Message {
    Message {
        role: Role::Assistant,
        parts: Vec::new(),
        tool_calls: vec![call.clone()],
        tool_call_id: None,
    }
}

fn tool_output(call: &ToolCall, value: Value) -> Message {
    Message::tool_result(call.id.clone(), value.to_string())
}

/// One tool round: user request, assistant tool call, tool result.
fn tool_round_messages() -> Vec<Message> {
    let call = ToolCall {
        id: "call_1".to_string(),
        name: "cinvar.media.import_folder".to_string(),
        arguments: json!({"path": "/media/clips"}),
    };
    vec![
        Message::user("make a vertical cut"),
        assistant_call(&call),
        tool_output(&call, json!({"imported": []})),
    ]
}

/// The exact JSON body the OpenAI adapter sends for one tool call.
///
/// {"messages":[{"content":"system prompt","role":"system"},
///  {"content":"make a vertical cut","role":"user"},
///  {"content":"","role":"assistant","tool_calls":[{"function":{"arguments":"{\"path\":\"/media/clips\"}","name":"cinvar.media.import_folder"},"id":"call_1","type":"function"}]},
///  {"content":"{\"imported\":[]}","role":"tool","tool_call_id":"call_1"}],
///  "model":"gpt-4o-mini","tool_choice":"auto",
///  "tools":[{"function":{"description":"Run cinvar.media.import_folder","name":"cinvar.media.import_folder","parameters":{"properties":{},"required":[],"type":"object"}},"type":"function"}]}
#[test]
fn openai_request_body_for_one_tool_call() {
    let wire = request(
        "gpt-4o-mini",
        tool_round_messages(),
        vec![tool("cinvar.media.import_folder")],
    );
    let body = internals::openai_request_body(&wire);
    println!(
        "openai wire body: {}",
        internals::openai_request_json(&wire)
    );

    let expected = json!({
        "model": "gpt-4o-mini",
        "messages": [
            {"role": "system", "content": "system prompt"},
            {"role": "user", "content": "make a vertical cut"},
            {"role": "assistant", "content": "", "tool_calls": [{
                "id": "call_1",
                "type": "function",
                "function": {
                    "name": "cinvar.media.import_folder",
                    "arguments": "{\"path\":\"/media/clips\"}"
                }
            }]},
            {"role": "tool", "tool_call_id": "call_1", "content": "{\"imported\":[]}"}
        ],
        "tools": [{
            "type": "function",
            "function": {
                "name": "cinvar.media.import_folder",
                "description": "Run cinvar.media.import_folder",
                "parameters": {"type": "object", "properties": {}, "required": []}
            }
        }],
        "tool_choice": "auto"
    });
    assert_eq!(body, expected);
}

#[test]
fn openai_body_omits_tools_when_none_are_offered() {
    let body = internals::openai_request_body(&request("m", vec![Message::user("hi")], Vec::new()));
    assert!(body.get("tools").is_none());
    assert!(body.get("tool_choice").is_none());
    assert_eq!(
        body["messages"][1],
        json!({"role": "user", "content": "hi"})
    );
}

#[test]
fn openai_parses_a_text_only_response() {
    let value = json!({
        "id": "chatcmpl-1",
        "model": "gpt-4o-mini",
        "choices": [{
            "message": {"role": "assistant", "content": "Hello there"},
            "finish_reason": "stop"
        }],
        "usage": {"prompt_tokens": 11, "completion_tokens": 7}
    });
    let response = internals::openai_parse_response(&value, "fallback-model").unwrap();
    assert_eq!(response.text, "Hello there");
    assert!(response.tool_calls.is_empty());
    assert_eq!(response.usage.prompt_tokens, 11);
    assert_eq!(response.usage.completion_tokens, 7);
    assert_eq!(response.stop_reason, "stop");
    assert_eq!(response.model, "gpt-4o-mini");
}

#[test]
fn openai_parses_a_tool_call_and_keeps_the_provider_id() {
    let value = json!({
        "choices": [{
            "message": {
                "role": "assistant",
                "content": null,
                "tool_calls": [{
                    "id": "call_abc",
                    "type": "function",
                    "function": {"name": "cinvar.project.status", "arguments": "{\"a\":1}"}
                }]
            },
            "finish_reason": "tool_calls"
        }]
    });
    let response = internals::openai_parse_response(&value, "m").unwrap();
    assert_eq!(response.tool_calls.len(), 1);
    assert_eq!(response.tool_calls[0].id, "call_abc");
    assert_eq!(response.tool_calls[0].name, "cinvar.project.status");
    assert_eq!(response.tool_calls[0].arguments, json!({"a": 1}));
    assert_eq!(response.usage.total(), 0, "absent usage maps to zero");

    let no_id = json!({
        "choices": [{
            "message": {"tool_calls": [
                {"type": "function", "function": {"name": "cinvar.project.status", "arguments": "{}"}}
            ]}
        }]
    });
    let response = internals::openai_parse_response(&no_id, "m").unwrap();
    assert!(
        response.tool_calls[0].id.starts_with("call_"),
        "id was {}",
        response.tool_calls[0].id
    );
}

#[test]
fn openai_keeps_a_turn_alive_when_tool_arguments_are_unparsable() {
    let value = json!({
        "choices": [{
            "message": {"tool_calls": [{
                "id": "call_bad",
                "type": "function",
                "function": {"name": "cinvar.timeline.create", "arguments": "{\"width\": "}
            }]}
        }]
    });
    let response = internals::openai_parse_response(&value, "m").unwrap();
    assert_eq!(response.tool_calls.len(), 1);
    assert_eq!(
        response.tool_calls[0].arguments,
        json!({"__malformed": "{\"width\":"})
    );
}

#[test]
fn anthropic_body_maps_tool_use_and_tool_result_blocks() {
    let body = internals::anthropic_request_body(
        &request(
            "claude-3-5-sonnet-latest",
            tool_round_messages(),
            vec![tool("cinvar.media.import_folder")],
        ),
        4_096,
        None,
    );
    assert_eq!(body["model"], json!("claude-3-5-sonnet-latest"));
    assert_eq!(body["max_tokens"], json!(4096));
    assert_eq!(
        body["system"],
        json!("system prompt"),
        "system is top level"
    );
    assert_eq!(body["messages"][0]["role"], json!("user"));
    assert_eq!(body["messages"][0]["content"][0]["type"], json!("text"));
    assert_eq!(body["messages"][1]["role"], json!("assistant"));
    assert_eq!(body["messages"][1]["content"][0]["type"], json!("tool_use"));
    assert_eq!(body["messages"][1]["content"][0]["id"], json!("call_1"));
    assert_eq!(
        body["messages"][1]["content"][0]["input"],
        json!({"path": "/media/clips"})
    );
    assert_eq!(body["messages"][2]["role"], json!("user"));
    assert_eq!(
        body["messages"][2]["content"][0]["type"],
        json!("tool_result")
    );
    assert_eq!(
        body["messages"][2]["content"][0]["tool_use_id"],
        json!("call_1")
    );
    assert_eq!(
        body["tools"][0]["name"],
        json!("cinvar.media.import_folder")
    );
    assert_eq!(
        body["tools"][0]["input_schema"],
        json!({"type": "object", "properties": {}, "required": []})
    );

    let parsed = internals::anthropic_parse_response(
        &json!({
            "model": "claude-3-5-sonnet-latest",
            "stop_reason": "tool_use",
            "content": [
                {"type": "text", "text": "importing"},
                {"type": "tool_use", "id": "toolu_1", "name": "cinvar.media.import_folder", "input": {"path": "/x"}}
            ],
            "usage": {"input_tokens": 20, "output_tokens": 5}
        }),
        "fallback",
    )
    .unwrap();
    assert_eq!(parsed.text, "importing");
    assert_eq!(parsed.tool_calls[0].id, "toolu_1");
    assert_eq!(parsed.tool_calls[0].arguments, json!({"path": "/x"}));
    assert_eq!(parsed.usage.prompt_tokens, 20);
    assert_eq!(parsed.usage.completion_tokens, 5);
    assert_eq!(parsed.stop_reason, "tool_use");
}

#[test]
fn gemini_body_maps_contents_and_function_declarations() {
    let body = internals::gemini_request_body(
        &request(
            "gemini-1.5-pro",
            tool_round_messages(),
            vec![tool("cinvar.media.import_folder")],
        ),
        None,
        None,
    );
    assert_eq!(
        body["systemInstruction"]["parts"][0]["text"],
        json!("system prompt")
    );
    assert_eq!(body["contents"][0]["role"], json!("user"));
    assert_eq!(
        body["contents"][0]["parts"][0]["text"],
        json!("make a vertical cut")
    );
    assert_eq!(body["contents"][1]["role"], json!("model"));
    assert_eq!(
        body["contents"][1]["parts"][0]["functionCall"],
        json!({"name": "cinvar.media.import_folder", "args": {"path": "/media/clips"}})
    );
    assert_eq!(body["contents"][2]["role"], json!("user"));
    assert_eq!(
        body["contents"][2]["parts"][0]["functionResponse"]["name"],
        json!("cinvar.media.import_folder"),
        "functionResponse carries the call name, recovered from the assistant turn"
    );
    assert_eq!(
        body["tools"][0]["functionDeclarations"][0]["name"],
        json!("cinvar.media.import_folder")
    );

    let parsed = internals::gemini_parse_response(
        &json!({
            "candidates": [{
                "content": {"role": "model", "parts": [{"functionCall": {"name": "cinvar.project.status", "args": {}}}]},
                "finishReason": "STOP"
            }],
            "usageMetadata": {"promptTokenCount": 9, "candidatesTokenCount": 4},
            "modelVersion": "gemini-1.5-pro"
        }),
        "fallback",
    )
    .unwrap();
    assert_eq!(parsed.tool_calls[0].name, "cinvar.project.status");
    assert!(
        parsed.tool_calls[0].id.starts_with("call_"),
        "gemini supplies no id"
    );
    assert_eq!(parsed.usage.prompt_tokens, 9);
    assert_eq!(parsed.usage.completion_tokens, 4);
    assert_eq!(parsed.stop_reason, "STOP");
}

#[test]
fn retry_classifier_covers_only_transient_statuses() {
    for status in [429u16, 500, 502, 503, 504] {
        assert!(
            internals::is_retryable_status(status),
            "{status} should be retried"
        );
    }
    for status in [200u16, 201, 400, 401, 403, 404, 409, 422, 499, 501] {
        assert!(
            !internals::is_retryable_status(status),
            "{status} should not be retried"
        );
    }
    assert_eq!(internals::backoff_schedule(), &[1u64, 2, 4]);
}

#[test]
fn error_text_is_redacted_and_truncated() {
    let message = internals::redacted_error_message(
        401,
        "{\"error\":{\"message\":\"Incorrect API key provided: sk-abcdefghijklmnopqrstuvwx\"}}",
    );
    assert!(message.contains("401"));
    assert!(message.contains("[redacted]"));
    assert!(!message.contains("sk-abc"), "key leaked: {message}");

    let flood = "x".repeat(5_000);
    let shortened = internals::shorten(&flood);
    assert!(shortened.len() < 700, "length {}", shortened.len());
    assert!(shortened.ends_with("...[truncated]"));
}

#[test]
fn a_missing_key_names_the_environment_variable() {
    if std::env::var("OPENAI_API_KEY").is_ok() || std::env::var("CINVAR_API_KEY").is_ok() {
        return;
    }
    let profile = ModelProfile {
        provider: "openai".to_string(),
        model: "gpt-4o-mini".to_string(),
        api_key_env: Some("CINVAR_TEST_ABSENT_KEY_W2".to_string()),
        ..Default::default()
    };
    let error = build("w2-absent", &profile, &Config::default())
        .err()
        .expect("no key should fail");
    assert_eq!(error.code, ErrorCode::Provider);
    assert!(
        error
            .hint
            .unwrap_or_default()
            .contains("CINVAR_TEST_ABSENT_KEY_W2"),
        "hint should name the env var"
    );
}

#[test]
fn an_unknown_provider_lists_the_supported_ones() {
    let profile = ModelProfile {
        provider: "acme-chat".to_string(),
        model: "acme-1".to_string(),
        base_url: None,
        ..Default::default()
    };
    let error = build("acme", &profile, &Config::default())
        .err()
        .expect("unknown provider should fail");
    assert_eq!(error.code, ErrorCode::InvalidInput);
    let hint = error.hint.unwrap_or_default();
    assert!(hint.contains("openai") && hint.contains("anthropic") && hint.contains("base_url"));
}

#[test]
fn an_unknown_provider_with_a_base_url_speaks_openai() {
    let profile = ModelProfile {
        provider: "acme-chat".to_string(),
        model: "acme-1".to_string(),
        base_url: Some("http://127.0.0.1:9999/v1".to_string()),
        ..Default::default()
    };
    let provider = build("acme", &profile, &Config::default()).expect("base_url makes it usable");
    assert_eq!(provider.id(), "acme-chat");
    assert!(provider.capabilities().tool_calling);
    assert!(!provider.capabilities().video);
}

fn mock_provider() -> Box<dyn Provider> {
    let profile = ModelProfile {
        provider: "mock".to_string(),
        model: "mock-planner".to_string(),
        ..Default::default()
    };
    build("mock", &profile, &Config::default()).expect("mock provider builds without credentials")
}

fn planner_tools() -> Vec<ToolSchema> {
    vec![
        tool("cinvar.project.status"),
        tool("cinvar.media.import_folder"),
        tool("cinvar.skills.list"),
        tool("cinvar.timeline.create"),
        tool("cinvar.timeline.append_clip"),
        tool("cinvar.timeline.text"),
        tool("cinvar.timeline.clip_audio"),
        tool("cinvar.render.export"),
        tool("cinvar.render.status"),
        tool("cinvar.qa.render_check"),
    ]
}

#[test]
fn mock_capabilities_are_honest() {
    let capabilities = mock_provider().capabilities();
    assert!(capabilities.tool_calling);
    assert!(!capabilities.images);
    assert!(!capabilities.video);
    assert!(!capabilities.streaming);
    assert_eq!(capabilities.context_window, 128_000);
    assert!(mock_provider().health().is_ok());
}

#[test]
fn mock_planner_first_three_steps() {
    let provider = mock_provider();
    let tools = planner_tools();
    let mut messages = vec![Message::user(
        "Make a vertical short from the footage in /media/clips and add a title",
    )];

    let first = provider
        .chat(&request("mock-planner", messages.clone(), tools.clone()))
        .unwrap();
    assert_eq!(first.tool_calls.len(), 1);
    assert_eq!(first.tool_calls[0].name, "cinvar.project.status");
    assert_eq!(first.tool_calls[0].arguments, json!({}));
    assert!(first.text.is_empty());

    let status_call = first.tool_calls[0].clone();
    messages.push(assistant_call(&status_call));
    messages.push(tool_output(
        &status_call,
        json!({"project": {"name": "Demo", "assets": [], "timelines": []}, "path": "/media/demo"}),
    ));

    let second = provider
        .chat(&request("mock-planner", messages.clone(), tools.clone()))
        .unwrap();
    assert_eq!(second.tool_calls.len(), 1);
    assert_eq!(second.tool_calls[0].name, "cinvar.media.import_folder");
    assert_eq!(
        second.tool_calls[0].arguments,
        json!({"path": "/media/clips"})
    );

    let import_call = second.tool_calls[0].clone();
    messages.push(assistant_call(&import_call));
    messages.push(tool_output(
        &import_call,
        json!({"imported": [
            {"asset_id": "asset_1", "file": "intro.mp4", "duration": 4.0},
            {"asset_id": "asset_2", "file": "theme.wav", "duration": 30.0},
            {"asset_id": "asset_3", "file": "outro.mov", "duration": 2.0}
        ]}),
    ));

    let third = provider
        .chat(&request("mock-planner", messages.clone(), tools.clone()))
        .unwrap();
    assert_eq!(third.tool_calls.len(), 1);
    assert_eq!(third.tool_calls[0].name, "cinvar.timeline.create");
    assert_eq!(
        third.tool_calls[0].arguments,
        json!({"name": "Main", "width": 1080, "height": 1920, "fps": "30"}),
        "a vertical request gets a vertical canvas"
    );

    let _ = provider;
}

#[test]
fn mock_planner_without_media_falls_back_to_skills() {
    let provider = mock_provider();
    let tools = planner_tools();
    let mut messages = vec![Message::user("Plan a square logo reveal")];
    let first = provider
        .chat(&request("mock-planner", messages.clone(), tools.clone()))
        .unwrap();
    let status_call = first.tool_calls[0].clone();
    messages.push(assistant_call(&status_call));
    messages.push(tool_output(
        &status_call,
        json!({"project": {"name": "Demo"}}),
    ));
    let second = provider
        .chat(&request("mock-planner", messages.clone(), tools.clone()))
        .unwrap();
    assert_eq!(second.tool_calls[0].name, "cinvar.skills.list");
}

#[test]
fn mock_planner_derives_a_title_of_at_most_forty_characters() {
    let provider = mock_provider();
    let tools = planner_tools();
    let long = "Please make a very long and descriptive cinematic launch video for the offline demo project";
    let mut messages = vec![Message::user(long)];
    // Walk to the title step: status, then skills.list, then create, then no assets.
    let sequence = [
        "cinvar.project.status",
        "cinvar.skills.list",
        "cinvar.timeline.create",
    ];
    for (index, expected) in sequence.iter().enumerate() {
        let response = provider
            .chat(&request("mock-planner", messages.clone(), tools.clone()))
            .unwrap();
        assert_eq!(&response.tool_calls[0].name, expected, "step {index}");
        let call = response.tool_calls[0].clone();
        let result = match call.name.as_str() {
            "cinvar.project.status" => json!({"project": {"name": "Demo"}}),
            "cinvar.skills.list" => json!({"skills": []}),
            _ => json!({"timeline_id": "tl_1", "width": 1920, "height": 1080}),
        };
        messages.push(assistant_call(&call));
        messages.push(tool_output(&call, result));
    }
    let response = provider
        .chat(&request("mock-planner", messages, tools))
        .unwrap();
    assert_eq!(response.tool_calls[0].name, "cinvar.timeline.text");
    let arguments = &response.tool_calls[0].arguments;
    assert_eq!(arguments["at"], json!("0"));
    assert_eq!(arguments["duration"], json!("3"));
    assert_eq!(arguments["size"], json!(96));
    assert_eq!(arguments["position"], json!("center"));
    let title = arguments["text"].as_str().unwrap_or_default();
    assert!(!title.is_empty());
    assert!(title.chars().count() <= 40, "title too long: {title}");
}

/// Drive the whole plan against synthetic tool results: no project, no ffmpeg
/// and no network are involved.
#[test]
fn mock_planner_completes_the_plan_offline() {
    let provider = mock_provider();
    let tools = planner_tools();
    let mut messages = vec![Message::user(
        "Make a vertical short from the footage in /media/clips and add a title",
    )];
    let mut order: Vec<String> = Vec::new();
    let mut polls = 0u32;
    let mut turns = 0u32;

    let final_text = loop {
        turns += 1;
        assert!(turns < 40, "planner did not finish: {order:?}");
        let response = provider
            .chat(&request("mock-planner", messages.clone(), tools.clone()))
            .unwrap();
        if response.tool_calls.is_empty() {
            break response.text.clone();
        }
        messages.push(Message {
            role: Role::Assistant,
            parts: Vec::new(),
            tool_calls: response.tool_calls.clone(),
            tool_call_id: None,
        });
        for call in &response.tool_calls {
            order.push(call.name.clone());
            let result = match call.name.as_str() {
                "cinvar.project.status" => {
                    json!({"project": {"name": "Demo"}, "path": "/media/demo"})
                }
                "cinvar.media.import_folder" => json!({"imported": [
                    {"asset_id": "asset_1", "file": "a.mp4", "duration": 3.0},
                    {"asset_id": "asset_2", "file": "b.mp4", "duration": 4.0}
                ]}),
                "cinvar.timeline.create" => {
                    json!({"timeline_id": "tl_1", "width": 1080, "height": 1920})
                }
                "cinvar.timeline.append_clip" => json!({
                    "clip_id": format!("clip_{}", call.arguments["asset_id"].as_str().unwrap_or("x")),
                    "timeline_duration": 7.0
                }),
                "cinvar.timeline.text" => json!({"clip_id": "clip_title", "text": "Short"}),
                "cinvar.timeline.clip_audio" => json!({"ok": true}),
                "cinvar.render.export" => json!({
                    "job_id": "job_1",
                    "output": "/media/demo/renders/demo-r1.mp4"
                }),
                "cinvar.render.status" => {
                    polls += 1;
                    if polls == 1 {
                        json!({"id": "job_1", "status": "running", "progress": 0.4})
                    } else {
                        json!({
                            "id": "job_1",
                            "status": "succeeded",
                            "output": {"output": "/media/demo/renders/demo-r1.mp4"}
                        })
                    }
                }
                "cinvar.qa.render_check" => json!({
                    "passed": true,
                    "checks": [{"name": "duration", "passed": true, "detail": "7.00s"}],
                    "score": 0.97
                }),
                other => {
                    json!({"error": {"code": "not_found", "message": format!("unknown tool {other}")}})
                }
            };
            messages.push(tool_output(call, result));
        }
    };

    assert_eq!(
        order[..3].to_vec(),
        vec![
            "cinvar.project.status".to_string(),
            "cinvar.media.import_folder".to_string(),
            "cinvar.timeline.create".to_string()
        ]
    );
    let appends: Vec<&String> = order
        .iter()
        .filter(|name| name.as_str() == "cinvar.timeline.append_clip")
        .collect();
    assert_eq!(appends.len(), 2, "one append per imported video");
    let audio: Vec<&String> = order
        .iter()
        .filter(|name| name.as_str() == "cinvar.timeline.clip_audio")
        .collect();
    assert_eq!(audio.len(), 2, "one audio call per appended clip");
    assert_eq!(
        order
            .iter()
            .filter(|name| name.as_str() == "cinvar.render.status")
            .count(),
        2,
        "queued/running is polled until the job settles"
    );
    assert_eq!(
        order.last().map(|name| name.as_str()),
        Some("cinvar.qa.render_check")
    );

    assert!(
        final_text.contains("offline mock planner"),
        "summary: {final_text}"
    );
    assert!(
        final_text.contains("/media/demo/renders/demo-r1.mp4"),
        "summary: {final_text}"
    );
    assert!(
        final_text.contains("7.00s"),
        "timeline length reported: {final_text}"
    );
    assert!(
        final_text.contains("QA verdict: passed"),
        "qa verdict: {final_text}"
    );
    assert!(
        final_text.contains("/media/clips"),
        "imported folder: {final_text}"
    );
}

#[test]
fn mock_planner_stops_and_reports_a_failed_tool() {
    let provider = mock_provider();
    let tools = planner_tools();
    let mut messages = vec![Message::user("Cut the footage in /media/clips")];
    let first = provider
        .chat(&request("mock-planner", messages.clone(), tools.clone()))
        .unwrap();
    let status_call = first.tool_calls[0].clone();
    messages.push(assistant_call(&status_call));
    messages.push(Message::tool_result(
        status_call.id.clone(),
        json!({"error": {"code": "not_found", "message": "/media/clips is not a folder", "hint": "check the path"}})
            .to_string(),
    ));
    let response = provider
        .chat(&request("mock-planner", messages, tools))
        .unwrap();
    assert!(
        response.tool_calls.is_empty(),
        "no further calls after a failure"
    );
    assert!(response.text.contains("offline mock planner"));
    assert!(response.text.contains("/media/clips is not a folder"));
    assert!(response.text.contains("check the path"));
}

#[test]
fn mock_planner_ignores_json_mode() {
    let provider = mock_provider();
    let mut wire = request(
        "mock-planner",
        vec![Message::user("Cut the footage in /media/clips")],
        planner_tools(),
    );
    wire.json_mode = true;
    let response = provider.chat(&wire).unwrap();
    assert_eq!(response.tool_calls[0].name, "cinvar.project.status");
}
