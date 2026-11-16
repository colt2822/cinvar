# CINVAR SUPER SPRINT — PROGRESS

Shared source of truth for the AI-native video-editor + Blender-harness sprint.
Governing spec: Gemini 4 Argon recommendations. This document records what is REAL (executed,
measured) versus what is still open. Nothing here is claimed without evidence.

## Current state

| Field | Value |
|---|---|
| Canonical branch | `integration/super-sprint` |
| Base | `feature/deepseek-hardening-showcase` @ `39411e3` |
| Integration commits | `c54cc23` (vflash), `9c85323` (freepro), doc resolution, `e5ff2af` (seam) |
| Toolchain | cargo 1.98.1, ffmpeg 6.1.1, Blender 4.0.2, python 3.x |
| Hardware | 12 CPU threads, 31 GB RAM, RTX 3060 Ti (display) + Tesla P40 (compute) |
| Render workers | n1, n2, n3, n4 reachable over SSH (BatchMode, no password) |

## PHASE 1 — Unify engineering (COMPLETE)

Worktrees inspected: `cinvar` (main, 32 commits), `cinvar-w-audio`, `-captions`, `-enhance`,
`-scenes` (20 each), `-vflash` (39), `-freepro` (58).

Findings that changed the plan:

- The six `sprint3/*` + hardening feature sets were **already merged into main**: audio
  (Demucs/Auto-Editor), captions (WhisperX/ASS), enhance (Real-ESRGAN), scenes
  (PySceneDetect) all show **0 unique commits** versus main. Rebuilding them would have been
  wasted work, so Phase 1 became integration + verification, not re-implementation.
- Real integration debt was exactly two branches: `vflash/hardening` (7 commits) and
  `cinvar/free-pro-phase1` (26 commits). Both are descendants of main, so both merged cleanly
  with no conflicts.
- `src/edition/mod.rs`, `src/capability.rs`, `src/mcp/groups.rs` from the brief: only the first
  exists, and only on the freepro branch. `capability.rs` and `mcp/groups.rs` exist nowhere —
  the real paths are `src/edition/mod.rs`, `src/tools/mod.rs` + `src/tools/groups.rs`.

### Recovered fixes

| Commit | Change | Status |
|---|---|---|
| `b7fc9ca` | Reset subject-tracking crop positions across hard scene cuts (hold-and-takeover boundary points) | merged |
| `9f510f9` | Global 60-keyframe cap across all shots; forward `CINVAR_TRACK_DEVICE/CONF`; helper fails cleanly on corrupt input | merged |
| `0edde82` | `--project` on an existing customer dir initializes IN it instead of creating `name-2` | merged |
| `1be7b0e` | `auto-edit --reframe` integrates subject-aware reframing into the auto-edit pipeline | merged |
| `fd549ae` | cut-jump + 60-point-cap regression tests | merged |
| `ed44b0c` | customer-path acceptance script `scripts/acceptance.sh` | merged |
| `5ea453f` | Blender washed-out colour fix (Standard view transform, emission 1.0) | merged |
| `941d5da` | Studio timeline picker, stale-page banner, timeline styling | merged |
| 26 freepro commits | edition gating, license verifier/issuer, Studio UI, model roles, video bridges | merged |

### Conflict markers

`CINVAR_INTEGRATION_PROGRESS.md` and `CINVAR_LICENSE_INVENTORY.md` carried nested conflict
markers from successive sprint3 merges. Resolved as a union of all four sections (scenes,
captions, enhance, audio) with duplicated titles collapsed — no content discarded.

### Baseline verification

- `cargo build --release` — clean (warnings only).
- `cargo test --release` — every test binary reports `test result: ok`, 0 failures.
- Linux Free/Pro builds compile from one binary (feature-gated entitlements, not separate binaries).
- Existing release artifacts preserved at `dist/cinvar-0.1.0-linux-x86_64*` (untouched).

### Worktree preservation

All seven worktrees remain in place with their uncommitted work untouched. Nothing was deleted.

## PHASE 2 — Edition contract (FROZEN, being enforced)

The capability contract is committed so every later phase gates against one table:
25 `Feature` variants covering Argon's Free/Pro split, `tool_feature()` mapping the tool surface,
`SEMANTIC_FREE_CLIP_LIMIT = 10`, and `activate()`/`ActivationSource` for license import.

