# CINVAR security review package (for an INDEPENDENT reviewer)

Status: **self-review only. No independent review has been performed.** A Codex/GPT review interface
exists on the build host (`codex`) but is under a default-deny admission governor and was not used.
Nothing below is an approval. Reviewer: please treat every "fixed" item as a claim to re-verify.

Branch `ascension/p0` of `~/cinvar` (no remote). Rebuild: `cargo build --locked`.
Full suite: `cargo test` (see `~/CINVAR_ASCENSION_II_FINAL.md` for the counts at HEAD real-dependency tests). Strict lint: `cargo clippy --all-targets -- -D warnings`.

## Trust boundaries
1. **Agent (MCP client) -> tools.** Arguments are untrusted. `src/mcp/mod.rs` (dispatch), `src/mcp/compat.rs` (`Mode`, `level_of`: read_only/edit/full, unknown value = read-only, unknown tool = admin), `src/tools/groups.rs` (all tool bodies).
2. **Imported documents (.otio, subtitles, LUTs, workflow descriptors, plugin manifests) -> parsers.**
3. **Media bytes -> FFmpeg/ffprobe/Blender.** `src/media/mod.rs`, `src/render/graph.rs`, `src/blender/mod.rs`.
4. **Plugins -> subprocess.** `src/plugins/mod.rs`, `src/plugins/registry.rs`.
5. **Cloud.** `src/cloud/{runpod,orchestrator,worker,manifest}.rs`. Remote worker/pod output is untrusted.
6. **Secrets.** `src/secrets.rs` (0600 credentials file, `redact`).

## Fixed during this review (re-verify each)
| ID | Sev (self) | Finding | Fix | Regression test |
|----|-----------|---------|-----|-----------------|
| S-01 | High | `.otio` import followed any `target_url`, checked only the extension; renamed files and symlinks were copied into the project | `src/otio/mod.rs stage_reference`: canonicalize, root confinement, identity snapshot before open, single open, on Linux re-resolve the open fd via `/proc/self/fd` and re-check containment (closes the canonicalize->open race; on other platforms only a dev/inode check guards it and a swapped parent directory could fool it - NOT closed, NOT race-tested), copy from the open handle into `tmp/`, content probe, allow-listed container (`src/media/mod.rs:306,343`) | `tests/otio_security.rs` (12 tests: renamed text/ELF, truncated media, ffconcat playlist, symlink file/dir, `..`, absolute, missing, valid) |
| S-02 | High | `cinvar.otio.export` wrote JSON to any path the agent named (overwrite of arbitrary user files) | confined with `paths::ensure_within`, `.otio` extension required (`src/tools/groups.rs` `cinvar.otio.export`) | `tests/tool_path_security.rs::otio_export_cannot_overwrite_files_outside_the_project` |
| S-03 | Medium | `cinvar.media.import` / `import_folder` copied any file (including secrets named `*.mp4` or via symlink) into the project | content probe `probe_untrusted` before import | `tool_path_security.rs::media_import_copies_only_real_media`, `import_folder_skips_...` |
| S-04 | Medium | ffprobe could be steered by crafted playlists to open other protocols | `-protocol_whitelist file` in `media::probe` (`src/media/mod.rs:279`) | `otio_security.rs::ffconcat_playlist_cannot_smuggle_other_files` |
| S-05 | Medium | Plugin subprocesses ran with the user's full filesystem and network | bubblewrap wrapper `src/plugins/mod.rs:301 sandbox_command` (read-only root, empty `$HOME` AND empty CINVAR config dir so `credentials.toml` is invisible, project-only write, no network unless the manifest asks). Health checks run in the same sandbox. **Fails closed**: with no `bwrap`, plugin commands are refused unless `security.allow_unsandboxed_plugins = true`; `cinvar.plugins.list` reports `sandbox` and `plugin_commands_allowed` | `src/plugins/mod.rs` `sandbox_tests`, `tests/plugin_sandbox.rs` (need `bwrap`) |
| S-06 | Medium | Cloud confirm path returned `"billed": true` before any validation and ran preconditions asynchronously | synchronous refusal for missing acknowledgement / quote mismatch; honest `billed` wording (`cinvar.render.cloud`) | `tests/cloud_runpod_mock.rs::without_billing_risk_acknowledgement_nothing_is_provisioned`; demo step |
| S-07 | Medium | Duplicate-export race: a second identical export adopted a still-queued first job and started a second render | adopt only records with `attempts > 0` | `tests/ascension_8k.rs::duplicate_export_is_refused_and_job_survives_in_the_store` (was intermittently failing) |
| S-08 | Low | Relative `output` for `render.export` resolved against the process cwd instead of the project | resolved against the project root | demo steps (4K/8K export) |
| S-10 | Low | `lut` effect passed project paths containing quotes/commas through drawtext escaping; ffmpeg failed to parse (found by test) | LUT is staged to a content-named temp file and single-quoted (`src/render/graph.rs stage_lut_for_filter`) | `tests/color_ocio.rs::lut_path_with_spaces_commas_and_quotes_still_renders` |
| S-11 | Low | A job interrupted before it ever ran could never be re-run after `jobs.retry` (adoption keyed on attempts>0) | explicit `retried` flag | `src/jobstore.rs` test `retry_marks_a_never_started_job...` |
| S-09 | Info | Agent file tools were unrestricted | optional (**partially wrong as first written**: `project.relink` bypassed it, fixed as R-04)  `security.media_roots` confines `media.import/import_folder/inspect`, `qa.render_check`, `captions.import`, `color.import_lut`, `otio.import` (always confined) | `tool_path_security.rs::media_roots_confine_agent_file_tools_when_configured` |

