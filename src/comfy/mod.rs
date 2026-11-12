//! ComfyUI as an OPTIONAL workflow backend, over its HTTP API (`/prompt`, `/history/{id}`,
//! `/view`, `/object_info`, `/system_stats`, `/interrupt`, `/upload/image`).
//!
//! No model family is hard-coded: a *capability registry* of user-supplied workflow descriptors
//! (`<config>/comfy-workflows/*.json`) advertises tasks (upscale, remove_background, broll, ...)
//! and maps CINVAR parameters onto API-format workflow nodes. Before submitting, the workflow is
//! checked against the server's `/object_info` so a missing node type or uninstalled model
//! fails with a clear message instead of a half-run job.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};

use crate::error::{Error, ErrorCode, Result};
use crate::util::paths;

pub const DEFAULT_URL: &str = "http://127.0.0.1:8188";

#[derive(Clone)]
pub struct ComfyClient {
    base: String,
    agent: ureq::Agent,
}

fn io_err(what: &str, e: impl std::fmt::Display) -> Error {
    Error::new(ErrorCode::Provider, format!("ComfyUI {what}: {e}"))
}

impl ComfyClient {
    pub fn new(base: &str) -> ComfyClient {
        ComfyClient {
            base: base.trim_end_matches('/').to_string(),
            agent: ureq::AgentBuilder::new()
                .timeout_connect(Duration::from_secs(3))
                .timeout(Duration::from_secs(60))
                .build(),
        }
    }

    /// True when the server is on this machine (loopback host).
    pub fn is_local(&self) -> bool {
        let rest = self.base.split("://").nth(1).unwrap_or("");
        let host = rest.split('/').next().unwrap_or("");
        let host = host.rsplit_once(':').map(|(h, _)| h).unwrap_or(host);
        matches!(host, "127.0.0.1" | "localhost" | "[::1]")
    }

    pub fn from_env() -> Result<ComfyClient> {
        let url = std::env::var("CINVAR_COMFYUI_URL").unwrap_or_else(|_| DEFAULT_URL.into());
        if !(url.starts_with("http://") || url.starts_with("https://")) {
            return Err(Error::invalid("CINVAR_COMFYUI_URL must be an http(s) URL"));
        }
        Ok(ComfyClient::new(&url))
    }

    fn get_json(&self, path: &str) -> Result<Value> {
        self.agent
            .get(&format!("{}{path}", self.base))
            .call()
            .map_err(|e| io_err(path, e))?
            .into_json()
            .map_err(|e| io_err(path, e))
    }

    pub fn system_stats(&self) -> Result<Value> {
        self.get_json("/system_stats")
    }

    pub fn object_info(&self) -> Result<Value> {
        self.get_json("/object_info")
    }

    /// Queue an API-format workflow; returns the prompt id.
    pub fn queue(&self, workflow: &Value, client_id: &str) -> Result<String> {
        let resp = self
            .agent
            .post(&format!("{}/prompt", self.base))
            .send_json(json!({"prompt": workflow, "client_id": client_id}));
        match resp {
            Ok(r) => {
                let v: Value = r.into_json().map_err(|e| io_err("/prompt", e))?;
                let id = v["prompt_id"]
                    .as_str()
                    .map(String::from)
                    .ok_or_else(|| io_err("/prompt", "response had no prompt_id"))?;
                // The id is later used in local file names: never trust a server to choose a path.
                if id.is_empty()
                    || id.len() > 64
                    || !id
                        .chars()
                        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
                {
                    return Err(io_err("/prompt", "response carried an unsafe prompt_id"));
                }
                Ok(id)
            }
            Err(ureq::Error::Status(400, r)) => {
                let body = r.into_string().unwrap_or_default();
                Err(Error::new(
                    ErrorCode::InvalidInput,
                    format!(
                        "ComfyUI rejected the workflow: {}",
                        body.chars().take(400).collect::<String>()
                    ),
                ))
            }
            Err(e) => Err(io_err("/prompt", e)),
        }
    }

    pub fn history(&self, prompt_id: &str) -> Result<Value> {
        self.get_json(&format!("/history/{}", urlenc(prompt_id)))
    }