Enforcement choke points (verified by reading the code):

- `src/tools/mod.rs:116` consults `tool_feature(name)` for **every** MCP tool call.
- `src/edition::Entitlements::check_export(w,h)` guards render, enhance and Blender output size.
- `resolve_from_disk()` performs **no caching**, so a license installed by `cinvar activate`
  unlocks Pro for the next resolution without restarting the application — this is what makes
  live unlock possible without redesigning anything.

Live evidence on this machine (owner license, key `k1`):

```text
$ cinvar edition status --json
  "edition": "pro", "enforcement": "enforce", "export_limit": "engine limit",
  "features": [ export.2k, export.4k, export.8k, render.cloud, render.batch, export.presets,
                color.advanced, export.pro_codecs, captions.word_level, captions.karaoke,
                captions.branded, enhance.upscale, audio.stems, reframe.tracking,
                semantic.full, search.cross_project, ai.autonomous, batch.multi_folder,
                pacing.custom, shorts.multi, mcp.production, export.otio,
                blender.production, blender.mockup, blender.distributed ]
```

## Phase status board

| Phase | Area | Status |
|---|---|---|
| 1 | Unify engineering | COMPLETE |
| 2 | Free/Pro boundaries + `--pro-preview` | in progress (edition agent) |
| 3 | Installation friction, `setup --pro-ai`, activate | in progress (install agent) |
| 4 | Shorts / ducking / vocabulary | in progress (editing agent) |
| 5A/5B | Blender alpha + `preview_check` | in progress (blender agent) |
| 5C/5D | Declarative scene graph + mockup | queued (needs 5A/5B seam) |
| 5E/5F | Warm workers, GPU detection, distributed render | queued |
| 6 | Five Blender MCP tools | queued (needs blender module) |
| 7 | Studio integration | in progress (studio agent) |
| 8 | End-to-end demos A–F | queued |
| 9 | Benchmarks | queued |
| 10 | Tests, docs, release artifacts | queued |

## Blockers requiring owner intervention

None so far. `cinvar activate` supports license-file import today; order-key redemption needs
the license service URL in `CINVAR_LICENSE_REDEEM_URL` because no redemption endpoint is
configured in this repo — the code path exists and reports this honestly rather than pretending
the key was checked.

## Artifact hashes

Recorded at release time. Current untouched baseline:
`dist/cinvar-0.1.0-linux-x86_64.tar.gz.sha256` (pre-existing, preserved).
## PHASE 5F — feasibility probe (measured, before implementation)

Probed to decide whether distributed rendering is real or aspirational:

| Node | Host | Cores | RAM | Blender | ffmpeg | passwordless sudo | Internet |
|---|---|---|---|---|---|---|---|
| n1 | elyraonn1 | 4 | 30 GB | no | no | no | yes (download.blender.org -> 200) |
| n2 | ELYREON-N2 | 6 | 14 GB | no | no | no | (assumed same LAN egress) |
| n3 | ELYREON-N3 | 4 | 14 GB | no | no | no | (assumed same LAN egress) |
| n4 | elyraonn4 | 12 | 30 GB | no | no | no | (probed, reachable) |

Consequences that shape the implementation:

- **No passwordless sudo and no Blender installed** on any worker, so a system install is out.
  A **portable Blender tarball** cached per worker (`~/.cache/cinvar/blender-dist/<version>/`)
  is the only route, and n1 confirmed it can download from download.blender.org.
- **No ffmpeg needed on workers.** Blender writes PNG frames itself; encoding and frame
  reassembly stay on the coordinator, which already has ffmpeg 6.1.1. This keeps the worker
  contract to "render these frame numbers into this directory".
- **No fast shared storage.** `/mnt/elyreon-data` and `/mnt/elyreon-models` are local ext4;
  the only cross-machine path is an sshfs mount of a NAS *public* share, which is unsuitable
  for frame traffic. So artifacts move by explicit transfer (tar over ssh), not by a shared
  mount. Work units must therefore be self-describing and idempotent.
- Worker roots have 32-36 GB free, enough for a Blender tarball plus a frame working set that is
  cleaned per job.
