# CINVAR Studio — endpoint contract

Every HTTP call the Studio page (`web/index.html`) makes, the JSON it sends, the JSON it reads,
and whether `src/server/` implements it **today**. This is the wiring list for the backend.

## How the status in this file was measured

```
$ CARGO_TARGET_DIR=/tmp/tgt-studio cargo build --release --bin cinvar      # OK
$ /tmp/tgt-studio/release/cinvar ui --port 7787
CINVAR web UI on http://127.0.0.1:7787
$ curl -s -o /dev/null -w '%{http_code} %{size_download}\n' http://127.0.0.1:7787/
200 101773
$ for p in /api/shorts /api/captions/style /api/audio/enhance /api/setup/pro-ai /api/blender/preview; do
    printf '%-24s -> ' "$p"; curl -s -w ' [%{http_code}]\n' "http://127.0.0.1:7787$p"; done
/api/shorts              -> {"error": "unknown route"} [404]
/api/captions/style      -> {"error": "unknown route"} [404]
/api/audio/enhance       -> {"error": "unknown route"} [404]
/api/setup/pro-ai        -> {"error": "unknown route"} [404]
/api/blender/preview     -> {"error": "unknown route"} [404]
$ curl -s -w ' [%{http_code}]\n' -X POST -H 'Content-Type: application/json' \
    -d '{"order_key":"TEST"}' http://127.0.0.1:7787/api/license/activate
{"error": "unknown route"} [404]
```

The routers live in `src/server/mod.rs` `handle()`: an unmatched path falls through to
`json_response(404, {"error":"unknown route"})` (end of the function). Nothing else answers 404,
so that exact body is a reliable "this route does not exist" signal.

---

## 0. How the page decides a control is live

The page never assumes. On opening **◆ Suite** it sends one `GET` per feature group to the
read-only status route below and classifies the reply:

* `404` **and** `{"error":"unknown route"}` → the feature exists nowhere in the server.
  The feature's `<fieldset>` gets the `locked` class (dimmed, `pointer-events:none`) and an
  amber banner appears naming the missing method and path, e.g.
  *"Not available in this build. This needs `GET /api/shorts`, which the local server does not
  implement yet."* Nothing is simulated and no placeholder result is rendered.
* Anything else (`200`, `400`, `403`, `500`, …) → the route exists, the controls unlock and the
  panel renders whatever the server reported, including its `error` string.

Actions that POST to a missing route degrade the same way at click time (inline note in the panel,
naming the method and path) — see `ep()` / `notAvail()` in `web/index.html`.

**Contract this implies for the backend:** these routes must reject a request they cannot serve
*before* doing work — `400 {"error": "…"}` for a missing/invalid body. The probe is a plain `GET`
on the status routes, so a status route must be safe to call at any time.

**Nothing needs to change in `web/index.html` when a route lands.** Implement the route, reload the
page, and that panel unlocks itself.

| Suite tab | probe (read-only) | unlocks |
|---|---|---|
| Shorts | `GET /api/shorts` | `POST /api/shorts` |
| Captions | `GET /api/captions/style` | `POST /api/captions/style`, `/style/preset`, `/word_level` |
| Audio | `GET /api/audio/enhance` | `POST /api/audio/enhance` |
| AI setup | `GET /api/setup/pro-ai` | `POST /api/setup/pro-ai` |
| Blender | `GET /api/blender/preview` | `POST /api/blender/scene`, `/render`, `/recover` |

License activation is not probed: the JSON-licence path (`POST /api/license`) already works, so the
panel is always live and the order-key button degrades by itself on click.

---

## 1. Automated Shorts (tab **Shorts**)

Source, count, duration range, vocabulary, progress, resulting files.

### `GET /api/shorts` — implemented: **no**
Poll for progress + the list of runs. The page calls it on open and then every ~900 ms while the
displayed run is `queued`/`running`.

