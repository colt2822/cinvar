# Editing actions and dataset format

An **editing action** is one call to a CINVAR tool: `{"tool": "cinvar.timeline.trim", "args": {...}}`.
The ~80 tools (`docs/TOOLS.md`) already have JSON-schema inputs and refuse invalid edits, so there is
one command language shared by the UI, the agent loop, MCP clients and any future editing model.

## Validate before executing

`cinvar::actions::validate_calls(&registry, &calls)` checks, without running anything: the tool
exists, required arguments are present, no unknown arguments, top-level argument types match.
Anything a model emits should pass this before it is replayed against a project.

## Journal (opt-in, local)

Set `[actions] record = true` in `config.toml`. Each tool call appends a `cinvar.action/1` line to
`<project>/actions/actions.jsonl`:

    {"schema":"cinvar.action/1","ts":"…","project_id":"…","tool":"cinvar.timeline.trim",
     "args":{"clip_id":"c1","source_out":"2.0"},"ok":true,"revision_before":4,"revision_after":5}

Failed calls are recorded with `ok:false` and `error_code`. Absolute paths become `<path>/file.ext`.
Media is never copied. Nothing is uploaded. Default: off.

## Dataset example — `cinvar.dataset/1`

| Field | Meaning |
|---|---|
| `instruction` | The user's request in natural language |
| `project_context` | Timeline/settings summary the model sees first |
| `media_metadata` | Per asset: duration, size, fps, audio — metadata only |
| `editing_plan` | Short step list in words |
| `tool_calls` | Ordered `{tool,args}` that realise the plan (must pass `validate_calls`) |
| `expected_timeline` | Timeline summary after replaying the calls |
| `validation_result` | `{ok, errors}` from validating/replaying |
| `provenance` | `{source: synthetic|licensed|owner_created|customer_opt_in, rights, training_permitted}` |

`DatasetExample::validate` rejects: no instruction, no calls, invalid calls, missing `rights`,
unknown `source`, or `training_permitted: false`.

## Rules

- Start from synthetic footage (the test fixtures) or footage whose license permits training.
- Customer projects/journals are never used without explicit opt-in and rights; do not resell them.
- A journal is raw material, not a dataset: rights review and replay-verified `expected_timeline`
  are required before an example is trainable. No model training is part of this work.

## Replay and the synthetic benchmark

`cinvar::actions::replay::replay(registry, calls, media)` runs a call list in a scratch project (a
temp directory; the journal is off) and returns the canonical timeline plus a SHA-256. Real ids are
random, so calls reference things symbolically: `"@asset:NAME"` (an imported media file),
`"@clip:N"` (the Nth clip created by earlier calls), `"@track:N"` (Nth track). The canonical form
drops every random id, so identical calls on identical media always hash the same.

`cinvar::actions::bench` generates deterministic tasks (cut, trim, arrange, transition, audio,
color, keyframe, speed) over ffmpeg test-pattern media, with reference calls and a reference hash,
and `score()` grades a candidate on: valid / replays / exact timeline match. Generate a set:

    cargo run --example synth_bench -- OUT_DIR 64 1     # tasks.jsonl + dataset.jsonl

Every generated example has `provenance.source = "synthetic"`. Rendering is intentionally not part
of replay (it is an output step, not an edit). This is evaluation scaffolding; no model is trained
by this repository, and journaling remains off by default.
