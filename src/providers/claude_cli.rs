//! "claude-cli" provider: answers through the user's own installed `claude` (Claude Code) binary.
//!
//! The binary holds the user's subscription login; CINVAR never reads, stores or forwards that
//! credential. Claude Code's own tools are switched off, so it acts as a plain text model. Tool
//! calling is emulated: the tool schemas go into the prompt and the reply is parsed as one JSON object.

use serde_json::{json, Value};
use std::io::Write;
use std::process::{Command, Stdio};

use crate::config::ModelProfile;
use crate::error::{Error, ErrorCode, Result};
use crate::providers::{
    Capabilities, ChatRequest, ChatResponse, Provider, Role, ToolCall, Usage,
};
use crate::util::ids;

pub(crate) struct ClaudeCliProvider {
    model: String,
}

impl ClaudeCliProvider {
    pub(crate) fn new(profile: &ModelProfile) -> ClaudeCliProvider {
        ClaudeCliProvider { model: profile.model.clone() }
    }
}

fn run_claude(args: &[&str], stdin: Option<&str>) -> Result<String> {
    let mut child = Command::new("claude")
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| {
            Error::new(ErrorCode::DependencyMissing, format!("cannot run `claude`: {e}"))
                .with_hint("install Claude Code and sign in with `claude auth login`")
        })?;
    if let Some(mut pipe) = child.stdin.take() {
        if let Some(text) = stdin {
            let _ = pipe.write_all(text.as_bytes());
        }
    }
    let out = child.wait_with_output().map_err(|e| Error::new(ErrorCode::Provider, e.to_string()))?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        return Err(Error::new(ErrorCode::Provider, format!("claude exited with {}: {}", out.status, err.trim().chars().take(300).collect::<String>()))
            .with_hint("run `claude auth login` in a terminal"));
    }
    Ok(String::from_utf8_lossy(&out.stdout).to_string())
}

fn transcript(request: &ChatRequest) -> String {
    let mut s = String::new();
    for m in &request.messages {
        let who = match m.role {
            Role::System => "SYSTEM",
            Role::User => "USER",
            Role::Assistant => "ASSISTANT",
            Role::Tool => "TOOL_RESULT",
        };
        s.push_str(&format!("[{who}]\n{}\n", m.text()));
        for c in &m.tool_calls {
            s.push_str(&format!("(called {} with {})\n", c.name, c.arguments));
        }
    }
    s
}

/// Pull the first balanced JSON object out of a reply (models sometimes add prose or fences).
pub(crate) fn extract_json(text: &str) -> Option<Value> {
    let start = text.find('{')?;
    let bytes = text.as_bytes();
    let (mut depth, mut in_str, mut esc) = (0i32, false, false);
    for (i, &b) in bytes.iter().enumerate().skip(start) {
        if in_str {
            if esc { esc = false } else if b == b'\\' { esc = true } else if b == b'"' { in_str = false }
            continue;
        }
        match b {
            b'"' => in_str = true,
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    return serde_json::from_str(&text[start..=i]).ok();
                }
            }
            _ => {}
        }
    }
    None
}

pub(crate) fn parse_reply(raw: &str) -> (String, Vec<ToolCall>) {
    if let Some(v) = extract_json(raw) {
        let calls: Vec<ToolCall> = v["tool_calls"].as_array().map(|a| {
            a.iter().filter_map(|c| {
                let name = c["name"].as_str()?.to_string();
                Some(ToolCall { id: ids::new_id("call"), name, arguments: c.get("arguments").cloned().unwrap_or(json!({})) })
            }).collect()
        }).unwrap_or_default();
        if v.get("tool_calls").is_some() || v.get("text").is_some() {
            return (v["text"].as_str().unwrap_or("").to_string(), calls);
        }
    }
    (raw.trim().to_string(), Vec::new())
}

impl Provider for ClaudeCliProvider {
    fn id(&self) -> &str { "claude-cli" }

    fn capabilities(&self) -> Capabilities {
        Capabilities { tool_calling: true, structured_output: false, images: false, video: false, audio: false, streaming: false, context_window: 200_000 }
    }

