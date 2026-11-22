//! Ties everything together: OBS ingest -> delay engine -> destinations, plus timers, OBS control,
//! settings and the status shown on the dashboard.

use crate::Logger;
use crate::config::Config;
use crate::engine::{CapturedKey, Engine, EngineEvent, EngineMsg, EngineStatus};
use crate::obs::ObsHandle;
use crate::output::OutputHandle;
use crate::plugin_link::PluginLink;
use serde_json::{Value, json};
use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tokio::sync::{mpsc, oneshot, watch};

/// When `DELAY_BRIDGE_LOG` is set (the OBS plugin sets it), log lines are also appended there.
fn log_file() -> Option<&'static Mutex<std::fs::File>> {
    static FILE: std::sync::OnceLock<Option<Mutex<std::fs::File>>> = std::sync::OnceLock::new();
    FILE.get_or_init(|| {
        let path = std::env::var_os("DELAY_BRIDGE_LOG")?;
        std::fs::OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(path)
            .ok()
            .map(Mutex::new)
    })
    .as_ref()
}

pub fn epoch_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

#[derive(Clone)]
struct LogLine {
    t: u64,
    msg: String,
}

pub struct App {
    pub cfg: Mutex<Config>,
    pub config_path: PathBuf,
    logs: Mutex<VecDeque<LogLine>>,
    started: Instant,
    expires: watch::Sender<Option<u64>>,
    busy: Mutex<Option<&'static str>>,
    last_mode: Mutex<String>,
    pub engine_tx: mpsc::Sender<EngineMsg>,
    outputs: Mutex<Vec<OutputHandle>>,
    pending_outputs: AtomicBool,
    obs: Mutex<Option<Arc<ObsHandle>>>,
    trigger_tx: mpsc::Sender<()>,
    pub ingest_active: Arc<AtomicBool>,
    pub ingest_key: Arc<Mutex<String>>,
    /// Channel to the OBS plugin, when it started us.
    plugin: Option<Arc<PluginLink>>,
    obs_info: Mutex<Value>,
}

impl App {
    /// Build the app and spawn its background tasks (engine, timer, hotkey listener).
    pub fn start(cfg: Config, config_path: PathBuf, plugin: Option<Arc<PluginLink>>) -> Arc<App> {
        let (engine_tx, engine_rx) = mpsc::channel(8192);
        let (trigger_tx, mut trigger_rx) = mpsc::channel(4);
        let (expires, mut expires_rx) = watch::channel(None);
        let app = Arc::new(App {
            ingest_key: Arc::new(Mutex::new(cfg.ingest.stream_key.clone())),
            last_mode: Mutex::new(cfg.delay.default_mode.clone()),
            cfg: Mutex::new(cfg.clone()),
            config_path,
            logs: Mutex::new(VecDeque::new()),
            started: Instant::now(),
            expires,
            busy: Mutex::new(None),
            engine_tx,
            outputs: Mutex::new(Vec::new()),
            pending_outputs: AtomicBool::new(false),
            obs: Mutex::new(None),
            trigger_tx,
            ingest_active: Arc::new(AtomicBool::new(false)),
            plugin,
            obs_info: Mutex::new(Value::Null),
        });
        app.build_outputs();
        app.start_obs();

        // Delay engine: its events fan out straight to the destinations.
        let sink_app = app.clone();
        let engine = Engine::new(
            cfg.delay.filler_fps,
            cfg.delay.max_seconds * 1000,
            Box::new(move |ev| match ev {
                EngineEvent::Packet(p) => {
                    for o in sink_app.outputs.lock().unwrap().iter_mut() {
                        o.offer(&p);
                    }
                }
                EngineEvent::Notice(msg) => sink_app.log(&msg),
                EngineEvent::Ended => {
                    sink_app.log("Stream finished sending — destinations closed");
                    for o in sink_app.outputs.lock().unwrap().iter() {
                        o.end();
                    }
                    if sink_app.pending_outputs.load(Ordering::SeqCst) {
                        let a = sink_app.clone();
                        tokio::spawn(async move {
                            tokio::time::sleep(Duration::from_secs(3)).await;
                            a.build_outputs();
                        });
                    }
                }
            }),
        );
        tokio::spawn(engine.run(engine_rx));
        let _ = app.engine_tx.try_send(EngineMsg::Configure {
            filler_fps: cfg.delay.filler_fps,
            max_delay_ms: cfg.delay.max_seconds * 1000,
            ramp_pct: cfg.delay.ramp_percent,
        });

        // Auto-off timer: sleeps until the deadline, no polling.
        let timer_app = app.clone();
        tokio::spawn(async move {
            loop {
                let deadline = *expires_rx.borrow_and_update();
                match deadline {
                    None => {
                        if expires_rx.changed().await.is_err() {
                            return;
                        }
                    }
                    Some(t) => {
                        let wait = Duration::from_millis(t.saturating_sub(epoch_ms()));
                        tokio::select! {
                            _ = tokio::time::sleep(wait) => {
                                if *expires_rx.borrow() == Some(t) {
                                    timer_app.log("Delay timer finished — removing delay");
                                    let _ = timer_app.disable(None).await;
                                }
                            }
                            r = expires_rx.changed() => if r.is_err() { return },
                        }
                    }
                }
            }
        });

        // OBS hotkey (show/hide the trigger source) toggles the delay.
        let hk_app = app.clone();
        tokio::spawn(async move {
            while trigger_rx.recv().await.is_some() {
                let name = hk_app.cfg.lock().unwrap().obs.trigger_source.clone();
                hk_app.log(&format!("Hotkey: \"{name}\" toggled"));
                if let Err(e) = hk_app.toggle(None, None, None).await {
                    hk_app.log(&e);
                }
            }
        });
        app
    }

