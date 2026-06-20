#!/usr/bin/env sh
# Install stats-melee into the per-user XDG locations so it shows up in your
# application menu with its icon. No root needed. Override the install root
# with PREFIX=... (default: ~/.local).
set -eu

PREFIX="${PREFIX:-$HOME/.local}"
BIN_DIR="$PREFIX/bin"
APP_DIR="$PREFIX/share/applications"
ICON_DIR="$PREFIX/share/icons/hicolor/256x256/apps"

# Directory this script lives in (the extracted release folder).
SRC="$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)"

mkdir -p "$BIN_DIR" "$APP_DIR" "$ICON_DIR"

install -m 0755 "$SRC/stats-melee-app" "$BIN_DIR/stats-melee-app"
install -m 0644 "$SRC/stats-melee.png" "$ICON_DIR/stats-melee.png"

# Write the .desktop with an absolute Exec so it works regardless of whether
# ~/.local/bin is on PATH.
sed "s|@EXEC@|$BIN_DIR/stats-melee-app|g" "$SRC/stats-melee.desktop.in" \
  > "$APP_DIR/stats-melee.desktop"
chmod 0644 "$APP_DIR/stats-melee.desktop"

# Refresh the menu database / icon cache where the tools exist (best-effort).
update-desktop-database "$APP_DIR" 2>/dev/null || true
gtk-update-icon-cache "$PREFIX/share/icons/hicolor" 2>/dev/null || true

echo "Installed stats-melee to $PREFIX."
echo "It should appear in your application menu (you may need to log out/in)."
echo "Run 'sh uninstall.sh' from this folder to remove it."
