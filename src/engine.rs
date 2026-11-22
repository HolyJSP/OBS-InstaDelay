//! The delay buffer.
//!
//! Buffers encoded packets from OBS and releases them `delay_ms` later, rewriting timestamps so
//! the platform always sees one continuous, monotonic stream (no reconnect, no restart).
//!
//! Changing the delay while live:
//!  * **Increase, OBS sends frequent keyframes (≤ 3 s apart):** there's nothing new to show for
//!    (new − old) seconds, so we send *filler* (the last keyframe or a captured overlay frame,
//!    repeated, plus silent audio) and resume at the next keyframe that becomes due.
//!  * **Increase, keyframes are rare or never come** (OBS's keyframe interval on "auto", common
//!    with hardware encoders): we pause the video, keep the audio going with silence, and then
//!    resume *exactly* where we stopped. The decoder never misses a frame, so no keyframe is
//!    needed and nothing is skipped.
//!  * **Decrease / remove:** we keep streaming at the old delay and jump ahead at the next
//!    keyframe, so the stream never freezes while waiting for one.
//!  * **Safety net:** if a keyframe we're waiting for is more than [`KEY_WAIT_MS`] overdue, we
//!    resume anyway rather than leave viewers on a frozen screen.
//!
//! Runs as a single task with no polling: it sleeps until the next packet is due and releases
//! everything due in one batch.

use crate::media::{self, AacConfig, InPacket, Kind, OutPacket, VideoInfo};
use bytes::Bytes;
use serde::Serialize;
use std::collections::VecDeque;
use std::time::{Duration, Instant};
use tokio::sync::{mpsc, oneshot};

const BATCH_MS: u64 = 15;
const FILLER_TICK_MS: u64 = 20;
const FRAME_GAP: i64 = 34;
/// Keyframes at most this far apart count as "frequent".
const FREQUENT_KEYS_MS: u64 = 3000;
/// Give up waiting for a keyframe after it's this overdue and resume anyway.
pub const KEY_WAIT_MS: u64 = 3000;
/// See `catchup_threshold`.
const CATCHUP_MS: u64 = 2500;

#[derive(Clone, Debug)]
pub struct CapturedKey {
    pub session: u32,
    pub data: Bytes,
    pub info: VideoInfo,
}

pub enum EngineMsg {
    Packet(InPacket),
    SourceStart,
    SourceEnd,
    SetDelay {
        ms: u64,
        filler: Option<CapturedKey>,
        /// Change gradually (play slightly slower/faster) instead of all at once.
        gradual: bool,
    },
    Flush,
    Capture {
        after_ms: u64,
        reply: oneshot::Sender<CapturedKey>,
    },
    Status(oneshot::Sender<EngineStatus>),
    Configure {
        filler_fps: u32,
        max_delay_ms: u64,
        ramp_pct: u32,
    },
}

pub enum EngineEvent {
    Packet(OutPacket),
    Ended,
    /// Something worth telling the user (goes to the activity log).
    Notice(String),
}

#[derive(Serialize, Clone, Debug, Default)]
#[serde(rename_all = "camelCase")]
pub struct EngineStatus {
    pub delay_ms: u64,
    pub switching: bool,
    pub filler_active: bool,
    pub filler_is_overlay: bool,
    /// While adding delay: video paused (no frozen frame) because keyframes are rare.
    pub video_paused: bool,
    pub switch_remaining_ms: u64,
    /// After reducing delay: still streaming at the old delay, waiting for a keyframe to jump.
    pub catching_up: bool,
    /// Gradual change in progress (playing slower to add delay or faster to remove it).
    pub ramping: bool,
    /// Playback speed during a gradual change (0.9 = 10 % slower).
    pub ramp_speed: f64,
    /// Estimated time until a gradual change reaches its target.
    pub ramp_eta_ms: Option<u64>,
    pub buffered_ms: u64,
    pub buffered_bytes: usize,
    pub viewer_lag_ms: Option<u64>,
    /// Time between the last two keyframes from OBS.
    pub keyframe_interval_ms: Option<u64>,
    pub source_active: bool,
    pub draining: bool,
    pub live: bool,
    pub in_kbps: u64,
    pub out_kbps: u64,
}

#[derive(Clone)]
struct QPacket {
    seq: u64,
    kind: Kind,
    ts: i64,
    data: Bytes,
    arrive: u64,
    session: u32,
    key: bool,
    header: bool,
    info: VideoInfo,
}

struct Filler {
    start_wall: u64,
    start_ts: i64,
    next_v: f64,
    next_a: f64,
    /// Repeated keyframe; None = video paused (audio-only filler).
    video: Option<Bytes>,
    overlay: bool,
}