    pub fn interrupt(&self) -> Result<()> {
        self.agent
            .post(&format!("{}/interrupt", self.base))
            .send_bytes(b"")
            .map(|_| ())
            .map_err(|e| io_err("/interrupt", e))
    }

    /// Download one output file (`/view`).
    pub fn view(&self, file: &OutputFile, dest: &Path) -> Result<u64> {
        let url = format!(
            "{}/view?filename={}&subfolder={}&type={}",
            self.base,
            urlenc(&file.filename),
            urlenc(&file.subfolder),
            urlenc(&file.kind)
        );
        let resp = self
            .agent
            .get(&url)
            .call()
            .map_err(|e| io_err("/view", e))?;
        let mut reader = std::io::Read::take(resp.into_reader(), 8 * 1024 * 1024 * 1024);
        if let Some(p) = dest.parent() {
            paths::ensure_dir(p)?;
        }
        let tmp = dest.with_extension("part");
        let mut f = std::fs::File::create(&tmp)?;
        let n = std::io::copy(&mut reader, &mut f)?;
        f.sync_all()?;
        std::fs::rename(&tmp, dest)?;
        Ok(n)
    }

    /// Upload an input image/video frame the workflow can reference (`LoadImage`).
    pub fn upload(&self, path: &Path, name: &str) -> Result<String> {
        let data = std::fs::read(path)?;
        let boundary = "----cinvarboundary7d1c";
        let mut body: Vec<u8> = Vec::new();
        body.extend(format!("--{boundary}\r\nContent-Disposition: form-data; name=\"image\"; filename=\"{name}\"\r\nContent-Type: application/octet-stream\r\n\r\n").as_bytes());
        body.extend(&data);
        body.extend(format!("\r\n--{boundary}\r\nContent-Disposition: form-data; name=\"overwrite\"\r\n\r\ntrue\r\n--{boundary}--\r\n").as_bytes());
        let v: Value = self
            .agent
            .post(&format!("{}/upload/image", self.base))
            .set(
                "Content-Type",
                &format!("multipart/form-data; boundary={boundary}"),
            )
            .send_bytes(&body)
            .map_err(|e| io_err("/upload/image", e))?
            .into_json()
            .map_err(|e| io_err("/upload/image", e))?;
        Ok(v["name"].as_str().unwrap_or(name).to_string())
    }
}

fn urlenc(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' => (b as char).to_string(),
            _ => format!("%{b:02X}"),
        })
        .collect()
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct OutputFile {
    pub filename: String,
    pub subfolder: String,
    pub kind: String,
}

/// Where a CINVAR parameter lands in the workflow.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Binding {
    pub node: String,
    pub input: String,
    #[serde(default)]
    pub description: String,
}

/// One entry in the capability registry.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkflowDef {
    pub name: String,
    /// What it does: upscale | remove_background | enhance | inpaint | transition | vfx | restore | broll | other
    pub task: String,
    #[serde(default)]
    pub description: String,
    pub workflow: Value,
    #[serde(default)]
    pub params: std::collections::BTreeMap<String, Binding>,
    /// Params that are input FILES (uploaded then referenced by name).
    #[serde(default)]
    pub file_params: Vec<String>,
}

pub const TASKS: &[&str] = &[
    "broll",
    "remove_background",
    "enhance",
    "inpaint",
    "transition",
    "vfx",
    "upscale",
    "restore",
    "other",
];

pub fn registry_dir() -> PathBuf {
    paths::config_dir().join("comfy-workflows")
}

pub fn load_registry(dir: &Path) -> Vec<WorkflowDef> {
    let mut out = Vec::new();
    let Ok(rd) = std::fs::read_dir(dir) else {
        return out;
    };
    let mut files: Vec<PathBuf> = rd.filter_map(|e| e.ok().map(|e| e.path())).collect();
    files.sort();
    for f in files
        .into_iter()
        .filter(|f| f.extension().and_then(|e| e.to_str()) == Some("json"))
    {
        if let Ok(text) = std::fs::read_to_string(&f) {
            if let Ok(def) = serde_json::from_str::<WorkflowDef>(&text) {
                if TASKS.contains(&def.task.as_str()) && !def.name.trim().is_empty() {
                    out.push(def);
                }
            }
        }
    }
    out
}

