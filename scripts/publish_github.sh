#!/usr/bin/env bash
# Publish CINVAR to GitHub as a PUBLIC repository with a clean squashed history.
# Owner-approved target: colt2822/cinvar. Refuses to run without an explicit confirmation naming it:
#
#   CINVAR_PUBLISH_CONFIRM=colt2822/cinvar ./scripts/publish_github.sh
#
# What it does:
#   1. preflight: clean tree, no existing 'origin', target repo does not exist, secret/private-path scans
#   2. preserves the FULL original history locally (git bundle in $HOME + local tag)
#   3. builds ONE fresh commit (orphan branch) authored with the GitHub noreply address - no demo media in
#      history, no local paths, no private email
#   4. creates the PUBLIC repo, pushes `main`, enables private vulnerability reporting and topics
# It does NOT tag or release: tag/release after CI is green (see docs/RELEASE_NOTES_0.1.0-alpha.md).
set -euo pipefail
cd "$(dirname "$0")/.."

TARGET="${CINVAR_PUBLISH_CONFIRM:-}"
[ "$TARGET" = "colt2822/cinvar" ] || { sed -n 2,16p "$0"; echo; echo "refusing: set CINVAR_PUBLISH_CONFIRM=colt2822/cinvar"; exit 2; }
command -v gh >/dev/null || { echo "gh CLI required"; exit 1; }
OWNER="${TARGET%%/*}"
UID_GH="$(gh api user --jq .id)"
[ "$(gh api user --jq .login)" = "$OWNER" ] || { echo "gh is not logged in as $OWNER"; exit 1; }

echo "==> preflight"
[ -z "$(git status --porcelain)" ] || { echo "working tree not clean"; exit 1; }
if git remote get-url origin >/dev/null 2>&1; then echo "an 'origin' remote already exists"; exit 1; fi
if gh repo view "$TARGET" >/dev/null 2>&1; then echo "$TARGET already exists; refusing to overwrite"; exit 1; fi
SRC_SHA="$(git rev-parse HEAD)"; SRC_BRANCH="$(git rev-parse --abbrev-ref HEAD)"

PAT='(sk-[A-Za-z0-9]{20,}|ghp_[A-Za-z0-9]{30,}|xox[baprs]-[A-Za-z0-9-]{10,}|AKIA[0-9A-Z]{16}|AIza[0-9A-Za-z_-]{30,}|rpa_[A-Za-z0-9]{20,}|-----BEGIN [A-Z ]*PRIVATE KEY)'
echo "==> secret-pattern scan (tree + all history)"
if git grep -I -n -E "$PAT" $(git rev-list --all) -- . ':!Cargo.lock' | grep -v -E 'sk-abcdefghijklmnop|rpa_GOODKEY|rpa_FAKEKEY|rpa_DEMO_PLACEHOLDER|fs::write\(&secret, "-----BEGIN PRIVATE KEY-----"\)' | grep -q .; then
  echo "possible secrets found - review before publishing"; exit 1
fi
echo "==> private-path / personal-detail scan (tree to be published)"
if git grep -I -n -E 'colton|mcclain|gmail\.com|/home/[a-z]+/' -- . ':!Cargo.lock' ':!scripts/publish_github.sh' | grep -q .; then
  git grep -I -n -E 'colton|mcclain|gmail\.com|/home/[a-z]+/' -- . ':!Cargo.lock' ':!scripts/publish_github.sh' | head; echo "private strings in tree"; exit 1
fi
if git ls-files | grep -E '\.(mp4|mov|mkv|wav|mp3)$' | grep -q .; then echo "media files are tracked"; exit 1; fi

echo "==> preserving the full original history locally"
BUNDLE="$HOME/cinvar-full-history-$(date +%Y%m%d).bundle"
git bundle create "$BUNDLE" --all
git tag -f "archive/full-history-${SRC_SHA:0:7}" "$SRC_SHA" >/dev/null
echo "    bundle: $BUNDLE   local tag: archive/full-history-${SRC_SHA:0:7}   (source $SRC_BRANCH @ ${SRC_SHA:0:7})"

echo "==> building the clean single-commit branch"
git checkout -q --orphan public-main
git add -A
GIT_AUTHOR_NAME="$OWNER" GIT_AUTHOR_EMAIL="${UID_GH}+${OWNER}@users.noreply.github.com" \
GIT_COMMITTER_NAME="$OWNER" GIT_COMMITTER_EMAIL="${UID_GH}+${OWNER}@users.noreply.github.com" \
git commit -q -m "CINVAR 0.1.0-alpha: initial public source release" \
  -m "Open-source, model-agnostic AI video production engine (Rust, FFmpeg, Blender, MCP). Full pre-publication history is preserved by the maintainers; this is its squashed, cleaned form." \
  -m "Co-Authored-By: Claude Sonnet 5.5 <noreply@anthropic.com>"
git branch -M main

echo "==> creating PUBLIC repository $TARGET and pushing main"
gh repo create "$TARGET" --public --source . --remote origin \
  --description "Open-source, model-agnostic AI-native video production engine: Rust, FFmpeg, Blender, 8K, MCP" --push
gh repo edit "$TARGET" --add-topic video-editing --add-topic ffmpeg --add-topic mcp --add-topic rust --add-topic blender --add-topic ai-agents || true
gh api -X PUT "repos/$TARGET/private-vulnerability-reporting" >/dev/null 2>&1 && echo "private vulnerability reporting enabled" || echo "could not enable private vulnerability reporting (do it in Settings > Security)"
echo "==> published: https://github.com/$TARGET  ($(git rev-parse --short HEAD))"
