//! Delay Bridge's side of the private channel to the OBS plugin (see obs-plugin/src/link.rs).
//!
//! When the plugin starts us, our stdout carries JSON requests to the plugin and our stdin
//! carries its replies. stdin closing means OBS has gone away.

use serde_json::{Value, json};
use std::collections::HashMap;
use std::io::{BufRead, Write};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::oneshot;

pub struct PluginLink {
    pending: Mutex<HashMap<u64, oneshot::Sender<Result<Value, String>>>>,
    next: AtomicU64,
    alive: AtomicBool,
    out: Mutex<std::io::Stdout>,
}

impl PluginLink {
    /// Start reading replies from stdin. `on_gone` fires when the pipe closes (OBS exited).
    pub fn start(on_gone: oneshot::Sender<()>) -> Arc<PluginLink> {
        let link = Arc::new(PluginLink {
            pending: Mutex::new(HashMap::new()),
            next: AtomicU64::new(1),
            alive: AtomicBool::new(true),
            out: Mutex::new(std::io::stdout()),
        });
        let l = link.clone();
        std::thread::spawn(move || {
            for line in std::io::stdin().lock().lines() {
                let Ok(line) = line else { break };
                let Ok(v) = serde_json::from_str::<Value>(&line) else {
                    continue;
                };
                let Some(id) = v["id"].as_u64() else { continue };
                let tx = l.pending.lock().unwrap().remove(&id);
                if let Some(tx) = tx {
                    let r = if v["ok"].as_bool() == Some(true) {
                        Ok(v["data"].clone())
                    } else {
                        Err(v["error"]
                            .as_str()
                            .unwrap_or("OBS plugin error")
                            .to_string())
                    };
                    let _ = tx.send(r);
                }
            }
            l.alive.store(false, Ordering::SeqCst);
            for (_, tx) in l.pending.lock().unwrap().drain() {
                let _ = tx.send(Err("OBS closed".into()));
            }
            let _ = on_gone.send(());
        });
        link
    }

    pub fn alive(&self) -> bool {
        self.alive.load(Ordering::SeqCst)
    }

    pub async fn call(&self, cmd: &str, args: Value, timeout: Duration) -> Result<Value, String> {
        if !self.alive() {
            return Err("OBS is not running".into());
        }
        let id = self.next.fetch_add(1, Ordering::SeqCst);
        let (tx, rx) = oneshot::channel();
        self.pending.lock().unwrap().insert(id, tx);
        {
            let mut out = self.out.lock().unwrap();
            let line = json!({ "id": id, "cmd": cmd, "args": args });
            if writeln!(out, "{line}").and_then(|_| out.flush()).is_err() {
                self.pending.lock().unwrap().remove(&id);
                return Err("lost connection to the OBS plugin".into());
            }
        }
        match tokio::time::timeout(timeout, rx).await {
            Ok(Ok(r)) => r,
            Ok(Err(_)) => Err("OBS plugin did not answer".into()),
            Err(_) => {
                self.pending.lock().unwrap().remove(&id);
                Err("OBS plugin did not answer in time".into())
            }
        }
    }
}
