#!/usr/bin/env bash
# Clone -> build -> run -> produce a video. No accounts, no network, no AI model needed.
#
#   ./examples/quickstart.sh [output-dir]      (default: ./quickstart-out, git-ignored)
#
# Requires: Rust (cargo) and FFmpeg/ffprobe on PATH. Uses an isolated config directory so it
# never touches your real CINVAR settings. The offline "mock" planner makes the edit
# deterministically; it is a smoke test of the real engine, not an AI.
set -euo pipefail
cd "$(dirname "$0")/.."
OUT_ARG="${1:-quickstart-out}"
mkdir -p "$OUT_ARG"
OUT="$(cd "$OUT_ARG" && pwd -P)"
command -v ffmpeg >/dev/null && command -v ffprobe >/dev/null || { echo "FFmpeg + ffprobe are required (apt/brew/choco install ffmpeg)"; exit 1; }
command -v cargo >/dev/null || { echo "Rust is required: https://rustup.rs"; exit 1; }

echo "==> building"
cargo build --release --quiet
BIN="$PWD/target/release/cinvar"
export CINVAR_CONFIG_DIR="$OUT/config"
rm -rf "$OUT"; mkdir -p "$OUT"

"$BIN" setup --yes >/dev/null
"$BIN" models add mock --provider mock --model mock-planner >/dev/null
./examples/make_footage.sh "$OUT/footage" >/dev/null
"$BIN" project create --name Quickstart --dir "$OUT" >/dev/null
( cd "$OUT/quickstart" && "$BIN" run --media-root "$OUT/footage" "Combine the clips in $OUT/footage and export a 1080p video" )

VIDEO="$(ls "$OUT"/quickstart/renders/*.mp4 | head -1)"
echo "==> verifying with ffprobe"
ffprobe -v error -select_streams v:0 -show_entries stream=codec_name,width,height -show_entries format=duration -of default=nw=1 "$VIDEO"
echo "QUICKSTART OK: $VIDEO"
