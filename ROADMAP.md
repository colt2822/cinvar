# Roadmap

Honest status first: this is a **source alpha** for developers. The engine works; the edges are rough.
Items marked 🌱 are good first issues. Priorities are proposals - open an issue to argue for a different order.

Legend: ✅ working (real tests) · 🧪 experimental (mock/limited tests) · ⬜ planned

## Core engine
- ✅ Timelines, FFmpeg rendering, QA, durable jobs, up-to-8K export
- ⬜ Ripple trim / ripple delete (agents currently move later clips by hand)
- ⬜ Keyframe animation that survives rendering (scale keyframes currently do not render)
- ⬜ Proxy media + faster preview renders
- 🌱 More `cinvar doctor` hints (missing fonts, encoder list explanations)

## MCP
- ✅ stdio server, permission modes, safe tool names
- ⬜ MCP progress + `notifications/cancelled` support
- ⬜ Verified configs for Cursor, Codex, Gemini CLI, Claude Desktop (templates exist in `docs/MCP.md`; only Claude Code has been exercised)
- ⬜ Authenticated remote transport (opt-in, off by default)
- 🌱 Add a tool-reference generator (`cinvar mcp tools --markdown`)

## Blender
- ✅ Parameterised animation templates, native 8K frames
- ⬜ More templates (lower thirds, logo reveals, particle backgrounds) 🌱
- ⬜ Compositing / VFX passes, image-sequence import

## Cloud rendering
- 🧪 RunPod adapter, quote → confirm → render → verify → terminate (mock only)
- ⬜ One supervised live smoke test by someone with a RunPod account and a spend cap
- ⬜ Generic "your own render server" backend (SSH/HTTP worker)

## AI integrations
- 🧪 ComfyUI workflow discovery/run (mock only)
- ✅ faster-whisper transcription
- ⬜ WhisperX alignment, DeepFilterNet denoise, AI upscaling, background removal, interpolation, tracking - as **plugins** 🌱

## Cross-platform
- ⬜ Run CI on Windows and macOS and fix what breaks (workflow is written; never executed)
- ⬜ Plugin sandbox for macOS (sandbox-exec) and Windows (job objects / AppContainer)
- ⬜ Prebuilt release binaries (after the above)

## User experience
- ⬜ Optional desktop GUI (there is a local web UI: `cinvar ui`)
- ⬜ Installer / package-manager formulas
- 🌱 Translate the getting-started guide

## Plugins
- ✅ Manifest v1, registry, sandboxed execution, example plugin
- ⬜ `cinvar plugins new` scaffold + `cinvar plugins test` (wraps `plugins/test_plugin.py`) 🌱
- ⬜ Community plugin index (a plain git list, not a paid marketplace)

## Security
- ⬜ Independent security review of the paths listed in `docs/SECURITY_REVIEW_PACKAGE.md`
- ⬜ Race-test the OTIO import re-check; close the gap off Linux

## Not planned
Mandatory accounts, telemetry, subscriptions, a paid plugin store, or features that require ELYREON-hosted services.
