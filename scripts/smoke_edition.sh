#!/usr/bin/env bash
# Smoke-test a packaged edition tarball end to end with clean config/data dirs.
#   scripts/smoke_edition.sh dist-editions/0.1.0/cinvar-free-0.1.0-linux-x86_64.tar.gz
# Pro tarballs additionally need ISSUER=<path to cinvar-license-issuer> (built with --features issuer)
# and optionally KEY=<signing key> (default ~/.cinvar-issuer/license-signing.key) to issue a real license.
# Needs ffmpeg/ffprobe. Exits non-zero on the first failed expectation.
set -euo pipefail
TAR="$(readlink -f "$1")"
W="$(mktemp -d)"; trap 'rm -rf "$W"' EXIT
tar xzf "$TAR" -C "$W"; D="$(echo "$W"/cinvar-*)"; BIN="$D/cinvar"
export CINVAR_CONFIG_DIR="$W/cfg" CINVAR_DATA_DIR="$W/data"; mkdir -p "$W/cfg" "$W/data" "$W/footage"
ok() { echo "PASS  $*"; }; die() { echo "FAIL  $*"; exit 1; }
size() { ffprobe -v error -select_streams v:0 -show_entries stream=width,height -of csv=p=0 "$1"; }

"$BIN" --version >/dev/null && ok "launches: $("$BIN" --version)"
[ -f "$D/EDITION.txt" ] && ok "edition marker: $(head -1 "$D/EDITION.txt")"
ST="$("$BIN" --json edition status)"
echo "$ST" | grep -Eq '"edition": *"free"' && ok "no license => free" || die "status: $ST"
echo "$ST" | grep -Eq '"enforcement": *"enforce"' && ok "enforcement on in the packaged binary" || die "enforcement off: $ST"

ffmpeg -y -v error -f lavfi -i testsrc2=size=3840x2160:rate=24:duration=2 -f lavfi -i sine=frequency=440:duration=2 -c:v libx264 -pix_fmt yuv420p -c:a aac "$W/footage/clip4k.mp4"
ffmpeg -y -v error -f lavfi -i testsrc2=size=1280x720:rate=24:duration=2 -f lavfi -i sine=frequency=330:duration=2 -c:v libx264 -pix_fmt yuv420p -c:a aac "$W/footage/clip720.mp4"
"$BIN" setup --yes >/dev/null; "$BIN" models add mock --provider mock --model mock-planner >/dev/null
"$BIN" project create --name Smoke --dir "$W" >/dev/null; P="$W/smoke"
( cd "$P" && "$BIN" run --media-root "$W/footage" "Combine the clips in $W/footage and export a 1080p video" >/dev/null ) && ok "imports a 4K source, edits, saves project, exports"
OUT="$(ls "$P"/renders/*.mp4 | head -1)"; [ "$(size "$OUT")" = "1920,1080" ] && ok "mock-planned export is 1920x1080 (from 4K+720p sources)" || die "export size $(size "$OUT")"

r() { "$BIN" --project "$P" render --output "$P/renders/$1.mp4" --width "$2" --height "$3" --wait; }
for s in "1920 1080" "2048 1080" "1080 2048" "1280 720"; do set -- $s
  r "ok_$1x$2" "$1" "$2" >/dev/null 2>&1 && [ "$(size "$P/renders/ok_$1x$2.mp4")" = "$1,$2" ] && ok "free exports $1x$2" || die "free should export $1x$2"; done
for s in "2560 1440" "3840 2160" "7680 4320" "1440 2560" "2050 1080"; do set -- $s
  if r "no_$1x$2" "$1" "$2" > "$W/err.txt" 2>&1; then die "free exported $1x$2"; fi
  grep -q 'Free' "$W/err.txt" && [ ! -e "$P/renders/no_$1x$2.mp4" ] && ok "free refuses $1x$2" || die "unexpected failure for $1x$2: $(cat "$W/err.txt")"; done

