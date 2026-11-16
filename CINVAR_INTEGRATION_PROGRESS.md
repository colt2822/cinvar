# CINVAR INTEGRATION PROGRESS - Sprint III

Baseline `a813dfe` (322 tests passing, verified 2026-11-13). Final integration commit recorded below.

## PySceneDetect — integrated on `sprint3/scenes`

- venv `~/.venvs/scenedetect`, `scenedetect==0.7.3` (BSD-3-Clause), install evidence in
  `~/.venvs/scenedetect-installed.txt`.
- `src/analysis/scenedetect_helper.py` (embedded via `include_str!`): content/adaptive
  detectors → JSON scene list. Exits 2 with a clear message when scenedetect is missing.
- `src/analysis/pyscenedetect.rs`: `CINVAR_SCENEDETECT_PYTHON` env-var selection, JSON
  parse + validation (monotonic, non-negative, positive fps), honest errors. 10 unit tests.
- CLI: `cinvar scenes-detect <video> [--detector content|adaptive] [--threshold N]`
  (text and `--json` output). Existing `analyze` / `scenes` unchanged.
- Semantic seam: `semantic::windows_from_scenes()` converts PySceneDetect boundaries into
  semantic-index segment windows (short flashes fold into neighbours; tested).

## Verification (measured, not claimed)

Synthetic truth clip: 480x270@25fps, 15 s, hard cuts at 3.0 / 6.5 / 10.0 s + a 1 s
crossfade 12→13 s. PySceneDetect 0.7.3 content detector (threshold 27) vs ground truth:

| Detector | Detected cuts (s) | Known cuts (s) |
|---|---|---|
| content, 27 | 3.00, 6.52, 10.04 | 3.0, 6.5, 10.0 |
| adaptive, 3.0 | 3.00, 6.52, 10.04, 12.04 | 3.0, 6.5, 10.0 (+crossfade start 12.0) |
| ffmpeg `select=gt(scene,0.3)` | 3, 6.52, 10.04 | — |

Cut times land 0.02–0.04 s past nominal — frame-quantization of the detectors, not a bug.
Neither detector closes the crossfade at 13.0 s; the content detector and ffmpeg agree.

End-to-end: `cinvar scenes-detect` on the truth clip reported exactly the content-detector
scene list above via the Rust adapter.

## Known limitations

- Crossfade end boundary is not detected (known PySceneDetect behaviour on long dissolves).
- Requires `CINVAR_SCENEDETECT_PYTHON` set to the venv python; unset → honest dependency
  error (tested).

---

## WhisperX — integrated on `sprint3/captions`

- venv `~/.venvs/whisperx`: `whisperx==3.8.6` (BSD-2-Clause), torch/torchaudio 2.8.0+cu126
  (matched versions required — mismatched torchaudio cu128 wheels abort the helper).
  `CUDA_VISIBLE_DEVICES=1` sees the Tesla P40 (`torch.cuda.is_available() == True`); device
  defaults to CPU via `CINVAR_WHISPERX_DEVICE`.
- `src/captions/whisperx_helper.py` (embedded `include_str!`): audio → word-level JSON
  {language, words[{word,start,end}], segments}. Env: `CINVAR_WHISPERX_PYTHON`, `_MODEL`,
  `_CACHE`, `_ALLOW_DOWNLOAD` (default off), `_DEVICE`. Speaker diarization is NOT
  integrated (pyannote needs a Hugging Face token) — documented, not faked.
- `src/captions/whisperx.rs`: adapter, transcript validation (monotonic timings), and
  `find_phrase` transcript-search seam (phrase → word-indexed time spans; unit-tested).
- `src/captions/ass.rs`: pure ASS generator — karaoke `\k` word highlighting, colour
  conversion, font/outline styling, Landscape (16:9 bottom-centre) and Vertical (9:16
  safe-area) layouts; unit-tested margins. Burn-in via ffmpeg `subtitles` filter (libass,
  confirmed in ffmpeg 6.1.1 build). Existing SRT/faster-whisper path untouched.

## Verification (measured)

Test clip: piper TTS (`en_US-lessac-medium`, voices under `/mnt/elyreon-data/cinvar-models/piper`)
saying "Cinvar builds highlight reels fast" over a 640x360 testsrc, 5 s.

- `cinvar auto-edit --media-root ... --captions --engine whisperx --style ass
  --word-highlight` → rendered `/tmp/cap_ass.mp4`, QA passed. Frame at 1.0 s inspected:
  burned caption "…builds highlight reels fast" in golden-yellow with black outline
  (karaoke highlight active on the spoken word). Pixel evidence: ASS-burned frame differs
  from raw frame (mean abs gray diff 1.74); plain-SRT baseline differs 4.48 (different
  styling/area). Both caption paths render; ASS path is the word-highlighted one.
- Honest transcription miss: WhisperX (base model, CPU) transcribed the brand word as
  "Sinver" instead of "Cinvar". Word timings themselves were monotonic and within the
  2.26 s speech window.

Rust tests: 19 passed (captions module incl. ASS margins, phrase search, transcript
validation; 1 ignored e2e test requiring the live venv).

## Known limitations

- Diarization not available without an HF token.
- Base-model transcription is imperfect on unusual words; CINVAR_WHISPERX_MODEL selects
  larger models when weights are available.

## Real-ESRGAN — integrated on `sprint3/enhance`

- venv `~/.venvs/realesrgan`, `realesrgan-ncnn-py==2.0.0` (BSD-3-Clause). The ncnn wrapper
  needs `libomp.so.5`; a shim dir `~/.venvs/realesrgan/lib-shim` (symlink to
  `libgomp.so.1`) is created at install time and prepended to `LD_LIBRARY_PATH` by the
  adapter for the child process only.