    fn chat(&self, request: &ChatRequest) -> Result<ChatResponse> {
        let tools: Vec<Value> = request.tools.iter().map(|t| json!({"name": t.name, "description": t.description, "input_schema": t.input_schema})).collect();
        let plain = request.tools.is_empty(); // no tools to call: ask for the answer itself, not the JSON envelope
        let system = if plain {
            request.system.clone()
        } else {
            format!(
                "{}\n\nYou are the planner inside a video editor. You have NO tools of your own. Reply with exactly one JSON object and nothing else: \
                 {{\"text\": \"<message to the user, may be empty>\", \"tool_calls\": [{{\"name\": \"<tool>\", \"arguments\": {{}}}}]}}. \
                 Use tool_calls to act; when the task is finished, return an empty tool_calls list and put the final answer in text. \
                 Available tools (JSON): {}",
                request.system, serde_json::to_string(&tools).unwrap_or_default())
        };
        // The prompt goes through a private temp file: tool schemas can be large, and one argv entry is capped (~128 KiB).
        let prompt_path = std::env::temp_dir().join(format!("cinvar-claude-{}.txt", ids::new_id("sp")));
        std::fs::write(&prompt_path, &system).map_err(|e| Error::new(ErrorCode::Io, format!("cannot write prompt file: {e}")))?;
        let prompt_arg = prompt_path.to_string_lossy().to_string();
        // --strict-mcp-config / --setting-sources "" keep the user's own MCP servers, hooks and CLAUDE.md out of the call.
        let mut args = vec!["-p", "--output-format", "json", "--no-session-persistence", "--tools", "", "--disable-slash-commands", "--strict-mcp-config", "--setting-sources", "", "--system-prompt-file", &prompt_arg];
        if !self.model.is_empty() && self.model != "default" {
            args.push("--model");
            args.push(&self.model);
        }
        let ran = run_claude(&args, Some(&transcript(request)));
        let _ = std::fs::remove_file(&prompt_path);
        let out = ran?;
        let v: Value = serde_json::from_str(out.trim()).map_err(|e| Error::new(ErrorCode::Provider, format!("unreadable claude output: {e}")))?;
        if v["is_error"].as_bool() == Some(true) {
            return Err(Error::new(ErrorCode::Provider, format!("claude: {}", v["result"].as_str().unwrap_or("error").chars().take(300).collect::<String>())));
        }
        let raw = v["result"].as_str().unwrap_or("");
        let (text, tool_calls) = if plain { (raw.trim().to_string(), Vec::new()) } else { parse_reply(raw) };
        let stop = if tool_calls.is_empty() { "end_turn" } else { "tool_use" };
        Ok(ChatResponse {
            text, tool_calls,
            usage: Usage { prompt_tokens: v["usage"]["input_tokens"].as_u64().unwrap_or(0), completion_tokens: v["usage"]["output_tokens"].as_u64().unwrap_or(0) },
            stop_reason: stop.into(), model: self.model.clone(), raw: None,
        })
    }

    fn health(&self) -> Result<()> {
        let out = run_claude(&["auth", "status"], None)?;
        let v: Value = serde_json::from_str(out.trim()).unwrap_or(json!({}));
        if v["loggedIn"].as_bool() == Some(true) { Ok(()) } else {
            Err(Error::new(ErrorCode::Provider, "claude is not signed in").with_hint("run `claude auth login`"))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn parses_tool_calls_with_fences() {
        let (t, c) = parse_reply("```json\n{\"text\":\"hi\",\"tool_calls\":[{\"name\":\"cinvar.project.status\",\"arguments\":{\"a\":\"}\"}}]}\n```");
        assert_eq!(t, "hi"); assert_eq!(c.len(), 1); assert_eq!(c[0].name, "cinvar.project.status");
    }
    #[test]
    fn plain_text_passes_through() {
        let (t, c) = parse_reply("just words"); assert_eq!(t, "just words"); assert!(c.is_empty());
    }
}
