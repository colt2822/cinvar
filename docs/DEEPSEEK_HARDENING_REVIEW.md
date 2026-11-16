# DeepSeek hardening pass - handoff for an independent reviewer

Status: **implementation-focused self-testing only. No independent review has been performed on
this pass, and none of it is an approval.** Every "fixed" line below is a claim to re-verify, and
each one names the test that is supposed to catch a regression. This document is a delta on top of
`docs/SECURITY_REVIEW_PACKAGE.md`; read that first for the trust boundaries and the earlier rounds.

Branch: `feature/deepseek-hardening-showcase` of `~/cinvar` (local only, not pushed).
Baseline commit: `aeb6e69` is the published v0.1.0-alpha; development started from `4f4e7d7`.
Rebuild: `cargo build --locked`. Suites: `cargo test --all`, `cargo clippy --all-targets -- -D warnings`.

## What this pass changed, and why

### F-1 (High) An import hashed one file and copied another - the check/use window was real

`ProjectStore::import_asset` used to be:

1. `std::fs::metadata(source)` for the recorded size,
2. `sha256_file(source)` - open #1,
3. dedupe against existing assets by that hash,
4. `std::fs::copy(source, dest)` - open #2.

Everything CINVAR later trusts about an asset (its identity for dedupe, its provenance record, its
recorded size) came from step 2 while the bytes actually stored came from step 4. A name repointed
between them - a symlink swapped in, a file rewritten in place, a directory replaced - made the
record describe different bytes than the project holds. That is exactly the "check a path, then
reopen it by name" pattern this pass was asked to remove, and it was reachable from
`cinvar.media.import` (the agent tool) as well as the CLI.

**Reproduction, before the fix** (`tests/fs_race_security.rs::the_recorded_hash_and_size_always_describe_the_stored_bytes`,
attacker thread alternating the source name between two hard links with different content and
length):

```
the stored bytes are not the bytes that were hashed (5 of 8 imports):
["recorded sha 887ce002f120 / 307200 bytes, stored sha 068e3f90a34c / 716800 bytes", ...]
```

**Fix.** A new module `src/util/fshandle.rs` makes the boundary decision and the open one kernel
operation:

- `Dir` opens a root once and keeps the descriptor. `Dir::open_file` resolves the candidate
  relative to that descriptor with `openat2(2)` and `RESOLVE_BENEATH | RESOLVE_NO_MAGICLINKS`, so
  any resolution that would leave the root (through `..`, an absolute path, or a symlink planted
  inside) fails instead of opening something else. Where `openat2` is unavailable (older kernels,
  a seccomp policy) it falls back to a component-by-component `openat(2)` walk with `O_NOFOLLOW`,
  which cannot be redirected either (it is stricter: it refuses every symlink).
- `stage_into` copies and hashes **in one pass from that single descriptor**, into a temporary
  file created with `O_CREAT|O_EXCL|O_NOFOLLOW` inside the destination directory, and
  `Staged::publish` renames it into place with `renameat(2)` in the same directory. A link planted
  at the destination name is replaced, not written through; an abandoned staging copy deletes
  itself.
- `ProjectStore::import_handle` takes the open descriptor, so the file that passed the boundary
  check is the file that gets copied. The recorded `sha256` and `bytes` now come from that single
  copy pass, not from separate lookups.
- The agent tools (`cinvar.media.import`, `cinvar.media.import_folder`, `cinvar.project.relink`,
  `cinvar.color.import_lut`) open through `open_agent_file`, which tries each allowed root by
  descriptor and hands the handle on. `ImportPolicy::MediaOnly` applies the content policy to the
  **copied bytes** (the staged file) rather than to a first look at the source, so what is judged
  is what is stored, and a rejected import leaves nothing behind.

**Regression tests** - `tests/fs_race_security.rs` (6 tests):
`the_recorded_hash_and_size_always_describe_the_stored_bytes` (fails on the pre-fix code),
`a_parent_swapped_for_a_symlink_cannot_redirect_an_open`,
`a_link_planted_under_the_asset_name_is_not_written_through`,
`a_rejected_import_leaves_no_staged_bytes_in_the_project`,
`legitimate_imports_and_dedupe_still_work`, `an_atomic_write_never_follows_a_planted_link`.
The existing `tests/tool_path_security.rs` (8) and `tests/otio_security.rs` (12) still pass, and
they exercise the same tools end to end through the real stdio server.

### F-2 (Medium) Atomic writes used a predictable temp name and a following create

