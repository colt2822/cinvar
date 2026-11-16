# Ready-to-file issues for 0.1.1-alpha

These are the residuals I did **not** close in the hardening pass, written as issue bodies so they
can be filed as-is. They are ordered by how much they matter to a user, not by effort. Everything
here is honest about what is verified and what is a hypothesis.

---

## 1. Plugin sandbox does not exist on Windows or macOS (`platform:windows`, `platform:macos`, `security`, `area:plugins`)

Plugin commands are **refused** on both platforms unless the user sets
`security.allow_unsandboxed_plugins = true` (fail-closed, so nothing is currently exploitable by
default - third-party plugins just do not run). The Linux path uses bubblewrap plus a seccomp
filter; there is no equivalent here.

Needed: an equivalent isolation story per platform (Windows: AppContainer/job objects; macOS:
`sandbox_init`/`sandbox-exec` profiles), behind the existing `sandbox_command` seam so
`execution_allowed` keeps failing closed when it is missing.

## 2. No per-plugin resource limits (`security`, `area:plugins`, `good first issue`)

A sandboxed plugin can still fork and allocate until the host notices. `bwrap` 0.9.0 offers no
`--rlimit-nproc`/`--rlimit-as`, so the only bound today is the per-tool timeout. Options: a wrapper
that calls `setrlimit` before `exec`, cgroup v2 delegation, or `RLIMIT` via `prlimit(2)` in the
plugin spawn path. Needs a product decision on default budgets.

## 3. Render inputs do not pass `-protocol_whitelist file` (`security`, `area:render`)

`media::probe` already restricts ffprobe to the `file` protocol, which closed playlist-based file
smuggling. The render path in `src/render` feeds ffmpeg without that flag, so a crafted input could
make ffmpeg open a non-file protocol. Fix: add `-protocol_whitelist file` to the render invocation
and prove it with a fixture that tries to pull an HTTP/pipe URL.

## 4. Windows CI fails in text and caption rendering (`platform:windows`, `area:render`, `ci`)

The drawtext/caption tests fail on the Windows runner. The mechanism is **not** verified on a real
Windows machine from the machine that opened this issue: platform hypotheses are explicit font
selection instead of fontconfig-backed `font=`, filtergraph escaping of `:\\` and quotes, long paths,
and UTF-16/BOM subtitle files. Please attach the CI log of the failing assertion; the fix should
carry a test that fails for the same reason on Linux.

## 5. Intermittent FFmpeg 8 failure in the music-ducking test (`area:audio`, `flaky-test`)

The loudness/ducking assertion fails intermittently on the experimental FFmpeg 8 job. Do not skip
it: the likely cause is a timing-dependent assertion (exact sample/loudness values) rather than a
filter bug. A semantic assertion - the bed drops by at least N LU while the voiceover does not -
would still catch a regression and stop the flake.

## 6. `jobs.list` exposes a worker bearer token in `remote_ref` (`security`, `area:cloud`, `good first issue`)

For cloud jobs the stored remote reference contains the worker bearer token, and `jobs.list` returns
it to the agent. Redact it in the listing (keep it in the job store) so an agent transcript or a
pasted log does not carry a live credential for the worker.

## 7. Local UI: no per-session token, and some routes still answer a bodiless 500 (`security`, `area:ui`)

The Host/Origin/Content-Type guards and the 1 MiB body cap are in place, and the server binds
loopback only. Remaining: a foreign page can still issue a `no-cors` GET (the response is
unreadable, so the cost is request handling only), and routes other than the media route return a
bodiless 500 on an internal error instead of honest JSON. A per-session token would close the first
and is a product decision (it changes how the UI is opened).

## 8. Descriptor-relative opens are unavailable on Windows (`platform:windows`, `security`)

`src/util/fshandle.rs` makes the agent file boundary and the open one operation on Linux (and via an
`O_NOFOLLOW` walk on other Unix). On Windows it falls back to the previous canonicalise-then-open
behaviour, which is **not** race-free. Closing it needs the handle-based equivalents
(`CreateFile` with `FILE_FLAG_OPEN_REPARSE_POINT` plus per-component resolution, or
`NtCreateFile` relative opens).
