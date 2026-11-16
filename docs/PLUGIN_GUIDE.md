# Plugin developer guide

Plugins add tools to CINVAR without touching the Rust engine. You do **not** need ELYREON's approval to write or
distribute a plugin. A working example lives in [`plugins/project-stats/`](../plugins/project-stats); copy it.

    plugins/project-stats/
      plugin.json            # manifest
      bin/project-stats      # the tool (any language; reads JSON on stdin)

## Try the example (30 seconds)

    cargo build
    python3 plugins/test_plugin.py plugins/project-stats        # needs bubblewrap on Linux

The script creates a throwaway config + project, installs the plugin, enables it, calls each tool through the real
MCP server and checks it ran **inside the sandbox**. Use it for your own plugin: `python3 plugins/test_plugin.py path/to/yours`.

## Installing and enabling (users)
1. Copy the plugin directory to `<config dir>/plugins/<name>/` (`~/.config/cinvar/plugins/` on Linux; override with
   `CINVAR_CONFIG_DIR`). CINVAR **never runs install scripts**; `install` in the manifest is human-readable text.
2. Plugins start **disabled**. Enable: MCP `cinvar.plugins.set_enabled {id, enabled:true}` (needs a `full` MCP session)
   or edit `<config dir>/plugins-state.json`.
3. To run any plugin command you must also set `security.allow_plugin_subprocess = true` in `config.toml`.
4. `cinvar plugins list` / MCP `cinvar.plugins.list` show health, compatibility, missing dependencies and disclosures.

## Manifest (`plugin.json`, schema_version 1)

| Field | Req | Meaning |
|---|---|---|
| `name`, `version`, `description` | yes | tools appear as `plugin.<name>.<tool>` |
| `cinvar_min` | yes | minimum CINVAR version; newer-required plugins are skipped with a message. `cinvar_max` optional |
| `id` | no | stable reverse-DNS id (defaults to `name`) used for enable/pin |
| `capabilities` | no | free-form tags, e.g. `video.effects`, `analysis.project`, `export.format`, `render.backend`, `ai.enhance` |
| `platforms` | no | `linux`, `macos`, `windows`; others skip the plugin. Empty = all |
| `requires` | no | programs that must be on PATH (reported by `plugins list`) |
| `permissions` | no | `spawn_subprocess` (must be true to run anything), `network` (default false; **egress only** - see the security model), `write_paths` (declared and disclosed to users; the sandbox still grants write access to the project only) |
| `inputs`, `outputs`, `hardware` | no | documentation for users |
| `costs_money`, `uses_cloud`, `sends_files_off_device` | no | **disclose honestly**; shown to users before enabling |
| `health` | no | argv run (sandboxed, short timeout) to report availability |
| `license`, `install` | no | SPDX id; human-readable install steps |
| `tools[]` | yes | `name`, `description`, `input` (JSON Schema), `command` (argv), `timeout_secs` |

## Tool contract
- `command` is an argv array run **without a shell**. `command[0]` is a bare name found on PATH or a path inside the plugin
  directory (no absolute paths, no `..`).
- **Input**: the tool arguments as one JSON object on stdin, then EOF. **cwd**: the project root.
- **Output**: whatever you print to stdout (JSON recommended; ≤1 MiB; secrets are redacted). CINVAR returns
  `{"sandboxed": bool, "stdout", "stderr", "exit_code"}`.
- **Errors**: exit non-zero and write a one-line explanation to stderr; it becomes an error result. Do not rely on retries.
- **Timeout**: killed after `timeout_secs`.
- **Environment**: cleared except PATH, HOME (an empty tmpfs inside the sandbox), temp and locale. No credentials are passed.
- Writing files: only inside the project. Write results under `cache/` or `tmp/`, return the relative path, and let the
  agent import it with `cinvar.media.import`.

## Security model (read before publishing a plugin)
- On Linux every command runs under **bubblewrap**: read-only system, empty `$HOME`, CINVAR config/credentials hidden,
  project writable, plugin dir read-only, **no network unless `permissions.network` is true**, new PID/IPC namespaces.
  Only `/proc` of the plugin's own PID namespace is visible, so host processes cannot be listed, read or signalled.
- `permissions.network: true` buys **network egress, never control of host services**. The sandbox keeps the host network
  namespace (so a local ComfyUI, a render box or DNS stays reachable) and installs a seccomp filter that makes
  `socket(AF_UNIX, ...)` fail with EPERM. A plugin therefore **cannot talk to any host Unix socket**: not the D-Bus /
  systemd sockets under `/run` (masked anyway) and not the *abstract* ones (`@dbus-*`, `@systemd/*`, docker,
  gpg-agent), which no path masking can hide because they live in the network namespace. `AF_INET`, `AF_INET6` and
  `AF_NETLINK` (DNS) keep working, and `socketpair()` is untouched, so plugins that talk to their own threads still work.
  Design plugins so they do not need a local daemon socket.
- Where no filter can be installed (architectures other than x86_64 and aarch64, or an unwritable CINVAR config directory)
  the sandbox **fails closed** to an isolated, empty network namespace: a `network: true` plugin then has no network at
  all. Treat network as optional in your plugin and report a clear error when a connection fails.
- Without bubblewrap, commands are **refused** unless the user sets `security.allow_unsandboxed_plugins = true`
  (then the plugin runs with the user's full privileges). macOS and Windows have **no sandbox yet** - treat plugins there as
  running as you. Do not ship plugins that assume otherwise.
- A plugin that crashes or times out produces an error result; it cannot break core editing or other plugins.
- Anything an AI agent passes in `input` is untrusted: validate it, never build shell strings from it, never `eval` it.
- Network, cost and off-device disclosures are your responsibility; users rely on them.

## Compatibility
Declare `cinvar_min`/`cinvar_max`. Manifest `schema_version` is 1; additive fields will not break v1 manifests.
`cinvar.plugins.pin` lets a user pin a known-good version.

## Ideas that fit this contract
Video effects and transitions, AI enhancement (wrap a local model or ComfyUI), transcription backends, colour tools,
Blender helpers, export formats, render backends, cloud adapters, workflow automation. Planned but unwritten: AI
upscaling, OpenFX, cloud storage (contract-only entries appear in `plugins list`). See ROADMAP.md.
