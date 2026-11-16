# CINVAR Progress Log

## Sprint 2026-11-13 — Autonomous Editing (FLASH 5.3)

Branch: `feature/deepseek-hardening-showcase` @ `0aef2f3` baseline.

### Baseline verification (P0)
- `./examples/quickstart.sh` — PASS: built, imported 3 clips, rendered 1080p MP4, ffprobe verified, QA passed. 13 tool calls, offline mock planner.
- `cargo test --release` — PASS: 166 lib + 42 integration tests.
- Confirmed existing capabilities: scene detection (`cinvar.media.scenes`), silence detection, transcription, keyframes, transitions, crossfades, undo/redo, rollback, 55+ MCP tools, QA render check, Blender/OTIO/OCIO.

### Implemented this sprint
- `src/analysis/autoedit.rs` — deterministic autonomous highlight-reel pipeline: folder scan → highlight scoring per file (scene cuts + audio energy) → greedy round-robin selection filling a target runtime → interleaved reel ordering (import-order deterministic) → timeline build with crossfades, normalised audio, optional title. Unit-tested.
- `cinvar auto-edit` CLI command — orchestrates plan → render → QA → correction loop (bounded by `--max-revisions`, default 2). Corrections implemented: black_flash (trim out point), audio too quiet/clipping (±6 dB), dead air (fade tails). `--project` now creates the project when missing.
- `cinvar.ai.auto_edit` MCP tool — same pipeline over the agent/tool interface; respects `within_agent_roots` filesystem security (verified: refuses paths outside allowed roots).
- QA additions (`src/qa/mod.rs`, `src/qa/probe.rs`) — `frozen_frames` (ffmpeg freezedetect) and `dead_air` (silencedetect inside the programme) checks in `validate_render`; both verified live on a crafted defective file.

### Verified demonstrations
| Demo | Result |
|---|---|
| 60s highlight reel from 5 synthetic clips | 10 clips, 60.0s, QA passed, 1080p MP4 |
| Vertical Shorts reel (`--vertical`) | 1080x1920, QA passed |
| Correction loop (footage at −71.6 dBFS mean) | pass 1 QA fail `audio_levels` → +6 dB correction → pass 3 QA passed |
| Dead-air QA check | injected 8 s silence detected at 10.01–18.02 s, FAIL reported |
| MCP agent-driven edit | `cinvar.ai.auto_edit` over stdio MCP built a 30 s timeline; rendered via `cinvar render --wait`, QA score 1.000 |

### Ownership / worktrees
Single integration checkout `~/cinvar`, no parallel workers this sprint; work was one vertical slice (P0) to avoid worktree merge overhead.

### Next
- P1: GPU benchmark harness (nvenc already used by renders — measure vs libx264), caption generation in the autoedit pipeline, semantic/keyword footage search over transcripts, frozen-frame correction (reselect window instead of flagging).

## Sprint II 2026-11-13 — Semantic Intelligence (FLASH 5.3 II)
Baseline `47b1025`. Implemented:
- Semantic scene index (`analysis/semantic.rs`, `cinvar analyze`, `cinvar scenes`, MCP `cinvar.scenes.index`/`cinvar.scenes.search`) with local vision (gemma3:4b via Ollama; moondream rejected for garbage output) and clearly-labelled deterministic fallback.
- Instruction-driven selection in auto-edit (0.6 heuristic + 0.4 semantic; greedy best-first under a brief); honest refusal when nothing matches.
- Captions integrated into auto-edit (`--captions --caption-preset subtitles|social|documentary|cinematic`) via faster-whisper; burned-in captions verified frame-by-frame; PyAV 15 compat shim.
- QA: caption_overflow project check; frozen_frames EOF-span bug fixed; frozen-frame repair (head/tail/middle, ripple trims) added to the correction loop.
- Benchmarks: encode-only NVENC vs x264 harness + independent spot check (`CINVAR_RENDER_BENCHMARKS.md`).
- Workflows A/B/C rendered and verified (`CINVAR_CREATIVE_WORKFLOWS.md`); the rendered demo videos are not included in the public source tree.
- Competitive evaluation from documented capabilities only (`CINVAR_COMPETITIVE_EVALUATION.md`).
Ownership: single integration checkout; benchmarks+competitive delegated to isolated workers, outputs integrated and spot-verified.
Known pre-existing failure: `plugin_sandbox::host_abstract_unix_sockets_are_not_reachable_from_a_network_plugin` fails on the clean baseline too (environmental, not a regression).
