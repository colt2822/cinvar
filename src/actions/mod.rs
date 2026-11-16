//! Structured editing actions: the versioned, validated command format a future editing model
//! would emit, plus the local journal and dataset-record types that produce training data.
//!
//! CINVAR already routes every edit through the tool registry (`tools::Registry`): each tool has
//! a name and a JSON-schema input, and invalid edits are refused. An *editing action* is exactly
//! one such call, so the action format is the registry call, not a second language. A model that
//! emits `[{tool, args}]` is validated by [`validate_calls`] before anything touches a project.
//!
//! Privacy: the journal is OFF by default (`actions.record = false`), local to the project, and
//! stores no media. Absolute paths in arguments are reduced to their file name. Nothing here
//! uploads anything. Datasets built from journals need the rights/consent fields in
//! [`Provenance`] filled in truthfully; see `docs/EDITING_ACTIONS.md`.

pub mod bench;
pub mod replay;

use std::io::Write;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::error::Result;
use crate::tools::{Registry, ToolCtx};

/// Version of the action *record* format written to the journal.
pub const ACTION_SCHEMA: &str = "cinvar.action/1";
/// Version of the dataset *example* format.
pub const DATASET_SCHEMA: &str = "cinvar.dataset/1";

/// One tool call as requested by a human UI, an agent, or a model.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolCall {
    pub tool: String,
    #[serde(default)]
    pub args: Value,
}

/// One journal line: a call plus what happened to the project.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ActionRecord {
    pub schema: String,
    pub ts: String,
    pub project_id: String,
    pub tool: String,
    pub args: Value,
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error_code: Option<String>,
    pub revision_before: u64,
    pub revision_after: u64,
}

/// Replaces absolute paths with their file name so a journal never leaks directory layout.
pub fn redact_args(value: &Value) -> Value {
    match value {
        Value::String(s)
            if s.starts_with('/') || s.chars().nth(1) == Some(':') && s.contains('\\') =>
        {
            let name = s.rsplit(['/', '\\']).next().unwrap_or("");
            Value::String(format!("<path>/{name}"))
        }
        Value::Array(a) => Value::Array(a.iter().map(redact_args).collect()),
        Value::Object(o) => {
            Value::Object(o.iter().map(|(k, v)| (k.clone(), redact_args(v))).collect())
        }
        other => other.clone(),
    }
}

/// Appends a record for a finished tool call. Best effort: a journal failure never fails the
/// edit. Only called when `config.actions.record` is true.
pub fn record(
    ctx: &ToolCtx,
    tool: &str,
    args: &Value,
    revision_before: u64,
    result: &Result<Value>,
) {
    if ctx.dry_run {
        return;
    }
    let rec = ActionRecord {
        schema: ACTION_SCHEMA.into(),
        ts: crate::util::clock::now_rfc3339(),
        project_id: ctx.project.id.clone(),
        tool: tool.into(),
        args: redact_args(args),
        ok: result.is_ok(),
        error_code: result.as_ref().err().map(|e| e.code.as_str().to_string()),
        revision_before,
        revision_after: ctx.project.revision,
    };
    let dir = ctx.store.actions_dir();
    if std::fs::create_dir_all(&dir).is_err() {
        return;
    }
    if let Ok(mut line) = serde_json::to_string(&rec) {
        line.push('\n');
        if let Ok(mut f) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(dir.join("actions.jsonl"))
        {
            let _ = f.write_all(line.as_bytes());
        }
    }
}

/// Where an example came from. `rights` must be stated; unknown provenance is not trainable.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Provenance {
    /// "synthetic" | "licensed" | "owner_created" | "customer_opt_in"
    pub source: String,
    /// Free-text licence or consent reference (e.g. a licence id), never personal data.
    pub rights: String,
    /// True only if the rights holder agreed to use as training data.
    pub training_permitted: bool,
}

/// One supervised example for a future editing model. Field names follow the product brief.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DatasetExample {
    pub schema: String,
    pub instruction: String,
    /// Project state the model sees before acting (timeline summary, settings).
    pub project_context: Value,
    /// Per-asset facts (duration, size, fps, audio) — metadata only, never media bytes.
    pub media_metadata: Vec<Value>,
    /// Short natural-language plan, one step per line.
    pub editing_plan: Vec<String>,
    /// The validated calls that realise the plan.
    pub tool_calls: Vec<ToolCall>,
    /// Timeline summary after replay (what a correct result looks like).
    pub expected_timeline: Value,
    /// Output of replaying `tool_calls` and checking the result.
    pub validation_result: ValidationResult,
    pub provenance: Provenance,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ValidationResult {
    pub ok: bool,
    pub errors: Vec<String>,
}

fn json_type_ok(expected: &str, v: &Value) -> bool {
    match expected {
        "string" => v.is_string(),
        "number" | "integer" => v.is_number(),
        "boolean" => v.is_boolean(),
        "array" => v.is_array(),
        "object" => v.is_object(),
        _ => true,
    }
}

