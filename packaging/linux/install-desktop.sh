#!/usr/bin/env bash
# Installs a clickable CINVAR icon (app menu + Desktop). Run once after building.
set -euo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
APP="$HOME/.local/share/applications"; ICON="$HOME/.local/share/icons/hicolor/scalable/apps"
mkdir -p "$APP" "$ICON"
cp "$HERE/cinvar.svg" "$ICON/cinvar.svg"
BIN="$HERE/../../target/release/cinvar"; [ -x "$BIN" ] || BIN="$HERE/../../cinvar"
# Rendered from the shipped template so the desktop entry, the AppImage entry and the packaged
# launcher cannot drift apart.
TEMPLATE="$HERE/cinvar.desktop.in"
[ -f "$TEMPLATE" ] || { echo "error: missing $TEMPLATE" >&2; exit 1; }
EXEC_LINE="env CINVAR_BIN=$(readlink -f "$BIN") $(readlink -f "$HERE/cinvar-launcher.sh")"
EXEC_LINE="$EXEC_LINE" python3 - "$TEMPLATE" "$APP/cinvar.desktop" <<'PY'
import os, sys
template, out = sys.argv[1], sys.argv[2]
exec_line = os.environ["EXEC_LINE"]
text = open(template, encoding="utf-8").read().replace("@@EXEC@@", exec_line)
if "@@EXEC@@" in text:
    sys.exit("desktop template still has a placeholder")
open(out, "w", encoding="utf-8").write(text)
PY
chmod +x "$APP/cinvar.desktop" "$HERE/cinvar-launcher.sh"
if command -v desktop-file-validate >/dev/null; then
  desktop-file-validate "$APP/cinvar.desktop" || echo "warning: desktop-file-validate reported problems" >&2
fi
D="$(xdg-user-dir DESKTOP 2>/dev/null || echo "$HOME/Desktop")"
[ -d "$D" ] && { cp "$APP/cinvar.desktop" "$D/CINVAR.desktop"; chmod +x "$D/CINVAR.desktop"; gio set "$D/CINVAR.desktop" metadata::trusted true 2>/dev/null || true; }
echo "Installed. Find CINVAR in your app menu or on the Desktop."