    pub fn logger(self: &Arc<Self>) -> Logger {
        let app = self.clone();
        Arc::new(move |m: String| app.log(&m))
    }

    pub fn log(&self, msg: &str) {
        let up = self.started.elapsed().as_secs();
        let stamp = format!("[{:02}:{:02}:{:02}]", up / 3600, (up / 60) % 60, up % 60);
        if let Some(f) = log_file() {
            use std::io::Write;
            let _ = writeln!(f.lock().unwrap(), "{stamp} {msg}");
        }
        // With the OBS plugin, stdout is the private channel to the plugin — never print there.
        if self.plugin.is_none() {
            println!("{stamp} {msg}");
        }
        let mut logs = self.logs.lock().unwrap();
        logs.push_back(LogLine {
            t: epoch_ms(),
            msg: msg.to_string(),
        });
        while logs.len() > 80 {
            logs.pop_front();
        }
    }

    fn build_outputs(self: &Arc<Self>) {
        let mut outs = self.outputs.lock().unwrap();
        if outs.iter().any(|o| o.is_busy()) {
            self.pending_outputs.store(true, Ordering::SeqCst);
            return;
        }
        let dests = self.cfg.lock().unwrap().destinations.clone();
        let log = self.logger();
        *outs = dests
            .into_iter()
            .filter(|d| !d.url.is_empty())
            .map(|d| OutputHandle::spawn(d, log.clone()))
            .collect();
        self.pending_outputs.store(false, Ordering::SeqCst);
    }

    fn start_obs(self: &Arc<Self>) {
        let o = self.cfg.lock().unwrap().obs.clone();
        let handle = o.enabled.then(|| {
            Arc::new(ObsHandle::spawn(
                o.url,
                o.password,
                o.trigger_source,
                self.trigger_tx.clone(),
                self.logger(),
            ))
        });
        *self.obs.lock().unwrap() = handle;
    }

    pub async fn engine_status(&self) -> EngineStatus {
        let (tx, rx) = oneshot::channel();
        if self.engine_tx.send(EngineMsg::Status(tx)).await.is_err() {
            return EngineStatus::default();
        }
        rx.await.unwrap_or_default()
    }

    // ---- actions -----------------------------------------------------------------------------

