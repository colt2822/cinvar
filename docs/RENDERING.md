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

## Platform notes (fonts, subtitles, ducking)

- **Fonts.** `drawtext` is given an explicit `fontfile=` wherever a font file can be resolved. On Windows a family
  name such as `font=Arial` is turned into `%WINDIR%\Fonts\arial.ttf` (see `src/render/fonts.rs`); the FFmpeg builds used
  there ship without a fontconfig configuration and *crash* (exit status `0xC0000005`, after
  `Fontconfig error: Cannot load default config file`) instead of reporting that they cannot resolve a family. Font paths
  are quoted for the filtergraph with forward slashes and an escaped drive colon. A family that has no Windows font file
  mapped to it falls back to the system default font file rather than to fontconfig.
- **Subtitles.** `.srt`/`.vtt` files are decoded with UTF-16 (either byte order, BOM required) and UTF-8 with a
  byte-order mark in addition to plain UTF-8/CRLF text, because Windows editors write all three.
- **Speech ducking.** The compressor's inputs are padded two seconds past the render duration and its output is trimmed
  back afterwards. FFmpeg 8.0.x ends `sidechaincompress` output early and nondeterministically (up to ~1.2 s of the music
  bed, measured on 8.0.1 while 6.1 and 8.1 are exact); with the padding the loss lands past the last frame that is kept.
