#!/usr/bin/env bash
# CINVAR release pipeline: build Free + Pro, test, smoke, checksum, manifest, R2 upload, upload verification,
# storefront mapping, rollback. Free SOURCE publication (scripts/publish_github.sh) and proprietary Pro
# distribution are separate; production signing material is never read here except by the smoke test's issuer.
#
#   scripts/release.sh build                      # tests + packages + smoke on both tarballs + manifest
#   scripts/release.sh storefront                 # write catalog/cinvar-release.json in $STOREFRONT_DIR (keeps .prev for rollback)
#   scripts/release.sh r2 staging|production      # upload Free (public/) + Pro (products/); --dry-run prints only
#   scripts/release.sh verify-r2 <origin>         # download the public Free file over HTTP, compare sha256 to the manifest
#   scripts/release.sh rollback                   # restore the previous storefront mapping (redeploy afterwards)
# Env: STOREFRONT_DIR (storefront repo/worktree), ISSUER (cinvar-license-issuer binary; built here if unset).
set -euo pipefail
cd "$(dirname "$0")/.."
VERSION="$(grep -m1 '^version' Cargo.toml | cut -d'"' -f2)"
ARCH="linux-x86_64"
OUT="dist-editions/$VERSION"
FREE="cinvar-free-$VERSION-$ARCH.tar.gz"; PRO="cinvar-pro-$VERSION-$ARCH.tar.gz"
STOREFRONT_DIR="${STOREFRONT_DIR:-$HOME/elyraon.com.co}"
cmd="${1:-}"; shift || true

sha() { sha256sum "$1" | cut -d' ' -f1; }
need_built() { [ -f "$OUT/$FREE" ] && [ -f "$OUT/$PRO" ] && [ -f "$OUT/SHA256SUMS" ] || { echo "run: scripts/release.sh build"; exit 1; }; }

case "$cmd" in
build)
  [ -z "$(git status --porcelain)" ] || { echo "working tree not clean: release builds come from a committed source"; exit 1; }
  echo "==> tests"
  cargo test --offline --locked 2>&1 | tail -3
  cargo test --offline --locked -p cinvar-pro 2>&1 | tail -3
  cargo test --offline --locked --features issuer --test issuer 2>&1 | tail -3
  echo "==> issuer (operator-only, never packaged)"
  if [ -z "${ISSUER:-}" ]; then
    cargo build --offline --locked --release --features issuer --bin cinvar-license-issuer 2>&1 | tail -1
    ISSUER="$PWD/target/release/cinvar-license-issuer"
  fi
  echo "==> packages"
  scripts/package_editions.sh
  echo "==> reproducibility (fresh target dir, same path)"
  SUMS_A="$(mktemp)"; cp "$OUT/SHA256SUMS" "$SUMS_A"
  CINVAR_PKG_TARGET_DIR=target-release-editions-repro scripts/package_editions.sh >/dev/null
  rm -rf target-release-editions-repro
  diff "$SUMS_A" "$OUT/SHA256SUMS" && echo "reproducible: identical sha256" || { echo "NOT reproducible"; exit 1; }
  rm -f "$SUMS_A"
  echo "==> packaged smoke"
  for t in "$FREE" "$PRO"; do ISSUER="$ISSUER" scripts/smoke_edition.sh "$OUT/$t" | tail -1; done
  echo "==> package hygiene"
  FREE_LIST="$(tar tzf "$OUT/$FREE")"; PRO_LIST="$(tar tzf "$OUT/$PRO")"   # captured first: grep -q + pipefail would SIGPIPE tar
  for list in "$FREE_LIST" "$PRO_LIST"; do
    if grep -E 'license-issuer|\.key$|license-signing' <<<"$list"; then echo "FATAL: issuer/key material in a package"; exit 1; fi
  done
  if grep -E '(^|/)pro/|LICENSE-PRO|cinvar-pro' <<<"$FREE_LIST"; then echo "FATAL: Pro material in Free package"; exit 1; fi
  grep -q 'LICENSE-PRO' <<<"$PRO_LIST" || { echo "FATAL: Pro package lacks LICENSE-PRO"; exit 1; }
  echo "package hygiene OK"
  echo "==> manifest"
  python3 - "$OUT" "$FREE" "$PRO" "$VERSION" <<'PY'
import json, os, subprocess, sys
out, free, pro, ver = sys.argv[1:5]
rel = json.load(open(f"{out}/release.json"))
sums = dict(l.split()[::-1] for l in open(f"{out}/SHA256SUMS").read().strip().splitlines())
m = {"product": "CINVAR", "version": ver, "source_commit": rel["commit"], "platform": rel["platform"], "rustc": rel["rustc"],
     "free": {"file": free, "dir": free[:-7], "size_bytes": os.path.getsize(f"{out}/{free}"), "sha256": sums[free], "binary_sha256": rel["free_binary_sha256"]},
     "pro": {"file": pro, "dir": pro[:-7], "size_bytes": os.path.getsize(f"{out}/{pro}"), "sha256": sums[pro], "binary_sha256": rel["pro_binary_sha256"]}}
