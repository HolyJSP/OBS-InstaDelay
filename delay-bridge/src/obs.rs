//! Minimal obs-websocket v5 client (built into OBS 28+): scene switching for overlay mode and
//! a hotkey trigger (showing/hiding a named source toggles the delay).

use crate::Logger;
use base64::Engine as _;
use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinHandle;
use tokio_tungstenite::tungstenite::Message;

const SUB_SCENE_ITEMS: u32 = 1 << 7;

#[derive(Default)]
pub struct ObsShared {
    pub connected: bool,
    pub error: Option<String>,
}

struct Req {
    typ: String,
    data: Value,
    reply: oneshot::Sender<Result<Value, String>>,
}

pub struct ObsHandle {
    req: mpsc::Sender<Req>,
    pub shared: Arc<Mutex<ObsShared>>,
    task: JoinHandle<()>,
}

impl Drop for ObsHandle {
    fn drop(&mut self) {
        self.task.abort();
    }
}

fn sha_b64(s: &str) -> String {
    base64::engine::general_purpose::STANDARD.encode(Sha256::digest(s.as_bytes()))
}

impl ObsHandle {
    pub fn spawn(
        url: String,
        password: String,
        trigger_source: String,
        trigger: mpsc::Sender<()>,
        log: Logger,
    ) -> Self {
        let (req, rx) = mpsc::channel(16);
        let shared = Arc::new(Mutex::new(ObsShared::default()));
        let task = tokio::spawn(run(
            url,
            password,
            trigger_source,
            trigger,
            log,
            rx,
            shared.clone(),
        ));
        ObsHandle { req, shared, task }
    }

    pub fn connected(&self) -> bool {
        self.shared.lock().unwrap().connected
    }

    pub fn error(&self) -> Option<String> {
        self.shared.lock().unwrap().error.clone()
    }

    pub async fn request(&self, typ: &str, data: Value) -> Result<Value, String> {
        let (tx, rx) = oneshot::channel();
        self.req
            .send(Req {
                typ: typ.into(),
                data,
                reply: tx,
            })
            .await
            .map_err(|_| "OBS client stopped".to_string())?;
        tokio::time::timeout(Duration::from_secs(5), rx)
            .await
            .map_err(|_| format!("OBS did not answer {typ}"))?
            .map_err(|_| "OBS disconnected".to_string())?
    }

    pub async fn current_scene(&self) -> Result<String, String> {
        let r = self.request("GetCurrentProgramScene", json!({})).await?;
        r.get("currentProgramSceneName")
            .or_else(|| r.get("sceneName"))
            .and_then(|v| v.as_str())
            .map(String::from)
            .ok_or_else(|| "OBS returned no scene".to_string())
    }

    pub async fn set_scene(&self, name: &str) -> Result<(), String> {
        self.request("SetCurrentProgramScene", json!({ "sceneName": name }))
            .await
            .map(|_| ())
    }

    pub async fn scene_names(&self) -> Result<Vec<String>, String> {
        let r = self.request("GetSceneList", json!({})).await?;
        let mut names: Vec<String> = r
            .get("scenes")
            .and_then(|s| s.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|s| {
                        s.get("sceneName")
                            .and_then(|n| n.as_str())
                            .map(String::from)
                    })
                    .collect()
            })
            .unwrap_or_default();
        names.reverse();
        Ok(names)
    }
}

