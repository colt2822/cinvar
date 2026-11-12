#!/usr/bin/env python3
"""Regenerate THIRD_PARTY_NOTICES.md from Cargo.lock metadata: python3 scripts/gen_notices.py"""
import json, subprocess
m = json.loads(subprocess.check_output(["cargo", "metadata", "--format-version", "1", "--locked"]))
pk = sorted((p for p in m["packages"] if p["source"]), key=lambda p: (p["name"], p["version"]))
out = ["# Third-party notices", "",
       "CINVAR itself is Apache-2.0 (see LICENSE and NOTICE). This file lists third-party software it links or invokes.",
       "It is an inventory generated from `Cargo.lock`, not legal advice; the full licence text of each crate is in its own",
       "published package (and in `~/.cargo/registry` after `cargo fetch`). Redistributing a CINVAR binary must reproduce the",
       "notices those licences require.", "",
       "## Rust crates linked into the `cinvar` binary", "",
       "| Crate | Version | Licence (SPDX) |", "|---|---|---|"]
for p in pk:
    out.append(f"| {p['name']} | {p['version']} | {p.get('license') or 'see package'} |")
out += ["", "`r-efi` is offered as `MIT OR Apache-2.0 OR LGPL-2.1-or-later`: CINVAR uses it under MIT.",
        "Two crates carry CDLA-Permissive-2.0 (certificate/root data).", "",
        "## External programs and libraries (run as separate processes; NOT bundled or linked)", "",
        "| Program | Licence | Notes |", "|---|---|---|",
        "| FFmpeg / ffprobe | LGPL-2.1+ or GPL-2.0+ depending on build | most distro builds enable x264/x265 (GPL). Install your own; if you redistribute FFmpeg with CINVAR its terms apply to that bundle |",
        "| Blender | GPL-3.0-or-later | optional; driven by generated scripts |",
        "| ComfyUI | GPL-3.0 | optional separate service over HTTP; its custom nodes and models carry their own licences |",
        "| OpenColorIO | BSD-3-Clause | optional Python helper |",
        "| OpenTimelineIO | Apache-2.0 | optional test/interop helper |",
        "| faster-whisper (+ CTranslate2, onnxruntime, PyAV, tokenizers) | MIT / BSD-3 / Apache-2.0 | optional transcription helper |",
        "| bubblewrap | LGPL-2.0-or-later | optional Linux plugin sandbox |",
        "| RunPod | commercial service | bring your own account |", "",
        "## Model weights", "",
        "CINVAR bundles **no model weights** and downloads none by default. Whisper/faster-whisper models and ComfyUI models are",
        "separate works with their own licences; check them before redistributing.", "",
        "## Trademarks", "",
        "Blender, FFmpeg, RunPod, ComfyUI and other names belong to their owners and are used only to describe compatibility."]
open("THIRD_PARTY_NOTICES.md", "w").write("\n".join(out) + "\n")
print(len(pk), "crates")
