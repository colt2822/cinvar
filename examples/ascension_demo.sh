#!/usr/bin/env bash
# CINVAR Ascension II demo. Offline, free, no credentials; nothing is sent to RunPod or any service.
#   Part 1  REAL tools over MCP (FFmpeg, 4K/8K, OTIO, job store, cancellation, crash recovery)
#   Part 2  SIMULATED external services (local mock RunPod API with a real worker; mock ComfyUI)
#   Part 3  REAL third-party integrations, each only when its dependency is configured:
#           CINVAR_BLENDER, CINVAR_OTIO_PYTHON, CINVAR_OCIO_PYTHON, CINVAR_FASTER_WHISPER_PYTHON(+CINVAR_TEST_SPEECH)
set -euo pipefail
cd "$(dirname "$0")/.."
BIN="${CINVAR_BIN:-target/debug/cinvar}"
# The cloud-quote step needs a `full` MCP session (default is `edit`); the demo asks for it explicitly.
export CINVAR_MCP_PERMISSIONS=full
[ -x "$BIN" ] || { echo "build first: cargo build"; exit 1; }
WORK="${WORK_DIR:-$(mktemp -d)}"

echo "######## PART 1: real tools over MCP ########"
python3 examples/ascension_demo.py "$BIN" "$WORK/w"

echo "######## PART 2: SIMULATED services (mock RunPod API + real worker; mock ComfyUI) ########"
cargo test --offline --test cloud_runpod_mock -- --test-threads=1 2>&1 | grep -E "^test |test result"
cargo test --offline --test comfy_mock 2>&1 | grep -E "^test |test result"

echo "######## PART 3: real integrations (only when configured) ########"
ran=0
if [ -n "${CINVAR_BLENDER:-}" ]; then ran=1; echo "-- real Blender"; cargo test --offline --test blender_real -- --ignored 2>&1 | grep -E "^test |test result"; else echo "-- Blender: SKIPPED (set CINVAR_BLENDER)"; fi
if [ -n "${CINVAR_OTIO_PYTHON:-}" ]; then ran=1; echo "-- real OpenTimelineIO"; cargo test --offline --test otio_real -- --ignored 2>&1 | grep -E "^test |test result"; else echo "-- OpenTimelineIO: SKIPPED (set CINVAR_OTIO_PYTHON)"; fi
if [ -n "${CINVAR_OCIO_PYTHON:-}" ]; then ran=1; echo "-- real OpenColorIO"; cargo test --offline --test color_ocio -- --ignored 2>&1 | grep -E "^test |test result"; else echo "-- OpenColorIO: SKIPPED (set CINVAR_OCIO_PYTHON)"; fi
if [ -n "${CINVAR_FASTER_WHISPER_PYTHON:-}" ] && [ -n "${CINVAR_TEST_SPEECH:-}" ]; then ran=1; echo "-- real faster-whisper"; cargo test --offline --test transcribe_real -- --ignored 2>&1 | grep -E "^test |test result"; else echo "-- faster-whisper: SKIPPED (set CINVAR_FASTER_WHISPER_PYTHON and CINVAR_TEST_SPEECH)"; fi
echo "DEMO COMPLETE (real third-party integrations executed: $([ "$ran" = 1 ] && echo yes || echo none))"