    pub async fn enable(
        self: &Arc<Self>,
        seconds: Option<f64>,
        minutes: Option<f64>,
        mode: Option<String>,
    ) -> Result<Value, String> {
        let d = self.cfg.lock().unwrap().delay.clone();
        let seconds = seconds.unwrap_or(d.default_seconds as f64);
        if !seconds.is_finite() || seconds < 0.0 {
            return Err("Invalid delay".into());
        }
        let seconds = seconds.min(d.max_seconds as f64);
        let minutes = minutes.unwrap_or(d.default_duration_minutes).max(0.0);
        let mode = parse_mode(mode.as_deref(), &d.default_mode);
        if seconds == 0.0 {
            return self.disable(Some(mode.to_string())).await;
        }
        if self.busy.lock().unwrap().is_some() {
            return Err("Already switching — try again in a moment".into());
        }
        let ms = (seconds * 1000.0).round() as u64;
        let st = self.engine_status().await;
        let mut filler = None;
        if ms > st.delay_ms && mode == "overlay" && st.live {
            *self.busy.lock().unwrap() = Some("overlay");
            match self.capture_overlay().await {
                Ok(k) => filler = Some(k),
                Err(e) => self.log(&format!(
                    "Overlay mode failed ({e}) — falling back to frozen frame"
                )),
            }
            *self.busy.lock().unwrap() = None;
        }
        let overlay = filler.is_some();
        let _ = self
            .engine_tx
            .send(EngineMsg::SetDelay {
                ms,
                filler,
                gradual: mode == "gradual",
            })
            .await;
        *self.last_mode.lock().unwrap() = mode.to_string();
        let _ = self.expires.send(if minutes > 0.0 {
            Some(epoch_ms() + (minutes * 60_000.0) as u64)
        } else {
            None
        });
        let dur = if minutes > 0.0 {
            format!(" for {minutes} min")
        } else {
            String::new()
        };
        let how = if overlay {
            " (overlay)"
        } else if mode == "gradual" {
            " (gradually)"
        } else {
            ""
        };
        self.log(&format!("Delay set to {seconds}s{dur}{how}"));
        Ok(self.status().await)
    }

    async fn capture_keyframe(&self, after_ms: u64) -> Result<CapturedKey, String> {
        let (tx, rx) = oneshot::channel();
        let _ = self
            .engine_tx
            .send(EngineMsg::Capture {
                after_ms,
                reply: tx,
            })
            .await;
        match tokio::time::timeout(Duration::from_secs(6), rx).await {
            Ok(Ok(k)) => Ok(k),
            _ => Err("no keyframe received from OBS (set Keyframe Interval to 1–2 s)".into()),
        }
    }

    async fn capture_overlay(&self) -> Result<CapturedKey, String> {
        let scene = self.cfg.lock().unwrap().obs.overlay_scene.clone();
        let obs = self.obs.lock().unwrap().clone();
        let obs = obs
            .filter(|o| o.connected())
            .ok_or("not connected to OBS WebSocket")?;
        if scene.is_empty() {
            return Err("no overlay scene chosen in settings".into());
        }
        let previous = obs.current_scene().await?;
        if previous == scene {
            return self.capture_keyframe(0).await;
        }
        obs.set_scene(&scene).await?;
        let r = self.capture_keyframe(400).await;
        if let Err(e) = obs.set_scene(&previous).await {
            self.log(&format!("Could not switch back to \"{previous}\": {e}"));
        }
        r
    }

    pub async fn disable(self: &Arc<Self>, mode: Option<String>) -> Result<Value, String> {
        let _ = self.expires.send(None);
        let default_mode = self.cfg.lock().unwrap().delay.default_mode.clone();
        let gradual = parse_mode(mode.as_deref(), &default_mode) == "gradual";
        if self.engine_status().await.delay_ms > 0 {
            self.log(if gradual {
                "Removing delay gradually"
            } else {
                "Delay removed — viewers jump to live"
            });
        }
        let _ = self
            .engine_tx
            .send(EngineMsg::SetDelay {
                ms: 0,
                filler: None,
                gradual,
            })
            .await;
        Ok(self.status().await)
    }

    pub async fn toggle(
        self: &Arc<Self>,
        seconds: Option<f64>,
        minutes: Option<f64>,
        mode: Option<String>,
    ) -> Result<Value, String> {
        if self.engine_status().await.delay_ms > 0 {
            self.disable(mode.clone()).await
        } else {
            self.enable(seconds, minutes, mode).await
        }
    }

