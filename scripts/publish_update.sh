#!/usr/bin/env bash
# Update the EXISTING public CINVAR Free repository with the current core (no history rewrite, no force push).
# scripts/publish_github.sh is the one-time initial publisher; this is the release workflow for every later version.
#
#   scripts/publish_update.sh                 # PREPARE ONLY: clone, replace tree, scrub, build-verify, show the diff
#   scripts/publish_update.sh --push          # also commit, tag v<version>, push, and create the GitHub release
# The public tree is exactly the Apache-2.0 core: no pro/ module, no issuer-only secrets, no private business config.
# Needs `gh` logged in as the repository owner. Nothing is pushed without --push.
set -euo pipefail
cd "$(dirname "$0")/.."
TARGET="${CINVAR_PUBLIC_REPO:-colt2822/cinvar}"
VERSION="$(grep -m1 '^version' Cargo.toml | cut -d'"' -f2)"
PUSH=0; [ "${1:-}" = "--push" ] && PUSH=1
[ -z "$(git status --porcelain)" ] || { echo "working tree not clean"; exit 1; }
SRC_SHA="$(git rev-parse HEAD)"; SRC_ROOT="$PWD"
OWNER="${TARGET%%/*}"
[ "$(gh api user --jq .login)" = "$OWNER" ] || { echo "gh is not logged in as $OWNER"; exit 1; }

W="$(mktemp -d)"; trap 'rm -rf "$W"' EXIT
echo "==> clone $TARGET"
gh repo clone "$TARGET" "$W/repo" -- -q
cd "$W/repo"
BEFORE="$(git rev-parse HEAD)"
git rm -r -q --cached . >/dev/null; find . -mindepth 1 -maxdepth 1 ! -name .git -exec rm -rf {} +
git -C "$SRC_ROOT" archive "$SRC_SHA" | tar -x -C "$W/repo"

echo "==> remove the proprietary module and operator-only material"
rm -rf pro CINVAR_FREE_PRO_PLAN.md CINVAR_STOREFRONT_HANDOFF.md docs/LICENSE_OPERATIONS.md
python3 - <<'PY'
import re
s = open("Cargo.toml").read()
s = re.sub(r'\n\[workspace\]\nmembers = \["pro"\]\n', '\n', s)
open("Cargo.toml", "w").write(s)
l = open("Cargo.lock").read()
l = re.sub(r'\[\[package\]\]\nname = "cinvar-pro"\n.*?(?=\n\[\[package\]\]|\Z)', '', l, flags=re.S).replace("\n\n\n", "\n\n")
open("Cargo.lock", "w").write(l)
PY

echo "==> scrub: secrets, private strings, media, proprietary references"
PAT='(sk-[A-Za-z0-9]{20,}|ghp_[A-Za-z0-9]{30,}|xox[baprs]-[A-Za-z0-9-]{10,}|AKIA[0-9A-Z]{16}|AIza[0-9A-Za-z_-]{30,}|rpa_[A-Za-z0-9]{20,}|-----BEGIN [A-Z ]*PRIVATE KEY)'
if grep -rIn -E "$PAT" . --exclude-dir=.git --exclude=Cargo.lock | grep -v -E 'sk-abcdefghijklmnop|rpa_GOODKEY|rpa_FAKEKEY|rpa_DEMO_PLACEHOLDER|"-----BEGIN PRIVATE KEY-----"' | grep -q .; then echo "possible secrets: review"; exit 1; fi
if grep -rIn -E 'colton|mcclain|gmail\.com|/home/[a-z]+/|\.cinvar-issuer|license-signing\.key' . --exclude-dir=.git --exclude=Cargo.lock --exclude=publish_update.sh --exclude=publish_github.sh --exclude=release.sh --exclude=cinvar-license-issuer.rs --exclude=LICENSE_OPERATIONS.md | grep -q .; then
  grep -rIn -E 'colton|mcclain|gmail\.com|/home/[a-z]+/|\.cinvar-issuer|license-signing\.key' . --exclude-dir=.git --exclude=Cargo.lock --exclude=publish_update.sh --exclude=publish_github.sh --exclude=release.sh --exclude=cinvar-license-issuer.rs | head; echo "private strings in public tree"; exit 1; fi
find . -path ./.git -prune -o -type f \( -name '*.mp4' -o -name '*.mov' -o -name '*.mkv' -o -name '*.wav' -o -name '*.mp3' -o -name '*.key' \) -print | grep -q . && { echo "media/key files in tree"; exit 1; } || true
[ ! -e pro ] && ! grep -rIl 'cinvar_pro::' . --exclude-dir=.git --exclude=publish_update.sh | grep -q . || { echo "pro references remain"; exit 1; }
grep -q 'Apache License' LICENSE || { echo "LICENSE missing"; exit 1; }

echo "==> the public core builds on its own (locked)"
cargo build --locked --release --bin cinvar 2>&1 | tail -1
BIN=target/release/cinvar
"$BIN" tool list | grep -q 'cinvar.render.batch_start' && { echo "Pro tools in public build"; exit 1; } || echo "public build has no Pro tools"
rm -rf target
git add -A
echo "==> diff vs $TARGET@${BEFORE:0:7}"
git diff --cached --stat | tail -5
if [ "$PUSH" = 1 ]; then
  UID_GH="$(gh api user --jq .id)"
  GIT_AUTHOR_NAME="$OWNER" GIT_AUTHOR_EMAIL="${UID_GH}+${OWNER}@users.noreply.github.com" GIT_COMMITTER_NAME="$OWNER" GIT_COMMITTER_EMAIL="${UID_GH}+${OWNER}@users.noreply.github.com" \
    git commit -q -m "CINVAR $VERSION: Free and Pro editions (Apache-2.0 core)" -m "Core at source commit ${SRC_SHA:0:12}. The proprietary Pro module is not part of this repository."
  git tag "v$VERSION"
  git push origin HEAD:main "v$VERSION"
  echo "pushed $TARGET main + v$VERSION"
  OUT="$SRC_ROOT/dist-editions/$VERSION"; FREE="cinvar-free-$VERSION-linux-x86_64.tar.gz"
  if [ -f "$OUT/$FREE" ] && [ -f "$OUT/SHA256SUMS-$VERSION" ]; then
    gh release create "v$VERSION" "$OUT/$FREE" "$OUT/SHA256SUMS-$VERSION" --repo "$TARGET" --title "CINVAR $VERSION (Free)" \
      --notes "CINVAR Free $VERSION (Apache-2.0 core, Linux x86_64). Exports up to 2048x1080. CINVAR Pro Early Access is a separate paid package: https://elyraon.com.co/cinvar/. Verify with SHA256SUMS-$VERSION."
    echo "GitHub release v$VERSION created"
  fi
else
  echo "PREPARED ONLY (re-run with --push to publish). Public tree verified: no pro/, no secrets, builds locked."
fi
