//! RTMP / RTMPS publisher: pushes the (delayed) stream to Twitch, YouTube, Kick…

use super::chunk::ChunkWriter;
use super::conn::{self, Io, RtmpReader};
use crate::amf0::Amf;
use crate::media::{Kind, OutPacket};
use std::io;
use std::sync::{Arc, OnceLock};
use std::time::Duration;
use tokio::io::{AsyncWriteExt, ReadHalf, WriteHalf};
use tokio::net::TcpStream;
use tokio::sync::mpsc;

#[derive(Debug, Clone, PartialEq)]
pub struct Target {
    pub tls: bool,
    pub host: String,
    pub port: u16,
    pub app: String,
    pub tc_url: String,
    pub key: String,
}

pub fn parse_url(url: &str, key: &str) -> Result<Target, String> {
    let url = url.trim();
    let (scheme, rest) = url
        .split_once("://")
        .ok_or("Server URL must start with rtmp:// or rtmps://")?;
    let tls = match scheme.to_ascii_lowercase().as_str() {
        "rtmp" => false,
        "rtmps" => true,
        other => {
            return Err(format!(
                "Unsupported protocol \"{other}\" (use rtmp:// or rtmps://)"
            ));
        }
    };
    let (hostport, path) = rest.split_once('/').unwrap_or((rest, ""));
    let (host, port) = match hostport.rsplit_once(':') {
        Some((h, p)) if !h.contains(']') || h.ends_with(']') => (
            h.to_string(),
            p.parse::<u16>()
                .map_err(|_| format!("Invalid port \"{p}\""))?,
        ),
        _ => (hostport.to_string(), if tls { 443 } else { 1935 }),
    };
    if host.is_empty() {
        return Err("Server URL has no host".into());
    }
    let (path, query) = match path.split_once('?') {
        Some((p, q)) => (p, format!("?{q}")),
        None => (path, String::new()),
    };
    let mut segments: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
    let mut key = key.trim().to_string();
    if key.is_empty() && segments.len() > 1 {
        key = segments.pop().unwrap().to_string(); // key pasted into the URL
    }
    let app = format!("{}{}", segments.join("/"), query);
    let explicit_port = hostport
        .rsplit_once(':')
        .map(|(_, p)| format!(":{p}"))
        .unwrap_or_default();
    let tc_url = format!("{scheme}://{host}{explicit_port}/{app}");
    Ok(Target {
        tls,
        host,
        port,
        app,
        tc_url,
        key,
    })
}

fn tls_config() -> Arc<rustls::ClientConfig> {
    static CFG: OnceLock<Arc<rustls::ClientConfig>> = OnceLock::new();
    CFG.get_or_init(|| {
        let mut roots = rustls::RootCertStore::empty();
        roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
        let cfg = rustls::ClientConfig::builder_with_provider(Arc::new(
            rustls::crypto::ring::default_provider(),
        ))
        .with_safe_default_protocol_versions()
        .expect("TLS setup")
        .with_root_certificates(roots)
        .with_no_client_auth();
        Arc::new(cfg)
    })
    .clone()
}

type Stream = Box<dyn Io>;

pub struct Publisher {
    wr: WriteHalf<Stream>,
    w: ChunkWriter,
    stream_id: u32,
    key: String,
    buf: Vec<u8>,
    /// Control replies from the reader task (pongs, acks). Closed when the server disconnects.
    pub ctrl: mpsc::Receiver<Vec<u8>>,
}

fn err(msg: impl Into<String>) -> io::Error {
    io::Error::other(msg.into())
}

pub async fn connect(target: &Target) -> io::Result<Publisher> {
    tokio::time::timeout(Duration::from_secs(12), connect_inner(target))
        .await
        .map_err(|_| err("Timed out waiting for the server to accept the stream"))?
}

