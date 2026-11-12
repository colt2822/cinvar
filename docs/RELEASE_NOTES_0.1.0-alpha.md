# CINVAR 0.1.0-alpha — first public source release

An open-source, model-agnostic engine for AI-native video production: Rust + FFmpeg + Blender + MCP. **This is an alpha for
developers.** There is no GUI and no installer, and nothing here requires an account, telemetry, a subscription or a paid API.
Build it from source: `./examples/quickstart.sh` (needs Rust and FFmpeg).

## WORKING (exercised by tests that run the real software, on Linux x86_64)
- Project/timeline engine, FFmpeg rendering, deterministic QA, durable crash-safe jobs; MCP stdio server with permission modes (77 tools).
- 4K and 8K FFmpeg export; native 8K Blender rendering (Blender 5.3.0).
- OpenTimelineIO export/import (verified against OTIO 0.18.1), OpenColorIO LUT workflow (OCIO 2.6.0).
- faster-whisper transcription with SRT/VTT export (faster-whisper 1.2.1, local models only).
- Linux plugin sandbox (bubblewrap) and an example plugin.
- Human-controlled cloud authorization (`cinvar cloud approve`) and project-scoped agent file access by default.
- 284 automated tests, clippy `-D warnings`, `cargo audit` clean; 8 further tests run against real Blender/OTIO/OCIO/faster-whisper.

## EXPERIMENTAL (mock-tested only, or limited environments)
- **RunPod cloud rendering** — tested against a local mock; never run against a real account. RunPod has no provider-side spend cap: CINVAR's limits are local best-effort.
- **ComfyUI** workflow runner — mock-tested only.
- Model providers (OpenAI-compatible, Anthropic, Gemini, Ollama): wire formats unit-tested; Claude Code over MCP was driven for real.
- Codex / Cursor / Gemini CLI MCP configs in docs/MCP.md are templates, not tested against those clients.
- CI workflow: Linux gates; Linux (Ubuntu 24.04, FFmpeg 6.1 and 8) is green. macOS and Windows run as experimental, non-gating jobs: macOS test suite passed in the latest run; Windows fails the drawtext/text-rendering tests (Fontconfig crash in the CI FFmpeg build). Treat both platforms as unsupported for now.

## PLANNED (not implemented)
Desktop GUI and installers, Windows/macOS plugin sandbox, WhisperX, DeepFilterNet, AI upscaling, OpenFX, remote (HTTP) MCP,
MCP progress/cancel notifications. See ROADMAP.md.

## Security
Two review rounds by a fresh-context AI reviewer and one focused independent review by Codex found and fixed 3+1 high-severity
issues before this release; details and remaining limitations: docs/SECURITY_REVIEW_PACKAGE.md. This is not a formal audit.
Report vulnerabilities privately (SECURITY.md).

## Licensing
Apache-2.0. Third-party notices: THIRD_PARTY_NOTICES.md. FFmpeg, Blender, ComfyUI and model weights are separate works with their
own licences; none are bundled. Inventory only, not legal advice (docs/LICENSES.md).

## Artifacts
Source (GitHub-generated archives) and a Linux x86_64 build of the CLI with its SHA-256, built by `scripts/package.sh` from the tagged commit.


## Known issues

- `ducking_lowers_music_only_while_speech_plays` fails intermittently on FFmpeg 8 (passed on other runs; unresolved). FFmpeg 6.1 is the gating configuration.
- Windows: text/caption rendering tests fail in CI (FFmpeg drawtext/Fontconfig crash). No plugin sandbox on Windows/macOS.
- No formal third-party security audit; a focused self/Codex review is documented in `docs/SECURITY_REVIEW_PACKAGE.md`.
