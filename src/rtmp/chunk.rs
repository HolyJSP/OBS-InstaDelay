//! RTMP chunk stream reader / writer.

use bytes::{Buf, Bytes, BytesMut};
use std::collections::HashMap;

const HEADER_LEN: [usize; 4] = [11, 7, 3, 0];
const MAX_MESSAGE: usize = 32 * 1024 * 1024;

#[derive(Debug, Clone)]
pub struct Message {
    pub type_id: u8,
    pub stream_id: u32,
    pub timestamp: u32,
    pub payload: Bytes,
}

#[derive(Default)]
struct CsState {
    ts: u32,
    ts_field: u32,
    length: usize,
    type_id: u8,
    stream_id: u32,
    ext: bool,
    buf: Vec<u8>,
}

pub struct ChunkReader {
    pub chunk_size: usize,
    streams: HashMap<u32, CsState>,
    buf: BytesMut,
    pub bytes_in: u64,
}

impl Default for ChunkReader {
    fn default() -> Self {
        Self::new()
    }
}

impl ChunkReader {
    pub fn new() -> Self {
        ChunkReader {
            chunk_size: 128,
            streams: HashMap::new(),
            buf: BytesMut::with_capacity(64 * 1024),
            bytes_in: 0,
        }
    }

    pub fn push(&mut self, data: &[u8]) {
        self.bytes_in += data.len() as u64;
        self.buf.extend_from_slice(data);
    }

    pub fn abort(&mut self, csid: u32) {
        if let Some(st) = self.streams.get_mut(&csid) {
            st.buf.clear();
        }
    }

    /// Parse buffered chunks until one message is complete. `Ok(None)` = need more data.
    pub fn next_message(&mut self) -> Result<Option<Message>, &'static str> {
        loop {
            match self.parse_chunk()? {
                None => return Ok(None),
                Some(Some(m)) => return Ok(Some(m)),
                Some(None) => continue,
            }
        }
    }

    // Ok(None): incomplete chunk. Ok(Some(None)): consumed a chunk, message not complete yet.
    fn parse_chunk(&mut self) -> Result<Option<Option<Message>>, &'static str> {
        let b = &self.buf[..];
        if b.is_empty() {
            return Ok(None);
        }
        let mut p = 0usize;
        let b0 = b[p];
        p += 1;
        let fmt = (b0 >> 6) as usize;
        let mut csid = (b0 & 0x3f) as u32;
        if csid == 0 {
            if b.len() < p + 1 {
                return Ok(None);
            }
            csid = 64 + b[p] as u32;
            p += 1;
        } else if csid == 1 {
            if b.len() < p + 2 {
                return Ok(None);
            }
            csid = 64 + b[p] as u32 + b[p + 1] as u32 * 256;
            p += 2;
        }
        let hl = HEADER_LEN[fmt];
        if b.len() < p + hl {
            return Ok(None);
        }
        let prev = self.streams.get(&csid);
        let (mut ts_field, mut length, mut type_id, mut stream_id, mut ext, got) = match prev {
            Some(s) => (
                s.ts_field,
                s.length,
                s.type_id,
                s.stream_id,
                s.ext,
                s.buf.len(),
            ),
            None => (0, 0, 0, 0, false, 0),
        };
        let be24 = |i: usize| ((b[i] as u32) << 16) | ((b[i + 1] as u32) << 8) | b[i + 2] as u32;
        if fmt <= 2 {
            ts_field = be24(p);
            ext = ts_field == 0xffffff;
        }
        if fmt <= 1 {
            length = be24(p + 3) as usize;
            type_id = b[p + 6];
        }
        if fmt == 0 {
            stream_id = u32::from_le_bytes([b[p + 7], b[p + 8], b[p + 9], b[p + 10]]);
        }
        p += hl;
        if ext {
            if b.len() < p + 4 {
                return Ok(None);
            }
            let e = u32::from_be_bytes([b[p], b[p + 1], b[p + 2], b[p + 3]]);
            p += 4;
            if fmt <= 2 {
                ts_field = e;
            }
        }
        if length > MAX_MESSAGE {
            return Err("RTMP message too large");
        }
        let take = self.chunk_size.min(length.saturating_sub(got));
        if b.len() < p + take {
            return Ok(None);
        }

        // Commit
        let new_msg = got == 0;
        let st = self.streams.entry(csid).or_default();
        st.ts_field = ts_field;
        st.length = length;
        st.type_id = type_id;
        st.stream_id = stream_id;
        st.ext = ext;
        if new_msg {
            st.ts = if fmt == 0 {
                ts_field
            } else {
                st.ts.wrapping_add(ts_field)
            };
            st.buf.reserve(length);
        }
        st.buf.extend_from_slice(&self.buf[p..p + take]);
        let complete = st.buf.len() >= length;
        let msg = if complete {
            let payload = Bytes::from(std::mem::take(&mut st.buf));
            Some(Message {
                type_id,
                stream_id,
                timestamp: st.ts,
                payload,
            })
        } else {
            None
        };
        self.buf.advance(p + take);
        Ok(Some(msg))
    }
}