async fn connect_inner(t: &Target) -> io::Result<Publisher> {
    if t.key.is_empty() {
        return Err(err("No stream key — add it in Settings"));
    }
    let tcp = TcpStream::connect((t.host.as_str(), t.port)).await?;
    tcp.set_nodelay(true)?;
    let mut stream: Stream = if t.tls {
        let name = rustls::pki_types::ServerName::try_from(t.host.clone())
            .map_err(|_| err("Invalid host name"))?;
        let tls = tokio_rustls::TlsConnector::from(tls_config())
            .connect(name, tcp)
            .await?;
        Box::new(tls)
    } else {
        Box::new(tcp)
    };
    conn::handshake_client(&mut stream).await?;
    let (rd, mut wr) = tokio::io::split(stream);
    let mut reader = RtmpReader::new(rd);
    let mut w = ChunkWriter::default();
    let mut out = Vec::with_capacity(1024);

    conn::set_chunk_size(&mut w, &mut out, 4096);
    conn::command(
        &w,
        &mut out,
        3,
        0,
        &[
            Amf::str("connect"),
            Amf::Number(1.0),
            Amf::obj(&[
                ("app", Amf::str(&t.app)),
                ("type", Amf::str("nonprivate")),
                ("flashVer", Amf::str("FMLE/3.0 (compatible; FMSc/1.0)")),
                ("swfUrl", Amf::str(&t.tc_url)),
                ("tcUrl", Amf::str(&t.tc_url)),
            ]),
        ],
    );
    wr.write_all(&out).await?;

    let mut stream_id = 1u32;
    loop {
        let m = reader.read().await?;
        out.clear();
        if let Some(n) = reader.take_ack() {
            conn::ack(&w, &mut out, n);
        }
        if let Some(p) = conn::ping_reply(&m) {
            conn::user_control(&w, &mut out, 7, &p);
        }
        if let Some(c) = conn::parse_command(&m) {
            match c.name.as_str() {
                "_result" if c.txn == 1.0 => {
                    conn::command(
                        &w,
                        &mut out,
                        3,
                        0,
                        &[
                            Amf::str("releaseStream"),
                            Amf::Number(2.0),
                            Amf::Null,
                            Amf::str(&t.key),
                        ],
                    );
                    conn::command(
                        &w,
                        &mut out,
                        3,
                        0,
                        &[
                            Amf::str("FCPublish"),
                            Amf::Number(3.0),
                            Amf::Null,
                            Amf::str(&t.key),
                        ],
                    );
                    conn::command(
                        &w,
                        &mut out,
                        3,
                        0,
                        &[Amf::str("createStream"), Amf::Number(4.0), Amf::Null],
                    );
                }
                "_result" if c.txn == 4.0 => {
                    stream_id = c.arg(0).and_then(|a| a.as_f64()).unwrap_or(1.0) as u32;
                    conn::command(
                        &w,
                        &mut out,
                        4,
                        stream_id,
                        &[
                            Amf::str("publish"),
                            Amf::Number(5.0),
                            Amf::Null,
                            Amf::str(&t.key),
                            Amf::str("live"),
                        ],
                    );
                }
                "_error" => {
                    let info = c.arg(0);
                    let d = info
                        .and_then(|i| i.get("description").or(i.get("code")))
                        .and_then(|s| s.as_str())
                        .unwrap_or("Server rejected the connection");
                    return Err(err(d.to_string()));
                }
                "onStatus" => {
                    let info = c.arg(0);
                    let code = info
                        .and_then(|i| i.get("code"))
                        .and_then(|s| s.as_str())
                        .unwrap_or("");
                    let level = info
                        .and_then(|i| i.get("level"))
                        .and_then(|s| s.as_str())
                        .unwrap_or("");
                    if code == "NetStream.Publish.Start" {
                        if !out.is_empty() {
                            wr.write_all(&out).await?;
                        }
                        break;
                    }
                    if level == "error" {
                        let d = info
                            .and_then(|i| i.get("description"))
                            .and_then(|s| s.as_str())
                            .unwrap_or(code);
                        return Err(err(format!("Publish failed: {d}")));
                    }
                }
                _ => {}
            }
        }
        if !out.is_empty() {
            wr.write_all(&out).await?;
        }
    }

    // Background reader: answers pings/acks; closing `ctrl` signals a disconnect.
    let (ctrl_tx, ctrl) = mpsc::channel(16);
    tokio::spawn(read_loop(reader, ctrl_tx));
    Ok(Publisher {
        wr,
        w,
        stream_id,
        key: t.key.clone(),
        buf: Vec::with_capacity(256 * 1024),
        ctrl,
    })
}