/// Fill parameters into a copy of the workflow. Unknown params and missing nodes are errors.
pub fn bind(def: &WorkflowDef, params: &Map<String, Value>) -> Result<Value> {
    let mut wf = def.workflow.clone();
    for (key, value) in params {
        let b = def.params.get(key).ok_or_else(|| {
            Error::invalid(format!(
                "workflow '{}' has no parameter '{key}' (known: {})",
                def.name,
                def.params.keys().cloned().collect::<Vec<_>>().join(", ")
            ))
        })?;
        let node = wf
            .get_mut(&b.node)
            .and_then(|n| n.get_mut("inputs"))
            .and_then(|i| i.as_object_mut())
            .ok_or_else(|| {
                Error::invalid(format!(
                    "workflow '{}': node {} (parameter '{key}') does not exist",
                    def.name, b.node
                ))
            })?;
        node.insert(b.input.clone(), value.clone());
    }
    Ok(wf)
}

/// Check node types and enumerated inputs (e.g. checkpoint names) against `/object_info`.
pub fn validate_against(info: &Value, wf: &Value) -> Result<()> {
    let nodes = wf
        .as_object()
        .ok_or_else(|| Error::invalid("workflow must be an API-format JSON object"))?;
    let mut problems = Vec::new();
    for (id, node) in nodes {
        let Some(class) = node["class_type"].as_str() else {
            problems.push(format!("node {id}: missing class_type"));
            continue;
        };
        let Some(spec) = info.get(class) else {
            problems.push(format!(
                "node {id}: node type '{class}' is not installed on the ComfyUI server"
            ));
            continue;
        };
        let Some(inputs) = node["inputs"].as_object() else {
            continue;
        };
        for (name, value) in inputs {
            let Some(text) = value.as_str() else { continue };
            for section in ["required", "optional"] {
                if let Some(choices) = spec["input"][section][name][0].as_array() {
                    if !choices.iter().any(|c| c.as_str() == Some(text)) {
                        problems.push(format!("node {id} ({class}): '{text}' is not available for '{name}' (installed models/options: {} choices)", choices.len()));
                    }
                }
            }
        }
    }
    if problems.is_empty() {
        Ok(())
    } else {
        Err(Error::new(ErrorCode::InvalidInput, problems.join("; ")))
    }
}

/// Outputs of a finished prompt: images / gifs / videos / audio lists across all nodes.
pub fn outputs_of(history: &Value, prompt_id: &str) -> Vec<OutputFile> {
    let mut out = Vec::new();
    let Some(outputs) = history[prompt_id]["outputs"].as_object() else {
        return out;
    };
    let mut ids: Vec<&String> = outputs.keys().collect();
    // Node ids are numeric strings: order 9 before 10.
    ids.sort_by_key(|k| (k.parse::<u64>().unwrap_or(u64::MAX), k.to_string()));
    for id in ids {
        for list in ["images", "gifs", "videos", "audio"] {
            if let Some(arr) = outputs[id][list].as_array() {
                for f in arr {
                    if let Some(name) = f["filename"].as_str() {
                        out.push(OutputFile {
                            filename: name.to_string(),
                            subfolder: f["subfolder"].as_str().unwrap_or("").to_string(),
                            kind: f["type"].as_str().unwrap_or("output").to_string(),
                        });
                    }
                }
            }
        }
    }
    out
}

#[derive(Debug)]
pub struct RunResult {
    pub prompt_id: String,
    pub files: Vec<PathBuf>,
}

