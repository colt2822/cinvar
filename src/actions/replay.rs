//! Deterministic replay of editing actions against a scratch project.
//!
//! Real ids are random, so recorded or model-written calls refer to things symbolically:
//! * `"@asset:NAME"` - the asset imported from the media file called NAME,
//! * `"@clip:N"`    - the Nth clip (0-based) that earlier calls created, in creation order,
//! * `"@track:N"`   - the Nth track (0-based) of the active timeline.
//!
//! After the calls run, the timeline is reduced to a canonical form with every random id removed
//! and keys sorted, then hashed. Replaying the same calls on the same media always gives the same
//! hash, so replay is a deterministic oracle for "did this action list produce this edit".

use std::collections::{BTreeMap, HashSet};
use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use super::{validate_calls, ToolCall};
use crate::project::store::ProjectStore;
use crate::tools::{Registry, ToolCtx};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReplayReport {
    pub ok: bool,
    pub errors: Vec<String>,
    pub calls_run: usize,
    /// Canonical, id-free timeline summary after the last call.
    pub timeline: Value,
    /// SHA-256 of the canonical timeline JSON.
    pub hash: String,
}

fn sorted(v: &Value) -> Value {
    match v {
        Value::Object(o) => {
            let m: BTreeMap<_, _> = o
                .iter()
                .filter(|(k, _)| k.as_str() != "id")
                .map(|(k, v)| (k.clone(), sorted(v)))
                .collect();
            Value::Object(m.into_iter().collect())
        }
        Value::Array(a) => Value::Array(a.iter().map(sorted).collect()),
        other => other.clone(),
    }
}

/// Canonical form of the active timeline: ids removed, assets named by their imported file name.
pub fn canonical_timeline(
    project: &crate::project::model::Project,
    asset_names: &BTreeMap<String, String>,
) -> Value {
    let Some(tl) = project.timeline() else {
        return Value::Null;
    };
    let mut v = serde_json::to_value(tl).unwrap_or(Value::Null);
    if let Some(tracks) = v.get_mut("tracks").and_then(|t| t.as_array_mut()) {
        for t in tracks {
            if let Some(clips) = t.get_mut("clips").and_then(|c| c.as_array_mut()) {
                for c in clips {
                    if let Some(a) = c.get("asset").and_then(|a| a.as_str()) {
                        let label = asset_names.get(a).cloned().unwrap_or_else(|| "?".into());
                        c["asset"] = json!(label);
                    }
                }
            }
        }
    }
    sorted(&v)
}

pub fn hash_value(v: &Value) -> String {
    let mut h = Sha256::new();
    h.update(serde_json::to_string(v).unwrap_or_default().as_bytes());
    h.finalize().iter().map(|b| format!("{b:02x}")).collect()
}

struct Bindings {
    assets: BTreeMap<String, String>, // NAME -> asset id
    clips: Vec<String>,
}

fn resolve(
    v: &Value,
    b: &Bindings,
    project: &crate::project::model::Project,
) -> Result<Value, String> {
    match v {
        Value::String(s) if s.starts_with("@asset:") => b
            .assets
            .get(&s[7..])
            .map(|id| json!(id))
            .ok_or_else(|| format!("unknown media '{}'", &s[7..])),
        Value::String(s) if s.starts_with("@clip:") => {
            let n: usize = s[6..].parse().map_err(|_| format!("bad reference '{s}'"))?;
            b.clips
                .get(n)
                .map(|id| json!(id))
                .ok_or_else(|| format!("{s}: only {} clip(s) exist so far", b.clips.len()))
        }
        Value::String(s) if s.starts_with("@track:") => {
            let n: usize = s[7..].parse().map_err(|_| format!("bad reference '{s}'"))?;
            project
                .timeline()
                .and_then(|t| t.tracks.get(n))
                .map(|t| json!(t.id))
                .ok_or_else(|| format!("{s}: no such track"))
        }
        Value::Array(a) => a
            .iter()
            .map(|x| resolve(x, b, project))
            .collect::<Result<Vec<_>, _>>()
            .map(Value::Array),
        Value::Object(o) => {
            let mut m = serde_json::Map::new();
            for (k, x) in o {
                m.insert(k.clone(), resolve(x, b, project)?);
            }
            Ok(Value::Object(m))
        }
        other => Ok(other.clone()),
    }
}