/// Gradual delay change: output time of media timestamp `m` is `o0 + (m − m0) / speed`, and it
/// is released at that output time plus `k` (wall clock).
struct Ramp {
    speed: f64,
    m0: i64,
    o0: f64,
    k: f64,
    target: u64,
}

impl Ramp {
    fn out(&self, m: i64) -> f64 {
        self.o0 + (m - self.m0) as f64 / self.speed
    }
}

/// Audio time-stretch session (only while ramping, plus a short hand-back afterwards).
struct Xcode {
    t: crate::stretch::Transcoder,
    next_ts: f64,
    frame_ms: f64,
    /// Original frames fed so far: (index since start, media ts, data).
    fed: VecDeque<(u64, i64, Bytes)>,
    fed_count: u64,
    /// After the ramp ends: frames left to feed before handing back to the original audio.
    finishing_left: Option<u32>,
}

/// Last original packet sent: media ts, output ts, release wall time, arrival.
#[derive(Clone, Copy)]
struct LastEmit {
    m: i64,
    out: f64,
    wall: f64,
    arrive: u64,
}

#[derive(PartialEq, Eq, Clone, Copy, Debug)]
enum State {
    Resync,
    Normal,
}

/// How a resync ends.
#[derive(PartialEq, Eq, Clone, Copy, Debug)]
enum Mode {
    /// Resume at a keyframe that becomes due (stream start, OBS reconnect, frequent keyframes).
    Keyframe,
    /// Resume exactly where output stopped (video paused meanwhile); needs no keyframe.
    Continue,
}

pub type Sink = Box<dyn FnMut(EngineEvent) + Send>;

pub struct Engine {
    filler_fps: u32,
    max_delay_ms: u64,
    delay_ms: u64,
    queue: VecDeque<QPacket>,
    queue_bytes: usize,
    /// (seq, arrive) of every keyframe still in the queue, oldest first.
    keys: VecDeque<(u64, u64)>,
    next_seq: u64,
    session: u32,
    source_active: bool,
    waiters: Vec<(u64, oneshot::Sender<CapturedKey>)>,
    audio_cfg: Option<AacConfig>,
    in_rate: RateMeter,
    out_rate: RateMeter,
    last_key_arrive: Option<u64>,
    key_interval_ms: Option<u64>,
    epoch: Instant,
    sink: Sink,
    // output-side state
    state: State,
    mode: Mode,
    offset: Option<i64>,
    out_session: Option<u32>,
    last_out: [i64; 3],
    last_out_max: i64,
    /// Latest presentation time of any video frame sent (DTS + composition offset).
    last_video_pts: i64,
    pending: [Option<QPacket>; 3],
    slack_ms: u64,
    filler: Option<Filler>,
    filler_override: Option<CapturedKey>,
    last_sent_key: Option<CapturedKey>,
    last_sent_arrive: Option<u64>,
    output_started: bool,
    warned_no_keyframe: bool,
    ramp_pct: f64,
    ramp: Option<Ramp>,
    xcode: Option<Xcode>,
    recent_audio: VecDeque<Bytes>,
    audio_header: Option<Bytes>,
    audio_adjust: i64,
    last_emit: Option<LastEmit>,
}

impl Engine {
    pub fn new(filler_fps: u32, max_delay_ms: u64, sink: Sink) -> Self {
        Engine {
            filler_fps: filler_fps.clamp(1, 30),
            max_delay_ms,
            delay_ms: 0,
            queue: VecDeque::with_capacity(4096),
            queue_bytes: 0,
            keys: VecDeque::new(),
            next_seq: 0,
            session: 0,
            source_active: false,
            waiters: Vec::new(),
            audio_cfg: None,
            in_rate: RateMeter::new(),
            out_rate: RateMeter::new(),
            last_key_arrive: None,
            key_interval_ms: None,
            epoch: Instant::now(),
            sink,
            state: State::Resync,
            mode: Mode::Keyframe,
            offset: None,
            out_session: None,
            last_out: [-1; 3],
            last_out_max: -1,
            last_video_pts: -1,
            pending: [None, None, None],
            slack_ms: 0,
            filler: None,
            filler_override: None,
            last_sent_key: None,
            last_sent_arrive: None,
            output_started: false,
            warned_no_keyframe: false,
            ramp_pct: 0.10,
            ramp: None,
            xcode: None,
            recent_audio: VecDeque::new(),
            audio_header: None,
            audio_adjust: 0,
            last_emit: None,
        }
    }

    fn now(&self) -> u64 {
        self.epoch.elapsed().as_millis() as u64
    }

