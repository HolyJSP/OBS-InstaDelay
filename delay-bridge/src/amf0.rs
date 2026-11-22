//! Minimal AMF0 encoder/decoder — enough for RTMP commands and metadata.

#[derive(Debug, Clone, PartialEq)]
pub enum Amf {
    Number(f64),
    Bool(bool),
    Str(String),
    Object(Vec<(String, Amf)>),
    Null,
    Undefined,
    Array(Vec<Amf>),
    Date(f64),
}

impl Amf {
    pub fn str(s: &str) -> Amf {
        Amf::Str(s.to_string())
    }

    pub fn obj(pairs: &[(&str, Amf)]) -> Amf {
        Amf::Object(
            pairs
                .iter()
                .map(|(k, v)| (k.to_string(), v.clone()))
                .collect(),
        )
    }

    pub fn get(&self, key: &str) -> Option<&Amf> {
        match self {
            Amf::Object(v) => v.iter().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }

    pub fn as_str(&self) -> Option<&str> {
        match self {
            Amf::Str(s) => Some(s),
            _ => None,
        }
    }

    pub fn as_f64(&self) -> Option<f64> {
        match self {
            Amf::Number(n) => Some(*n),
            _ => None,
        }
    }
}

pub fn encode(values: &[Amf]) -> Vec<u8> {
    let mut out = Vec::with_capacity(128);
    for v in values {
        enc(v, &mut out);
    }
    out
}

fn enc_str_body(s: &str, out: &mut Vec<u8>) {
    out.extend_from_slice(&(s.len() as u16).to_be_bytes());
    out.extend_from_slice(s.as_bytes());
}

fn enc(v: &Amf, out: &mut Vec<u8>) {
    match v {
        Amf::Number(n) => {
            out.push(0x00);
            out.extend_from_slice(&n.to_be_bytes());
        }
        Amf::Bool(b) => out.extend_from_slice(&[0x01, *b as u8]),
        Amf::Str(s) => {
            if s.len() > 0xffff {
                out.push(0x0c);
                out.extend_from_slice(&(s.len() as u32).to_be_bytes());
                out.extend_from_slice(s.as_bytes());
            } else {
                out.push(0x02);
                enc_str_body(s, out);
            }
        }
        Amf::Object(pairs) => {
            out.push(0x03);
            for (k, v) in pairs {
                if *v == Amf::Undefined {
                    continue;
                }
                enc_str_body(k, out);
                enc(v, out);
            }
            out.extend_from_slice(&[0, 0, 0x09]);
        }
        Amf::Null => out.push(0x05),
        Amf::Undefined => out.push(0x06),
        Amf::Array(items) => {
            out.push(0x0a);
            out.extend_from_slice(&(items.len() as u32).to_be_bytes());
            for i in items {
                enc(i, out);
            }
        }
        Amf::Date(ms) => {
            out.push(0x0b);
            out.extend_from_slice(&ms.to_be_bytes());
            out.extend_from_slice(&[0, 0]);
        }
    }
}

pub fn decode(buf: &[u8]) -> Result<Vec<Amf>, &'static str> {
    let mut out = Vec::new();
    let mut pos = 0;
    while pos < buf.len() {
        let (v, p) = dec(buf, pos)?;
        out.push(v);
        pos = p;
    }
    Ok(out)
}

fn need(buf: &[u8], pos: usize, n: usize) -> Result<(), &'static str> {
    if pos + n > buf.len() {
        Err("truncated AMF0")
    } else {
        Ok(())
    }
}

fn dec_props(buf: &[u8], mut pos: usize) -> Result<(Amf, usize), &'static str> {
    let mut pairs = Vec::new();
    while pos + 3 <= buf.len() {
        let len = u16::from_be_bytes([buf[pos], buf[pos + 1]]) as usize;
        pos += 2;
        if len == 0 && buf[pos] == 0x09 {
            return Ok((Amf::Object(pairs), pos + 1));
        }
        need(buf, pos, len)?;
        let key = String::from_utf8_lossy(&buf[pos..pos + len]).into_owned();
        pos += len;
        let (v, p) = dec(buf, pos)?;
        pairs.push((key, v));
        pos = p;
    }
    Ok((Amf::Object(pairs), buf.len()))
}

fn dec(buf: &[u8], pos: usize) -> Result<(Amf, usize), &'static str> {
    need(buf, pos, 1)?;
    let t = buf[pos];
    let pos = pos + 1;
    match t {
        0x00 => {
            need(buf, pos, 8)?;
            Ok((
                Amf::Number(f64::from_be_bytes(buf[pos..pos + 8].try_into().unwrap())),
                pos + 8,
            ))
        }
        0x01 => {
            need(buf, pos, 1)?;
            Ok((Amf::Bool(buf[pos] != 0), pos + 1))
        }
        0x02 => {
            need(buf, pos, 2)?;
            let l = u16::from_be_bytes([buf[pos], buf[pos + 1]]) as usize;
            need(buf, pos + 2, l)?;
            Ok((
                Amf::Str(String::from_utf8_lossy(&buf[pos + 2..pos + 2 + l]).into_owned()),
                pos + 2 + l,
            ))
        }
        0x03 => dec_props(buf, pos),
        0x05 => Ok((Amf::Null, pos)),
        0x06 | 0x0d => Ok((Amf::Undefined, pos)),
        0x08 => {
            need(buf, pos, 4)?;
            dec_props(buf, pos + 4)
        }
        0x0a => {
            need(buf, pos, 4)?;
            let n = u32::from_be_bytes(buf[pos..pos + 4].try_into().unwrap()) as usize;
            let mut p = pos + 4;
            let mut items = Vec::with_capacity(n.min(1024));
            for _ in 0..n {
                let (v, np) = dec(buf, p)?;
                items.push(v);
                p = np;
            }
            Ok((Amf::Array(items), p))
        }
        0x0b => {
            need(buf, pos, 10)?;
            Ok((
                Amf::Date(f64::from_be_bytes(buf[pos..pos + 8].try_into().unwrap())),
                pos + 10,
            ))
        }
        0x0c => {
            need(buf, pos, 4)?;
            let l = u32::from_be_bytes(buf[pos..pos + 4].try_into().unwrap()) as usize;
            need(buf, pos + 4, l)?;
            Ok((
                Amf::Str(String::from_utf8_lossy(&buf[pos + 4..pos + 4 + l]).into_owned()),
                pos + 4 + l,
            ))
        }
        _ => Err("unsupported AMF0 type"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip() {
        let v = vec![
            Amf::str("connect"),
            Amf::Number(1.0),
            Amf::obj(&[
                ("app", Amf::str("live")),
                ("n", Amf::Number(3.5)),
                ("b", Amf::Bool(true)),
            ]),
            Amf::Null,
            Amf::Array(vec![Amf::Number(1.0), Amf::str("x")]),
        ];
        assert_eq!(decode(&encode(&v)).unwrap(), v);
    }
}
