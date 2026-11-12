#!/usr/bin/env bash
# Assemble a release folder + tar.gz (+ sha256) from a release build. Linux/macOS; Windows runs the
# equivalent steps in CI (see .github/workflows/ci.yml). Nothing is uploaded or published here.
set -euo pipefail
cd "$(dirname "$0")/.."
VERSION="$(grep -m1 '^version' Cargo.toml | cut -d'"' -f2)"
OS="$(uname -s | tr '[:upper:]' '[:lower:]')"; ARCH="$(uname -m)"
NAME="cinvar-$VERSION-$OS-$ARCH"
OUT="dist/$NAME"
rm -rf "$OUT"; mkdir -p "$OUT/docs" "$OUT/examples"
cargo build --release --locked
cp target/release/cinvar "$OUT/"
cp LICENSE README.md INSTALLATION.md SECURITY.md CHANGELOG.md ROADMAP.md CONTRIBUTING.md NOTICE THIRD_PARTY_NOTICES.md "$OUT/" 2>/dev/null || true
cp -r plugins "$OUT/" 2>/dev/null || true
mkdir -p "$OUT/packaging" && cp -r packaging/linux "$OUT/packaging/" 2>/dev/null || true
cp docs/*.md "$OUT/docs/" 2>/dev/null || true
cp examples/*.sh examples/*.py "$OUT/examples/" 2>/dev/null || true
# Third-party crate licence inventory (identifiers only; full texts live in each crate).
cargo metadata --format-version 1 --locked | python3 -c '
import json,sys
m=json.load(sys.stdin)
for p in sorted((p for p in m["packages"] if p["source"]), key=lambda p:p["name"]):
    print("%s %s  %s  %s" % (p["name"], p["version"], p.get("license") or "UNKNOWN", p.get("repository") or ""))
' > "$OUT/THIRD_PARTY_CRATES.txt"
cat > "$OUT/EXTERNAL_TOOLS.txt" <<'TXT'
CINVAR runs these as separate programs; they are NOT bundled and keep their own licences:
FFmpeg (LGPL/GPL by build), Blender (GPL-3.0-or-later), ComfyUI (GPL-3.0), OpenColorIO (BSD-3-Clause, via Python),
OpenTimelineIO (Apache-2.0, optional test helper), faster-whisper (MIT) + Whisper model weights (check their licence).
TXT
( cd dist && tar czf "$NAME.tar.gz" "$NAME" && ( sha256sum "$NAME.tar.gz" 2>/dev/null || shasum -a 256 "$NAME.tar.gz" ) > "$NAME.tar.gz.sha256" )
echo "built dist/$NAME.tar.gz"; cat "dist/$NAME.tar.gz.sha256"