/// Checks calls against the registry's own schemas without running anything: the tool exists,
/// required arguments are present, no unknown arguments, top-level types match.
pub fn validate_calls(registry: &Registry, calls: &[ToolCall]) -> ValidationResult {
    let mut errors = Vec::new();
    for (i, call) in calls.iter().enumerate() {
        let Some(spec) = registry.get(&call.tool) else {
            errors.push(format!("call {i}: unknown tool '{}'", call.tool));
            continue;
        };
        let args = match &call.args {
            Value::Null => json!({}),
            v => v.clone(),
        };
        let Some(obj) = args.as_object() else {
            errors.push(format!("call {i} ({}): args must be an object", call.tool));
            continue;
        };
        let props = spec.input.get("properties").and_then(|p| p.as_object());
        if let Some(req) = spec.input.get("required").and_then(|r| r.as_array()) {
            for r in req.iter().filter_map(|r| r.as_str()) {
                if !obj.contains_key(r) {
                    errors.push(format!(
                        "call {i} ({}): missing required argument '{r}'",
                        call.tool
                    ));
                }
            }
        }
        for (k, v) in obj {
            match props.and_then(|p| p.get(k)) {
                None => errors.push(format!("call {i} ({}): unknown argument '{k}'", call.tool)),
                Some(def) => {
                    if let Some(t) = def.get("type").and_then(|t| t.as_str()) {
                        if !v.is_null() && !json_type_ok(t, v) {
                            errors.push(format!(
                                "call {i} ({}): argument '{k}' must be {t}",
                                call.tool
                            ));
                        }
                    }
                }
            }
        }
    }
    ValidationResult {
        ok: errors.is_empty(),
        errors,
    }
}

impl DatasetExample {
    /// Structural checks plus call validation. A record is trainable only if this is ok.
    pub fn validate(&self, registry: &Registry) -> ValidationResult {
        let mut v = validate_calls(registry, &self.tool_calls);
        if self.schema != DATASET_SCHEMA {
            v.errors.push(format!("schema must be {DATASET_SCHEMA}"));
        }
        if self.instruction.trim().is_empty() {
            v.errors.push("instruction is empty".into());
        }
        if self.tool_calls.is_empty() {
            v.errors.push("tool_calls is empty".into());
        }
        if self.provenance.rights.trim().is_empty() {
            v.errors.push("provenance.rights must be stated".into());
        }
        if !matches!(
            self.provenance.source.as_str(),
            "synthetic" | "licensed" | "owner_created" | "customer_opt_in"
        ) {
            v.errors
                .push("provenance.source is not a known source".into());
        }
        if !self.provenance.training_permitted {
            v.errors
                .push("provenance.training_permitted is false".into());
        }
        v.ok = v.errors.is_empty();
        v
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn call(tool: &str, args: Value) -> ToolCall {
        ToolCall {
            tool: tool.into(),
            args,
        }
    }

    #[test]
    fn validates_real_registry_calls() {
        let reg = crate::tools::build_default();
        let ok = validate_calls(
            &reg,
            &[call(
                "cinvar.timeline.trim",
                json!({"clip_id": "c1", "source_out": "2.0", "ripple": true}),
            )],
        );
        assert!(ok.ok, "{:?}", ok.errors);
        let bad = validate_calls(
            &reg,
            &[
                call("cinvar.timeline.trim", json!({"source_out": "2.0"})),
                call("cinvar.timeline.nope", json!({})),
                call(
                    "cinvar.timeline.trim",
                    json!({"clip_id": "c1", "ripple": "yes"}),
                ),
                call("cinvar.timeline.trim", json!({"clip_id": "c1", "bogus": 1})),
            ],
        );
        assert!(!bad.ok);
        assert_eq!(bad.errors.len(), 4, "{:?}", bad.errors);
    }

    #[test]
    fn redaction_keeps_names_not_directories() {
        let v = redact_args(
            &json!({"path": "/home/a/secret/clip.mp4", "n": 3, "list": ["/x/y.wav", "plain"]}),
        );
        assert_eq!(
            v,
            json!({"path": "<path>/clip.mp4", "n": 3, "list": ["<path>/y.wav", "plain"]})
        );
    }

    fn example(source: &str, permitted: bool) -> DatasetExample {
        DatasetExample {
            schema: DATASET_SCHEMA.into(),
            instruction: "Trim the first clip to two seconds".into(),
            project_context: json!({"timeline": {"clips": 1}}),
            media_metadata: vec![
                json!({"asset": "a1", "duration": 5.0, "width": 1280, "height": 720, "fps": 24}),
            ],
            editing_plan: vec!["trim clip c1 to 2s".into()],
            tool_calls: vec![call(
                "cinvar.timeline.trim",
                json!({"clip_id": "c1", "source_out": "2.0"}),
            )],
            expected_timeline: json!({"clips": [{"id": "c1", "duration": 2.0}]}),
            validation_result: ValidationResult {
                ok: true,
                errors: vec![],
            },
            provenance: Provenance {
                source: source.into(),
                rights: "generated by CINVAR test fixtures".into(),
                training_permitted: permitted,
            },
        }
    }

    #[test]
    fn dataset_examples_need_rights_and_valid_calls() {
        let reg = crate::tools::build_default();
        assert!(example("synthetic", true).validate(&reg).ok);
        assert!(
            !example("synthetic", false).validate(&reg).ok,
            "no training permission"
        );
        assert!(
            !example("scraped", true).validate(&reg).ok,
            "unknown source"
        );
        let mut e = example("synthetic", true);
        e.provenance.rights.clear();
        assert!(!e.validate(&reg).ok);
        // round-trips as JSON with exactly the brief's field names
        let v = serde_json::to_value(example("synthetic", true)).unwrap();
        for k in [
            "instruction",
            "project_context",
            "media_metadata",
            "editing_plan",
            "tool_calls",
            "expected_timeline",
            "validation_result",
        ] {
            assert!(v.get(k).is_some(), "{k}");
        }
    }
}