```jsonc
// response
{
  "runs": [
    {
      "id": "run-7f3a",              // opaque id, echoed by POST
      "state": "queued|running|succeeded|failed|cancelled",
      "progress": 0.42,              // 0..1
      "count": 3,                    // how many Shorts were requested
      "error": "",                   // shown verbatim when non-empty
      "outputs": [
        {
          "index": 1,
          "path": "/abs/project/shorts/short-1.mp4",  // displayed as the file name
          "seconds": 41.2,
          "width": 1080, "height": 1920,
          "qa_passed": true,
          "url": "/api/media/short-1.mp4"  // optional; a "Save" button appears only when present
        }
      ]
    }
  ]
}
```

### `POST /api/shorts` — implemented: **no**
```jsonc
// request
{
  "source": "<asset id, exactly the id in GET /api/status assets[].id>",
  "count": 3,
  "min_seconds": 20,                 // inclusive duration bounds for each Short
  "max_seconds": 60,
  "vocab": ["CINVAR", "Elyreon"],    // may be empty
  "output_dir": "/abs/or/relative"   // optional; omit for the project default
}
// response
{ "run_id": "run-7f3a" }             // or { "error": "…" }
```

Engine already in the tree (no route yet): `src/shorts/mod.rs` — `ShortsRequest { input, count,
min_duration, max_duration, output_dir, vocab }` → `ShortsReport { source, shorts[], elapsed_ms }`
with `Short { index, path, start, end, score, width, height, qa_passed }`; that maps onto the JSON
above almost field for field. `cinvar shorts` is the CLI equivalent. Gated by
`Feature::ShortsMulti` (`src/cli/mod.rs`, `check_feature`).

---

## 2. Captions (tab **Captions**)

Timing mode (line / word-level / karaoke), style (size, colour, box, position, capitals), style
presets.

### `GET /api/captions/style` — implemented: **no**
```jsonc
// response
{
  "mode": "line|word|karaoke",
  "active": { "size": 48, "color": "#ffffff", "background": "black@0.6",
              "uppercase": false, "position": "bottom" },
  "presets": [
    { "name": "Bold yellow", "builtin": true, "active": false,
      "style": { "size": 64, "color": "#ffe14d", "background": "black@0.7",
                 "uppercase": true, "position": "lower_third" } }
  ]
}
```

### `POST /api/captions/style` — implemented: **no**
```jsonc
// request — restyle the existing caption track
{ "size": 48, "color": "#ffffff", "background": "black@0.6",
  "uppercase": false, "position": "bottom", "mode": "line" }
// response
{ "ok": true }                        // or { "error": "…" }
```

### `POST /api/captions/style/preset` — implemented: **no**
```jsonc
// request
{ "name": "Bold yellow", "style": { "size": 64, "color": "#ffe14d", "background": "black@0.7",
                                    "uppercase": true, "position": "lower_third" } }
// response
{ "ok": true, "saved": "Bold yellow" } // or { "error": "…" }
```

### `POST /api/captions/word_level` — implemented: **no**
Re-transcribe the selected timeline clip to get word timings / karaoke timing.
```jsonc
// request
{ "clip_id": "<clip id from GET /api/status timelines[].tracks[].clips[].id>",
  "mode": "word|karaoke" }
// response
{ "ok": true, "words": 214 }          // or { "error": "…" }
```

Engine already in the tree (no route yet): the tool `cinvar.captions.style`
(`src/tools/groups.rs`, `registry.add(... "cinvar.captions.style")`) already restyles an existing
caption track with exactly `size|color|background|uppercase|position`; `cinvar.captions.transcribe`
takes `clip_id` plus the same style props. **They are not reachable from the page today** because
`UI_TOOL_PREFIXES` (`src/server/mod.rs`, just above `fn edition_status`) whitelists only
`cinvar.render.batch_`, `cinvar.preset.`, `cinvar.color.grade`, `cinvar.project.status` — a
`POST /api/tool` with `cinvar.captions.style` answers `403 {"error":"tool '…' is not available from
this panel"}`. So there are two honest ways to wire this family: add the prefixes and let the page
use `/api/tool`, or add the four routes above. Word-level and karaoke are Pro capabilities
(`Feature::CaptionsWordLevel`, `Feature::CaptionsKaraoke`); the panel already shows the
`lockbox`/upgrade row from `GET /api/edition` for `captions.word_level`.

---

## 3. Audio enhancement (tab **Audio**)

Stems plus a clean mix with music ducked under speech.