    pub async fn extend(self: &Arc<Self>, minutes: f64) -> Result<Value, String> {
        if self.engine_status().await.delay_ms == 0 {
            return Err("Delay is not on".into());
        }
        let now = epoch_ms();
        let base = self.expires.borrow().filter(|t| *t > now).unwrap_or(now);
        let _ = self.expires.send(Some(base + (minutes * 60_000.0) as u64));
        self.log(&format!("Delay timer extended by {minutes} min"));
        Ok(self.status().await)
    }

    pub async fn clear_timer(self: &Arc<Self>) -> Result<Value, String> {
        let _ = self.expires.send(None);
        self.log("Delay timer cleared — delay stays on until you remove it");
        Ok(self.status().await)
    }

    pub async fn end_now(self: &Arc<Self>) -> Result<Value, String> {
        self.log("Dropping remaining buffered video");
        let _ = self.engine_tx.send(EngineMsg::Flush).await;
        Ok(self.status().await)
    }

    pub async fn shutdown(&self) {
        for o in self.outputs.lock().unwrap().iter() {
            o.end();
        }
        tokio::time::sleep(Duration::from_millis(600)).await;
    }

    // ---- status / config ---------------------------------------------------------------------

    pub async fn status(&self) -> Value {
        let engine = self.engine_status().await;
        let obs_info = self.obs_info().await;
        let cfg = self.cfg.lock().unwrap().clone();
        let outputs: Vec<_> = self
            .outputs
            .lock()
            .unwrap()
            .iter()
            .map(|o| o.status())
            .collect();
        let obs = self.obs.lock().unwrap().clone();
        let host = if cfg.ingest.host == "0.0.0.0" {
            "127.0.0.1"
        } else {
            cfg.ingest.host.as_str()
        };
        let logs: Vec<Value> = {
            let l = self.logs.lock().unwrap();
            l.iter()
                .skip(l.len().saturating_sub(30))
                .map(|x| json!({"t": x.t, "msg": x.msg}))
                .collect()
        };
        json!({
            "engine": engine,
            "expiresAt": *self.expires.borrow(),
            "now": epoch_ms(),
            "busy": *self.busy.lock().unwrap(),
            "mode": *self.last_mode.lock().unwrap(),
            "ingest": {
                "url": format!("rtmp://{host}:{}/live", cfg.ingest.port),
                "connected": self.ingest_active.load(Ordering::SeqCst),
                "keyRequired": !cfg.ingest.stream_key.is_empty(),
            },
            "outputs": outputs,
            "pendingOutputs": self.pending_outputs.load(Ordering::SeqCst),
            "obs": match (&obs, cfg.obs.enabled) {
                (Some(o), true) => json!({"enabled": true, "connected": o.connected(), "error": o.error(), "overlayScene": cfg.obs.overlay_scene}),
                _ => json!({"enabled": false}),
            },
            "delay": {
                "presets": cfg.delay.presets,
                "durationPresets": cfg.delay.duration_presets,
                "defaultSeconds": cfg.delay.default_seconds,
                "defaultDurationMinutes": cfg.delay.default_duration_minutes,
                "defaultMode": cfg.delay.default_mode,
                "rampPercent": cfg.delay.ramp_percent,
                "maxSeconds": cfg.delay.max_seconds,
            },
            "logs": logs,
            "plugin": { "connected": self.plugin.as_ref().is_some_and(|p| p.alive()), "obs": obs_info },
        })
    }

    pub fn public_config(&self) -> Value {
        let cfg = self.cfg.lock().unwrap().clone();
        let mut v = serde_json::to_value(&cfg).unwrap();
        if let Some(dests) = v["destinations"].as_array_mut() {
            for (i, d) in dests.iter_mut().enumerate() {
                let has = !d["key"].as_str().unwrap_or("").is_empty();
                d["key"] = json!("");
                d["hasKey"] = json!(has);
                d["id"] = json!(i);
            }
        }
        v["obs"]["hasPassword"] = json!(!cfg.obs.password.is_empty());
        if let Some(o) = v["obs"].as_object_mut() {
            o.remove("originalService");
        }
        v["obs"]["password"] = json!("");
        v
    }

