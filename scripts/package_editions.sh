#!/usr/bin/env bash
# Build the Free (public Apache-2.0 core) and Pro (core + proprietary pro/ module) packages as two
# distinct binaries. Nothing is uploaded, and the existing
# dist/ artifacts are never touched: output goes to dist-editions/<version>/.
#   scripts/package_editions.sh            # build + package
# Reproducibility: --locked, path remapping, sorted tar entries, fixed mtimes, gzip -n.
set -euo pipefail
cd "$(dirname "$0")/.."
VERSION="$(grep -m1 '^version' Cargo.toml | cut -d'"' -f2)"
OS="$(uname -s | tr '[:upper:]' '[:lower:]')"; ARCH="$(uname -m)"
COMMIT="$(git rev-parse HEAD 2>/dev/null || echo unknown)"
EPOCH="$(git log -1 --format=%ct 2>/dev/null || echo 0)"
OUTROOT="dist-editions/$VERSION"
TARGET_DIR="${CINVAR_PKG_TARGET_DIR:-target-release-editions}"
rm -rf "$OUTROOT"; mkdir -p "$OUTROOT"
RUSTFLAGS="--remap-path-prefix=$PWD=/build --remap-path-prefix=$HOME/.cargo=/cargo" \
  CARGO_TARGET_DIR="$TARGET_DIR" cargo build --release --locked -p cinvar --bin cinvar
cp "$TARGET_DIR/release/cinvar" "$TARGET_DIR/cinvar-free-bin"
RUSTFLAGS="--remap-path-prefix=$PWD=/build --remap-path-prefix=$HOME/.cargo=/cargo" \
  CARGO_TARGET_DIR="$TARGET_DIR" cargo build --release --locked -p cinvar-pro --bin cinvar-pro
cp "$TARGET_DIR/release/cinvar-pro" "$TARGET_DIR/cinvar-pro-bin"
FREE_SHA="$(sha256sum "$TARGET_DIR/cinvar-free-bin" | cut -d' ' -f1)"
PRO_SHA="$(sha256sum "$TARGET_DIR/cinvar-pro-bin" | cut -d' ' -f1)"

stage() { # $1 = free|pro
  local ed="$1" name="cinvar-$1-$VERSION-$OS-$ARCH" dir BIN="$TARGET_DIR/cinvar-$1-bin" SHA
  SHA="$(sha256sum "$BIN" | cut -d' ' -f1)"
  dir="$OUTROOT/$name"; mkdir -p "$dir/docs"
  cp "$BIN" "$dir/cinvar"
  cp LICENSE NOTICE README.md INSTALLATION.md SECURITY.md CHANGELOG.md THIRD_PARTY_NOTICES.md "$dir/" 2>/dev/null || true
  cp docs/*.md "$dir/docs/" 2>/dev/null || true
  cp -r plugins "$dir/" 2>/dev/null || true
  mkdir -p "$dir/packaging" && cp -r packaging/linux "$dir/packaging/" 2>/dev/null || true
  cat > "$dir/EDITION.txt" <<TXT
CINVAR $ed edition $VERSION ($OS-$ARCH), commit $COMMIT
binary sha256 $SHA
TXT
  if [ "$ed" = "pro" ]; then
    cp docs/PRO_ACTIVATION.md "$dir/ACTIVATION.md"
    cp pro/LICENSE-PRO "$dir/LICENSE-PRO"
    echo "Pro = public core + proprietary module (LICENSE-PRO). Unlock with a signed license: cinvar edition activate <file>." >> "$dir/EDITION.txt"
  else
    echo "Free = the public Apache-2.0 core. It does not contain the Pro module." >> "$dir/EDITION.txt"
  fi
  # defense: no issuer tool or key material in any package
  if find "$dir" \( -name 'cinvar-license-issuer*' -o -name '*.key' -o -name 'license-signing*' \) | grep -q .; then echo "FATAL: issuer/key in package" >&2; exit 1; fi
  ( cd "$OUTROOT" && tar --sort=name --owner=0 --group=0 --numeric-owner --mtime="@$EPOCH" -cf - "$name" | gzip -n -9 > "$name.tar.gz" )
  rm -rf "$dir"
}
stage free; stage pro
( cd "$OUTROOT" && sha256sum ./*.tar.gz | sed 's# \./# #' > SHA256SUMS )
cat > "$OUTROOT/release.json" <<JSON
{"product":"CINVAR","version":"$VERSION","commit":"$COMMIT","platform":"$OS-$ARCH",
 "free_binary_sha256":"$FREE_SHA","pro_binary_sha256":"$PRO_SHA","rustc":"$(rustc --version)",
 "packages":["cinvar-free-$VERSION-$OS-$ARCH.tar.gz","cinvar-pro-$VERSION-$OS-$ARCH.tar.gz"],
 "note":"Free = public core; Pro = core + proprietary module. Reproducible at an identical checkout path."}
JSON
cat "$OUTROOT/SHA256SUMS"; echo "free bin $FREE_SHA"; echo "pro bin $PRO_SHA"
