# Cinvar Project Inspector: submission packet (Linux x86-64 only)

## Artifact (the exact file to test, host and submit)
- `cinvar-project-inspector-0.2.0-inspector.1-linux-x64.mcpb`
- SHA-256 `5e47dbe0890e4493f7652117dde37aeb6004e7c161f109f15dde4080a3be525d`
- Contains `cinvar 0.2.0` binary (SHA-256 `f6dac826767351be5cb1beb56557d970b30f17400c9c47be0cb315c4ebc6f798`) built from commit `fa9bee5` (release profile). `mcpb validate` and `mcpb pack` passed.
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

## Contacts and policy (taken from https://elyraon.com.co, manifest already updated)
- Contact / security / research: colt@elyraon.com.co (the site's own "Security or Research Inquiry" address)
- Homepage: https://elyraon.com.co/cinvar/ . Support: https://elyraon.com.co/contact/ (the site has no /support or /security page and no security.txt)
- Privacy policy: https://elyraon.com.co/privacy/ . It covers accounts, billing and Foundry MCP queries and says nothing about
  local extensions, so publish `PRIVACY_ADDENDUM.md` there before applying.

## v0.2.0 hash conflict: what is true
- The live storefront (checked 2026-11-20) serves only **0.1.0**: `b0396488a46d...3fb3df0`, 5,022,793 B, which matches local `dist/cinvar-0.1.0`.
  It lists 0.2.0 as "Candidate / Forthcoming". So nothing live is mismatched.
- 0.2.0 exists as three different files: GitHub release `a17c107a...` (6,300,267 B), local storefront catalog `4c1d0cc0...`
  (5,421,165 B, source 34f1503), local `dist/` `57c0e10b...`. The inspector bundle does not use any of them.
- Rule for the review: only the .mcpb above (hash above) is submitted and hosted; never describe it as a "v0.2.0 release".
  Before 0.2.0 goes on the storefront, regenerate the catalog from the one published GitHub asset.

## Owner checklist (cannot be done from the repo)
- [ ] Clean-machine install of this exact .mcpb in Claude Desktop (Linux) and the 3 prompts pass.
- [ ] Publish the privacy addendum on /privacy/.
- [ ] Host the .mcpb (GitHub release asset under its own tag recommended) and use this exact file in the form.
- [ ] Read Anthropic's directory terms; submit through the desktop-extension form.
