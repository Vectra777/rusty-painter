#!/bin/sh
# Installs Rusty Painter for this user: the app, its menu entry and its icon
# (which is how Wayland desktops find the taskbar icon). Run from the unpacked
# release folder; run again to update. Remove with `./install.sh --uninstall`.
set -eu
cd "$(dirname "$0")"
BIN="$HOME/.local/bin/rusty-painter"
APPS="${XDG_DATA_HOME:-$HOME/.local/share}/applications"
ICONS="${XDG_DATA_HOME:-$HOME/.local/share}/icons/hicolor/256x256/apps"
if [ "${1:-}" = "--uninstall" ]; then
  rm -f "$BIN" "$APPS/io.vectra.rustypainter.desktop" "$ICONS/io.vectra.rustypainter.png"
  echo "Removed."
  exit
fi
mkdir -p "$(dirname "$BIN")" "$APPS" "$ICONS"
install -m 755 rusty-painter "$BIN"
install -m 644 rusty-painter.png "$ICONS/io.vectra.rustypainter.png"
# The full path, in case ~/.local/bin isn't on the menu's PATH.
sed "s|^Exec=rusty-painter|Exec=$BIN|" io.vectra.rustypainter.desktop > "$APPS/io.vectra.rustypainter.desktop"
update-desktop-database "$APPS" 2>/dev/null || true
gtk-update-icon-cache -q "${ICONS%/256x256/apps}" 2>/dev/null || true
echo "Installed: Rusty Painter is in the applications menu."