    fn reset_output(&mut self) {
        self.state = State::Resync;
        self.mode = Mode::Keyframe;
        self.offset = None;
        self.out_session = None;
        self.last_out = [-1; 3];
        self.last_out_max = -1;
        self.last_video_pts = -1;
        self.pending = [None, None, None];
        self.slack_ms = 0;
        self.filler = None;
        self.filler_override = None;
        self.last_sent_key = None;
        self.last_sent_arrive = None;
        self.output_started = false;
        self.ramp = None;
        self.xcode = None;
        self.recent_audio.clear();
        self.audio_adjust = 0;
        self.last_emit = None;
    }

    fn notice(&mut self, msg: impl Into<String>) {
        (self.sink)(EngineEvent::Notice(msg.into()));
    }

    /// Actor loop: owns the engine, sleeps until the next packet is due.
    pub async fn run(mut self, mut rx: mpsc::Receiver<EngineMsg>) {
        let sleep = tokio::time::sleep(Duration::from_secs(3600));
        tokio::pin!(sleep);
        let mut armed: Option<u64> = None;
        loop {
            tokio::select! {
                biased;
                m = rx.recv() => match m {
                    None => break,
                    Some(m) => self.handle(m),
                },
                _ = &mut sleep, if armed.is_some() => {
                    armed = None;
                    self.tick();
                }
            }
            let now = self.now();
            match self.next_wake(now) {
                Some(w) if armed != Some(w) => {
                    sleep
                        .as_mut()
                        .reset((self.epoch + Duration::from_millis(w)).into());
                    armed = Some(w);
                }
                Some(_) => {}
                None => armed = None,
            }
        }
    }

    pub fn handle(&mut self, m: EngineMsg) {
        match m {
            EngineMsg::Packet(p) => self.push(p),
            EngineMsg::SourceStart => {
                self.session += 1;
                self.source_active = true;
            }
            EngineMsg::SourceEnd => {
                self.source_active = false;
                self.tick();
            }
            EngineMsg::SetDelay {
                ms,
                filler,
                gradual,
            } => {
                if gradual {
                    self.set_delay_gradual(ms)
                } else {
                    self.set_delay(ms, filler)
                }
            }
            EngineMsg::Flush => {
                self.flush();
                self.tick();
            }
            EngineMsg::Capture { after_ms, reply } => {
                let at = self.now() + after_ms;
                self.waiters.push((at, reply));
            }
            EngineMsg::Status(reply) => {
                let _ = reply.send(self.status());
            }
            EngineMsg::Configure {
                filler_fps,
                max_delay_ms,
                ramp_pct,
            } => {
                self.filler_fps = filler_fps.clamp(1, 30);
                self.max_delay_ms = max_delay_ms;
                self.ramp_pct = (ramp_pct.clamp(2, 50) as f64) / 100.0;
            }
        }
    }

    fn push(&mut self, pkt: InPacket) {
        let now = self.now();
        let mut q = QPacket {
            seq: self.next_seq,
            kind: pkt.kind,
            ts: pkt.ts as i64,
            data: pkt.data,
            arrive: now,
            session: self.session,
            key: false,
            header: false,
            info: VideoInfo::default(),
        };
        self.next_seq += 1;
        match q.kind {
            Kind::Video => {
                q.info = media::classify_video(&q.data);
                q.key = q.info.key;
                q.header = q.info.header;
            }
            Kind::Audio => q.header = media::is_audio_header(&q.data),
            Kind::Data => q.header = true,
        }
        self.in_rate.add(q.data.len(), now);

        if q.key {
            if let Some(last) = self.last_key_arrive {
                self.key_interval_ms = Some(now - last);
            }
            self.last_key_arrive = Some(now);
            self.keys.push_back((q.seq, now));
            if !self.waiters.is_empty() {
                let mut i = 0;
                while i < self.waiters.len() {
                    if self.waiters[i].1.is_closed() {
                        self.waiters.swap_remove(i);
                    } else if now >= self.waiters[i].0 {
                        let (_, tx) = self.waiters.swap_remove(i);
                        let _ = tx.send(CapturedKey {
                            session: q.session,
                            data: q.data.clone(),
                            info: q.info,
                        });
                    } else {
                        i += 1;
                    }
                }
            }
        }

        self.queue_bytes += q.data.len();
        self.queue.push_back(q);
        if self.delay_ms == 0 || self.catching_up() {
            self.tick();
        }
    }

    /// Keyframes are arriving often enough to resume at one after adding delay.
    fn keyframes_frequent(&self, now: u64) -> bool {
        match (self.key_interval_ms, self.last_key_arrive) {
            (Some(i), Some(last)) => {
                i <= FREQUENT_KEYS_MS && now.saturating_sub(last) <= FREQUENT_KEYS_MS + 1000
            }
            _ => false,
        }
    }

