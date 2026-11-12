# CINVAR architecture

CINVAR is a lightweight, model-agnostic execution environment for AI-driven video
creation. The model supplies creative reasoning; CINVAR supplies the capabilities,
the guard rails and the evidence that something was really produced.

## Design principles

1. **The model is replaceable, the capabilities are not.** Every editing operation is
   a first-class, validated tool. Any provider that can call tools can drive it; a
   text-only model can still edit because all inspection is returned as text and JSON.
2. **Non-destructive by construction.** Source files are copied into the project and
   never modified. All edits are recorded in a versioned project document, with
   snapshots, undo/redo and rollback.
3. **Evidence over claims.** Rendering goes through FFmpeg and every output is probed
   with ffprobe. QA checks real numbers: duration, resolution, audio presence, black
   frames, loudness. The agent is instructed to run those checks before reporting.
4. **One runtime, three faces.** The CLI, the MCP server and the local web UI are thin
   clients over the same tool registry, project store and job manager.
5. **Lightweight core.** One Rust binary, one SQLite file, FFmpeg on PATH. No Docker,
   no Redis, no always-on server, no GPU requirement, no cloud account.

## Crate layout

    src/
      main.rs            process entry, exit codes
      lib.rs             module wiring, VERSION
      cli/               clap command surface (setup, doctor, models, project, run, chat,
                         render, status, jobs, media, skills, plugins, mcp serve, ui, otio, bench)
      config.rs          ~/.config/cinvar/config.toml: model profiles, budgets, security, render defaults
      secrets.rs         credential resolution + redaction (env, provider env, local store at 0600)
      error.rs           structured Error { code, message, hint } shared by every surface
      db.rs              SQLite: registered projects, sessions, job history, usage, analysis cache
      jobs.rs            background job manager (render/analysis) with progress + cancellation handles
      util/
        time.rs          integer-nanosecond Time, rational frame rates, frame<->time conversion
        proc.rs          hardened subprocess runner: argv only, env scrubbed, timeout, captured output
        paths.rs         config/data dirs, path normalisation, project-boundary enforcement
        clock.rs, ids.rs, text.rs
      project/
        model.rs         the canonical document: Project, Timeline, Track, Clip, Effect,
                         Keyframe, Transition, TextOverlay, CaptionTrack, RenderSettings
        store.rs         project directory layout, atomic save, revision snapshots, rollback,
                         asset import (copy + sha256 + probe), relink
        ops.rs           all timeline mutations with validation (overlap, bounds, non-zero length)
        history.rs       in-session undo/redo stack
      media/             ffprobe probe, frame extraction, contact sheets, scene/silence/black
                         detection, audio extraction, proxy generation
      audio/             loudness, clipping detection, silence cut plans
      render/            filtergraph construction, encoder selection, progress parsing,
                         cancellation, output validation
      qa/                deterministic render QA and project integrity checks
      tools/             the single source of truth for agent/MCP tools:
                         registry.rs semantics in mod.rs + groups.rs tool definitions
      agent/             system prompt construction, bounded tool-calling loop, edit plans,
                         session transcripts
      providers/         Provider trait + OpenAI-compatible, Anthropic, Gemini, Ollama and
                         offline mock adapters with capability profiles and retries
      mcp/               JSON-RPC 2.0 stdio MCP server (tools + resources)
      plugins/           plugin manifests, permission gating, subprocess tool execution
      skills/            dynamic creative-skill loader (Markdown + frontmatter)
      blender/           optional Blender bridge (script generation, headless render)
      otio/              OpenTimelineIO export with explicit warnings for unmapped features
      analysis/          highlight scoring from deterministic signals
      server/            local web UI (tiny_http) serving one HTML page and a JSON API
    skills/              the shipped creative knowledge library (ten skills + README)
    examples/            synthetic footage generator, demo script, MCP client, prompt library
    scripts/             bootstrap installers (install.sh, install.ps1)
    web/index.html       the single-page local UI

## Project format

A project is a directory:

    project.json         canonical, human-readable, diff-friendly JSON document
    assets/              imported media (copied, content-addressed prefix + original name)
    cache/               frames, contact sheets, proxies, Blender output
    renders/             rendered previews and exports
    revisions/           numbered snapshots written on every save (pruned to a limit)
    sessions/            agent transcripts (JSON)
    tmp/                 scratch space for in-flight work

`project.json` carries a "schema" version. Loading refuses a document newer than the
build understands instead of silently degrading it. Times are integer nanoseconds and
frame rates are exact rationals (30000/1001), so frame-accurate edits survive round trips;
the unit tests cover NTSC and 25 fps frame round-tripping.

Edits are expressed as operations on the document (insert, trim, split, move, speed,
freeze, reverse, transition, effect, keyframe, text, captions, audio). Every mutation
validates before it commits: clips may not overlap on a track, trims may not produce
zero-length clips, source ranges may not exceed the asset duration, keyframes must fall
inside the clip. Validation failures return a structured error with a hint, not a panic.

## Render pipeline

`render::plan_render` compiles the timeline into one FFmpeg invocation:

- one input per clip with explicit `-ss/-t` source ranges;
- per-stream normalisation to the canvas (scale + pad or crop, setsar=1, fps, yuv420p);
- clip effects as filters, overlays composited by track order, text overlays via drawtext,
  captions burned in with time-gated drawtext;
- audio per clip (atrim, gain, fades) mixed with amix then loudness-normalised;
- encoder selection with an explicit hardware-encode probe and CPU fallback;
- real progress from `-progress pipe:1` mapped to the job handle, and a cancel flag that
  kills the child process.

`render::validate_output` and `qa::validate_render` then probe the artefact. A render that
produced no file, no video stream, or the wrong duration fails loudly.

## Agent loop

`agent::AgentSession` holds the provider, the tool registry, the message history and the
usage counter. One turn is: build system prompt (role, rules, project digest, tool index,
installed skills) -> chat with tool schemas -> execute tool calls against the project ->
feed results back -> repeat until the model stops or the iteration/token/time budget is
exhausted. Repeated identical tool calls are interrupted with a corrective message.
Tool output is truncated before it enters the context, so a contact sheet listing or a
scene list cannot blow up the window. The transcript is persisted next to the project.

## MCP

`cinvar mcp serve --project <dir>` speaks newline-delimited JSON-RPC 2.0 on stdio:
initialize, tools/list, tools/call, resources/list, resources/read, ping, cancel. Tool
errors are returned as `isError` results with the structured error body. The project is
re-opened per call so an external agent and the CLI cannot silently overwrite each other.

## Security model

- No shell, ever: every subprocess is an argv array (see `util::proc`).
- Project boundary: asset and cache paths are resolved through `paths::ensure_within`;
  traversal attempts are rejected with `permission_denied`.
- Credentials: resolved from the environment first, then an owner-only credentials file;
  every error message and log line passes through redaction.
- Network egress is limited to the configured provider endpoint; uploaded media requires
  `security.allow_media_upload` to be enabled.
- Plugins declare their permissions; subprocess plugins are refused unless both the plugin
  manifest and the host configuration allow it, and their program path may not escape the
  plugin directory.
- Destructive tools are marked destructive in their schema so hosts can gate them.

## Performance

- Lazy work: media is probed once at import and cached in the project document.
- Analysis results and thumbnails live in `cache/`; previews render at 640px.
- Proxies are available for expensive footage; preview mode trades quality for speed.
- The core binary has no background daemon and starts in milliseconds (measured by
  `cinvar bench`, which reports real numbers rather than claims).
