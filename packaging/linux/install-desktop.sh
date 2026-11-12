#!/usr/bin/env bash
# Installs a clickable CINVAR icon (app menu + Desktop). Run once after building.
set -euo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
APP="$HOME/.local/share/applications"; ICON="$HOME/.local/share/icons/hicolor/scalable/apps"
mkdir -p "$APP" "$ICON"
cp "$HERE/cinvar.svg" "$ICON/cinvar.svg"
BIN="$HERE/../../target/release/cinvar"; [ -x "$BIN" ] || BIN="$HERE/../../cinvar"
cat > "$APP/cinvar.desktop" <<D
[Desktop Entry]
Type=Application
Name=CINVAR
Comment=AI video editor
Exec=env CINVAR_BIN=$(readlink -f "$BIN") $(readlink -f "$HERE/cinvar-launcher.sh")
Icon=cinvar
Terminal=false
Categories=AudioVideo;Video;
D
chmod +x "$APP/cinvar.desktop" "$HERE/cinvar-launcher.sh"
D="$(xdg-user-dir DESKTOP 2>/dev/null || echo "$HOME/Desktop")"
[ -d "$D" ] && { cp "$APP/cinvar.desktop" "$D/CINVAR.desktop"; chmod +x "$D/CINVAR.desktop"; gio set "$D/CINVAR.desktop" metadata::trusted true 2>/dev/null || true; }
echo "Installed. Find CINVAR in your app menu or on the Desktop."
