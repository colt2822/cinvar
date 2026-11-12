pub mod compat;

use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;

use serde_json::{json, Value};

use crate::config::Config;
use crate::error::{Error, ErrorCode, Result};
use crate::jobs::Jobs;
use crate::project::history::History;
use crate::project::store::ProjectStore;
use crate::tools::{Registry, ToolCtx};
use crate::util::paths;

pub const PROTOCOL_VERSION: &str = "2024-11-05";

pub const PARSE_ERROR: i64 = -32700;
pub const INVALID_REQUEST: i64 = -32600;
pub const METHOD_NOT_FOUND: i64 = -32601;
pub const INVALID_PARAMS: i64 = -32602;
pub const INTERNAL_ERROR: i64 = -32603;

pub const STATUS_URI: &str = "cinvar://project/status";
pub const PROJECT_URI: &str = "cinvar://project/json";

pub struct McpServer {
    pub registry: Registry,
    pub project_dir: Option<PathBuf>,
    /// Advertise `cinvar_render_export` instead of `cinvar.render.export` (CINVAR_MCP_NAMES=safe).
    pub safe_names: bool,
    /// What this session may do (CINVAR_MCP_PERMISSIONS=read_only|edit|full).
    pub mode: compat::Mode,
}

impl McpServer {
    pub fn new(registry: Registry, project_dir: Option<PathBuf>) -> McpServer {
        McpServer {
            registry,
            project_dir,
            safe_names: std::env::var("CINVAR_MCP_NAMES")
                .map(|v| v.eq_ignore_ascii_case("safe"))
                .unwrap_or(false),
            mode: compat::Mode::from_env(),
        }
    }

    pub fn serve_stdio(&mut self) -> Result<()> {
        let stdin = std::io::stdin();
        let mut reader = BufReader::new(stdin.lock());
        let stdout = std::io::stdout();
        let mut writer = stdout.lock();
        // One Jobs instance for the whole session so render handles stay pollable.
        let jobs = Jobs::new();
        let mut history = History::default();
        while let Some(raw) = read_message(&mut reader)? {
            let Some(reply) = self.handle(&raw, &jobs, &mut history) else {
                continue;
            };
            let line = serde_json::to_string(&reply)?;
            writeln!(writer, "{line}")
                .and_then(|_| writer.flush())
                .map_err(|e| {
                    Error::new(ErrorCode::Io, format!("mcp: writing to stdout failed: {e}"))
                })?;
        }
        Ok(())
    }

    fn handle(&self, raw: &[u8], jobs: &Jobs, history: &mut History) -> Option<Value> {
        let parsed: Value = match serde_json::from_slice(raw) {
            Ok(value) => value,
            Err(err) => {
                return Some(error_reply(
                    Value::Null,
                    PARSE_ERROR,
                    &format!("parse error: {err}"),
                ))
            }
        };
        let Some(object) = parsed.as_object() else {
            return Some(error_reply(
                Value::Null,
                INVALID_REQUEST,
                "request must be a JSON object",
            ));
        };
        let id = object.get("id").cloned();
        let Some(method) = object
            .get("method")
            .and_then(|m| m.as_str())
            .map(|m| m.to_string())
        else {
            let id = id?;
            return Some(error_reply(id, INVALID_REQUEST, "missing 'method'"));
        };
        let params = object.get("params").cloned().unwrap_or(Value::Null);

        if method.starts_with("notifications/") || method == "$/cancelRequest" {
            return None;
        }

        let outcome: std::result::Result<Value, (i64, String)> = match method.as_str() {
            "initialize" => Ok(initialize_result()),
            "ping" => Ok(json!({})),
            "tools/list" => Ok(json!({
                "tools": self
                    .registry
                    .specs()
                    .iter()
                    .filter(|spec| self.mode.allows(compat::level_of(&spec.name)))
                    .map(|spec| json!({
                        "name": if self.safe_names { compat::safe_name(&spec.name) } else { spec.name.clone() },
                        "description": spec.description,
                        "inputSchema": spec.input,
                    }))
                    .collect::<Vec<_>>(),
            })),
            "tools/call" => self.tool_call(&params, jobs, history),
            "resources/list" => Ok(json!({ "resources": self.resource_list() })),
            "resources/read" => self.resource_read(&params, jobs, history),
            other => Err((METHOD_NOT_FOUND, format!("unknown method '{other}'"))),
        };

        let id = id?; // a notification never gets a reply
        Some(match outcome {
            Ok(result) => json!({"jsonrpc": "2.0", "id": id, "result": result}),
            Err((code, message)) => error_reply(id, code, &message),
        })
    }

