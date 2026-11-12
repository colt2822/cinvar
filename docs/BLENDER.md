# Blender integration

CINVAR drives Blender as a **subprocess** (it is not bundled; Blender is GPL-3.0-or-later).

## Detect / install
Install Blender 3.6+ (verified on **5.3.0**), or point `CINVAR_BLENDER=/path/to/blender`. `cinvar doctor` and
`cinvar.blender.status` report what was found. Blender use can be disabled with `security.allow_blender = false`.

## What agents can do
`cinvar.blender.animation {template, text, color, width, height, fps, duration, place_at, timeline_id}` with a
**parameterised template** (`logo_reveal`, `title_3d`). CINVAR generates the Blender Python itself from validated
parameters; agents cannot submit arbitrary Python. Frames render to a hash-keyed job directory (resumable and
cancellable), are encoded, imported as an asset and optionally placed on the timeline.

## Limits
- Only the two templates exist; add more by extending `src/blender/mod.rs` (good first issue).
- No compositing/VFX passes or image-sequence import yet.
- Native 8K works (see [EIGHT_K.md](EIGHT_K.md)) but is slow and memory-hungry.
- Tests: 4 real-Blender tests run with `CINVAR_BLENDER` set (`cargo test --test blender_real -- --ignored`).
