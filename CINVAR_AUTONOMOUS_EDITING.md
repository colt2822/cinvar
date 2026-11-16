# CINVAR Autonomous Editing

## What it does
`cinvar auto-edit --media-root DIR [--target-secs 60] [--vertical] [--title "..."] [--project DIR]`
takes raw footage, analyses it, builds an edit, renders it, inspects the render with QA, and applies bounded corrections.

The same capability is available to agents as the MCP tool `cinvar.ai.auto_edit`.

## Pipeline (deterministic, no model, no network)
1. **Import** every video in the folder (content-checked, hashed, deduped by the project store).
2. **Score highlights** per file on a 2 s grid: 0.45 × scene-cut density + 0.55 × audio energy (existing `analysis::score_highlights`).
3. **Select** windows greedily (best score first) in round-robin file order so no single source dominates; per-file source budget tracked; clip length clamped to `[min_clip, max_clip]`.
4. **Order** the reel interleaved across files, chronological within each file, deterministic by import order.
5. **Build** the timeline: back-to-back clips with 0.5 s crossfades, per-clip audio normalisation, optional title overlay.
6. **Render** through the standard FFmpeg pipeline (hardware encoder when available).
7. **Inspect** with `qa::validate_render`.
8. **Correct** if QA fails and revisions remain: black flash → trim the offending clip's out point by 0.05 s; audio too quiet/clipping → ±6 dB; dead air → fade tails. Bounded by `--max-revisions` (default 2). Corrections are recorded and returned.

The timeline remains a normal editable CINVAR project: every selection, clip and correction survives save/reload/render and can be revised by hand or by an agent.

## Verified evidence
- 60 s reel from 5 synthetic 1280x720 clips: 10 clips, QA passed, ffprobe-verified MP4.
- Vertical 1080x1920 reel: QA passed.
- Too-quiet footage (−71.6 dBFS mean): pass 1 QA `audio_levels` FAIL → +6 dB correction applied → re-render QA passed (3 render passes total).
- MCP: `cinvar.ai.auto_edit` over stdio built a 30 s timeline in an existing project; the project then rendered with `cinvar render --wait` (h264_nvenc) and scored 1.000 on QA.
- Security: the MCP tool refuses media roots outside the allowed roots (verified live).

## Reproduce
```
cd ~/cinvar
./target/release/cinvar auto-edit --media-root <folder of clips> --project /tmp/reel --title "Demo Reel"
./target/release/cinvar qa --project /tmp/reel /tmp/reel/renders/reel-autoedit-r*.mp4
```
