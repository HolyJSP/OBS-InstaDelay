//! RTMP ingest server: OBS / Streamlabs publishes here instead of directly to the platform.

use super::chunk::ChunkWriter;
use super::conn::{self, RtmpReader};
use crate::Logger;
use crate::amf0::{self, Amf};
use crate::engine::EngineMsg;
use crate::media::{InPacket, Kind};
use bytes::Bytes;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use tokio::io::AsyncWriteExt;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::mpsc;

#[derive(Clone)]
pub struct IngestCtx {
    pub stream_key: Arc<std::sync::Mutex<String>>,
    pub active: Arc<AtomicBool>,
    pub tx: mpsc::Sender<EngineMsg>,
    pub log: Logger,
}

pub async fn serve(listener: TcpListener, ctx: IngestCtx) {
    loop {
        let Ok((sock, _)) = listener.accept().await else {
            continue;
        };
        let ctx = ctx.clone();
        tokio::spawn(async move {
            let _ = sock.set_nodelay(true);
            let mut publishing = false;
            let res = handle(sock, &ctx, &mut publishing).await;
            if publishing {
                (ctx.log)("OBS stopped streaming".into());
                ctx.active.store(false, Ordering::SeqCst);
                let _ = ctx.tx.send(EngineMsg::SourceEnd).await;
            }
            drop(res);
        });
    }
}