`paths::write_atomic` computed `<name>.<ext>.<pid>.<counter>.tmp` and created it with
`File::create`, which follows a symlink. A process that can write inside the project (that is the
threat model for plugins, and for a hostile project folder) could plant a link at one of those
names and have `project.json` - or a revision, or any other atomic write - land on an arbitrary
file. `write_atomic` now goes through `fshandle::write_atomic`: exclusive create of an
unpredictable name in the destination directory, fsync, then `renameat`. The regression test
plants links at all 16 names the old counter could produce and asserts the victim file is untouched
(it fails on the pre-fix code).

### Preserved behaviour (checked, not assumed)

- CLI import keeps its semantics: a human may import from anywhere, and their own symlink is still
  followed - what changed is that the name is read once.
- An absolute symlink that stays inside an allowed root still works: `RESOLVE_BENEATH` refuses
  absolute links outright, so `Dir::open_file` resolves such a link exactly once and re-opens the
  result **through the descriptor**. Containment is still the kernel's decision; a swap can only
  change which file inside the root is opened.
- Error messages improved rather than degraded: a path that belongs to a root but cannot be opened
  reports the real reason (missing, not a regular file, link leaves the root) instead of "outside
  the project", and a rejected import reports the caller's file name instead of an internal
  staging path.

### F-3 (High) A network-enabled plugin could reach host abstract Unix sockets

A manifest with `network: true` kept the **host** network namespace, so the sandboxed child could
`connect()` to abstract-namespace sockets owned by host processes (`@dbus-*`, `@systemd/*`, docker,
gpg-agent). Masking `/run` hides their filesystem cousins but not these: abstract sockets live in
the network namespace, so only the socket call itself can be denied.

**Fix** (`src/plugins/seccomp.rs`, new, `src/plugins/mod.rs`). The host namespace is kept - egress
must keep working for a plugin that declared it - and a raw-BPF seccomp program is installed with
`bwrap --seccomp`. The program validates `seccomp_data.arch` first and **kills** on mismatch, so a
32-bit compat entry point cannot reach `socket` through another syscall number; it then returns
`EPERM` for `socket(AF_UNIX, ...)` and allows `AF_UNSPEC`/`AF_INET`/`AF_INET6`/`AF_NETLINK`. It fails
closed: an unknown architecture, an unwritable config directory or a missing `sh` all fall back to
`--unshare-net`, never to an unfiltered sandbox.

The filter is installed through a small `sh -c 'exec 3<"$0" || exit 126; exec "$@"'` shim, because
`proc::run` cannot hand an inherited descriptor to bubblewrap and `--seccomp` reads its program from
one. Reviewed instruction by instruction: the jump targets, the `seccomp_data` offsets
(`args[0]` at 16) and the four-family allow-list are correct, and the program's last two returns are
`EPERM` then `ALLOW`.

