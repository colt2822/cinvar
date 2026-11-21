# Building the Project Inspector bundle

1. `cargo build --release --bin cinvar` on the target OS (only Linux x86-64 has been built and tested so far).
2. Stage: `mkdir -p stage/server && cp target/release/cinvar[.exe] stage/server/ && cp packaging/mcpb/manifest.json stage/`
3. Validate and pack (downloads the official MCPB CLI, so run it yourself): `npx @anthropic-ai/mcpb validate stage/manifest.json && npx @anthropic-ai/mcpb pack stage cinvar-project-inspector-0.2.0-inspector.2-<os>.mcpb`
4. `sha256sum` the .mcpb. Use that exact file for Desktop testing, the reviewer download and the application.

Owner items before applying (not in the repo, not invented here): verified security and product contacts,
privacy-policy link, support/troubleshooting URL, reading Anthropic's directory terms, submitting the form.
Local behaviour to disclose: the inspector writes nothing to the project (no lock, no call log), makes no
network calls, returns project details (asset names, timeline structure, local paths, free disk/memory)
to Claude. It does create empty working subfolders (assets, cache, renders, revisions, sessions, tmp) if absent.