    pub async fn update_config(self: &Arc<Self>, patch: Value) -> Result<Value, String> {
        let old = self.cfg.lock().unwrap().clone();
        let mut v = serde_json::to_value(&old).unwrap();
        merge(&mut v, &patch);
        if let Some(dests) = patch.get("destinations").and_then(|d| d.as_array()) {
            let list: Vec<Value> = dests
                .iter()
                .map(|d| {
                    let prev = d
                        .get("id")
                        .and_then(|i| i.as_u64())
                        .and_then(|i| old.destinations.get(i as usize));
                    let key = d
                        .get("key")
                        .and_then(|k| k.as_str())
                        .filter(|k| !k.trim().is_empty())
                        .map(String::from)
                        .or_else(|| prev.map(|p| p.key.clone()))
                        .unwrap_or_default();
                    json!({
                        "name": d.get("name").and_then(|x| x.as_str()).unwrap_or("Destination"),
                        "url": d.get("url").and_then(|x| x.as_str()).unwrap_or(""),
                        "key": key,
                        "enabled": d.get("enabled").and_then(|x| x.as_bool()).unwrap_or(true),
                    })
                })
                .collect();
            v["destinations"] = Value::Array(list);
        }
        if patch.get("obs").is_some() && patch["obs"]["password"].as_str().unwrap_or("").is_empty()
        {
            v["obs"]["password"] = json!(old.obs.password);
        }
        let mut next: Config =
            serde_json::from_value(v).map_err(|e| format!("Invalid settings: {e}"))?;
        next.sanitize();
        next.save(&self.config_path)?;
        *self.cfg.lock().unwrap() = next.clone();

        let _ = self
            .engine_tx
            .send(EngineMsg::Configure {
                filler_fps: next.delay.filler_fps,
                max_delay_ms: next.delay.max_seconds * 1000,
                ramp_pct: next.delay.ramp_percent,
            })
            .await;
        *self.ingest_key.lock().unwrap() = next.ingest.stream_key.clone();
        if old.destinations != next.destinations {
            self.build_outputs();
            if self.pending_outputs.load(Ordering::SeqCst) {
                self.log("Destination changes will apply when this stream ends");
            }
        }
        if old.obs != next.obs {
            self.start_obs();
        }
        let restart = old.ingest.host != next.ingest.host
            || old.ingest.port != next.ingest.port
            || old.web != next.web;
        if restart {
            self.log("Port / address changes take effect after restarting Delay Bridge");
        }
        self.log("Settings saved");
        Ok(json!({"ok": true, "restartRequired": restart}))
    }

    // ---- OBS plugin integration ------------------------------------------------------------

    fn ingest_args(&self) -> Value {
        let c = self.cfg.lock().unwrap();
        json!({
            "ingestPort": c.ingest.port,
            "ingestUrl": format!("rtmp://127.0.0.1:{}/live", c.ingest.port),
            "ingestKey": c.ingest.stream_key,
        })
    }

    fn plugin(&self) -> Result<&Arc<PluginLink>, String> {
        self.plugin
            .as_ref()
            .filter(|p| p.alive())
            .ok_or_else(|| "Only available when Delay Bridge runs as the OBS plugin".to_string())
    }

    /// Live info from OBS (stream settings, encoder, dropped frames). Cached; refreshed at most
    /// about once a second and only while someone is looking at the dashboard.
    async fn obs_info(&self) -> Value {
        let Ok(p) = self.plugin() else {
            return Value::Null;
        };
        match p
            .call("info", self.ingest_args(), Duration::from_millis(600))
            .await
        {
            Ok(v) => {
                *self.obs_info.lock().unwrap() = v.clone();
                v
            }
            Err(_) => self.obs_info.lock().unwrap().clone(),
        }
    }

    /// Add or update a destination from OBS's stream settings.
    fn upsert_destination(cfg: &mut Config, name: &str, server: &str, key: &str) -> String {
        let norm = |u: &str| u.trim().trim_end_matches('/').to_ascii_lowercase();
        let pretty = name.split(" - ").next().unwrap_or(name).trim();
        let pretty = if pretty.is_empty() || pretty == "Custom" {
            "Stream"
        } else {
            pretty
        };
        if let Some(d) = cfg
            .destinations
            .iter_mut()
            .find(|d| norm(&d.url) == norm(server) || d.name.eq_ignore_ascii_case(pretty))
        {
            d.url = server.to_string();
            d.key = key.to_string();
            d.enabled = true;
            return d.name.clone();
        }
        cfg.destinations.insert(
            0,
            crate::config::Destination {
                name: pretty.to_string(),
                url: server.to_string(),
                key: key.to_string(),
                enabled: true,
            },
        );
        pretty.to_string()
    }

