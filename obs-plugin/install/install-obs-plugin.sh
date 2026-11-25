#!/usr/bin/env bash
# Install the Delay Bridge OBS plugin for the current user (macOS or Linux).
# Copies the ready-made plugin folder next to this script into OBS's plugin folder.
# (You can also copy that folder there yourself; see HOW-TO-INSTALL.txt.)
set -euo pipefail
cd "$(dirname "$0")"

copy_folder() { # <source folder> <plugins dir>
  mkdir -p "$2"
  rm -rf "$2/$(basename "$1")"
  cp -R "$1" "$2/"
  echo "  installed to $2/$(basename "$1")"
}

if [ "$(uname)" = "Darwin" ]; then
  SRC=obs-delay-bridge.plugin
  [ -d "$SRC" ] || { echo "Missing $SRC: run ./build.sh first and use dist/obs-plugin."; exit 1; }
  DEST="$HOME/Library/Application Support/obs-studio/plugins"
  copy_folder "$SRC" "$DEST"
  xattr -dr com.apple.quarantine "$DEST/$SRC" 2>/dev/null || true
else
  SRC=obs-delay-bridge
  [ -d "$SRC" ] || { echo "Missing $SRC: run ./build.sh first and use dist/obs-plugin."; exit 1; }
  DEST="$HOME/.config/obs-studio/plugins"
  copy_folder "$SRC" "$DEST"
  # Flatpak OBS keeps its config elsewhere.
  if [ -d "$HOME/.var/app/com.obsproject.Studio" ]; then
    copy_folder "$SRC" "$HOME/.var/app/com.obsproject.Studio/config/obs-studio/plugins"
  fi
fi

echo
echo "  Delay Bridge OBS plugin installed."
echo "  Restart OBS. Then use the Delay Bridge dock (or Tools > Delay Bridge) to finish setup."
echo "  Hotkeys: OBS Settings > Hotkeys > search \"Delay Bridge\"."