### `GET /api/audio/enhance` — implemented: **no**
```jsonc
// response
{
  "state": "idle|queued|running|succeeded|failed",
  "progress": 0.6,
  "error": "",
  "stems": [ { "name": "vocals", "url": "/api/media/stems/vocals.wav" },
             { "name": "drums",  "url": "/api/media/stems/drums.wav" } ],
  "mix":   { "url": "/api/media/stems/clean-mix.wav",
             "note": "music −12 dB under speech" }   // "note" is shown as-is
}
```
`url` may be omitted on any item: the row then says "no file" instead of offering a broken link.

### `POST /api/audio/enhance` — implemented: **no**
```jsonc
// request
{ "source": "<asset id>", "stems": true, "mix": true,
  "duck_db": -12, "denoise": false, "model": "htdemucs|htdemucs_ft|mdx_extra" }
// response
{ "job_id": "…" } | { "error": "…" }
```

Engine already in the tree (no route yet): `src/audio/demucs.rs` — `separate()`, `DuckOptions`,
`duck_mix()`, `clean_mix_ducked()`, returning `DemucsReport { stems, seconds }`. It runs an
external environment named by `CINVAR_DEMUCS_PYTHON`, weights downloads gated by
`CINVAR_DEMUCS_ALLOW_DOWNLOAD`, device via `CINVAR_DEMUCS_DEVICE` + `CUDA_VISIBLE_DEVICES`
(use the P40, never the display GPU). `cinvar audio` CLI subcommand exists.

---

## 4. AI setup (tab **AI setup**)

Install the Pro AI stack with per-component progress.

### `GET /api/setup/pro-ai` — implemented: **no**
```jsonc
// response
{
  "venv_dir": "/home/<user>/.local/share/cinvar/venvs",   // shown as "Environments: …"
  "components": [
    { "id": "whisperx", "label": "WhisperX",
      "state": "queued|running|succeeded|failed|skipped",
      "progress": 0.35,
      "detail": "installing whisperx 3.3.1" }             // free text, shown verbatim
  ]
}
```
The page treats `queued`/`running` as "keep polling" (~1.2 s) and colours `succeeded`/`failed` with
the existing `st-*` classes.

### `POST /api/setup/pro-ai` — implemented: **no**
```jsonc
// request
{ "components": ["whisperx", "faster-whisper", "realesrgan", "demucs"],
  "allow_download": true }
// response
{ "ok": true } | { "error": "…" }
```
**Component ids are the engine's own ids** — `Component::as_str()` in `src/setup/mod.rs` returns
`"whisperx" | "faster-whisper" | "realesrgan" | "demucs"`. Do not use `faster_whisper` or
`real_esrgan`; the page sends the hyphen/short forms above.

Engine already in the tree (no route yet): `src/setup/mod.rs` — `Component::ALL`, `pro_ai()` →
`SetupReport { components: [ComponentReport { component, venv, installed, problem }], config_path,
complete }`. `cinvar setup --pro-ai` is the CLI equivalent. `SetupReport` is per-component *after*
the fact; live per-component progress needs the route to report incrementally (the current function
returns once at the end).

---

## 5. License activation (tab **Edition & license**)

### `POST /api/license/activate` — implemented: **no** (order key only)
```jsonc
// request
{ "order_key": "CINVAR-XXXX-XXXX-XXXX" }
// response — identical to GET /api/edition, so the panel re-renders without a reload
{ "edition": "pro", "enforcement": "enforce", "source": { "kind": "license", "id": "…" },
  "features": ["export.4k", "…"], "export_limit": "3840x2160 / 2160x3840",
  "modules": ["pro"], "catalog": [ … ], "upgrade_url": "…", "pro_module": true }
// failure
{ "error": "order key not recognised" }      // shown inline, no state change
```
Why it cannot just be a field on the existing route: `POST /api/license` verifies a **signed
licence JSON** (`crate::edition::license::install_text` → `verify()`, `src/edition/license.rs`). A
bare order key is not a signed document, so activating one needs a server-side exchange (look the key
up, fetch/derive the signed licence, then install it). Until that exists the panel keeps the working
JSON path and tells the user to paste the `license.json` instead — verified behaviour, see
"Not available in this build" for `POST /api/license/activate`.

