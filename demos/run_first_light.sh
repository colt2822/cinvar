#!/usr/bin/env bash
# Demo A - First Light: a real model (Claude Code, over CINVAR's MCP server) creates a video
# from one natural-language instruction. Requires: ffmpeg, cargo, an authenticated claude CLI.
set -euo pipefail
cd "$(dirname "$0")/.."

DEMO="demos/first_light"
BIN="$PWD/target/debug/cinvar"
[ -x "$BIN" ] || { echo "== building"; cargo build; }
export CINVAR_CONFIG_DIR="$PWD/demos/.config"
export CINVAR_DATA_DIR="$PWD/demos/.data"

if [ ! -f "$DEMO/footage/clip-5.mp4" ]; then
  echo "== generating synthetic footage"
  ./examples/make_footage.sh "$DEMO/footage" >/dev/null
  ffmpeg -hide_banner -loglevel error -y -f lavfi -i "color=c=0x502010:s=1920x1080:d=4:r=30,drawtext=text='SHOT 4':fontcolor=white:fontsize=140:x=(w-text_w)/2:y=(h-text_h)/2" -f lavfi -i "sine=frequency=660:duration=4" -c:v libx264 -preset ultrafast -pix_fmt yuv420p -c:a aac -shortest "$DEMO/footage/clip-4.mp4"
  ffmpeg -hide_banner -loglevel error -y -f lavfi -i "color=c=0x104050:s=1920x1080:d=7:r=30,drawtext=text='SHOT 5':fontcolor=white:fontsize=140:x=(w-text_w)/2:y=(h-text_h)/2" -f lavfi -i "sine=frequency=180:duration=7" -c:v libx264 -preset ultrafast -pix_fmt yuv420p -c:a aac -shortest "$DEMO/footage/clip-5.mp4"
fi

if [ ! -f "$DEMO/first-light/project.json" ]; then
  echo "== creating project"
  "$BIN" project create --name first-light --dir "$DEMO" >/dev/null
fi

python3 - "$BIN" "$PWD/$DEMO/first-light" > "$DEMO/mcp.json" <<'PY'
import json, sys
binary, project = sys.argv[1], sys.argv[2]
print(json.dumps({"mcpServers": {"cinvar": {"command": binary, "args": ["mcp", "serve", "--project", project]}}}, indent=2))
PY

echo "== running the model (this spends real model usage)"
claude -p "$(cat "$DEMO/prompt.txt")" \
  --mcp-config "$DEMO/mcp.json" --strict-mcp-config \
  --allowedTools "mcp__cinvar,Read" \
  --add-dir "$PWD/$DEMO/first-light" \
  --max-turns 80 --output-format json > "$DEMO/trace.json"

python3 - "$DEMO/trace.json" <<'PY'
import json, sys
d = json.load(open(sys.argv[1]))
print("turns:", d.get("num_turns"), "cost_usd:", round(d.get("total_cost_usd") or 0, 4), "model:", list(d.get("modelUsage", {})))
print("result:", (d.get("result") or "")[:400])
PY
./demos/verify_demos.sh
