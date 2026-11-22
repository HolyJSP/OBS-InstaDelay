//! Shared RTMP connection pieces: handshake, message reader, control/command builders.

use super::chunk::{ChunkReader, ChunkWriter, Message};
use crate::amf0::{self, Amf};
use std::io;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

pub trait Io: AsyncRead + AsyncWrite + Unpin + Send {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send> Io for T {}

const HS: usize = 1536;

fn random_fill(buf: &mut [u8]) {
    // Handshake bytes only need to be unpredictable-ish, not cryptographic.
    let mut x = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0x9e3779b97f4a7c15)
        | 1;
    for b in buf.iter_mut() {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        *b = x as u8;
    }
}

pub async fn handshake_server<S: AsyncRead + AsyncWrite + Unpin>(s: &mut S) -> io::Result<()> {
    let mut c0c1 = vec![0u8; 1 + HS];
    s.read_exact(&mut c0c1).await?;
    let mut out = Vec::with_capacity(1 + HS * 2);
    out.push(3);
    let mut s1 = vec![0u8; HS];
    random_fill(&mut s1[8..]);
    out.extend_from_slice(&s1);
    out.extend_from_slice(&c0c1[1..]);
    s.write_all(&out).await?;
    let mut c2 = vec![0u8; HS];
    s.read_exact(&mut c2).await?;
    Ok(())
}

pub async fn handshake_client<S: AsyncRead + AsyncWrite + Unpin>(s: &mut S) -> io::Result<()> {
    let mut c0c1 = vec![0u8; 1 + HS];
    c0c1[0] = 3;
    random_fill(&mut c0c1[9..]);
    s.write_all(&c0c1).await?;
    let mut s012 = vec![0u8; 1 + HS * 2];
    s.read_exact(&mut s012).await?;
    s.write_all(&s012[1..1 + HS]).await?;
    Ok(())
}

pub struct RtmpReader<R> {
    r: R,
    cr: ChunkReader,
    tmp: Box<[u8]>,
    pub peer_window: u32,
    last_ack: u64,
}

impl<R: AsyncRead + Unpin> RtmpReader<R> {
    pub fn new(r: R) -> Self {
        RtmpReader {
            r,
            cr: ChunkReader::new(),
            tmp: vec![0u8; 64 * 1024].into_boxed_slice(),
            peer_window: 0,
            last_ack: 0,
        }
    }

    /// Next complete message. Protocol-level state (chunk size, abort, window) is applied here.
    pub async fn read(&mut self) -> io::Result<Message> {
        loop {
            match self.cr.next_message() {
                Ok(Some(m)) => {
                    match m.type_id {
                        1 if m.payload.len() >= 4 => {
                            let n = u32::from_be_bytes(m.payload[..4].try_into().unwrap())
                                & 0x7fff_ffff;
                            self.cr.chunk_size = (n as usize).max(1);
                        }
                        2 if m.payload.len() >= 4 => {
                            self.cr
                                .abort(u32::from_be_bytes(m.payload[..4].try_into().unwrap()));
                        }
                        5 if m.payload.len() >= 4 => {
                            self.peer_window =
                                u32::from_be_bytes(m.payload[..4].try_into().unwrap());
                        }
                        _ => {}
                    }
                    return Ok(m);
                }
                Ok(None) => {}
                Err(e) => return Err(io::Error::new(io::ErrorKind::InvalidData, e)),
            }
            let n = self.r.read(&mut self.tmp).await?;
            if n == 0 {
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "connection closed",
                ));
            }
            self.cr.push(&self.tmp[..n]);
        }
    }

    /// If the peer asked for acknowledgements and enough bytes arrived, returns the ack value.
    pub fn take_ack(&mut self) -> Option<u32> {
        if self.peer_window > 0 && self.cr.bytes_in - self.last_ack >= self.peer_window as u64 {
            self.last_ack = self.cr.bytes_in;
            Some(self.cr.bytes_in as u32)
        } else {
            None
        }
    }
}

// ---- message builders (append encoded chunks to `out`) -------------------------------

pub fn control(w: &ChunkWriter, out: &mut Vec<u8>, type_id: u8, payload: &[u8]) {
    w.write(out, 2, type_id, 0, 0, payload);
}

pub fn set_chunk_size(w: &mut ChunkWriter, out: &mut Vec<u8>, n: u32) {
    control(w, out, 1, &n.to_be_bytes());
    w.chunk_size = n as usize;
}

pub fn window_ack(w: &ChunkWriter, out: &mut Vec<u8>, n: u32) {
    control(w, out, 5, &n.to_be_bytes());
}

pub fn peer_bandwidth(w: &ChunkWriter, out: &mut Vec<u8>, n: u32) {
    let mut p = n.to_be_bytes().to_vec();
    p.push(2);
    control(w, out, 6, &p);
}

pub fn ack(w: &ChunkWriter, out: &mut Vec<u8>, n: u32) {
    control(w, out, 3, &n.to_be_bytes());
}

pub fn user_control(w: &ChunkWriter, out: &mut Vec<u8>, event: u16, data: &[u8]) {
    let mut p = event.to_be_bytes().to_vec();
    p.extend_from_slice(data);
    control(w, out, 4, &p);
}

pub fn command(w: &ChunkWriter, out: &mut Vec<u8>, csid: u32, stream_id: u32, values: &[Amf]) {
    w.write(out, csid, 20, stream_id, 0, &amf0::encode(values));
}

/// If this is a ping request (user control 6), returns the matching pong payload.
pub fn ping_reply(m: &Message) -> Option<[u8; 4]> {
    if m.type_id == 4
        && m.payload.len() >= 6
        && u16::from_be_bytes([m.payload[0], m.payload[1]]) == 6
    {
        Some(m.payload[2..6].try_into().unwrap())
    } else {
        None
    }
}

pub struct Command {
    pub name: String,
    pub txn: f64,
    /// Values after the transaction id (command object, then arguments).
    pub rest: Vec<Amf>,
    pub stream_id: u32,
}

impl Command {
    pub fn arg(&self, i: usize) -> Option<&Amf> {
        self.rest.get(i + 1)
    }
}

pub fn parse_command(m: &Message) -> Option<Command> {
    let body: &[u8] = match m.type_id {
        20 => &m.payload,
        17 if !m.payload.is_empty() => &m.payload[1..],
        _ => return None,
    };
    let mut vals = amf0::decode(body).ok()?.into_iter();
    let name = vals.next()?.as_str()?.to_string();
    let txn = vals.next().and_then(|v| v.as_f64()).unwrap_or(0.0);
    Some(Command {
        name,
        txn,
        rest: vals.collect(),
        stream_id: m.stream_id,
    })
}
