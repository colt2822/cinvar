#!/usr/bin/env python3
"""Real-ESRGAN helper: batch-upscale PNG frames from an input dir into an output dir.

Usage: esrgan_helper.py <input_dir> <output_dir> <scale 2|4> <gpuid> [tilesize]

Uses `realesrgan-ncnn-py` (ncnn/Vulkan). The wrapper needs `libomp.so.5`; the Rust adapter
prepends a shim dir (venv-relative `lib-shim`) to LD_LIBRARY_PATH before launching.

Model mapping (bundled in the wheel):
  scale 2 -> realesr-animevideov3-x2 is NOT the photo model; we use model 4
  (realesrgan-x4plus, scale 4) for 4x and model 0 equivalent is skipped: for scale 2 we
  still run x4plus and downsample the result, so photographic detail matches the
  x4plus look across scales.

JSON on stdout: {"ok": true, "count": N, "gpuid": G, "scale": S, "seconds": T}
Any failure: exits non-zero with a message on stderr (no partial JSON).
"""

import json
import pathlib
import sys
import time

SCALE = int(sys.argv[3]) if len(sys.argv) > 3 else 4
GPUID = int(sys.argv[4]) if len(sys.argv) > 4 else 1
TILESIZE = int(sys.argv[5]) if len(sys.argv) > 5 else 0  # 0 = auto


def fail(msg: str) -> None:
    print(msg, file=sys.stderr)
    sys.exit(2)


try:
    from PIL import Image
    from realesrgan_ncnn_py import Realesrgan
except ImportError as e:
    fail(f"realesrgan-ncnn-py unavailable in this python ({e}); "
         "point CINVAR_ESRGAN_PYTHON at a venv with realesrgan-ncnn-py==2.0.0")


def main() -> None:
    if len(sys.argv) < 3:
        fail("usage: esrgan_helper.py <input_dir> <output_dir> <scale 2|4> <gpuid> [tilesize]")
    if SCALE not in (2, 4):
        fail(f"unsupported scale {SCALE} (use 2 or 4)")

    in_dir = pathlib.Path(sys.argv[1])
    out_dir = pathlib.Path(sys.argv[2])
    if not in_dir.is_dir():
        fail(f"input dir not found: {in_dir}")
    out_dir.mkdir(parents=True, exist_ok=True)

    frames = sorted(in_dir.glob("*.png"))
    if not frames:
        fail(f"no PNG frames in {in_dir}")

    started = time.time()
    # model 4 = realesrgan-x4plus (photographic). For scale 2 we output at 4x then
    # downsample — x2plus has a different look; keep one model for consistency.
    up = Realesrgan(gpuid=GPUID, tilesize=TILESIZE if TILESIZE else 0, model=4)
    count = 0
    for f in frames:
        img = Image.open(f).convert("RGB")
        out = up.process_pil(img)  # always 4x
        if SCALE == 2:
            out = out.resize((img.width * 2, img.height * 2), Image.LANCZOS)
        out.save(out_dir / f.name, compress_level=1)
        count += 1

    print(json.dumps({
        "ok": True,
        "count": count,
        "gpuid": GPUID,
        "scale": SCALE,
        "seconds": round(time.time() - started, 2),
    }))


main()