    /// Viewers more than this far behind the requested delay get jumped ahead at the next
    /// keyframe. With OBS's recommended 2 s keyframe interval one jump always lands within it; with
    /// long intervals a second jump at the following keyframe brings them close to live.
    fn catchup_threshold(&self) -> u64 {
        CATCHUP_MS
    }

    fn catching_up(&self) -> bool {
        self.state == State::Normal && self.slack_ms > self.catchup_threshold()
    }

    fn set_delay(&mut self, ms: u64, filler: Option<CapturedKey>) {
        let ms = ms.min(self.max_delay_ms);
        self.freeze_ramp();
        let now = self.now();
        let filler = filler.filter(|f| f.session == self.session);
        if ms == self.delay_ms {
            self.filler_override = filler;
            return;
        }
        let prev = self.delay_ms;
        self.delay_ms = ms;
        if self.offset.is_none() {
            self.filler_override = filler;
            self.tick();
            return; // not streaming yet: the delay simply applies from the start
        }

        if self.state == State::Resync {
            // Already switching: keep the current approach, it just aims at the new delay.
            self.filler_override = filler.or(self.filler_override.take());
        } else if ms > prev {
            // Adding delay. Viewers' output continues at the same pace, so any catch-up slack
            // counts towards the new delay.
            let extra = ms - prev;
            if self.slack_ms >= extra {
                self.slack_ms -= extra;
            } else {
                self.slack_ms = 0;
                self.state = State::Resync;
                self.filler = None;
                if self.keyframes_frequent(now) {
                    self.mode = Mode::Keyframe;
                    self.filler_override = filler;
                } else {
                    self.mode = Mode::Continue;
                    self.filler_override = None;
                    if filler.is_some() {
                        self.notice("Overlay mode needs OBS's keyframe interval at 1–2 s — pausing the video instead");
                    }
                }
            }
        } else {
            // Reducing delay: keep streaming at the current pace and jump ahead at the next
            // keyframe (immediately if one is already buffered).
            self.slack_ms += prev - ms;
        }
        self.tick();
    }

    /// Current actual delay viewers have (what the last packet sent was held for).
    fn current_delay(&self) -> Option<f64> {
        self.last_emit.map(|l| l.wall - l.arrive as f64)
    }

    /// Stop a gradual change where it is, continuing normally at the delay reached so far.
    fn freeze_ramp(&mut self) {
        let Some(_) = self.ramp.take() else { return };
        if let Some(l) = self.last_emit {
            let cur = (l.wall - l.arrive as f64).max(0.0);
            self.offset = Some((l.out - l.m as f64).round() as i64);
            self.delay_ms = cur.round() as u64;
            self.slack_ms = 0;
        }
        self.begin_audio_handback();
    }

    /// Change the delay gradually: play a little slower (adding delay) or faster (removing it)
    /// until the target is reached. No pause, no jump, no keyframe needed.
    fn set_delay_gradual(&mut self, ms: u64) {
        let ms = ms.min(self.max_delay_ms);
        let (Some(_), Some(last), Some(cur)) = (self.offset, self.last_emit, self.current_delay())
        else {
            // Not streaming yet: the delay simply applies from the start.
            self.delay_ms = ms;
            self.tick();
            return;
        };
        if self.state != State::Normal {
            return self.set_delay(ms, None); // an instant switch is in progress; finish it that way
        }
        let target_now = self
            .ramp
            .as_ref()
            .map(|r| r.target)
            .unwrap_or(self.delay_ms);
        if ms == target_now && (self.ramp.is_some() || (cur - ms as f64).abs() < 300.0) {
            return;
        }
        // Fold any pending catch-up into the ramp.
        self.slack_ms = 0;
        let speed = if ms as f64 > cur {
            1.0 - self.ramp_pct
        } else {
            1.0 + self.ramp_pct
        };
        self.ramp = Some(Ramp {
            speed,
            m0: last.m,
            o0: last.out,
            k: last.wall - last.out,
            target: ms,
        });
        self.delay_ms = ms;

        // Audio: stretch it along with the video (pitch preserved).
        if let Some(x) = self.xcode.as_mut() {
            x.t.wsola.set_speed(speed);
            x.finishing_left = None;
        } else {
            self.xcode = self.start_transcoder(speed);
            if self.xcode.is_none() && self.audio_header.is_some() {
                self.notice("Audio can't be time-stretched for this stream (needs AAC-LC mono/stereo); audio may stutter during the change");
            }
        }
        self.tick();
    }

