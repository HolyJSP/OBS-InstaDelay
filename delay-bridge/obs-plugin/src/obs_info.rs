//! Reading OBS's stream settings and live output stats, and switching OBS between streaming
//! directly to the platform and streaming through Delay Bridge.

use crate::api;
use crate::ffi::{P, cstr};
use serde_json::{Value, json};
use std::ffi::{CStr, CString};

const CONNECT_SERVER_URL: u32 = 0;
const CONNECT_STREAM_ID: u32 = 2;

/// Owned obs_data_t reference, released on drop.
struct Data(P);

impl Drop for Data {
    fn drop(&mut self) {
        if let (false, Some(release)) = (self.0.is_null(), api().data_release) {
            // SAFETY: we own one reference to this obs_data_t.
            unsafe { release(self.0) };
        }
    }
}

impl Data {
    fn string(&self, key: &CStr) -> String {
        match api().data_get_string {
            // SAFETY: valid obs_data_t and key.
            Some(get) if !self.0.is_null() => cstr(unsafe { get(self.0, key.as_ptr()) }),
            _ => String::new(),
        }
    }

    fn int(&self, key: &CStr) -> Option<i64> {
        match api().data_get_int {
            // SAFETY: valid obs_data_t and key.
            Some(get) if !self.0.is_null() => Some(unsafe { get(self.0, key.as_ptr()) }),
            _ => None,
        }
    }

    fn json(&self) -> Value {
        match api().data_get_json {
            // SAFETY: returns a string owned by the obs_data_t.
            Some(get) if !self.0.is_null() => {
                serde_json::from_str(&cstr(unsafe { get(self.0) })).unwrap_or(Value::Null)
            }
            _ => Value::Null,
        }
    }
}

pub struct Snapshot {
    pub kind: String,
    pub name: String,
    pub server: String,
    pub key: String,
    pub settings: Value,
}

fn default_server(name: &str) -> Option<&'static str> {
    let n = name.to_ascii_lowercase();
    if n.contains("twitch") {
        Some("rtmp://live.twitch.tv/app")
    } else if n.contains("youtube") {
        Some("rtmp://a.rtmp.youtube.com/live2")
    } else {
        None
    }
}

/// Current streaming service as set in OBS → Settings → Stream.
pub fn snapshot() -> Option<Snapshot> {
    let a = api();
    // SAFETY: returns OBS's current service (not addref'd) or NULL.
    let svc = unsafe { (a.get_streaming_service?)() };
    if svc.is_null() {
        return None;
    }
    // SAFETY: valid service pointer for the calls below.
    let kind = a
        .service_get_type
        .map(|f| cstr(unsafe { f(svc) }))
        .unwrap_or_default();
    let settings = Data(
        a.service_get_settings
            .map(|f| unsafe { f(svc) })
            .unwrap_or(std::ptr::null_mut()),
    );
    let connect = |t: u32| {
        a.service_get_connect_info
            .map(|f| cstr(unsafe { f(svc, t) }))
            .unwrap_or_default()
    };

    let name = match kind.as_str() {
        "rtmp_common" => settings.string(c"service"),
        "rtmp_custom" => "Custom".to_string(),
        other => other.to_string(),
    };
    // Read the saved settings first. Asking OBS to resolve "auto" can trigger a network lookup
    // (Twitch's ingest list), so "auto" maps to the platform's own auto-routing address instead.
    let mut server = settings.string(c"server");
    if server == "auto" || server.is_empty() {
        server = default_server(&name).map(String::from).unwrap_or_default();
    }
    if server.is_empty() || !server.contains("://") {
        server = connect(CONNECT_SERVER_URL);
    }
    let mut key = settings.string(c"key");
    if key.is_empty() {
        key = connect(CONNECT_STREAM_ID);
    }
    Some(Snapshot {
        kind,
        name,
        server,
        key,
        settings: settings.json(),
    })
}

pub fn points_to_bridge(server: &str, port: u64) -> bool {
    let s = server.to_ascii_lowercase();
    s.contains(&format!("127.0.0.1:{port}")) || s.contains(&format!("localhost:{port}"))
}

/// Everything the dashboard shows about OBS. Never includes the stream key.
pub fn info(ingest_port: u64) -> Value {
    let a = api();
    let version = a
        .get_version_string
        .map(|f| cstr(unsafe { f() }))
        .unwrap_or_default();
    // SAFETY: plain query.
    let streaming = a.streaming_active.map(|f| unsafe { f() }).unwrap_or(false);
    let service = snapshot().map(|s| {
        json!({
            "type": s.kind,
            "name": s.name,
            "server": s.server,
            "hasKey": !s.key.is_empty(),
            "viaBridge": points_to_bridge(&s.server, ingest_port),
        })
    });

    let mut output = Value::Null;
    let mut encoder = Value::Null;
    if streaming && let Some(get) = a.get_streaming_output {
        // SAFETY: returns an addref'd output (released below) or NULL.
        let out = unsafe { get() };
        if !out.is_null() {
            // SAFETY: valid output pointer.
            unsafe {
                output = json!({
                    "totalFrames": a.output_get_total_frames.map(|f| f(out)).unwrap_or(0),
                    "droppedFrames": a.output_get_frames_dropped.map(|f| f(out)).unwrap_or(0),
                    "congestion": a.output_get_congestion.map(|f| f(out)).unwrap_or(0.0),
                    "totalBytes": a.output_get_total_bytes.map(|f| f(out)).unwrap_or(0),
                });
                let enc = a
                    .output_get_video_encoder
                    .map(|f| f(out))
                    .unwrap_or(std::ptr::null_mut());
                if !enc.is_null() {
                    let s = Data(
                        a.encoder_get_settings
                            .map(|f| f(enc))
                            .unwrap_or(std::ptr::null_mut()),
                    );
                    encoder = json!({
                        "id": a.encoder_get_id.map(|f| cstr(f(enc))).unwrap_or_default(),
                        "keyintSec": s.int(c"keyint_sec"),
                        "bitrate": s.int(c"bitrate"),
                        "rateControl": s.string(c"rate_control"),
                    });
                }
                if let Some(release) = a.output_release {
                    release(out);
                }
            }
        }
    }
    json!({ "obsVersion": version, "streaming": streaming, "service": service, "output": output, "encoder": encoder })
}

