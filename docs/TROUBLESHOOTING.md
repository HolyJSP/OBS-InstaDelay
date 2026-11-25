# Troubleshooting

Start with the log. It records every connection, error and delay change:
- **Standalone program:** the Delay Bridge console window.
- **OBS plugin:** `delay-bridge.log` in OBS's plugin settings folder. On Windows, paste `%APPDATA%\obs-studio\plugin_config\obs-delay-bridge` into Explorer's address bar. OBS's own log (**Help → Log Files → View Current Log**) also shows lines starting with `[delay-bridge]`.

The dashboard's **Connections** card and **Activity** log show the same information.

## build.bat finished but there's no plugin folder

The plugin folder is `dist\obs-plugin\obs-delay-bridge` inside the source folder (build.bat opens `dist\obs-plugin` in File Explorer when it's done). If it isn't there, scroll up in the build window for the message:

- **"Build failed"**: the errors are saved in `build.log` next to `build.bat`. The usual causes are missing Visual Studio Build Tools ("Desktop development with C++") or an old Rust (`rustup update`). Attach `build.log` if you open an issue.
- **"Can't clear the old dist folder" / "Delay Bridge is still running"**: close OBS, any Delay Bridge window and any Explorer window showing `dist`, then run build.bat again.

## The OBS plugin doesn't show up (no "Delay Bridge" in the Tools menu)

- OBS must be **30 or newer** (Help → About).
- Windows: run `install-obs-plugin.bat` again with OBS closed. You should have `C:\ProgramData\obs-studio\plugins\obs-delay-bridge\obs-delay-bridge.dll` and `delay-bridge.exe` in that same folder. A common mistake when copying by hand is one folder too many (`plugins\obs-delay-bridge\obs-delay-bridge\...`) or copying only the DLL.
- Portable OBS ignores ProgramData: put the `obs-delay-bridge` folder in your portable OBS's `plugins` folder.
- macOS: `~/Library/Application Support/obs-studio/plugins/obs-delay-bridge.plugin` must exist. Linux: `~/.config/obs-studio/plugins/obs-delay-bridge/bin/64bit/obs-delay-bridge.so`. Run `./install-obs-plugin.sh` again and restart OBS.
- Check OBS's log (**Help → Log Files → View Current Log**) for `obs-delay-bridge`.

## No "Delay Bridge" dock in OBS

- Check **Docks → Delay Bridge**. It may just be hidden.
- If you removed it, the plugin won't re-add it. Add it by hand: **Docks → Custom Browser Docks**, Name `Delay Bridge`, URL `http://127.0.0.1:8080/`.
- Some Linux OBS packages are built without browser support, so there are no browser docks at all. Use **Tools → Delay Bridge** (opens in your browser) instead.

## "Use Delay Bridge with my … settings" doesn't work

The message under the button says why:
- *Set your stream key in OBS first:* OBS → Settings → Stream has no key. Add it there, or type it in the dock's **Settings** tab.
- *stream type … isn't supported:* WHIP/WebRTC services can't be relayed. Choose an RTMP/RTMPS server for your platform in OBS.
- The change applies the next time you start streaming. A stream that's already live keeps going as it was.

## The plugin loads, but the dashboard says Delay Bridge isn't running

- In OBS's log, look for `[delay-bridge] starting …`. If it says `not found next to the plugin`, `delay-bridge.exe` wasn't copied next to `obs-delay-bridge.dll`; reinstall.
- If `delay-bridge.log` says `Could not listen for OBS on 127.0.0.1:1935`, a standalone copy (or another app) is already using the port. Close it; the plugin starts its own.

## OBS won't connect / "Failed to connect to server"

- Is Delay Bridge running? The console must say `Delay Bridge … is running`.
- Check OBS **Settings → Stream**: Service **Custom…**, Server exactly `rtmp://127.0.0.1:1935/live`.
- If you set `ingest.streamKey`, OBS's Stream Key must match it exactly. The console shows `Rejected an encoder: wrong stream key`.
- Another app on port 1935? See [Port already in use](#could-not-listen-for-obs--port-already-in-use).

## "Could not listen for OBS… Address already in use"

Something else already uses port 1935 (another copy of Delay Bridge, a local RTMP server, some capture tools).
- Close the other copy, **or**
- Edit `config.json` → `"ingest": { "port": 1936 }`, restart, and use `rtmp://127.0.0.1:1936/live` in OBS.

Same for the dashboard: change `web.port` (default 8080).

## The platform shows red / "reconnecting…"

The error under the destination says why:

| Error | Fix |
|---|---|
| `No stream key — add it in Settings` | Paste your key in **Settings** and save. |
| `Connection refused`, `failed to lookup address` | Check the Server URL for typos and your internet connection. |
| `Publish failed…` / closed right after connecting | The stream key is wrong or was reset. Copy it again from your platform. |
| `Timed out waiting for the server…` | A firewall or VPN may be blocking port 1935 (RTMP). Try the `rtmps://` URL your platform offers (port 443). Twitch: `rtmps://live.twitch.tv:443/app`. |

Delay Bridge retries automatically (1, 2, 4… up to 30 s apart).

## My stream key changes didn't apply

Destination changes made **while you're live** take effect when the stream ends, so the current broadcast isn't cut. Stop and start streaming in OBS to apply them right away.

## No video or audio after turning the delay on (fixed in 1.0.1)

Version 1.0.0 waited for a keyframe from OBS before resuming after the delay. With OBS's keyframe interval on "auto", especially with NVIDIA, AMD or Intel hardware encoders, that keyframe could take a very long time, so viewers saw a frozen frame with no sound. It could also take a long time for the platform to show any picture after connecting. Since 1.0.1:
- when keyframes are rare, the video pauses and then continues exactly where it stopped;
- each destination starts from the latest keyframe it buffered;
- removing the delay keeps streaming until it can jump ahead.

Update to the latest release. Setting OBS → Settings → Output (Advanced) → **Keyframe Interval: 2 s** is still recommended: it's what Twitch, YouTube and Kick expect, and it makes removing the delay snap back within 2 s.

## The delay change was off by a few seconds

Changes snap to keyframes. In OBS → Settings → Output, switch Output Mode to **Advanced** and set **Keyframe Interval** to **2** (or 1). See the [OBS guide](OBS-SETUP.md#output-keyframe-interval-important). With `0 (auto)` or in Simple mode, x264 can go 8 s between keyframes. The plugin's dock shows the actual value while you're live.

## Overlay mode shows a frozen frame instead of my scene

The log says why, for example `Overlay mode failed (…)`:
- `not connected to OBS WebSocket`: enable it in OBS (**Tools → WebSocket Server Settings**) and in **Settings → OBS control**, with the right password.
- `no overlay scene chosen`: pick one under **Settings → Overlay scene**.
- `no keyframe received`: set the keyframe interval to 1–2 s.
- The scene name must match exactly (it's case-sensitive). Use **Load scenes** to pick it.

## The OBS hotkey does nothing

**Plugin hotkeys** (*Delay Bridge: Toggle delay* …):
- Check the key is set in **OBS → Settings → Hotkeys** (filter: "Delay Bridge").
- If nothing happens, OBS's log shows `Delay Bridge is not reachable…`. See the section above.

**"Delay Toggle" source method** (standalone program):
- The dashboard must show **OBS control (WebSocket): connected**.
- The source name must exactly match **Settings → Hotkey source name** (default `Delay Toggle`).
- The source must be in the scene that's live when you press the key, and the key must be bound to **both** *Show 'Delay Toggle'* and *Hide 'Delay Toggle'* for that scene.
- Watch the log: each press prints `Hotkey: "Delay Toggle" toggled`.

## "OBS rejected the WebSocket password"

Copy the password again from OBS → **Tools → WebSocket Server Settings → Show Connect Info**, paste it in **Settings → OBS control**, and save.

## Stream Deck button does nothing

- Use the **Website** action with **GET request in background** ticked, otherwise it opens a browser tab.
- The URL must start with `http://127.0.0.1:8080/api/`.
- Test the URL in a browser: you should get a block of JSON text back.

## Viewers saw a long frozen screen when I stopped streaming

That's the delay draining: if you stop OBS with a 60 s delay on, Delay Bridge still sends the last 60 s so viewers see the end. To end immediately, click **End now** on the dashboard (or POST to `/api/end-now`). Or remove the delay before you stop.

## High memory use

Memory ≈ bitrate × delay. A 10-minute delay at 8 Mbps holds about 600 MB of video. Lower `delay.maxSeconds` if you want a hard cap.

## Windows SmartScreen / antivirus warning

The exe isn't code-signed, so Windows doesn't recognize it yet. Choose **More info → Run anyway**, or build from source. Every release is built by GitHub Actions from the code in this repository. You can check the workflow run linked from each release.

## Still stuck?

[Open an issue](https://github.com/HolyJSP/delay-bridge/issues) and include:
- your OS and OBS version,
- the console log (remove your stream key if it appears anywhere!),
- your platform and the Server URL you use (not the key).

## Gradual delay takes a long time

That's by design: at the default 30% it takes about 3.3 s of streaming per second of delay (30 s of delay ≈ 100 seconds). The dashboard shows the progress and how long is left. To go faster, raise **Settings → Gradual speed** to 50% (clearly slow-motion), or set the dock's **Type** menu to **Rewind** (instant).

## Audio sounds slightly "watery" while delay is being added or removed

During a gradual change the audio is time-stretched so it keeps its pitch. Music with strong transients can sound a little smeared at high ramp speeds. Lower **Settings → Gradual speed** (10–20%) and it becomes very hard to hear. Outside a ramp the audio is untouched.