/// Queue, poll to completion (cancellable, bounded), and download all outputs into `dest_dir`.
pub fn run(
    client: &ComfyClient,
    workflow: &Value,
    dest_dir: &Path,
    timeout: Duration,
    poll: Duration,
    cancelled: &dyn Fn() -> bool,
    progress: &dyn Fn(f32, &str),
) -> Result<RunResult> {
    progress(0.02, "validating workflow");
    validate_against(&client.object_info()?, workflow)?;
    let client_id = crate::util::ids::new_id("cinvar");
    let prompt_id = client.queue(workflow, &client_id)?;
    let started = Instant::now();
    loop {
        if cancelled() {
            let _ = client.interrupt();
            return Err(Error::cancelled("ComfyUI run cancelled"));
        }
        if started.elapsed() > timeout {
            let _ = client.interrupt();
            return Err(Error::new(
                ErrorCode::Provider,
                format!("ComfyUI run exceeded {}s", timeout.as_secs()),
            ));
        }
        let h = client.history(&prompt_id)?;
        if let Some(entry) = h.get(&prompt_id) {
            let status = entry["status"]["status_str"].as_str().unwrap_or("");
            if status == "error" {
                return Err(Error::new(
                    ErrorCode::Render,
                    format!(
                        "ComfyUI execution failed: {}",
                        entry["status"]["messages"]
                            .to_string()
                            .chars()
                            .take(400)
                            .collect::<String>()
                    ),
                ));
            }
            if entry["status"]["completed"].as_bool().unwrap_or(false) || status == "success" {
                let files = outputs_of(&h, &prompt_id);
                if files.is_empty() {
                    return Err(Error::new(
                        ErrorCode::Render,
                        "ComfyUI finished but produced no output files",
                    ));
                }
                let mut saved = Vec::new();
                for (i, f) in files.iter().enumerate() {
                    progress(
                        0.8 + 0.2 * (i as f32 / files.len() as f32),
                        "downloading outputs",
                    );
                    // File names come from the server: keep only the final component.
                    let name = Path::new(&f.filename)
                        .file_name()
                        .map(|n| n.to_string_lossy().to_string())
                        .unwrap_or_else(|| format!("output_{i}"));
                    let dest = dest_dir.join(format!("{prompt_id}_{name}"));
                    client.view(f, &dest)?;
                    saved.push(dest);
                }
                return Ok(RunResult {
                    prompt_id,
                    files: saved,
                });
            }
        }
        progress(0.1, "running");
        std::thread::sleep(poll);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn info() -> Value {
        json!({"CheckpointLoaderSimple": {"input": {"required": {"ckpt_name": [["modelA.safetensors", "modelB.safetensors"]]}}},
               "SaveImage": {"input": {"required": {"images": ["IMAGE"]}}}})
    }

    #[test]
    fn missing_node_and_missing_model_are_named() {
        let ok = json!({"1": {"class_type": "CheckpointLoaderSimple", "inputs": {"ckpt_name": "modelA.safetensors"}}});
        assert!(validate_against(&info(), &ok).is_ok());
        let bad_model = json!({"1": {"class_type": "CheckpointLoaderSimple", "inputs": {"ckpt_name": "nope.safetensors"}}});
        let e = validate_against(&info(), &bad_model).unwrap_err();
        assert!(e.message.contains("nope.safetensors"), "{e}");
        let bad_node = json!({"1": {"class_type": "WanVideo", "inputs": {}}});
        assert!(validate_against(&info(), &bad_node)
            .unwrap_err()
            .message
            .contains("not installed"));
    }

    #[test]
    fn binding_rejects_unknown_params_and_nodes() {
        let def = WorkflowDef {
            name: "t".into(),
            task: "upscale".into(),
            description: String::new(),
            workflow: json!({"1": {"class_type": "X", "inputs": {"a": 1}}}),
            params: [
                (
                    "scale".to_string(),
                    Binding {
                        node: "1".into(),
                        input: "a".into(),
                        description: String::new(),
                    },
                ),
                (
                    "ghost".to_string(),
                    Binding {
                        node: "9".into(),
                        input: "a".into(),
                        description: String::new(),
                    },
                ),
            ]
            .into(),
            file_params: vec![],
        };
        let wf = bind(&def, json!({"scale": 4}).as_object().unwrap()).unwrap();
        assert_eq!(wf["1"]["inputs"]["a"], 4);
        assert!(bind(&def, json!({"nope": 1}).as_object().unwrap()).is_err());
        assert!(bind(&def, json!({"ghost": 1}).as_object().unwrap()).is_err());
    }

    #[test]
    fn outputs_are_collected_across_kinds() {
        let h = json!({"p1": {"outputs": {"9": {"images": [{"filename": "a.png", "subfolder": "", "type": "output"}]}, "10": {"gifs": [{"filename": "b.mp4", "subfolder": "v", "type": "output"}]}}}});
        let o = outputs_of(&h, "p1");
        assert_eq!(o.len(), 2);
        assert_eq!(o[1].subfolder, "v");
    }
}