## Open items (NOT fixed - decide before beta)
- **O-01 (Medium) No sandbox on Windows/macOS.** Plugin commands are refused there by default (fail-closed) until sandboxing exists or the user opts in. Beta blocker for third-party plugin support on those platforms; built-in integrations unaffected.
- ~~O-02~~ **RESOLVED**: agent file access is project-scoped by default (`within_agent_roots`); test `agents_are_project_scoped_by_default_and_roots_are_human_chosen`.
- **O-03 (Medium) Direct tool paths have a check-then-copy window** (`ProjectStore::import_asset`, `src/project/store.rs:226`). Only the OTIO path stages from an open handle.
- **O-04 (Medium) Untrusted FFmpeg/Blender attack surface.** Media decoders are inherently parsing untrusted bytes; keep FFmpeg patched. Blender runs a generated script with `allow_blender` on by default; script text is template-built with escaping (`src/blender/mod.rs` `escapes_python_strings`), no agent-supplied Python.
- **O-05 (Medium) Cloud worker image is an unaudited template** (`deploy/runpod/Dockerfile`); worker token travels in pod env and the RunPod proxy URL is HTTPS but the token is bearer-only. No live RunPod run has happened.
- **O-06 (resolved) `cargo audit`** (cargo-audit 0.23.0, advisory DB of 1453 entries, 2026-11-11) found RUSTSEC-2026-0459 (libsqlite3-sys 0.30.1, bundled SQLite, medium 6.8). Fixed by upgrading `rusqlite` 0.32 -> 0.42 (libsqlite3-sys 0.40.0); re-audit clean, full suite green. Python helper environments were not scanned (no `pip-audit` run).
- **O-07 (Low) MCP is stdio only.** No remote transport exists, therefore no remote attack surface; a remote transport must add auth, origin validation, TLS and resource limits first (not built).
- **O-08 (Info) Resource exhaustion.** Frame/size caps exist for renders (`src/render/capability.rs`), uploads (worker), imports (64 GiB staging cap), but there is no global per-session rate limit on MCP calls.

## Review round 2 (2026-11-11): fresh-context AI review (same model family - NOT an independent audit)
A read-only subagent with no access to the author's reasoning reviewed the code. It found real issues the self-review missed. The author verified and fixed each one below; none of this replaces an independent human/vendor review (still PENDING).

