#!/usr/bin/env bash
# Regression test for scripts/publish_update.sh's "pro references remain" gate.
# Plants the Pro module path in an ordinary public source file of a throwaway clone and asserts the publisher
# (prepare-only, never --push) refuses with that exact message. The gate runs before the build, so this is fast.
set -uo pipefail
HERE="$(cd "$(dirname "$0")/.." && pwd)"
T="$(mktemp -d)"; trap 'rm -rf "$T"' EXIT
git clone -q "$HERE" "$T/src" && cd "$T/src"
printf '\n// planted: use %s%s install;\n' cinvar_pro '::' >> src/main.rs
git -c user.name=t -c user.email=t@t commit -qam "plant pro reference"
out="$(bash scripts/publish_update.sh 2>&1)"; rc=$?
if [ "$rc" -ne 0 ] && grep -q 'pro references remain' <<<"$out"; then echo "PASS: a planted Pro module reference in src/main.rs blocks publication"; else echo "FAIL (rc=$rc)"; echo "$out" | tail -5; exit 1; fi
