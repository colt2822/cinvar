# Changelog

All notable changes. Format: [Keep a Changelog](https://keepachangelog.com/); versions follow SemVer
(0.x = alpha: anything may change).

## [Unreleased] - 0.1.0-alpha (first public source release)

### Working (exercised by tests that run real software)
- Rust engine: project documents, timelines, FFmpeg rendering, QA, durable job store, MCP stdio server.
- Blender 3.6+ integration through parameterised templates (verified on 5.3.0), including native 8K frames.
- FFmpeg exports up to 8K with preflight checks and encoder fallback.
- OpenTimelineIO export/import (verified against OpenTimelineIO 0.18.1), OpenColorIO transforms baked to LUTs (OCIO 2.6.0).
- faster-whisper transcription with SRT/VTT export (faster-whisper 1.2.1, local models only).
- Linux plugin sandbox (bubblewrap) and an example plugin.

### Experimental (mock-tested only - no live validation)
- RunPod cloud rendering (adapter, quote/confirm flow, cost guards). Never run against a live account.
- ComfyUI client and workflow runner.

### Security (fresh-context review, round 2 - see docs/SECURITY_REVIEW_PACKAGE.md R-01..R-09)
- Plugin sandbox now hides host control sockets (`/run`), drops capabilities; plugin commands are gated at call time; health checks never run for disabled plugins.
- Local web UI refuses wrong `Host`, foreign `Origin` and non-JSON mutations; its embedded agent obeys the MCP permission mode.
- ComfyUI `prompt_id` validated; `project.relink` content-validated and confined by `media_roots`; writes no longer follow planted symlinks; apostrophes in titles no longer break renders; credentials file created 0600.

### Security (owner-requested hardening before publication)
- **Human-controlled cloud authorization:** `cinvar cloud approve <quote-id>` (interactive terminal only, typed `APPROVE`, single-use, 15-minute expiry); the agent's own arguments no longer authorise billing.
- **Project-scoped default file access:** agents read/write only inside the project; extra folders via `--media-root`, `CINVAR_MEDIA_ROOTS` or `security.media_roots`, chosen by the human.

- Independent Codex review (round 3): render partial-file symlink overwrite fixed; Gemini key moved to a header; import/inspect use resolved paths.

### Changed
- MCP sessions now default to `edit` permission (was `full`): cloud-spend and plugin-administration tools need
  `CINVAR_MCP_PERMISSIONS=full` set by whoever starts the server.
- Repository prepared for publication: generated demo media, model transcripts and internal status
  reports with local paths removed from the tree (internal reports kept, sanitised, in `docs/history/`).

### Not implemented
GUI/installer, WhisperX, DeepFilterNet, AI upscaling, OpenFX, remote (HTTP) MCP, Windows/macOS plugin sandbox.