"Unlock with no restart" is already true for the JSON path: `POST /api/license` writes the licence to
disk and the panel re-reads `GET /api/edition` immediately (`edition_status()` re-resolves from the
config each call, and the HTTP layer is stateless per request). No process restart, no page reload.

---

## 6. Blender (tab **Blender**)

Scene creation entry point, asset import, camera + lighting presets, four-view preview, spatial
warnings, render settings, render progress, failed-frame recovery, transparent overlay, GPU/worker
selection.

### `GET /api/blender/preview` — implemented: **no**
One read route for everything the tab displays; polled ~900 ms while a render runs.
```jsonc
// response
{
  "blender": { "available": true, "version": "4.0.2", "path": "/usr/bin/blender" },
  "scene": {
    "id": "scene-1",
    "views": [                                  // names must be exactly these
      { "name": "front", "url": "/api/media/preview/front.png" },
      { "name": "side",  "url": "…" },
      { "name": "persp", "url": "…" },
      { "name": "top",   "url": "…" }
    ]
  },
  "warnings": [ { "severity": "warning|error", "message": "Camera clips the floor",
                  "object": "Camera" } ],
  "render": {
    "job_id": "…", "state": "queued|running|succeeded|failed",
    "progress": 0.3,
    "done_frames": 72, "total_frames": 240,
    "eta_seconds": 96,
    "failed_frames": [87, 88, 91],
    "error": ""
  },
  "gpus":    [ { "id": 1, "name": "Tesla P40", "vram_mb": 24576 } ],
  "workers": [ { "id": "local", "label": "This machine", "state": "idle" } ]
}
```
Any missing sub-object is allowed: the tab then renders "create a scene to see the four views",
"No spatial warnings.", an "auto" GPU and "this machine". A view without `url` shows
"no <view> view rendered yet" — never a stock image.

### `POST /api/blender/scene` — implemented: **no**
```jsonc
// request
{ "template": "logo_reveal|title_3d|empty",
  "text": "CINVAR",
  "asset_ids": ["<asset id>", "…"],           // imported into the scene
  "camera":   { "preset": "orbit|dolly_in|static|handheld" },
  "lighting": { "preset": "studio|softbox|rim|night" },
  "transparent": false }
// response
{ "scene_id": "scene-1" } | { "error": "…" }
```

### `POST /api/blender/render` — implemented: **no**
```jsonc
// request
{ "scene_id": "scene-1",
  "width": 1080, "height": 1920, "fps": 30, "samples": 64,
  "engine": "eevee|cycles",
  "device": "gpu|cpu",
  "gpu_id": 1,                 // null when device is cpu or "auto"
  "worker": "local",           // null for this machine
  "transparent": true,         // alpha overlay for compositing
  "resume": true,              // continue an interrupted render
  "frames": "1-240" }          // null means "all"
// response
{ "job_id": "…" } | { "error": "…" }
```

### `POST /api/blender/recover` — implemented: **no**
Re-render only the frames that failed. The button appears only when `render.failed_frames` is
non-empty.
```jsonc
// request
{ "job_id": "…", "frames": [87, 88, 91] }
// response
{ "requeued": [87, 88, 91] } | { "error": "…" }
```

Engine already in the tree (no route yet): `src/blender/mod.rs` — `AnimationSpec`, `inspect()`,
`detect()`, `RenderControl`, `render_animation_job()`, `BlenderRender` (with `alpha_verified()`),
`png_sequence_source` → `alpha::OverlaySource`; `src/blender/preview.rs` builds and runs a
four-view/preview job with its own warnings (it already errors with
"assign a camera with bpy.context.scene.camera before previewing"). The tools
`cinvar.blender.status` and `cinvar.blender.animation` exist but are **not**
`UI_TOOL_PREFIXES`-whitelisted, so the page cannot use `/api/tool` for them either.
`cinvar.blender.animation` is gated by `Feature::BlenderProduction` (via `tool_feature` in
`src/tools/mod.rs`); `config.security.allow_blender` must also allow it.

---

## 7. Endpoints that already existed (implemented — unchanged by this work)