# other render paths must enforce the same ceiling (tool/MCP export, Blender, implicit-size export of a 4K canvas)
tcall() { "$BIN" --project "$P" tool call "$@"; }
if tcall cinvar.render.export --wait --args "{\"output\":\"$P/renders/tool4k.mp4\",\"width\":3840,\"height\":2160}" >"$W/err.txt" 2>&1; then die "tool render.export accepted 4K"; fi
[ ! -e "$P/renders/tool4k.mp4" ] && ok "tool/MCP export refuses 4K in free (job fails, no file)" || die "tool export produced a 4K file"
if tcall cinvar.blender.animation --args '{"template":"logo_reveal","text":"x","width":3840,"height":2160}' >"$W/err.txt" 2>&1; then die "blender accepted 4K"; fi
grep -q 'Free' "$W/err.txt" && ok "blender animation refuses 4K in free" || die "blender unexpected: $(cat "$W/err.txt")"
"$BIN" project create --name Canvas4k --dir "$W" >/dev/null; C="$W/canvas4k"
"$BIN" --project "$C" tool call cinvar.timeline.create --args '{"name":"Main","width":3840,"height":2160,"fps":"24"}' >/dev/null
AID="$(cp "$W/footage/clip4k.mp4" "$C/clip4k.mp4"; "$BIN" --project "$C" --json tool call cinvar.media.import --args "{\"path\":\"$C/clip4k.mp4\"}" | sed -n 's/.*"asset_id": *"\([^"]*\)".*/\1/p' | head -1)"
"$BIN" --project "$C" tool call cinvar.timeline.append_clip --args "{\"asset_id\":\"$AID\"}" >/dev/null
"$BIN" --project "$C" render --output "$C/renders/implicit.mp4" --wait >/dev/null 2>&1; S4="$(size "$C/renders/implicit.mp4")"
case "$TAR" in *-free-*) [ "$S4" = "1920,1080" ] && ok "implicit export of a 4K canvas is fitted to 1920x1080 in free" || die "implicit export gave $S4";; esac
printf '[edition]\nenforce = false\n' > "$CINVAR_CONFIG_DIR/config.toml"
if r no_config 3840 2160 >/dev/null 2>&1; then die "config file loosened enforcement"; else ok "config cannot disable enforcement"; fi
echo junk > "$W/lic.json"
if "$BIN" edition activate --file "$W/lic.json" >/dev/null 2>&1; then die "junk license accepted"; else ok "junk license refused"; fi
"$BIN" --json edition status | grep -Eq '"edition": *"free"' && ok "still free after bad activation" || die "edition changed"

TOOLS="$("$BIN" tool list)"
case "$TAR" in
*-free-*)
  echo "$TOOLS" | grep -q 'cinvar.render.batch_start' && die "free binary contains Pro tools" || ok "free binary has no Pro tools (module absent)"
  if [ -n "${ISSUER:-}" ]; then
    "$ISSUER" issue --order-ref smoke-free --out "$W/pro-lic.json" --ledger "$W/ledger.csv" --key "${KEY:-$HOME/.cinvar-issuer/license-signing.key}" >/dev/null
    "$BIN" edition activate --file "$W/pro-lic.json" >/dev/null 2>&1 || true
    "$BIN" --json edition status | grep -q 'not include\|module' && ok "free binary + valid license: explains Pro module is missing" || die "free+license status unclear"
    # the export ceiling is a core capability, so a valid license lifts it in the Free binary too;
    # module features (batch/presets/color) stay absent there.
    r lic4k 3840 2160 >/dev/null 2>&1 && [ "$(size "$P/renders/lic4k.mp4")" = "3840,2160" ] && ok "valid license lifts the export ceiling in the free binary" || die "licensed 4K export failed in free binary"
  fi ;;
*-pro-*)
  echo "$TOOLS" | grep -q 'cinvar.render.batch_start' || die "pro binary lacks Pro tools"
  ok "pro binary registers Pro tools"
  [ -n "${ISSUER:-}" ] || die "set ISSUER for Pro smoke"
  "$ISSUER" issue --order-ref smoke-pro --out "$W/pro-lic.json" --ledger "$W/ledger.csv" --key "${KEY:-$HOME/.cinvar-issuer/license-signing.key}" >/dev/null && ok "issued a license with the production key"
  # refused: tampered signature, tampered payload, and a license signed by a different (non-production) key
  python3 - "$W/pro-lic.json" "$W" <<'PY'
