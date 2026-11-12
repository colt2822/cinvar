# Rust engine and FFmpeg: editing, rendering, audio, captions, export

Everything here runs locally with FFmpeg/ffprobe on PATH; no network, account or AI model is needed.

## Projects and timelines
A project is a folder with a versioned `project.json`, copied-in `assets/`, `revisions/` (undo/redo/rollback) and
`renders/`. Source files are copied in and never modified. Timelines have video/audio/text tracks; the engine refuses
overlapping clips, zero-length trims and source ranges beyond the asset instead of silently fixing them.

    cinvar project create --name Demo --dir ~/Videos
    cinvar status --project ~/Videos/demo
    cinvar render --project ~/Videos/demo --output renders/out.mp4 --wait

Edits are normally made through tools (`cinvar.timeline.*`, see [TOOLS.md](TOOLS.md)) by an AI agent over MCP or the
built-in agent loop (`cinvar run "..."` with a model profile). Known gaps: no ripple trim, and scale keyframes do not
render (use the `zoom` effect / `cinvar.timeline.animate` presets).

## Media processing
`cinvar.media.inspect|scenes|frames|contact_sheet|silence|black|proxy`, `cinvar.audio.analyze|clipping|silence_plan`.
Imports are content-validated (container allow-list) before they enter a project.

## Audio
Per-clip gain, fades, mute, loudness normalisation, speech ducking (`cinvar.timeline.clip_audio`).

## Captions
Add cues (`cinvar.timeline.captions`, `captions.from_script`, `captions.import` for SRT/VTT), style them, burn them in
on export (`burn_captions`), export `.srt/.vtt` (`cinvar.captions.export`). Speech-to-text: [TRANSCRIPTION.md](TRANSCRIPTION.md).

## Export
`cinvar render` / `cinvar.render.export`: `codec`, `crf`, `fps`, `width`/`height`, `format`. Hardware encoders are used
when a trial encode at the requested size succeeds, otherwise CPU x264/x265. Exports are durable jobs
(`<project>/jobs/`): duplicates are refused, interrupted jobs are listed and can be retried, jobs can be cancelled.
After a render, `cinvar qa` / `cinvar.qa.render_check` probes the file (duration, resolution, audio, black frames,
clipping). Above 1080p see [EIGHT_K.md](EIGHT_K.md).
