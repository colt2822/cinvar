# CINVAR Ascension — user guide

## Blender (real, local)
Set `CINVAR_BLENDER=/path/to/blender` (3.6+; verified on 5.3.0). Agents call `cinvar.blender.animation` with a *parameterised template* — never raw Python. Renders are resumable (hash-keyed job dir), cancellable, and imported as assets.

## High-resolution export
`cinvar.render.preflight` (size_preset 720p|1080p|1440p|4k|8k, bit_depth 8|10) tells you, before any work starts, whether this machine can do it, the encoder that will be used (trial-encoded at the exact size) and an estimated RAM/disk/time. Requests above 16384 px are refused, not clamped. On the reference host `h264_nvenc` fails at 8K and CINVAR falls back to libx264; `hevc_nvenc` works. Upscaling is a scale filter, not AI upscaling.

## Durable jobs
Exports are recorded in `<project>/jobs/`. Duplicate submissions are refused (`duplicate_of`); after a crash, `cinvar.jobs.list` shows `Interrupted` jobs and `cinvar.jobs.retry` re-runs them (bounded).

## RunPod cloud rendering (optional, off by default)
1. `security.allow_cloud_render = true`; key in `CINVAR_RUNPOD_API_KEY` (or credentials profile `runpod`). Keys are never written to projects, jobs or logs.
2. `cinvar.render.cloud` first returns a **quote**; nothing is created until a human runs `cinvar cloud approve <id>` and the agent re-calls with `confirm_quote`. A price ceiling and runtime limit are required (max 24 h). The pod is terminated if its price exceeds the ceiling.
3. Cost protection is **best effort**: RunPod offers no provider-side hard spend cap through this API; the worker self-stops at its runtime limit. Always check your RunPod console. `cinvar.cloud.cleanup` terminates leftover pods of this project.
4. Only a local mock RunPod API has been tested. The `deploy/runpod/Dockerfile` is an untested template.

## Plugins and ComfyUI
`cinvar.plugins.list/set_enabled/pin` show health, compatibility, missing dependencies and disclosures (costs money / uses cloud / sends files off-device). External plugins are opt-in; install instructions are displayed, never executed. ComfyUI workflows are registered in `<config>/comfy-workflows`, validated against `/object_info`, and run through `cinvar.ai.run_workflow` (tested against a mock server only).

## OpenTimelineIO
`cinvar otio export`, `cinvar.otio.import` (only `.otio`, media file types only). Verified against the real OpenTimelineIO 0.18.1 library.

## MCP
stdio only. `CINVAR_MCP_PERMISSIONS=read_only|edit|full` (unknown values fall back to read-only; default `edit` (no cloud spend or plugin administration)). `CINVAR_MCP_NAMES=safe` advertises `cinvar_foo_bar` names for clients that reject dots. Logical aliases (`cinvar.render_cloud`, …) are listed by `cinvar.system.aliases`.

Long operations (`render.export`, `render.cloud`, `blender.animation`) return a job id immediately where durable; poll `cinvar.jobs.list`. Cancellation of in-flight work is supported inside the engine (Blender, cloud, ComfyUI) but is **not** exposed as an MCP `notifications/cancelled` handler; MCP progress notifications are not emitted. Authenticated remote (HTTP) transport is not built.

## Colour (OpenColorIO)
`cinvar.color.inspect` lists a config's colour spaces (project `color/config.ocio`, `$OCIO`, else the OCIO builtin). `cinvar.color.apply_transform` bakes ONE named source->destination transform into a `.cube` in the project and applies it as a visible `lut` effect (remove the effect to undo). Unknown names are errors; with no colour step, pixels are untouched. LUT values are clamped to 0..1 (no HDR range). Needs `CINVAR_OCIO_PYTHON`; validated against real OpenColorIO 2.6.0.

## Transcription and captions
`cinvar.captions.transcribe` uses faster-whisper (CPU int8, local model only) when `CINVAR_FASTER_WHISPER_PYTHON` is set, else whisper.cpp/whisper. Cues land on the timeline as an editable caption track; `cinvar.captions.export` writes `.srt`/`.vtt` inside the project. WhisperX and DeepFilterNet are not integrated.

## Cloud account
`cinvar cloud connect|status|disconnect`. `connect` reads the key from stdin, validates it against RunPod with a read-only call, and stores it privately (0600). Cloud renders additionally require `acknowledge_billing_risk=true` and may carry `max_spend_usd`; neither replaces checking your RunPod console.

## Plugins are sandboxed (Linux)
Plugin subprocesses run under bubblewrap: read-only system, empty home, project-only writes, no network unless the manifest asks. Without bubblewrap they are refused unless `security.allow_unsandboxed_plugins = true`. The CINVAR config directory (credentials) is hidden from plugin code.

## Dependency licences
145 third-party Rust crates: all permissive (MIT/Apache-2.0/BSD/ISC/Zlib/Unicode/Unlicense/CDLA-Permissive/0BSD; `r-efi` offers an MIT option). External programs run as separate processes: FFmpeg (GPL if built with x264/x265, as on the reference host), Blender (GPL), OpenTimelineIO (Apache-2.0, test-only helper), ComfyUI (GPL-3.0, separate service), RunPod (commercial service). Legal review of redistribution is not done.