    fn start_transcoder(&self, speed: f64) -> Option<Xcode> {
        let hdr = self.audio_header.as_ref()?;
        let cfg = self.audio_cfg.as_ref()?;
        if cfg.object_type != 2 || hdr.len() < 4 || self.recent_audio.is_empty() {
            return None;
        }
        let frame_ms = 1024.0 * 1000.0 / cfg.sample_rate as f64;
        // Estimate the stream's audio bitrate from recent frame sizes.
        let avg = self.recent_audio.iter().map(|b| b.len()).sum::<usize>() as f64
            / self.recent_audio.len() as f64;
        let bitrate = (avg * 8.0 * 1000.0 / frame_ms) as u32;
        let history: Vec<Bytes> = self.recent_audio.iter().cloned().collect();
        let t = crate::stretch::Transcoder::new(
            &hdr[2..],
            hdr[0],
            cfg.sample_rate,
            cfg.channels as usize,
            bitrate,
            &history,
            speed,
        )
        .ok()?;
        Some(Xcode {
            t,
            next_ts: self.last_out[Kind::Audio.idx()] as f64 + frame_ms,
            frame_ms,
            fed: VecDeque::new(),
            fed_count: 0,
            finishing_left: None,
        })
    }

    /// Ramp finished: run the stretcher at normal speed for a few frames, then hand back to the
    /// original audio at an exact frame boundary.
    fn begin_audio_handback(&mut self) {
        if let Some(x) = self.xcode.as_mut()
            && x.finishing_left.is_none()
        {
            x.t.wsola.settle();
            x.finishing_left = Some(10);
        }
    }

    fn finish_audio_handback(&mut self) {
        let Some(mut x) = self.xcode.take() else {
            return;
        };
        let Some(offset) = self.offset else { return };
        let j = (x.t.frames_covered().round().max(0.0) as u64).min(x.fed_count);
        let mut last: Option<(i64, f64)> = None;
        let frames: Vec<(i64, Bytes)> = x
            .fed
            .drain(..)
            .filter(|(i, _, _)| *i >= j)
            .map(|(_, m, d)| (m, d))
            .collect();
        for (m, data) in frames {
            let ts = x.next_ts;
            x.next_ts += x.frame_ms;
            self.emit(&Kind::Audio, ts.round() as i64, data.clone(), false, false);
            self.remember_audio(data);
            last = Some((m, ts));
        }
        // Later original audio continues seamlessly from here.
        self.audio_adjust = match last {
            Some((m, ts)) => (ts - (m + offset) as f64).round() as i64,
            None => (x.next_ts - x.frame_ms).round() as i64 - self.last_out[Kind::Audio.idx()],
        };
    }

    fn remember_audio(&mut self, data: Bytes) {
        self.recent_audio.push_back(data);
        while self.recent_audio.len() > 4 {
            self.recent_audio.pop_front();
        }
    }

    fn flush(&mut self) {
        self.queue.clear();
        self.keys.clear();
        self.queue_bytes = 0;
    }

    fn pop_front(&mut self) -> Option<QPacket> {
        let p = self.queue.pop_front()?;
        self.queue_bytes -= p.data.len();
        if p.key {
            self.keys.pop_front();
        }
        Some(p)
    }

    /// Queue index of the newest keyframe that is due under the current delay.
    fn latest_due_key(&self, now: u64) -> Option<usize> {
        let front = self.queue.front()?.seq;
        self.keys
            .iter()
            .rev()
            .find(|(_, arrive)| arrive + self.delay_ms <= now)
            .map(|(seq, _)| (seq - front) as usize)
    }

    fn next_wake(&self, now: u64) -> Option<u64> {
        if !self.source_active && self.queue.is_empty() {
            return None;
        }
        let delay = self.delay_ms;
        match self.state {
            State::Resync if self.offset.is_some() => Some(now + FILLER_TICK_MS),
            State::Resync => self
                .keys
                .front()
                .map(|(_, a)| (a + delay).max(now) + BATCH_MS),
            State::Normal => {
                let front = self.queue.front().map(|p| match &self.ramp {
                    // During a gradual change, packets are due on the ramp's schedule.
                    Some(r) => ((r.out(p.ts) + r.k).max(0.0) as u64).max(now) + BATCH_MS,
                    None => (p.arrive + delay + self.slack_ms).max(now) + BATCH_MS,
                });
                let next_key = if self.catching_up() {
                    self.keys
                        .iter()
                        .find(|(_, a)| a + delay > now)
                        .map(|(_, a)| a + delay + 1)
                } else {
                    None
                };
                match (front, next_key) {
                    (Some(a), Some(b)) => Some(a.min(b)),
                    (a, b) => a.or(b),
                }
            }
        }
    }

    pub fn tick(&mut self) {
        let now = self.now();
        for _ in 0..4 {
            if self.state == State::Resync {
                self.resync(now);
            }
            if self.state != State::Normal {
                break;
            }
            if self.catching_up()
                && let Some(idx) = self.latest_due_key(now)
            {
                self.resume_at(idx, now);
            }
            if !self.drain(now) {
                break;
            }
        }
        if !self.source_active && self.queue.is_empty() && self.output_started {
            self.reset_output();
            (self.sink)(EngineEvent::Ended);
        }
    }

