# Cinvar Project Inspector: submission packet (Linux x86-64 only)

## Artifact (the exact file to test, host and submit)
- `cinvar-project-inspector-0.2.0-inspector.1-linux-x64.mcpb`
- SHA-256 `c4e0c1c87d3e2ffcfeea6600318ec2f74959077eb6f72b83ead49092c9eeb2e9`
- Built from commit `71fca3e` plus the Linux-only manifest change. `mcpb validate` and `mcpb pack` passed.
- Smoke test done on the unpacked file with a minimal environment: initialize (2024-11-05), tools/list
  returns exactly 3 tools, all 3 work on the sample, `cinvar.ai.generate_video` is refused even with
  `CINVAR_MCP_PERMISSIONS=full`.

## Supported platform and requirements
- Linux x86-64 with glibc 2.39 or newer (Ubuntu 24.04+). Older distributions will not run the binary.
- `ffmpeg` on PATH for `render.preflight` (it does a trial encode). The other two tools do not need it.
- No account, login, API key or network access.

## Reviewer prompts (project: `make_sample.py` output, see `sample/EXPECTED.md`)
1. "Summarize this project."
2. "Check this project for problems."
3. "What would prevent this project from exporting?"
Also try: a folder with no project (clear error), and "edit the timeline" (refused; inspection only).

## Disclosures
- Tools: `cinvar.project.status`, `cinvar.qa.project_check`, `cinvar.render.preflight`; all annotated
  readOnlyHint=true, destructiveHint=false, openWorldHint=false.
- Reads only inside the folder chosen in the install dialog; no tool accepts a path.
- Writes nothing to the project: no lock file, no call log. It creates empty working subfolders
  (assets, cache, renders, revisions, sessions, tmp) if they are absent. Preflight's trial encode may use
  the system temp directory.
- Returns to Claude: asset names/durations/sizes, timeline and track structure, captions, issues, the
  project's local path, encoder name and free disk/memory.
- No network calls, no telemetry.
- Excluded by design: all AI generation/editing, rendering, export, cloud and plugin tools.

## Owner checklist (cannot be done from the repo)
- [ ] Clean-machine install of this exact .mcpb in Claude Desktop (Linux) and the 3 prompts pass.
- [ ] Add verified security and product contact addresses to the manifest and the form.
- [ ] Privacy-policy URL and support/troubleshooting URL (none exist in the repo).
- [ ] Decide the host for the reviewer download (GitHub release asset recommended) and use this exact file.
- [ ] Read Anthropic's directory terms; submit through the desktop-extension form.
- [ ] Release hashes: GitHub v0.2.0 free archive is `a17c107a...` (6,300,267 B); the local storefront
      catalog says `4c1d0cc0...` (5,421,165 B, source 34f1503); local `dist/` has `57c0e10b...`. Three
      different files carry the name 0.2.0. This bundle does not depend on any of them, but do not claim
      "v0.2.0" for the base product until the live storefront file is checked.
