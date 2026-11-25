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

if [ "$(uname)" = "Darwin" ]; then OS=macos; LIB=libobs_delay_bridge.dylib; DROP="obs-delay-bridge.plugin"; INTO="~/Library/Application Support/obs-studio/plugins"
else OS=linux; LIB=libobs_delay_bridge.so; DROP="obs-delay-bridge/"; INTO="~/.config/obs-studio/plugins"; fi
VERSION=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)

rm -rf dist
mkdir -p dist/obs-plugin
cp target/release/delay-bridge README.md LICENSE config.example.json dist/
cp -R docs dist/docs
chmod +x dist/delay-bridge
# Drop-in plugin folder (copy it into OBS's plugins folder), plus an installer that does that.
bash obs-plugin/install/make-plugin-folder.sh "$OS" target/release/delay-bridge "target/release/$LIB" dist/obs-plugin "$VERSION" >/dev/null
cp obs-plugin/install/install-obs-plugin.sh obs-plugin/install/HOW-TO-INSTALL.txt dist/obs-plugin/
chmod +x dist/obs-plugin/install-obs-plugin.sh

echo
echo "  Done! Built into $(pwd)/dist"
echo
echo "    obs-plugin/$DROP   the OBS plugin, ready to drop into $INTO"
echo "    obs-plugin/install-obs-plugin.sh   or let this copy it there for you"
echo "    delay-bridge    standalone program, if you prefer to start it yourself"
echo
if [ -t 0 ]; then
  read -r -p "  Install the OBS plugin now? [y/N] " answer
  case "$answer" in
    [yY]*) ./dist/obs-plugin/install-obs-plugin.sh ;;
    *) echo "  Install later with ./dist/obs-plugin/install-obs-plugin.sh" ;;
  esac
fi
