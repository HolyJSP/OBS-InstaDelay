# Changelog

## 1.1.0

- **Gradual delay (new default).** Adding delay no longer freezes the stream: playback runs slower (30% by default) until the full delay is built up, then returns to normal speed. Removing delay works the same way, playing faster until you're back to live. Viewers see no pause, no jump and no freeze.
- Audio during a ramp is time-stretched with pitch preserved, so voices don't sound lower or higher. Outside a ramp the audio is passed through untouched.
- Ramp speed is configurable in Settings (5%, 10%, 20%, 30% or 50%). Faster ramps are quicker but more noticeable.
- Dashboard shows ramp progress ("1.8s → 3s, ~12s left") and a mode picker: Gradually / Frozen frame / Overlay. Frozen frame and Overlay still change the delay instantly.
- Auto-off timer uses your default mode, so a gradual delay also comes off gradually.
- Linux release builds now target glibc instead of musl.
- Third-party notices: Fraunhofer FDK AAC (see `licenses/FDK-AAC-NOTICE.txt`) and Symphonia (MPL-2.0).

## 1.0.1

- **Fix: no video/audio after turning the delay on** when OBS's keyframe interval is on "auto" (common with NVIDIA/AMD/Intel encoders). When keyframes are rare, adding delay now pauses the video and resumes exactly where it left off. No keyframe needed, nothing skipped.
- **Fix: platform could take a long time to show a picture** after connecting or reconnecting. Each destination now starts from the latest buffered keyframe.
- Removing or reducing delay never freezes: it keeps streaming and jumps ahead at the next keyframe, a second time if needed to get close to live.
- Safety net: if a keyframe is more than 3 s overdue, output resumes anyway.
- Filler frames start after the last frame's display time, so frames never collide with B-frames.
- Dashboard: shows "video paused" and "removing delay, waiting for keyframe" states; warns when OBS's keyframes are far apart.
- New end-to-end test that streams with a single keyframe to reproduce the bug.

## 1.0.0

First release.

- Add, change or remove stream delay while live, with no reconnect.
- Auto-off timer, extend, or keep on.
- Frozen-frame or overlay-scene filler while the delay builds up.
- **OBS plugin**: starts Delay Bridge with OBS, adds native OBS hotkeys and Tools → Delay Bridge; shuts Delay Bridge down with OBS (even after a crash). Installers for Windows, macOS and Linux.
- Built-in **Delay Bridge dock** in OBS (added automatically by the plugin).
- **One-click setup from OBS:** copies your platform server and stream key from OBS and points OBS at Delay Bridge; "Stream directly" restores the original setting. Import-only option in Settings.
- Live OBS info in the dashboard: routing status, encoder, bitrate, keyframe interval (with a warning), dropped frames.
- Dashboard opens automatically on start (standalone) or during first-time setup (plugin).
- OBS hotkey toggle via obs-websocket (standalone), Stream Deck and HTTP API.
- Multistreaming to any number of RTMP/RTMPS destinations, with automatic reconnect.
- Dashboard (usable as an OBS dock) and optional viewer badge overlay.
- Single native executable for Windows, macOS and Linux (~2.5 MB, under 1% CPU).