async fn read_loop(mut reader: RtmpReader<ReadHalf<Stream>>, tx: mpsc::Sender<Vec<u8>>) {
    let w = ChunkWriter { chunk_size: 4096 };
    loop {
        let m = tokio::select! {
            r = reader.read() => match r { Ok(m) => m, Err(_) => return },
            _ = tx.closed() => return,
        };
        let mut out = Vec::new();
        if let Some(n) = reader.take_ack() {
            conn::ack(&w, &mut out, n);
        }
        if let Some(p) = conn::ping_reply(&m) {
            conn::user_control(&w, &mut out, 7, &p);
        }
        if let Some(c) = conn::parse_command(&m) {
            let level = c
                .arg(0)
                .and_then(|i| i.get("level"))
                .and_then(|s| s.as_str())
                .unwrap_or("");
            if c.name == "onStatus" && level == "error" {
                return; // server ended the publish
            }
        }
        if !out.is_empty() && tx.send(out).await.is_err() {
            return;
        }
    }
}

impl Publisher {
    /// Queue a packet into the outgoing buffer (call `flush` to send).
    pub fn queue(&mut self, p: &OutPacket, ts: u32) {
        let (csid, type_id) = match p.kind {
            Kind::Audio => (4, 8),
            Kind::Video => (6, 9),
            Kind::Data => (5, 18),
        };
        self.w
            .write(&mut self.buf, csid, type_id, self.stream_id, ts, &p.data);
    }

    pub fn queue_raw(&mut self, bytes: &[u8]) {
        self.buf.extend_from_slice(bytes);
    }

    pub fn pending_bytes(&self) -> usize {
        self.buf.len()
    }

    pub async fn flush(&mut self) -> io::Result<usize> {
        if self.buf.is_empty() {
            return Ok(0);
        }
        self.wr.write_all(&self.buf).await?;
        let n = self.buf.len();
        self.buf.clear();
        if self.buf.capacity() > 4 * 1024 * 1024 {
            self.buf.shrink_to(256 * 1024);
        }
        Ok(n)
    }

    pub async fn close(mut self) {
        let mut out = Vec::new();
        conn::command(
            &self.w,
            &mut out,
            3,
            0,
            &[
                Amf::str("FCUnpublish"),
                Amf::Number(6.0),
                Amf::Null,
                Amf::str(&self.key),
            ],
        );
        conn::command(
            &self.w,
            &mut out,
            3,
            0,
            &[
                Amf::str("deleteStream"),
                Amf::Number(7.0),
                Amf::Null,
                Amf::Number(self.stream_id as f64),
            ],
        );
        let _ = tokio::time::timeout(Duration::from_secs(2), async {
            let _ = self.flush().await;
            let _ = self.wr.write_all(&out).await;
            let _ = self.wr.shutdown().await;
        })
        .await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn urls() {
        let t = parse_url("rtmp://live.twitch.tv/app", "k").unwrap();
        assert_eq!(
            (t.host.as_str(), t.port, t.app.as_str(), t.tls),
            ("live.twitch.tv", 1935, "app", false)
        );
        let t = parse_url("rtmps://x.global-contribute.live-video.net:443/app/", "sk").unwrap();
        assert_eq!(
            (t.port, t.app.as_str(), t.tls, t.tc_url.as_str()),
            (
                443,
                "app",
                true,
                "rtmps://x.global-contribute.live-video.net:443/app"
            )
        );
        let t = parse_url("rtmp://live.twitch.tv/app/live_123", "").unwrap();
        assert_eq!((t.app.as_str(), t.key.as_str()), ("app", "live_123"));
        let t = parse_url("rtmp://a.rtmp.youtube.com/live2", "abc").unwrap();
        assert_eq!(t.app, "live2");
        assert!(parse_url("http://x/app", "k").is_err());
    }
}
