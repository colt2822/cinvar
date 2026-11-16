#!/usr/bin/env python3
"""Subject-tracking helper for CINVAR intelligent reframing.

Samples frames from a video (ffmpeg rawvideo pipe), runs a COCO person detector
(torchvision fasterrcnn) on each sampled frame, and emits a smoothed subject
path as JSON. Runs on one CUDA device chosen by CINVAR_TRACK_DEVICE (default
the device with the most free memory, preferring index >= 1 so desktop GPUs
stay untouched); falls back to CPU.

stdout: one JSON line:
  {"width":W,"height":H,"sample_fps":F,"frames_sampled":N,"frames_with_person":M,
   "keyframes":[{"t":sec,"cx":px,"cy":px,"score":s},...]}
cx/cy are in ORIGINAL video pixel coordinates (center of the tracked subject).
"""
from __future__ import annotations
import json
import os
import subprocess
import sys


def pick_device() -> str:
    dev = os.environ.get("CINVAR_TRACK_DEVICE", "")
    if dev:
        return dev
    try:
        import torch
        n = torch.cuda.device_count()
        if n == 0:
            return "cpu"
        if n > 1:
            return "cuda:1"  # keep desktop GPUs untouched when alternatives exist
        return "cuda:0"
    except Exception:
        return "cpu"


def main() -> int:
    import numpy as np
    import torch
    import torchvision

    inp = sys.argv[1]
    sample_fps = float(sys.argv[2]) if len(sys.argv) > 2 else 2.0
    conf = float(os.environ.get("CINVAR_TRACK_CONF", "0.5"))

    # probe dimensions — corrupt/truncated/zero-byte/audio-only inputs must
    # fail with an actionable message, never a raw traceback
    probe = subprocess.run(
        ["ffprobe", "-v", "error", "-select_streams", "v:0",
         "-show_entries", "stream=width,height", "-of", "csv=p=0", inp],
        capture_output=True, text=True)
    if probe.returncode != 0 or not probe.stdout.strip():
        tail = probe.stderr.strip().splitlines()[-1][:200] if probe.stderr.strip() else "no stderr"
        raise SystemExit(
            f"track helper: '{inp}' has no readable video stream — "
            "the file is missing, corrupt, truncated, or audio-only. "
            f"Re-export it with ffmpeg and retry. ffprobe: {tail}")
    dims = probe.stdout.strip().split(",")
    if len(dims) < 2 or not dims[0].strip().isdigit() or not dims[1].strip().isdigit():
        raise SystemExit(
            f"track helper: '{inp}' has no video stream (audio-only or unsupported codec) — "
            "reframe needs video")
    W, H = int(dims[0]), int(dims[1])

    # analyse at reduced resolution for speed; keep full-res coordinates
    aw = 960
    if W <= aw:
        ah, sw, sh = H, W, H
    else:
        ah = round(H * aw / W / 2) * 2
        sw, sh = aw, ah

    dev = pick_device()
    model = torchvision.models.detection.fasterrcnn_resnet50_fpn(weights="DEFAULT")
    model.to(dev).eval()

    cmd = ["ffmpeg", "-v", "error", "-i", inp, "-vf", f"fps={sample_fps},scale={sw}:{sh}",
           "-f", "rawvideo", "-pix_fmt", "rgb24", "-"]
    proc = subprocess.Popen(cmd, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL)
    frame_bytes = sw * sh * 3

    keyframes: list[dict] = []
    sampled = 0
    with_person = 0
    batch: list[tuple[float, np.ndarray]] = []
    dt = 1.0 / sample_fps

    def flush(batch):
        nonlocal sampled, with_person
        if not batch:
            return
        ts = [b[0] for b in batch]
        imgs = torch.from_numpy(
            np.stack([b[1] for b in batch]).astype(np.float32) / 255.0
        ).permute(0, 3, 1, 2).to(dev)
        with torch.no_grad():
            outs = model(imgs)
        for t, out in zip(ts, outs):
            sampled += 1
            persons = [
                (b.tolist(), s.item())
                for b, s, l in zip(out["boxes"], out["scores"], out["labels"])
                if l == 1 and s > conf
            ]
            if not persons:
                continue
            with_person += 1
            persons.sort(key=lambda p: -p[1])
            # subject = highest-confidence person; secondary persons pull the
            # center only if they are close in score (multi-subject framing)
            (bx, by, bx2, by2), score = persons[0]
            cx, cy = (bx + bx2) / 2, (by + by2) / 2
            if len(persons) > 1 and persons[1][1] > score * 0.85:
                cx2, cy2 = (persons[1][0][0] + persons[1][0][2]) / 2, (persons[1][0][1] + persons[1][0][3]) / 2
                cx, cy = (cx * 0.65 + cx2 * 0.35), (cy * 0.65 + cy2 * 0.35)
            keyframes.append({
                "t": round(t, 3),
                "cx": round(cx * W / sw, 1),
                "cy": round(cy * H / sh, 1),
                "score": round(score, 3),
            })
        torch.cuda.empty_cache() if dev != "cpu" else None

    idx = 0
    while True:
        buf = proc.stdout.read(frame_bytes)
        if len(buf) < frame_bytes:
            break
        arr = np.frombuffer(buf, dtype=np.uint8).reshape(sh, sw, 3)
        batch.append((idx * dt, arr))
        idx += 1
        if len(batch) >= 8:
            flush(batch)
            batch = []
    flush(batch)
    proc.wait()

    print(json.dumps({
        "width": W, "height": H, "sample_fps": sample_fps,
        "frames_sampled": sampled, "frames_with_person": with_person,
        "device": dev, "keyframes": keyframes,
    }))
    return 0


if __name__ == "__main__":
    sys.exit(main())
