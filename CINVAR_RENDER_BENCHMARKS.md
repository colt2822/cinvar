# CINVAR Render Benchmarks

Sprint II, 2026-11-13. Harness: `benchmarks/run_render_bench.py`; raw data `benchmarks/render-benchmarks.json`; commands in `benchmarks/draft.md`.

## Encode-only comparison, 1080p30, 300 frames, 3 runs each (median)

| Config | Median fps | Bitrate (kbps) | PSNR (dB) | SSIM |
|---|---|---|---|---|
| libx264 medium crf23 | 18.1 | 98,032 | 33.92 | 0.936 |
| libx264 veryfast crf23 | 62.1 | 85,107 | 33.00 | 0.913 |
| h264_nvenc p4 cq28 | 243.7 | 11,043 | 27.62 | 0.570 |
| h264_nvenc p4 cq23 | 244.2 | 17,316 | 27.85 | 0.600 |

Independent spot check this sprint (clean 1080p testsrc2 source, single run): libx264 medium 3.64 s / 300 frames = 82 fps; h264_nvenc p4 1.26 s = 238 fps — consistent with the harness's NVENC magnitude and its direction (NVENC substantially faster than x264 medium).

## Quality-matched sweep (run after the initial table)

| Config | fps | PSNR (dB) | SSIM |
|---|---|---|---|
| libx264 medium crf18 | 16.7 | 39.28 | 0.981 |
| h264_nvenc p4 cq14 | 246.3 | 27.85 | 0.599 |
| h264_nvenc p4 cq18 | 244.3 | 27.85 | 0.599 |

**NVENC quality saturates**: lowering CQ from 28 to 14 changes nothing measurable (PSNR 27.85 / SSIM 0.599 at every CQ), while x264 crf18 reaches 39.3 dB / 0.981. On this noisy synthetic source, quality-matched NVENC is **not achievable through CQ alone** — the ~13× speed advantage comes with a large, fixed quality gap. Likely causes worth a follow-up: NVENC tuning/high-bitdepth options, or an artifact of the raw-noise source. Until a follow-up explains the gap, CINVAR quality-critical renders should stay on x264; the render pipeline already falls back to CPU.

## Honest reading
- **Speed:** NVENC p4 is ~3× faster than x264 medium on easy content and ~13× on the harness's noisy synthetic source. Do not compare raw speed across different sources.
- **Quality:** the NVENC rows are NOT quality-matched to x264 (PSNR 27.6 vs 33.9; SSIM 0.57 vs 0.94). CINVAR's default NVENC settings trade real quality for speed. A quality-matched NVENC preset sweep is still outstanding — until then, claims of "NVENC is as good as x264" are NOT supported by our data.
- Encoding was measured separately from decode/filter/AI inference (rawvideo input, `-c:v` encode only).
- Limitations: synthetic source; single machine; encode-only; no 4K/8K or multitrack runs yet in this pass (harness supports adding them; not measured this sprint).

## Prior baseline
CINVAR renders already auto-select h264_nvenc when present (verified: render logs show `encoding 1920x1080 at 30.000 fps with h264_nvenc`), with CPU fallback intact.
