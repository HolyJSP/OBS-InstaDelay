# Contributing

Thanks for helping! Bug reports, docs fixes and pull requests are all welcome.

## Reporting a bug

Please include your OS, OBS (or Streamlabs) version, platform, and the Delay Bridge console log. **Remove your stream key** if it appears anywhere.

## Development setup

1. Install [Rust](https://rustup.rs) (stable) and [ffmpeg](https://ffmpeg.org/download.html) (used by the end-to-end tests).
2. Clone and build. `build.bat` / `./build.sh` produce a release build in `dist/`; for development use Cargo directly:
   ```bash
   cargo build
   cargo run -- ./dev-config.json      # uses a separate config file
   ```
3. Point OBS (or ffmpeg) at `rtmp://127.0.0.1:1935/live`. A handy test source:
   ```bash
   ffmpeg -re -f lavfi -i testsrc2=size=1280x720:rate=30 -f lavfi -i sine \
     -c:v libx264 -g 60 -c:a aac -f flv rtmp://127.0.0.1:1935/live/test
   ```

## Before opening a PR

```bash
cargo fmt --all
cargo clippy --release --workspace --all-targets -- -D warnings
cargo test --release --workspace -- --test-threads=1 --nocapture
```

The end-to-end tests stream through the real bridge into a local RTMP sink, change the delay while live, and check that:
- the destination saw exactly one connection,
- timestamps never go backwards, have no gaps over 600 ms, and stay in step with the wall clock,
- the output decodes cleanly with ffmpeg,
- outputs reconnect on their own and the delayed tail is still sent after OBS stops.

CI runs all of this on Windows, macOS and Linux.

## Working on the OBS plugin

The plugin lives in `obs-plugin/` (same Cargo workspace). Build it with `cargo build --release -p obs-delay-bridge`, then run `./build.sh` or `build.bat` to get an installable `dist/obs-plugin/` folder. It needs OBS 30+ to test. Add `--verbose` to OBS's command line and look for `[delay-bridge]` lines in its log.

Because the plugin resolves OBS functions at runtime, any new OBS function you use must be added to the `Api` struct with its exact C signature.

## Guidelines

- **Performance matters.** This runs next to games. Avoid polling, per-packet allocations in hot paths, and extra threads.
- **Keep it dependency-light.** Ask in an issue before adding a crate.
- **The dashboard** (`public/index.html`) is plain HTML/JS embedded into the binary with `include_str!`. No build step, no frameworks. Only touch the DOM when values change.
- **Don't break the JSON API.** Stream Deck and Companion setups depend on it.

## Releasing (maintainers)

1. Bump `version` in `Cargo.toml` and add an entry to `CHANGELOG.md`.
2. `git tag v1.2.3 && git push --tags`. The Release workflow builds Windows, macOS and Linux zips and publishes the GitHub Release.