    fn tool_call(
        &self,
        params: &Value,
        jobs: &Jobs,
        history: &mut History,
    ) -> std::result::Result<Value, (i64, String)> {
        let Some(name) = params
            .get("name")
            .and_then(|v| v.as_str())
            .map(|v| v.to_string())
        else {
            return Err((INVALID_PARAMS, "tools/call requires a 'name'".to_string()));
        };
        let Some(name) = compat::resolve(&self.registry, &name) else {
            return Err((INVALID_PARAMS, format!("unknown tool '{name}'")));
        };
        let level = compat::level_of(&name);
        if !self.mode.allows(level) {
            return Ok(content(
                &json!({"error": {
                    "code": "permission_denied",
                    "message": format!("{name} needs {level:?} permission; this MCP session allows {:?} only", self.mode),
                    "hint": "restart the server with CINVAR_MCP_PERMISSIONS=full if you intend to allow it"
                }}),
                true,
            ));
        }
        let args = params.get("arguments").cloned().unwrap_or(Value::Null);
        let started = std::time::Instant::now();
        let outcome = self.call_tool(jobs, history, &name, args);
        self.log_call(&name, started.elapsed(), outcome.as_ref().err());
        Ok(match outcome {
            Ok(value) => content(&value, false),
            Err(err) => content(&error_payload(&err), true),
        })
    }

    fn resource_list(&self) -> Vec<Value> {
        let mut out = vec![json!({
            "uri": STATUS_URI,
            "name": "project status",
            "description": "Assets, timelines, tracks, captions and validation issues of the open project",
            "mimeType": "application/json",
        })];
        if self.open_project().is_ok() {
            out.push(json!({
                "uri": PROJECT_URI,
                "name": "project.json",
                "description": "The raw project.json document",
                "mimeType": "application/json",
            }));
        }
        out
    }

    fn resource_read(
        &self,
        params: &Value,
        jobs: &Jobs,
        history: &mut History,
    ) -> std::result::Result<Value, (i64, String)> {
        let Some(uri) = params
            .get("uri")
            .and_then(|v| v.as_str())
            .map(|v| v.to_string())
        else {
            return Err((
                INVALID_PARAMS,
                "resources/read requires a 'uri'".to_string(),
            ));
        };
        match uri.as_str() {
            STATUS_URI => {
                let value = self
                    .call_tool(jobs, history, "cinvar.project.status", json!({}))
                    .map_err(|err| (INTERNAL_ERROR, error_message(&err)))?;
                Ok(json!({"contents": [{
                    "uri": uri,
                    "mimeType": "application/json",
                    "text": serde_json::to_string_pretty(&value).unwrap_or_default(),
                }]}))
            }
            PROJECT_URI => {
                let store = self
                    .open_project()
                    .map_err(|err| (INVALID_PARAMS, error_message(&err)))?;
                let text = std::fs::read_to_string(store.project_file()).map_err(|err| {
                    (
                        INTERNAL_ERROR,
                        format!("could not read project.json: {err}"),
                    )
                })?;
                Ok(json!({"contents": [{
                    "uri": uri,
                    "mimeType": "application/json",
                    "text": crate::secrets::redact(&text),
                }]}))
            }
            other => Err((INVALID_PARAMS, format!("unknown resource uri '{other}'"))),
        }
    }

    /// Reopen the store and the project for every call so edits made by another
    /// process between calls are never overwritten by a stale copy.
    fn open_project(&self) -> Result<ProjectStore> {
        let dir = self.project_dir.as_ref().ok_or_else(|| {
            Error::not_found("this MCP server has no project directory")
                .with_hint("pass --project <dir> when starting cinvar mcp serve")
        })?;
        let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        ProjectStore::open(&paths::absolutize(dir, &cwd))
    }

    /// Append one line per tool call to `<project>/sessions/mcp-calls.jsonl`. This is the evidence
    /// for how a model actually used the tools (call counts, errors, slow calls). Arguments are
    /// not logged, so it never holds secrets or file contents.
    fn log_call(&self, name: &str, elapsed: std::time::Duration, err: Option<&Error>) {
        let Ok(store) = self.open_project() else {
            return;
        };
        let line = json!({
            "at": crate::util::clock::now_rfc3339(),
            "tool": name,
            "ok": err.is_none(),
            "ms": elapsed.as_millis() as u64,
            "error": err.map(|e| e.code.as_str()),
        });
        let dir = store.sessions_dir();
        if std::fs::create_dir_all(&dir).is_ok() {
            use std::io::Write;
            if let Ok(mut file) = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(dir.join("mcp-calls.jsonl"))
            {
                let _ = writeln!(file, "{line}");
            }
        }
    }

