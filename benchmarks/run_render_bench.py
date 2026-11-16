#!/usr/bin/env python3
"""Reproducible 1080p H.264 encode benchmark: libx264 (CPU) vs h264_nvenc (GPU).

Usage:  python3 benchmarks/run_render_bench.py [--frames 300] [--runs 3] [--workdir DIR]

- Source is generated deterministically (testsrc2 + seeded noise) once as raw
  yuv420p, so decode cost is excluded and both encoders see identical input.
- Each config is run `--runs` times; wall time of the ffmpeg encode process is
  measured (output written to a scratch file, then deleted).
- Quality (PSNR / SSIM vs. the raw source) is measured on a separate decode pass
  that is NOT included in the timing.
- Writes benchmarks/render-benchmarks.json (machine-readable) and
  benchmarks/draft.md (human-readable draft).
"""
import argparse
import json
import os
import platform
import re
import shutil
import statistics
import subprocess
import sys
import time
from pathlib import Path

W, H, FPS = 1920, 1080, 30
HERE = Path(__file__).resolve().parent

# (name, encoder, args) -- x264 and nvenc settings are the ones compared.
CONFIGS = [
    ("libx264_medium_crf23", "libx264", ["-preset", "medium", "-crf", "23"]),
    ("libx264_veryfast_crf23", "libx264", ["-preset", "veryfast", "-crf", "23"]),
    ("h264_nvenc_p4_cq28", "h264_nvenc", ["-preset", "p4", "-rc", "vbr", "-cq", "28", "-b:v", "0", "-gpu", "0"]),
    ("h264_nvenc_p4_cq23", "h264_nvenc", ["-preset", "p4", "-rc", "vbr", "-cq", "23", "-b:v", "0", "-gpu", "0"]),
    ("h264_nvenc_p4_cq18", "h264_nvenc", ["-preset", "p4", "-rc", "vbr", "-cq", "18", "-b:v", "0", "-gpu", "0"]),
    ("h264_nvenc_p4_cq14", "h264_nvenc", ["-preset", "p4", "-rc", "vbr", "-cq", "14", "-b:v", "0", "-gpu", "0"]),
    ("libx264_medium_crf18", "libx264", ["-preset", "medium", "-crf", "18"]),
]
HEADLINE = {"x264": "libx264_medium_crf23", "nvenc": "h264_nvenc_p4_cq28"}


def run(cmd, **kw):
    return subprocess.run(cmd, capture_output=True, text=True, **kw)


def sh(cmd):
    r = run(cmd)
    return r.stdout.strip() if r.returncode == 0 else f"ERR({r.returncode}): {r.stderr.strip()[:200]}"


def make_source(path: Path, frames: int):
    """Deterministic raw yuv420p source: testsrc2 with seeded noise overlay."""
    dur = frames / FPS
    cmd = [
        "ffmpeg", "-y", "-hide_banner", "-loglevel", "error",
        "-f", "lavfi", "-i", f"testsrc2=size={W}x{H}:rate={FPS}:duration={dur}",
        "-f", "lavfi", "-i", f"nullsrc=size={W}x{H}:rate={FPS}:duration={dur},geq=random(1)*255:128:128",
        "-filter_complex", "[0:v][1:v]blend=all_mode=average:all_opacity=0.35,format=yuv420p[v]",
        "-map", "[v]", "-frames:v", str(frames),
        "-f", "rawvideo", "-pix_fmt", "yuv420p", str(path),
    ]
    r = run(cmd)
    if r.returncode != 0:
        sys.exit(f"source generation failed: {r.stderr[-500:]}")


def encode_cmd(src: Path, out: Path, enc: str, args: list, frames: int):
    return (["ffmpeg", "-y", "-hide_banner", "-loglevel", "error",
             "-f", "rawvideo", "-pix_fmt", "yuv420p", "-s", f"{W}x{H}", "-r", str(FPS),
             "-i", str(src), "-frames:v", str(frames),
             "-c:v", enc] + args + ["-pix_fmt", "yuv420p", str(out)])


