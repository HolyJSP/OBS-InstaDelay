//! End-to-end tests: ffmpeg -> Delay Bridge -> local RTMP sink. Requires `ffmpeg` on PATH
//! (tests are skipped if it's missing). Run with: cargo test --release -- --nocapture --test-threads=1

use delay_bridge::amf0;
use delay_bridge::app::App;
use delay_bridge::config::{Config, Destination};
use delay_bridge::engine::EngineMsg;
use delay_bridge::media::Kind;
use delay_bridge::rtmp::server::{self, IngestCtx};
use std::io::Write;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::net::TcpListener;
use tokio::sync::mpsc;

fn have_ffmpeg() -> bool {
    Command::new("ffmpeg")
        .arg("-version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok()
}

#[derive(Clone, Debug)]
struct Rx {
    wall: Instant,
    ts: u32,
    kind: Kind,
}

struct Sink {
    packets: Arc<Mutex<Vec<Rx>>>,
    publishes: Arc<Mutex<u32>>,
    ends: Arc<Mutex<u32>>,
    active: Arc<AtomicBool>,
    flv: Arc<Mutex<Option<std::fs::File>>>,
}

/// A local RTMP server (our own ingest code) that records what the bridge sends it.
async fn start_sink(port: u16, flv_path: Option<&str>) -> Sink {
    let (tx, mut rx) = mpsc::channel(8192);
    let active = Arc::new(AtomicBool::new(false));
    let ctx = IngestCtx {
        stream_key: Arc::new(Mutex::new("sinkkey".into())),
        active: active.clone(),
        tx,
        log: Arc::new(|_m: String| {}),
    };
    let l = TcpListener::bind(("127.0.0.1", port)).await.unwrap();
    tokio::spawn(server::serve(l, ctx));
    let flv = flv_path.map(|p| {
        let mut f = std::fs::File::create(p).unwrap();
        f.write_all(&[0x46, 0x4c, 0x56, 1, 5, 0, 0, 0, 9, 0, 0, 0, 0])
            .unwrap();
        f
    });
    let s = Sink {
        packets: Arc::new(Mutex::new(Vec::new())),
        publishes: Arc::new(Mutex::new(0)),
        ends: Arc::new(Mutex::new(0)),
        active,
        flv: Arc::new(Mutex::new(flv)),
    };
    let (packets, publishes, ends, flv) = (
        s.packets.clone(),
        s.publishes.clone(),
        s.ends.clone(),
        s.flv.clone(),
    );
    tokio::spawn(async move {
        while let Some(m) = rx.recv().await {
            match m {
                EngineMsg::SourceStart => *publishes.lock().unwrap() += 1,
                EngineMsg::SourceEnd => *ends.lock().unwrap() += 1,
                EngineMsg::Packet(p) => {
                    if let Some(f) = flv.lock().unwrap().as_mut() {
                        let (t, data) = match p.kind {
                            Kind::Audio => (8u8, p.data.to_vec()),
                            Kind::Video => (9, p.data.to_vec()),
                            Kind::Data => {
                                let vals = amf0::decode(&p.data).unwrap();
                                (18, amf0::encode(&vals[1..]))
                            }
                        };
                        let mut h = vec![t];
                        h.extend_from_slice(&(data.len() as u32).to_be_bytes()[1..]);
                        h.extend_from_slice(&(p.ts & 0xffffff).to_be_bytes()[1..]);
                        h.push((p.ts >> 24) as u8);
                        h.extend_from_slice(&[0, 0, 0]);
                        f.write_all(&h).unwrap();
                        f.write_all(&data).unwrap();
                        f.write_all(&((11 + data.len()) as u32).to_be_bytes())
                            .unwrap();
                    }
                    packets.lock().unwrap().push(Rx {
                        wall: Instant::now(),
                        ts: p.ts,
                        kind: p.kind,
                    });
                }
                _ => {}
            }
        }
    });
    s
}

fn start_app(dir: &std::path::Path, ingest_port: u16, sink_port: u16, key: &str) -> Arc<App> {
    start_app_with(dir, ingest_port, sink_port, key, |_| {})
}

fn start_app_with(
    dir: &std::path::Path,
    ingest_port: u16,
    sink_port: u16,
    key: &str,
    tweak: impl FnOnce(&mut Config),
) -> Arc<App> {
    let mut cfg = Config::default();
    // Most tests exercise the instant switch; the gradual test opts in explicitly.
    cfg.delay.default_mode = "instant".into();
    tweak(&mut cfg);
    cfg.ingest.port = ingest_port;
    cfg.ingest.stream_key = key.into();
    cfg.destinations = vec![Destination {
        name: "Sink".into(),
        url: format!("rtmp://127.0.0.1:{sink_port}/app"),
        key: "sinkkey".into(),
        enabled: true,
    }];
    App::start(cfg, dir.join("config.json"), None)
}

async fn start_ingest(app: &Arc<App>, port: u16) {
    let l = TcpListener::bind(("127.0.0.1", port)).await.unwrap();
    let ctx = IngestCtx {
        stream_key: app.ingest_key.clone(),
        active: app.ingest_active.clone(),
        tx: app.engine_tx.clone(),
        log: app.logger(),
    };
    tokio::spawn(server::serve(l, ctx));
}

/// `gop` = frames between keyframes; 0 = a single keyframe at the start and never again
/// (like a hardware encoder with OBS's keyframe interval on "auto").
fn ffmpeg_publish(url: &str, secs: u32, gop: u32) -> Child {
    let keyint = if gop == 0 {
        "keyint=infinite:scenecut=0".to_string()
    } else {
        format!("keyint={gop}")
    };
    Command::new("ffmpeg")
        .args([
            "-hide_banner",
            "-loglevel",
            "error",
            "-re",
            "-f",
            "lavfi",
            "-i",
            "testsrc2=size=640x360:rate=30",
            "-f",
            "lavfi",
            "-i",
            "sine=frequency=440:sample_rate=48000",
            "-c:v",
            "libx264",
            "-preset",
            "veryfast",
            "-x264-params",
            &keyint,
            "-bf",
            "2",
            "-b:v",
            "1500k",
            "-c:a",
            "aac",
            "-ac",
            "2",
            "-b:a",
            "128k",
            "-t",
            &secs.to_string(),
            "-f",
            "flv",
            url,
        ])
        .stdin(Stdio::null())
        .spawn()
        .expect("spawn ffmpeg")
}

async fn wait_child(c: &mut Child) {
    while c.try_wait().unwrap().is_none() {
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

async fn lag(app: &Arc<App>) -> Option<u64> {
    app.engine_status().await.viewer_lag_ms
}

#[tokio::test(flavor = "current_thread")]
async fn delay_changes_while_live() {
    if !have_ffmpeg() {
        eprintln!("ffmpeg not found — skipping");
        return;
    }
    let dir = std::env::temp_dir().join(format!("db-e2e-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let flv_path = dir.join("out.flv");
    let sink = start_sink(29350, Some(flv_path.to_str().unwrap())).await;
    let app = start_app(&dir, 29351, 29350, "testkey");
    start_ingest(&app, 29351).await;

    let t0 = Instant::now();
    let mut ff = ffmpeg_publish("rtmp://127.0.0.1:29351/live/testkey", 52, 30);
    let at = |s: u64| async move {
        let target = t0 + Duration::from_secs(s);
        let now = Instant::now();
        if target > now {
            tokio::time::sleep(target - now).await;
        }
    };
    let mut checks: Vec<(String, bool)> = Vec::new();

    at(5).await;
    app.enable(Some(10.0), None, Some("instant".into()))
        .await
        .unwrap();
    at(9).await;
    checks.push((
        "switching while ramping up".into(),
        app.engine_status().await.switching,
    ));
    at(20).await;
    let l = lag(&app).await.unwrap_or(0);
    checks.push((
        format!("~10s delay after ramp (got {l}ms)"),
        (9000..12500).contains(&l),
    ));
    app.enable(Some(4.0), None, Some("instant".into()))
        .await
        .unwrap();
    at(23).await;
    let l = lag(&app).await.unwrap_or(0);
    checks.push((
        format!("~4s delay right after reduce (got {l}ms)"),
        (3000..6500).contains(&l),
    ));
    app.enable(Some(12.0), Some(0.25), Some("instant".into()))
        .await
        .unwrap(); // 15 s auto-off timer
    at(37).await;
    let l = lag(&app).await.unwrap_or(0);
    checks.push((
        format!("~12s delay after second ramp (got {l}ms)"),
        (11000..14500).contains(&l),
    ));
    at(42).await;
    let st = app.engine_status().await;
    let l = st.viewer_lag_ms.unwrap_or(99999);
    checks.push((
        format!(
            "auto-off timer removed the delay (delay={} lag={l}ms)",
            st.delay_ms
        ),
        st.delay_ms == 0 && l < 2500,
    ));

    wait_child(&mut ff).await;
    tokio::time::sleep(Duration::from_secs(3)).await;
    drop(sink.flv.lock().unwrap().take());

    let pk = sink.packets.lock().unwrap().clone();
    let mut backwards = 0;
    for k in [Kind::Audio, Kind::Video] {
        let v: Vec<_> = pk.iter().filter(|p| p.kind == k).collect();
        backwards += v.windows(2).filter(|w| w[1].ts < w[0].ts).count();
    }
    let v: Vec<_> = pk.iter().filter(|p| p.kind == Kind::Video).collect();
    let max_gap = v
        .windows(2)
        .map(|w| w[1].ts as i64 - w[0].ts as i64)
        .max()
        .unwrap_or(0);
    let drift = (v.last().unwrap().ts as i64 - v[0].ts as i64)
        - v.last().unwrap().wall.duration_since(v[0].wall).as_millis() as i64;
    checks.push((
        "sink saw exactly one publish (no reconnects)".into(),
        *sink.publishes.lock().unwrap() == 1,
    ));
    checks.push(("timestamps never go backwards".into(), backwards == 0));
    checks.push((
        format!("no timestamp gap > 600ms (max {max_gap})"),
        max_gap <= 600,
    ));
    checks.push((
        format!("output timeline tracks wall clock (drift {drift}ms; a new connection starts with up to one keyframe interval of buffered video)"),
        drift.abs() < 1700,
    ));
    checks.push((
        "destination closed after stream ended".into(),
        *sink.ends.lock().unwrap() == 1,
    ));

    let dec = Command::new("ffmpeg")
        .args([
            "-hide_banner",
            "-v",
            "error",
            "-i",
            flv_path.to_str().unwrap(),
            "-f",
            "null",
            "-",
        ])
        .output()
        .unwrap();
    // Ignore ffmpeg's null-muxer notes about rounding ms timestamps onto a 30 fps grid; count real
    // decoder errors only.
    let errs = String::from_utf8_lossy(&dec.stderr)
        .lines()
        .filter(|l| !l.contains("non monotonically increasing dts to muxer"))
        .count();
    checks.push((
        format!("output decodes cleanly with ffmpeg ({errs} messages)"),
        dec.status.success() && errs == 0,
    ));

    let mut ok = true;
    for (n, p) in &checks {
        println!("{}  {n}", if *p { "PASS" } else { "FAIL" });
        ok &= *p;
    }
    assert!(ok, "some checks failed");
}

#[tokio::test(flavor = "current_thread")]
async fn reconnect_and_drain() {
    if !have_ffmpeg() {
        eprintln!("ffmpeg not found — skipping");
        return;
    }
    let dir = std::env::temp_dir().join(format!("db-drain-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();

    // Start streaming while the destination is unreachable; it comes up 4 s later.
    // The output must keep retrying and then go live on its own.
    let app = start_app(&dir, 29361, 29360, "");
    start_ingest(&app, 29361).await;
    let mut ff = ffmpeg_publish("rtmp://127.0.0.1:29361/live/anything", 20, 60);
    tokio::time::sleep(Duration::from_secs(4)).await;
    let sink = start_sink(29360, None).await;
    tokio::time::sleep(Duration::from_secs(7)).await; // retry backoff is 1, 2, 4 s…
    let got = sink.packets.lock().unwrap().len();
    println!("packets after destination came up: {got}");
    assert!(
        *sink.publishes.lock().unwrap() == 1 && got > 100,
        "output should reconnect once the destination is reachable"
    );

    app.enable(Some(8.0), None, Some("instant".into()))
        .await
        .unwrap();
    wait_child(&mut ff).await;
    let stopped = Instant::now();
    tokio::time::sleep(Duration::from_secs(12)).await;
    let last = sink.packets.lock().unwrap().last().map(|p| p.wall).unwrap();
    let tail = last.duration_since(stopped).as_secs_f64();
    let ends = *sink.ends.lock().unwrap();
    println!("tail after OBS stopped: {tail:.1}s, sink ends: {ends}");
    assert!(
        tail > 6.5 && tail < 10.0,
        "expected ~8s of delayed video after OBS stopped, got {tail:.1}s"
    );
    assert_eq!(ends, 1, "destination should close after the buffer drains");
    assert!(!sink.active.load(std::sync::atomic::Ordering::SeqCst));
}

/// The bug report case: keyframes never come after the first one. Adding delay must still resume
/// video and audio, removing it must never freeze, and the output must decode cleanly.
#[tokio::test(flavor = "current_thread")]
async fn rare_keyframes() {
    if !have_ffmpeg() {
        eprintln!("ffmpeg not found — skipping");
        return;
    }
    let dir = std::env::temp_dir().join(format!("db-rare-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let flv_path = dir.join("out.flv");
    let sink = start_sink(29370, Some(flv_path.to_str().unwrap())).await;
    let app = start_app(&dir, 29371, 29370, "");
    start_ingest(&app, 29371).await;
    let t0 = Instant::now();
    let mut ff = ffmpeg_publish("rtmp://127.0.0.1:29371/live/x", 40, 0);
    let at = |s: u64| async move {
        let target = t0 + Duration::from_secs(s);
        let now = Instant::now();
        if target > now {
            tokio::time::sleep(target - now).await;
        }
    };
    let video_since = |since: Instant| {
        sink.packets
            .lock()
            .unwrap()
            .iter()
            .filter(|p| p.kind == Kind::Video && p.wall >= since)
            .count()
    };
    let mut checks: Vec<(String, bool)> = Vec::new();

    at(4).await;
    app.enable(Some(8.0), None, Some("instant".into()))
        .await
        .unwrap();
    at(6).await;
    let st = app.engine_status().await;
    checks.push((
        format!(
            "video paused (not frozen on a keyframe) while adding delay: paused={}",
            st.video_paused
        ),
        st.video_paused,
    ));
    at(16).await;
    let mark = Instant::now();
    at(19).await;
    let v = video_since(mark);
    let l = app.engine_status().await.viewer_lag_ms.unwrap_or(0);
    checks.push((
        format!("video flows again after the delay ({v} frames in 3 s, lag {l} ms)"),
        v >= 80 && (7500..9500).contains(&l),
    ));

    app.disable(Some("instant".into())).await.unwrap();
    let mark = Instant::now();
    at(24).await;
    let v = video_since(mark);
    let st = app.engine_status().await;
    checks.push((
        format!(
            "removing delay without keyframes never freezes ({v} frames in ~5 s, catching up={})",
            st.catching_up
        ),
        v >= 130 && st.catching_up,
    ));

    wait_child(&mut ff).await;
    tokio::time::sleep(Duration::from_secs(12)).await;
    drop(sink.flv.lock().unwrap().take());
    let pk = sink.packets.lock().unwrap().clone();
    let mut backwards = 0;
    for k in [Kind::Audio, Kind::Video] {
        let v: Vec<_> = pk.iter().filter(|p| p.kind == k).collect();
        backwards += v.windows(2).filter(|w| w[1].ts < w[0].ts).count();
    }
    let a: Vec<_> = pk.iter().filter(|p| p.kind == Kind::Audio).collect();
    let max_audio_gap = a
        .windows(2)
        .map(|w| w[1].ts as i64 - w[0].ts as i64)
        .max()
        .unwrap_or(0);
    checks.push(("timestamps never go backwards".into(), backwards == 0));
    checks.push((
        format!("audio never stops (max gap {max_audio_gap} ms)"),
        max_audio_gap <= 200,
    ));
    checks.push((
        "one continuous connection".into(),
        *sink.publishes.lock().unwrap() == 1,
    ));
    let dec = Command::new("ffmpeg")
        .args([
            "-hide_banner",
            "-v",
            "error",
            "-i",
            flv_path.to_str().unwrap(),
            "-f",
            "null",
            "-",
        ])
        .output()
        .unwrap();
    // Ignore ffmpeg's null-muxer notes about rounding ms timestamps onto a 30 fps grid; count real
    // decoder errors only.
    let errs = String::from_utf8_lossy(&dec.stderr)
        .lines()
        .filter(|l| !l.contains("non monotonically increasing dts to muxer"))
        .count();
    checks.push((
        format!("output decodes cleanly — nothing skipped mid-GOP ({errs} decoder messages)"),
        dec.status.success() && errs == 0,
    ));

    let mut ok = true;
    for (n, p) in &checks {
        println!("{}  {n}", if *p { "PASS" } else { "FAIL" });
        ok &= *p;
    }
    assert!(ok, "some checks failed");
}

/// Gradual mode: the delay builds up and goes away by playing slower/faster — no pause, no jump,
/// pitch-preserved audio, and nothing skipped.
#[tokio::test(flavor = "current_thread")]
async fn gradual_delay() {
    if !have_ffmpeg() {
        eprintln!("ffmpeg not found — skipping");
        return;
    }
    let dir = std::env::temp_dir().join(format!("db-gradual-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let flv_path = dir.join("out.flv");
    let sink = start_sink(29380, Some(flv_path.to_str().unwrap())).await;
    // The default 30 %: +6 s takes 20 s, removing it takes 20 s.
    let app = start_app_with(&dir, 29381, 29380, "", |c| c.delay.ramp_percent = 30);
    start_ingest(&app, 29381).await;
    let t0 = Instant::now();
    let mut ff = ffmpeg_publish("rtmp://127.0.0.1:29381/live/x", 62, 0); // keyframes don't matter here
    let at = |s: u64| async move {
        let target = t0 + Duration::from_secs(s);
        let now = Instant::now();
        if target > now {
            tokio::time::sleep(target - now).await;
        }
    };
    let mut checks: Vec<(String, bool)> = Vec::new();

    at(4).await;
    app.enable(Some(6.0), None, Some("gradual".into()))
        .await
        .unwrap();
    at(14).await;
    let st = app.engine_status().await;
    let l = st.viewer_lag_ms.unwrap_or(0);
    checks.push((
        format!(
            "halfway: ramping, delay ~3 s (got {l} ms, speed {})",
            st.ramp_speed
        ),
        st.ramping && (2000..4200).contains(&l),
    ));
    at(28).await;
    let st = app.engine_status().await;
    let l = st.viewer_lag_ms.unwrap_or(0);
    checks.push((
        format!("reached 6 s (got {l} ms, ramping={})", st.ramping),
        !st.ramping && (5600..6600).contains(&l),
    ));

    app.disable(Some("gradual".into())).await.unwrap();
    at(54).await;
    let st = app.engine_status().await;
    let l = st.viewer_lag_ms.unwrap_or(99999);
    checks.push((
        format!("removed gradually (lag {l} ms, ramping={})", st.ramping),
        !st.ramping && l < 700,
    ));

    wait_child(&mut ff).await;
    tokio::time::sleep(Duration::from_secs(3)).await;
    drop(sink.flv.lock().unwrap().take());

    let pk = sink.packets.lock().unwrap().clone();
    let v: Vec<_> = pk.iter().filter(|p| p.kind == Kind::Video).collect();
    let a: Vec<_> = pk.iter().filter(|p| p.kind == Kind::Audio).collect();
    let max_wall_gap = v
        .windows(2)
        .map(|w| w[1].wall.duration_since(w[0].wall).as_millis())
        .max()
        .unwrap_or(0);
    let backwards = [&v, &a]
        .iter()
        .map(|s| s.windows(2).filter(|w| w[1].ts < w[0].ts).count())
        .sum::<usize>();
    // Skip the sequence header (ts 0) at the start.
    let max_audio_ts_gap = a[1..]
        .windows(2)
        .map(|w| w[1].ts as i64 - w[0].ts as i64)
        .max()
        .unwrap_or(0);
    checks.push((
        format!("video never pauses (longest gap {max_wall_gap} ms)"),
        max_wall_gap < 250,
    ));
    checks.push(("timestamps never go backwards".into(), backwards == 0));
    checks.push((
        format!("audio continuous (max ts step {max_audio_ts_gap} ms)"),
        max_audio_ts_gap <= 30,
    ));
    checks.push((
        "one continuous connection".into(),
        *sink.publishes.lock().unwrap() == 1,
    ));
    // A/V sync: audio is stretched separately from the video, so check they still line up.
    let sync = a.last().unwrap().ts as i64 - v.last().unwrap().ts as i64;
    checks.push((
        format!("audio and video stay in sync (end offset {sync} ms)"),
        sync.abs() < 120,
    ));

    let dec = Command::new("ffmpeg")
        .args([
            "-hide_banner",
            "-v",
            "error",
            "-i",
            flv_path.to_str().unwrap(),
            "-f",
            "null",
            "-",
        ])
        .output()
        .unwrap();
    let errs = String::from_utf8_lossy(&dec.stderr)
        .lines()
        .filter(|l| !l.contains("non monotonically increasing dts to muxer"))
        .count();
    checks.push((
        format!("output decodes cleanly ({errs} decoder errors)"),
        dec.status.success() && errs == 0,
    ));

    // Audio: the 440 Hz test tone must stay at 440 Hz (pitch preserved) without clicks.
    let pcm = Command::new("ffmpeg")
        .args([
            "-v",
            "error",
            "-i",
            flv_path.to_str().unwrap(),
            "-vn",
            "-ac",
            "1",
            "-ar",
            "48000",
            "-f",
            "f32le",
            "-",
        ])
        .output()
        .unwrap()
        .stdout;
    let x: Vec<f32> = pcm
        .chunks(4)
        .map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]]))
        .collect();
    let mut worst_pitch = 0.0f64;
    for w in x[48000..x.len() - 48000].chunks_exact(48000) {
        let z = w
            .windows(2)
            .filter(|p| (p[0] < 0.0) != (p[1] < 0.0))
            .count() as f64
            / 2.0;
        worst_pitch = f64::max(worst_pitch, (z - 440.0).abs());
    }
    let clicks = x[48000..x.len() - 48000]
        .windows(2)
        .filter(|w| (w[1] - w[0]).abs() > 0.03)
        .count();
    checks.push((
        format!("pitch stays 440 Hz (worst 1 s window off by {worst_pitch:.1} Hz)"),
        worst_pitch < 6.0,
    ));
    checks.push((format!("no audible clicks ({clicks})"), clicks <= 4));

    let mut ok = true;
    for (n, p) in &checks {
        println!("{}  {n}", if *p { "PASS" } else { "FAIL" });
        ok &= *p;
    }
    assert!(ok, "some checks failed");
}

/// Rewind mode: adding delay replays the last N seconds straight away (no pause, no slow-down);
/// removing it jumps back to live. Rewinding further than the history holds adds the rest
/// gradually.
#[tokio::test(flavor = "current_thread")]
async fn rewind_delay() {
    if !have_ffmpeg() {
        eprintln!("ffmpeg not found — skipping");
        return;
    }
    let dir = std::env::temp_dir().join(format!("db-rewind-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let flv_path = dir.join("out.flv");
    let sink = start_sink(29390, Some(flv_path.to_str().unwrap())).await;
    let app = start_app_with(&dir, 29391, 29390, "", |c| {
        c.delay.rewind_seconds = 10;
        c.delay.ramp_percent = 50;
    });
    start_ingest(&app, 29391).await;
    let t0 = Instant::now();
    let secs = 44;
    let mut ff = ffmpeg_publish("rtmp://127.0.0.1:29391/live/x", secs, 30); // 1 s keyframes
    let at = |s: u64| async move {
        let target = t0 + Duration::from_secs(s);
        let now = Instant::now();
        if target > now {
            tokio::time::sleep(target - now).await;
        }
    };
    let mut checks: Vec<(String, bool)> = Vec::new();

    at(12).await;
    app.enable(Some(6.0), None, Some("rewind".into()))
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(400)).await;
    let st = app.engine_status().await;
    let l = st.viewer_lag_ms.unwrap_or(0);
    checks.push((
        format!(
            "instantly 6 s behind (lag {l} ms, delay {} ms, ramping={})",
            st.delay_ms, st.ramping
        ),
        !st.ramping && !st.switching && st.delay_ms == 6000 && (5500..7100).contains(&l),
    ));
    at(18).await;
    let st = app.engine_status().await;
    let l = st.viewer_lag_ms.unwrap_or(0);
    checks.push((
        format!("stays 6 s behind (lag {l} ms)"),
        (5500..7100).contains(&l),
    ));

    app.disable(Some("rewind".into())).await.unwrap();
    at(22).await;
    let st = app.engine_status().await;
    let l = st.viewer_lag_ms.unwrap_or(99999);
    checks.push((format!("removed: back to live (lag {l} ms)"), l < 1500));

    // More than the 10 s of history: rewinds what it has, ramps the rest.
    at(26).await;
    app.enable(Some(14.0), None, Some("rewind".into()))
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(400)).await;
    let st = app.engine_status().await;
    let l = st.viewer_lag_ms.unwrap_or(0);
    checks.push((
        format!(
            "short history: rewound ~10 s then ramping (lag {l} ms, ramping={})",
            st.ramping
        ),
        st.ramping && (9500..12200).contains(&l),
    ));
    at(36).await;
    let st = app.engine_status().await;
    let l = st.viewer_lag_ms.unwrap_or(0);
    checks.push((
        format!("reached 14 s (lag {l} ms, ramping={})", st.ramping),
        !st.ramping && (13500..15000).contains(&l),
    ));

    wait_child(&mut ff).await;
    // Let the delayed tail drain.
    tokio::time::sleep(Duration::from_secs(17)).await;
    drop(sink.flv.lock().unwrap().take());

    let pk = sink.packets.lock().unwrap().clone();
    let v: Vec<_> = pk
        .iter()
        .filter(|p| p.kind == Kind::Video && p.ts > 0)
        .collect();
    let a: Vec<_> = pk.iter().filter(|p| p.kind == Kind::Audio).collect();
    // The stream drains fully, so the output is longer than the input by the final delay
    // (14 s; the first rewind's extra seconds were skipped again when removing it).
    let span = (v.last().unwrap().ts - v.first().unwrap().ts) as f64 / 1000.0;
    checks.push((
        format!("replayed video is in the output (span {span:.1} s for {secs} s of input)"),
        (span - (secs as f64 + 14.0)).abs() < 1.5,
    ));
    let max_wall_gap = v
        .windows(2)
        .map(|w| w[1].wall.duration_since(w[0].wall).as_millis())
        .max()
        .unwrap_or(0);
    checks.push((
        format!("video never pauses (longest gap {max_wall_gap} ms)"),
        max_wall_gap < 250,
    ));
    let backwards = [&v, &a]
        .iter()
        .map(|s| s.windows(2).filter(|w| w[1].ts < w[0].ts).count())
        .sum::<usize>();
    checks.push(("timestamps never go backwards".into(), backwards == 0));
    let max_audio_ts_gap = a[1..]
        .windows(2)
        .map(|w| w[1].ts as i64 - w[0].ts as i64)
        .max()
        .unwrap_or(0);
    // Each cut (rewind, jump back to live) may leave one short audio gap: the new segment starts
    // after the last video frame's display time. No other gaps.
    let audio_gaps = a[1..]
        .windows(2)
        .filter(|w| w[1].ts as i64 - w[0].ts as i64 > 30)
        .count();
    checks.push((
        format!("audio continuous apart from cuts ({audio_gaps} gap(s), max ts step {max_audio_ts_gap} ms)"),
        audio_gaps <= 3 && max_audio_ts_gap <= 200,
    ));
    checks.push((
        "one continuous connection".into(),
        *sink.publishes.lock().unwrap() == 1,
    ));
    let dec = Command::new("ffmpeg")
        .args(["-hide_banner", "-v", "error", "-i"])
        .arg(&flv_path)
        .args(["-f", "null", "-"])
        .output()
        .unwrap();
    let errs = String::from_utf8_lossy(&dec.stderr)
        .lines()
        .filter(|l| !l.contains("non monotonically increasing dts to muxer"))
        .count();
    checks.push((
        format!("output decodes cleanly ({errs} decoder errors)"),
        dec.status.success() && errs == 0,
    ));

    let mut ok = true;
    for (n, p) in &checks {
        println!("{}  {n}", if *p { "PASS" } else { "FAIL" });
        ok &= *p;
    }
    assert!(ok, "some checks failed");
}
