#!/usr/bin/env bash
# Build a single-file Linux AppImage from the same release binary the tarball ships.
#
#   scripts/build_appimage.sh
#
# Output: dist/cinvar-<version>-<arch>.AppImage (+ .sha256)
#
# The AppDir is assembled here (usr/bin/cinvar, .desktop, icon, docs) and packed with appimagetool.
# appimagetool is downloaded once into ${CINVAR_TOOLS_DIR:-~/.cache/cinvar-tools} unless
# CINVAR_APPIMAGETOOL points at a local copy; the downloaded file's sha256 is printed, because the
# "continuous" channel is not version-pinned. Nothing here signs the AppImage: signing needs a
# private key this repository does not hold.
set -euo pipefail
cd "$(dirname "$0")/.."
ROOT="$PWD"

VERSION="$(grep -m1 '^version' Cargo.toml | cut -d'"' -f2)"
ARCH="${CINVAR_ARCH:-$(uname -m)}"
TARGET_DIR="${CARGO_TARGET_DIR:-/tmp/tgt-package-linux}"
TOOLS_DIR="${CINVAR_TOOLS_DIR:-$HOME/.cache/cinvar-tools}"
OUT="dist/cinvar-$VERSION-$ARCH.AppImage"
EPOCH="${SOURCE_DATE_EPOCH:-$(git log -1 --format=%ct 2>/dev/null || echo 1700000000)}"

# Progress goes to stderr: tool() is captured in a command substitution and must print only a path.
say() { printf '\033[1;34m==>\033[0m %s\n' "$*" >&2; }
die() { printf '\033[1;31merror:\033[0m %s\n' "$*" >&2; exit 1; }

tool() {
  if [ -n "${CINVAR_APPIMAGETOOL:-}" ]; then echo "$CINVAR_APPIMAGETOOL"; return; fi
  mkdir -p "$TOOLS_DIR"
  local path="$TOOLS_DIR/appimagetool-$ARCH.AppImage"
  if [ ! -x "$path" ]; then
    say "downloading appimagetool into $TOOLS_DIR"
    curl -fsSL --retry 3 -o "$path" \
      "https://github.com/AppImage/appimagetool/releases/download/continuous/appimagetool-$ARCH.AppImage" \
      || die "cannot download appimagetool; set CINVAR_APPIMAGETOOL to a local appimagetool binary"
    chmod +x "$path"
    say "appimagetool sha256 $(sha256sum "$path" | cut -d' ' -f1)"
  fi
  echo "$path"
}

say "building release binary"
CARGO_TARGET_DIR="$TARGET_DIR" cargo build --release --locked --bin cinvar
BIN="$TARGET_DIR/release/cinvar"
[ -x "$BIN" ] || die "no binary at $BIN"

WORK="$(mktemp -d)"; trap 'rm -rf "$WORK"' EXIT
APPDIR="$WORK/CINVAR.AppDir"
mkdir -p "$APPDIR/usr/bin" "$APPDIR/usr/share/cinvar" "$APPDIR/usr/share/doc/cinvar" \
         "$APPDIR/usr/share/icons/hicolor/scalable/apps" "$APPDIR/usr/share/applications"
install -m 755 "$BIN" "$APPDIR/usr/bin/cinvar"
install -m 644 packaging/appimage/cinvar.desktop "$APPDIR/cinvar.desktop"
install -m 644 packaging/appimage/cinvar.desktop "$APPDIR/usr/share/applications/cinvar.desktop"
install -m 644 packaging/linux/cinvar.svg "$APPDIR/cinvar.svg"
install -m 644 packaging/linux/cinvar.svg "$APPDIR/usr/share/icons/hicolor/scalable/apps/cinvar.svg"
install -m 755 packaging/appimage/AppRun "$APPDIR/AppRun"
[ -f packaging/linux/cinvar-launcher.sh ] && install -m 755 packaging/linux/cinvar-launcher.sh "$APPDIR/usr/share/cinvar/cinvar-launcher.sh"
for doc in README.md INSTALLATION.md LICENSE NOTICE THIRD_PARTY_NOTICES.md; do
  [ -f "$doc" ] && install -m 644 "$doc" "$APPDIR/usr/share/doc/cinvar/$doc"
done
find "$APPDIR" -exec touch -h -d "@$EPOCH" {} +

APPIMAGETOOL="$(tool)"
say "packing $OUT with $APPIMAGETOOL"
mkdir -p dist
export ARCH
# appimagetool is itself an AppImage: --appimage-extract-and-run avoids needing FUSE on a build host.
"$APPIMAGETOOL" --appimage-extract-and-run --no-appstream "$APPDIR" "$ROOT/$OUT" \
  || "$APPIMAGETOOL" --no-appstream "$APPDIR" "$ROOT/$OUT"
chmod 755 "$OUT"
( cd dist && sha256sum "$(basename "$OUT")" > "$(basename "$OUT").sha256" )
say "sha256 $(cut -d' ' -f1 "$OUT.sha256")"

say "smoke test (extract and run, no FUSE needed)"
"$OUT" --appimage-extract-and-run --version 2>&1 | head -1 || die "the AppImage does not run"
ROOT_DIR="$(cd "$(dirname "$0")/.." && pwd)"
say "checking the AppImage enforces edition limits"
"$ROOT_DIR/scripts/check_enforcement.sh" "$BIN"

echo
echo "artifact: $OUT"
echo "checksum: $OUT.sha256"