- Local GPU capability for the render-device detector: RTX 3060 Ti (sm_86) and Tesla P40 (sm_61);
  Cycles is present and its device list is queryable from `blender -b --python-expr`, which is
  how the detector will enumerate CUDA/OptiX instead of guessing.

### Warm worker plan

The local warm worker (Phase 5E) and the remote workers (5F) share one request/response
shape: a JSON job (scene, frame range, output dir, device preference) over a unix socket
locally and over ssh remotely. `spec_hash` from `src/blender/mod.rs` stays the cache identity
on both, and `use_placeholder`/`use_overwrite` keep frames resumable when a worker dies mid-range.

## Delivered and independently verified (coordinator review, not agent self-report)

### PHASE 2 — Free/Pro boundaries + `--pro-preview` (COMMITTED `4c5c104`)

`gate_feature`/`gate_export` carry a `Grant` (Included | Preview) so a preview can pass while the
strict checks still refuse. Real ProRes (`prores_ks`), DNxHR and AV1 encoders were added, because
gating a codec that did not exist would have been theatre. The Free semantic cap is
`SEMANTIC_FREE_CLIP_LIMIT = 10`, enforced in `tools::gate_arguments` and the CLI **before** the
index is built (no partial index). `--pro-preview` truncates the real filtergraph to 10 s and burns
the notice in the same ffmpeg pass — no second encode, and the request field is `serde(skip)` so an
MCP caller cannot grant itself a preview.

Measured by the coordinator: `tests/edition_split.rs` 15/15 and `tests/pro_preview.rs` 6/6 pass;
`render --width 3840` exits 3 with the edition error while `--pro-preview render --width 3840`
produces a real 3840×2160 10.000 s file.

### PHASE 3 — installation friction (COMMITTED `09b104d`)

`cinvar setup --pro-ai` genuinely installs four components into `~/.local/share/cinvar/venvs/`
with one coherent cu128 torch/torchaudio pair, the `libomp.so.5` shim Real-ESRGAN needs, cached
weights and per-stage logs. Verified by import and by real functional runs (demucs separated a
tone into four stems; realesrgan upscaled 64×48→128×96 through the shim).

Artifacts: `dist/cinvar-0.2.0-linux-x86_64.tar.gz` (reproducible — identical sha256 on re-pack)
and `dist/cinvar-0.2.0-x86_64.AppImage`. The 0.1.0 artifacts remain in place, untouched.

**Windows:** no binary can be produced on this Linux host (no MSVC, no mingw, no Windows target
and bundled SQLite needs a C toolchain). The documented recipe and the config/path fixes ship; the
gap is stated as a gap.

**Windows font escaping — the premise was wrong.** The escaping is already correct
(`src/render/fonts.rs:24-30` emits `C\:/Windows/Fonts/arial.ttf`; four font unit tests pass). The
real defects found are different and are recorded below as open items: an apostrophe in a font path
silently drops `fontfile=` and falls back to the fontconfig path that crashes on Windows.

### PHASE 4 — shorts, ducking, vocabulary (COMMITTED `ac143e5`)

`generate()` composes transcript → weighted highlight scoring → non-overlapping selection → pacing
→ 9:16 reframe → karaoke ASS → render → QA. The score is a documented function of speech density
(0.45), measured audio energy (0.30) and sentence completeness (0.25) — not N spaced slices.

Ducking replaced the static 0.35 gain with real `sidechaincompress` (speech drives the bed) plus
`amix` + `alimiter`, including the subtle fix that both stems must be padded because
`sidechaincompress` stops when its sidechain ends. Measured: the bed drops ~15.7 dB during speech,
still −7.7 dB 50 ms after it stops, and returns to exactly the unducked level in the pause while
the clipping fixture stays at −2.2 dBFS instead of clipping at 0.0.

### PHASE 5A/5B — transparent alpha + closed-loop verification (COMMITTED `45619ff`)

Alpha is verified on the **delivered file**, not on the PNG frames: `alpha_plane_stats` decodes
with `alphaextract` and measures min/max/mean/coverage. ProRes 4444 came out `yuva444p12le` with
5.87% alpha coverage. **WebM is honestly not delivered**: this ffmpeg lists libvpx-vp9 but the
encoded file decodes as `yuv420p` with no alpha plane, so the probe refuses rather than shipping a
file that silently lost transparency.

