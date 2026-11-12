//! Deterministic offline planner used by the "mock" provider.
//!
//! It needs no network and no model: every decision is derived from the tool
//! schemas and the tool results already present in the message list, so the same
//! transcript always produces the same plan. It drives a complete edit:
//!
//! 1. cinvar.project.status
//! 2. cinvar.media.import_folder (when the request names footage in a folder) or
//!    cinvar.skills.list
//! 3. cinvar.timeline.create, sized from the request (vertical/square/landscape)
//! 4. one cinvar.timeline.append_clip per imported video asset, in import order
//! 5. cinvar.timeline.text with a short title, unless the request says "no title"
//! 6. one cinvar.timeline.clip_audio per appended clip
//! 7. cinvar.render.export, then one cinvar.render.status per turn while the job
//!    is queued or running
//! 8. cinvar.qa.render_check on the rendered path
//! 9. a plain-text summary
//!
//! Every id and path comes from a real tool result. A failed tool stops the plan
//! and is reported instead of being retried blindly. json_mode is ignored.

use regex::Regex;
use serde_json::{json, Value};

use crate::config::ModelProfile;
use crate::error::Result;
use crate::providers::{
    Capabilities, ChatRequest, ChatResponse, Message, Provider, Role, ToolCall, Usage,
};
use crate::util::ids;

const STATUS: &str = "cinvar.project.status";
const IMPORT: &str = "cinvar.media.import_folder";
const SKILLS: &str = "cinvar.skills.list";
const CREATE: &str = "cinvar.timeline.create";
const APPEND: &str = "cinvar.timeline.append_clip";
const TEXT: &str = "cinvar.timeline.text";
const AUDIO: &str = "cinvar.timeline.clip_audio";
const EXPORT: &str = "cinvar.render.export";
const RENDER_STATUS: &str = "cinvar.render.status";
const QA: &str = "cinvar.qa.render_check";

const STAGE_STATUS: usize = 0;
const STAGE_INSPECT: usize = 1;
const STAGE_CREATE: usize = 2;
const STAGE_APPEND: usize = 3;
const STAGE_TEXT: usize = 4;
const STAGE_AUDIO: usize = 5;
const STAGE_EXPORT: usize = 6;
const STAGE_POLL: usize = 7;
const STAGE_QA: usize = 8;

pub(crate) struct MockProvider {
    id: String,
    context_window: u32,
}

impl MockProvider {
    pub(crate) fn new(profile: &ModelProfile) -> MockProvider {
        MockProvider {
            id: "mock".to_string(),
            context_window: profile.context_window,
        }
    }
}

