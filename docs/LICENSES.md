# Licence and dependency review (inventory, NOT a legal opinion)

## Rust crates (statically linked into `cinvar`)
146 third-party crates from `Cargo.lock` (rescanned 2026-11-11 after the rusqlite bump; list in `~/CINVAR_ASCENSION_II_EVIDENCE/licenses.txt`); `scripts/package.sh` writes `THIRD_PARTY_CRATES.txt` with exact versions and SPDX identifiers.
All are permissive (MIT / Apache-2.0 / BSD / ISC / Zlib / Unicode-3.0 / Unlicense / CDLA-Permissive-2.0 / 0BSD). `r-efi` is `MIT OR Apache-2.0 OR LGPL-2.1-or-later` (choose MIT). Redistribution obligation: reproduce notices.
CDLA-Permissive-2.0 (2 crates, certificate/root data) should be reviewed by counsel.

## Known-vulnerability scan
`cargo audit` clean on the committed `Cargo.lock` (2026-11-11, after bumping rusqlite to 0.42 for RUSTSEC-2026-0459). CI runs it on every push. Python helper environments are unscanned.

## External programs (separate processes; not linked, not bundled)
| Program | Version tested | Licence | Notes |
|---|---|---|---|
| FFmpeg | 6.1.1-3ubuntu5 (distro build) | **GPL** (`--enable-gpl`, libx264, libx265) | Redistributing a GPL FFmpeg alongside CINVAR needs source offer; users installing their own avoids this. LGPL-only builds lack x264/x265. Patent/codec licensing (H.264/HEVC) is the distributor's/user's question. |
| Blender | 5.3.0 (sha256 verified) | GPL-3.0-or-later | Driven as a subprocess with generated scripts. Not bundled. |
| ComfyUI | not installed | GPL-3.0 | Separate service over HTTP. Custom nodes/models carry their own licences. |
| OpenColorIO | 2.6.0 (PyPI wheel) | BSD-3-Clause | Via `CINVAR_OCIO_PYTHON`; built-in ACES config files have their own licences (see the OCIO distribution). |
| OpenTimelineIO | 0.18.1 | Apache-2.0 | Test/interop helper only. |
| faster-whisper | 1.2.1 | MIT | + ctranslate2 4.9.0 (MIT), onnxruntime 1.31.1 (MIT), PyAV 16.1.0 (BSD-3; wraps FFmpeg libs), numpy 2.5.4 (BSD-3 AND 0BSD AND MIT AND Zlib AND CC0-1.0), tokenizers 0.23.3 (Apache-2.0), huggingface_hub 2.4.0 (Apache-2.0). Needs **PyAV < 17** (av 19 breaks it). |
| bubblewrap | 0.9.0 | LGPL-2.0-or-later | Used if present; not bundled. |
| RunPod | service | commercial terms | User's own account/key. |

## Model weights
- `Systran/faster-whisper-tiny.en` (CTranslate2 conversion of OpenAI Whisper tiny.en) was downloaded for tests only to `~/cinvar-deps/fw-models` (75 MB). The local cache contains **no licence file**; Whisper is MIT upstream and the conversion repo's card states MIT, but this was **not verified from the files**. Unresolved - verify before documenting redistribution. **CINVAR bundles no model weights** and faster-whisper never downloads unless `CINVAR_FASTER_WHISPER_ALLOW_DOWNLOAD=1`.
- DeepFilterNet, WhisperX, AI upscalers: not integrated; licences not reviewed.

## Test fixtures
`CINVAR_TEST_SPEECH` points at a locally generated TTS clip owned by the operator; it is not in the repository.

## Unresolved legal questions
1. GPL FFmpeg/Blender coexistence is by process boundary (arms-length); counsel should confirm for a commercial bundle.
2. Codec patent licensing for H.264/HEVC distribution.
3. Model weight redistribution rights (above).
4. CDLA-Permissive-2.0 data crates; trademark use of "Blender", "RunPod", "ComfyUI".
