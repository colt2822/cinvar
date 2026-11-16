#!/usr/bin/env bash
# Fail when a binary to be shipped does not enforce edition limits.
#
# Why this exists: the `edition-observe` cargo feature (enabled through the self dev-dependency so
# the existing test suite can render 4K/8K without a license) is unified across a target directory.
# A release build that shares a target dir with test artifacts therefore links a library whose
# ENFORCE_BY_DEFAULT is false, and every Pro gate silently disappears from the shipped binary.
# Measured during the sprint: a binary from a target dir that had run `cargo test` reported
# "enforcement": "observe", while a fresh-directory build reported "enforce".
#
# Usage: scripts/check_enforcement.sh <path-to-cinvar> [extra args for running it]
set -euo pipefail

BIN="${1:?usage: check_enforcement.sh <cinvar binary>}"
RUN=( "${@:2}" )
[ -x "$BIN" ] || { echo "check_enforcement: not executable: $BIN" >&2; exit 1; }

DIR="$(mktemp -d)"
trap 'rm -rf "$DIR"' EXIT

STATUS="$(CINVAR_CONFIG_DIR="$DIR" "${RUN[@]}" "$BIN" edition status --json 2>/dev/null || true)"
FLAT="$(printf '%s' "$STATUS" | tr -d ' \n')"

case "$FLAT" in
  *'"enforcement":"enforce"'*) ;;
  *)
    echo "check_enforcement: $BIN does NOT enforce edition limits." >&2
    echo "  status: $STATUS" >&2
    echo "  The edition-observe cargo feature leaked into this build (shared CARGO_TARGET_DIR with cargo test)." >&2
    echo "  Rebuild in a clean target dir, for example:" >&2
    echo "    CARGO_TARGET_DIR=$(mktemp -d) cargo build --release --bin cinvar" >&2
    exit 1
    ;;
esac
echo "enforcement ok: enforce (checked with an empty config dir)"
