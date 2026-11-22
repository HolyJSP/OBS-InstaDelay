//! Pitch-preserving audio time-stretch for gradual delay changes.
//!
//! While the delay ramps up (output plays slightly slower) or down (slightly faster), the audio
//! has to change speed with the video without changing pitch. Compressed AAC can't be stretched
//! directly, so only during a ramp we decode → stretch (WSOLA) → re-encode. Outside ramps,
//! audio passes through untouched.
//!
//! Switching between original and re-encoded audio is aligned to the sample: the encoder is
//! primed with the audio viewers just heard and its internal delay is compensated, so the
//! stream doesn't click or skip at the start or end of a ramp.

use bytes::Bytes;
use symphonia_codec_aac::AacDecoder;
use symphonia_core::codecs::audio::well_known::CODEC_ID_AAC;
use symphonia_core::codecs::audio::{AudioCodecParameters, AudioDecoder, AudioDecoderOptions};
use symphonia_core::packet::PacketRef;
use symphonia_core::units::{Duration, Timestamp};

pub const FRAME: usize = 1024;

// ---- WSOLA ------------------------------------------------------------------------------------

/// Waveform-similarity overlap-add time stretcher (interleaved f32).
pub struct Wsola {
    ch: usize,
    l: usize,
    hs: usize,
    delta: i64,
    win: Vec<f32>,
    inp: Vec<f32>,
    inp_base: i64,
    pos: f64,
    acc: Vec<f32>,
    out_idx: i64,
    prev: Vec<f32>,
    has_prev: bool,
    speed: f64,
    search: bool,
    /// Settle once processing reaches this input index (audio fed before it keeps the old speed).
    settle_at: Option<i64>,
    /// (output index, input index) of the start of the last emitted hop.
    pub last_map: (i64, i64),
}

const DECIM: usize = 4;

impl Wsola {
    pub fn new(ch: usize, sample_rate: u32) -> Wsola {
        // ~43 ms windows at 48 kHz, 50 % overlap, ±5 ms alignment search.
        let l = if sample_rate > 32000 { 2048 } else { 1024 };
        let hs = l / 2;
        let win = (0..l)
            .map(|n| (0.5 - 0.5 * (2.0 * std::f64::consts::PI * n as f64 / l as f64).cos()) as f32)
            .collect();
        Wsola {
            ch,
            l,
            hs,
            delta: (l / 8) as i64,
            win,
            inp: Vec::new(),
            inp_base: 0,
            pos: 0.0,
            acc: vec![0.0; l * ch],
            out_idx: 0,
            prev: Vec::new(),
            has_prev: false,
            speed: 1.0,
            search: true,
            settle_at: None,
            last_map: (0, 0),
        }
    }

    pub fn hop(&self) -> usize {
        self.hs
    }

    /// Output plays at `speed` × the input rate (0.9 = 10 % slower, output 11 % longer).
    pub fn set_speed(&mut self, speed: f64) {
        self.speed = speed.clamp(0.5, 2.0);
        self.search = true;
        self.settle_at = None;
    }

    /// Go back to speed 1 with no alignment search, so output becomes the input shifted by a
    /// constant and can hand over cleanly to the original audio. Audio already fed (it belongs
    /// to the ramp) is still processed at the ramp speed; the change happens exactly after it.
    pub fn settle(&mut self) {
        self.settle_at = Some(self.inp_base + self.frames());
    }

    pub fn settled(&self) -> bool {
        !self.search && self.settle_at.is_none()
    }

    fn frames(&self) -> i64 {
        (self.inp.len() / self.ch) as i64
    }

    fn mono(&self, start: i64, len: usize, step: usize) -> Vec<f32> {
        let off = (start - self.inp_base) as usize;
        (0..len / step)
            .map(|i| {
                let f = (off + i * step) * self.ch;
                self.inp[f..f + self.ch].iter().sum::<f32>()
            })
            .collect()
    }

    fn score(&self, start: i64) -> f32 {
        let cand = self.mono(start, self.l, DECIM);
        let (mut dot, mut en) = (0.0f32, 1e-9f32);
        for (a, b) in cand.iter().zip(&self.prev) {
            dot += a * b;
            en += a * a;
        }
        dot / en.sqrt()
    }

