#!/usr/bin/env python3
"""Demucs helper: separate an audio/video file into 4 stems (vocals/drums/bass/other).

Usage: demucs_helper.py <input> <out_dir> [model]

Uses the demucs CLI (python -m demucs.separate) inside the venv named by the invoking
python. Weights download to the HF cache pointed at by HF_HOME (the adapter sets it to
CINVAR_DEMUCS_CACHE and refuses to run without CINVAR_DEMUCS_ALLOW_DOWNLOAD=1).
htdemucs expects ~3 GB VRAM on GPU (use CINVAR_DEMUCS_DEVICE=cuda with CUDA_VISIBLE_DEVICES
pointing at the compute GPU) or ~4 GB RAM on CPU (slower).

JSON on stdout: {"ok": true, "stems": [...], "seconds": T}
"""

import json
import pathlib
import subprocess
import sys
import time


def fail(msg):
    print(msg, file=sys.stderr)
    sys.exit(2)


def main():
    if len(sys.argv) < 3:
        fail("usage: demucs_helper.py <input> <out_dir> [model]")
    src = pathlib.Path(sys.argv[1])
    out_dir = pathlib.Path(sys.argv[2])
    model = sys.argv[3] if len(sys.argv) > 3 else "htdemucs"
    if not src.exists():
        fail(f"input not found: {src}")

    started = time.time()
    cmd = [sys.executable, "-m", "demucs.separate", "-n", model,
           "-o", str(out_dir), str(src)]
    device = None
    try:
        import os
        device = os.environ.get("CINVAR_DEMUCS_DEVICE")
    except Exception:
        pass
    if device:
        cmd += ["-d", device]
    proc = subprocess.run(cmd, capture_output=True, text=True)
    if proc.returncode != 0:
        fail(f"demucs.separate failed: {proc.stderr.strip()[-500:]}")

    # demucs writes <out_dir>/<model>/<input-stem>/<stem>.wav
    stem_dir = out_dir / model / src.stem
    stems = sorted(p.name for p in stem_dir.glob("*.wav")) if stem_dir.is_dir() else []
    if not stems:
        fail(f"no stems produced under {stem_dir}")
    print(json.dumps({"ok": True, "stems": [str(stem_dir / s) for s in stems],
                      "seconds": round(time.time() - started, 2)}))


main()