    fn call_tool(
        &self,
        jobs: &Jobs,
        history: &mut History,
        name: &str,
        args: Value,
    ) -> Result<Value> {
        let store = self.open_project()?;
        // Serialise load -> edit -> save across processes (another MCP session or the CLI).
        let _lock = store.lock(std::time::Duration::from_secs(20))?;
        let mut project = store.load()?;
        let config = Config::load().unwrap_or_default();
        let mut ctx = ToolCtx {
            project: &mut project,
            store: &store,
            config: &config,
            jobs,
            history,
            dry_run: false,
        };
        self.registry.call(name, &mut ctx, args)
    }
}

const INSTRUCTIONS: &str = "\
CINVAR is a video editing studio you operate through tools. The project is already open; do not create one.

Workflow (follow it, do not guess):
1. cinvar.project.status - see what exists. cinvar.skills.list then cinvar.skills.load for the style you are making (cinematic, short-form, youtube, commercial, motion-graphics, podcast): the skills hold editing judgement, load one before cutting.
2. Look before you cut: cinvar.media.import / import_folder, cinvar.media.inspect, cinvar.media.contact_sheet and cinvar.media.scenes on footage, cinvar.audio.analyze on sound. Open contact sheets and frames as images - never judge footage you have not seen.
3. Build: cinvar.timeline.create, append_clip / insert_clip, trim (ripple:true), transition, text, transform (reframe, picture-in-picture), animate (push_in, pan, fades, slides) or keyframe, clip_audio (gain, fades, duck_under_speech on music), captions (import, from_script, transcribe if an engine exists).
4. Check: cinvar.render.preview (fast), then cinvar.media.frames on the render and OPEN the frames; check titles are readable and do not collide with picture text, motion is visible, audio levels are sane. cinvar.qa.render_check validates the file but says nothing about taste.
5. Export: cinvar.render.export (writes inside the project's renders folder), poll cinvar.render.status.
6. Revise: edit the existing timeline, never rebuild. cinvar.project.history lists revisions and cinvar.project.rollback restores one. Tell the user which revision is the safe return point.

Rules: motion and effects must serve the story (see the decision-framework skill). Tools refuse unsupported requests with an explanation instead of approximating - read errors and follow their hints. Times accept seconds (12.5) or MM:SS.";

fn initialize_result() -> Value {
    json!({
        "protocolVersion": PROTOCOL_VERSION,
        "capabilities": {
            "tools": {"listChanged": false},
            "resources": {"subscribe": false},
        },
        "serverInfo": {"name": "cinvar", "version": crate::VERSION},
        "instructions": INSTRUCTIONS,
    })
}

fn content(value: &Value, is_error: bool) -> Value {
    json!({
        "content": [{
            "type": "text",
            "text": serde_json::to_string_pretty(value).unwrap_or_else(|_| "null".to_string()),
        }],
        "isError": is_error,
    })
}

fn error_payload(err: &Error) -> Value {
    json!({"error": {
        "code": err.code.as_str(),
        "message": crate::secrets::redact(&err.message),
        "hint": err.hint.as_deref().map(crate::secrets::redact),
    }})
}

fn error_message(err: &Error) -> String {
    let text = match &err.hint {
        Some(hint) => format!("{} (hint: {})", err.message, hint),
        None => err.message.clone(),
    };
    crate::secrets::redact(&text)
}

fn error_reply(id: Value, code: i64, message: &str) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": {"code": code, "message": crate::secrets::redact(message)},
    })
}

/// Read one message from stdin: either a newline-delimited JSON object or a
/// Content-Length framed body. Returns None at EOF.
fn read_message<R: BufRead>(reader: &mut R) -> Result<Option<Vec<u8>>> {
    loop {
        let mut line = Vec::new();
        if read_line(reader, &mut line)? == 0 {
            return Ok(None);
        }
        let trimmed = trim(&line);
        if trimmed.is_empty() {
            continue;
        }
        if let Some(mut length) = content_length(trimmed) {
            loop {
                let mut header = Vec::new();
                if read_line(reader, &mut header)? == 0 {
                    return Ok(None);
                }
                let header = trim(&header);
                if header.is_empty() {
                    break;
                }
                if let Some(inner) = content_length(header) {
                    length = inner;
                }
            }
            let mut body = vec![0u8; length];
            match reader.read_exact(&mut body) {
                Ok(()) => return Ok(Some(body)),
                Err(err) if err.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(None),
                Err(err) => {
                    return Err(Error::io(format!("mcp: reading from stdin failed: {err}")))
                }
            }
        }
        return Ok(Some(trimmed.to_vec()));
    }
}

