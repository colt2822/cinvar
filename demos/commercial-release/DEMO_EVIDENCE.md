# CINVAR Commercial Release — Demo Evidence (2026-11-13)

All runs by Flash 5.3 on the sprint machine. Tesla P40 only; RTX 3060 Ti untouched.

## DEMO A — Podcast Short (vertical, captions)
- Input: `spoken-input.mp4` — 90.0 s of spoken narration (TTS-generated, owned) over real footage bed.
- Command: `CINVAR_FASTER_WHISPER_PYTHON=~/.venvs/faster-whisper/bin/python ./target/release/cinvar auto-edit --media-root . --project project --target-secs 60 --vertical --captions --caption-preset social --json`
- Output: `project/renders/project-autoedit-r3.mp4` — 1080x1920, 60.000 s, QA passed, score 1.0, 1 render pass.
- Caption verification: frame at 20 s shows burned-in social-style captions matching narration ("...waste six attribute points... steal or block").
- Speech engine: faster-whisper (local, `~/.venvs/faster-whisper`), no network download.

## DEMO B — Action Highlight Reel (16:9)
- Input: `input2/trailer1_INTERNAL_TEST.mp4` — GTA VI Trailer 1 (promotional trailer footage on the owner's machine). **INTERNAL TEST ONLY — licensed promotional material, NOT cleared for commercial redistribution or demo publication.**
- Command: `./target/release/cinvar auto-edit --media-root input2 --project project --target-secs 45 --title "Pipeline Validation Reel (INTERNAL TEST)" --json`
- Output: `project/renders/project-autoedit-r3.mp4` — 1920x1080, 45.000 s, 8 selected clips, QA passed score 1.0, 0 corrections needed, 1 render pass.
- First run exposed a real bug: selections on short files overran asset duration (fixed in commit 213b3e8, regression test added; 356 tests pass).

## DEMO C — Restoration (Real-ESRGAN, P40)
- Input: `degraded-480p.mp4` — 6.000 s real footage, deliberately degraded to 480x270 @ crf 40.
- Command: `CINVAR_ESRGAN_PYTHON=~/.venvs/realesrgan/bin/python cinvar enhance --upscale 4 --json degraded-480p.mp4 enhanced-1920p.mp4`
- Output: 1920x1080, duration preserved exactly (6.000 s), audio stream intact.
- Measurable quality: Laplacian variance (5 sampled frames, normalized 960x540): degraded ~23–27 → enhanced ~86–92 (~3.6x sharper).
- GPU: Vulkan ncnn device 1 = Tesla P40 (code default keeps renders off the 3060 Ti). gpu_seconds 257.2, wall 265.1 s for 180 frames.
- GPU identity verified via nvidia-smi during Demo B render + ollama placement check: only GPU 1 (uuid e63ab24f…) carried sprint workloads.

## Non-detections / QA refusals that validated the gate
- The ELYREON TikTok-vault "sports" exports turned out to be generated graphics (static fractal art). auto-edit's QA correctly failed them on `frozen_frames` and refused to declare success — the QA gate is not a rubber stamp.

## Not yet complete (honest gaps)
- No truly multi-minute human-spoken footage existed on the machine; Demo A uses owned TTS narration as speech source.
- Demo B footage is licensed promotional material usable for internal validation only; a rights-cleared customer-like source is needed before publishing demos.
- Subject-aware reframing (SAM2/Grounding-DINO) not exercised this session; vertical canvas used center/scale path.