import json, sys, base64
lic = json.load(open(sys.argv[1])); w = sys.argv[2]
sig = bytearray(base64.b64decode(lic["signature"])); sig[0] ^= 1
json.dump({**lic, "signature": base64.b64encode(bytes(sig)).decode()}, open(w + "/bad-sig.json", "w"))
pl = json.loads(base64.b64decode(lic["payload"])); pl["edition"] = "pro"; pl["id"] = "forged"
json.dump({**lic, "payload": base64.b64encode(json.dumps(pl).encode()).decode()}, open(w + "/bad-payload.json", "w"))
open(w + "/malformed.json", "w").write('{"key_id":"k1","payload":')
PY
  "$ISSUER" keygen --out "$W/other-key" >/dev/null 2>&1 && "$ISSUER" issue --order-ref other-key --out "$W/other-signed.json" --ledger "$W/ledger.csv" --key "$W/other-key/license-signing.key" >/dev/null
  for f in bad-sig bad-payload malformed other-signed; do
    if "$BIN" edition activate --file "$W/$f.json" >/dev/null 2>&1; then die "$f license accepted"; fi
  done
  "$BIN" --json edition status | grep -Eq '"edition": *"free"' && ok "tampered, malformed and wrong-key licenses refused; still free" || die "bad license changed edition"
  "$BIN" edition activate --file "$W/pro-lic.json" >/dev/null && ok "activated offline"
  ST="$("$BIN" --json edition status)"
  echo "$ST" | grep -Eq '"edition": *"pro"' && ok "edition is pro after activation" || die "not pro: $ST"
  r pro_4k 3840 2160 >/dev/null 2>&1 && [ "$(size "$P/renders/pro_4k.mp4")" = "3840,2160" ] && ok "pro exports 3840x2160" || die "pro 4K export failed"
  "$BIN" --project "$P" tool call cinvar.preset.save --args '{"preset":{"name":"smoke4k","width":3840,"height":2160,"fps":"24","codec":"h264","crf":23}}' >/dev/null && ok "preset saved" || die "preset save failed"
  "$BIN" --project "$P" tool call cinvar.color.grade --args '{"all":true,"grade":{"exposure":0.4,"saturation":1.3}}' >/dev/null && ok "color grade applied" || die "color grade failed"
  "$BIN" --project "$P" tool call cinvar.color.grade_preset_save --args '{"name":"warm","grade":{"exposure":0.2,"temperature":7200}}' >/dev/null && "$BIN" --project "$P" tool call cinvar.color.grade_preset_apply --args '{"name":"warm","all":true}' >/dev/null && ok "grade preset saved and re-applied" || die "grade preset reuse failed"
  B="$("$BIN" --project "$P" tool call cinvar.render.batch_start --wait --args "{\"items\":[{\"output\":\"$P/renders/b1.mp4\",\"preset\":\"smoke4k\"},{\"output\":\"$P/renders/b2.mp4\",\"width\":1280,\"height\":720}]}")"
  [ "$(size "$P/renders/b1.mp4")" = "3840,2160" ] && [ "$(size "$P/renders/b2.mp4")" = "1280,720" ] && ok "batch rendered both items (preset 4K + 720p)" || die "batch outputs wrong: $B"
  "$BIN" edition deactivate >/dev/null; if r back_free 3840 2160 >/dev/null 2>&1; then die "4K after deactivate"; else ok "deactivate returns to Free ceiling"; fi
  "$BIN" --project "$P" render --output "$P/renders/graded_in_free.mp4" --width 1280 --height 720 --wait >/dev/null 2>&1 && ok "Pro-graded project still renders in Free (grade skipped)" || die "graded project failed in free" ;;
esac
echo "SMOKE OK ($TAR)"
