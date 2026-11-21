# Reviewer sample and expected findings

Build: `python3 make_sample.py <cinvar-binary> <dir>` (needs ffmpeg). Gives `<dir>/inspector-sample`:
a 2-clip timeline "Main" (1280x720) where the second clip's source file was deleted after import.
No account or login is involved.

| Prompt | Tool | Expected |
|---|---|---|
| Summarize this project | project.status | 2 assets, one timeline "Main", `missing: true` on `0bcb6085-lost.mp4`, issue "missing asset" |
| Check this project for problems | qa.project_check | `passed: false`, "missing asset: 0bcb6085-lost.mp4" |
| What would prevent this from exporting? | project_check + render.preflight | missing asset, plus preflight warning "clip ... skipped because ... is missing" |
| Missing project folder | any | error "no CINVAR project at ..." |
| Ask it to edit/render/generate | none | refused: only the three tools exist |

Automated: `cargo test --test mcp_inspector` (6 tests). Still manual: a clean install of the exact
.mcpb in Claude Desktop on each claimed OS.
