#!/usr/bin/env bash
# Render a Homebrew formula for CINVAR from real release artifacts and their sha256 digests.
#
#   scripts/render_homebrew_formula.sh --version 0.2.0 \
#       --linux-x86_64 dist/cinvar-0.2.0-linux-x86_64.tar.gz \
#       --macos-arm64 cinvar-0.2.0-macos-arm64.tar.gz
#
# Every artifact given must have a sibling <artifact>.sha256 (scripts/package_linux.sh writes one).
# Digests are read from the artifact itself, never from the file name, and the rendered formula is
# validated by asking Ruby to parse it. A platform whose artifact is missing renders an explicit
# "no artifact" guard instead of a silent, broken download URL.
#
# Output: Formula/cinvar.rb (override with --out PATH) and a JSON summary on stdout tail.
set -euo pipefail
cd "$(dirname "$0")/.."
ROOT="$PWD"
VERSION=""
OUT="Formula/cinvar.rb"
REPO="${CINVAR_REPO:-colt2822/cinvar}"
declare -A FILE=()

say() { printf '\033[1;34m==>\033[0m %s\n' "$*"; }
die() { printf '\033[1;31merror:\033[0m %s\n' "$*" >&2; exit 1; }
usage() { sed -n '2,12p' "$0" | sed 's/^# \{0,1\}//'; exit 2; }

while [ "$#" -gt 0 ]; do
  case "$1" in
    --version) VERSION="${2:?}"; shift 2 ;;
    --out) OUT="${2:?}"; shift 2 ;;
    --repo) REPO="${2:?}"; shift 2 ;;
    --linux-x86_64|--linux-arm64|--macos-x86_64|--macos-arm64)
      FILE["${1#--}"]="${2:?$1 needs an artifact}"; shift 2 ;;
    -h|--help) usage ;;
    *) die "unknown option $1" ;;
  esac
done
[ -n "$VERSION" ] || die "--version is required (the Cargo.toml version of the artifacts)"

sha256_of() {
  if command -v sha256sum >/dev/null; then sha256sum "$1" | cut -d' ' -f1
  elif command -v shasum >/dev/null; then shasum -a 256 "$1" | cut -d' ' -f1
  else die "no sha256sum or shasum"; fi
}

# Resolve digests, preferring a sibling .sha256 but never trusting a mismatched one.
declare -A SHA=() URL=()
for key in "${!FILE[@]}"; do
  f="${FILE[$key]}"
  [ -f "$f" ] || die "declared artifact $f does not exist; render the formula from real release artifacts only"
  actual="$(sha256_of "$f")"
  if [ -f "$f.sha256" ]; then
    recorded="$(cut -d' ' -f1 "$f.sha256")"
    [ "$recorded" = "$actual" ] || die "$f does not match its recorded sha256 ($recorded vs $actual)"
  fi
  SHA["$key"]="$actual"
  URL["$key"]="https://github.com/$REPO/releases/download/v$VERSION/$(basename "$f")"
  say "$key: $(basename "$f") sha256 $actual"
done

block() {
  local key="$1" indent="$2"
  if [ -n "${SHA[$key]:-}" ]; then
    cat <<BLOCK
${indent}url "${URL[$key]}"
${indent}sha256 "${SHA[$key]}"
BLOCK
  else
    cat <<BLOCK
${indent}odie "CINVAR $VERSION has no $key release artifact; build one with the recipe in docs/PACKAGING.md"
BLOCK
  fi
}

mkdir -p "$(dirname "$OUT")"
say "rendering $OUT"
{
cat <<'HEADER'
# typed: false
# frozen_string_literal: true

# Rendered by scripts/render_homebrew_formula.sh from real release artifacts and their sha256
# digests. Do not edit by hand: re-render it instead, so the URLs and digests stay in step with
# what was actually published.
class Cinvar < Formula
  desc "AI video creation harness"
  homepage "https://github.com/colt2822/cinvar"
HEADER
echo "  version \"$VERSION\""
echo "  license \"Apache-2.0\""
echo
cat <<'BODY'
  depends_on "ffmpeg"

  on_macos do
    depends_on :macos
    if Hardware::CPU.arm?
BODY
block macos-arm64 "      "
cat <<'BODY2'
    else
BODY2
block macos-x86_64 "      "
cat <<'BODY3'
    end
  end

  on_linux do
    if Hardware::CPU.arm?
BODY3
block linux-arm64 "      "
cat <<'BODY4'
    else
BODY4
block linux-x86_64 "      "
cat <<'BODY5'
    end
  end

  def install
    bin.install "cinvar"
    (share/"doc/cinvar").install Dir["*.md"], "LICENSE", "NOTICE" if Dir["*.md"].any?
    (share/"cinvar").install "packaging/linux/cinvar-launcher.sh" if File.exist?("packaging/linux/cinvar-launcher.sh")
    (share/"cinvar").install "packaging/linux/cinvar.svg" if File.exist?("packaging/linux/cinvar.svg")
    (share/"doc/cinvar").install "docs" if File.directory?("docs")
  end

  def caveats
    <<~CAVEAT
      The Pro AI stack is installed per user, not by Homebrew:

        cinvar setup --pro-ai

      That creates one virtualenv per component under ~/.local/share/cinvar/venvs and records every
      resolved path in ~/.config/cinvar/pro-ai.toml. Add ~/.local/bin to PATH if it is not there.
    CAVEAT
  end

  test do
    assert_match version.to_s, shell_output("#{bin}/cinvar --version")
    system "#{bin}/cinvar", "doctor", "--json"
  end
end
BODY5
} > "$OUT"

if command -v ruby >/dev/null; then
  ruby -c "$OUT" >/dev/null || die "rendered formula does not parse as Ruby"
  say "ruby -c: syntax ok"
else
  say "warning: no ruby on PATH, formula syntax not validated"
fi

echo
echo "formula: $OUT"
echo "version: $VERSION"
for key in "${!SHA[@]}"; do echo "  $key sha256 ${SHA[$key]}"; done
