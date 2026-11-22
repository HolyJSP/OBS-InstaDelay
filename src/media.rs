//! Packet types and helpers for inspecting FLV audio/video tag bodies carried over RTMP.

use bytes::Bytes;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    Audio,
    Video,
    Data,
}

impl Kind {
    pub fn idx(self) -> usize {
        match self {
            Kind::Audio => 0,
            Kind::Video => 1,
            Kind::Data => 2,
        }
    }
}

/// Packet received from OBS.
#[derive(Clone, Debug)]
pub struct InPacket {
    pub kind: Kind,
    pub ts: u32,
    pub data: Bytes,
}

/// Packet released by the delay engine (timestamps already rewritten).
#[derive(Clone, Debug)]
pub struct OutPacket {
    pub kind: Kind,
    pub ts: i64,
    pub data: Bytes,
    pub key: bool,
    /// Sequence header (audio/video) or metadata (data).
    pub header: bool,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct VideoInfo {
    pub header: bool,
    pub key: bool,
    pub enhanced: bool,
    pub pkt_type: u8,
}

pub fn classify_video(d: &[u8]) -> VideoInfo {
    if d.len() < 2 {
        return VideoInfo::default();
    }
    let enhanced = d[0] & 0x80 != 0; // Enhanced RTMP (HEVC/AV1)
    let frame_type = (d[0] >> 4) & 0x07;
    if enhanced {
        let pkt_type = d[0] & 0x0f;
        let header = pkt_type == 0;
        let coded = pkt_type == 1 || pkt_type == 3;
        return VideoInfo {
            header,
            key: frame_type == 1 && coded,
            enhanced,
            pkt_type,
        };
    }
    let codec = d[0] & 0x0f;
    let avc_like = matches!(codec, 7 | 12 | 13);
    let header = avc_like && d[1] == 0;
    let media = if avc_like { d[1] == 1 } else { true };
    VideoInfo {
        header,
        key: frame_type == 1 && media && !header,
        enhanced: false,
        pkt_type: d[1],
    }
}

pub fn is_audio_header(d: &[u8]) -> bool {
    d.len() >= 2 && d[0] >> 4 == 10 && d[1] == 0
}

const AAC_RATES: [u32; 13] = [
    96000, 88200, 64000, 48000, 44100, 32000, 24000, 22050, 16000, 12000, 11025, 8000, 7350,
];
// Silent AAC-LC raw frames (1024 samples), valid at any sample rate (same frames hls.js uses for gap filling).
const AAC_SILENT_MONO: [u8; 6] = [0x00, 0xc8, 0x00, 0x80, 0x23, 0x80];
const AAC_SILENT_STEREO: [u8; 9] = [0x21, 0x00, 0x49, 0x90, 0x02, 0x19, 0x00, 0x23, 0x80];

#[derive(Clone, Debug)]
pub struct AacConfig {
    pub sample_rate: u32,
    pub channels: u8,
    pub object_type: u8,
    pub silent_tag: Option<Bytes>,
}

pub fn parse_aac_header(d: &[u8]) -> Option<AacConfig> {
    if d.len() < 4 {
        return None;
    }
    let object_type = d[2] >> 3;
    let freq_idx = (((d[2] & 0x07) << 1) | (d[3] >> 7)) as usize;
    let channels = (d[3] >> 3) & 0x0f;
    let sample_rate = *AAC_RATES.get(freq_idx)?;
    let silent: Option<&[u8]> = match (object_type, channels) {
        (2, 1) => Some(&AAC_SILENT_MONO),
        (2, 2) => Some(&AAC_SILENT_STEREO),
        _ => None,
    };
    let silent_tag = silent.map(|s| {
        let mut v = vec![d[0], 0x01];
        v.extend_from_slice(s);
        Bytes::from(v)
    });
    Some(AacConfig {
        sample_rate,
        channels,
        object_type,
        silent_tag,
    })
}

/// Copy of a keyframe tag that is safe to repeat on its own (composition offset zeroed).
pub fn standalone_keyframe(d: &Bytes, info: &VideoInfo) -> Bytes {
    let mut out = d.to_vec();
    if !info.enhanced && out.len() > 5 && out[1] == 1 {
        out[2..5].fill(0);
    } else if info.enhanced && info.pkt_type == 1 && out.len() > 8 {
        out[5..8].fill(0);
    }
    Bytes::from(out)
}

/// Composition time offset (PTS − DTS, in ms) of a video tag; 0 if it has none.
pub fn composition_offset(d: &[u8]) -> i64 {
    let i24 = |b: &[u8]| {
        let v = ((b[0] as i32) << 16) | ((b[1] as i32) << 8) | b[2] as i32;
        (if v & 0x80_0000 != 0 {
            v - 0x100_0000
        } else {
            v
        }) as i64
    };
    if d.len() < 5 {
        return 0;
    }
    if d[0] & 0x80 != 0 {
        // Enhanced RTMP: [hdr][fourcc x4][cts x3] for PacketTypeCodedFrames
        if d[0] & 0x0f == 1 && d.len() >= 8 {
            i24(&d[5..8])
        } else {
            0
        }
    } else if matches!(d[0] & 0x0f, 7 | 12 | 13) && d[1] == 1 {
        i24(&d[2..5])
    } else {
        0
    }
}

/// Copy of a video tag with its composition offset (PTS − DTS) multiplied by `factor`, used when
/// playing slower/faster so B-frames keep their display order and spacing.
pub fn scale_composition_offset(d: &Bytes, factor: f64) -> Bytes {
    let cts = composition_offset(d);
    if cts == 0 {
        return d.clone();
    }
    let new = ((cts as f64) * factor).round() as i64;
    let at = if d[0] & 0x80 != 0 { 5 } else { 2 };
    let mut v = d.to_vec();
    let b = (new as i32).to_be_bytes();
    v[at..at + 3].copy_from_slice(&b[1..]);
    Bytes::from(v)
}
