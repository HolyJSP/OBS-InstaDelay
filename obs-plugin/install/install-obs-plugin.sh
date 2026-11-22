#!/usr/bin/env bash
# Install the Delay Bridge OBS plugin for the current user (macOS or Linux).
# Run from the folder that contains the plugin library and the delay-bridge binary.
set -euo pipefail
cd "$(dirname "$0")"

if [ "$(uname)" = "Darwin" ]; then
  LIB=obs-delay-bridge.dylib
  [ -f "$LIB" ] || { echo "Missing $LIB — run ./build.sh first and use dist/obs-plugin."; exit 1; }
  B="$HOME/Library/Application Support/obs-studio/plugins/obs-delay-bridge.plugin/Contents"
  mkdir -p "$B/MacOS"
  cp "$LIB" "$B/MacOS/obs-delay-bridge"
  cp delay-bridge "$B/MacOS/delay-bridge"
  chmod +x "$B/MacOS/delay-bridge"
  cat > "$B/Info.plist" <<'PLIST'
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
  <key>CFBundleExecutable</key><string>obs-delay-bridge</string>
  <key>CFBundleIdentifier</key><string>com.github.holyjsp.obs-delay-bridge</string>
  <key>CFBundleName</key><string>obs-delay-bridge</string>
  <key>CFBundlePackageType</key><string>BNDL</string>
  <key>CFBundleVersion</key><string>1.0.0</string>
</dict></plist>
PLIST
  xattr -dr com.apple.quarantine "$B/.." 2>/dev/null || true
  codesign --force --sign - "$B/MacOS/delay-bridge" "$B/MacOS/obs-delay-bridge" 2>/dev/null || true
  DEST="$B/MacOS"
else
  LIB=obs-delay-bridge.so
  [ -f "$LIB" ] || { echo "Missing $LIB — run ./build.sh first and use dist/obs-plugin."; exit 1; }
  install_to() {
    mkdir -p "$1"
    cp "$LIB" "$1/obs-delay-bridge.so"
    cp delay-bridge "$1/delay-bridge"
    chmod +x "$1/delay-bridge"
    echo "  installed to $1"
  }
  DEST="$HOME/.config/obs-studio/plugins/obs-delay-bridge/bin/64bit"
  install_to "$DEST"
  # Flatpak OBS keeps its config elsewhere.
  if [ -d "$HOME/.var/app/com.obsproject.Studio" ]; then
    install_to "$HOME/.var/app/com.obsproject.Studio/config/obs-studio/plugins/obs-delay-bridge/bin/64bit"
  fi
fi

echo
echo "  Delay Bridge OBS plugin installed ($DEST)."
echo "  Restart OBS. Then: Settings > Stream > Custom, Server rtmp://127.0.0.1:1935/live"
echo "  Hotkeys: OBS Settings > Hotkeys > search \"Delay Bridge\". Dashboard: Tools > Delay Bridge."
