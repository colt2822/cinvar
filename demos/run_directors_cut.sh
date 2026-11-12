#!/usr/bin/env bash
# Demo B - Director's Cut: the same real model revises the existing project conversationally.
set -euo pipefail
cd "$(dirname "$0")/.."
DEMO="demos/first_light"
BIN="$PWD/target/debug/cinvar"
export CINVAR_CONFIG_DIR="$PWD/demos/.config"
export CINVAR_DATA_DIR="$PWD/demos/.data"
[ -f "$DEMO/first-light/project.json" ] || { echo "run demos/run_first_light.sh first"; exit 1; }

echo "== running the follow-up instruction on the existing project (spends real model usage)"
claude -p "$(cat "$DEMO/revision_prompt.txt")" \
  --mcp-config "$DEMO/mcp.json" --strict-mcp-config \
  --allowedTools "mcp__cinvar,Read" \
  --add-dir "$PWD/$DEMO/first-light" \
  --max-turns 60 --output-format json > "$DEMO/revision_trace.json"

python3 - "$DEMO/revision_trace.json" <<'PY'
import json, sys
d = json.load(open(sys.argv[1]))
print("turns:", d.get("num_turns"), "cost_usd:", round(d.get("total_cost_usd") or 0, 4))
print("result:", (d.get("result") or "")[:600])
PY
./demos/verify_demos.sh