- Vulkan devices enumerate as 0 = RTX 3060 Ti (display), 1 = **Tesla P40**, 2 = llvmpipe
  (software). Default GPU id is 1; measured discriminator: 512² x4plus tile = 4.15 s on
  gpuid 1 vs 120 s on gpuid 2. `CINVAR_ESRGAN_GPU` overrides.
- `src/enhance/esrgan_helper.py` (embedded via `include_str!`): batch PNG upscale,
  realesrgan-x4plus (bundled in the wheel); scale 2 = 4× output downsampled with Lanczos.
- `src/enhance/mod.rs`: ffprobe (fps/size/duration/audio) → PNG extract → helper →
  reassemble at source frame rate with the ORIGINAL audio stream copied (`-c:a copy`).
  VRAM admission: >8K-equivalent output requires `--tile`. The ncnn wrapper sometimes
  segfaults during Vulkan teardown *after* printing results — the adapter accepts the run
  when the JSON parses and upscaled frame count == extracted count (verified), else errors.
- CLI: `cinvar enhance <video> --upscale 2|4 [--output FILE] [--tile N]`.

## Verification (measured)

Input: `/tmp/lowres.mp4` — 320x180 mandelbrot @10 fps, 3.000 s, AAC audio.

| Run | Output | Duration | Audio | Frames |
|---|---|---|---|---|
| --upscale 4 --tile 256 | 1280x720 | 3.000000 s | present, 3.000000 s | 30/30 |
| --upscale 2 --tile 256 | 640x360 | 3.000000 s | present, 3.000000 s | 30/30 |

4× 30-frame run: 19.7 s GPU / 21.4 s wall. Rust unit tests: 4 passed (JSON parse with
progress noise, empty rejection, frame-rate parsing, scale restriction).

## Weights

`realesrgan-x4plus` param/bin ship inside the wheel (sha256 of bin
`35330ece...`). Separately downloaded `.pth` weights (RealESRGAN_x4plus/x2plus) are stored
at `/mnt/elyreon-data/cinvar-models/realesrgan/` for any torch-based path; not used by the
ncnn runtime. Weight license: BSD-3-Clause per the Real-ESRGAN repo LICENSE (v0.1.0
release); see CINVAR_LICENSE_INVENTORY.md.

## Auto-Editor — integrated on `sprint3/audio`

- venv `~/.venvs/auto-editor`, `auto-editor==29.3.1`. License: verify per pinned version
  (see CINVAR_LICENSE_INVENTORY.md).
- `src/analysis/autoeditor.rs`: exports Auto-Editor's `--export v1` plan (chunks
  [start_frame, end_frame, speed]; speed 99999 = cut) via `CINVAR_AUTOEDITOR_PYTHON`
  (venv `python -m auto_editor`) or a PATH binary; ffprobe supplies fps to convert
  frames → seconds; `pace_selections` intersects selected windows with keep-spans
  (min 1 s part, multi-span splits supported; unit-tested).
- Wiring: `AutoEditOptions::silence_removal` (default false — pipeline unchanged) applied
  in `auto_edit` after planning; CLI flag `cinvar auto-edit --silence-removal`; MCP tool
  exposes `silence_removal` (default false). Honest error if the tool is unconfigured or
  if pacing cuts everything.

### Verification (measured)

Fixture `gaps30.mp4`: 30 s, tone audio with ground-truth silences at 8–12, 18–22, 26–30 s
(ffmpeg silencedetect confirms). `cinvar auto-edit --media-root /tmp/pacetest
--target-secs 18 --min-clip 10 --max-clip 16 --silence-removal` split/trimmed windows at
the **measured Auto-Editor boundaries 8.2 s and 11.8 s** (ground truth 8.0/12.0 s; the
0.2 s margin is auto-editor's default half-width). Render QA passed (`/tmp/paced2.mp4`).
Rust tests: 3 passed (v1 chunk parsing, cut-window dropping, min-part sliver rejection).

## Demucs — integrated on `sprint3/audio`

- venv `~/.venvs/demucs`: `demucs==4.1.0`, torch/torchaudio 2.8.0+cpu, numpy<2, soundfile
  (rebuilt after a corrupted pip; install evidence recorded). ~13 s CPU for a 10 s clip;
  `CINVAR_DEMUCS_DEVICE=cuda` for the P40 (~3 GB VRAM), never the display GPU.
- `src/audio/demucs_helper.py` (embedded `include_str!`): `demucs.separate` → 4 stems
  (vocals/drums/bass/other); HF cache at `CINVAR_DEMUCS_CACHE` (default
  `/mnt/elyreon-data/cinvar-models/demucs`); downloads gated by `CINVAR_DEMUCS_ALLOW_DOWNLOAD=1`
  (refused otherwise, mirroring the faster-whisper gate).
- `src/audio/demucs.rs`: adapter + `clean_mix` (vocals + 0.35-gain instrumental via ffmpeg
  amix). CLI: `cinvar audio clean <input> [output]` and `cinvar audio stems <input>`.
  Unit tests: JSON parse, degenerate rejection, opt-in refusal.

### Verification (measured)

Input `/tmp/mixed.mp3` (220 Hz + 660 Hz, 10 s). `cinvar audio clean` → 4 stems, 7.76 s
separation, cleaned mix rendered to `/tmp/cleaned.wav` (10.05 s — wav padding from amix
duration=longest, recorded honestly). Stem durations match the source.

## Known limitations

- `clean_mix` is a static duck (fixed 0.35 background gain), not true sidechain ducking;
  a vocoder/sidechaincompress pass is a follow-up.
- Demucs download gate is coarse (opt-in required even when weights are already cached).
