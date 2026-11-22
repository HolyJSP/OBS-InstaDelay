//! Settings, stored as `config.json` next to the executable.

use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct Ingest {
    pub host: String,
    pub port: u16,
    pub stream_key: String,
}

impl Default for Ingest {
    fn default() -> Self {
        Ingest {
            host: "127.0.0.1".into(),
            port: 1935,
            stream_key: String::new(),
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct Web {
    pub host: String,
    pub port: u16,
    /// Open the dashboard in the browser when Delay Bridge starts on its own.
    /// (When started by the OBS plugin it only opens during first-time setup.)
    pub open_on_start: bool,
}

impl Default for Web {
    fn default() -> Self {
        Web {
            host: "127.0.0.1".into(),
            port: 8080,
            open_on_start: true,
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct Destination {
    pub name: String,
    pub url: String,
    pub key: String,
    pub enabled: bool,
}

impl Default for Destination {
    fn default() -> Self {
        Destination {
            name: "Destination".into(),
            url: String::new(),
            key: String::new(),
            enabled: true,
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct Delay {
    pub presets: Vec<u64>,
    pub default_seconds: u64,
    pub default_duration_minutes: f64,
    pub duration_presets: Vec<u64>,
    pub max_seconds: u64,
    pub default_mode: String,
    pub filler_fps: u32,
    /// Gradual mode: how much slower/faster the stream plays while the delay changes (percent).
    pub ramp_percent: u32,
}

impl Default for Delay {
    fn default() -> Self {
        Delay {
            presets: vec![15, 30, 60, 120],
            default_seconds: 60,
            default_duration_minutes: 0.0,
            duration_presets: vec![5, 10, 20, 30],
            max_seconds: 600,
            default_mode: "gradual".into(),
            filler_fps: 2,
            ramp_percent: 30,
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct Obs {
    pub enabled: bool,
    pub url: String,
    pub password: String,
    pub overlay_scene: String,
    pub trigger_source: String,
    /// What OBS streamed to before "Use Delay Bridge" switched it (used by "Stream directly").
    /// Contains the stream key, so it's never sent to the dashboard.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub original_service: Option<serde_json::Value>,
}

impl Default for Obs {
    fn default() -> Self {
        Obs {
            enabled: false,
            url: "ws://127.0.0.1:4455".into(),
            password: String::new(),
            overlay_scene: String::new(),
            trigger_source: "Delay Toggle".into(),
            original_service: None,
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct Config {
    pub ingest: Ingest,
    pub web: Web,
    pub destinations: Vec<Destination>,
    pub delay: Delay,
    pub obs: Obs,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            ingest: Ingest::default(),
            web: Web::default(),
            destinations: vec![Destination {
                name: "Twitch".into(),
                url: "rtmp://live.twitch.tv/app".into(),
                ..Default::default()
            }],
            delay: Delay::default(),
            obs: Obs::default(),
        }
    }
}

impl Config {
    pub fn load(path: &Path) -> Result<Config, String> {
        if !path.exists() {
            let c = Config::default();
            c.save(path)?;
            return Ok(c);
        }
        let text = std::fs::read_to_string(path)
            .map_err(|e| format!("Could not read {}: {e}", path.display()))?;
        serde_json::from_str(&text)
            .map_err(|e| format!("{} is not valid JSON: {e}", path.display()))
    }

    pub fn save(&self, path: &Path) -> Result<(), String> {
        if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
            let _ = std::fs::create_dir_all(dir);
        }
        let tmp = path.with_extension("json.tmp");
        let text = serde_json::to_string_pretty(self).unwrap() + "\n";
        std::fs::write(&tmp, text)
            .map_err(|e| format!("Could not write {}: {e}", tmp.display()))?;
        std::fs::rename(&tmp, path).map_err(|e| format!("Could not save {}: {e}", path.display()))
    }

    pub fn sanitize(&mut self) {
        self.delay.presets.retain(|n| *n > 0);
        self.delay.presets.truncate(8);
        self.delay.duration_presets.retain(|n| *n > 0);
        self.delay.duration_presets.truncate(8);
        self.delay.max_seconds = self.delay.max_seconds.clamp(5, 3600);
        self.delay.filler_fps = self.delay.filler_fps.clamp(1, 30);
        if !matches!(self.delay.default_mode.as_str(), "overlay" | "gradual") {
            self.delay.default_mode = "instant".into();
        }
        self.delay.ramp_percent = self.delay.ramp_percent.clamp(2, 50);
        for d in &mut self.destinations {
            d.name = d.name.chars().take(60).collect();
            d.url = d.url.trim().to_string();
            d.key = d.key.trim().to_string();
        }
    }
}