impl Provider for MockProvider {
    fn id(&self) -> &str {
        &self.id
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities {
            tool_calling: true,
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
        let step = plan_next(request);
        Ok(match step {
            MockStep::Calls(calls) => ChatResponse {
                text: String::new(),
                tool_calls: calls,
                usage: Usage::default(),
                stop_reason: "tool_use".to_string(),
                model: request.model.clone(),
                raw: None,
            },
            MockStep::Text(text) => ChatResponse {
                text,
                tool_calls: Vec::new(),
                usage: Usage::default(),
                stop_reason: "end_turn".to_string(),
                model: request.model.clone(),
                raw: None,
            },
        })
    }

    fn health(&self) -> Result<()> {
        Ok(())
    }
}

enum MockStep {
    Calls(Vec<ToolCall>),
    Text(String),
}

/// One completed tool call: what was asked and what came back.
struct Trace {
    name: String,
    arguments: Value,
    result: Value,
}

fn call(name: &str, arguments: Value) -> ToolCall {
    ToolCall {
        id: ids::new_id("call"),
        name: name.to_string(),
        arguments,
    }
}

fn plan_next(request: &ChatRequest) -> MockStep {
    let traces = collect_traces(&request.messages);
    let user = first_user_message(&request.messages);

    if let Some(report) = failure_report(&traces) {
        return MockStep::Text(report);
    }
    if request.tools.is_empty() {
        return MockStep::Text(summary(&traces, &user));
    }

    let mut stage = STAGE_STATUS;
    for trace in &traces {
        stage = advance(stage, &trace.name);
    }
    stage = settle(stage, &traces, &user);

    match stage {
        STAGE_STATUS => MockStep::Calls(vec![call(STATUS, json!({}))]),
        STAGE_INSPECT => MockStep::Calls(vec![inspect(&user)]),
        STAGE_CREATE => {
            let (width, height) = canvas(&user);
            MockStep::Calls(vec![call(
                CREATE,
                json!({"name": "Main", "width": width, "height": height, "fps": "30"}),
            )])
        }
        STAGE_APPEND => append_step(&traces, &user),
        STAGE_TEXT => title_step(&user).unwrap_or_else(|| audio_step(&traces)),
        STAGE_AUDIO => audio_step(&traces),
        STAGE_EXPORT => MockStep::Calls(vec![call(EXPORT, json!({}))]),
        STAGE_POLL => poll_step(&traces),
        _ => MockStep::Text(summary(&traces, &user)),
    }
}

fn advance(stage: usize, name: &str) -> usize {
    match name {
        STATUS if stage == STAGE_STATUS => STAGE_INSPECT,
        IMPORT | SKILLS if stage == STAGE_INSPECT => STAGE_CREATE,
        CREATE if stage == STAGE_CREATE => STAGE_APPEND,
        APPEND if stage == STAGE_APPEND => STAGE_APPEND,
        TEXT if (STAGE_APPEND..=STAGE_TEXT).contains(&stage) => STAGE_TEXT,
        AUDIO if (STAGE_APPEND..=STAGE_AUDIO).contains(&stage) => STAGE_AUDIO,
        EXPORT if (STAGE_APPEND..=STAGE_EXPORT).contains(&stage) => STAGE_EXPORT,
        RENDER_STATUS if stage == STAGE_EXPORT || stage == STAGE_POLL => STAGE_POLL,
        QA if stage == STAGE_POLL => STAGE_QA,
        _ => stage,
    }
}

/// Roll a stage forward when its work is already done, so a repeated chat call
/// never re-issues an append, a title, an audio pass or the export.
fn settle(mut stage: usize, traces: &[Trace], user: &str) -> usize {
    loop {
        let before = stage;
        match stage {
            STAGE_APPEND => {
                let expected = imported_videos(traces).len();
                let done = count(traces, APPEND);
                if expected == 0 || done >= expected {
                    stage = STAGE_TEXT;
                }
            }
            STAGE_TEXT => {
                if count(traces, TEXT) > 0 || user.to_ascii_lowercase().contains("no title") {
                    stage = STAGE_AUDIO;
                }
            }
            STAGE_AUDIO => {
                let expected = appended_clips(traces).len();
                let done = count(traces, AUDIO);
                if expected == 0 || done >= expected {
                    stage = STAGE_EXPORT;
                }
            }
            STAGE_EXPORT if count(traces, EXPORT) > 0 => {
                stage = STAGE_POLL;
            }
            _ => {}
        }
        if stage == before {
            return stage;
        }
    }
}

fn count(traces: &[Trace], name: &str) -> usize {
    traces.iter().filter(|trace| trace.name == name).count()
}

fn inspect(user: &str) -> ToolCall {
    let wants_media = ["footage", "clip", "folder", "video"]
        .iter()
        .any(|word| user.to_ascii_lowercase().contains(word));
    if wants_media {
        if let Some(path) = extract_path(user) {
            return call(IMPORT, json!({"path": path}));
        }
    }
    call(SKILLS, json!({}))
}

fn append_step(traces: &[Trace], user: &str) -> MockStep {
    let assets = imported_videos(traces);
    let done = count(traces, APPEND);
    if done >= assets.len() {
        return title_step(user).unwrap_or_else(|| audio_step(traces));
    }
    MockStep::Calls(
        assets
            .into_iter()
            .skip(done)
            .map(|asset| call(APPEND, json!({"asset_id": asset})))
            .collect(),
    )
}

fn title_step(user: &str) -> Option<MockStep> {
    if user.to_ascii_lowercase().contains("no title") {
        return None;
    }
    let title = title_for(user);
    Some(MockStep::Calls(vec![call(
        TEXT,
        json!({"text": title, "at": "0", "duration": "3", "size": 96, "position": "center"}),
    )]))
}

fn audio_step(traces: &[Trace]) -> MockStep {
    let clips = appended_clips(traces);
    let done = count(traces, AUDIO);
    if done >= clips.len() {
        return MockStep::Calls(vec![call(EXPORT, json!({}))]);
    }
    MockStep::Calls(
        clips
            .into_iter()
            .skip(done)
            .map(|clip| call(AUDIO, json!({"clip_id": clip, "normalize": true})))
            .collect(),
    )
}

fn poll_step(traces: &[Trace]) -> MockStep {
    let last = traces
        .iter()
        .rev()
        .find(|trace| trace.name == RENDER_STATUS);
    let job_id = last
        .and_then(|trace| trace.arguments.get("job_id").and_then(|v| v.as_str()))
        .map(|v| v.to_string())
        .or_else(|| export_job_id(traces))
        .unwrap_or_default();
    if last.is_none() {
        return if job_id.is_empty() {
            MockStep::Text(
                "The offline mock planner stopped: the export did not return a job id to poll."
                    .to_string(),
            )
        } else {
            MockStep::Calls(vec![call(
                RENDER_STATUS,
                json!({"job_id": job_id, "wait_seconds": 60}),
            )])
        };
    }
    let status = last
        .and_then(|trace| trace.result.get("status").and_then(|v| v.as_str()))
        .unwrap_or_default()
        .to_string();

    match status.as_str() {
        "queued" | "running" => {
            MockStep::Calls(vec![call(RENDER_STATUS, json!({"job_id": job_id, "wait_seconds": 60}))])
        }
        "failed" | "cancelled" => {
            let message = last
                .and_then(|trace| trace.result.get("message").and_then(|v| v.as_str()))
                .unwrap_or("the render job did not finish");
            MockStep::Text(format!(
                "The offline mock planner stopped: the render job ended as {status} ({message}).",
            ))
        }
        _ => match last.and_then(|trace| output_path(&trace.result)) {
            Some(path) => MockStep::Calls(vec![call(QA, json!({"path": path}))]),
            None => MockStep::Text(
                "The offline mock planner stopped: the render job reported success without an output path, so there is nothing to verify."
                    .to_string(),
            ),
        },
    }
}

fn collect_traces(messages: &[Message]) -> Vec<Trace> {
    let mut known: Vec<(String, String, Value)> = Vec::new();
    for message in messages {
        for tool_call in &message.tool_calls {
            known.push((
                tool_call.id.clone(),
                tool_call.name.clone(),
                tool_call.arguments.clone(),
            ));
        }
    }
    let mut traces: Vec<Trace> = Vec::new();
    for message in messages {
        if message.role != Role::Tool {
            continue;
        }
        let Some(id) = message.tool_call_id.as_ref() else {
            continue;
        };
        let Some((_, name, arguments)) = known.iter().find(|(known_id, _, _)| known_id == id)
        else {
            continue;
        };
        let text = message.text();
        let result =
            serde_json::from_str::<Value>(text.trim()).unwrap_or_else(|_| json!({"raw": text}));
        traces.push(Trace {
            name: name.clone(),
            arguments: arguments.clone(),
            result,
        });
    }
    traces
}

fn first_user_message(messages: &[Message]) -> String {
    messages
        .iter()
        .find(|message| message.role == Role::User && message.tool_call_id.is_none())
        .map(|message| message.text())
        .unwrap_or_default()
}

fn failure_report(traces: &[Trace]) -> Option<String> {
    let last = traces.last()?;
    let error = last.result.get("error").filter(|value| !value.is_null())?;
    let code = error
        .get("code")
        .and_then(|v| v.as_str())
        .unwrap_or("error");
    let message = error
        .get("message")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown error");
    let hint = error
        .get("hint")
        .and_then(|v| v.as_str())
        .unwrap_or_default();
    let mut report = format!(
        "The offline mock planner stopped: {} failed ({code}): {message}",
        last.name
    );
    if !hint.is_empty() {
        report.push_str(&format!("\nHint: {hint}"));
    }
    Some(report)
}

fn imported_videos(traces: &[Trace]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for trace in traces.iter().filter(|trace| trace.name == IMPORT) {
        let Some(entries) = trace.result.get("imported").and_then(|v| v.as_array()) else {
            continue;
        };
        for entry in entries {
            if entry.get("error").is_some() {
                continue;
            }
            let Some(asset_id) = entry.get("asset_id").and_then(|v| v.as_str()) else {
                continue;
            };
            let file = entry
                .get("file")
                .and_then(|v| v.as_str())
                .unwrap_or_default();
            if is_video(file) {
                out.push(asset_id.to_string());
            }
        }
    }
    out
}

fn appended_clips(traces: &[Trace]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for trace in traces.iter().filter(|trace| trace.name == APPEND) {
        if let Some(clip) = trace.result.get("clip_id").and_then(|v| v.as_str()) {
            out.push(clip.to_string());
        }
    }
    out
}

fn export_job_id(traces: &[Trace]) -> Option<String> {
    traces
        .iter()
        .rev()
        .find(|trace| trace.name == EXPORT)
        .and_then(|trace| trace.result.get("job_id").and_then(|v| v.as_str()))
        .map(|v| v.to_string())
}

fn output_path(snapshot: &Value) -> Option<String> {
    let output = snapshot.get("output")?;
    if let Some(text) = output.as_str() {
        return Some(text.to_string());
    }
    output
        .get("output")
        .and_then(|v| v.as_str())
        .map(|v| v.to_string())
}

fn is_video(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    [".mp4", ".mov", ".mkv", ".webm", ".avi", ".m4v"]
        .iter()
        .any(|ext| lower.ends_with(ext))
}

/// Pull a folder path out of the request: "./relative/path" first, plain
/// "/absolute/path" otherwise.
fn extract_path(request: &str) -> Option<String> {
    let relative = Regex::new(r#"\./[^ \t"]+"#).ok();
    let absolute = Regex::new(r#"/[^ \t"]+"#).ok();
    let found = relative
        .as_ref()
        .and_then(|re| re.find(request))
        .or_else(|| absolute.as_ref().and_then(|re| re.find(request)))?;
    let trimmed = found
        .as_str()
        .trim_end_matches(|c: char| {
            matches!(
                c,
                '.' | ',' | ';' | ':' | ')' | ']' | '}' | '\'' | '"' | '!' | '?'
            )
        })
        .trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

fn canvas(request: &str) -> (u32, u32) {
    let lower = request.to_ascii_lowercase();
    if ["vertical", "shorts", "tiktok", "reel"]
        .iter()
        .any(|word| lower.contains(word))
    {
        (1080, 1920)
    } else if lower.contains("square") {
        (1080, 1080)
    } else {
        (1920, 1080)
    }
}

/// A short on-screen title derived from the request, at most 40 characters.
fn title_for(request: &str) -> String {
    let mut text = request.to_string();
    for pattern in [r#"\./[^ \t"]+"#, r#"/[^ \t"]+"#] {
        if let Ok(re) = Regex::new(pattern) {
            text = re.replace_all(&text, " ").to_string();
        }
    }
    let filler = [
        "please", "make", "create", "build", "edit", "cut", "assemble", "a", "an", "the", "me",
        "my",
    ];
    let words: Vec<&str> = text.split_whitespace().collect();
    let mut start = 0;
    while start < words.len() && filler.contains(&words[start].to_ascii_lowercase().as_str()) {
        start += 1;
    }
    let mut joined = words[start..].join(" ");
    if joined.is_empty() {
        joined = words.join(" ");
    }
    joined = joined
        .trim_matches(|c: char| !c.is_alphanumeric() && c != ' ')
        .trim()
        .to_string();
    if joined.is_empty() {
        joined = "Untitled".to_string();
    }
    let limited: String = joined.chars().take(40).collect();
    let limited = limited.trim_end().to_string();
    let mut chars = limited.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
        None => "Untitled".to_string(),
    }
}

fn summary(traces: &[Trace], user: &str) -> String {
    let mut lines: Vec<String> = Vec::new();
    let folder = traces
        .iter()
        .filter(|trace| trace.name == IMPORT)
        .find_map(|trace| trace.arguments.get("path").and_then(|v| v.as_str()))
        .map(|v| v.to_string());
    let videos = imported_videos(traces);
    match &folder {
        Some(dir) => lines.push(format!(
            "Imported {} video file(s) from {dir}.",
            videos.len()
        )),
        None => {
            let why = if user.trim().is_empty() {
                "the request was empty".to_string()
            } else {
                "the request did not name a folder of footage".to_string()
            };
            lines.push(format!("No media was imported because {why}."));
        }
    }

    if let Some(created) = traces.iter().find(|trace| trace.name == CREATE) {
        let width = created
            .result
            .get("width")
            .and_then(|v| v.as_u64())
            .unwrap_or(0);
        let height = created
            .result
            .get("height")
            .and_then(|v| v.as_u64())
            .unwrap_or(0);
        let name = created
            .arguments
            .get("name")
            .and_then(|v| v.as_str())
            .unwrap_or("Main");
        lines.push(format!("Created timeline '{name}' at {width}x{height}."));
    }
    let clips = appended_clips(traces);
    if !clips.is_empty() {
        lines.push(format!("Placed {} clip(s) on the timeline.", clips.len()));
    }
    if let Some(duration) = traces
        .iter()
        .filter(|trace| trace.name == APPEND)
        .filter_map(|trace| {
            trace
                .result
                .get("timeline_duration")
                .and_then(|v| v.as_f64())
        })
        .next_back()
    {
        lines.push(format!("Timeline length: {duration:.2}s."));
    }
    if traces.iter().any(|trace| trace.name == TEXT) {
        lines.push("Added a centred title overlay.".to_string());
    }
    if traces.iter().any(|trace| trace.name == AUDIO) {
        lines.push("Normalised the audio on every clip.".to_string());
    }

    let render = traces
        .iter()
        .rev()
        .find(|trace| trace.name == RENDER_STATUS);
    match render.and_then(|trace| output_path(&trace.result)) {
        Some(path) => lines.push(format!("Rendered: {path}")),
        None => lines.push("No render output was reported.".to_string()),
    }
    match traces.iter().rev().find(|trace| trace.name == QA) {
        Some(check) => {
            let passed = check
                .result
                .get("passed")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            let failed: Vec<String> = check
                .result
                .get("checks")
                .and_then(|v| v.as_array())
                .map(|items| {
                    items
                        .iter()
                        .filter(|item| item.get("passed").and_then(|v| v.as_bool()) == Some(false))
                        .filter_map(|item| {
                            item.get("name")
                                .and_then(|v| v.as_str())
                                .map(|v| v.to_string())
                        })
                        .collect()
                })
                .unwrap_or_default();
            if failed.is_empty() {
                lines.push(format!(
                    "QA verdict: {}.",
                    if passed { "passed" } else { "failed" }
                ));
            } else {
                lines.push(format!("QA verdict: failed ({}).", failed.join(", ")));
            }
        }
        None => lines.push("QA verdict: not checked.".to_string()),
    }
    lines.push("Produced by the offline mock planner (no network, deterministic).".to_string());
    lines.join("\n")
}