`Impl.` = present in `src/server/mod.rs` `handle()`. Shapes below list the fields **the page reads**
(grepped from `web/index.html`), not necessarily the full server payload.

| Method | Path | Page sends | Page reads | Impl. |
|---|---|---|---|---|
| GET | `/` | — | the page itself; `<meta name="ui-build">` is injected for the stale-tab notice | yes |
| GET | `/api/status` | — | `open`, `name`, `path`, `revision`, `issues[]`, `ui_build`, `assets[{id,name,kind,seconds,missing}]`, `timelines[{id,name,duration,clips,width,height,fps,tracks[{id,name,kind,muted,clips[{id,name,start,duration,speed,text,effects}]}]}]`, `renders[]` | yes |
| GET | `/api/edition` | — | `edition`, `enforcement`, `source{kind,id,reason,grandfathered}`, `features[]`, `export_limit`, `modules[]`, `catalog[{capability,title,description,unlocked,experimental,reason}]`, `upgrade_url`, `pro_module` | yes |
| POST | `/api/license` | `{license: "<signed json text>"}` | edition status, or `{error}` (400 — verified: *"license file is not valid JSON"*) | yes |
| POST | `/api/license/remove` | `{}` | edition status | yes |
| POST | `/api/tool` | `{name, args}` | tool result; `403` outside `UI_TOOL_PREFIXES`; `{error, code, hint}` otherwise | yes |
| GET | `/api/doctor` | — | `checks[]`/`items[]` (`{name,status,detail}`) or raw JSON fallback | yes |
| POST | `/api/project` | `{name, dir?}` | `{name}`, `{error}` | yes |
| POST | `/api/open` | `{dir}` | `{name}`, `{error}` | yes |
| POST | `/api/chat` | `{message, skills[]}` | `{text, skills[], tool_calls[], error}` | yes |
| POST | `/api/render` | `{preview, timeline_id}` | `{job_id}`, `{error}` | yes |
| GET | `/api/jobs/{id}` | — | `status`, `progress`, `output{output\|file}`, `error{message}` | yes |
| GET | `/api/media/{name}` | — | the media bytes (player + Save links) | yes |
| POST | `/api/upload?name=` | raw file body (`application/octet-stream`) | `{error?}` | yes |
| POST | `/api/model` | `{name}` | `{active}`, `{error}` | yes |
| GET | `/api/skills` | — | `[{name,title,summary}]` | yes |
| GET | `/api/harness` | — | `roles{assistant,script,blender,motion,video_backend,video_workflow,auto_skills}`, `connections[{provider,label,kind,connected,video_only,models[],base_url}]`, `video{workflows[],backends[],settings{enabled,max_job_seconds,max_day_seconds,used_today_seconds}}` | yes |
| POST | `/api/connections` | `{provider, api_key, base_url}` | `{error?}` | yes |
| POST | `/api/connections/remove` | `{provider}` | — | yes |
| POST | `/api/roles` | `{role, provider, model}` / `{role:"video", backend, model}` / `{role:"video", workflow}` | `{error?}` | yes |
| POST | `/api/models/test` | `{name}` | `{ok, message}` | yes |
| POST | `/api/auto_skills` | `{enabled}` | — | yes |
| POST | `/api/videogen/settings` | `{enabled, max_job_seconds, max_day_seconds}` | — | yes |

Routes that exist server-side but that the current page does **not** call:
`GET /api/models`, `POST /api/models/add`, `POST /api/models/key`, `POST /api/models/remove`.

---

## 8. What the page deliberately refuses to do

* No mock data, no optimistic fake progress: every number on screen comes from a response.
* No invented file links. A result row shows a **Save** button only when the backend gave a `url`;
  otherwise the row says "no file". (Shorts files land under `<project>/shorts/` and are **not**
  under `renders/`, so `/api/media/<name>` cannot be assumed to serve them — hence `url` is optional
  and authoritative.)
* No silent success: a 404 route, a 400 validation error and a 500 all surface their own text.
* Empty states say what is missing ("create a scene to see the four views") instead of showing a
  plausible-looking placeholder.
* Offline first: no CDN, no new JavaScript dependency — the page remains one self-contained
  `web/index.html` (the server embeds it with `include_str!`, so a UI change requires a rebuild).