    /// Next free output timestamp: after the last packet sent *and* after the last video frame's
    /// display time (B-frames show later than they decode), so frames never collide.
    fn next_free_ts(&self) -> i64 {
        if self.last_out_max < 0 {
            0
        } else {
            self.last_out_max.max(self.last_video_pts) + FRAME_GAP
        }
    }

    fn timeline_now(&self, now: u64) -> i64 {
        if let Some(f) = &self.filler {
            return self
                .next_free_ts()
                .max(f.start_ts + (now - f.start_wall) as i64);
        }
        self.next_free_ts()
    }

    /// Continue output from queue index `idx` (dropping what's before it).
    fn resume_at(&mut self, idx: usize, now: u64) {
        for _ in 0..idx {
            if let Some(p) = self.pop_front()
                && p.header
            {
                let k = p.kind.idx();
                self.pending[k] = Some(p);
            }
        }
        let Some(first) = self.queue.front() else {
            return;
        };
        // If the resume point is already overdue, pace the rest by that slack instead of
        // bursting, so the output timeline stays in step with real time.
        self.slack_ms = now.saturating_sub(first.arrive + self.delay_ms);
        let (first_ts, first_session) = (first.ts, first.session);
        let start = self.timeline_now(now);
        self.offset = Some(start - first_ts);
        self.out_session = Some(first_session);
        for kind in [Kind::Data, Kind::Video, Kind::Audio] {
            if let Some(h) = self.pending[kind.idx()].take() {
                let ts = start.max(self.last_out[kind.idx()]);
                self.emit(&h.kind, ts, h.data.clone(), false, true);
            }
        }
        self.filler = None;
        self.filler_override = None;
        self.mode = Mode::Keyframe;
        self.state = State::Normal;
        self.ramp = None;
        self.xcode = None;
        self.audio_adjust = 0;
    }

    fn resync(&mut self, now: u64) {
        let head = self.queue.front().map(|p| (p.arrive, p.session));
        // A new OBS session (reconnect) always needs a keyframe.
        if self.mode == Mode::Continue && head.is_some_and(|(_, s)| Some(s) != self.out_session) {
            self.mode = Mode::Keyframe;
        }
        match self.mode {
            Mode::Continue => {
                if let Some((arrive, _)) = head
                    && arrive + self.delay_ms <= now
                {
                    self.resume_at(0, now);
                    return;
                }
            }
            Mode::Keyframe => {
                if let Some(idx) = self.latest_due_key(now) {
                    self.resume_at(idx, now);
                    return;
                }
                // Safety net: never leave viewers on a frozen screen waiting for a keyframe.
                if self.offset.is_some()
                    && let Some((arrive, session)) = head
                    && Some(session) == self.out_session
                    && arrive + self.delay_ms + KEY_WAIT_MS <= now
                {
                    if !self.warned_no_keyframe {
                        self.warned_no_keyframe = true;
                        self.notice("OBS stopped sending keyframes — resumed without one (set OBS's keyframe interval to 2 s)");
                    }
                    self.resume_at(0, now);
                    return;
                }
            }
        }
        if !self.source_active && self.keys.is_empty() && self.mode == Mode::Keyframe {
            self.flush();
            return;
        }
        self.send_filler(now);
    }

    fn send_filler(&mut self, now: u64) {
        if self.offset.is_none() {
            return; // stream hasn't started yet
        }
        if self.filler.is_none() {
            let (video, overlay) = match self.mode {
                Mode::Continue => (None, false), // pause the video; the decoder must not see other frames
                Mode::Keyframe => match &self.filler_override {
                    Some(f) if Some(f.session) == self.out_session => {
                        (Some(media::standalone_keyframe(&f.data, &f.info)), true)
                    }
                    _ => (
                        self.last_sent_key
                            .as_ref()
                            .map(|k| media::standalone_keyframe(&k.data, &k.info)),
                        false,
                    ),
                },
            };
            let start_ts = self.next_free_ts();
            self.filler = Some(Filler {
                start_wall: now,
                start_ts,
                next_v: start_ts as f64,
                next_a: (start_ts.max(self.last_out[Kind::Audio.idx()] + 1)) as f64,
                video,
                overlay,
            });
        }
        let silent = self
            .audio_cfg
            .as_ref()
            .and_then(|a| a.silent_tag.clone().map(|t| (t, a.sample_rate)));
        let v_int = 1000.0 / self.filler_fps as f64;
        let a_int = silent
            .as_ref()
            .map(|(_, sr)| 1024.0 * 1000.0 / *sr as f64)
            .unwrap_or(f64::INFINITY);
        loop {
            let f = self.filler.as_mut().unwrap();
            let t = (f.start_ts + (now - f.start_wall) as i64) as f64;
            let v_due = f.video.is_some() && f.next_v <= t;
            let a_due = silent.is_some() && f.next_a <= t;
            if !v_due && !a_due {
                break;
            }
            if v_due && (!a_due || f.next_v <= f.next_a) {
                let (ts, data) = (f.next_v.round() as i64, f.video.clone().unwrap());
                f.next_v += v_int;
                self.emit(&Kind::Video, ts, data, true, false);
            } else {
                let ts = f.next_a.round() as i64;
                f.next_a += a_int;
                let data = silent.as_ref().unwrap().0.clone();
                self.emit(&Kind::Audio, ts, data, false, false);
            }
        }
    }

