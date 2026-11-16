//! Synthetic editing benchmark.
//!
//! Generates instruction -> action-list tasks from templates over synthetic (ffmpeg `lavfi`) media,
//! with reference calls and a reference timeline hash obtained by replaying them. A candidate (a
//! rules baseline today, a model later) is scored by validity, replay success and exact timeline
//! match. Everything is deterministic for a given seed. No customer data, no media from elsewhere;
//! every example is `provenance.source = "synthetic"`. This is evaluation scaffolding only; no model
//! is trained here.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use super::replay::{replay, ReplayReport};
use super::{DatasetExample, Provenance, ToolCall, ValidationResult, DATASET_SCHEMA};
use crate::tools::Registry;

pub const BENCH_SCHEMA: &str = "cinvar.bench/1";

/// Synthetic media the tasks refer to: (name, seconds, has_audio).
pub const MEDIA: [(&str, u32, bool); 3] =
    [("a.mp4", 6, true), ("b.mp4", 4, true), ("c.mp4", 8, true)];

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BenchTask {
    pub schema: String,
    pub id: String,
    /// cut | trim | arrange | transition | audio | color | keyframe | speed
    pub category: String,
    pub instruction: String,
    pub media: Vec<String>,
    pub reference_calls: Vec<ToolCall>,
    pub reference_hash: String,
    pub reference_timeline: Value,
}

fn rng(seed: u64) -> impl FnMut(u64) -> u64 {
    let mut s = seed
        .wrapping_mul(6364136223846793005)
        .wrapping_add(1442695040888963407);
    move |n| {
        s = s
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        (s >> 33) % n.max(1)
    }
}

fn call(tool: &str, args: Value) -> ToolCall {
    ToolCall {
        tool: format!("cinvar.{tool}"),
        args,
    }
}

fn setup() -> Vec<ToolCall> {
    vec![call(
        "timeline.create",
        json!({"name": "Main", "width": 1280, "height": 720, "fps": "24"}),
    )]
}

/// Writes the three synthetic clips (needs `ffmpeg`). Returns (name, path) pairs for `replay`.
pub fn generate_media(dir: &Path) -> std::io::Result<Vec<(String, PathBuf)>> {
    let mut out = vec![];
    for (i, (name, secs, _audio)) in MEDIA.iter().enumerate() {
        let path = dir.join(name);
        let st = std::process::Command::new("ffmpeg")
            .args(["-y", "-v", "error", "-f", "lavfi", "-i"])
            .arg(format!("testsrc2=size=640x360:rate=24:duration={secs}"))
            .args(["-f", "lavfi", "-i"])
            .arg(format!("sine=frequency={}:duration={secs}", 220 * (i + 1)))
            .args([
                "-c:v",
                "libx264",
                "-pix_fmt",
                "yuv420p",
                "-c:a",
                "aac",
                "-shortest",
            ])
            .arg(&path)
            .status()?;
        if !st.success() {
            return Err(std::io::Error::other(
                "ffmpeg failed to generate synthetic media",
            ));
        }
        out.push((name.to_string(), path));
    }
    Ok(out)
}

const CATEGORIES: [&str; 8] = [
    "cut",
    "trim",
    "arrange",
    "transition",
    "audio",
    "color",
    "keyframe",
    "speed",
];