    fn best_start(&self, nominal: i64) -> i64 {
        let lo = (nominal - self.delta).max(self.inp_base);
        let hi = nominal + self.delta;
        if !(self.search && self.has_prev) || lo >= hi {
            return nominal.max(self.inp_base);
        }
        // Coarse search, then refine around the best candidate.
        let mut best = (f32::MIN, nominal.max(lo));
        let mut s = lo;
        while s <= hi {
            let v = self.score(s);
            if v > best.0 {
                best = (v, s);
            }
            s += DECIM as i64;
        }
        let center = best.1;
        for s in (center - DECIM as i64).max(lo)..=(center + DECIM as i64).min(hi) {
            let v = self.score(s);
            if v > best.0 {
                best = (v, s);
            }
        }
        best.1
    }

    pub fn process(&mut self, pcm: &[f32]) -> Vec<f32> {
        self.inp.extend_from_slice(pcm);
        let mut out = Vec::with_capacity(pcm.len() * 2);
        loop {
            if let Some(at) = self.settle_at
                && self.pos >= at as f64
            {
                self.speed = 1.0;
                self.search = false;
                self.pos = self.pos.round();
                self.settle_at = None;
            }
            let nominal = self.pos.round() as i64;
            if nominal + self.delta + (self.l + self.hs) as i64 > self.inp_base + self.frames() {
                break;
            }
            let start = self.best_start(nominal);
            let off = (start - self.inp_base) as usize;
            for n in 0..self.l {
                let w = self.win[n];
                for c in 0..self.ch {
                    self.acc[n * self.ch + c] += self.inp[(off + n) * self.ch + c] * w;
                }
            }
            out.extend_from_slice(&self.acc[..self.hs * self.ch]);
            self.last_map = (self.out_idx, start);
            self.acc.drain(..self.hs * self.ch);
            self.acc.resize(self.l * self.ch, 0.0);
            self.out_idx += self.hs as i64;
            self.prev = self.mono(start + self.hs as i64, self.l, DECIM);
            self.has_prev = true;
            self.pos += self.hs as f64 * self.speed;
            // Drop input we can no longer need.
            let keep = (self.pos.floor() as i64 - self.delta - 1).max(self.inp_base);
            let drop = (keep - self.inp_base) as usize;
            if drop > 4096 {
                self.inp.drain(..drop * self.ch);
                self.inp_base = keep;
            }
        }
        out
    }
}

// ---- AAC transcoder ---------------------------------------------------------------------------

pub struct Transcoder {
    dec: AacDecoder,
    enc: fdk_aac::enc::Encoder,
    ch: usize,
    tag0: u8,
    pub wsola: Wsola,
    pcm: Vec<f32>,
    enc_buf: Vec<i16>,
    skip_out_samples: usize,
    discard_frames: usize,
    /// Encoded frames emitted so far (each covers FRAME samples of the new audio timeline).
    pub emitted: u64,
    /// Stretcher input index where original frame #0 (the first one pushed) begins.
    first_frame_in: i64,
}

fn decode_into(dec: &mut AacDecoder, raw: &[u8], out: &mut Vec<f32>) -> Result<(), String> {
    let pkt = PacketRef::new(0, Timestamp::new(0), Duration::new(FRAME as u64), raw);
    let buf = dec.decode_ref(&pkt).map_err(|e| e.to_string())?;
    buf.copy_to_vec_interleaved(out);
    Ok(())
}

