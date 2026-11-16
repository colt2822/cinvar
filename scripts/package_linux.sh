#!/usr/bin/env bash
# Reproducible prebuilt Linux tarball: dist/cinvar-<version>-linux-<arch>.tar.gz (+ .sha256).
#
#   scripts/package_linux.sh                 # build + package + verify the archive runs
#   scripts/package_linux.sh --reproducible   # also package twice and prove identical sha256
#
# Reproducibility contract: same source, same Cargo.lock, same Rust toolchain -> same sha256.
# The archive carries a fixed mtime (SOURCE_DATE_EPOCH, the commit time), owner 0:0, sorted
# entries (tar --sort=name) and gzip -n (no timestamp, no name), so two packs of one build match.
# The binary is built with --locked into a target directory outside the tree, so a dirty ./target
# cannot leak into the archive.
#
# Env: CARGO_TARGET_DIR (default /tmp/tgt-package-linux), SOURCE_DATE_EPOCH, DIST (default dist),
#      CINVAR_ARCH (default uname -m).
set -euo pipefail
cd "$(dirname "$0")/.."
ROOT="$PWD"
REPRODUCIBLE=0
[ "${1:-}" = "--reproducible" ] && REPRODUCIBLE=1

VERSION="$(grep -m1 '^version' Cargo.toml | cut -d'"' -f2)"
ARCH="${CINVAR_ARCH:-$(uname -m)}"
DIST="${DIST:-$ROOT/dist}"
NAME="cinvar-$VERSION-linux-$ARCH"
ARCHIVE="$DIST/$NAME.tar.gz"
TARGET_DIR="${CARGO_TARGET_DIR:-/tmp/tgt-package-linux}"
EPOCH="${SOURCE_DATE_EPOCH:-$(git log -1 --format=%ct 2>/dev/null || echo 1700000000)}"
export SOURCE_DATE_EPOCH="$EPOCH"

say() { printf '\033[1;34m==>\033[0m %s\n' "$*"; }
die() { printf '\033[1;31merror:\033[0m %s\n' "$*" >&2; exit 1; }

command -v cargo >/dev/null || die "cargo is required"
[ "$ARCH" = "x86_64" ] || say "warning: packaging for $ARCH; the shipped FFmpeg advice assumes x86_64"

say "building release binary (target dir $TARGET_DIR, SOURCE_DATE_EPOCH=$EPOCH)"
CARGO_TARGET_DIR="$TARGET_DIR" cargo build --release --locked --bin cinvar
BIN="$TARGET_DIR/release/cinvar"
[ -x "$BIN" ] || die "no binary at $BIN"

say "assembling $NAME"
mkdir -p "$DIST"
STAGE="$(mktemp -d)"
trap 'rm -rf "$STAGE"' EXIT
OUT="$STAGE/$NAME"
mkdir -p "$OUT/docs" "$OUT/examples" "$OUT/packaging"
install -m 755 "$BIN" "$OUT/cinvar"
for doc in LICENSE NOTICE THIRD_PARTY_NOTICES.md README.md INSTALLATION.md SECURITY.md CHANGELOG.md ROADMAP.md CONTRIBUTING.md; do
  [ -f "$doc" ] && install -m 644 "$doc" "$OUT/$doc"