json.dump(m, open(f"{out}/cinvar-release.json", "w"), indent=2); print(json.dumps(m, indent=2))
PY
  echo "$(sha "$OUT/$FREE")  $FREE" > "$OUT/SHA256SUMS-$VERSION"
  echo "BUILD OK ($OUT)"
  ;;
storefront)
  need_built
  [ -d "$STOREFRONT_DIR/catalog" ] || { echo "STOREFRONT_DIR/catalog not found"; exit 1; }
  [ -f "$STOREFRONT_DIR/catalog/cinvar-release.json" ] && cp "$STOREFRONT_DIR/catalog/cinvar-release.json" "$STOREFRONT_DIR/catalog/cinvar-release.prev.json"
  python3 - "$OUT/cinvar-release.json" "$STOREFRONT_DIR/catalog/cinvar-release.json" <<'PY'
import json, sys
m = json.load(open(sys.argv[1]))
json.dump({"_comment": "Written by scripts/release.sh storefront. Do not edit by hand.", "version": m["version"], "free": {k: m["free"][k] for k in ("file", "dir", "size_bytes", "sha256")},
           "pro": {k: m["pro"][k] for k in ("file", "dir", "size_bytes", "sha256")}, "source_commit": m["source_commit"]}, open(sys.argv[2], "w"), indent=2)
open(sys.argv[2], "a").write("\n")
PY
  echo "wrote $STOREFRONT_DIR/catalog/cinvar-release.json (previous kept as cinvar-release.prev.json)"
  ;;
r2)
  need_built
  target="${1:?staging|production}"; shift || true
  case "$target" in staging) BUCKET=elyraon-products-staging; CFG=wrangler.staging.toml;; production) BUCKET=elyraon-products; CFG=wrangler.toml;; *) echo "staging|production"; exit 1;; esac
  put() { # $1 key  $2 file  $3 content-type
    if [ "${1:-}" = "--dry-run" ]; then :; fi
    echo "r2://$BUCKET/$1  <-  $2 ($(sha "$2"))"
    [ "${DRY:-0}" = 1 ] || ( cd "$STOREFRONT_DIR" && npx --no-install wrangler r2 object put "$BUCKET/$1" --file "$OLDPWD/$2" --content-type "$3" --remote -c "$CFG" )
  }
  DRY=0; [ "${1:-}" = "--dry-run" ] && DRY=1
  put "public/cinvar/$FREE" "$OUT/$FREE" application/gzip
  put "public/cinvar/SHA256SUMS-$VERSION" "$OUT/SHA256SUMS-$VERSION" text/plain
  put "products/$PRO" "$OUT/$PRO" application/gzip
  ;;
verify-r2)
  need_built
  origin="${1:?origin, e.g. https://elyraon.com.co}"
  tmp="$(mktemp -d)"; trap 'rm -rf "$tmp"' EXIT
  curl -fsS -o "$tmp/$FREE" "$origin/downloads/cinvar/$FREE"
  [ "$(sha "$tmp/$FREE")" = "$(sha "$OUT/$FREE")" ] && echo "PASS public Free download sha256 matches ($(sha "$tmp/$FREE"))" || { echo "FAIL sha mismatch"; exit 1; }
  [ "$(stat -c %s "$tmp/$FREE")" = "$(stat -c %s "$OUT/$FREE")" ] && echo "PASS size matches"
  code="$(curl -s -o /dev/null -w '%{http_code}' "$origin/downloads/cinvar/$PRO")"
  [ "$code" = 404 ] && echo "PASS Pro package is not publicly downloadable (HTTP $code)" || { echo "FAIL Pro reachable on public route ($code)"; exit 1; }
  # isolated config/data dirs: a license activated on the operator's own machine must not leak into this check
  export CINVAR_CONFIG_DIR="$tmp/cfg" CINVAR_DATA_DIR="$tmp/data"; mkdir -p "$tmp/cfg" "$tmp/data"
  tar xzf "$tmp/$FREE" -C "$tmp" && "$tmp/${FREE%.tar.gz}/cinvar" --version && "$tmp/${FREE%.tar.gz}/cinvar" --json edition status | grep -E '"edition": *"free"' >/dev/null && echo "PASS downloaded Free extracts, launches, reports free"
  ;;
rollback)
  [ -f "$STOREFRONT_DIR/catalog/cinvar-release.prev.json" ] || { echo "no previous mapping"; exit 1; }
  cp "$STOREFRONT_DIR/catalog/cinvar-release.prev.json" "$STOREFRONT_DIR/catalog/cinvar-release.json"
  echo "restored previous mapping; rebuild and redeploy the storefront. Old R2 objects are versioned by file name and were never overwritten."
  ;;
*) sed -n 2,12p "$0"; exit 1;;
esac
