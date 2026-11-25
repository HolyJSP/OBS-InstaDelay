#!/usr/bin/env bash
# Assemble the drop-in OBS plugin folder.
#
#   make-plugin-folder.sh <windows|linux|macos> <delay-bridge binary> <plugin library> <out dir> [version]
#
# Creates, inside <out dir>, a folder that can be copied straight into OBS's plugin folder:
#
#   windows  obs-delay-bridge/                 -> C:\ProgramData\obs-studio\plugins\
#              obs-delay-bridge.dll              OBS 33 and newer
#              delay-bridge.exe
#              bin/64bit/obs-delay-bridge.dll    OBS 32 and older (finds delay-bridge.exe two levels up)
#              data/
#   linux    obs-delay-bridge/                 -> ~/.config/obs-studio/plugins/
#              bin/64bit/obs-delay-bridge.so
#              bin/64bit/delay-bridge
#              data/
#   macos    obs-delay-bridge.plugin/          -> ~/Library/Application Support/obs-studio/plugins/
#              Contents/MacOS/obs-delay-bridge, Contents/MacOS/delay-bridge, Contents/Info.plist
set -euo pipefail

os=$1 bridge=$2 lib=$3 out=$4 version=${5:-1.0.0}
mkdir -p "$out"

case "$os" in
  windows)
    d="$out/obs-delay-bridge"
    rm -rf "$d"
    mkdir -p "$d/bin/64bit" "$d/data"
    cp "$lib" "$d/obs-delay-bridge.dll"
    cp "$lib" "$d/bin/64bit/obs-delay-bridge.dll"
    cp "$bridge" "$d/delay-bridge.exe"
    ;;
  linux)
    d="$out/obs-delay-bridge"
    rm -rf "$d"
    mkdir -p "$d/bin/64bit" "$d/data"
    cp "$lib" "$d/bin/64bit/obs-delay-bridge.so"
    cp "$bridge" "$d/bin/64bit/delay-bridge"
    chmod +x "$d/bin/64bit/delay-bridge"
    ;;
  macos)
    d="$out/obs-delay-bridge.plugin"
    rm -rf "$d"
    mkdir -p "$d/Contents/MacOS" "$d/Contents/Resources"
    cp "$lib" "$d/Contents/MacOS/obs-delay-bridge"
    cp "$bridge" "$d/Contents/MacOS/delay-bridge"
    chmod +x "$d/Contents/MacOS/delay-bridge"
    cat > "$d/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
  <key>CFBundleExecutable</key><string>obs-delay-bridge</string>
  <key>CFBundleIdentifier</key><string>com.github.holyjsp.obs-delay-bridge</string>
  <key>CFBundleName</key><string>obs-delay-bridge</string>
  <key>CFBundlePackageType</key><string>BNDL</string>
  <key>CFBundleVersion</key><string>$version</string>
  <key>CFBundleShortVersionString</key><string>$version</string>
</dict></plist>
PLIST
    if command -v codesign >/dev/null 2>&1; then
      codesign --force --sign - "$d/Contents/MacOS/delay-bridge" "$d/Contents/MacOS/obs-delay-bridge" 2>/dev/null || true
    fi
    ;;
  *)
    echo "unknown OS: $os" >&2
    exit 1
    ;;
esac
echo "  plugin folder: $d"