    /// Returns true if it switched to resync (OBS reconnected) and the caller should loop.
    fn drain(&mut self, now: u64) -> bool {
        let Some(mut offset) = self.offset else {
            return false;
        };
        while let Some(p) = self.queue.front() {
            // When is this packet due, and at what output time?
            let (out_f, wall) = match &self.ramp {
                Some(r) => {
                    let o = r.out(p.ts);
                    (o, o + r.k)
                }
                None => (
                    (p.ts + offset) as f64,
                    (p.arrive + self.delay_ms + self.slack_ms) as f64,
                ),
            };
            if wall > now as f64 {
                break;
            }
            if Some(p.session) != self.out_session {
                self.state = State::Resync;
                self.mode = Mode::Keyframe;
                self.ramp = None;
                self.xcode = None;
                return true;
            }
            let p = self.pop_front().unwrap();
            let speed = self.ramp.as_ref().map(|r| r.speed).unwrap_or(1.0);

            // Ramp reached its target? Continue normally from this packet on.
            if let Some(r) = &self.ramp {
                let cur = wall - p.arrive as f64;
                if (r.speed < 1.0 && cur >= r.target as f64)
                    || (r.speed > 1.0 && cur <= r.target as f64)
                {
                    let target = r.target;
                    offset = (out_f - p.ts as f64).round() as i64;
                    self.offset = Some(offset);
                    self.delay_ms = target;
                    self.slack_ms =
                        (wall - p.arrive as f64 - target as f64).max(0.0).round() as u64;
                    self.ramp = None;
                    self.begin_audio_handback();
                    self.notice(format!("Delay is now {:.0}s", target as f64 / 1000.0));
                }
            }

            let k = p.kind.idx();
            let mut ts = out_f.round() as i64;
            if p.kind == Kind::Audio && !p.header {
                if self.xcode.is_some() {
                    self.feed_audio(&p);
                    self.last_emit = Some(LastEmit {
                        m: p.ts,
                        out: out_f,
                        wall,
                        arrive: p.arrive,
                    });
                    self.last_sent_arrive = Some(p.arrive);
                    continue;
                }
                if self.ramp.is_none() {
                    ts += self.audio_adjust;
                }
                if ts <= self.last_out[k] {
                    continue; // would go backwards: drop
                }
            } else {
                ts = ts.max(self.last_out[k]).max(0);
            }
            if ts < 0 {
                continue;
            }
            let data = if p.kind == Kind::Video && !p.header && speed != 1.0 {
                media::scale_composition_offset(&p.data, 1.0 / speed)
            } else {
                p.data.clone()
            };
            self.emit(&p.kind, ts, data, p.key, p.header);
            if p.kind == Kind::Audio && !p.header {
                self.remember_audio(p.data.clone());
            }
            if p.key {
                self.last_sent_key = Some(CapturedKey {
                    session: p.session,
                    data: p.data,
                    info: p.info,
                });
            }
            self.last_emit = Some(LastEmit {
                m: p.ts,
                out: out_f,
                wall,
                arrive: p.arrive,
            });
            self.last_sent_arrive = Some(p.arrive);
        }
        false
    }

    /// Route one original audio frame through the time-stretcher.
    fn feed_audio(&mut self, p: &QPacket) {
        let Some(x) = self.xcode.as_mut() else { return };
        let out = x.t.push(&p.data);
        x.fed.push_back((x.fed_count, p.ts, p.data.clone()));
        x.fed_count += 1;
        while x.fed.len() > 64 {
            x.fed.pop_front();
        }
        let mut emits = Vec::with_capacity(out.len());
        for f in out {
            emits.push((x.next_ts.round() as i64, f));
            x.next_ts += x.frame_ms;
        }
        let settled = x.t.wsola.settled();
        let done = match x.finishing_left.as_mut() {
            Some(n) if settled => {
                *n = n.saturating_sub(1);
                *n == 0
            }
            _ => false,
        };
        for (ts, f) in emits {
            if ts > self.last_out[Kind::Audio.idx()] {
                self.emit(&Kind::Audio, ts, f, false, false);
            }
        }
        if done {
            self.finish_audio_handback();
        }
    }