    fn apply_destinations(self: &Arc<Self>, next: Config) -> Result<(), String> {
        next.save(&self.config_path)?;
        *self.cfg.lock().unwrap() = next;
        self.build_outputs();
        Ok(())
    }

    /// Copy OBS's stream settings (server + key) into Delay Bridge, without changing OBS.
    pub async fn import_from_obs(self: &Arc<Self>) -> Result<Value, String> {
        let r = self
            .plugin()?
            .call("import", self.ingest_args(), Duration::from_secs(5))
            .await?;
        let (name, server, key) = (
            r["name"].as_str().unwrap_or(""),
            r["server"].as_str().unwrap_or(""),
            r["key"].as_str().unwrap_or(""),
        );
        let mut next = self.cfg.lock().unwrap().clone();
        let dest = Self::upsert_destination(&mut next, name, server, key);
        self.apply_destinations(next)?;
        self.log(&format!("Imported {dest} stream settings from OBS"));
        Ok(json!({ "ok": true, "destination": dest }))
    }

    /// One-click setup: take OBS's stream settings into Delay Bridge and point OBS at Delay Bridge.
    pub async fn use_from_obs(self: &Arc<Self>) -> Result<Value, String> {
        let r = self
            .plugin()?
            .call("use_bridge", self.ingest_args(), Duration::from_secs(10))
            .await?;
        if r["already"].as_bool() == Some(true) {
            return Ok(json!({ "ok": true, "already": true }));
        }
        let (name, server, key) = (
            r["name"].as_str().unwrap_or(""),
            r["server"].as_str().unwrap_or(""),
            r["key"].as_str().unwrap_or(""),
        );
        let mut next = self.cfg.lock().unwrap().clone();
        let dest = Self::upsert_destination(&mut next, name, server, key);
        next.obs.original_service = Some(r["original"].clone());
        self.apply_destinations(next)?;
        let note = if self.ingest_active.load(Ordering::SeqCst)
            || self.obs_info.lock().unwrap()["streaming"] == true
        {
            " — takes effect the next time you start streaming"
        } else {
            ""
        };
        self.log(&format!(
            "OBS now streams through Delay Bridge → {dest}{note}"
        ));
        Ok(json!({ "ok": true, "destination": dest, "note": note.trim_start_matches(" — ") }))
    }

    /// Point OBS straight at the platform again (bypassing Delay Bridge).
    pub async fn stream_direct(self: &Arc<Self>) -> Result<Value, String> {
        let cfg = self.cfg.lock().unwrap().clone();
        let args = match &cfg.obs.original_service {
            Some(orig) => json!({ "original": orig }),
            None => {
                let d = cfg
                    .destinations
                    .iter()
                    .find(|d| d.enabled && !d.key.is_empty())
                    .ok_or("No destination with a stream key to switch OBS to")?;
                json!({ "server": d.url, "key": d.key })
            }
        };
        self.plugin()?
            .call("restore", args, Duration::from_secs(10))
            .await?;
        self.log("OBS now streams directly to the platform (Delay Bridge bypassed)");
        Ok(json!({ "ok": true }))
    }

    pub async fn obs_scenes(&self) -> Vec<String> {
        let obs = self.obs.lock().unwrap().clone();
        match obs {
            Some(o) if o.connected() => o.scene_names().await.unwrap_or_default(),
            _ => vec![],
        }
    }
}

fn merge(base: &mut Value, patch: &Value) {
    match (base, patch) {
        (Value::Object(b), Value::Object(p)) => {
            for (k, v) in p {
                match b.get_mut(k) {
                    Some(bv) if bv.is_object() && v.is_object() => merge(bv, v),
                    _ => {
                        b.insert(k.clone(), v.clone());
                    }
                }
            }
        }
        (b, p) => *b = p.clone(),
    }
}

/// "overlay", "gradual" or "instant" (frozen frame); falls back to the configured default.
fn parse_mode(mode: Option<&str>, default: &str) -> &'static str {
    match mode.unwrap_or(default) {
        "overlay" => "overlay",
        "gradual" => "gradual",
        _ => "instant",
    }
}
