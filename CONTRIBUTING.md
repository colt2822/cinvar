# Contributing to CINVAR

CINVAR is built for a hard constraint: an AI model must be able to do real professional
video work through validated operations, and a human must be able to check the result.

## Ground rules

- Write working code before writing prose. A feature is done when a test produces a file
  that ffprobe agrees with.
- Never claim a capability you have not executed. Mark it UNVERIFIED, MOCKED or
  NOT IMPLEMENTED in the docs and in your pull request.
- Keep the core lightweight. No new mandatory runtime services, no shell execution, no
  new heavyweight dependencies without discussing it in an issue first.
- Every new tool needs: a name in `cinvar.<group>.<verb>` form, a strict JSON schema,
  a description that tells a model when to use it, argument validation, a structured
  error, and a test.

## Development loop

    cargo build
    cargo test                                   # ~280 tests; needs FFmpeg/ffprobe on PATH
    cargo clippy --all-targets -- -D warnings    # CI is strict
    cargo fmt --all
    ./examples/quickstart.sh                     # end-to-end smoke test (offline)
    python3 plugins/test_plugin.py plugins/project-stats   # plugin harness (Linux, needs bubblewrap)

Tests that need real third-party software (Blender, OpenTimelineIO, OpenColorIO, faster-whisper) are `#[ignore]`d by
default and skipped in CI; run them with `cargo test -- --ignored --test-threads=1` after setting `CINVAR_BLENDER`,
`CINVAR_OTIO_PYTHON`, `CINVAR_OCIO_PYTHON`, `CINVAR_FASTER_WHISPER_PYTHON` (+ model, `CINVAR_TEST_SPEECH`). Say in your PR
which ones you ran. Label results honestly: TESTED / NOT TESTED / DEPENDENCY UNAVAILABLE.

## Where things are
`ARCHITECTURE.md` has the module map. Docs live in `docs/`; the tool reference `docs/TOOLS.md` is generated
(`python3 scripts/gen_tool_docs.py`). Good first issues are marked 🌱 in `ROADMAP.md`.

## Reporting bugs, security problems, ideas
Bugs and feature ideas: GitHub issues (templates provided). **Vulnerabilities: do not open an issue** - see SECURITY.md.
Larger design changes: open an issue first so the decision is public.

## Releases
Maintainers tag `vX.Y.Z`, update CHANGELOG.md (Working / Experimental / Planned), run `scripts/package.sh`, and publish
only artifacts they built and checked; release notes must not claim anything CI has not run. No contributor needs ELYREON
infrastructure, accounts or internal tooling.

## Adding a tool

1. Add the implementation to the owning module (media, render, project::ops, qa, ...).
2. Register it in `src/tools/groups.rs` with a strict schema and a description.
3. If it mutates the project, record history and persist through `ToolCtx`.
4. Add a test that proves the effect in the project document, or the artefact on disk.
5. Mention it in ARCHITECTURE.md if it introduces a new capability class.

## Adding a provider

Implement `providers::Provider` and wire it into `providers::build`. Capabilities must be
honest: if the model cannot see images, `images` must be false so the harness degrades to
text-and-artifact analysis. Never log credentials or request headers.

## Adding a skill

Skills are data, not code: drop a Markdown file with frontmatter into `skills/` (or
`~/.config/cinvar/skills/` to keep it local). See `skills/README.md`.

## Pull request checklist

- [ ] `cargo fmt`, `cargo clippy --all-targets -- -D warnings` and `cargo test` pass
- [ ] new behaviour has a test that fails without the change
- [ ] documentation updated, with honest status markers
- [ ] no secrets, keys, tokens or personal media in the diff
- [ ] no new mandatory dependency without justification

## Licensing

Contributions are accepted under Apache-2.0. By opening a pull request you confirm you
have the right to license the contribution under those terms. FFmpeg is invoked as an
external program, not linked, which keeps CINVAR's own licensing simple; if you add a
dependency with a copyleft license, document the obligation in
docs/LICENSES.md before merging.