`preview_check` returns the four-view sheet plus bpy-computed telemetry. **The 1.5 s target is
missed**: measured 2.2–4.5 s end to end, of which Blender process start alone is 1.07–1.44 s and the
first Workbench frame ~1.7 s. What does amortise is the correction loop — `preview_correct` with 3
rounds costs ~3.1–4.1 s total, i.e. ~90 ms per extra diagnostics round.

### PHASE 6 — Blender MCP tools (partially delivered, COMMITTED `d799328`)

`cinvar.blender.preview_check` (Free) and `cinvar.blender.render_job` (Pro) are registered and
verified over the real MCP server. A live call on a scene with a deliberately off-frame cube
returned `passed: false` with findings in Argon's exact format:

```
"Cube is 11% outside the camera frame at frame 30."
"Cube.001 is 100% outside the camera frame at frame 30."
```

`contact_sheet.png` and `diagnostics.json` were written and Blender 4.0.2 was reported. Under an
empty (Free) config, `render_job` is refused by `tool_feature` while `preview_check` still runs —
the edition table is enforced, not decorative. `inspect`, `scene_build` and `mockup_screen` are
**deliberately not registered yet** rather than shipped as stubs; they land with the scene-graph and
mockup modules.

### PHASE 7 — Studio (COMMITTED `a506c48`, `d67dc3a`)

Production-suite modal with five tabs, reviewed against before/after captures at 1400×813 in the
same theme. Honest degradation verified in the rendered page: each unimplemented route shows a
disabled control naming the missing method and path. `POST /api/license/activate` was added and
verified live (owner license → 200 and edition reloads to Pro with no restart; unknown order key →
400 naming `CINVAR_LICENSE_REDEEM_URL`). `UI_TOOL_PREFIXES` was widened for the families the suite
calls, without widening authority — each tool still passes `tool_feature`.

## Defect found in review and fixed (would have shipped broken licensing)

The `edition-observe` cargo feature is unified across a target directory. A release build sharing
a target dir with `cargo test` therefore links `ENFORCE_BY_DEFAULT = false` and **every Pro gate
disappears**. Measured: a binary from a directory that had run the test suite reported
`"enforcement": "observe"`, while the packaged artifact (fresh target dir) reported `enforce`.

Fixed at the control point that matters (`5c79683`): `scripts/check_enforcement.sh` runs the
artifact with an empty config dir and fails packaging unless it reports `enforce`. Both
`package_linux.sh` and `build_appimage.sh` call it. Verified both ways — the leaked binary is
rejected with exit 1 and proof, the packaged one passes.

## Open items routed, not silently accepted

1. **Apostrophe in a font or subtitle path** silently drops `fontfile=` and falls back to the
   fontconfig path documented as an access violation on Windows (`src/render/fonts.rs:26-28`,
   `graph.rs:219-223`, `preview.rs:27`, `captions/ass.rs:229`). The fix is to stage the file to a
   quote-free name instead of trying to escape it. Not yet applied.
2. **Adapter wiring for `pro-ai.toml`:** `Config.helpers` now exists (`802bf8e`) and setup records
   the paths, but the adapters still read only environment variables, so setup prints the exports.
   Reading `Config.helpers` in `enhance`, `captions` and `audio::demucs` is the remaining step.
3. **`audio/demucs.rs` refuses to run without `CINVAR_DEMUCS_ALLOW_DOWNLOAD=1` even when the
   weights are cached.** Recorded by setup, but the check should be softened to "cached suffices".
4. **WebM alpha** depends on the local ffmpeg build; automatic on a build that keeps VP9 alpha.
5. **Windows binary** cannot be produced on this host (documented above).

### PHASE 5E - warm Blender worker (COMMITTED 835a563)

A real headless Blender binds the unix socket itself and answers length-prefixed JSON; nothing
about Blender is re-implemented in Rust. Job isolation forces a VERIFIED factory reset before an
op, crash recovery respawns and retries once, checkpoints round-trip (a changed checkpoint is
refused), and the optional GUI bridge requires operator opt-in + caller authorization + a live
bridge, so headless never depends on it.