async fn handle(
    mut sock: TcpStream,
    ctx: &IngestCtx,
    publishing: &mut bool,
) -> std::io::Result<()> {
    conn::handshake_server(&mut sock).await?;
    let (rd, mut wr) = sock.into_split();
    let mut reader = RtmpReader::new(rd);
    let mut w = ChunkWriter::default();
    let mut out = Vec::with_capacity(4096);
    let mut app = String::new();

    loop {
        let m = reader.read().await?;
        out.clear();
        if let Some(n) = reader.take_ack() {
            conn::ack(&w, &mut out, n);
        }
        if let Some(pong) = conn::ping_reply(&m) {
            conn::user_control(&w, &mut out, 7, &pong);
        }
        match m.type_id {
            8 | 9 if *publishing && m.payload.len() >= 2 => {
                let kind = if m.type_id == 8 {
                    Kind::Audio
                } else {
                    Kind::Video
                };
                if ctx
                    .tx
                    .send(EngineMsg::Packet(InPacket {
                        kind,
                        ts: m.timestamp,
                        data: m.payload,
                    }))
                    .await
                    .is_err()
                {
                    return Ok(());
                }
            }
            18 | 15 if *publishing => {
                let body = if m.type_id == 15 {
                    m.payload.slice(1..)
                } else {
                    m.payload.clone()
                };
                let first = amf0::decode(&body).ok().and_then(|v| v.into_iter().next());
                let data = match first.as_ref().and_then(|v| v.as_str()) {
                    Some("@setDataFrame") => body,
                    Some("onMetaData") => {
                        let mut v = amf0::encode(&[Amf::str("@setDataFrame")]);
                        v.extend_from_slice(&body);
                        Bytes::from(v)
                    }
                    _ => continue,
                };
                let _ = ctx
                    .tx
                    .send(EngineMsg::Packet(InPacket {
                        kind: Kind::Data,
                        ts: m.timestamp,
                        data,
                    }))
                    .await;
            }
            17 | 20 => {
                let Some(c) = conn::parse_command(&m) else {
                    continue;
                };
                match c.name.as_str() {
                    "connect" => {
                        app = c
                            .rest
                            .first()
                            .and_then(|o| o.get("app"))
                            .and_then(|a| a.as_str())
                            .unwrap_or("")
                            .to_string();
                        conn::window_ack(&w, &mut out, 5_000_000);
                        conn::peer_bandwidth(&w, &mut out, 5_000_000);
                        conn::set_chunk_size(&mut w, &mut out, 4096);
                        conn::command(
                            &w,
                            &mut out,
                            3,
                            0,
                            &[
                                Amf::str("_result"),
                                Amf::Number(c.txn),
                                Amf::obj(&[
                                    ("fmsVer", Amf::str("FMS/3,0,1,123")),
                                    ("capabilities", Amf::Number(31.0)),
                                ]),
                                Amf::obj(&[
                                    ("level", Amf::str("status")),
                                    ("code", Amf::str("NetConnection.Connect.Success")),
                                    ("description", Amf::str("Connection succeeded.")),
                                    ("objectEncoding", Amf::Number(0.0)),
                                ]),
                            ],
                        );
                    }
                    "releaseStream" | "FCPublish" if c.txn > 0.0 => {
                        conn::command(
                            &w,
                            &mut out,
                            3,
                            0,
                            &[Amf::str("_result"), Amf::Number(c.txn), Amf::Null],
                        );
                    }
                    "createStream" => {
                        conn::command(
                            &w,
                            &mut out,
                            3,
                            0,
                            &[
                                Amf::str("_result"),
                                Amf::Number(c.txn),
                                Amf::Null,
                                Amf::Number(1.0),
                            ],
                        );
                    }
                    "publish" => {
                        let key = c
                            .arg(0)
                            .and_then(|a| a.as_str())
                            .unwrap_or("")
                            .split('?')
                            .next()
                            .unwrap_or("")
                            .to_string();
                        let sid = if c.stream_id == 0 { 1 } else { c.stream_id };
                        let required = ctx.stream_key.lock().unwrap().clone();
                        let reject = if !required.is_empty() && key != required {
                            Some("wrong stream key")
                        } else if ctx
                            .active
                            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
                            .is_err()
                        {
                            Some("another encoder is already publishing")
                        } else {
                            None
                        };
                        if let Some(why) = reject {
                            (ctx.log)(format!("Rejected an encoder: {why}"));
                            conn::command(
                                &w,
                                &mut out,
                                5,
                                sid,
                                &[
                                    Amf::str("onStatus"),
                                    Amf::Number(0.0),
                                    Amf::Null,
                                    Amf::obj(&[
                                        ("level", Amf::str("error")),
                                        ("code", Amf::str("NetStream.Publish.BadName")),
                                        ("description", Amf::str(why)),
                                    ]),
                                ],
                            );
                            wr.write_all(&out).await?;
                            tokio::time::sleep(std::time::Duration::from_millis(200)).await;
                            return Ok(());
                        }
                        conn::user_control(&w, &mut out, 0, &sid.to_be_bytes());
                        conn::command(
                            &w,
                            &mut out,
                            5,
                            sid,
                            &[
                                Amf::str("onStatus"),
                                Amf::Number(0.0),
                                Amf::Null,
                                Amf::obj(&[
                                    ("level", Amf::str("status")),
                                    ("code", Amf::str("NetStream.Publish.Start")),
                                    ("description", Amf::str("Publishing.")),
                                ]),
                            ],
                        );
                        *publishing = true;
                        (ctx.log)(format!("OBS connected (app \"{app}\")"));
                        let _ = ctx.tx.send(EngineMsg::SourceStart).await;
                    }
                    "FCUnpublish" | "deleteStream" | "closeStream" if *publishing => {
                        if !out.is_empty() {
                            wr.write_all(&out).await?;
                        }
                        return Ok(());
                    }
                    "play" => {
                        conn::command(
                            &w,
                            &mut out,
                            5,
                            1,
                            &[
                                Amf::str("onStatus"),
                                Amf::Number(0.0),
                                Amf::Null,
                                Amf::obj(&[
                                    ("level", Amf::str("error")),
                                    ("code", Amf::str("NetStream.Play.Failed")),
                                    ("description", Amf::str("Playback is not supported")),
                                ]),
                            ],
                        );
                        wr.write_all(&out).await?;
                        return Ok(());
                    }
                    _ => {}
                }
            }
            _ => {}
        }
        if !out.is_empty() {
            wr.write_all(&out).await?;
        }
    }
}
