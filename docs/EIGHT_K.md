# 8K (and 4K) output

## Two verified paths
1. **FFmpeg export** of a timeline at 7680x4320 (`size_preset: "8k"`). Verified by a test that decodes the result and
   checks exact frame count and dimensions. This is **upscaling by scale filter**, not AI upscaling: 8K output from a
   1080p source is not 8K detail.
2. **Native Blender render** at 7680x4320 (`cinvar.blender.animation` with width/height 7680x4320). Verified by a real
   Blender 5.3.0 test that renders frames at native size.

## Before you start
`cinvar.render.preflight {size_preset:"8k", bit_depth:8|10}` answers whether this machine can do it, which encoder
will be used (trial-encoded at the exact size), and **heuristic** RAM/disk/time estimates (labelled as such; roughly
6-10 GB RAM for FFmpeg 8K, more for 10-bit and for Blender scenes). Requests above 16384 px on an axis are refused,
not clamped.

## Encoder limitations
- `h264_nvenc` fails at 8K on the reference GPU: CINVAR falls back to libx264 (slow). `hevc_nvenc` worked.
- H.264/HEVC patent licensing is the user's/distributor's concern ([LICENSES.md](LICENSES.md)).
- 10-bit output is requested and verified via ffprobe pixel format.

## Verifying output
    cinvar qa --help           # or MCP: cinvar.qa.render_check {path, expected_duration}
    ffprobe -v error -select_streams v:0 -show_entries stream=width,height,pix_fmt -of default=nw=1 out.mp4

Not tested: 8K on Windows/macOS, GPU memory limits of other cards, HDR metadata.
