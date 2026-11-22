#!/usr/bin/env bash
# Build Delay Bridge on macOS or Linux.
#   ./build.sh        build the release binary into ./dist
#   ./build.sh test   run the unit and end-to-end tests (needs ffmpeg)
set -euo pipefail
cd "$(dirname "$0")"

echo
echo "  ==============================="
echo "   Building Delay Bridge"
echo "  ==============================="
echo

if ! command -v cargo >/dev/null 2>&1; then
  echo "  Rust is not installed yet. Install it with:"
  echo
  echo "    curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh"
  echo
  echo "  then open a new terminal and run ./build.sh again."
  exit 1
fi

if [ "${1:-}" = "test" ]; then
  echo "Running tests (the end-to-end tests need ffmpeg and take about 2 minutes)..."
  cargo test --release --workspace -- --test-threads=1 --nocapture
  echo
  echo "  All tests passed."
  exit 0
fi

echo "Compiling Delay Bridge and the OBS plugin (the first build takes a few minutes)..."
echo
cargo build --release --workspace

if [ "$(uname)" = "Darwin" ]; then SRC=libobs_delay_bridge.dylib; LIB=obs-delay-bridge.dylib
else SRC=libobs_delay_bridge.so; LIB=obs-delay-bridge.so; fi

rm -rf dist
mkdir -p dist/obs-plugin
cp target/release/delay-bridge README.md LICENSE config.example.json dist/
cp -R docs dist/docs
cp "target/release/$SRC" "dist/obs-plugin/$LIB"
cp target/release/delay-bridge obs-plugin/install/install-obs-plugin.sh dist/obs-plugin/
chmod +x dist/delay-bridge dist/obs-plugin/delay-bridge dist/obs-plugin/install-obs-plugin.sh

echo
echo "  Done! Built into $(pwd)/dist"
echo
echo "    obs-plugin/     OBS plugin: Delay Bridge starts with OBS, adds hotkeys and Tools > Delay Bridge (recommended)"
echo "    delay-bridge    standalone program, if you prefer to start it yourself"
echo
if [ -t 0 ]; then
  read -r -p "  Install the OBS plugin now? [y/N] " answer
  case "$answer" in
    [yY]*) ./dist/obs-plugin/install-obs-plugin.sh ;;
    *) echo "  Install later with ./dist/obs-plugin/install-obs-plugin.sh" ;;
  esac
fi
