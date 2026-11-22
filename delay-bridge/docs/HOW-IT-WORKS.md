# How the delay works

This is for the curious and for contributors. You don't need it to use Delay Bridge.

## The problem

OBS's built-in **Stream Delay** is applied to the encoded output when the stream *starts* and can't be changed while live. Changing it means stopping the stream: viewers drop, the VOD splits, and you lose momentum. Re-encoding a delayed copy of the video would cost GPU/CPU you'd rather keep for your game.

## The approach

Delay Bridge is a small RTMP server and client in one program:

```
OBS ──RTMP──▶ ingest server ──packets──▶ delay engine ──packets──▶ outputs ──RTMP/RTMPS──▶ platforms
```

1. **Ingest** (`src/rtmp/server.rs`): accepts OBS's publish, exactly like Twitch's ingest would, and hands each audio/video/metadata message to the engine. It's still OBS's already-encoded H.264/AAC; nothing is decoded.
2. **Engine** (`src/engine.rs`): a queue. Each packet is stamped with its arrival time and released when `arrival + delay ≤ now`.
3. **Outputs** (`src/output.rs`): one task per platform with its own RTMP/RTMPS connection. Each output batches everything that's ready into one socket write and reconnects on failure.

Because everything is copied by reference (`bytes::Bytes`), a packet is stored once no matter how many destinations there are.

## Changing the delay without breaking the stream

The platform must receive a stream whose timestamps keep increasing smoothly, and decoding must restart cleanly at a keyframe. The engine has two states:

- **normal**: release packets on schedule, rewriting timestamps as `ts_out = ts_in + offset`.
- **resync**: entered whenever the delay changes (or OBS reconnects). It waits for a keyframe that is *due* under the new delay, drops anything before it, recomputes `offset` so the keyframe lands right after the last packet sent, then goes back to normal.

### Increasing the delay (Frozen frame / Overlay modes)

From 0 s to 60 s, the packets that arrived just now aren't due for another 60 s, so the platform would get nothing. What happens next depends on how often OBS sends keyframes:

- **Keyframes at least every 3 s** (OBS set to a 1–2 s keyframe interval): the engine sends **filler**: the last keyframe sent (or, in overlay mode, a keyframe captured from your overlay scene), repeated at `fillerFps` with its composition offset zeroed so it decodes on its own, plus standard silent AAC-LC audio frames. When a keyframe becomes due under the new delay, output resumes there.
- **Keyframes rarer than that**, which is common when OBS's keyframe interval is on "auto" (x264 then uses about 8 s; some hardware encoders go much longer): waiting for the next keyframe could take a long time. Instead the engine **pauses the video** and sends silent audio only. When the packet right after the last one it sent becomes due, it **resumes exactly there**. The decoder never misses a frame, so no keyframe is needed, nothing is skipped, and the new delay is exact.

Filler timestamps follow the wall clock and start after the last frame's *display* time (B-frames display later than they decode), so frames never collide.

**Safety net:** if output is waiting for a keyframe that is more than 3 s overdue, it resumes anyway rather than leave viewers on a frozen screen, and logs a hint to fix the keyframe interval.

### Decreasing or removing the delay (Frozen frame / Overlay modes)

The engine never freezes the stream to do this. It keeps sending at the current pace, the old delay expressed as *slack*, and as soon as a keyframe is due under the new delay it **jumps ahead** to the newest such keyframe, dropping the seconds in between. If viewers are still more than 2.5 s behind after that jump (keyframes far apart), it jumps again at the following keyframe. With a 2 s keyframe interval, one jump lands within 2 s of live; with OBS's "auto" interval, it takes up to two keyframes (about 8–16 s with x264).

### Connecting to the platform

Players can only start decoding at a keyframe. Each destination therefore keeps everything since the most recent keyframe (up to 48 MB). When it connects or reconnects, it sends that group first, so viewers get a picture immediately instead of waiting for OBS's next keyframe.

### OBS reconnects mid-stream

A new OBS session restarts its timestamps at 0. Packets carry a session number, and the first packet of a new session puts the engine into resync, which picks the new session's first keyframe and recomputes the offset. To the platform, it's still one stream.

### Stopping while delayed

When OBS disconnects, the engine keeps releasing what's buffered. When the queue is empty it emits `Ended`, and each output sends `FCUnpublish`/`deleteStream` and closes, so the platform goes offline only after viewers have seen everything.

### Gradual mode (speed ramp)

The default mode never pauses or skips. Instead of jumping to the new delay, the engine changes the *playback rate* for a while:

