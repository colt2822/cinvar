#!/usr/bin/env bash
# Reproducible end-to-end verification of a CINVAR checkout.
# Prints one PASS/FAIL line per claim; exits non-zero if any check failed.
set -uo pipefail
cd "$(dirname "$0")/.."

# Use an explicit binary, a release build, a debug build, or build one now.
BIN="${CINVAR_BIN:-}"
if [ -z "$BIN" ]; then
  if [ -x target/release/cinvar ]; then BIN="target/release/cinvar"
  elif [ -x target/debug/cinvar ]; then BIN="target/debug/cinvar"
  else
    echo "== no binary found, building"
    cargo build || { echo "FAIL  build.binary: cargo build failed"; exit 1; }
    BIN="target/debug/cinvar"
  fi
fi
WORK="$(mktemp -d)"
export CINVAR_CONFIG_DIR="$WORK/config"
export CINVAR_DATA_DIR="$WORK/data"
FAILURES=0

pass() { printf 'PASS  %s: %s\n' "$1" "$2"; }
fail() { printf 'FAIL  %s: %s\n' "$1" "$2"; FAILURES=$((FAILURES + 1)); }

echo "== workspace $WORK"
echo "== binary $BIN"

[ -x "$BIN" ] && pass "build.binary" "$BIN" || fail "build.binary" "missing $BIN"

"$BIN" --json doctor > "$WORK/doctor.json" 2>/dev/null
TOOLS=$(python3 -c "import json;print(json.load(open('$WORK/doctor.json'))['tool_count'])" 2>/dev/null)
if [ -n "$TOOLS" ] && [ "$TOOLS" -ge 40 ]; then pass "doctor.tools" "$TOOLS tools"; else fail "doctor.tools" "count=$TOOLS"; fi

./examples/make_footage.sh "$WORK/footage" >/dev/null 2>&1
[ -f "$WORK/footage/clip-1.mp4" ] && pass "footage.synthetic" "3 clips + audio" || fail "footage.synthetic" "generation failed"

"$BIN" models add mock --provider mock --model mock-planner >/dev/null 2>&1
"$BIN" project create --name "Verify" --dir "$WORK" >/dev/null 2>&1
PROJECT="$WORK/verify"
[ -f "$PROJECT/project.json" ] && pass "project.create" "$PROJECT" || fail "project.create" "no project.json"

"$BIN" --project "$PROJECT" --media-root "$WORK/footage" run "Combine the clips in $WORK/footage, add a title, normalize the audio and export a 1080p video" > "$WORK/agent.log" 2>&1
RENDERED=$(ls "$PROJECT"/renders/*.mp4 2>/dev/null | head -1)
if [ -n "$RENDERED" ]; then
  DUR=$(ffprobe -v error -show_entries format=duration -of csv=p=0 "$RENDERED" 2>/dev/null)
  pass "agent.render" "$(basename "$RENDERED") duration=$DUR"
else
  fail "agent.render" "no render produced (see log)"
fi
grep -q "QA verdict: passed" "$WORK/agent.log" && pass "agent.qa_verdict" "planner saw a passing QA" || fail "agent.qa_verdict" "no passing QA verdict"

python3 examples/verify_mcp.py "$PROJECT" "$BIN" > "$WORK/mcp.log" 2>&1
MCP_RC=$?
COUNT=$(grep -c '^PASS' "$WORK/mcp.log")
echo "INFO  mcp checks passed: $COUNT"
grep '^FAIL' "$WORK/mcp.log" || true
if [ "$MCP_RC" -eq 0 ]; then pass "mcp.suite" "external agent checks passed"; else fail "mcp.suite" "see $WORK/mcp.log"; fi

SKILLS=$("$BIN" skills list 2>/dev/null | wc -l)
if [ "$SKILLS" -ge 10 ]; then pass "skills.loaded" "$SKILLS skills"; else fail "skills.loaded" "$SKILLS"; fi

OTIO="$WORK/timeline.otio"
"$BIN" --project "$PROJECT" otio export --output "$OTIO" >/dev/null 2>&1
if [ -s "$OTIO" ] && grep -q 'OTIO_SCHEMA' "$OTIO"; then pass "otio.export" "$(wc -c < "$OTIO") bytes"; else fail "otio.export" "no OTIO document"; fi

"$BIN" --project "$PROJECT" status > "$WORK/status.txt" 2>&1
grep -q "Verify" "$WORK/status.txt" && pass "cli.status" "project reported" || fail "cli.status" "unexpected output"

"$BIN" bench > "$WORK/bench.json" 2>/dev/null
grep -q 'config_load_ms' "$WORK/bench.json" && pass "bench.measured" "reported" || fail "bench.measured" "bench failed"

echo "== workspace kept at $WORK"
echo "SUMMARY failures=$FAILURES"
[ "$FAILURES" -eq 0 ]
