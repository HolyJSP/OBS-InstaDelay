//! Tiny HTTP/1.1 server: the dashboard (also an OBS dock) and the control API used by
//! Stream Deck, Bitfocus Companion, AutoHotkey, etc. Costs nothing while no one is connected.

use crate::app::App;
use serde_json::{Value, json};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

const INDEX_HTML: &str = include_str!("../public/index.html");
const OVERLAY_HTML: &str = include_str!("../public/overlay.html");

struct Request {
    method: String,
    path: String,
    query: HashMap<String, String>,
    headers: HashMap<String, String>,
    body: Vec<u8>,
}

pub async fn serve(listener: TcpListener, app: Arc<App>, host: String, port: u16) {
    let exposed = host != "127.0.0.1" && host != "localhost";
    loop {
        let Ok((sock, _)) = listener.accept().await else {
            continue;
        };
        let app = app.clone();
        tokio::spawn(async move {
            let _ = handle_conn(sock, app, exposed, port).await;
        });
    }
}

async fn handle_conn(
    mut sock: TcpStream,
    app: Arc<App>,
    exposed: bool,
    port: u16,
) -> std::io::Result<()> {
    let mut buf = Vec::with_capacity(4096);
    loop {
        let req =
            match tokio::time::timeout(Duration::from_secs(15), read_request(&mut sock, &mut buf))
                .await
            {
                Ok(Ok(Some(r))) => r,
                _ => return Ok(()),
            };
        let keep_alive = !req
            .headers
            .get("connection")
            .map(|c| c.eq_ignore_ascii_case("close"))
            .unwrap_or(false);
        let (code, ctype, body) = route(&app, req, exposed, port).await;
        let head = format!(
            "HTTP/1.1 {code} {}\r\nContent-Type: {ctype}\r\nContent-Length: {}\r\nCache-Control: no-store\r\nConnection: {}\r\n\r\n",
            reason(code),
            body.len(),
            if keep_alive { "keep-alive" } else { "close" }
        );
        let mut out = head.into_bytes();
        out.extend_from_slice(&body);
        sock.write_all(&out).await?;
        if !keep_alive {
            return Ok(());
        }
    }
}

fn reason(code: u16) -> &'static str {
    match code {
        200 => "OK",
        400 => "Bad Request",
        403 => "Forbidden",
        404 => "Not Found",
        405 => "Method Not Allowed",
        413 => "Payload Too Large",
        415 => "Unsupported Media Type",
        _ => "Error",
    }
}

async fn read_request(sock: &mut TcpStream, buf: &mut Vec<u8>) -> std::io::Result<Option<Request>> {
    let mut tmp = [0u8; 4096];
    let header_end = loop {
        if let Some(i) = find(buf, b"\r\n\r\n") {
            break i;
        }
        if buf.len() > 16 * 1024 {
            return Ok(None);
        }
        let n = sock.read(&mut tmp).await?;
        if n == 0 {
            return Ok(None);
        }
        buf.extend_from_slice(&tmp[..n]);
    };
    let head = String::from_utf8_lossy(&buf[..header_end]).into_owned();
    let mut lines = head.split("\r\n");
    let mut first = lines.next().unwrap_or("").split_whitespace();
    let method = first.next().unwrap_or("").to_string();
    let target = first.next().unwrap_or("/").to_string();
    let mut headers = HashMap::new();
    for l in lines {
        if let Some((k, v)) = l.split_once(':') {
            headers.insert(k.trim().to_ascii_lowercase(), v.trim().to_string());
        }
    }
    let len: usize = headers
        .get("content-length")
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);
    if len > 1_000_000 {
        return Ok(None);
    }
    let start = header_end + 4;
    while buf.len() < start + len {
        let n = sock.read(&mut tmp).await?;
        if n == 0 {
            return Ok(None);
        }
        buf.extend_from_slice(&tmp[..n]);
    }
    let body = buf[start..start + len].to_vec();
    buf.drain(..start + len);
    let (path, qs) = target.split_once('?').unwrap_or((&target, ""));
    Ok(Some(Request {
        method,
        path: path.to_string(),
        query: parse_query(qs),
        headers,
        body,
    }))
}

fn find(h: &[u8], n: &[u8]) -> Option<usize> {
    h.windows(n.len()).position(|w| w == n)
}

fn parse_query(qs: &str) -> HashMap<String, String> {
    qs.split('&')
        .filter(|p| !p.is_empty())
        .map(|p| {
            let (k, v) = p.split_once('=').unwrap_or((p, ""));
            (pct_decode(k), pct_decode(v))
        })
        .collect()
}

