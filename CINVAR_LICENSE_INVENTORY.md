# CINVAR LICENSE INVENTORY - Sprint III

Third-party components integrated into CINVAR Sprint III. Each entry records the pinned
version, code license, model-weight license (where applicable), and required attribution.
CINVAR is intended to become a commercial product: components with incompatible commercial
terms are gated behind env opt-ins and excluded from distributable releases until resolved.

## PySceneDetect

- Pinned: `scenedetect==0.7.3` (venv `~/.venvs/scenedetect`, Python 3.12)
- Code license: **BSD-3-Clause** (from dist-info; Copyright (C) 2014 Brandon Castellano)
- Runtime deps installed: opencv-python 5.0.0.93 (Apache-2.0), numpy 2.5.4 (BSD-3-Clause),
  click 8.5.1 (BSD-3-Clause), tqdm 4.70.1 (MIT/WTFPL dual), platformdirs 4.14.5 (MIT)
- Model weights: n/a
- Commercial use: permitted under BSD-3-Clause; attribution/notice required ( THIRD_PARTY_NOTICES.md)
- Invocation: optional, env-gated (`CINVAR_SCENEDETECT_PYTHON`); CINVAR builds without it.

---

Third-party components integrated into CINVAR Sprint III (captions worktree).

## WhisperX

- Pinned: `whisperx==3.8.6` (venv `~/.venvs/whisperx`)
- Code license: **BSD-2-Clause** (wheel metadata)
- Model weights: faster-whisper/Whisper base (fetched only with `CINVAR_WHISPERX_ALLOW_DOWNLOAD=1`,
  cache `/mnt/elyreon-data/cinvar-models/whisperx`). Weight terms follow the Hugging Face
  repos (MIT for openai-whisper; verify per selected model before shipping defaults).
- Deps: torch/torchaudio 2.8.0+cu126 (BSD-3, CUDA runtime under NVIDIA terms — runtime use),
  faster-whisper (MIT), pyannote-audio (MIT; NOT exercised — needs an HF token)
- Commercial use: code permitted with BSD-2-Clause notice in THIRD_PARTY_NOTICES.md.

## libass

- Pinned: system ffmpeg 6.1.1 built `--enable-libass` (no vendored copy). libass is
  **ISC**-licensed; used at runtime through ffmpeg's `subtitles` filter.
- ffmpeg itself is GPL-licensed (`--enable-gpl` build) — CINVAR *invokes* the system ffmpeg
  binary as a separate process, which does not impose GPL on CINVAR; shipping ffmpeg
  binaries with a release would require GPL compliance (keep ffmpeg as a runtime dependency).

## PySceneDetect / Real-ESRGAN / Auto-Editor / Demucs

See CINVAR_LICENSE_INVENTORY.md in the scenes / enhance / audio worktrees.

Third-party components integrated into CINVAR Sprint III. Pinned versions, code and
model-weight licenses, attribution requirements. CINVAR is intended for commercial release:
components with unclear commercial terms stay env-gated and out of distributable artifacts.

## Real-ESRGAN

- Pinned: `realesrgan-ncnn-py==2.0.0` (venv `~/.venvs/realesrgan`)
- Code license: **BSD-3-Clause** (package METADATA; upstream media2x/realesrgan-ncnn-vulkan
  is MIT-licensed, Copyright (c) 2021 ArchieMeng — MIT header in the shipped module)
- Model weights (bundled in wheel): `realesrgan-x4plus` — BSD-3-Clause per
  xinntao/Real-ESRGAN LICENSE. `.pth` checkpoints (x4plus v0.1.0, x2plus v0.2.1) downloaded
  to `/mnt/elyreon-data/cinvar-models/realesrgan/`, sha256 recorded, same BSD-3-Clause
  weight license; not used by the ncnn runtime path.
- Deps: opencv-python 5.0.0.93 (Apache-2.0), numpy 2.5.4 (BSD-3), pillow 13.0.0 (MIT-CMU)
- Runtime binary deps: ncnn (MIT), Vulkan loader + NVIDIA ICD (driver terms, runtime use
  only — no redistribution of driver components)
- Commercial use: permitted; attribution/notice required in THIRD_PARTY_NOTICES.md.
- Env gating: `CINVAR_ESRGAN_PYTHON`, `CINVAR_ESRGAN_GPU`; no runtime downloads.

## PySceneDetect

- Pinned: `scenedetect==0.7.3` — **BSD-3-Clause**, Copyright (C) 2014 Brandon Castellano.
  See the scenes-worktree inventory for full dependency list.

Third-party components integrated into CINVAR Sprint III (audio worktree).

## Auto-Editor

- Pinned: `auto-editor==29.3.1` (venv `~/.venvs/auto-editor`)
- Code license: **LGPL-3.0-or-later** per the project LICENSE (WyattBlue/auto-editor).
  LGPL is commercial-use compatible; CINVAR invokes it as a separate process (no linking),
  which keeps LGPL obligations to the tool itself. Notice required in THIRD_PARTY_NOTICES.md.
- Model weights: n/a (signal-analysis only).

## Demucs

- Pinned: `demucs==4.1.0` (venv `~/.venvs/demucs`, torch 2.8.0+cpu)
- Code license: **MIT** (facebookresearch/demucs, Copyright (c) Facebook, Inc. and its
  affiliates)
- Model weights: `htdemucs` (th_comas/demucs release) — MIT per the repo release; weights
  cached at `/mnt/elyreon-data/cinvar-models/demucs` (HF cache). Download gated by
  `CINVAR_DEMUCS_ALLOW_DOWNLOAD=1`; no weights are redistributed with CINVAR.
- Deps: torch/torchaudio (BSD-3; CPU build keeps NVIDIA runtime terms out of this path),
  numpy (BSD-3), soundfile (LGPL-2.1 wrapper over libsndfile)
- Commercial use: permitted; attribution/notice required in THIRD_PARTY_NOTICES.md.
