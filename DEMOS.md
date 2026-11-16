# CINVAR Publication-Ready Demo Requirements

Scope: what a demo must satisfy before it may be labeled **publishable**.
Anything not meeting every gate below is **INTERNAL TEST ONLY** and must be
labeled as such in the filename and the evidence doc.

## 1. Input rights gate (hard requirement)

- Only rights-cleared input: footage the owner shot, public-domain footage, or
  licensed material whose license explicitly permits redistribution in promo
  demos.
- GTA trailers and any other licensed promo footage are **INTERNAL TEST ONLY,
  NEVER publishable** — keep them out of any public artifact (video, README,
  screenshot).
- TikTok-vault "sports" exports are generated graphics, not footage — QA
  `frozen_frames` will (correctly) reject them.
- Record for every input: source, license basis, file hash, date acquired.

## 2. Pipeline gates

A demo render ships only when the full `auto-edit --json` report shows:

- `qa.passed = true` (file_exists, video_stream, resolution, duration ±0.9 s,
  audio_present, black_frames, black_flash, audio_levels, frozen_frames,
  dead_air).
- `render_passes` and `corrections` recorded honestly (a demo that needed 3
  correction passes is fine — undisclosed correction is not).
- If `--reframe`: output re-detection evidence (detector re-run on the output,
  subject offsets recorded) plus, where feasible, pixel crop-follow spot
  checks. Re-detection center-offset is unreliable in multi-person scenes
  (identity switching) — pair it with a crop-follow check.
- If `--captions`: extract a frame inside each cue window and verify text,
  position (bottom band, fully in-frame), and no face overlap.
- Audio: video/audio duration delta < 50 ms; `-c:a copy` through reframe.

## 3. Evidence to record per demo

1. Input files + hashes + rights basis.
2. Exact command line (all env vars: CINVAR_TRACK_PYTHON, CINVAR_TRACK_DEVICE,
   CINVAR_FASTER_WHISPER_PYTHON, ...).
3. Full `--json` report.
4. Device evidence (nvidia-smi compute-apps with GPU uuid during the run).
5. Frame extractions used for caption/subject checks.
6. Wall-clock timings, CPU and GPU.
7. Known gaps (what the demo does NOT show).

## 4. Labeling

- Publishable demo filename: `demo-<slug>-PUBLIC.mp4`.
- Internal-test filename: `...-INTERNAL_TEST.mp4`.
- Evidence doc states, verbatim, which gate list passed and which input rights
  basis applies.

## Tonight's runs (2026-11-13, V Flash hardening)

All runs below used GTA trailer excerpts (licensed promo) →
**INTERNAL TEST ONLY**, marked in the artifact names and here.

- Integrated reframe via auto-edit: /tmp/cinvar-av-proj renders (QA passed,
  captions verified in-frame, audio sync 10 ms).
- Hard-cut tracking: /tmp/cinvar-cut-reframed2.mp4 (5 cuts detected; crop-jump
  verified by pixel crop-follow, mean diff 21–46/255).
- Benchmarks: 15 s clip — cuda:1 7.0 s vs cpu 7.0 s; 84 s trailer — cuda:1
  32.2 s (337 frames, 190 with person). P40 peak 4644 MiB, uuid-verified.
- Publishable demo: **not produced** — no rights-cleared input is on the box
  yet. Owner action required (see VFLASH_CINVAR.md blockers).