impl Transcoder {
    /// `asc`: AudioSpecificConfig from the AAC sequence header. `history`: the last few original
    /// audio tags already sent (oldest first) so the decoder and encoder start from real audio.
    pub fn new(
        asc: &[u8],
        tag0: u8,
        sample_rate: u32,
        channels: usize,
        bitrate: u32,
        history: &[Bytes],
        speed: f64,
    ) -> Result<Transcoder, String> {
        if !(1..=2).contains(&channels) {
            return Err("only mono/stereo AAC can be stretched".into());
        }
        let mut params = AudioCodecParameters::new();
        params
            .for_codec(CODEC_ID_AAC)
            .with_extra_data(asc.to_vec().into_boxed_slice());
        let mut dec = AacDecoder::try_new(&params, &AudioDecoderOptions::default())
            .map_err(|e| e.to_string())?;
        let enc = fdk_aac::enc::Encoder::new(fdk_aac::enc::EncoderParams {
            bit_rate: fdk_aac::enc::BitRate::Cbr(bitrate.clamp(64_000, 320_000)),
            sample_rate,
            transport: fdk_aac::enc::Transport::Raw,
            channels: if channels == 1 {
                fdk_aac::enc::ChannelMode::Mono
            } else {
                fdk_aac::enc::ChannelMode::Stereo
            },
            audio_object_type: fdk_aac::enc::AudioObjectType::Mpeg4LowComplexity,
        })
        .map_err(|e| format!("{e:?}"))?;
        let delay = enc.info().map(|i| i.nDelay as usize).unwrap_or(2048);

        // Decode what viewers just heard.
        let mut hist = Vec::new();
        for h in history {
            if h.len() > 2 {
                let mut pcm = Vec::new();
                if decode_into(&mut dec, &h[2..], &mut pcm).is_ok() {
                    hist.extend_from_slice(&pcm);
                }
            }
        }
        let hist_frames = hist.len() / channels;

        let mut wsola = Wsola::new(channels, sample_rate);
        wsola.set_speed(speed);
        let hs = wsola.hop();
        // Prepend the last `hs` heard samples so the first window isn't a fade-in; its output
        // (one hop) is dropped below.
        let tail = hs.min(hist_frames);
        let mut lead = vec![0.0f32; (hs - tail) * channels];
        lead.extend_from_slice(&hist[(hist_frames - tail) * channels..]);
        let _ = wsola.process(&lead);

        // Prime the encoder with audio viewers already heard, sized so that an encoder output
        // frame boundary falls exactly where the new audio begins.
        let prefeed = (FRAME - delay % FRAME) % FRAME + FRAME;
        let avail = prefeed.min(hist_frames);
        let mut enc_buf: Vec<i16> = vec![0; (prefeed - avail) * channels];
        enc_buf.extend(
            hist[(hist_frames - avail) * channels..]
                .iter()
                .map(|s| (s * 32767.0).clamp(-32768.0, 32767.0) as i16),
        );

        Ok(Transcoder {
            dec,
            enc,
            ch: channels,
            tag0,
            wsola,
            pcm: Vec::with_capacity(FRAME * 2),
            enc_buf,
            skip_out_samples: hs,
            discard_frames: (prefeed + delay) / FRAME,
            emitted: 0,
            first_frame_in: hs as i64,
        })
    }

    /// Feed one original audio tag; returns re-encoded tags ready to send.
    pub fn push(&mut self, tag: &Bytes) -> Vec<Bytes> {
        self.pcm.clear();
        if tag.len() <= 2 || decode_into(&mut self.dec, &tag[2..], &mut self.pcm).is_err() {
            self.pcm.resize(FRAME * self.ch, 0.0); // keep timing even if a frame fails to decode
        }
        let mut stretched = self.wsola.process(&self.pcm);
        if self.skip_out_samples > 0 {
            let n = (self.skip_out_samples * self.ch).min(stretched.len());
            stretched.drain(..n);
            self.skip_out_samples -= n / self.ch;
        }
        self.enc_buf.extend(
            stretched
                .iter()
                .map(|s| (s * 32767.0).clamp(-32768.0, 32767.0) as i16),
        );

        let mut out = Vec::new();
        let mut buf = vec![0u8; 8192];
        let n = FRAME * self.ch;
        while self.enc_buf.len() >= n {
            match self.enc.encode(&self.enc_buf[..n], &mut buf) {
                Ok(info) => {
                    self.enc_buf
                        .drain(..info.input_consumed.max(n).min(self.enc_buf.len()));
                    if info.output_size > 0 {
                        if self.discard_frames > 0 {
                            self.discard_frames -= 1;
                        } else {
                            let mut v = Vec::with_capacity(info.output_size + 2);
                            v.extend_from_slice(&[self.tag0, 1]);
                            v.extend_from_slice(&buf[..info.output_size]);
                            out.push(Bytes::from(v));
                            self.emitted += 1;
                        }
                    }
                }
                Err(_) => {
                    self.enc_buf.drain(..n);
                }
            }
        }
        out
    }