fn task_calls(cat: &str, r: &mut impl FnMut(u64) -> u64) -> (String, Vec<ToolCall>, Vec<String>) {
    let mut calls = setup();
    let app = |n: &str| {
        call(
            "timeline.append_clip",
            json!({"asset_id": format!("@asset:{n}")}),
        )
    };
    match cat {
        "cut" => {
            let t = 1 + r(4);
            calls.push(app("a.mp4"));
            calls.push(call(
                "timeline.split",
                json!({"clip_id": "@clip:0", "at": format!("{t}.0")}),
            ));
            (
                format!("Cut a.mp4 into two clips at {t} seconds."),
                calls,
                vec!["a.mp4".into()],
            )
        }
        "trim" => {
            let out = 2 + r(3);
            calls.push(app("b.mp4"));
            calls.push(call(
                "timeline.trim",
                json!({"clip_id": "@clip:0", "source_out": format!("{out}.0")}),
            ));
            (
                format!("Trim b.mp4 so it ends at {out} seconds of the source."),
                calls,
                vec!["b.mp4".into()],
            )
        }
        "arrange" => {
            let orders = [[2, 0, 1], [1, 2, 0], [2, 1, 0]];
            let o = orders[r(3) as usize];
            for n in ["a.mp4", "b.mp4", "c.mp4"] {
                calls.push(app(n));
            }
            calls.push(call(
                "timeline.reorder",
                json!({"track_id": "@track:0", "clip_ids": o.iter().map(|i| format!("@clip:{i}")).collect::<Vec<_>>()}),
            ));
            let names = ["a", "b", "c"];
            (
                format!(
                    "Put the clips in this order: {}, then {}, then {}.",
                    names[o[0]], names[o[1]], names[o[2]]
                ),
                calls,
                vec!["a.mp4".into(), "b.mp4".into(), "c.mp4".into()],
            )
        }
        "transition" => {
            let d = [0.5, 1.0, 1.5][r(3) as usize];
            calls.push(app("a.mp4"));
            calls.push(app("b.mp4"));
            calls.push(call("timeline.transition", json!({"clip_id": "@clip:1", "side": "in", "kind": "crossfade", "duration": format!("{d}")})));
            (
                format!("Add a {d} second crossfade into the second clip."),
                calls,
                vec!["a.mp4".into(), "b.mp4".into()],
            )
        }
        "audio" => {
            let g = -(2 + r(8) as i64);
            let f = 1 + r(2);
            calls.push(app("c.mp4"));
            calls.push(call("timeline.clip_audio", json!({"clip_id": "@clip:0", "gain_db": g, "fade_in": format!("{f}.0"), "fade_out": format!("{f}.0")})));
            (
                format!(
                    "Lower c.mp4 by {} dB and fade the audio in and out over {f} second(s).",
                    -g
                ),
                calls,
                vec!["c.mp4".into()],
            )
        }
        "color" => {
            let v = (r(5) as f64 + 1.0) / 20.0;
            calls.push(app("a.mp4"));
            calls.push(call(
                "timeline.effect",
                json!({"clip_id": "@clip:0", "kind": "brightness", "params": {"value": v}}),
            ));
            (
                format!("Brighten a.mp4 by {v}."),
                calls,
                vec!["a.mp4".into()],
            )
        }
        "keyframe" => {
            let v = [0.0, 0.25, 0.5][r(3) as usize];
            calls.push(app("b.mp4"));
            calls.push(call("timeline.keyframe", json!({"clip_id": "@clip:0", "property": "opacity", "at": "0.0", "value": v, "easing": "linear"})));
            calls.push(call("timeline.keyframe", json!({"clip_id": "@clip:0", "property": "opacity", "at": "1.0", "value": 1.0, "easing": "linear"})));
            (
                format!("Fade b.mp4 up from opacity {v} to full over the first second."),
                calls,
                vec!["b.mp4".into()],
            )
        }
        _ => {
            let s = [0.5, 1.5, 2.0][r(3) as usize];
            calls.push(app("c.mp4"));
            calls.push(call(
                "timeline.speed",
                json!({"clip_id": "@clip:0", "speed": s}),
            ));
            (
                format!("Play c.mp4 at {s}x speed."),
                calls,
                vec!["c.mp4".into()],
            )
        }
    }
}

/// Builds `n` tasks (cycling the categories) and fills in reference hashes by replaying them.
pub fn generate(
    registry: &Registry,
    media: &[(String, PathBuf)],
    seed: u64,
    n: usize,
) -> Vec<BenchTask> {
    let mut r = rng(seed);
    (0..n)
        .map(|i| {
            let cat = CATEGORIES[i % CATEGORIES.len()];
            let (instruction, reference_calls, used) = task_calls(cat, &mut r);
            let m: Vec<_> = media
                .iter()
                .filter(|(n, _)| used.contains(n))
                .cloned()
                .collect();
            let rep = replay(registry, &reference_calls, &m);
            assert!(
                rep.ok,
                "reference calls for {cat} failed to replay: {:?}",
                rep.errors
            );
            BenchTask {
                schema: BENCH_SCHEMA.into(),
                id: format!("syn-{seed}-{i:04}"),
                category: cat.into(),
                instruction,
                media: used,
                reference_calls,
                reference_hash: rep.hash,
                reference_timeline: rep.timeline,
            }
        })
        .collect()
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Score {
    pub valid: bool,
    pub replayed: bool,
    pub exact: bool,
    pub errors: Vec<String>,
}

/// Scores a candidate action list against a task.
pub fn score(
    registry: &Registry,
    task: &BenchTask,
    media: &[(String, PathBuf)],
    candidate: &[ToolCall],
) -> Score {
    let v = super::validate_calls(registry, candidate);
    if !v.ok {
        return Score {
            errors: v.errors,
            ..Default::default()
        };
    }
    let m: Vec<_> = media
        .iter()
        .filter(|(n, _)| task.media.contains(n))
        .cloned()
        .collect();
    let rep: ReplayReport = replay(registry, candidate, &m);
    Score {
        valid: true,
        replayed: rep.ok,
        exact: rep.ok && rep.hash == task.reference_hash,
        errors: rep.errors,
    }
}

/// Turns a task into a training-format example (still requires explicit permission to train; the
/// synthetic source is ours so it is permitted, but nothing is trained in this repository).
pub fn to_dataset_example(task: &BenchTask) -> DatasetExample {
    DatasetExample {
        schema: DATASET_SCHEMA.into(),
        instruction: task.instruction.clone(),
        project_context: json!({"media": task.media}),
        media_metadata: MEDIA
            .iter()
            .filter(|(n, _, _)| task.media.iter().any(|m| m == n))
            .map(|(n, s, a)| json!({"name": n, "duration": s, "width": 640, "height": 360, "fps": 24, "audio": a}))
            .collect(),
        editing_plan: task.reference_calls.iter().map(|c| c.tool.clone()).collect(),
        tool_calls: task.reference_calls.clone(),
        expected_timeline: task.reference_timeline.clone(),
        validation_result: ValidationResult { ok: true, errors: vec![] },
        provenance: Provenance {
            source: "synthetic".into(),
            rights: "generated by CINVAR benchmark templates over ffmpeg lavfi test patterns".into(),
            training_permitted: true,
        },
    }
}