**Evidence.** `tests/plugin_sandbox.rs::host_abstract_unix_sockets_are_not_reachable_from_a_network_plugin`
binds a real abstract socket in the test process and connects to it from a sandboxed child: it
reported `ABSTRACT_OK` before the change and `ABSTRACT_BLOCKED 1` after, while `INET_OK`, `UDP_OK`,
`INET6_OK` and `SOCKETPAIR_OK` keep working (the filter is an allow-list, and `socketpair` is a
different syscall, so a plugin's own threads are unaffected). `cargo test --test plugin_sandbox`
is 5/5 green here.

### F-4 (Medium) Unsafe bind targets in the plugin argv

The argv bound `plugin_root` and `writable` without resolving them. A symlinked plugin directory, a
project directory that is `/`, or a project that is (or contains) the CINVAR config directory could
move a bind or remount `credentials.toml` back over the tmpfs that hides it. Binds now go through
`bind_dir` (canonicalise, must be an existing directory, never `/`), and a writable path that
contains the config directory loses its writable bind - a plugin that cannot write is a plugin
error; leaked API keys are not.

### Verified rather than changed

`/proc` inside the sandbox lists 4 entries against 504 on the host and the host pid is unreadable
(`--unshare-pid` already did its job); `credentials.toml` stays invisible behind the empty `$HOME`
and empty config-directory tmpfs. Both now have tests.

### F-5 (Medium) Secrets under a custom env name were echoed verbatim

`secrets::redact` only knew the eight built-in key env names, so a configured model profile whose
`api_key_env` points at a custom variable had its key printed in tool errors, logs and refusals.
`redact` now unions the built-in names with every `api_key_env` the configuration declares. Proven
by removing the guard: `refusing to write outside the project: /tmp/pk_live_.../leak.otio` appeared
verbatim before, `[redacted]` after.

### F-6 (Medium) The local UI buffered an unbounded request body

`src/server/mod.rs` read whatever `Content-Length` said. It now refuses above a 1 MiB cap with
**413** before reading a byte (the reader is capped too, so a chunked body cannot slip past the
header check), and the guard returns an honest status instead of always 403. A media-route
traversal attempt now answers an explicit 404 JSON instead of erroring out of the handler with a
bodiless 500 and a dropped connection - containment held before, but the failure mode was opaque.

### F-7 (Low) The MCP server allocated an attacker-chosen body size

`src/mcp/mod.rs` did `vec![0u8; length]` straight from the client's `Content-Length`, and its
newline-delimited reader had no bound either, so one header or one missing newline was a memory
exhaustion. Both are capped at 16 MiB (`MAX_MESSAGE_BYTES`), the framed path refuses with a
protocol error before allocating, and the reader stops at the cap. Found during this pass by the
adversarial workstream (R-20) and fixed here because it sat outside that brief's boundary.

### Confirmed, not changed (attacked and held)

Verified by adversarial tests in `tests/mcp_adversarial.rs` (6 tests) against the real stdio
server and the real UI process: permission levels (266 calls across native/dotted/upper/trailing-
space/`mcp__cinvar__` name forms and every alias - all refused for a read-only session); an empty
or malformed `CINVAR_MCP_PERMISSIONS` cannot widen a session; an edit session cannot reach
spend/admin tools; **an agent cannot mint its own cloud approval** through eight other tools aimed
at the approval directory, and a replayed or expired approval is refused and consumed; a confirm
without human approval is refused synchronously and never reports `billed`; CSRF (rebound `Host`,
foreign `Origin`, `text/plain`/`urlencoded`/`multipart` POST), DNS rebinding and media-route
traversal are refused; the UI binds loopback only; a hostile ComfyUI output filename cannot choose a
local path.

## Known residual risks on this pass (NOT fixed - decide before beta)

- **Windows has no equivalent.** `fshandle` compiles there via the previous canonicalise-then-open
  behaviour and is **not** race-free; the agent boundary on Windows remains check-then-use.
- **Path-based parsers.** `cinvar.media.inspect`, `cinvar.qa.render_check`,
  `cinvar.captions.import` open through the boundary and then hand a *path* to ffprobe/a parser,
  which re-opens it by name. Containment is decided before that, and the file was validated by
  descriptor, but the parser's own open is not descriptor-relative.
- **Hard links carry no name.** An agent that can read a hard link inside an allowed root to an
  inode also reachable outside it is not stopped by any path-based check. Fixing it needs an inode
  ownership policy, not a path policy.
- **The published-then-probed window.** The staged copy is verified to still be the file at the
  published name (`same_file_as`) and only then probed; a writer inside `assets/` acting between
  those two steps could still make the probe describe a different file than the one stored.
- **`network: true` still reaches host loopback services** (R-16). The seccomp filter removes Unix
  sockets; TCP to `127.0.0.1` is what the permission declares. It is not isolation.
- **No per-plugin resource limits** (R-15): bwrap 0.9.0 has no `--rlimit-nproc`/`--rlimit-as`, so a
  plugin can still fork or allocate until the host notices. Only the per-tool timeout applies.
- **A plugin that needs a local daemon Unix socket no longer works with `network: true`** (R-17),
  and on architectures other than x86_64/aarch64 a network plugin gets no network at all (R-18).
- **No MCP rate limit** (O-08/R-23) and no per-session token on the UI, so a foreign page can still
  fire a no-cors GET at it (R-22): resource cost only, the answer is unreadable.
- **Some UI routes still answer a bodiless 500** on an internal error (R-21) - no disclosure, no
  mutation, but not the honest JSON the media route now returns.
- Unchanged from the earlier package: O-01 (no sandbox on Windows/macOS), O-04 (untrusted decoder
  surface), O-05 (unaudited cloud worker image), R-11, R-12 (`jobs.list` remote_ref bearer token),
  R-13 (render inputs lack `-protocol_whitelist file`).

## How a reviewer can re-run the security evidence

```
cargo test --test fs_race_security --test tool_path_security --test otio_security
cargo test --lib fshandle
```

To see F-1 fail rather than take it on trust, revert `src/project/store.rs` to `aeb6e69` and run
`cargo test --test fs_race_security the_recorded_hash_and_size`: it reproduces the mismatch in a
few seconds.