async fn run(
    url: String,
    password: String,
    trigger_source: String,
    trigger: mpsc::Sender<()>,
    log: Logger,
    mut reqs: mpsc::Receiver<Req>,
    shared: Arc<Mutex<ObsShared>>,
) {
    loop {
        match tokio_tungstenite::connect_async(url.as_str()).await {
            Err(_) => {
                shared.lock().unwrap().error = Some(format!(
                    "Could not reach OBS at {url} — is the WebSocket server enabled?"
                ));
            }
            Ok((ws, _)) => {
                let (mut sink, mut stream) = ws.split();
                let mut pending: HashMap<String, oneshot::Sender<Result<Value, String>>> =
                    HashMap::new();
                let mut trig_pending: HashMap<String, (String, i64)> = HashMap::new();
                let mut trigger_ids: HashMap<String, Option<i64>> = HashMap::new();
                let mut next_id: u64 = 1;
                let mut identified = false;

                loop {
                    tokio::select! {
                        msg = stream.next() => {
                            let text = match msg {
                                Some(Ok(Message::Text(t))) => t.as_str().to_string(),
                                Some(Ok(Message::Close(frame))) => {
                                    if frame.map(|f| u16::from(f.code) == 4009).unwrap_or(false) {
                                        shared.lock().unwrap().error = Some("OBS rejected the WebSocket password".into());
                                    }
                                    break;
                                }
                                Some(Ok(_)) => continue,
                                _ => break,
                            };
                            let Ok(v) = serde_json::from_str::<Value>(&text) else { continue };
                            let d = &v["d"];
                            match v["op"].as_u64() {
                                Some(0) => {
                                    let mut identify = json!({
                                        "rpcVersion": 1,
                                        "eventSubscriptions": if trigger_source.is_empty() { 0 } else { SUB_SCENE_ITEMS },
                                    });
                                    if let (Some(salt), Some(ch)) = (d["authentication"]["salt"].as_str(), d["authentication"]["challenge"].as_str()) {
                                        let secret = sha_b64(&format!("{password}{salt}"));
                                        identify["authentication"] = json!(sha_b64(&format!("{secret}{ch}")));
                                    }
                                    let _ = sink.send(Message::Text(json!({"op": 1, "d": identify}).to_string().into())).await;
                                }
                                Some(2) => {
                                    identified = true;
                                    trigger_ids.clear();
                                    *shared.lock().unwrap() = ObsShared { connected: true, error: None };
                                    log("Connected to OBS WebSocket".into());
                                }
                                Some(5) if d["eventType"] == "SceneItemEnableStateChanged" && !trigger_source.is_empty() => {
                                    let scene = d["eventData"]["sceneName"].as_str().unwrap_or("").to_string();
                                    let item = d["eventData"]["sceneItemId"].as_i64().unwrap_or(-1);
                                    match trigger_ids.get(&scene) {
                                        Some(Some(id)) => { if *id == item { let _ = trigger.try_send(()); } }
                                        Some(None) => {}
                                        None => {
                                            let rid = format!("trig-{next_id}");
                                            next_id += 1;
                                            trig_pending.insert(rid.clone(), (scene.clone(), item));
                                            let m = json!({"op": 6, "d": {"requestType": "GetSceneItemId", "requestId": rid,
                                                "requestData": {"sceneName": scene, "sourceName": trigger_source}}});
                                            let _ = sink.send(Message::Text(m.to_string().into())).await;
                                        }
                                    }
                                }
                                Some(7) => {
                                    let rid = d["requestId"].as_str().unwrap_or("").to_string();
                                    let ok = d["requestStatus"]["result"].as_bool().unwrap_or(false);
                                    if let Some((scene, item)) = trig_pending.remove(&rid) {
                                        let id = if ok { d["responseData"]["sceneItemId"].as_i64() } else { None };
                                        trigger_ids.insert(scene, id);
                                        if id == Some(item) { let _ = trigger.try_send(()); }
                                    } else if let Some(tx) = pending.remove(&rid) {
                                        let r = if ok {
                                            Ok(d.get("responseData").cloned().unwrap_or(json!({})))
                                        } else {
                                            Err(d["requestStatus"]["comment"].as_str().map(String::from)
                                                .unwrap_or_else(|| format!("{} failed", d["requestType"].as_str().unwrap_or("request"))))
                                        };
                                        let _ = tx.send(r);
                                    }
                                }
                                _ => {}
                            }
                        }
                        r = reqs.recv() => {
                            let Some(r) = r else { return };
                            if !identified {
                                let _ = r.reply.send(Err("Not connected to OBS".into()));
                                continue;
                            }
                            let rid = format!("r-{next_id}");
                            next_id += 1;
                            let m = json!({"op": 6, "d": {"requestType": r.typ, "requestId": rid, "requestData": r.data}});
                            if sink.send(Message::Text(m.to_string().into())).await.is_ok() {
                                pending.insert(rid, r.reply);
                            } else {
                                let _ = r.reply.send(Err("OBS disconnected".into()));
                            }
                        }
                    }
                }
                for (_, tx) in pending.drain() {
                    let _ = tx.send(Err("OBS disconnected".into()));
                }
                let was = std::mem::replace(&mut shared.lock().unwrap().connected, false);
                if was {
                    log("Lost connection to OBS WebSocket".into());
                }
            }
        }
        // Retry in 5 s; answer requests meanwhile.
        let sleep = tokio::time::sleep(Duration::from_secs(5));
        tokio::pin!(sleep);
        loop {
            tokio::select! {
                _ = &mut sleep => break,
                r = reqs.recv() => match r {
                    None => return,
                    Some(r) => { let _ = r.reply.send(Err("Not connected to OBS".into())); }
                }
            }
        }
    }
}
