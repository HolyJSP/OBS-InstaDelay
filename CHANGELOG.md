# Changelog

## 1.7.0

- **Type menu on the Control tab.** A dropdown right above the button picks how the delay is added: Rewind (instant replay), Gradual (plays slower, then catches up), Freeze (still picture), or Scene (your overlay scene, shown once OBS control is set up). It takes effect immediately and is saved, so hotkeys and Stream Deck use the same type.

## 1.6.0

- **Simpler dock.** The Control tab now has only what you need while live: the status, the delay length, when to turn it off, and one button (Turn on / Change / Remove delay). A one-line connection strip shows OBS and each platform, plus a message only when something needs fixing.
- The way the delay is added (Rewind, Gradual, Freeze, Scene) is chosen once in **Settings → How the delay is added** (top of Settings), instead of on every change. Hotkeys and Stream Deck use the same choice.
- Encoder/bitrate details, "Stream directly instead" and the activity log moved to **Settings → Connection details**.
- Fewer explanatory lines: steady states show just the state and the number; changes in progress show a short "About 2 min left" and a thin progress bar.

## 1.5.0

- **Rewind is the default.** Turning on the delay (dashboard, hotkeys, Stream Deck, auto-off) now replays the last seconds instantly unless you pick another mode. Settings from earlier versions that were still on the old default (Gradual) switch to Rewind once; any other choice you made is kept.
- **Dashboard buttons follow what you can do next:** one wide "Turn on" when the delay is off, one wide "Remove delay" when it's on, and "Change to …" plus "Remove" only when you pick a different length. Buttons show progress ("Rewinding…") until Delay Bridge answers.
- After a rewind the dock confirms it: "Replayed the last 30s for viewers."
- The Scene mode button only appears once OBS control (WebSocket) is turned on, instead of sitting greyed out.
- The "how far behind" track is hidden while there's no stream.

## 1.4.0

- **Redesigned dashboard, made for the OBS dock.** Everything you need while live fits in a normal-height dock without scrolling:
  - A coloured status strip you can read at a glance: green = live with no delay, amber = viewers are delayed, blue = a change is in progress.
  - A track showing how far behind you viewers are. It fills while a gradual change runs and jumps on a rewind; a marker shows the target.
  - Delay, turn-off time and mode are compact one-row selectors, with a single line explaining what the selected mode will do right now.
  - OBS details (encoder, bitrate, keyframes, dropped frames) moved into Connections; the OBS card only appears as a banner when OBS bypasses Delay Bridge.
  - Works from 280 px wide up to a full browser tab, keyboard focus is visible, and it respects "reduce motion".
- Mode buttons renamed to **Gradual**, **Rewind**, **Freeze** and **Scene**.
- Works in the older browser built into OBS 30 (no newer CSS features).

## 1.3.1

- **build.bat checks every step.** It used to hide copy errors, so if the old `dist` folder was in use (or the build failed and the message scrolled away) you could end up without the plugin folder and no explanation. It now stops with a clear message, saves compiler errors to `build.log`, and opens `dist\obs-plugin` in File Explorer when it's done.

## 1.3.0

- **Drop-in plugin folder.** The build (and the release zip) now contains a ready-made `obs-plugin/obs-delay-bridge` folder (`obs-delay-bridge.plugin` on macOS). Install by dragging it into OBS's plugins folder (Windows: `C:\ProgramData\obs-studio\plugins`), or run the installer, which does the same.
- **Windows: plugin moved to OBS's current plugin location** (`C:\ProgramData\obs-studio\plugins`). The old location (`Program Files\obs-studio\obs-plugins\64bit`) is flagged as legacy by OBS 33 and no longer loaded by OBS 34. The installer moves an existing install over, and only asks for admin rights when it has to remove the old copy.
- The folder works with both new (OBS 33+) and older OBS versions.
- `HOW-TO-INSTALL.txt` next to the plugin explains the manual install.

## 1.2.0

- **Rewind mode: instant delay with no freeze.** Turning on 30 s replays the last 30 s straight away, then the stream continues 30 s behind. No pause, no slow-motion, no waiting. Works for any amount up to the rewind memory; asking for more rewinds what's available and adds the rest gradually. Removing the delay jumps back to live.
- New setting **Rewind memory** (default 2 min, 0 = off). Uses about 45 MB per minute at 6000 kbps; no extra CPU.
- Dashboard: fourth mode button **Rewind**, showing how much can be replayed right now.

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
