# MCP integration

Status: **stdio only** (local process, no network listener). Remote/HTTP transport is not built and will not be
enabled by default. Full tool list: [TOOLS.md](TOOLS.md).

CINVAR ships its own MCP server, so any MCP-capable agent can edit video without you
installing a pile of community servers. The transport is newline-delimited JSON-RPC 2.0 on
stdio, the standard local transport for MCP hosts.

## Start the server

    cinvar mcp serve --project /path/to/project

With no `--project`, project tools return a corrective error telling the caller to pass one.
Diagnostics go to stderr; stdout carries protocol messages only.

## Fastest path: Claude Code

    cinvar connect claude --project ./my-video --footage ~/Videos/shoot-1
    cd my-video && claude

`cinvar connect claude` creates the project (if needed), imports the footage folder, and writes a
project-scoped `.mcp.json` plus a short `CLAUDE.md` into the project folder. Nothing global is
changed; Claude Code asks you to approve the `cinvar` server the first time. Pass `--apply` to also
run `claude mcp add` for that folder. You never configure individual tools: the server sends a
workflow guide in its `initialize` response (`instructions`) so the model knows the order of work -
look at footage, load a creative skill, build, preview, inspect frames, export, revise with rollback.

Every tool call is appended to `<project>/sessions/mcp-calls.jsonl` (tool, ok, milliseconds, error
code; never arguments) so you can see how a model used the tools. Concurrent processes are
serialised with a project lock, and a stale save is refused rather than overwriting newer work.

## Connect a client

Replace `/abs/path/to/cinvar` and the project path. No secrets belong in these files. **Verification status:** only
Claude Code has been driven end to end (see `demos/`); the other snippets follow each client's documented
format but have **not been tested** against the real client - tell us if one needs a fix.

**Claude Code** (verified) - `cinvar connect claude --project ./my-video`, or by hand:

    claude mcp add cinvar -- /abs/path/to/cinvar mcp serve --project /abs/path/to/my-video

**Codex CLI** (`~/.codex/config.toml`, not verified):

    [mcp_servers.cinvar]
    command = "/abs/path/to/cinvar"
    args = ["mcp", "serve", "--project", "/abs/path/to/my-video"]

**Cursor** (`.cursor/mcp.json`, not verified) / **Claude Desktop** / most JSON-config clients:

    { "mcpServers": { "cinvar": {
        "command": "/abs/path/to/cinvar",
        "args": ["mcp", "serve", "--project", "/abs/path/to/my-video"],
        "env": { "CINVAR_MCP_PERMISSIONS": "edit" } } } }

**Gemini CLI** (`~/.gemini/settings.json`, not verified): same `mcpServers` object as above.

**Local agents / scripts**: speak newline-delimited JSON-RPC on stdin/stdout; `examples/mcp_client.py` is a
dependency-free reference client. Some clients reject dots in tool names: set `CINVAR_MCP_NAMES=safe` to advertise
`cinvar_media_import` style names (every form is accepted on input).

## Permissions

`CINVAR_MCP_PERMISSIONS` (set by whoever launches the server, not by the agent):

| Mode | Allows |
|---|---|
| `read_only` | inspect media/projects, QA, status, list jobs. Mutating tools are hidden and refused |
| `edit` (**default**) | everything above + editing, rendering, importing, Blender, ComfyUI jobs, OTIO/captions export |
| `full` | + cloud rendering/cleanup (still needs `security.allow_cloud_render` and a confirmed quote) and plugin administration/execution (still needs `security.allow_plugin_subprocess` and a sandbox) |

An unrecognised value means `read_only`. Config-level switches live in `<config dir>/config.toml` under `[security]`.
**File access (project-scoped by default).** Agents can read and write only inside the project folder. Footage elsewhere
is brought in by *you*: `cinvar media import FILE`, `cinvar connect claude --footage DIR`, or by allowing a folder when you
launch the server - `cinvar --media-root ~/Videos mcp serve`, `CINVAR_MEDIA_ROOTS=~/Videos` (a PATH-style list) or
`security.media_roots` in `config.toml`. The agent cannot change these. Symlinks are resolved before the check.

**Cloud spending needs a human.** `cinvar.render.cloud` first returns a quote and records it as pending. Rendering starts
only after *you* run `cinvar cloud approve <quote-id>` in a terminal (it shows the image, GPU price ceiling, runtime and
worst-case cost and requires typing `APPROVE`; approvals are single-use and expire after 15 minutes). The agent echoing the
quote id or `acknowledge_billing_risk` is not an authorization.

## What an agent can do

Inspect media (`cinvar.media.*`), build and edit timelines (`cinvar.timeline.*`), add captions (`cinvar.captions.*`),
apply effects/colour/audio, create Blender jobs (`cinvar.blender.animation`), export (`cinvar.render.export`), check
results (`cinvar.qa.render_check`), and cancel/retry jobs (`cinvar.render.cancel`, `cinvar.jobs.retry`).

## Protocol surface

    initialize                 -> protocolVersion 2024-11-05, serverInfo {name, version}
    notifications/initialized  -> accepted silently
    ping                       -> {}
    tools/list                 -> every registered tool with a strict JSON Schema
    tools/call                 -> content blocks + isError
    resources/list             -> cinvar://project/status, cinvar://project/json
    resources/read             -> the project status document or the raw project.json
    $/cancelRequest            -> acknowledged
    unknown method             -> JSON-RPC error -32601
    malformed JSON line        -> JSON-RPC error -32700, the session continues

## Tool naming

Tools are named `cinvar.<group>.<verb>`:

    cinvar.project.status        cinvar.media.import           cinvar.timeline.append_clip
    cinvar.media.import_folder   cinvar.media.scenes           cinvar.timeline.split
    cinvar.timeline.trim         cinvar.timeline.text          cinvar.timeline.captions
    cinvar.audio.analyze         cinvar.render.export          cinvar.render.status
    cinvar.render.cancel         cinvar.qa.render_check        cinvar.skills.load
    cinvar.blender.status        cinvar.otio.export            cinvar.system.capabilities

Call `tools/list` for the authoritative list and schemas.

## Errors

A failed tool returns an MCP result with `isError: true` whose text is the structured error:

    {"error": {"code": "invalid_input", "message": "...", "hint": "..."}}

Codes: not_found, invalid_input, unsupported, permission_denied, dependency_missing,
io_error, provider_error, protocol_error, render_failed, cancelled, budget_exceeded,
not_implemented, conflict, internal_error. Hints are actionable sentences; surface them
instead of retrying blindly.

## Asynchronous rendering

`cinvar.render.export` returns a job handle immediately:

    {"job_id": "job_1a2b3c4d", "output": "/project/renders/my-video-r3.mp4"}

Poll `cinvar.render.status` with that handle. The response carries status (queued, running,
succeeded, failed, cancelled), progress (0-1), message and, on success, the render outcome
(path, frames, duration, elapsed seconds, encoder). `cinvar.render.cancel` stops a job.

## Safety

- No shell: tools never concatenate a command line.
- Project boundary: file paths resolve inside the project directory; escapes are denied.
- A project opened by the MCP server is re-read per call, so external edits are not lost.