    /// After `settle()`, how many original frames (counted from the first one pushed) the
    /// emitted output covers. Hand back to the original audio at the next whole frame.
    pub fn frames_covered(&self) -> f64 {
        let (o, i) = self.wsola.last_map;
        let c = i - o; // constant once settled
        let covered_in = c + self.emitted as i64 * FRAME as i64 + self.wsola.hop() as i64;
        (covered_in - self.first_frame_in) as f64 / FRAME as f64
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sine(n: usize, ch: usize, sr: f64, f: f64, start: usize) -> Vec<f32> {
        (0..n)
            .flat_map(|i| {
                std::iter::repeat_n(
                    (2.0 * std::f64::consts::PI * f * (start + i) as f64 / sr).sin() as f32 * 0.5,
                    ch,
                )
            })
            .collect()
    }

    /// Dominant frequency by counting zero crossings.
    fn freq(x: &[f32], ch: usize, sr: f64) -> f64 {
        let m: Vec<f32> = x.chunks(ch).map(|c| c[0]).collect();
        let z = m
            .windows(2)
            .filter(|w| (w[0] < 0.0) != (w[1] < 0.0))
            .count();
        z as f64 / 2.0 / (m.len() as f64 / sr)
    }

    #[test]
    fn wsola_keeps_pitch_and_changes_length() {
        for speed in [0.7, 0.9, 1.1, 1.3] {
            let mut w = Wsola::new(2, 48000);
            w.set_speed(speed);
            let input = sine(48000 * 4, 2, 48000.0, 440.0, 0);
            let mut out = Vec::new();
            for chunk in input.chunks(1024 * 2) {
                out.extend(w.process(chunk));
            }
            let ratio = (out.len() as f64 / 2.0) / (48000.0 * 4.0);
            let f = freq(&out[48000 * 2..], 2, 48000.0); // skip the start
            assert!(
                (ratio - 1.0 / speed).abs() < 0.03,
                "speed {speed}: length ratio {ratio}"
            );
            assert!((f - 440.0).abs() < 3.0, "speed {speed}: pitch {f} Hz");
            // No clicks: sample-to-sample jumps stay within what a 440 Hz sine produces.
            let max_jump = out[48000 * 2..]
                .chunks(2)
                .map(|c| c[0])
                .collect::<Vec<_>>()
                .windows(2)
                .map(|p| (p[1] - p[0]).abs())
                .fold(0.0f32, f32::max);
            assert!(max_jump < 0.05, "speed {speed}: jump {max_jump}");
        }
    }
}

#[cfg(test)]
mod aac_tests {
    use super::*;
    use std::process::Command;

    fn flv_audio_tags(path: &str) -> Vec<Bytes> {
        let d = std::fs::read(path).unwrap();
        let mut pos = 13;
        let mut tags = Vec::new();
        while pos + 11 <= d.len() {
            let size = u32::from_be_bytes([0, d[pos + 1], d[pos + 2], d[pos + 3]]) as usize;
            if d[pos] == 8 {
                tags.push(Bytes::copy_from_slice(&d[pos + 11..pos + 11 + size]));
            }
            pos += 11 + size + 4;
        }
        tags
    }

    fn write_flv(path: &str, header: &Bytes, frames: &[Bytes], sr: f64) {
        let mut out = vec![0x46, 0x4c, 0x56, 1, 4, 0, 0, 0, 9, 0, 0, 0, 0];
        let mut tag = |data: &[u8], ts: u32| {
            out.push(8);
            out.extend_from_slice(&(data.len() as u32).to_be_bytes()[1..]);
            out.extend_from_slice(&(ts & 0xffffff).to_be_bytes()[1..]);
            out.push((ts >> 24) as u8);
            out.extend_from_slice(&[0, 0, 0]);
            out.extend_from_slice(data);
            out.extend_from_slice(&((11 + data.len()) as u32).to_be_bytes());
        };
        tag(header, 0);
        for (i, f) in frames.iter().enumerate() {
            tag(f, (i as f64 * 1024.0 * 1000.0 / sr).round() as u32);
        }
        std::fs::write(path, out).unwrap();
    }