done
[ -d plugins ] && cp -r plugins "$OUT/"
[ -d packaging/linux ] && cp -r packaging/linux "$OUT/packaging/"
[ -d packaging/appimage ] && cp -r packaging/appimage "$OUT/packaging/"
for f in docs/*.md; do [ -f "$f" ] && install -m 644 "$f" "$OUT/docs/$(basename "$f")"; done
for f in examples/*.sh examples/*.py; do [ -f "$f" ] && install -m 644 "$f" "$OUT/examples/$(basename "$f")"; done
install -m 755 scripts/install-prebuilt.sh "$OUT/install.sh"

# Licence inventories are derived from Cargo.lock, so they are reproducible too. A fully populated
# crate cache lets this run with --offline; otherwise cargo may fetch the index, which is still
# deterministic for a --locked manifest.
crate_inventory() {
  python3 -c '
import json, sys
m = json.load(sys.stdin)
for p in sorted((p for p in m["packages"] if p["source"]), key=lambda p: (p["name"], p["version"])):
    print("%s %s  %s  %s" % (p["name"], p["version"], p.get("license") or "UNKNOWN", p.get("repository") or ""))
'
}
if cargo metadata --format-version 1 --locked --offline 2>/dev/null | crate_inventory > "$OUT/THIRD_PARTY_CRATES.txt" \
   && [ -s "$OUT/THIRD_PARTY_CRATES.txt" ]; then
  :
elif cargo metadata --format-version 1 --locked 2>/dev/null | crate_inventory > "$OUT/THIRD_PARTY_CRATES.txt" \
   && [ -s "$OUT/THIRD_PARTY_CRATES.txt" ]; then
  :
else
  rm -f "$OUT/THIRD_PARTY_CRATES.txt"
  say "warning: could not enumerate crates (cargo metadata); THIRD_PARTY_NOTICES.md still ships"
fi
cat > "$OUT/EXTERNAL_TOOLS.txt" <<'TXT'
CINVAR runs these as separate programs; they are NOT bundled and keep their own licences:
FFmpeg (LGPL/GPL by build), Blender (GPL-3.0-or-later), ComfyUI (GPL-3.0), OpenColorIO (BSD-3-Clause, via Python),
OpenTimelineIO (Apache-2.0, optional test helper), faster-whisper (MIT) + Whisper model weights (check their licence).
The Pro AI stack (WhisperX, faster-whisper, Real-ESRGAN, Demucs) is installed into per-component virtualenvs
by "cinvar setup --pro-ai"; no Python packages or model weights are shipped in this archive.
TXT

say "normalising timestamps and modes"
find "$OUT" -exec touch -h -d "@$EPOCH" {} +
find "$OUT" -type d -exec chmod 755 {} +
find "$OUT" -type f -exec chmod a-x {} +
chmod 755 "$OUT/cinvar" "$OUT/install.sh"

pack() {
  local dest="$1"
  ( cd "$STAGE" && tar --sort=name --mtime="@$EPOCH" --owner=0 --group=0 --numeric-owner \
      --format=gnu -cf - "$NAME" | gzip -9n > "$dest" )
}

say "packing $ARCHIVE"
pack "$ARCHIVE"
( cd "$DIST" && sha256sum "$NAME.tar.gz" > "$NAME.tar.gz.sha256" )
SUM="$(cut -d' ' -f1 "$DIST/$NAME.tar.gz.sha256")"
say "sha256 $SUM"

say "verifying the archive (checksum, layout, runs)"
( cd "$DIST" && sha256sum -c "$NAME.tar.gz.sha256" >/dev/null )
VERIFY="$(mktemp -d)"
tar -xzf "$ARCHIVE" -C "$VERIFY"
VERIFY_BIN="$VERIFY/$NAME/cinvar"
[ -x "$VERIFY_BIN" ] || die "archive has no executable cinvar"
say "extracted binary says: $("$VERIFY_BIN" --version 2>&1 | head -1)"
ROOT_DIR="$(cd "$(dirname "$0")/.." && pwd)"
say "checking the packaged binary enforces edition limits"
"$ROOT_DIR/scripts/check_enforcement.sh" "$VERIFY_BIN"
rm -rf "$VERIFY"

if [ "$REPRODUCIBLE" = 1 ]; then
  say "reproducibility check: packing again and comparing sha256"
  AGAIN="$(mktemp)"
  pack "$AGAIN"
  SECOND="$(sha256sum "$AGAIN" | cut -d' ' -f1)"
  rm -f "$AGAIN"
  [ "$SECOND" = "$SUM" ] || die "not reproducible: $SUM vs $SECOND"
  say "reproducible: identical sha256 on a second pack"
fi

echo
echo "artifact: $ARCHIVE"
echo "checksum: $DIST/$NAME.tar.gz.sha256"