fn pct_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'+' => out.push(b' '),
            b'%' if i + 2 < b.len() => {
                match u8::from_str_radix(std::str::from_utf8(&b[i + 1..i + 3]).unwrap_or("zz"), 16)
                {
                    Ok(v) => {
                        out.push(v);
                        i += 2;
                    }
                    Err(_) => out.push(b'%'),
                }
            }
            c => out.push(c),
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

type Resp = (u16, &'static str, Vec<u8>);

fn json_resp(code: u16, v: Value) -> Resp {
    (code, "application/json", v.to_string().into_bytes())
}

fn result(r: Result<Value, String>) -> Resp {
    match r {
        Ok(v) => json_resp(200, v),
        Err(e) => json_resp(400, json!({ "error": e })),
    }
}

async fn route(app: &Arc<App>, req: Request, exposed: bool, port: u16) -> Resp {
    // Block DNS rebinding: only answer to local host names unless deliberately exposed.
    let host = req
        .headers
        .get("host")
        .map(|h| h.to_ascii_lowercase())
        .unwrap_or_default();
    let allowed = [
        format!("127.0.0.1:{port}"),
        format!("localhost:{port}"),
        format!("[::1]:{port}"),
    ];
    if !exposed && !allowed.contains(&host) {
        return json_resp(403, json!({"error": "Forbidden host"}));
    }

    if !req.path.starts_with("/api/") {
        return match req.path.as_str() {
            "/" | "/index.html" => (
                200,
                "text/html; charset=utf-8",
                INDEX_HTML.as_bytes().to_vec(),
            ),
            "/overlay" | "/overlay.html" => (
                200,
                "text/html; charset=utf-8",
                OVERLAY_HTML.as_bytes().to_vec(),
            ),
            _ => (404, "text/plain", b"Not found".to_vec()),
        };
    }

    // Parameters come from the query string (GET, e.g. Stream Deck) or a JSON body (POST).
    let mut params: serde_json::Map<String, Value> = req
        .query
        .iter()
        .map(|(k, v)| (k.clone(), json!(v)))
        .collect();
    let is_post = req.method == "POST";
    if is_post {
        // Requiring JSON forces a CORS preflight, so other web pages can't silently post here.
        if !req
            .headers
            .get("content-type")
            .map(|c| c.contains("application/json"))
            .unwrap_or(false)
        {
            return json_resp(415, json!({"error": "Use Content-Type: application/json"}));
        }
        if !req.body.is_empty() {
            match serde_json::from_slice::<Value>(&req.body) {
                Ok(Value::Object(m)) => params.extend(m),
                Ok(_) => {}
                Err(_) => return json_resp(400, json!({"error": "Invalid JSON"})),
            }
        }
    }
    let num = |k: &str| {
        params.get(k).and_then(|v| {
            v.as_f64()
                .or_else(|| v.as_str().and_then(|s| s.trim().parse().ok()))
        })
    };
    let seconds = num("seconds");
    let minutes = num("minutes").or_else(|| num("durationMinutes"));
    let mode = params
        .get("mode")
        .and_then(|v| v.as_str())
        .map(String::from);

    match req.path.as_str() {
        "/api/status" => json_resp(200, app.status().await),
        "/api/on" | "/api/delay" => result(app.enable(seconds, minutes, mode).await),
        "/api/off" => result(app.disable(mode).await),
        "/api/toggle" => result(app.toggle(seconds, minutes, mode).await),
        "/api/extend" => result(app.extend(minutes.unwrap_or(5.0)).await),
        "/api/clear-timer" => result(app.clear_timer().await),
        "/api/end-now" if is_post => result(app.end_now().await),
        "/api/end-now" => json_resp(405, json!({"error": "POST only"})),
        "/api/config" if is_post => result(app.update_config(Value::Object(params)).await),
        "/api/config" => json_resp(200, app.public_config()),
        "/api/obs/use-bridge" if is_post => result(app.use_from_obs().await),
        "/api/obs/stream-direct" if is_post => result(app.stream_direct().await),
        "/api/obs/import" if is_post => result(app.import_from_obs().await),
        "/api/obs/scenes" => json_resp(200, json!({"scenes": app.obs_scenes().await})),
        _ => json_resp(404, json!({"error": "Unknown endpoint"})),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decode() {
        assert_eq!(pct_decode("Delay%20Toggle+x"), "Delay Toggle x");
        assert_eq!(pct_decode("100%"), "100%");
        let q = parse_query("seconds=60&minutes=20&mode=overlay");
        assert_eq!(q["seconds"], "60");
    }
}
