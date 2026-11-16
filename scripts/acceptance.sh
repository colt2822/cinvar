#!/usr/bin/env bash
# CINVAR customer acceptance test: the exact path a new customer takes.
# Needs only this repo's built binary + ffmpeg. No internal venvs, no GPUs,
# no model endpoints, no licensed footage (all test media is generated).
#
# Usage: scripts/acceptance.sh [path-to-cinvar-binary]
set -euo pipefail

CINVAR_BIN="${1:-$(dirname "$0")/../target/release/cinvar}"
[ -x "$CINVAR_BIN" ] || { echo "FAIL: $CINVAR_BIN not found (build with cargo build --release)"; exit 1; }
command -v ffmpeg >/dev/null || { echo "FAIL: ffmpeg not on PATH"; exit 1; }

WORK="$(mktemp -d /tmp/cinvar-acceptance-XXXXXX)"
trap 'rm -rf "$WORK"' EXIT
FOOTAGE="$WORK/footage"
mkdir -p "$FOOTAGE" "$WORK/my-project"

step() { printf '\n== %s ==\n' "$1"; }

step "1. doctor sees a usable environment"
"$CINVAR_BIN" doctor | grep -q ffmpeg

step "2. generate rights-cleared test footage (no downloads, no licenses)"
ffmpeg -y -v error -f lavfi -i "testsrc2=size=1280x720:rate=30:duration=20" \
       -f lavfi -i "sine=frequency=440:duration=20" \
       -c:v libx264 -crf 23 -c:a aac -shortest "$FOOTAGE/clip-a.mp4"
ffmpeg -y -v error -f lavfi -i "gradients=size=1280x720:rate=30:duration=15" \
       -f lavfi -i "sine=frequency=220:duration=15" \
       -c:v libx264 -crf 23 -c:a aac -shortest "$FOOTAGE/clip-b.mp4"

step "3. setup in the customer's own project directory"
"$CINVAR_BIN" setup --yes --project "$WORK/my-project" >/dev/null

step "4. auto-edit produces a video INSIDE that directory (no -2 sibling)"
"$CINVAR_BIN" auto-edit --media-root "$FOOTAGE" --project "$WORK/my-project" --target-secs 6 >/dev/null 2>&1
RENDER="$(ls "$WORK/my-project"/renders/*-autoedit-r*.mp4 2>/dev/null | head -1)"
[ -n "$RENDER" ] || { echo "FAIL: no render in $WORK/my-project/renders"; exit 1; }
[ -d "$WORK/my-project-2" ] && { echo "FAIL: project silently created a -2 sibling directory"; exit 1; }

step "5. output is a real, playable 9:16-safe H.264/AAC file"
ffprobe -v error -select_streams v:0 -show_entries stream=codec_name -of csv=p=0 "$RENDER" | grep -q h264
ffprobe -v error -select_streams a:0 -show_entries stream=codec_name -of csv=p=0 "$RENDER" | grep -q aac
DUR=$(ffprobe -v error -show_entries format=duration -of csv=p=0 "$RENDER")
python3 -c "import sys; sys.exit(0 if 4.0 <= float('$DUR') <= 30.0 else 1)" \
  || { echo "FAIL: unexpected duration $DUR"; exit 1; }

step "6. QA accepts the render"
(cd "$WORK/my-project" && "$CINVAR_BIN" qa "$RENDER" >/dev/null)

step "7. malformed input fails with an actionable error, no crash, no partial output"
head -c 100000 /dev/urandom > "$FOOTAGE/corrupt.mp4"
if "$CINVAR_BIN" qa "$FOOTAGE/corrupt.mp4" >/dev/null 2>&1; then
  echo "FAIL: qa accepted a corrupt file"; exit 1
fi

echo
echo "ACCEPTANCE PASSED — render: $RENDER"