| ID | Sev | Finding | Fix | Regression test |
|---|---|---|---|---|
| R-01 | High | Plugin sandbox left host Unix sockets reachable (`/run/user/$UID/bus` = user systemd/D-Bus; docker/gpg sockets), a likely sandbox escape | `/run` is a tmpfs when the plugin has no network; with network, `/run/user`, `/run/dbus` are masked and docker/podman sockets bound to `/dev/null`; `--cap-drop ALL`, `--unshare-uts` | `plugin_sandbox.rs::host_control_sockets_under_run_are_not_reachable` |
| R-01b | High | `allow_plugin_subprocess` only checked at registration (manifest could omit `spawn_subprocess`); health checks ran for disabled plugins | enforced in `run_tool`; health checks only for enabled plugins | `plugin_sandbox.rs` (health not run when disabled) |
| R-02 | High | Local web UI had no Host/Origin/Content-Type checks (CSRF + DNS rebinding) and its embedded agent had no permission gate | `guard_request` (Host allow-list, Origin, JSON-only mutations); embedded agent obeys `CINVAR_MCP_PERMISSIONS` (default edit) | `ui_security.rs` |
| R-03 | High | Hostile ComfyUI server could choose a local file path through `prompt_id` | prompt_id must match `[A-Za-z0-9_-]{1,64}` | `comfy_mock.rs::a_hostile_prompt_id_cannot_choose_a_local_path` |
| R-04 | Medium | `project.relink` skipped `media_roots` and content validation; render then read the path (e.g. ANSI-art/tty detection rasterising a text secret) | `within_agent_roots` + `probe_untrusted` | `tool_path_security.rs::relink_*` |
| R-05 | Medium | Output writes followed symlinks planted in `cache/`, `luts/`, `assets/` | symlink leaf removed before writes (frames, contact sheet, proxy, LUT, asset copy) | `a_symlink_planted_in_cache_is_replaced_not_followed` |
| R-06 | Med-low | Empty `CINVAR_MCP_PERMISSIONS` meant `full` | empty means `edit` | `mcp_compat.rs` |
| R-07 | Med-low | `allow_media_upload` never enforced; ComfyUI file uploads could go to a remote server | non-loopback ComfyUI needs `allow_media_upload` | (code path; no dedicated test) |
| R-08 | Low | Apostrophe in title/caption text broke the whole render | correct two-level quoting | `graph.rs::escapes_drawtext_specials` (runs ffmpeg) |
| R-09 | Low | credentials file briefly world-readable | created with mode 0600 | - |

### Still open after round 2 (not fixed)
- ~~R-10~~ **RESOLVED**: human-controlled approval (`src/cloud/approval.rs`, `cinvar cloud approve`) - the agent chooses the image but the human sees it in the approval prompt; test `cloud_approval.rs::agent_cannot_self_approve_a_billable_render`. Remaining caveat: an agent with *shell* access to the same user account could still edit the config/approval directory; CINVAR's MCP agent has no shell.
- **R-11 (Low) Gemini API key sits in the request URL** and may appear in transport errors if a profile uses a custom key env name; send it as a header. Not fixed.
- **R-12 (Low)** predictable `/tmp` LUT staging path; `jobs.list` shows a worker bearer token in `remote_ref`; unbounded MCP `Content-Length`; relink is an existence oracle.
- **R-13 (unconfirmed)** possible cross-process double pod creation (in-process write guard only); render inputs lack `-protocol_whitelist file`.
- ~~Abstract Unix sockets remain reachable by a plugin that declares `network: true` (shared network namespace).~~ **FIXED in the DeepSeek hardening pass** - see the round-4 section at the end: `AF_UNIX` socket creation is denied by a seccomp filter while the host network namespace is kept for plugins that declare `network: true`.

## Review round 3 (2026-11-11): independent review by Codex (OpenAI Codex CLI 0.164.0, read-only, run on the owner's authorisation)
A different vendor's model reviewed branch `release/alpha-1` (after R-10/O-02 were fixed). Raw output summarised here; it found no critical issues.

