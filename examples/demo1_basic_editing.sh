#!/usr/bin/env bash
# Demo 1 (+ revision): build a project, assemble a cut from synthetic footage, render it.
set -euo pipefail
cd "$(dirname "$0")/.."
BIN="$CINVAR_BIN"
[ -n "$BIN" ] || BIN="target/release/cinvar"
[ -x "$BIN" ] || BIN="target/debug/cinvar"
[ -x "$BIN" ] || { echo "build first: cargo build"; exit 1; }

WORK="$WORK_DIR"
[ -n "$WORK" ] || WORK="$(mktemp -d)"
echo "==> workspace: $WORK"
./examples/make_footage.sh "$WORK/footage" >/dev/null

echo "==> doctor"
"$BIN" doctor

echo "==> create project"
"$BIN" project create --name "Demo One" --dir "$WORK"
PROJECT="$WORK/demo-one"

echo "==> configure the offline mock planner (no credentials needed)"
export CINVAR_CONFIG_DIR="$WORK/config"
"$BIN" models add mock --provider mock --model mock-planner >/dev/null
"$BIN" models use mock >/dev/null

echo "==> natural-language instruction"
"$BIN" --project "$PROJECT" --media-root "$WORK/footage" run "Combine the clips in $WORK/footage, add a title, insert transitions, normalize the audio and export a 1080p video."

echo "==> status"
"$BIN" --project "$PROJECT" status

echo "==> project summary"
"$BIN" --project "$PROJECT" project show

echo
echo "Project:  $PROJECT"
echo "Renders:  $PROJECT/renders"
echo "Inspect with: ffprobe -hide_banner \$(ls $PROJECT/renders/*.mp4 2>/dev/null | head -1)"
