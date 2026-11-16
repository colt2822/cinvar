# CINVAR Subject-Aware Reframing — Benchmarks & Evidence (2026-11-13)

Feature: `cinvar reframe <video> [out] --target vertical|square|landscape [--sample-fps 4]`
Commit: 7b7044e. Tests: 363 passed (8 new reframe unit tests).

## How it works
1. Helper (`src/reframe/track_helper.py`, run via `CINVAR_TRACK_PYTHON`, torch+torchvision venv) samples frames at N fps, runs torchvision FastERRCNN ResNet50-FPN (COCO) on a CUDA device (default prefers `cuda:1` = P40; `CINVAR_TRACK_DEVICE` overrides), and emits the highest-confidence person's center per sampled frame (full-res coords). Multi-subject: a second person within 15% score pulls the focal point 35%.
2. Rust (`src/reframe/mod.rs`): converts centers to crop-window left edges, splits the path into per-shot segments at detector jumps (>35% frame width) and detection gaps, simplifies each segment with RDP to ≤60 keyframes (ffmpeg eval-depth safety), and renders a piecewise-linear animated crop via a single ffmpeg `crop=x='…':y='…'` expression to the target aspect.

## Weights
- `fasterrcnn_resnet50_fpn_coco` (~160 MB), downloaded once to `~/.cache/torch/hub/checkpoints/` (manually authorized; CINVAR itself never downloads).

## Benchmarks (Tesla P40, uuid e63ab24f verified)

### Continuous shot (11.0 s, 1080p, moving subject + camera pan; cut from the internal-test trailer)
| Config | Tracked-path mean error | p90 | wall |
|---|---|---|---|
| 2 fps + median-7 (first cut) | 7.4% of frame width | 22.0% | — |
| 2 fps, no median | 5.2% | 15.3% | — |
| **4 fps, no median (shipped default)** | **0.03%** | **0.44%** | 6.0 s (11 s video) |
| Fixed-center crop baseline (output-frame measurement) | 24.5% | — | — |

Higher sampling beats smoothing: median smoothing across samples only adds lag during pans; RDP alone rejects jitter. Default changed to 4 fps + no median on this evidence.

### Montage (90 s trailer, hard cuts every ~1 s)
- Path tracking stays accurate to its own detections, but "subject identity" across cuts is inherently ambiguous; measured offset vs a fixed-center crop showed no reliable win (47.9% vs 45.2%) on montage material. Cut-aware segmentation prevents smoothing across cuts (the earlier median smoothed across scene changes — a real defect found and fixed by benchmarks).
- Honest conclusion: `reframe` is for shot-level subject framing; for montage material, per-shot reframing should be driven by auto-edit scene selection (future work).

### GPU
- Detection: P40 (cuda:1), batches of 8 frames at 960px; ~0.5 s model load, 180 frames in ~20 s.
- Render: CPU ffmpeg (crop+scale, libx264 crf20). RTX 3060 Ti untouched (verified via compute-apps uuid during runs).

## Bugs found & fixed during this work (regression-tested)
1. Crop x/y are window top-left edges, not centers — the first implementation systematically shifted the window right by half its width (caught by output-frame offset measurement).
2. Median smoothing spanning scene cuts dragged the crop between unrelated subjects (caught on montage footage).
3. Deep nested `if()` crop expressions exceed ffmpeg's eval limits — RDP simplification bounds keyframe count (unit test: 180→≤60 points).
4. Escaped commas inside quoted filter values are unnecessary and confusing — removed.

## Verification methodology & limitations
- "Tracked-path error" measures the rendered crop path against the detector's own per-frame subject centers — it proves the render faithfully follows tracking, not that the tracked person is always the human-intended subject. Multi-person scenes can switch the tracked identity between samples; output-frame re-detection measurements therefore conflate detector-choice differences (reported above for honesty).
- All footage: internal-test licensed promo material — NOT publishable.
