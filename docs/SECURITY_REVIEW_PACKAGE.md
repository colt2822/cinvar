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
- Abstract Unix sockets remain reachable by a plugin that declares `network: true` (shared network namespace).

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
