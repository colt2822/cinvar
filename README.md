# CINVAR

**An open-source, model-agnostic engine for AI-native video production** - Rust, FFmpeg, Blender, up to 8K, MCP.

> **Status: public alpha (0.1.0), source release.** The engine is real and tested; it is a command-line tool plus an
> MCP server. There is **no native installer** (a local web studio opens from a desktop icon on Linux, or `cinvar ui`). Windows and macOS builds
> are **partially verified** (macOS suite passed in CI; Windows text rendering fails). RunPod and ComfyUI support is **experimental (mock-tested only)**. See
> [Status](#status-what-is-verified-and-what-is-not).

CINVAR gives people *and* AI agents the capabilities of a video editor: import media, build a timeline, render with
FFmpeg or Blender, inspect the result, revise. Use it by hand, from any MCP client (Claude Code, Codex, Cursor, local
agents), or with your own model (OpenAI-compatible, Anthropic, Gemini, Ollama, llama.cpp). It runs entirely on your
machine: **no account, no telemetry, no subscription, no ELYREON services, no paid API required.** RunPod cloud
rendering and ComfyUI are optional add-ons you connect with your own accounts/installs.

## Launching the app (no terminal)

Linux: run `packaging/linux/install-desktop.sh` once (after building, or from the release tarball). A **CINVAR** icon appears in your app menu and on the Desktop; click it and the studio opens in its own window (projects are saved in `~/CINVAR Projects`). Needs FFmpeg installed. Windows/macOS have no click-to-run packaging yet. In the studio: drag media in, chat with the assistant, preview/export, and customise theme, accent colour, layout and shortcuts under ⚙ (Ctrl+K opens the command palette).

## Try it in two minutes (no AI model, no network)

Needs [Rust](https://rustup.rs) and FFmpeg (`apt install ffmpeg` / `brew install ffmpeg` / `winget install Gyan.FFmpeg`).

    git clone <this repository> cinvar && cd cinvar
    ./examples/quickstart.sh

That builds CINVAR, generates synthetic footage, runs an edit with the offline *mock planner* (deterministic, not an
AI), renders a 1080p MP4 and verifies it with ffprobe. It uses a throwaway config directory and touches nothing else.
Then connect a real model or agent: [docs/MCP.md](docs/MCP.md), [MODEL_PROVIDERS.md](MODEL_PROVIDERS.md).

## Install

    cargo install --path .            # puts `cinvar` in ~/.cargo/bin
    cinvar setup --yes && cinvar doctor     # checks FFmpeg, encoders, Blender, fonts; writes a starter config

`scripts/install.sh` (Linux/macOS, tested on Linux) and `install.ps1` (Windows, untested) build from this checkout; no prebuilt binaries exist yet.
Detailed notes and optional dependencies: [INSTALLATION.md](INSTALLATION.md), [docs/GETTING_STARTED.md](docs/GETTING_STARTED.md).

## Use it

    cinvar project create --name MyVideo --dir ~/Videos
    cinvar models add local --provider ollama --model qwen2.5:7b      # or any provider; see MODEL_PROVIDERS.md
    cinvar run --media-root ~/footage "Combine the clips in ~/footage, add a title, export 1080p" --project ~/Videos/myvideo
    cinvar chat --project ~/Videos/myvideo                              # conversational editing
    cinvar mcp serve --project ~/Videos/myvideo                         # for any MCP client (stdio)
    cinvar ui --project ~/Videos/myvideo                                # local web UI on 127.0.0.1

### Connect an MCP client
    claude mcp add cinvar -- "$(which cinvar)" mcp serve --project ~/Videos/myvideo
Configs for Codex, Cursor and others, permission modes (default `edit`; cloud spend and plugin admin need `full`), the
project-only file access rule and the tool reference: [docs/MCP.md](docs/MCP.md), [docs/TOOLS.md](docs/TOOLS.md).

### Blender, 8K, colour, transcription (all optional)
| Want | Guide |
|---|---|
| Editing, FFmpeg export, audio, captions | [docs/RENDERING.md](docs/RENDERING.md) |
| 3D titles/animation (Blender 3.6+, tested on 5.3.0) | [docs/BLENDER.md](docs/BLENDER.md) |
| 4K/8K output and limits | [docs/EIGHT_K.md](docs/EIGHT_K.md) |
| OpenTimelineIO import/export | [docs/OPENTIMELINEIO.md](docs/OPENTIMELINEIO.md) |
| OpenColorIO / LUTs | [docs/OPENCOLORIO.md](docs/OPENCOLORIO.md) |
| faster-whisper subtitles, SRT/VTT | [docs/TRANSCRIPTION.md](docs/TRANSCRIPTION.md) |
| RunPod cloud rendering (experimental) | [docs/RUNPOD.md](docs/RUNPOD.md) |
| ComfyUI workflows (experimental) | [docs/COMFYUI.md](docs/COMFYUI.md) |
| Write a plugin | [docs/PLUGIN_GUIDE.md](docs/PLUGIN_GUIDE.md) (+ working example in `plugins/`) |

## Why it is built this way
- **Non-destructive projects** - versioned JSON, revision snapshots, undo/rollback; sources are copied, never modified.
- **~77 validated tools** shared by the CLI, agent loop and MCP server; invalid edits are refused, not silently fixed.
- **Real rendering and honest QA** - ffprobe-checked duration, resolution, audio, black frames, clipping.
- **Safe by default for agents** - project-confined writes, content-validated imports, no shell, sandboxed plugins (Linux),
  cloud spend gated behind a quote + explicit confirmation + your own opt-in.
- **Model- and provider-agnostic, hardware-adaptive** - CPU or hardware encoders with fallback; local rendering is never restricted.
- **Creative skills** - eleven editing disciplines loaded on demand (`skills/`).

## Command surface

    cinvar setup | doctor
    cinvar models list|add|use|test|key|remove
    cinvar project create|list|show|open|relink|history|rollback
    cinvar run "instruction" [--model P] [--dry-run] [--max-iterations N]
    cinvar chat [--model P]
    cinvar render [--preview] [--output FILE] [--width N] [--height N] [--crf N] [--range A-B] [--wait]
    cinvar status | qa FILE | jobs list|status
    cinvar media probe|import|frames|scenes
    cinvar skills list|show
    cinvar plugins list
    cinvar mcp serve | ui --port N | otio export --output FILE
    cinvar cloud connect|status|disconnect
    cinvar bench

Every command accepts `--json` for machine-readable output and `--project DIR`.

## Status: what is verified and what is not

TESTED = exercised by an automated test that runs the real thing. MOCK = tested only against a local fake of the service.
NOT TESTED = code exists, never executed. PLANNED = not implemented. Numbers come from `cargo test` on Linux x86_64
(278 passed, 0 failed, 8 ignored-by-default tests that need real dependencies - those 8 pass when the dependencies are installed).

| Area | Status |
|---|---|
| Project model, timelines, FFmpeg rendering, QA, durable jobs | TESTED (Linux) |
| MCP stdio server, permission modes | TESTED (Linux) |
| 4K and 8K FFmpeg export | TESTED (Linux) |
| Blender 5.3.0 rendering incl. native 8K | TESTED (Linux; needs Blender installed) |
| OpenTimelineIO export/import | TESTED against OTIO 0.18.1 |
| OpenColorIO LUT workflow | TESTED against OCIO 2.6.0 |
| faster-whisper transcription, SRT/VTT | TESTED against faster-whisper 1.2.1 |
| Plugin registry + Linux (bubblewrap) sandbox + example plugin | TESTED (Linux) |
| Model providers (OpenAI-compatible, Anthropic, Gemini, Ollama) | MOCK - wire formats unit-tested, no live vendor call in CI. Claude Code over MCP was driven for real |
| RunPod cloud rendering | MOCK - never run against a real account |
| ComfyUI | MOCK - never run against a real server |
| Windows, macOS | PARTIAL - CI builds and runs the suite on both (experimental, non-gating). macOS: all tests passed in the last run (quickstart script fixed after). Windows: ~6 text-rendering tests fail (FFmpeg drawtext/Fontconfig crash on the CI runner). No plugin sandbox on either |
| Independent security review | PENDING - self-review only, see [docs/SECURITY_REVIEW_PACKAGE.md](docs/SECURITY_REVIEW_PACKAGE.md) |
| Desktop GUI/installer, WhisperX, DeepFilterNet, AI upscaling, OpenFX, remote MCP | PLANNED |

Roadmap: [ROADMAP.md](ROADMAP.md). History: [CHANGELOG.md](CHANGELOG.md).

## Architecture in one paragraph

One Rust binary. A versioned JSON project document with integer-nanosecond times and exact
rational frame rates. A validated timeline engine. FFmpeg/ffprobe as the only media
dependency. A tool registry that both the agent loop and the MCP server speak. A provider
abstraction with honest capability profiles. A job manager for cancellable background
renders. SQLite for projects, sessions, jobs, usage and analysis cache. See
ARCHITECTURE.md for the module map and data flow.

## Repository map

    src/            Rust crate (see ARCHITECTURE.md)
    skills/         the creative knowledge library (Markdown + frontmatter)
    examples/       synthetic footage generator, demo script, MCP client, prompt library
    scripts/        install.sh, install.ps1
    web/            single-page local UI
    docs: README, ARCHITECTURE, INSTALLATION, MODEL_PROVIDERS, MCP_INTEGRATION,
          PLUGIN_DEVELOPMENT, CREATIVE_SKILLS, CONTRIBUTING, SECURITY, CODE_OF_CONDUCT,
          CINVAR_PROGRESS, CINVAR_TEST_RESULTS, CINVAR_RELEASE_STATUS

## Dependencies and licensing

CINVAR's own code is Apache-2.0 (see LICENSE). Direct Rust dependencies are permissively
licensed: anyhow, thiserror, serde, serde_json, toml, clap, rusqlite, ureq, sha2, rand,
regex, base64, tempfile, tiny_http (MIT/Apache-2.0 family; rusqlite bundles SQLite, which is
public domain). Cargo.lock pins the exact tree.

FFmpeg is **not** linked: CINVAR executes the installed ffmpeg/ffprobe binaries as external
programs. That keeps CINVAR's own licensing simple, but it matters if you redistribute
CINVAR together with FFmpeg binaries: many builds (including Ubuntu's) are GPL-enabled, so
the GPL obligations attach to that bundle, not to CINVAR's source. Do not ship a bundled
FFmpeg without satisfying them.

Blender is optional, GPL-licensed, and also invoked as an external program; CINVAR never
redistributes it.

## Repository map

    src/            Rust crate (module map: ARCHITECTURE.md)
    plugins/        example plugin + plugin test harness
    skills/         creative knowledge library (Markdown)
    examples/       quickstart.sh, synthetic footage, MCP client, full demo
    docs/           feature guides, tool reference, security review package, licence inventory
    scripts/        install scripts, packaging, doc generator
    demos/          scripts for the real-model demonstrations (outputs are regenerated, not stored)
    web/            single-page local UI

## Licensing

CINVAR's own code is **Apache-2.0** ([LICENSE](LICENSE)). Rust dependencies are permissively licensed
([docs/LICENSES.md](docs/LICENSES.md)). **That is not the whole ecosystem:** FFmpeg (often GPL-built), Blender (GPL),
ComfyUI (GPL) and model weights are separate programs/assets with their own licences. CINVAR runs them as external processes,
bundles none of them, and ships no model weights. If you redistribute CINVAR together with any of them, their terms apply to
that bundle. This is an inventory, not legal advice.

## Safety

Imported media is untrusted input: it is content-validated, copied into the project and passed to FFmpeg as argv arrays
(never a shell). Agents read and write only inside the project; footage elsewhere comes in through you (`cinvar media import`,
`cinvar connect --footage`) or a folder you allow (`--media-root DIR`, `CINVAR_MEDIA_ROOTS`, `security.media_roots`).
Billable cloud renders need an approval only a human can give (`cinvar cloud approve`). Credentials are redacted from output and never stored in projects. See [SECURITY.md](SECURITY.md) and the
known limitations in [docs/SECURITY_REVIEW_PACKAGE.md](docs/SECURITY_REVIEW_PACKAGE.md).

## Contributing

Welcome - see [CONTRIBUTING.md](CONTRIBUTING.md) and [ROADMAP.md](ROADMAP.md) (look for 🌱 good first issues). The one rule
that matters: a feature is done when a test produces a file that ffprobe agrees with, and anything unverified is labelled so.
The name CINVAR is provisional until trademark clearance completes.
