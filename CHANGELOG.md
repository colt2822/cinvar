# Changelog

All notable changes. Format: [Keep a Changelog](https://keepachangelog.com/); versions follow SemVer
(0.x = alpha: anything may change).

## [0.2.0] - 2026-11-14 (CINVAR Free + CINVAR Pro Early Access)

- **Editions.** CINVAR Free (this Apache-2.0 repository, free, unlimited editing, no watermark, no account, export up to
  2048x1080 / 1080x2048) and CINVAR Pro Early Access (core + a separately licensed Pro module, $29.99 one-time perpetual).
- **Pro module:** 4K/8K export, batch rendering, saved export presets, advanced color grading. Offline Ed25519 license
  activation (`cinvar edition activate --file license.json`); web UI edition badge, license and Pro panels.
- **Open core.** The Pro module lives in a separate proprietary crate that links against the core; the Free build does not
  contain it. Extension points: `render::extensions`, `tools::register_extension`, `edition::register_module`.
- **Editing-action schema** extended with deterministic replay (symbolic references, canonical timeline hash) and a
  synthetic benchmark generator. Journaling stays off by default. No model is trained.
- Release builds enforce the Free export ceiling on every render path (CLI, tool/MCP, UI, Blender, enhance).

## [0.1.1-alpha] - unreleased (branch `feature/deepseek-hardening-showcase`)

### Security - deep hardening pass (see docs/DEEPSEEK_HARDENING_REVIEW.md)
Self-tested only: **no independent review of this pass has been performed.** Each item names the
test that catches a regression.

- **Filesystem boundary is descriptor-relative.** The boundary decision and the open of a path are
  now one kernel operation (`openat2` + `RESOLVE_BENEATH`, or a component `O_NOFOLLOW` walk), so a
  symlink or directory swapped between the check and the use cannot redirect a read.
  `import_asset` used to hash one open and copy from a second, so an asset record could describe
  bytes the project did not hold - reproduced, then fixed with a single-pass hash-and-copy from one
  descriptor and atomic `renameat` publication. `write_atomic` no longer creates a predictable temp
  name through a symlink-following open. (`tests/fs_race_security.rs`, 6 tests + 4 unit tests.)
- **Plugin sandbox denies `AF_UNIX` sockets.** A manifest declaring `network: true` kept the host
  network namespace and could reach host abstract sockets (`@dbus-*`, `@systemd/*`, docker). A raw-BPF
  seccomp program now returns `EPERM` for `socket(AF_UNIX, ...)` while TCP/UDP/IPv6 keep working,
  and fails closed to `--unshare-net` when no filter can be installed. Binds are canonicalised and a
  writable path containing the config directory loses its bind. (`tests/plugin_sandbox.rs`, 5 tests.)
- **Secret redaction covers configured names.** A model profile with a custom `api_key_env` no
  longer has its key echoed in tool errors.
- **Local UI and MCP request bodies are capped** (1 MiB JSON with an honest 413; 16 MiB MCP
  messages refused before allocation), and a media-route traversal answers an explicit 404 instead
  of a bodiless 500.
- Adversarial verification of what was already claimed: permission model (266 calls across every
  name form), cloud-approval single-use/expiry and self-approval through other tools, CSRF, DNS
  rebinding, traversal, hostile ComfyUI filenames. (`tests/mcp_adversarial.rs`, 6 tests.)

### Fixed
- **`cinvar.blender.animation` and `cinvar.ai.import_outputs` returned an asset id they never
  persisted.** Every later tool call loads `project.json`, so the id was unusable
  ("asset ... is not in this project") - found while building the Blender showcase. Regression test
  `tests/blender_real.rs::a_rendered_clip_survives_into_a_later_tool_call` renders a clip in one
  call and places it by id in the next; it fails with the persist removed.

### Added
- `src/util/fshandle.rs`: descriptor-relative open/copy/publish primitives with tests.
- Three reproducible showcase videos in `~/cinvar-showcase` (agent editing, Blender production,
  native 8K) with reproduction scripts, render logs and verified media metadata.

### Known issues in this version
See `docs/GITHUB_ISSUES_0.1.1.md` - no plugin sandbox on Windows/macOS, no per-plugin resource
limits, render inputs still lack `-protocol_whitelist file`, and the Windows drawtext/caption path.

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
