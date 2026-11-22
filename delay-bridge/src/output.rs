//! One destination (Twitch, YouTube, Kick, …): connects when the stream starts, reconnects on
//! network drops, and closes cleanly when the stream ends.

use crate::Logger;
use crate::config::Destination;
use crate::engine::RateMeter;
use crate::media::{Kind, OutPacket};
use crate::rtmp::client::{self, Publisher};
use serde::Serialize;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::sync::mpsc::{self, error::TrySendError};
use tokio::task::JoinHandle;

/// Packets buffered per destination before we start dropping to the next keyframe
/// (only happens if your upload can't keep up).
const CHANNEL_CAP: usize = 4096;

pub enum OutMsg {
    Packet(OutPacket),
    End,
}

#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct OutputStatus {
    pub name: String,
    pub enabled: bool,
    pub state: String,
    pub error: Option<String>,
    pub kbps: u64,
    #[serde(rename = "backlogKB")]
    pub backlog_kb: u64,
    pub dropped: u64,
}

struct Shared {
    state: &'static str,
    error: Option<String>,
    rate: RateMeter,
    dropped: u64,
    epoch: Instant,
}

impl Shared {
    fn set(&mut self, state: &'static str, error: Option<String>) {
        self.state = state;
        self.error = error;
    }
}

pub struct OutputHandle {
    pub name: String,
    pub enabled: bool,
    tx: mpsc::Sender<OutMsg>,
    shared: Arc<Mutex<Shared>>,
    dropping: bool,
    task: JoinHandle<()>,
}

impl OutputHandle {
    pub fn spawn(dest: Destination, log: Logger) -> Self {
        let (tx, rx) = mpsc::channel(CHANNEL_CAP);
        let shared = Arc::new(Mutex::new(Shared {
            state: "idle",
            error: None,
            rate: RateMeter::new(),
            dropped: 0,
            epoch: Instant::now(),
        }));
        let task = tokio::spawn(run(dest.clone(), rx, shared.clone(), log));
        OutputHandle {
            name: dest.name,
            enabled: dest.enabled,
            tx,
            shared,
            dropping: false,
            task,
        }
    }

    /// Hand a packet to this destination without ever blocking the delay engine.
    pub fn offer(&mut self, p: &OutPacket) {
        if !self.enabled {
            return;
        }
        let is_key = p.kind == Kind::Video && p.key;
        if self.dropping && !p.header && !is_key {
            return;
        }
        match self.tx.try_send(OutMsg::Packet(p.clone())) {
            Ok(()) => {
                if is_key {
                    self.dropping = false;
                }
            }
            Err(TrySendError::Full(_)) => {
                self.dropping = true;
                self.shared.lock().unwrap().dropped += 1;
            }
            Err(TrySendError::Closed(_)) => {}
        }
    }

    pub fn end(&self) {
        if self.tx.try_send(OutMsg::End).is_err() {
            let tx = self.tx.clone();
            tokio::spawn(async move {
                let _ = tx.send(OutMsg::End).await;
            });
        }
    }

    pub fn is_busy(&self) -> bool {
        self.shared.lock().unwrap().state != "idle"
    }

    pub fn status(&self) -> OutputStatus {
        let mut s = self.shared.lock().unwrap();
        let now = s.epoch.elapsed().as_millis() as u64;
        OutputStatus {
            name: self.name.clone(),
            enabled: self.enabled,
            state: if self.enabled {
                s.state.to_string()
            } else {
                "disabled".into()
            },
            error: s.error.clone(),
            kbps: (s.rate.rate(now) * 8.0 / 1000.0).round() as u64,
            backlog_kb: 0,
            dropped: s.dropped,
        }
    }
}

impl Drop for OutputHandle {
    fn drop(&mut self) {
        self.task.abort();
    }
}

/// Most data kept per destination for the current keyframe group (see `Memory`).
const GOP_LIMIT_BYTES: usize = 48 * 1024 * 1024;

/// What a destination must remember so a (re)connection can start cleanly: the sequence
/// headers, plus everything since the most recent keyframe. Players can only start decoding at a
/// keyframe, and with OBS's keyframe interval on "auto" the next one may be many seconds away —
/// so instead of waiting for it we resend the current group from its keyframe.
#[derive(Default)]
struct Memory {
    headers: [Option<OutPacket>; 3],
    gop: Vec<OutPacket>,
    gop_bytes: usize,
    gop_valid: bool,
}

fn remember(mem: &mut Memory, p: &OutPacket) {
    if p.header {
        mem.headers[p.kind.idx()] = Some(p.clone());
        return;
    }
    if p.kind == Kind::Video && p.key {
        mem.gop.clear();
        mem.gop_bytes = 0;
        mem.gop_valid = true;
    }
    if mem.gop_valid {
        mem.gop_bytes += p.data.len();
        mem.gop.push(p.clone());
        if mem.gop_bytes > GOP_LIMIT_BYTES {
            // Keyframes are too far apart to keep the whole group; a reconnect will have to wait
            // for the next keyframe.
            mem.gop = Vec::new();
            mem.gop_bytes = 0;
            mem.gop_valid = false;
        }
    }
}

enum Wait<T> {
    Done(T),
    Ended,
    Closed,
}

/// Await `fut` while still consuming packets (remembering headers) so the channel never fills.
async fn while_draining<F: std::future::Future>(
    fut: F,
    rx: &mut mpsc::Receiver<OutMsg>,
    headers: &mut Memory,
) -> Wait<F::Output> {
    tokio::pin!(fut);
    loop {
        tokio::select! {
            r = &mut fut => return Wait::Done(r),
            m = rx.recv() => match m {
                None => return Wait::Closed,
                Some(OutMsg::End) => return Wait::Ended,
                Some(OutMsg::Packet(p)) => remember(headers, &p),
            }
        }
    }
}