fn basic_header(out: &mut Vec<u8>, fmt: u8, csid: u32) {
    if csid < 64 {
        out.push((fmt << 6) | csid as u8);
    } else if csid < 320 {
        out.push(fmt << 6);
        out.push((csid - 64) as u8);
    } else {
        let c = csid - 64;
        out.push((fmt << 6) | 1);
        out.push((c & 0xff) as u8);
        out.push((c >> 8) as u8);
    }
}

pub struct ChunkWriter {
    pub chunk_size: usize,
}

impl Default for ChunkWriter {
    fn default() -> Self {
        ChunkWriter { chunk_size: 128 }
    }
}

impl ChunkWriter {
    pub fn write(
        &self,
        out: &mut Vec<u8>,
        csid: u32,
        type_id: u8,
        stream_id: u32,
        timestamp: u32,
        payload: &[u8],
    ) {
        let ext = timestamp >= 0xffffff;
        basic_header(out, 0, csid);
        let t = if ext { 0xffffff } else { timestamp };
        out.extend_from_slice(&t.to_be_bytes()[1..]);
        out.extend_from_slice(&(payload.len() as u32).to_be_bytes()[1..]);
        out.push(type_id);
        out.extend_from_slice(&stream_id.to_le_bytes());
        if ext {
            out.extend_from_slice(&timestamp.to_be_bytes());
        }
        let mut off = self.chunk_size.min(payload.len());
        out.extend_from_slice(&payload[..off]);
        while off < payload.len() {
            basic_header(out, 3, csid);
            if ext {
                out.extend_from_slice(&timestamp.to_be_bytes());
            }
            let n = self.chunk_size.min(payload.len() - off);
            out.extend_from_slice(&payload[off..off + n]);
            off += n;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn write_then_read() {
        let w = ChunkWriter { chunk_size: 100 };
        let payload: Vec<u8> = (0..1000u32).map(|i| i as u8).collect();
        let mut out = Vec::new();
        w.write(&mut out, 6, 9, 1, 0x1234567, &payload);
        w.write(&mut out, 4, 8, 1, 5, &[1, 2, 3]);
        let mut r = ChunkReader::new();
        r.chunk_size = 100;
        // feed byte by byte to exercise partial parsing
        let mut msgs = Vec::new();
        for byte in out {
            r.push(&[byte]);
            while let Some(m) = r.next_message().unwrap() {
                msgs.push(m);
            }
        }
        assert_eq!(msgs.len(), 2);
        assert_eq!(&msgs[0].payload[..], &payload[..]);
        assert_eq!(msgs[0].timestamp, 0x1234567);
        assert_eq!(msgs[1].type_id, 8);
    }
}