| ID | Sev | Finding | Status |
|---|---|---|---|
| C-01 | High | Render wrote its predictable `.stem.partial.ext` file with ffmpeg `-y`, following a symlink planted in `renders/` (confirmed: overwrite of an external file) | **Fixed**: stale/linked partial removed before ffmpeg starts; cloud partial likewise. Test `render_partial_file_symlink_is_not_followed` (verified to fail without the fix) |
| C-02 | Medium | Check-then-open race in `within_agent_roots` (canonicalise, then use the original path) | **Mitigated**: import/inspect now use the resolved path. **Residual**: a component swapped after resolution is still possible for a local attacker with write access (project-writable plugin); OTIO staging uses the stronger open-once pattern, direct tools do not (O-03) |
| C-03 | Low | Gemini API key in URL query string | **Fixed**: sent as `x-goog-api-key` header |
| - | - | Cloud approval reuse/forgery, MCP alias/permission bypass, filtergraph/Python injection, web UI guard bypass | none found by the reviewer |

Not covered by this review: Windows/macOS, third-party tools themselves, a full audit of every module. It is a *focused* review, not a certification.

## Reproduction
```
cargo test --test otio_security --test tool_path_security --test cloud_runpod_mock --test mcp_compat
cargo test --lib sandbox            # needs bubblewrap
python3 examples/ascension_demo.py target/debug/cinvar /tmp/w   # hostile import refused, cloud refusals
```
Manual probes for a reviewer: place `evil.otio` (see `tests/otio_security.rs::otio_for`) beside a symlink to `~/.ssh/id_ed25519`; run `cinvar mcp serve` and call `cinvar.otio.import`.

## Review round 4 (2026-11-12): plugin sandbox - abstract Unix sockets (closes the last line of "Still open after round 2")