async fn run(
    dest: Destination,
    mut rx: mpsc::Receiver<OutMsg>,
    shared: Arc<Mutex<Shared>>,
    log: Logger,
) {
    let name = dest.name.clone();
    let set = |state: &'static str, err: Option<String>| shared.lock().unwrap().set(state, err);
    let mut headers = Memory::default();

    'idle: loop {
        // Wait for the stream to start.
        loop {
            match rx.recv().await {
                None => return,
                Some(OutMsg::Packet(p)) => {
                    remember(&mut headers, &p);
                    break;
                }
                Some(OutMsg::End) => {}
            }
        }

        let target = match client::parse_url(&dest.url, &dest.key) {
            Ok(t) if !t.key.is_empty() => Some(t),
            Ok(_) => {
                let e = "No stream key — add it in Settings".to_string();
                log(format!("{name}: {e}"));
                set("error", Some(e));
                None
            }
            Err(e) => {
                log(format!("{name}: {e}"));
                set("error", Some(e));
                None
            }
        };
        let Some(target) = target else {
            // Configuration problem: sit out this stream instead of retrying in a loop.
            if let Wait::Closed =
                while_draining(std::future::pending::<()>(), &mut rx, &mut headers).await
            {
                return;
            }
            headers = Memory::default();
            set("idle", None);
            continue 'idle;
        };

        let mut attempts: u32 = 0;
        'conn: loop {
            let prev_err = shared.lock().unwrap().error.clone();
            set(
                if attempts == 0 {
                    "connecting"
                } else {
                    "reconnecting"
                },
                prev_err,
            );
            let mut publisher: Publisher =
                match while_draining(client::connect(&target), &mut rx, &mut headers).await {
                    Wait::Closed => return,
                    Wait::Ended => break 'conn,
                    Wait::Done(Ok(p)) => p,
                    Wait::Done(Err(e)) => {
                        attempts += 1;
                        let wait = Duration::from_secs((1u64 << (attempts - 1).min(5)).min(30));
                        log(format!("{name}: {e} — retrying in {}s", wait.as_secs()));
                        set("reconnecting", Some(e.to_string()));
                        match while_draining(tokio::time::sleep(wait), &mut rx, &mut headers).await
                        {
                            Wait::Closed => return,
                            Wait::Ended => break 'conn,
                            Wait::Done(_) => continue 'conn,
                        }
                    }
                };

            attempts = 0;
            set("live", None);
            log(format!("{name}: live"));
            for h in headers.headers.iter().flatten() {
                publisher.queue(h, 0);
            }
            let mut base: Option<i64> = None;
            let mut need_key = true;
            // Start from the most recent keyframe so viewers get a picture right away.
            if headers.gop_valid
                && let Some(first) = headers.gop.first()
            {
                let b = first.ts;
                base = Some(b);
                need_key = false;
                for p in &headers.gop {
                    publisher.queue(p, (p.ts - b).max(0) as u32);
                }
            }

            let lost = loop {
                tokio::select! {
                    m = rx.recv() => {
                        let mut msg = m;
                        let mut ended = false;
                        // Handle this message plus anything else already waiting: one write per batch.
                        for _ in 0..512 {
                            match msg {
                                None => { publisher.close().await; return; }
                                Some(OutMsg::End) => { ended = true; break; }
                                Some(OutMsg::Packet(p)) => {
                                    remember(&mut headers, &p);
                                    if p.header {
                                        let ts = base.map(|b| (p.ts - b).max(0)).unwrap_or(0);
                                        publisher.queue(&p, ts as u32);
                                    } else {
                                        let is_key = p.kind == Kind::Video && p.key;
                                        if base.is_none() && is_key { base = Some(p.ts); }
                                        if let Some(b) = base {
                                            if need_key && is_key { need_key = false; }
                                            if !need_key {
                                                publisher.queue(&p, (p.ts - b).max(0) as u32);
                                                let mut s = shared.lock().unwrap();
                                                let now = s.epoch.elapsed().as_millis() as u64;
                                                s.rate.add(p.data.len(), now);
                                            }
                                        }
                                    }
                                }
                            }
                            match rx.try_recv() {
                                Ok(next) => msg = Some(next),
                                Err(_) => break,
                            }
                        }
                        if let Err(e) = publisher.flush().await {
                            break Some(e.to_string());
                        }
                        if ended {
                            publisher.close().await;
                            break None;
                        }
                    }
                    c = publisher.ctrl.recv() => match c {
                        Some(bytes) => {
                            publisher.queue_raw(&bytes);
                            if let Err(e) = publisher.flush().await { break Some(e.to_string()); }
                        }
                        None => break Some("Connection closed by server".to_string()),
                    }
                }
            };

            match lost {
                None => break 'conn, // stream ended normally
                Some(e) => {
                    attempts += 1;
                    log(format!("{name}: {e} — reconnecting"));
                    set("reconnecting", Some(e));
                    match while_draining(
                        tokio::time::sleep(Duration::from_secs(1)),
                        &mut rx,
                        &mut headers,
                    )
                    .await
                    {
                        Wait::Closed => return,
                        Wait::Ended => break 'conn,
                        Wait::Done(_) => continue 'conn,
                    }
                }
            }
        }
        log(format!("{name}: stream ended"));
        headers = Memory::default();
        set("idle", None);
    }
}