fn collect_clip_ids(result: &Value, seen: &mut HashSet<String>, out: &mut Vec<String>) {
    let mut push = |s: &str| {
        if seen.insert(s.to_string()) {
            out.push(s.to_string());
        }
    };
    for key in ["clip_id", "left_clip_id", "right_clip_id", "new_clip_id"] {
        if let Some(s) = result.get(key).and_then(|v| v.as_str()) {
            push(s);
        }
    }
    if let Some(a) = result.get("clip_ids").and_then(|v| v.as_array()) {
        for s in a.iter().filter_map(|v| v.as_str()) {
            push(s);
        }
    }
}

/// Runs `calls` in a fresh scratch project after importing `media` (NAME, path) pairs.
/// Stops at the first failing call. Never touches anything outside a temp directory.
pub fn replay(
    registry: &Registry,
    calls: &[ToolCall],
    media: &[(String, PathBuf)],
) -> ReplayReport {
    let fail = |errors: Vec<String>, calls_run| ReplayReport {
        ok: false,
        errors,
        calls_run,
        timeline: Value::Null,
        hash: String::new(),
    };
    let v = validate_calls(registry, calls);
    if !v.ok {
        return fail(v.errors, 0);
    }
    let dir = match tempfile::tempdir() {
        Ok(d) => d,
        Err(e) => return fail(vec![format!("tempdir: {e}")], 0),
    };
    let (store, mut project) = match ProjectStore::create(dir.path(), "replay") {
        Ok(x) => x,
        Err(e) => return fail(vec![format!("project: {}", e.message)], 0),
    };
    let jobs = crate::jobs::Jobs::new();
    let mut history = crate::project::history::History::default();
    let config = crate::config::Config::default(); // journaling stays off during replay
    let mut b = Bindings {
        assets: BTreeMap::new(),
        clips: vec![],
    };
    let mut seen = HashSet::new();
    // The harness (not the agent) imports the scratch media, so the agent file boundary is not
    // involved; calls under test only ever see the resulting asset ids.
    for (name, path) in media {
        let file = match std::fs::File::open(path) {
            Ok(f) => f,
            Err(e) => return fail(vec![format!("open {name}: {e}")], 0),
        };
        match store.import_handle(
            &mut project,
            file,
            name,
            "replay:import",
            crate::project::store::ImportPolicy::MediaOnly,
        ) {
            Ok(asset) => {
                b.assets.insert(name.clone(), asset.id);
            }
            Err(e) => return fail(vec![format!("import {name}: {}", e.message)], 0),
        }
    }
    if let Err(e) = store.save(&mut project) {
        return fail(vec![format!("save: {}", e.message)], 0);
    }
    let mut ctx = ToolCtx {
        project: &mut project,
        store: &store,
        config: &config,
        jobs: &jobs,
        history: &mut history,
        dry_run: false,
    };
    let mut n = 0;
    for (i, call) in calls.iter().enumerate() {
        let empty = Value::Object(Default::default());
        let raw = if call.args.is_null() {
            &empty
        } else {
            &call.args
        };
        let args = match resolve(raw, &b, ctx.project) {
            Ok(a) => a,
            Err(e) => return fail(vec![format!("call {i} ({}): {e}", call.tool)], n),
        };
        match registry.call(&call.tool, &mut ctx, args) {
            Ok(r) => collect_clip_ids(&r, &mut seen, &mut b.clips),
            Err(e) => return fail(vec![format!("call {i} ({}): {}", call.tool, e.message)], n),
        }
        n += 1;
    }
    let names: BTreeMap<String, String> = b
        .assets
        .iter()
        .map(|(k, v)| (v.clone(), k.clone()))
        .collect();
    let timeline = canonical_timeline(ctx.project, &names);
    let hash = hash_value(&timeline);
    ReplayReport {
        ok: true,
        errors: vec![],
        calls_run: n,
        timeline,
        hash,
    }
}
