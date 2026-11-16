# CINVAR INTEGRATION MATRIX — Sprint III

Baseline: `a813dfe` on `feature/deepseek-hardening-showcase`. Verified 2026-11-13:
**cargo test = 322 passed, 0 failed** (full suite, integration checkout).
Pattern: per-tool isolated venv at `~/.venvs/<name>` + env-var-selected Python helper
(`include_str!` pattern from `src/captions/fw_helper.py`) + Rust adapter + additive CLI.

| Project | Code license | Weights license | Status | Worktree / branch | Notes |
|---|---|---|---|---|---|
| PySceneDetect | BSD-3-Clause | n/a | **integrated** (b55e0af) | sprint3/scenes | cuts verified vs ground truth 3.0/6.5/10.0s → 3.00/6.52/10.04s |
| WhisperX | BSD-2-Clause | weights gated | **integrated** (0dc8858) | sprint3/captions | word timestamps verified; diarization skipped (HF token) |
| Auto-Editor | LGPL-3.0+ | n/a | **integrated** (e8df77b) | sprint3/audio | v1 plan → CINVAR-native pacing; cuts at 8.2/11.8s vs truth 8/12s |
| Demucs | MIT | MIT (gated download) | **integrated** (e8df77b) | sprint3/audio | 4 stems verified; clean mix rendered |
| libass | ISC (system ffmpeg) | n/a | **integrated** (0dc8858) | sprint3/captions | ASS karaoke burn-in verified visually + pixel diff |
| Real-ESRGAN | BSD-3 / MIT (ncnn-py) | BSD-3 (gated) | **integrated** (9697bf1) | sprint3/enhance | 2x/4x verified: duration/audio preserved, 30/30 frames |
| SAM 2 | Apache-2.0 (verify) | weights (verify) | not started | — | next priority |
| Grounding DINO | Apache-2.0 (verify) | weights (verify) | not started | — | — |
| Supervision | Apache-2.0 (verify) | n/a | not started | — | — |
| ByteTrack V2 | verify | n/a | not started | — | — |
| MediaPipe | Apache-2.0 (verify) | n/a | not started | — | — |
| MMAction2 | Apache-2.0 (verify) | weights (verify) | not started | — | — |

Statuses here are set only when the lead has verified the worker's evidence in the
integration checkout. "in progress" = dispatched to an isolated worktree worker, not
verified.

## Sprint III integration result (2026-11-13)

Final commit: see `git log` on `feature/deepseek-hardening-showcase`. All six
integrations merged (merge commits b3bbd07, fefec95, 3070250, 683b3fa + brace fix).
**cargo test: 355 passed, 0 failed** (baseline 322 → +33 new tests, no regressions).

## Already implemented in CINVAR (do not redo)

- Scene segmentation + local vision descriptions (Ollama gemma3:4b), semantic index + search
  (`cinvar analyze`, `cinvar scenes`, MCP `cinvar.scenes.*`) — Sprint II.
- Caption generation via faster-whisper (`~/.venvs/faster-whisper`), presets, burn-in — Sprint II.
- Autonomous auto-edit with QA correction loop (black flash, audio levels, dead air, frozen frames) — Sprints I–II.
- NVENC vs x264 render benchmarks — Sprint II.
- Known environmental failure: `plugin_sandbox::host_abstract_unix_sockets_are_not_reachable_from_a_network_plugin` fails on clean baseline.