Cold vs warm, 5 runs each on a 7-object scene, MEASURED BY THE COORDINATOR (not the agent):

| | median ms | multiplier |
|---|---|---|
| cold (process start + load + measure) | 2129.6 | - |
| warm load + measure | 21.3 | ~100x |
| warm measure only | ~5 | ~210x (agent measurement) |

Argon proposed 20x. The honest range across runs of this module is 59x-100x for load+measure
depending on machine load; the measurement-only figure is ~210x. Crash recovery and job isolation
were verified with real process kills and real scene swaps.

### PHASE 5F - GPU discovery + distributed rendering (COMMITTED c40fcf2, CLI 2b9dae6)

Device discovery asks Blender instead of guessing. On this machine it reports CUDA, HIP, NONE with
both GPUs named and no OptiX at all - so CUDA is the genuinely best backend here, which only a
probe could have established:

```text
$ cinvar blender devices
Blender 4.0.2
  CUDA     NVIDIA GeForce RTX 3060 Ti
  CUDA     Tesla P40
  CPU      Intel Core i5-10600K CPU @ 4.10GHz
chose CUDA device "NVIDIA GeForce RTX 3060 Ti"; fallback because OPTIX:OPTIX is not compiled
  into this Blender build (compute_device_type offers CUDA,HIP,NONE)
```

Distributed rendering measured on REAL workers n1 + n4 (24 frames, 960x540, Cycles CPU):

| | wall s | render s |
|---|---|---|
| single machine | 67.93 | 67.93 |
| distributed n1 + n4 | 31.18 | 23.19 |
| speedup | 2.18x end-to-end | 2.93x render-only |

Equivalence to the single-machine baseline: 0/24 byte-identical but PSNR 97.40 dB / SSIM 1.000
(different CPUs round Cycles differently, so byte equality is the wrong criterion and the test
asserts the measured one). Kill recovery kept the killed worker finished frames and delivered 24/24;
a deliberately damaged job resumed with reused=20, rendered=4; a finished range re-ran in 12.04 s
with planned_units=0.

Provisioning solved without sudo: a portable Blender tarball per worker plus a loader closure
converged by repeated ldd (9 sonames, 1.7 MB). No system packages, no reboot; worker scratch dirs
are empty afterwards and only the intended Blender cache remains.

use_overwrite=false is the guarantee wanted (a second pass renders nothing, so a verified frame
cannot be damaged). use_placeholder=false because a placeholder turns "missing" into "present but
corrupt", which is exactly the distinction the completeness check relies on.
## FINAL STATE

| Field | Value |
|---|---|
| Canonical branch | integration/super-sprint |
| Commits on the branch over main | 61 |
| Tests | 603 passed, 0 failed, 18 ignored, 40 targets, exit 0 |
| Worktrees | all six preserved on their original branches, untouched |
| Release tarball | dist/cinvar-0.2.0-linux-x86_64.tar.gz |
| Tarball sha256 | 29e11e14d3d075c3c7e6e65e1339928ae5534ca74b791f1c707007f6e1f5d01c |
| AppImage | dist/cinvar-0.2.0-x86_64.AppImage |
| AppImage sha256 | 86e725e6f3f081cfe0732f69d925d0b3988d2200ee2a59efa3092cb3944005c1 |
| Reproducibility | verified: identical sha256 on a second pack |
| Gate check | both artifacts report enforcement=enforce with an empty config dir |
| 0.1.0 artifacts | preserved untouched |

### Phase 6 - five Blender MCP tools

All five are registered and reachable through the real MCP server (83 tools total):

| Tool | Edition | Verified how |
|---|---|---|
| inspect | Free | live call on a real .blend returned active camera, lens, per-object world bboxes, materials, missing_textures |
| preview_check | Free | live call returned passed=false with "Cube.001 is 100% outside the camera frame at frame 30." plus contact_sheet.png and diagnostics.json |
| scene_build | Free build / Pro render | live call built a .blend with 2 objects / 1 camera / 3 lights / 904 vertices, counts re-verified by a second Blender process |
| mockup_screen | Pro | registered; the mockup module renders real video onto 3D surfaces with a timer proof of frame sync |
| render_job | Pro | refused under an empty (Free) config by tool_feature; allowed and alpha-verified under Pro |