    fn emit(&mut self, kind: &Kind, ts: i64, data: Bytes, key: bool, header: bool) {
        if *kind == Kind::Audio && header {
            self.audio_cfg = media::parse_aac_header(&data);
            self.audio_header = Some(data.clone());
        }
        let k = kind.idx();
        self.last_out[k] = self.last_out[k].max(ts);
        self.last_out_max = self.last_out_max.max(ts);
        if *kind == Kind::Video && !header {
            self.last_video_pts = self
                .last_video_pts
                .max(ts + media::composition_offset(&data));
        }
        self.output_started = true;
        let now = self.now();
        self.out_rate.add(data.len(), now);
        (self.sink)(EngineEvent::Packet(OutPacket {
            kind: *kind,
            ts,
            data,
            key,
            header,
        }));
    }

    pub fn status(&mut self) -> EngineStatus {
        let now = self.now();
        let switching = self.state == State::Resync && self.offset.is_some();
        let switch_remaining_ms = if switching {
            let target = match self.mode {
                Mode::Continue => self.queue.front().map(|p| p.arrive),
                Mode::Keyframe => self.keys.front().map(|(_, a)| *a),
            };
            target
                .map(|a| (a + self.delay_ms).saturating_sub(now))
                .unwrap_or(self.delay_ms)
        } else {
            0
        };
        EngineStatus {
            delay_ms: self.delay_ms,
            switching,
            filler_active: self.filler.is_some(),
            filler_is_overlay: self.filler.as_ref().map(|f| f.overlay).unwrap_or(false),
            video_paused: self.filler.as_ref().is_some_and(|f| f.video.is_none()),
            switch_remaining_ms,
            catching_up: self.catching_up(),
            ramping: self.ramp.is_some(),
            ramp_speed: self.ramp.as_ref().map(|r| r.speed).unwrap_or(1.0),
            ramp_eta_ms: self.ramp.as_ref().and_then(|r| {
                let cur = self.current_delay()?;
                Some(((r.target as f64 - cur).abs() / (r.speed - 1.0).abs()) as u64)
            }),
            buffered_ms: self.queue.front().map(|p| now - p.arrive).unwrap_or(0),
            buffered_bytes: self.queue_bytes,
            viewer_lag_ms: if switching {
                None
            } else {
                self.last_sent_arrive.map(|a| now - a)
            },
            keyframe_interval_ms: self.key_interval_ms,
            source_active: self.source_active,
            draining: !self.source_active && self.output_started && !self.queue.is_empty(),
            live: self.output_started,
            in_kbps: (self.in_rate.rate(now) * 8.0 / 1000.0).round() as u64,
            out_kbps: (self.out_rate.rate(now) * 8.0 / 1000.0).round() as u64,
        }
    }
}

/// Bitrate meter with fixed 250 ms buckets over a 2 s window (no allocations).
pub struct RateMeter {
    buckets: [f64; 8],
    cur: i64,
}

impl Default for RateMeter {
    fn default() -> Self {
        Self::new()
    }
}

impl RateMeter {
    const BUCKET_MS: u64 = 250;

    pub fn new() -> Self {
        RateMeter {
            buckets: [0.0; 8],
            cur: -1,
        }
    }

    fn advance(&mut self, now: u64) {
        let b = (now / Self::BUCKET_MS) as i64;
        let n = self.buckets.len() as i64;
        if self.cur < 0 || b - self.cur >= n {
            self.buckets = [0.0; 8];
        } else {
            for i in (self.cur + 1)..=b {
                self.buckets[(i % n) as usize] = 0.0;
            }
        }
        if b > self.cur {
            self.cur = b;
        }
    }

    pub fn add(&mut self, bytes: usize, now: u64) {
        self.advance(now);
        let n = self.buckets.len() as i64;
        self.buckets[(self.cur % n) as usize] += bytes as f64;
    }

    /// Bytes per second, excluding the partial current bucket.
    pub fn rate(&mut self, now: u64) -> f64 {
        self.advance(now);
        let n = self.buckets.len() as i64;
        let cur = (self.cur % n) as usize;
        let sum: f64 = self
            .buckets
            .iter()
            .enumerate()
            .filter(|(i, _)| *i != cur)
            .map(|(_, b)| b)
            .sum();
        sum / ((n as u64 - 1) * Self::BUCKET_MS) as f64 * 1000.0
    }
}