- A ramp starts at input time `m0` with output time `o0` and speed `s` (0.7 when adding delay at the default 30%, 1.3 when removing). Every packet's output timestamp is `o0 + (m − m0) / s`, and it is released when the wall clock reaches that timestamp plus the fixed latency. Played at 0.7×, each real second of input takes 1.43 s to show, so viewers fall 0.3 s further behind per second of playback until the target is reached.
- **Video** is passed through untouched: only its timestamps (and the B-frame composition offset, scaled by `1/s`) change. The frames themselves are never decoded.
- **Audio** can't just be stretched by changing timestamps (that would leave gaps or overlaps between AAC frames), so during a ramp it is decoded (Symphonia), time-stretched with **WSOLA** (overlap-add with a small waveform-similarity search, which keeps the pitch the same), and re-encoded with **FDK AAC** at the original bitrate. The encoder is primed with the audio just before the ramp so there's no click at the switch.
- When the delay reaches its target the ramp stops at an exact input sample, the stretcher flushes, and the original AAC packets are passed through again with timestamps that line up with what was already sent. A/V sync stays within one audio frame (~21 ms).
- Duration = change ÷ ramp %. 30 s at 30% ≈ 100 s; at 10% ≈ 5 min; at 50% ≈ 1 min. A new change during a ramp starts a new ramp from wherever the delay currently is.

The CPU cost is only paid during a ramp (decoding + encoding one stereo AAC stream is a few % of one core); the rest of the time audio is passed through.

## Efficiency

- **Single thread** (Tokio `current_thread` runtime). Everything is I/O-bound, so one thread avoids synchronization and keeps memory small.
- **No polling.** The engine sleeps until the head of the queue is due, plus a 15 ms batching window, then releases everything due in one go. Outputs write each batch with a single `write` call. With no delay, packets are forwarded as they arrive.
- **Zero-copy** packet payloads shared between the queue and all outputs.
- **Back-pressure safety:** if an upload can't keep up, that destination's channel fills, and it skips ahead to the next keyframe instead of growing memory without bound. Other destinations are unaffected.
- **Dashboard:** polls once per second, only while visible, and updates only the parts of the page that changed.

## The OBS plugin

`obs-plugin/` is a small Rust library that OBS loads like any plugin (`obs_module_load` / `obs_module_unload`). To keep it buildable with plain `cargo build` and no OBS SDK, it doesn't link against libobs. Instead it looks up the handful of functions it needs (`obs_hotkey_register_frontend`, `obs_frontend_add_tools_menu_item`, `obs_module_get_config_path`, …) in the already-loaded OBS libraries at runtime (`GetProcAddress` on Windows, `dlsym` on macOS/Linux).

On load it:
1. starts `delay-bridge(.exe)` from the plugin's own folder, hidden, with its settings in OBS's `plugin_config/obs-delay-bridge/` folder;
2. registers four frontend hotkeys (their key bindings are saved with OBS's other settings);
3. adds **Tools → Delay Bridge**, which opens the dashboard.

Hotkeys simply call the local HTTP API on a background thread, so OBS is never blocked.

**The dock:** OBS keeps Custom Browser Docks as a JSON list in its config (`[BasicWindow] ExtraBrowserDocks`, in the user config on OBS 31+ and the global config before that) and creates them after plugins load. The plugin adds a `Delay Bridge` entry once and records that in `plugin-state.json`, so removing the dock is respected.

**Private link:** Delay Bridge's stdin and stdout are a JSON-lines channel to the plugin (`src/plugin_link.rs` ↔ `obs-plugin/src/link.rs`). Delay Bridge asks; the plugin answers:

| Request | Plugin does |
|---|---|
| `info` | Reads OBS's stream service (type, platform name, server, whether a key is set, whether it already points at Delay Bridge), whether OBS is streaming, and while live the output's frames, dropped frames and congestion, plus encoder id, bitrate and keyframe interval. Never includes the key. |
| `import` | Returns server and key so Delay Bridge can add the destination. |
| `use_bridge` | On OBS's UI thread: saves the current service settings, switches OBS to `rtmp_custom` → `rtmp://127.0.0.1:<port>/live`, persists it, and returns the original (with key) for Delay Bridge to store. |
| `restore` | On OBS's UI thread: recreates the original service from the stored settings. |

The dashboard asks for `info` only while it's open (about once a second; each call takes well under a millisecond). Twitch's `auto` server is reported as `rtmp://live.twitch.tv/app` rather than resolved through OBS, because that resolution can trigger a network lookup.

**Lifetime:** the plugin keeps Delay Bridge's stdin open. On a normal OBS exit it closes the pipe and Delay Bridge shuts down cleanly (closing platform connections properly). If OBS crashes or is killed, the operating system closes the pipe and Delay Bridge exits on its own, so it never lingers in the background.

## Protocol notes

- RTMP handshake: "simple" (version 3, no digest). This is accepted by OBS, ffmpeg, Twitch, YouTube and Kick.
- Outgoing chunk size 4096; incoming chunk size is whatever the peer sets.
- RTMPS uses rustls with the Mozilla root store (`webpki-roots`), so it doesn't depend on the OS certificate store.
- Enhanced RTMP (HEVC/AV1 FourCC) packets are recognized for keyframes and sequence headers and passed through untouched.