**Status: fixed and regression-tested on Linux x86_64 (bubblewrap 0.9.0). Not independently reviewed.**
This entry supersedes the final line of *Still open after round 2* ("Abstract Unix sockets remain reachable by a plugin
that declares `network: true`"); that line is left in place as history.

| ID | Sev | Finding | Fix | Regression test |
|---|---|---|---|---|
| R-14 | High | A manifest declaring `network: true` shared the **host** network namespace. Masking `/run`, `/run/user`, `/run/dbus` and the docker/podman sockets hides *files*, but abstract-namespace Unix sockets live in the network namespace: a plugin could `connect()` to `@dbus-*`, `@systemd/*`, docker and gpg-agent endpoints owned by host processes. That is host control, not network egress. | The host namespace stays (egress is the permission the manifest asked for) and a **raw-BPF seccomp filter** is installed by `bwrap --seccomp`: `socket()` returns EPERM for `AF_UNIX`, while `AF_UNSPEC`/`AF_INET`/`AF_INET6`/`AF_NETLINK` are allowed and `socketpair()` is untouched. The program (`src/plugins/seccomp.rs`) checks `seccomp_data.arch` first and kills on mismatch, so a 32-bit compat entry point cannot reach `socket()` under a different number. If no filter can be installed (architecture outside x86_64/aarch64, or an unwritable config directory) the sandbox **fails closed** to `--unshare-net`. Also in this round: `plugin_root`/`writable` are canonicalised before becoming binds and `/` is never a bind target; a writable project that is or contains the CINVAR config directory drops the bind instead of re-mounting `credentials.toml` over the tmpfs that hides it. | `tests/plugin_sandbox.rs::host_abstract_unix_sockets_are_not_reachable_from_a_network_plugin` (binds an abstract socket in the test process; **fails on the pre-fix code with `ABSTRACT_OK`**, passes after), `sandbox_argv_binds_only_canonical_dirs_and_keeps_host_netns_guarded`, `src/plugins/seccomp.rs` unit test |

### Verified in this round (each with the command that proves it)

| Item | Result |
|---|---|
| Abstract Unix socket owned by a host process | Unreachable: `socket(AF_UNIX)` -> EPERM (errno 1) inside the sandbox. |
| Legitimate egress preserved | Against listeners in the test process: TCP `127.0.0.1` connect, UDP `AF_INET`, `AF_INET6`, and `socketpair(AF_UNIX)` all still work inside the same sandbox. The DNS path (`AF_NETLINK`) is allow-listed. |
| `--unshare-pid` + `--proc /proc` | `/proc` inside the sandbox lists 4 entries against 504 on the host; the host PID is neither listed nor readable. Now covered by `sandboxed_proc_shows_no_host_processes`. |
| `credentials.toml` invisibility | Unchanged and still covered (`credentials_outside_home_and_health_checks_are_sandboxed`): config dir and `$HOME` are tmpfs, and the new writable-bind filter also blocks re-exposing them through a project path. |
| Manifest-controlled host binds | None: the only binds are `/`, `/dev`, `/proc`, the plugin directory (read-only) and the project, all canonicalised existing directories; `/` is refused as a bind target. `permissions.write_paths` is disclosed by `plugins list` but never mounted. |
| `--unshare-net` / `/run` masking for `network: false` | Unchanged. |
| Resource limits | **Residual (R-15 below)**: bubblewrap 0.9.0 offers no `--rlimit-nproc`/`--rlimit-as` (checked against `bwrap --help`), so the only enforcement is the per-tool timeout (`SIGKILL`). Not added: it needs a per-plugin budget decision. |

### Residual risks after round 4 (not fixed; decide before beta)
- **R-15 (Medium) No per-plugin resource limits.** bwrap 0.9.0 has no rlimit options, so a plugin can still fork-bomb or
  exhaust memory inside its tool timeout. A per-plugin or global budget (`RLIMIT_NPROC`, a cgroup) is needed; imposing
  `RLIMIT_AS` blindly would break the FFmpeg/Blender workloads plugins exist for.
- **R-16 (Medium) `network: true` still reaches host loopback TCP/UDP services.** The filter removes Unix sockets only, so
  a plugin can connect to `127.0.0.1:<port>` services of the user's session (and the LAN). That is the blast radius the
  permission declares, but it is *not* isolation; a host-service allowlist or a proxy would be the next step.
- **R-17 (Low) A plugin that legitimately needs a local daemon Unix socket no longer works with `network: true`** (its
  `socket(AF_UNIX)` call fails). Intentional: the alternative was host control. Documented in `docs/PLUGIN_GUIDE.md`.
- **R-18 (Low) Architectures other than x86_64/aarch64 get no network** when a manifest asks for it (fail-closed to
  `--unshare-net`), because no filter program can be built there. A functional difference, not a security one.
- **R-19 (Low) The seccomp program reaches bubblewrap through a `/bin/sh` shim** (`exec 3<"$0"; exec "$@"`), because
  `proc::run` cannot set up an inherited descriptor and `bwrap --seccomp` only reads one. The shim's `$0` is the filter
  path and `$@` is bubblewrap's own argv; no shell expansion touches the plugin's arguments. If the shim cannot open
  the program, nothing runs.

### Reproduce round 4
```
cargo test --test plugin_sandbox            # needs bwrap + python3
cargo test --lib seccomp                    # filter program shape
```
Manual probe: bind an abstract socket in one process (`python3 -c 'import socket;s=socket.socket(socket.AF_UNIX);s.bind(chr(0)+"probe");s.listen()'`),
then run a plugin command whose manifest has `{"permissions": {"network": true}}`: before the fix it connects, after it sees EPERM.
---

## Round 4 (DeepSeek hardening pass) - Linux plugin sandbox: abstract Unix sockets

Appended by the hardening pass; the authoritative description of the code change lives in
`docs/DEEPSEEK_HARDENING_REVIEW.md` (F-3). Summary so this package stays self-consistent:

| ID | Sev | Finding | Fix | Regression test |
|----|-----|---------|-----|-----------------|
| F-3 | High | A plugin declaring `network: true` kept the **host** network namespace, so it could `connect()` to host abstract Unix sockets (`@dbus-*`, `@systemd/*`, docker, gpg-agent). Masking paths under `/run` cannot hide those: they live in the network namespace. | `src/plugins/seccomp.rs` (new): a raw-BPF program installed with `bwrap --seccomp`. It validates `seccomp_data.arch` (kills on mismatch, so an `int 0x80` path cannot reach the syscall by another number), then returns `EPERM` for `socket(AF_UNIX, ...)` while allowing `AF_UNSPEC`/`AF_INET`/`AF_INET6`/`AF_NETLINK`. The host namespace is kept, so egress still works. Fail-closed: no usable filter (unknown architecture, unwritable config dir, no `sh`) means `--unshare-net`, never an unfiltered sandbox. | `tests/plugin_sandbox.rs::host_abstract_unix_sockets_are_not_reachable_from_a_network_plugin` - binds a real host abstract socket, connects from a sandboxed child, and also asserts UDP/IPv6/TCP and `socketpair` still work. Fails on the pre-fix argv (verified). |
| F-4 | Medium | The bwrap argv bound `plugin_root`/`writable` without resolving them, so a symlinked plugin directory - or a project that is (or contains) the CINVAR config directory - could move a bind or remount `credentials.toml` over the tmpfs that hides it. | binds go through `bind_dir` (canonicalise, must be an existing directory, never `/`); a writable path that contains the config directory loses its writable bind (a plugin that cannot write is a plugin error; leaked keys are not) | `plugin_sandbox.rs::sandbox_argv_binds_only_canonical_dirs_and_keeps_host_netns_guarded`, `sandboxed_proc_shows_no_host_processes` |

Verified in this pass (were already correct, now covered): `/proc` shows no host processes under
`--unshare-pid`; `credentials.toml` is invisible (empty `$HOME` and empty config dir).

Residuals added by this round (see the handoff doc for the full list): **R-15** no per-plugin
rlimits (bwrap 0.9.0 has no `--rlimit-nproc`/`--rlimit-as`), **R-16** `network: true` still reaches
host loopback TCP/UDP services - that is the blast radius the permission declares, it is not
isolation, **R-17** a plugin that needs a *local daemon Unix socket* no longer works with
`network: true` (intentional), **R-18** architectures other than x86_64/aarch64 get no network at
all and a 32-bit compat syscall is killed rather than denied, **R-19** the `/bin/sh` fd shim exists
only because `proc::run` cannot pass an inherited descriptor to bwrap.


## Adversarial verification (2026-11-12): the agent-facing boundaries attacked, not re-read

**Status: adversarial verification pass. Two genuine holes were found and fixed; the rest of the
claims below held under attack. There is still no independent human/vendor audit.**

Scope: the MCP permission model (`src/mcp/compat.rs`), cloud spend approval
(`src/cloud/approval.rs`), the web UI guard (`src/server/mod.rs`), secret redaction
(`src/secrets.rs`) and path handling for untrusted external responses. Every attack below is a
test in `tests/mcp_adversarial.rs` (6 tests, unix-gated, real stdio MCP server / real UI / real
ComfyUI client).

| # | Attack | Verdict |
|---|---|---|
| A-1 | Every non-read tool from `tools/list` (50: Edit, Spend and Admin), called in a **read-only** session under 5 base name forms (native, dotted-safe, UPPER CASE, trailing space, `mcp__cinvar__` prefix) plus the logical alias and alias-safe form for each of the 8 with an alias | **Blocked**: every call answered `permission_denied` or `unknown tool`; `tools/list` did not expose one write tool. No alias, case, whitespace or prefix bypass exists - resolution happens before classification. |
| A-2 | Permission env edge cases: empty, `garbage`, `FULL_MODE`, `Read_Only`, `read-only`, `full ` (padded) | **As documented**: empty = edit (no spend, no admin), unknown/padded-cased values = read-only, `full ` trims to full. An empty variable never widens the session. |
| A-3 | Spend/Admin tools (`render.cloud`, `cloud.cleanup`, `plugins.set_enabled`/`pin`) from an **edit** session | **Blocked**: spend and administration stay closed above edit. |
| A-4 | The agent tries to mint its own approval through 8 other tools, each aimed at `<config>/cloud/approved/<quote-id>`: `otio.export` (absolute and relative), `captions.export`, `render.export`, `media.import` of the pending record, `plugins.set_enabled`/`pin` with a traversal id, `jobs.retry` | **Blocked**: no approval record appeared. Path confinement and known-id checks hold; no MCP tool writes anywhere in the config directory except a fixed plugin-state file. |
| A-5 | Confirm a quote while echoing the id and `acknowledge_billing_risk`, with no human approval | **Blocked synchronously**: `permission_denied`, no `job_id`, `billed` never true, nothing provisioned. |
| A-6 | Replay the same approval for a second render | **Blocked**: single use (the record is removed by the first consumer). |
| A-7 | An approval older than the 15-minute TTL | **Blocked**: "expired", and the record is consumed so it can never become usable. |
| A-8 | Drive a **configured secret** through a tool error: a key held in a custom `api_key_env` (model profile) and `CINVAR_RUNPOD_API_KEY` | **GENUINE HOLE (fixed)**: the runpod key was redacted, the profile-named key was echoed verbatim - `redact` only knew a fixed env-name list. It now also reads the names the configured model profiles point at. |
| A-9 | Web UI: rebound `Host`, `Host` without a port, foreign `Origin`, simple POST (`text/plain`, `application/x-www-form-urlencoded`, `multipart/form-data`) | **Blocked** (403) - re-verified, no change needed. |
| A-10 | Web UI: path traversal in the media route (4 encodings and an absolute form) | **Containment held, defect fixed**: no `project.json` and no `/etc/passwd` were disclosed, but the refusal surfaced as a **bodiless 500 and a dropped connection**. It is now an explicit 404 JSON, so a traversal is indistinguishable from a miss. |
| A-11 | Web UI: a 2 MiB JSON body | **GENUINE HOLE (fixed)**: the body was buffered in memory with no limit (R-12 listed only the *MCP* `Content-Length`). A declared length above 1 MiB is now refused with 413 before any byte is read, and the reader is capped too so a chunked body cannot bypass it. |
| A-12 | Web UI reachable from a non-loopback address | **Blocked**: binds `127.0.0.1` only; a connect to the host's LAN address is refused. |
| A-13 | A hostile ComfyUI response carrying a path in an output filename (hostile `prompt_id` is already covered by `comfy_mock.rs`) | **Blocked**: only the final path component is used, so the file lands inside the cache directory; nothing outside is created. |

### Fixes in this pass
| ID | File | Fix |
|---|---|---|
| D-01 | `src/secrets.rs` | `redact()` also covers every env name a configured model profile names (`api_key_env`), not only the built-in list. |
| D-02 | `src/server/mod.rs` | `MAX_JSON_BODY` (1 MiB): a `Content-Length` above the cap is refused with 413 before the body is read, and the reader is capped for chunked bodies. `guard_request` now returns a status, so the refusal code is honest instead of always 403. |
| D-03 | `src/server/mod.rs` | A media-route escape attempt answers 404 JSON instead of erroring out of the handler into a bodiless 500. |

### Guard-break proof (a test that fails when the guard is removed)
Each guard was temporarily removed, the failure captured, then restored and the suite re-run green.
```
# 1. redact() limited to the built-in env names again -> the custom-named key is echoed verbatim:
cargo test --test mcp_adversarial configured_secrets
#   panicked: profile api_key_env leaked through an MCP error:
#   "refusing to write outside the project: /tmp/pk_live_0123456789abcdefghijklmnop/leak.otio"
# 2. the media refusal removed -> traversal becomes a dropped connection:
#   panicked: traversal /api/media/../../project.json answered 500, expected an explicit refusal
# 3. the size cap removed -> a 2 MiB body is buffered and parsed:
#   panicked: a 2 MiB JSON body must be refused, not buffered (left: 400, right: 413)
```

### Repro
```
cargo test --test mcp_adversarial
```

### Residual risks found by this pass (not fixed here)
- **R-20 (Low) Unbounded MCP `Content-Length`** (already R-12): `src/mcp/mod.rs:381` allocates the
  declared length up front (`vec![0u8; length]`), so a hostile client can force a large allocation.
  It is outside this pass's boundary and is the same class as D-02; the fix is the same shape (cap
  the length and answer a JSON error).
- **R-21 (Low) Other UI handler errors still answer a bodiless 500.** The media route was fixed
  because it is attacker-reachable through a traversal; the remaining ones (for example
  `POST /api/open` with a nonexistent directory) disclose nothing and mutate nothing, but an
  honest JSON error needs a dispatch refactor that turns every route's `Result` into a response.
- **R-22 (Low) A foreign page can still trigger a no-cors GET** such as `/api/doctor`: its `Host` is
  ours and a no-cors GET carries no `Origin`. The page cannot read the answer and the handler only
  probes binaries, so this is a resource cost, not a disclosure. A per-session token in the served
  page would close it - a product decision.
- **R-23 (Info) No rate limit on the MCP surface** (O-08). 266 denied calls in one session were cheap,
  but no cap exists.