def quality(out: Path, src: Path, frames: int):
    """PSNR/SSIM of decoded output vs raw source. Not timed."""
    cmd = ["ffmpeg", "-hide_banner", "-nostats",
           "-i", str(out),
           "-f", "rawvideo", "-pix_fmt", "yuv420p", "-s", f"{W}x{H}", "-r", str(FPS),
           "-i", str(src),
           "-filter_complex", "[0:v][1:v]psnr=stats_file=-;[0:v][1:v]ssim=stats_file=-",
           "-frames:v", str(frames), "-f", "null", "-"]
    r = run(cmd)
    text = r.stderr + r.stdout
    psnr = re.findall(r"average:([\d.inf]+)", text)
    ssim = re.findall(r"All:([\d.]+)", text)
    return {
        "psnr_avg_db": float(psnr[0]) if psnr else None,
        "ssim_all": float(ssim[0]) if ssim else None,
    }


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--frames", type=int, default=300)
    ap.add_argument("--runs", type=int, default=3)
    ap.add_argument("--workdir", default=os.environ.get("TMPDIR", "/tmp"))
    a = ap.parse_args()

    work = Path(a.workdir) / "cinvar-bench"
    work.mkdir(parents=True, exist_ok=True)
    src = work / f"src_{W}x{H}_{FPS}fps_{a.frames}f.yuv"

    t0 = time.time()
    make_source(src, a.frames)
    src_gen_s = round(time.time() - t0, 2)

    results = []
    for name, enc, args in CONFIGS:
        out = work / f"{name}.mp4"
        cmd = encode_cmd(src, out, enc, args, a.frames)
        times = []
        for _ in range(a.runs):
            if out.exists():
                out.unlink()
            t = time.perf_counter()
            r = run(cmd)
            elapsed = time.perf_counter() - t
            if r.returncode != 0:
                sys.exit(f"{name} failed: {r.stderr[-500:]}")
            times.append(elapsed)
        size = out.stat().st_size
        q = quality(out, src, a.frames)
        med = statistics.median(times)
        results.append({
            "name": name,
            "encoder": enc,
            "args": " ".join(args),
            "command": " ".join(cmd),
            "frames": a.frames,
            "runs_sec": [round(x, 3) for x in times],
            "median_sec": round(med, 3),
            "fps_median": round(a.frames / med, 2),
            "fps_min": round(a.frames / max(times), 2),
            "fps_max": round(a.frames / min(times), 2),
            "output_bytes": size,
            "bitrate_kbps": round(size * 8 / (a.frames / FPS) / 1000, 1),
            **q,
        })
        out.unlink(missing_ok=True)
        print(f"{name}: median {round(a.frames/med,2)} fps, {round(size*8/(a.frames/FPS)/1000,1)} kbps, "
              f"PSNR {q['psnr_avg_db']}, SSIM {q['ssim_all']}", flush=True)

    by = {r["name"]: r for r in results}
    x264_fps = by[HEADLINE["x264"]]["fps_median"]
    nvenc_fps = by[HEADLINE["nvenc"]]["fps_median"]

    env = {
        "ffmpeg": sh(["ffmpeg", "-version"]).splitlines()[0] if shutil.which("ffmpeg") else None,
        "gpus": sh(["nvidia-smi", "--query-gpu=index,name,driver_version", "--format=csv,noheader"]),
        "cpu": sh(["bash", "-c", "lscpu | grep 'Model name' | head -1"]),
        "cpu_threads": os.cpu_count(),
        "os": platform.platform(),
        "source": f"testsrc2 {W}x{H} {FPS}fps + seeded geq noise blend, raw yuv420p, {a.frames} frames",
        "source_gen_sec": src_gen_s,
    }

    data = {
        "measured_numbers": True,
        "headline": {
            "x264_config": HEADLINE["x264"],
            "nvenc_config": HEADLINE["nvenc"],
            "note": "Speed is encode-only wall time of ffmpeg on raw input at 1080p. "
                    "Quality is PSNR/SSIM vs. source; the two headline configs are NOT quality-matched "
                    "(compare the cq23 nvenc row against crf23 x264 for a closer match).",
        },
        "x264_fps_1080p": x264_fps,
        "nvenc_fps_1080p": nvenc_fps,
        "speedup_nvenc_over_x264_medium": round(nvenc_fps / x264_fps, 2),
        "results": results,
        "environment": env,
    }
    (HERE / "render-benchmarks.json").write_text(json.dumps(data, indent=2) + "\n")

    lines = [
        "# Render benchmark: h264_nvenc vs libx264 (draft)", "",
        f"- Host: {env['cpu']}, {env['cpu_threads']} threads; GPUs: {env['gpus'].replace(chr(10), '; ')}",
        f"- {env['ffmpeg']}",
        f"- Source: {env['source']}",
        f"- Runs per config: {a.runs} (median reported). Encode-only timing.", "",
        "| Config | Encoder | Median fps | fps range | Bitrate (kbps) | PSNR (dB) | SSIM |",
        "|---|---|---|---|---|---|---|",
    ]
    for r in results:
        lines.append(f"| {r['name']} | {r['encoder']} | {r['fps_median']} | {r['fps_min']}–{r['fps_max']} | "
                     f"{r['bitrate_kbps']} | {r['psnr_avg_db']} | {r['ssim_all']} |")
    lines += ["", "## Commands", ""]
    lines += [f"- `{r['command']}`" for r in results]
    lines += ["", "## Caveats", "",
              "- Single synthetic clip; real footage (motion, film grain) will change absolute numbers.",
              "- Headline pair (x264 medium crf23 vs nvenc p4 cq28) is a speed comparison, not quality-matched.",
              "- nvenc uses GPU 0 as reported by nvidia-smi (see environment.gpus); verify it is the RTX 3060 Ti.",
              ""]
    (HERE / "draft.md").write_text("\n".join(lines))
    print(json.dumps({"x264_fps_1080p": x264_fps, "nvenc_fps_1080p": nvenc_fps}))


if __name__ == "__main__":
    main()
