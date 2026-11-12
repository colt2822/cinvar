pub mod plan;

use std::time::Instant;

use serde::{Deserialize, Serialize};

use crate::config::Config;
use crate::error::{Error, ErrorCode, Result};
use crate::jobs::Jobs;
use crate::project::history::History;
use crate::project::model::Project;
use crate::project::store::ProjectStore;
use crate::providers::{build, ChatRequest, Message, Provider, Usage};
use crate::tools::{Registry, ToolCtx};
use crate::util::text;

pub use plan::{EditPlan, PlanStep};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentOptions {
    pub max_iterations: u32,
    pub dry_run: bool,
    pub max_tool_result_chars: usize,
}

impl Default for AgentOptions {
    fn default() -> Self {
        AgentOptions {
            max_iterations: 24,
            dry_run: false,
            max_tool_result_chars: 12_000,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TurnOutcome {
    pub text: String,
    pub iterations: u32,
    pub tool_calls: Vec<ToolTrace>,
    pub usage: Usage,
    pub stop_reason: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolTrace {
    pub name: String,
    pub arguments: serde_json::Value,
    pub ok: bool,
    pub result: serde_json::Value,
    pub error: Option<String>,
}

pub struct AgentSession {
    pub config: Config,
    pub profile_name: String,
    pub model: String,
    pub provider: Box<dyn Provider>,
    pub registry: Registry,
    pub messages: Vec<Message>,
    pub usage: Usage,
    pub started: Instant,
}

impl AgentSession {
    pub fn new(config: &Config, profile_name: Option<&str>) -> Result<AgentSession> {
        let name = match profile_name {
            Some(name) => name.to_string(),
            None => config.active_model.clone().ok_or_else(|| {
                Error::invalid("no model configured").with_hint(
                    "add one with: cinvar models add local --provider mock --model mock-planner",
                )
            })?,
        };
        let profile = config.profile(&name)?.clone();
        let provider = build(&name, &profile, config)?;
        let registry = crate::tools::build_default();
        Ok(AgentSession {
            config: config.clone(),
            profile_name: name,
            model: profile.model,
            provider,
            registry,
            messages: Vec::new(),
            usage: Usage::default(),
            started: Instant::now(),
        })
    }

    pub fn system_prompt(&self, project: &Project) -> String {
        let mut prompt = String::new();
        prompt.push_str(
            "You are CINVAR, an AI video editing agent. You operate a real non-destructive editing engine: every edit you make is stored in the project file and every render produces a real video file that can be inspected with ffprobe.\n\n",
        );
        prompt.push_str("Operating rules:\n");
        prompt.push_str("1. Call tools to inspect before you decide. Never claim footage, duration or quality you have not measured with a tool.\n");
        prompt.push_str("2. Imported media is copied into the project; original files are never modified. You have no shell access and cannot touch files outside the project.\n");
        prompt.push_str("3. Build the cut on the timeline with cinvar.timeline.* tools, then render with cinvar.render.export and verify with cinvar.qa.render_check before reporting success.\n");
        prompt.push_str("4. Renders run as background jobs. Poll cinvar.render.status until the job is succeeded or failed.\n");
        prompt.push_str("5. When a request is style specific, load the matching skill with cinvar.skills.load and follow its guidance.\n");
        prompt.push_str("6. If something is impossible with the available tools, say so plainly and explain what is missing. Never invent a result.\n");
        prompt.push_str("7. Keep tool arguments in the documented format; times accept seconds, MM:SS or HH:MM:SS.mmm.\n\n");

        prompt.push_str("Project state:\n");
        let digest = serde_json::json!({
            "name": project.name,
            "revision": project.revision,
            "assets": project.assets.iter().map(|a| serde_json::json!({
                "id": a.id,
                "name": a.display_name(),
                "kind": format!("{:?}", a.kind).to_lowercase(),
                "seconds": a.info.as_ref().map(|i| (i.duration.as_secs_f64() * 100.0).round() / 100.0),
                "size": a.info.as_ref().map(|i| format!("{}x{}", i.width, i.height)),
                "has_audio": a.info.as_ref().map(|i| i.has_audio),
            })).collect::<Vec<_>>(),
            "timelines": project.timelines.iter().map(|t| serde_json::json!({
                "id": t.id,
                "name": t.name,
                "seconds": (t.duration().as_secs_f64() * 100.0).round() / 100.0,
                "size": format!("{}x{}", t.width, t.height),
                "tracks": t.tracks.iter().map(|tr| serde_json::json!({
                    "id": tr.id,
                    "kind": format!("{:?}", tr.kind).to_lowercase(),
                    "clips": tr.clips.len(),
                })).collect::<Vec<_>>(),
            })).collect::<Vec<_>>(),
            "captions": project.captions.iter().map(|c| c.cues.len()).sum::<usize>(),
        });
        prompt.push_str(&serde_json::to_string_pretty(&digest).unwrap_or_default());
        prompt.push_str("\n\nAvailable tools:\n");
        prompt.push_str(&self.registry.prompt_index());
        match crate::skills::load_all() {
            Ok(skills) if !skills.is_empty() => {
                prompt.push_str(
                    "Installed skills (load the relevant one before a style-specific edit):\n",
                );
                for skill in skills {
                    prompt.push_str(&format!("- {}: {}\n", skill.name, skill.summary));
                }
            }
            _ => {}
        }
        prompt
    }

    pub fn send(
        &mut self,
        project: &mut Project,
        store: &ProjectStore,
        jobs: &Jobs,
        history: &mut History,
        request: &str,
        opts: &AgentOptions,
    ) -> Result<TurnOutcome> {
        let system = self.system_prompt(project);
        self.messages.push(Message::user(request));
        let mut traces: Vec<ToolTrace> = Vec::new();
        let tools = self.registry.to_schemas();
        let mut last_repeat = String::new();
        let mut repeats = 0u32;
        let mut usage = Usage::default();
        let mut stop_reason = "max_iterations".to_string();
        let mut final_text = String::new();
        let mut turns = 0u32;

        for iteration in 0..opts.max_iterations {
            turns = iteration + 1;
            let elapsed = self.started.elapsed().as_secs();
            if self.config.budget.max_task_seconds > 0
                && elapsed > self.config.budget.max_task_seconds
            {
                return Err(Error::new(
                    ErrorCode::Budget,
                    "time budget for this task is exhausted",
                )
                .with_hint("raise budget.max_task_seconds or continue in a new turn"));
            }
            if self.config.budget.max_tokens > 0
                && self.usage.total() > self.config.budget.max_tokens
            {
                return Err(Error::new(
                    ErrorCode::Budget,
                    "token budget for this task is exhausted",
                )
                .with_hint("raise budget.max_tokens or use a smaller context"));
            }
            let chat = ChatRequest {
                model: self.model.clone(),
                system: system.clone(),
                messages: self.messages.clone(),
                tools: tools.clone(),
                temperature: None,
                max_tokens: None,
                json_mode: false,
            };
            let response = self.provider.chat(&chat)?;
            usage.prompt_tokens += response.usage.prompt_tokens;
            usage.completion_tokens += response.usage.completion_tokens;
            self.usage.prompt_tokens += response.usage.prompt_tokens;
            self.usage.completion_tokens += response.usage.completion_tokens;

            if response.tool_calls.is_empty() {
                final_text = response.text.clone();
                stop_reason = if response.stop_reason.is_empty() {
                    "end_turn".into()
                } else {
                    response.stop_reason.clone()
                };
                self.messages
                    .push(Message::assistant(response.text.clone()));
                break;
            }

            self.messages.push(Message {
                role: crate::providers::Role::Assistant,
                parts: if response.text.is_empty() {
                    Vec::new()
                } else {
                    vec![crate::providers::Part::Text {
                        text: response.text.clone(),
                    }]
                },
                tool_calls: response.tool_calls.clone(),
                tool_call_id: None,
            });

            for call in &response.tool_calls {
                let signature = format!("{}:{}", call.name, call.arguments);
                if signature == last_repeat {
                    repeats += 1;
                } else {
                    repeats = 0;
                    last_repeat = signature.clone();
                }
                let mut ctx = ToolCtx {
                    project,
                    store,
                    config: &self.config,
                    jobs,
                    history,
                    dry_run: opts.dry_run,
                };
                // The embedded agent obeys the same permission modes as an MCP session
                // (default `edit`: no cloud spend or plugin administration).
                let mode = crate::mcp::compat::Mode::from_env();
                let outcome = if mode.allows(crate::mcp::compat::level_of(&call.name)) {
                    self.registry
                        .call(&call.name, &mut ctx, call.arguments.clone())
                } else {
                    Err(crate::error::Error::new(
                        crate::error::ErrorCode::PermissionDenied,
                        format!(
                            "{} needs a higher permission level than this session allows ({mode:?})",
                            call.name
                        ),
                    )
                    .with_hint("start CINVAR with CINVAR_MCP_PERMISSIONS=full if you intend to allow it"))
                };
                let (payload, trace) = match outcome {
                    Ok(value) => {
                        let body = serde_json::to_string(&value).unwrap_or_default();
                        let clipped = if body.chars().count() > opts.max_tool_result_chars {
                            format!(
                                "{}... [truncated]",
                                text::truncate(&body, opts.max_tool_result_chars)
                            )
                        } else {
                            body
                        };
                        (
                            clipped,
                            ToolTrace {
                                name: call.name.clone(),
                                arguments: call.arguments.clone(),
                                ok: true,
                                result: value,
                                error: None,
                            },
                        )
                    }
                    Err(err) => {
                        let redacted = crate::secrets::redact(&err.message);
                        (
                            serde_json::json!({"error": {"code": err.code.as_str(), "message": redacted, "hint": err.hint}}).to_string(),
                            ToolTrace { name: call.name.clone(), arguments: call.arguments.clone(), ok: false, result: serde_json::Value::Null, error: Some(redacted) },
                        )
                    }
                };
                traces.push(trace);
                self.messages
                    .push(Message::tool_result(call.id.clone(), payload));
            }

            if repeats >= 2 {
                self.messages.push(Message::user(
                    "You repeated the same tool call three times. Inspect the last result, change your approach, or finish and report the outcome.",
                ));
            }
            if iteration + 1 == opts.max_iterations {
                self.messages.push(Message::user(
                    "The iteration limit for this turn is reached. Summarise what you actually did and what remains.",
                ));
                let last = ChatRequest {
                    model: self.model.clone(),
                    system: system.clone(),
                    messages: self.messages.clone(),
                    tools: Vec::new(),
                    temperature: None,
                    max_tokens: None,
                    json_mode: false,
                };
                if let Ok(response) = self.provider.chat(&last) {
                    usage.prompt_tokens += response.usage.prompt_tokens;
                    usage.completion_tokens += response.usage.completion_tokens;
                    final_text = response.text.clone();
                    self.messages.push(Message::assistant(response.text));
                }
                stop_reason = "max_iterations".into();
                break;
            }
        }

        Ok(TurnOutcome {
            text: final_text,
            iterations: turns,
            tool_calls: traces,
            usage,
            stop_reason,
        })
    }

    pub fn transcript_json(&self) -> serde_json::Value {
        serde_json::json!({
            "profile": self.profile_name,
            "model": self.model,
            "usage": self.usage,
            "messages": self.messages,
        })
    }

    pub fn save_transcript(&self, store: &ProjectStore) -> Result<std::path::PathBuf> {
        let path = store.sessions_dir().join(format!(
            "session-{}.json",
            crate::util::clock::now_epoch_secs()
        ));
        let body = serde_json::to_string_pretty(&self.transcript_json())?;
        crate::util::paths::write_atomic(&path, body.as_bytes())?;
        Ok(path)
    }
}
