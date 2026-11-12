# Security policy

## Reporting a vulnerability

Open a private security advisory on the repository, or email the maintainers listed in
the repository profile. Please do not open a public issue for a security problem.
We aim to acknowledge reports within 72 hours.

## Threats CINVAR takes seriously

CINVAR processes arbitrary media, launches subprocesses and can be driven by AI agents whose arguments must be treated as
**untrusted**. Details, findings and open items: [docs/SECURITY_REVIEW_PACKAGE.md](docs/SECURITY_REVIEW_PACKAGE.md).
**No independent security review has been done yet** - the review so far is the maintainers' own.

**Untrusted media.** Imports are content-validated against a container allow-list (ffprobe with a file-only protocol
whitelist) and copied into the project; source files are never modified.

**Hostile timelines (OpenTimelineIO).** Media references are confined to the project, the `.otio`'s folder and
`security.media_roots`; symlinks and `..` escapes are refused; files are opened once and copied from the open handle. The
swap-after-check re-verification uses `/proc` (Linux only) and has not been race-tested.

**Agent file access.** Project-scoped by default: agents read and write only inside the project (symlinks resolved).
Extra folders are chosen by the human launching the process (`--media-root`, `CINVAR_MEDIA_ROOTS`, `security.media_roots`).

**MCP permissions.** Sessions default to `edit`; cloud spend and plugin administration need `CINVAR_MCP_PERMISSIONS=full`,
chosen by whoever launches the server. stdio only; no network listener.

**Untrusted plugins.** Disabled until enabled; subprocess execution needs both the manifest permission and
`security.allow_plugin_subprocess`; argv only (no shell); output capped; timeouts. On Linux commands run in a
**bubblewrap** sandbox (read-only system, empty home, config/credentials hidden, project-only writes, no network unless
declared). Without bubblewrap they are refused unless `security.allow_unsandboxed_plugins` is set. **macOS and Windows have
no sandbox**; install scripts are never executed.

**Credential exposure.** Credentials come from the environment or an owner-only (`0600`) file, go only to the configured
endpoint, and are redacted from errors, logs, transcripts and plugin output. No tool returns a stored key.

**Unapproved spending.** Cloud rendering is off by default; it requires an opt-in, a quote, and a **human approval** given in an
interactive terminal (`cinvar cloud approve`, single-use, 15-minute expiry) in addition to the agent's confirmation arguments. Limits are **local best effort - RunPod offers no provider-side hard cap**, and the adapter
has only been tested against a mock.

**Denial of service against your own machine.** Subprocess timeouts, cancellable jobs, bounded retries, agent budgets,
truncated tool results, refused oversized renders.

## Known limitations

- FFmpeg and Blender decode untrusted files without a sandbox. Run CINVAR in a container or as a low-privilege user for hostile media.
- The local web UI (`cinvar ui`) binds to 127.0.0.1, has no authentication, and rejects wrong `Host`/foreign `Origin`/non-JSON posts; never expose the port.
- No plugin sandbox on macOS/Windows; Windows/macOS builds are untested.
- A human who widens `media_roots` widens what agents can read; choose narrow folders.
- The mock planner is not a security boundary.

## Scope
In scope: path traversal/arbitrary file read or write, command injection, secret leakage, sandbox escapes, unapproved
cloud spend, crashes on crafted media or `.otio` files. Out of scope: vulnerabilities in FFmpeg/Blender/etc. themselves
(report upstream; tell us if CINVAR makes them worse).
