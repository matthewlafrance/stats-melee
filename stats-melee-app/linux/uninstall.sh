#!/usr/bin/env sh
# Remove a stats-melee install done by install.sh. Honors the same PREFIX.
set -eu

PREFIX="${PREFIX:-$HOME/.local}"

rm -f "$PREFIX/bin/stats-melee-app"
rm -f "$PREFIX/share/applications/stats-melee.desktop"
rm -f "$PREFIX/share/icons/hicolor/256x256/apps/stats-melee.png"

update-desktop-database "$PREFIX/share/applications" 2>/dev/null || true
gtk-update-icon-cache "$PREFIX/share/icons/hicolor" 2>/dev/null || true

echo "Removed stats-melee from $PREFIX."