    fn decode_pcm(path: &str) -> Vec<f32> {
        let o = Command::new("ffmpeg")
            .args(["-v", "error", "-i", path, "-ac", "1", "-f", "f32le", "-"])
            .output()
            .unwrap();
        o.stdout
            .chunks(4)
            .map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]]))
            .collect()
    }

    /// Real AAC through decode → stretch → encode, switching in and out of the original stream.
    #[test]
    fn transcode_switches_cleanly() {
        if Command::new("ffmpeg").arg("-version").output().is_err() {
            return;
        }
        let dir = std::env::temp_dir().join(format!("db-stretch-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let src = dir.join("in.flv");
        let st = Command::new("ffmpeg")
            .args([
                "-v",
                "error",
                "-y",
                "-f",
                "lavfi",
                "-i",
                "sine=frequency=440:sample_rate=48000",
                "-ac",
                "2",
                "-c:a",
                "aac",
                "-b:a",
                "160k",
                "-t",
                "8",
            ])
            .arg(&src)
            .status()
            .unwrap();
        assert!(st.success());
        let tags = flv_audio_tags(src.to_str().unwrap());
        let header = tags[0].clone();
        let frames: Vec<Bytes> = tags[1..].to_vec();
        let asc = &header[2..];

        for speed in [0.7, 0.9, 1.1, 1.3] {
            let (n0, n1) = (100usize, 200usize);
            let mut out: Vec<Bytes> = frames[..n0].to_vec();
            let mut t = Transcoder::new(
                asc,
                header[0],
                48000,
                2,
                160_000,
                &frames[n0 - 4..n0],
                speed,
            )
            .unwrap();
            let mut fed = n0;
            for f in &frames[n0..n1] {
                out.extend(t.push(f));
                fed += 1;
            }
            t.wsola.settle();
            let mut after = 0;
            for f in &frames[n1..] {
                out.extend(t.push(f));
                fed += 1;
                if t.wsola.settled() {
                    after += 1;
                    if after == 8 {
                        break;
                    }
                }
            }
            let j = n0 + t.frames_covered().round() as usize;
            assert!(j <= fed, "handover frame {j} must already be fed ({fed})");
            out.extend_from_slice(&frames[j..]);

            let path = dir.join(format!("out-{speed}.flv"));
            write_flv(path.to_str().unwrap(), &header, &out, 48000.0);
            let pcm = decode_pcm(path.to_str().unwrap());
            // Expected length change: the stretched section is 1/speed as long.
            let expect = frames.len() as f64 + (n1 - n0) as f64 * (1.0 / speed - 1.0);
            let got = out.len() as f64;
            assert!(
                (got - expect).abs() < 3.0,
                "speed {speed}: {got} frames, expected ~{expect}"
            );
            // Clicks: the sine's natural max step is ~0.0072 (amplitude 1/8); flag 3× that.
            let body = &pcm[4800..pcm.len() - 4800];
            let clicks: Vec<usize> = body
                .windows(2)
                .enumerate()
                .filter(|(_, w)| (w[1] - w[0]).abs() > 0.022)
                .map(|(i, _)| i + 4800)
                .collect();
            let zc = body
                .windows(2)
                .filter(|w| (w[0] < 0.0) != (w[1] < 0.0))
                .count() as f64
                / 2.0
                / (body.len() as f64 / 48000.0);
            println!(
                "speed {speed}: frames {got} (expected {expect:.1}), pitch {zc:.1} Hz, clicks {} {:?}",
                clicks.len(),
                &clicks[..clicks.len().min(5)]
            );
            assert!((zc - 440.0).abs() < 3.0, "pitch drifted to {zc}");
            assert!(
                clicks.len() <= 2,
                "audible clicks at {:?}",
                &clicks[..clicks.len().min(8)]
            );
        }
    }
}