/// Stream settings to copy into Delay Bridge (includes the key — only sent over the private pipe).
pub fn import(ingest_port: u64) -> Result<Value, String> {
    let s = snapshot().ok_or("OBS has no stream settings yet")?;
    if points_to_bridge(&s.server, ingest_port) {
        return Err("OBS already streams through Delay Bridge — nothing to import".into());
    }
    if !s.server.starts_with("rtmp") {
        return Err(format!(
            "OBS's stream type \"{}\" isn't supported — use an RTMP/RTMPS server",
            s.name
        ));
    }
    if s.key.is_empty() {
        return Err("OBS has no stream key set (Settings → Stream)".into());
    }
    Ok(json!({ "name": s.name, "server": s.server, "key": s.key }))
}

fn set_service(kind: &CStr, settings: P) -> Result<(), String> {
    let a = api();
    let (create, set, save, release) = (
        a.service_create.ok_or("unsupported OBS version")?,
        a.set_streaming_service.ok_or("unsupported OBS version")?,
        a.save_streaming_service.ok_or("unsupported OBS version")?,
        a.service_release.ok_or("unsupported OBS version")?,
    );
    // SAFETY: create a service from valid settings, hand it to the frontend (which takes its own
    // reference), persist it, then drop ours.
    unsafe {
        let svc = create(
            kind.as_ptr(),
            c"default_service".as_ptr(),
            settings,
            std::ptr::null_mut(),
        );
        if svc.is_null() {
            return Err("OBS could not create the stream service".into());
        }
        set(svc);
        save();
        release(svc);
    }
    Ok(())
}

fn set_custom(server: &str, key: &str) -> Result<(), String> {
    let a = api();
    let (create, set_str, set_bool) = (
        a.data_create.ok_or("unsupported OBS version")?,
        a.data_set_string.ok_or("unsupported OBS version")?,
        a.data_set_bool.ok_or("unsupported OBS version")?,
    );
    let (server, key) = (
        CString::new(server).map_err(|e| e.to_string())?,
        CString::new(key).map_err(|e| e.to_string())?,
    );
    // SAFETY: building a fresh obs_data_t that we release via `Data`.
    let data = Data(unsafe { create() });
    unsafe {
        set_str(data.0, c"server".as_ptr(), server.as_ptr());
        set_str(data.0, c"key".as_ptr(), key.as_ptr());
        set_bool(data.0, c"use_auth".as_ptr(), false);
        set_bool(data.0, c"bwtest".as_ptr(), false);
    }
    set_service(c"rtmp_custom", data.0)
}

/// Point OBS at Delay Bridge. Returns what OBS was using, so Delay Bridge can stream there and
/// restore it later. Must run on the OBS UI thread.
pub fn use_bridge(ingest_url: &str, ingest_key: &str, ingest_port: u64) -> Result<Value, String> {
    let s = snapshot().ok_or("OBS has no stream settings yet")?;
    if points_to_bridge(&s.server, ingest_port) {
        return Ok(json!({ "already": true }));
    }
    if !s.server.starts_with("rtmp") {
        return Err(format!(
            "OBS's stream type \"{}\" isn't supported — use an RTMP/RTMPS server",
            s.name
        ));
    }
    if s.key.is_empty() {
        return Err("Set your stream key in OBS (Settings → Stream) first, or add it in Delay Bridge's Settings".into());
    }
    set_custom(ingest_url, ingest_key)?;
    Ok(json!({
        "already": false,
        "name": s.name,
        "server": s.server,
        "key": s.key,
        "original": { "type": s.kind, "settings": s.settings },
    }))
}

/// Put back the service OBS used before (or a custom server). Must run on the OBS UI thread.
pub fn restore(args: &Value) -> Result<Value, String> {
    if let Some(orig) = args.get("original").filter(|o| o.is_object()) {
        let kind = orig["type"].as_str().unwrap_or("rtmp_common");
        let settings = serde_json::to_string(&orig["settings"]).map_err(|e| e.to_string())?;
        let a = api();
        let from_json = a.data_create_from_json.ok_or("unsupported OBS version")?;
        let (kind_c, json_c) = (
            CString::new(kind).map_err(|e| e.to_string())?,
            CString::new(settings).map_err(|e| e.to_string())?,
        );
        // SAFETY: creating obs_data_t from JSON; released by `Data`.
        let data = Data(unsafe { from_json(json_c.as_ptr()) });
        set_service(&kind_c, data.0)?;
    } else {
        let server = args["server"].as_str().ok_or("nothing to restore")?;
        let key = args["key"].as_str().unwrap_or("");
        set_custom(server, key)?;
    }
    Ok(json!({ "restored": true }))
}
