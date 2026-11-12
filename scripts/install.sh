#!/usr/bin/env bash
# Build CINVAR from this checkout and install it. Linux and macOS. Never uses sudo.
#
#   git clone <repository> && cd cinvar && ./scripts/install.sh
#
# Env: CINVAR_PREFIX (default ~/.local; binary goes to $CINVAR_PREFIX/bin)
# No prebuilt binaries are published yet, so this always builds from source (needs Rust).
set -euo pipefail

PREFIX="${CINVAR_PREFIX:-$HOME/.local}"
BIN_DIR="$PREFIX/bin"
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

say() { printf '\033[1;34m==>\033[0m %s\n' "$*"; }
die() { printf '\033[1;31merror:\033[0m %s\n' "$*" >&2; exit 1; }

case "$(uname -s)" in
  Linux|Darwin) ;;
  *) die "unsupported operating system (Windows: use scripts/install.ps1)" ;;
esac
[ -f "$HERE/Cargo.toml" ] || die "run this script from a CINVAR checkout"

if ! command -v ffmpeg >/dev/null 2>&1 || ! command -v ffprobe >/dev/null 2>&1; then
  say "FFmpeg + ffprobe are required but were not found. Install them first:"
  echo "  Debian/Ubuntu: sudo apt install ffmpeg"
  echo "  Fedora:        sudo dnf install ffmpeg"
  echo "  macOS:         brew install ffmpeg"
  die "FFmpeg is required for media work"
fi
command -v cargo >/dev/null 2>&1 || die "cargo is required (https://rustup.rs)"

say "building CINVAR (release)"
( cd "$HERE" && cargo build --release --locked )
mkdir -p "$BIN_DIR"
install -m 755 "$HERE/target/release/cinvar" "$BIN_DIR/cinvar"
say "installed to $BIN_DIR/cinvar"

say "cinvar doctor"
"$BIN_DIR/cinvar" doctor || true

case ":$PATH:" in
  *":$BIN_DIR:"*) ;;
  *) say "add this to your shell profile: export PATH=\"$BIN_DIR:\$PATH\"" ;;
esac
say "done. next: cinvar setup --yes   (then see README.md)"
