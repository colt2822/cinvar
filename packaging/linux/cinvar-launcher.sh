#!/usr/bin/env bash
# One-click launcher: starts the CINVAR local web UI and opens it in your browser.
# Projects live in ~/CINVAR Projects (created on first run). Close the terminal-less app by
# running this again or using the "Quit" entry; the server stops when its window process ends.
set -euo pipefail
HERE="$(cd "$(dirname "$(readlink -f "$0")")" && pwd)"
BIN="${CINVAR_BIN:-$HERE/cinvar}"
[ -x "$BIN" ] || BIN="$(command -v cinvar || true)"
[ -x "${BIN:-}" ] || BIN="$HERE/../../target/release/cinvar"
PROJ="${CINVAR_PROJECT:-$HOME/CINVAR Projects/default}"
mkdir -p "$PROJ"
LOG="${XDG_STATE_HOME:-$HOME/.local/state}/cinvar-ui.log"; mkdir -p "$(dirname "$LOG")"
if ! command -v ffmpeg >/dev/null; then
  msg="CINVAR needs FFmpeg. Install it (sudo apt install ffmpeg) and open CINVAR again."
  command -v notify-send >/dev/null && notify-send "CINVAR" "$msg"; echo "$msg" >&2; exit 1
fi
# Reuse a running instance.
if URL=$(grep -o 'http://127.0.0.1:[0-9]*' "$LOG" 2>/dev/null | tail -1) && [ -n "$URL" ] \
   && curl -fsS -m 1 "$URL" >/dev/null 2>&1; then
  xdg-open "$URL" >/dev/null 2>&1; exit 0
fi
: > "$LOG"
setsid nohup "$BIN" ui --project "$PROJ" >>"$LOG" 2>&1 &
for _ in $(seq 1 50); do
  URL=$(grep -o 'http://127.0.0.1:[0-9]*' "$LOG" | head -1 || true)
  [ -n "$URL" ] && break; sleep 0.2
done
[ -n "${URL:-}" ] || { echo "CINVAR failed to start; see $LOG" >&2; exit 1; }
# Prefer a chrome-less app window (looks like a native app); fall back to the default browser.
# The engine keeps running in the background so re-opening the icon is instant; it is idle and local-only.
for B in google-chrome chromium chromium-browser brave-browser microsoft-edge; do
  P="$(command -v "$B" || true)"
  if [ -n "$P" ]; then
    EXTRA=()
    case "$(readlink -f "$P")" in /snap/*|*snap*) ;; *) EXTRA=(--user-data-dir="${XDG_STATE_HOME:-$HOME/.local/state}/cinvar-browser");; esac
    nohup "$P" --app="$URL" "${EXTRA[@]}" --window-size=1500,920 >/dev/null 2>&1 &
    disown; exit 0
  fi
done
xdg-open "$URL" >/dev/null 2>&1 || true
exit 0
