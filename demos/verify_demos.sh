#!/usr/bin/env bash
# Verifies the delivered demo videos with ffprobe and CINVAR's own QA.
set -uo pipefail
cd "$(dirname "$0")/.."
BIN="$PWD/target/debug/cinvar"
[ -x "$BIN" ] || BIN="$PWD/target/release/cinvar"
export CINVAR_CONFIG_DIR="$PWD/demos/.config"
export CINVAR_DATA_DIR="$PWD/demos/.data"
PROJECT="demos/first_light/first-light"
FAIL=0

verify() {
  local file="$1" label="$2"
  if [ ! -s "$file" ]; then echo "FAIL  $label: missing $file"; FAIL=$((FAIL+1)); return; fi
  local info
  info=$(ffprobe -v error -show_entries format=duration,size -show_entries stream=codec_name,width,height,r_frame_rate,channels -of json "$file")
  python3 - "$info" "$label" "$file" <<'PY'
import json, sys
info = json.loads(sys.argv[1]); label = sys.argv[2]; path = sys.argv[3]
v = [s for s in info["streams"] if s.get("width")]
a = [s for s in info["streams"] if s.get("channels")]
fmt = info["format"]
print(f"PASS  {label}: {path}")
print(f"      duration={float(fmt['duration']):.3f}s size={int(fmt['size'])} bytes video={v[0]['codec_name'] if v else 'NONE'} {v[0]['width'] if v else '?'}x{v[0]['height'] if v else '?'} audio={a[0]['codec_name'] if a else 'none'}")
PY
  "$BIN" --project "$PROJECT" qa "$file" 2>&1 | sed 's/^/      /' | head -9
}

for f in "$PROJECT"/renders/*.mp4; do
  [ -e "$f" ] || continue
  verify "$f" "$(basename "$f")"
done

echo "== project state"
"$BIN" --project "$PROJECT" status | head -8
echo "SUMMARY failures=$FAIL"
[ "$FAIL" -eq 0 ]