### Phase 5C - declarative scene graph

CinvarBlenderScene with objects, materials, three camera rigs, three lighting presets and asset
import for .blend/.glb/.gltf/.fbx/.obj/.svg (SVG extruded to solid geometry). Compatibility shims
cover Blender 3.6/4.x/5.x socket renames and Eevee engine naming, pinned by a drift test that parses
the SDK table with ast and compares it to the Rust table. Live: an SVG logo extruded with
three-point lighting (still luma 0.1624, coverage 0.1865, camera arc proven by centroid
0.4830 -> 0.5045) and a product shot with isometric turntable + revolved cyclorama + imported .glb.

Two findings that only measurement produces: keyframed euler aim drifted 1.9 m off axis on a 360
turn (now a TRACK_TO constraint), and a single-wall cyclorama put the turntable behind its own
backdrop (now a revolved cove).

### What is NOT done (stated plainly)

1. Windows binary: cannot be produced on this Linux host (no MSVC, no mingw, no Windows target,
   and the bundled SQLite needs a C toolchain). Recipe and config fixes documented instead.
2. Alpha WebM: this ffmpeg encodes VP9 without an alpha plane, so only ProRes 4444 is delivered;
   on a build that keeps VP9 alpha the WebM appears automatically.
3. preview_check latency: 2.2-4.5 s measured against a 1.5 s target. Blender process start alone
   is 1.07-1.44 s. The correction loop amortises at ~90 ms per extra round.
4. Apostrophe-in-path staging fix for fonts and subtitle burn-in: diagnosed with reproductions,
   not yet applied.
5. Adapters still read only environment variables for the Pro AI helper paths; Config.helpers now
   exists and setup records them, but enhance/captions/demucs do not consult it yet.
6. audio/demucs still requires CINVAR_DEMUCS_ALLOW_DOWNLOAD=1 even when weights are cached.
7. Demo B as specified (2D SVG to transparent 3D logo to composited overlay) is proven in parts:
   SVG extrusion and transparent ProRes 4444 each verified, the two joined only through the
   compositing function, not in one end-to-end artifact.
8. Studio routes: 2 of 15 new endpoints implemented (license activate; tool families widened).
   The remaining 13 are documented in web/STUDIO_ENDPOINTS.md with exact request/response shapes.
# CINVAR 0.2.0 — RELEASE SPRINT (Argon completion)

Baseline: integration/super-sprint, 62 commits, 603 tests, 0 failures. This sprint adds the
Studio surface, real cloud-agent proof, Argon gap closure and the commercial release gate.

## Verified so far

### Clean-install edition gate on the real artifact

scripts/smoke_edition.sh against dist/cinvar-0.2.0-linux-x86_64.tar.gz passed every expectation:
free exports 1920x1080 / 2048x1080 / 1080x2048 / 1280x720; refuses 2560x1440, 3840x2160, 7680x4320,
1440x2560, 2050x1080; MCP export refuses 4K in free; blender animation refuses 4K in free; config
cannot disable enforcement; a junk license is refused and the edition stays free.

### Customer journey, steps 1-4, on the packaged binary

Extracted the 0.2.0 tarball to a clean prefix with empty CINVAR_CONFIG_DIR/CINVAR_DATA_DIR:

- install: cinvar 0.2.0 runs, edition free, enforcement enforce
- activate: cinvar activate <license file> -> "CINVAR Pro activated (license L-e7484e1c9629)",
  edition pro immediately, and pro again in a fresh process (no restart, and it persists)
- connect: cinvar connect claude wrote a project-scoped .mcp.json pointing at the packaged binary

### Real cloud-model execution: Claude Code 2.1.325 against CINVAR MCP

Claude Code (installed, authenticated) was run headless with --allowedTools mcp__cinvar__* against
the packaged binary. It connected, imported the footage, ran a contact sheet, ran scene detection,
measured integrated loudness and true peak, ran the clipping check, built a 1080x1920 timeline,
normalised the audio (source peaked +0.17 dB, render peaked -1.5 dB), rendered, and passed
qa.render_check. Artifact: a real 1080x1920 14.567 s Short with AAC 48 kHz stereo.

