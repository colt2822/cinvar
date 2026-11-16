# Open-core structure

| Part | License | Where |
|---|---|---|
| Editing engine, timeline, ordinary rendering up to 2048x1080, CLI, MCP, web UI, tool registry, action schema, license verifier | Apache-2.0 (unchanged; already-distributed code is not restricted retroactively) | repo root (`src/`, `web/`) |
| Batch rendering, export presets, advanced color | Proprietary (`pro/LICENSE-PRO`, draft pending counsel) | `pro/` crate `cinvar-pro` |

**Direction of dependency:** `pro` links against core; core never links `pro`. Core exposes three
extension points: `render::extensions::register_effect`, `tools::register_extension`,
`edition::register_module`. The `cinvar-pro` binary installs the proprietary module through the core edition hook and then runs core's main.

**Free builds do not contain Pro code** (`scripts/package_editions.sh` builds `-p cinvar` for Free and
`-p cinvar-pro` for Pro). Entitlement checks in the core (export ceiling, tool gating) are defense in
depth, not the boundary. `scripts/publish_github.sh` removes `pro/` before any public publish.

**Compatibility:** projects store grades as ordinary effects; Free ignores unknown effect kinds with a
warning, so Pro-graded projects open and render in Free.
**Not guaranteed:** since the core is open source, a rebuilt core can drop the ceiling. That is
accepted; the paid value is the Pro module, support and updates.
