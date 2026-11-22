//! Private channel between the plugin and Delay Bridge over the child's stdin/stdout.
//!
//! Delay Bridge writes one JSON request per line to its stdout:
//!   {"id": 1, "cmd": "info", "args": {...}}
//! and the plugin answers on Delay Bridge's stdin:
//!   {"id": 1, "ok": true, "data": {...}}   or   {"id": 1, "ok": false, "error": "..."}
//! Nothing here is reachable from the network.

use crate::{obs_info, on_ui};
use serde_json::{Value, json};
use std::io::{BufRead, BufReader, Write};
use std::process::{ChildStdin, ChildStdout};
use std::sync::{Arc, Mutex};

pub type SharedStdin = Arc<Mutex<Option<ChildStdin>>>;

pub fn start(stdout: ChildStdout, stdin: SharedStdin) {
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines() {
            let Ok(line) = line else { break };
            let Ok(req) = serde_json::from_str::<Value>(&line) else {
                continue;
            };
            let id = req["id"].clone();
            let result = handle(req["cmd"].as_str().unwrap_or(""), &req["args"]);
            let reply = match result {
                Ok(data) => json!({ "id": id, "ok": true, "data": data }),
                Err(e) => json!({ "id": id, "ok": false, "error": e }),
            };
            let Ok(mut guard) = stdin.lock() else { break };
            let Some(pipe) = guard.as_mut() else { break };
            if writeln!(pipe, "{reply}")
                .and_then(|_| pipe.flush())
                .is_err()
            {
                break;
            }
        }
    });
}

fn handle(cmd: &str, args: &Value) -> Result<Value, String> {
    let port = args["ingestPort"].as_u64().unwrap_or(1935);
    match cmd {
        "info" => Ok(obs_info::info(port)),
        "import" => obs_info::import(port),
        "use_bridge" => {
            let url = args["ingestUrl"]
                .as_str()
                .unwrap_or("rtmp://127.0.0.1:1935/live")
                .to_string();
            let key = args["ingestKey"]
                .as_str()
                .filter(|k| !k.is_empty())
                .unwrap_or("live")
                .to_string();
            on_ui(move || obs_info::use_bridge(&url, &key, port))
                .unwrap_or_else(|| Err("OBS is busy — try again".into()))
        }
        "restore" => {
            let args = args.clone();
            on_ui(move || obs_info::restore(&args))
                .unwrap_or_else(|| Err("OBS is busy — try again".into()))
        }
        other => Err(format!("unknown command {other}")),
    }
}
