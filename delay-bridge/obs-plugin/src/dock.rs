//! Adds the Delay Bridge dashboard to OBS as a browser dock (Docks → Delay Bridge).
//!
//! OBS keeps "Custom Browser Docks" as a JSON list in its config (`[BasicWindow]
//! ExtraBrowserDocks`) and creates them after all plugins have loaded, so adding an entry here
//! makes OBS create the dock itself. It's added once; if you remove it, it stays removed.

use crate::api;
use crate::ffi::cstr;
use serde_json::{Value, json};
use std::ffi::CString;
use std::path::Path;

fn uuid() -> String {
    let mut x = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(1)
        ^ ((std::process::id() as u64) << 32);
    let mut hex = String::new();
    for _ in 0..32 {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        hex.push(char::from_digit((x & 0xf) as u32, 16).unwrap_or('0'));
    }
    format!(
        "{}-{}-4{}-a{}-{}",
        &hex[0..8],
        &hex[8..12],
        &hex[13..16],
        &hex[17..20],
        &hex[20..32]
    )
}

/// Returns true if the dock exists (added now or earlier).
pub fn ensure(port: u16, state_path: &Path) -> bool {
    let a = api();
    let (Some(get), Some(set)) = (a.config_get_string, a.config_set_string) else {
        return false;
    };
    // OBS 31+ keeps docks in the user config; earlier versions in the global config.
    // SAFETY: plain getters returning OBS-owned config handles.
    let cfg = unsafe {
        match (a.get_user_config, a.get_global_config) {
            (Some(f), _) => f(),
            (None, Some(f)) => f(),
            _ => return false,
        }
    };
    if cfg.is_null() {
        return false;
    }
    // SAFETY: valid config handle and section/key names.
    let current = cstr(unsafe { get(cfg, c"BasicWindow".as_ptr(), c"ExtraBrowserDocks".as_ptr()) });
    let mut docks: Vec<Value> = serde_json::from_str(&current).unwrap_or_default();
    let prefix = format!("http://127.0.0.1:{port}");
    if docks
        .iter()
        .any(|d| d["url"].as_str().is_some_and(|u| u.starts_with(&prefix)))
    {
        return true;
    }

    let mut state: Value = std::fs::read_to_string(state_path)
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or(json!({}));
    if state["dockAdded"].as_bool() == Some(true) {
        return false; // the user removed it — respect that
    }
    docks.push(json!({ "title": "Delay Bridge", "url": format!("{prefix}/"), "uuid": uuid() }));
    let Ok(value) = CString::new(Value::Array(docks).to_string()) else {
        return false;
    };
    // SAFETY: valid config handle and strings; OBS copies the value.
    unsafe {
        set(
            cfg,
            c"BasicWindow".as_ptr(),
            c"ExtraBrowserDocks".as_ptr(),
            value.as_ptr(),
        );
        if let Some(save) = a.config_save_safe {
            save(cfg, c"tmp".as_ptr(), std::ptr::null());
        }
    }
    state["dockAdded"] = json!(true);
    let _ = std::fs::write(state_path, state.to_string());
    crate::log("added the Delay Bridge dock (Docks → Delay Bridge)");
    true
}