fn read_line<R: BufRead>(reader: &mut R, buf: &mut Vec<u8>) -> Result<usize> {
    reader
        .read_until(b'\n', buf)
        .map_err(|err| Error::io(format!("mcp: reading from stdin failed: {err}")))
}

fn trim(bytes: &[u8]) -> &[u8] {
    let mut start = 0;
    let mut end = bytes.len();
    while start < end && (bytes[start] as char).is_ascii_whitespace() {
        start += 1;
    }
    while end > start && (bytes[end - 1] as char).is_ascii_whitespace() {
        end -= 1;
    }
    &bytes[start..end]
}

fn content_length(header: &[u8]) -> Option<usize> {
    let text = std::str::from_utf8(header).ok()?;
    let (key, value) = text.split_once(':')?;
    if !key.trim().eq_ignore_ascii_case("content-length") {
        return None;
    }
    value.trim().parse::<usize>().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn server() -> McpServer {
        McpServer::new(Registry::new(), None)
    }

    fn drive(server: &mut McpServer, line: &str) -> Option<Value> {
        let jobs = Jobs::new();
        let mut history = History::default();
        server.handle(line.as_bytes(), &jobs, &mut history)
    }

    #[test]
    fn initializes_with_protocol_version() {
        let reply = drive(
            &mut server(),
            r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}"#,
        )
        .unwrap();
        assert_eq!(reply["result"]["protocolVersion"], PROTOCOL_VERSION);
        assert_eq!(reply["result"]["serverInfo"]["name"], "cinvar");
        assert_eq!(reply["result"]["serverInfo"]["version"], crate::VERSION);
        assert!(reply["result"]["capabilities"]["tools"].is_object());
    }

    #[test]
    fn malformed_json_answers_parse_error_and_keeps_serving() {
        let mut server = server();
        let bad = drive(&mut server, "{not json").unwrap();
        assert_eq!(bad["error"]["code"], PARSE_ERROR);
        let ping = drive(&mut server, r#"{"jsonrpc":"2.0","id":2,"method":"ping"}"#).unwrap();
        assert_eq!(ping["id"], 2);
        assert!(ping["result"].is_object());
    }

    #[test]
    fn unknown_method_is_method_not_found() {
        let reply = drive(
            &mut server(),
            r#"{"jsonrpc":"2.0","id":3,"method":"does/not/exist"}"#,
        )
        .unwrap();
        assert_eq!(reply["error"]["code"], METHOD_NOT_FOUND);
    }

    #[test]
    fn unknown_tool_is_invalid_params() {
        let reply = drive(
            &mut server(),
            r#"{"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"nope","arguments":{}}}"#,
        )
        .unwrap();
        assert_eq!(reply["error"]["code"], INVALID_PARAMS);
    }

    #[test]
    fn notifications_are_silent() {
        let mut server = server();
        assert!(drive(
            &mut server,
            r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#
        )
        .is_none());
        assert!(drive(
            &mut server,
            r#"{"jsonrpc":"2.0","method":"$/cancelRequest","params":{"id":1}}"#
        )
        .is_none());
    }

    #[test]
    fn empty_lines_are_skipped_and_content_length_is_accepted() {
        let body = r#"{"jsonrpc":"2.0","id":9,"method":"ping"}"#;
        let framed = format!("Content-Length: {}\r\n\r\n{body}", body.len());
        let mut reader = std::io::Cursor::new(format!("\n\n{framed}").into_bytes());
        let first = read_message(&mut reader).unwrap().unwrap();
        assert_eq!(std::str::from_utf8(&first).unwrap(), body);
        assert!(read_message(&mut reader).unwrap().is_none());
    }

    #[test]
    fn call_without_project_reports_is_error() {
        let mut registry = Registry::new();
        registry.add(
            crate::tools::ToolSpec {
                name: "cinvar.test.noop".into(),
                group: "test".into(),
                description: "noop".into(),
                input: json!({"type": "object"}),
                destructive: false,
            },
            |_ctx, _args| Ok(json!({"ok": true})),
        );
        let mut server = McpServer::new(registry, None);
        let reply = drive(
            &mut server,
            r#"{"jsonrpc":"2.0","id":5,"method":"tools/call","params":{"name":"cinvar.test.noop","arguments":{}}}"#,
        )
        .unwrap();
        assert_eq!(reply["result"]["isError"], true);
        let text = reply["result"]["content"][0]["text"].as_str().unwrap();
        assert!(text.contains("--project"), "{text}");
    }
}
