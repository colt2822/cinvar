# CINVAR Creative Workflows (Sprint II)

All footage is synthetic/local (TTS via local piper, ffmpeg-drawn scenes). Every workflow produced a real playable MP4 verified with ffprobe and QA. Demos live in `demos/sprint2/`.

## A. Intelligent highlight reel with captions — VERIFIED
Input: 12 s TTS-speech clip ("Welcome to CINVAR…"). Command:
```
cinvar auto-edit --media-root footage5 --project workflowA --captions --caption-preset social --vertical --title "Workflow A" --target-secs 10
```
Result: 1080x1920 render, QA passed; faster-whisper (base.en) transcribed the speech; captions **burned into the video** — verified by inspecting an extracted frame (rendered text: "THIS IS THE AUTONOMOUS CAPTION DEMO."). Minor transcription errors are visible and reported honestly ("Sinvar" for "CINVAR").

## B. Cinematic trailer — VERIFIED (semantics + titles; honest gaps)
Input: three labeled scene clips + scene index. Commands:
```
cinvar analyze --media-root footage4 --project workflowB
cinvar auto-edit --instruction "fast chase action" --title "CINVAR" --target-secs 20 --project workflowB
```
Result: QA-passed 20 s render; action.mp4 ranked and placed first via semantic match 0.33; crossfade transitions + title overlay; the opposite brief "calm suspense" produced a different ordering (calm boosted 0.50). Not yet implemented: true escalating pacing curves, music synchronization (no music source in the demo), speed-ramped trailer arcs.

## C. Autonomous repair — VERIFIED
Input: clip with a 3.9 s frozen stretch at 3.07–7.0 s (measured with freezedetect).
```
cinvar auto-edit --media-root footage8 --project repair9 --target-secs 12
```
Result: pass 1 QA FAIL `frozen_frames` → deterministic correction (trim the frozen span, ripple later clips) → pass 2 one more frozen boundary → pass 3 **QA passed**. Corrections recorded per clip in the report. A fully-frozen source (freeze from 0.57 s to EOF) is correctly reported as unrepairable rather than silently "fixed".

## Bugs found and fixed by these workflows
- `freezedetect` never emits `freeze_duration` when a freeze runs to end-of-file → QA could pass a fully frozen render. Fixed (`parse_freeze_spans_with_duration` closes at measured file duration); proven by the repair workflow.
- PyAV ≥ 15 removed `metadata_errors`, breaking the faster-whisper helper. Fixed with a shim in `fw_helper.py`.
