#!/usr/bin/env bash
# Install a prebuilt CINVAR archive, verifying its checksum first, then run "cinvar doctor".
#
#   scripts/install-prebuilt.sh dist/cinvar-0.2.0-linux-x86_64.tar.gz
#   scripts/install-prebuilt.sh https://example.invalid/cinvar-0.2.0-linux-x86_64.tar.gz --pro-ai
#   scripts/install-prebuilt.sh <archive> --sha256 <hex> --prefix ~/.local
#
# Never uses sudo. Refuses plain http. Fails closed when no checksum is available, because an
# unverified archive is exactly the thing this script exists to prevent.
#
# Options: --prefix DIR (default $CINVAR_PREFIX or ~/.local), --sha256 HEX, --pro-ai (also run
# "cinvar setup --pro-ai"), --no-doctor, --desktop (install the .desktop entry + icon when the
# archive ships packaging/linux).
set -euo pipefail

PREFIX="${CINVAR_PREFIX:-$HOME/.local}"
WANT_SHA=""
PRO_AI=0
DOCTOR=1
DESKTOP=0
ARG=""

say() { printf '\033[1;34m==>\033[0m %s\n' "$*"; }
die() { printf '\033[1;31merror:\033[0m %s\n' "$*" >&2; exit 1; }
usage() { sed -n '2,15p' "$0" | sed 's/^# \{0,1\}//'; exit 2; }

while [ "$#" -gt 0 ]; do
  case "$1" in
    --prefix) PREFIX="${2:?--prefix needs a directory}"; shift 2 ;;
    --sha256) WANT_SHA="${2:?--sha256 needs a hex digest}"; shift 2 ;;
    --pro-ai) PRO_AI=1; shift ;;
    --no-doctor) DOCTOR=0; shift ;;
    --desktop) DESKTOP=1; shift ;;
    -h|--help) usage ;;
    -*) die "unknown option $1" ;;
    *) ARG="$1"; shift ;;
  esac
done
[ -n "$ARG" ] || usage

WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT
ARCHIVE="$ARG"

case "$ARCHIVE" in
  http://*) die "refusing plain http; use an https URL so the download cannot be swapped in transit" ;;
  https://*)
    say "downloading $ARCHIVE"
    curl -fsSL --retry 3 --max-time 1800 -o "$WORK/archive.tar.gz" "$ARCHIVE" || die "download failed"
    ARCHIVE="$WORK/archive.tar.gz"
    if [ -z "$WANT_SHA" ]; then
      say "fetching $ARG.sha256"
      curl -fsSL --max-time 60 -o "$WORK/archive.sha256" "$ARG.sha256" 2>/dev/null || true
      [ -s "$WORK/archive.sha256" ] && WANT_SHA="$(cut -d' ' -f1 "$WORK/archive.sha256")"
    fi
    ;;
esac
[ -f "$ARCHIVE" ] || die "no such archive: $ARCHIVE"

if [ -z "$WANT_SHA" ] && [ -f "$ARCHIVE.sha256" ]; then
  WANT_SHA="$(cut -d' ' -f1 "$ARCHIVE.sha256")"
fi
[ -n "$WANT_SHA" ] || die "no checksum found (expected $ARCHIVE.sha256 or --sha256); refusing to install an unverified archive"

sha256_of() {
  if command -v sha256sum >/dev/null; then sha256sum "$1" | cut -d' ' -f1
  elif command -v shasum >/dev/null; then shasum -a 256 "$1" | cut -d' ' -f1
  else die "no sha256sum or shasum on this system"; fi
}
GOT_SHA="$(sha256_of "$ARCHIVE")"
if [ "$GOT_SHA" != "$WANT_SHA" ]; then
  die "checksum mismatch for $ARCHIVE
  expected: $WANT_SHA
  actual:   $GOT_SHA"
fi
say "checksum ok ($GOT_SHA)"

say "extracting"
tar -xzf "$ARCHIVE" -C "$WORK"
ROOTDIR="$(find "$WORK" -maxdepth 1 -mindepth 1 -type d | head -1)"
[ -n "$ROOTDIR" ] || die "archive has no top-level directory"
BIN="$ROOTDIR/cinvar"
[ -f "$BIN" ] || die "archive does not contain a cinvar binary"
chmod 755 "$BIN"

BIN_DIR="$PREFIX/bin"
DOC_DIR="$PREFIX/share/doc/cinvar"
mkdir -p "$BIN_DIR" "$DOC_DIR"
install -m 755 "$BIN" "$BIN_DIR/cinvar"
for f in "$ROOTDIR"/*.md "$ROOTDIR"/LICENSE "$ROOTDIR"/NOTICE; do
  [ -f "$f" ] && install -m 644 "$f" "$DOC_DIR/$(basename "$f")"
done
[ -d "$ROOTDIR/docs" ] && cp -r "$ROOTDIR/docs" "$DOC_DIR/" 2>/dev/null || true
say "installed $BIN_DIR/cinvar ($("$BIN_DIR/cinvar" --version 2>&1 | head -1))"

if [ "$DESKTOP" = 1 ] && [ -d "$ROOTDIR/packaging/linux" ]; then
  APP="$HOME/.local/share/applications"; ICON="$HOME/.local/share/icons/hicolor/scalable/apps"
  mkdir -p "$APP" "$ICON"
  [ -f "$ROOTDIR/packaging/linux/cinvar.svg" ] && install -m 644 "$ROOTDIR/packaging/linux/cinvar.svg" "$ICON/cinvar.svg"
  cat > "$APP/cinvar.desktop" <<DESKTOP_EOF
[Desktop Entry]
Type=Application
Name=CINVAR
Comment=AI video editor
Exec=env CINVAR_BIN=$BIN_DIR/cinvar $ROOTDIR/packaging/linux/cinvar-launcher.sh
Icon=cinvar
Terminal=false
Categories=AudioVideo;Video;
DESKTOP_EOF
  say "installed desktop entry ($APP/cinvar.desktop)"
fi

if [ "$DOCTOR" = 1 ]; then
  say "cinvar doctor"
  set +e
  "$BIN_DIR/cinvar" doctor || say "doctor reported missing tools; install them, then re-run: cinvar doctor"
  set -e
fi

if [ "$PRO_AI" = 1 ]; then
  say "cinvar setup --pro-ai (one virtualenv per component; multi-GB)"
  "$BIN_DIR/cinvar" setup --pro-ai || say "some components failed; re-run: cinvar setup --pro-ai"
fi

case ":$PATH:" in
  *":$BIN_DIR:"*) ;;
  *) say "add this to your shell profile: export PATH=\"$BIN_DIR:\$PATH\"" ;;
esac
say "done"