**Two real product defects were exposed by that run, which is the point of running it for real:**

1. A clean customer configuration reports NO transcription engine, because the adapters read only
   CINVAR_*_PYTHON environment variables and never Config.helpers. A customer who ran
   setup --pro-ai would still have no captions. This is exactly the Priority 4.2 defect, now
   confirmed from the customer side rather than from code reading.
2. cinvar.captions.transcribe returns sentence-level cues only, and cinvar.captions.word_level was
   listed in the edition tool table but DID NOT EXIST as a tool. Claude Code correctly refused to
   invent word timings. Fixed this sprint: cinvar.captions.word_level now exists, returns per-word
   timings from WhisperX and can write a karaoke ASS.

Codex CLI 0.164.0 was also attempted and is REFUSED by the harness admission governor
("codex admission denied [BACKGROUND_DISABLED] ... try again at 10:22 AM"); no Codex process was
started. That is an external gate, not a CINVAR defect, and it is reported as such.
## 0.2.0 RELEASE SPRINT - FINAL STATE

| Field | Value |
|---|---|
| Final commit | 8a8495b on integration/super-sprint (69 commits over 39411e3) |
| Tests | 657 passed, 0 failed, 20 ignored, exit 0 |
| Tarball | dist/cinvar-0.2.0-linux-x86_64.tar.gz sha256 57c0e10b9ddf0025f34682c422b2c0921c4909860b050c8916938b8149de94ce |
| AppImage | dist/cinvar-0.2.0-x86_64.AppImage sha256 485b020848a12ac25f5fcf08c770e9321c3f2678a661733bb02fa21c4e0ff7dd |
| Reproducible | yes (identical sha256 on a second pack) |
| Release gate | both artifacts report enforcement=enforce with an empty config dir |
| Clean-install smoke | SMOKE OK on the rebuilt tarball (19/19 expectations) |
| 0.1.0 artifacts | untouched, rollback preserved |
| Deployed | NO - deliberately left for owner approval |

### Studio: 14 of 15 route+method pairs implemented (the 15th was already present)

src/server/studio.rs drives the real engines with observable job progress and /api/media delivery.
Proven live: two 1080x1920 Shorts served with HTTP Range, 309 WhisperX word timings, 4 Demucs stems
plus a sidechain clean mix, a scene build with a passing four-view preview, a 24-frame ProRes 4444
render with verified alpha, frame recovery requeuing exactly the deleted frame, and a real
setup --pro-ai run. Six Pro routes return 403 on a Free config while probes stay 200.

### Blender preview target MET

preview_check: 3801 ms -> 504 ms median through the warm worker (7.5x); every warm call is under
the 1.5 s Argon target (worst 866 ms). Diagnostics pixel-identical to the cold path (decoded RGBA
md5 of the sheet and all four views). A genuinely cold start is 2.8-3.7 s and is reported, not hidden.

### Customer AI path closed

setup --pro-ai now writes Config.helpers as well as pro-ai.toml, and all four adapters resolve
through paths::helper_path (environment first, then config). Proven on a clean configuration:
before setup "transcribe: none"; after setup the four venvs install and config.toml carries every
CINVAR_* path, with no environment variables set by the operator.

### OUTSTANDING: alpha is lost when a transparent Blender clip is composited on the timeline

Argon 3.3 requires scene -> RGBA -> alpha intermediate -> timeline -> final video. Every step works
in isolation: ProRes 4444 with verified yuva444p12le alpha, and compositing via
alpha::composite_over_video. The TIMELINE overlay path does not: a clip whose SOURCE carries alpha
is still forced to format=yuv420p because src/render/graph.rs computes
alpha = opacity < 0.999 || ex_op.is_some() || force_alpha and never consults the source pix_fmt.
Claude Code reproduced it: title on solid black over invisible footage, while qa.render_check
reported passed (it validates file properties, not picture).

Planned fix, precise and small: treat TrackKind::Overlay clips as compositing (thread the track kind
into the chain builder) and/or probe the source pix_fmt once per clip, then promote to rgba/yuva
instead of yuv420p. Not applied because src/render/graph.rs was owned by the reliability agent for
the whole session.