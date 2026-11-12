#!/usr/bin/env bash
# Generates deterministic synthetic footage so the examples run without personal media.
set -euo pipefail
OUT="$1"
[ -n "$OUT" ] || OUT="footage"
mkdir -p "$OUT"

echo "==> generating $OUT/clip-1.mp4 (5s, 1080p)"
ffmpeg -hide_banner -loglevel error -y \
  -f lavfi -i "color=c=0x102040:s=1920x1080:d=5:r=30,drawtext=text='SHOT 1':fontcolor=white:fontsize=140:x=(w-text_w)/2:y=(h-text_h)/2" \
  -f lavfi -i "sine=frequency=320:duration=5" \
  -c:v libx264 -preset ultrafast -pix_fmt yuv420p -c:a aac -shortest "$OUT/clip-1.mp4"

echo "==> generating $OUT/clip-2.mp4 (4s, 1080p)"
ffmpeg -hide_banner -loglevel error -y \
  -f lavfi -i "color=c=0x301020:s=1920x1080:d=4:r=30,drawtext=text='SHOT 2':fontcolor=white:fontsize=140:x=(w-text_w)/2:y=(h-text_h)/2" \
  -f lavfi -i "sine=frequency=440:duration=4" \
  -c:v libx264 -preset ultrafast -pix_fmt yuv420p -c:a aac -shortest "$OUT/clip-2.mp4"

echo "==> generating $OUT/clip-3.mp4 (6s, 1080p)"
ffmpeg -hide_banner -loglevel error -y \
  -f lavfi -i "color=c=0x103020:s=1920x1080:d=6:r=30,drawtext=text='SHOT 3':fontcolor=white:fontsize=140:x=(w-text_w)/2:y=(h-text_h)/2" \
  -f lavfi -i "sine=frequency=520:duration=6" \
  -c:v libx264 -preset ultrafast -pix_fmt yuv420p -c:a aac -shortest "$OUT/clip-3.mp4"

echo "==> generating $OUT/interview.wav (12s)"
ffmpeg -hide_banner -loglevel error -y \
  -f lavfi -i "sine=frequency=220:duration=12" \
  -af "volume=0.6,tremolo=f=2:d=0.8" \
  -c:a pcm_s16le "$OUT/interview.wav"

echo "==> generating $OUT/logo.png (512x512)"
ffmpeg -hide_banner -loglevel error -y \
  -f lavfi -i "color=c=black:s=512x512:d=1" -frames:v 1 "$OUT/logo.png"

ls -la "$OUT"
echo "==> footage ready in $OUT"
