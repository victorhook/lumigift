//! The radio scheduler thread. It alone owns the transport and decides what
//! goes on air: show frames (highest priority), queued unicast exchanges and
//! broadcast jobs from operation threads, and telemetry polls in the gaps.

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, Sender};
use lumigift_proto::{self as lp, Broadcast, Packet, Reply, Unicast};
use nrf24_usb::Transport;
use nrf24_usb_proto::{CmdStatus, Command, Event, Mode, Payload, TxStatus, tx_flags};

use crate::clock::ClockEstimator;
use crate::config::RadioSettings;
use crate::state::Shared;
use crate::types::*;

pub(crate) type Connector = Box<dyn FnMut() -> Result<Box<dyn Transport>, nrf24_usb::Error> + Send>;

// ---------------------------------------------------------------------------
// Requests from operation threads
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum UniMode {
    /// Send the packet itself (normally a Poll); success = acknowledged.
    /// The ack payload is decoded if possible.
    PollOnly,
    /// Send the command, then a Poll, and return the reply whose echo
    /// matches the command's seq. With `accept_cmd_ack`, an acknowledged
    /// command counts as success even without a matching reply (Reboot).
    Collect { accept_cmd_ack: bool },
}

impl UniMode {
    /// Command + Poll; only a matching reply counts.
    pub const COLLECT: UniMode = UniMode::Collect {
        accept_cmd_ack: false,
    };
}

/// Outcome of a unicast exchange: the decoded reply, if any.
pub(crate) type UniResult = Result<Option<Reply>, LinkError>;

pub(crate) struct UniReq {
    pub addr: u8,
    pub msg: Unicast,
    pub mode: UniMode,
    pub attempts: u8,
    pub reply: Sender<UniResult>,
}

pub(crate) struct BcastReq {
    pub packets: Vec<Payload>,
    /// Packets per second (None = as fast as flow control allows).
    pub rate: Option<f32>,
    /// May also go out in the `tick_guard` before a show tick (for a burst
    /// of tiny packets that must not pause: the `Wake` burst).
    pub gapless: bool,
    /// Resolves to the number of packets the bridge sent.
    pub reply: Sender<Result<u32, LinkError>>,
}

pub(crate) enum RadioReq {
    Unicast(UniReq),
    Broadcast(BcastReq),
    Reconfigure {
        radio: RadioSettings,
        reply: Sender<Result<(), LinkError>>,
    },
}

/// Handle used by operation threads to talk to the scheduler.
#[derive(Clone)]
pub(crate) struct Radio {
    pub tx: Sender<RadioReq>,
}

const REQ_TIMEOUT: Duration = Duration::from_secs(10);

/// Wait for a scheduler reply; a missing one counts as a timeout.
pub(crate) fn wait_reply<T>(
    rx: &Receiver<Result<T, LinkError>>,
    timeout: Duration,
    what: &str,
) -> Result<T, LinkError> {
    rx.recv_timeout(timeout)
        .unwrap_or_else(|_| Err(LinkError::Timeout(what.into())))
}

impl Radio {
    fn submit(&self, req: RadioReq) {
        if let Err(e) = self.tx.send(req) {
            reject(e.0, LinkError::Shutdown);
        }
    }

    pub fn unicast_async(
        &self,
        addr: u8,
        msg: Unicast,
        mode: UniMode,
        attempts: u8,
    ) -> Receiver<UniResult> {
        let (tx, rx) = crossbeam_channel::bounded(1);
        self.submit(RadioReq::Unicast(UniReq {
            addr,
            msg,
            mode,
            attempts: attempts.max(1),
            reply: tx,
        }));
        rx
    }

    pub fn unicast(&self, addr: u8, msg: Unicast, mode: UniMode, attempts: u8) -> UniResult {
        let rx = self.unicast_async(addr, msg, mode, attempts);
        wait_reply(&rx, REQ_TIMEOUT, "radio request")
    }

    /// Several unicasts at once (all queued before waiting for any), keyed by `K`.
    pub fn unicast_many<K>(
        &self,
        reqs: impl IntoIterator<Item = (K, u8, Unicast)>,
        mode: UniMode,
        attempts: u8,
        timeout: Duration,
        what: &str,
    ) -> Vec<(K, UniResult)> {
        let pending: Vec<_> = reqs
            .into_iter()
            .map(|(k, addr, msg)| (k, self.unicast_async(addr, msg, mode, attempts)))
            .collect();
        pending
            .into_iter()
            .map(|(k, rx)| (k, wait_reply(&rx, timeout, what)))
            .collect()
    }

    /// Command + Poll, returning the matching reply.
    pub fn command(&self, addr: u8, msg: Unicast, attempts: u8) -> Result<Reply, LinkError> {
        self.unicast(addr, msg, UniMode::COLLECT, attempts)?
            .ok_or(LinkError::NoReply)
    }

    /// Broadcast `packets` and wait; resolves to the number sent.
    pub fn broadcast(&self, packets: Vec<Payload>, rate: Option<f32>) -> Result<u32, LinkError> {
        let n = packets.len();
        let rx = self.broadcast_async(packets, rate);
        let budget =
            REQ_TIMEOUT + Duration::from_secs_f32(n as f32 / rate.unwrap_or(500.0).max(1.0));
        wait_reply(&rx, budget, "broadcast")
    }

    /// Queue a broadcast job and return at once. Jobs run back to back in
    /// submission order (a job starts as soon as the previous one has
    /// handed its last packet to the bridge), each paced at its `rate`.
    pub fn broadcast_async(
        &self,
        packets: Vec<Payload>,
        rate: Option<f32>,
    ) -> Receiver<Result<u32, LinkError>> {
        self.broadcast_job(packets, rate, false)
    }

    /// [`broadcast_async`](Self::broadcast_async) for a continuous burst of
    /// tiny packets: it keeps going through the `tick_guard` before show
    /// ticks (a 1-byte packet is ~80 µs of air, so the few in flight delay a
    /// show packet by well under a millisecond), so the burst has no gap
    /// long enough to swallow a dozing gift's 2 ms listen window.
    pub fn broadcast_gapless(&self, packets: Vec<Payload>) -> Receiver<Result<u32, LinkError>> {
        self.broadcast_job(packets, None, true)
    }

    fn broadcast_job(
        &self,
        packets: Vec<Payload>,
        rate: Option<f32>,
        gapless: bool,
    ) -> Receiver<Result<u32, LinkError>> {
        let (tx, rx) = crossbeam_channel::bounded(1);
        self.submit(RadioReq::Broadcast(BcastReq {
            packets,
            rate,
            gapless,
            reply: tx,
        }));
        rx
    }

    pub fn reconfigure(&self, radio: RadioSettings) -> Result<(), LinkError> {
        let (tx, rx) = crossbeam_channel::bounded(1);
        self.submit(RadioReq::Reconfigure { radio, reply: tx });
        wait_reply(&rx, REQ_TIMEOUT, "reconfigure")
    }
}

pub(crate) fn encode(p: Packet) -> Payload {
    let mut buf = [0u8; lp::MAX_PAYLOAD];
    let n = p.encode(&mut buf);
    Payload::new(&buf[..n]).expect("lumigift packets fit 32 bytes")
}

/// `u` with its sequence number replaced.
fn with_seq(mut u: Unicast, new: u8) -> Unicast {
    match &mut u {
        Unicast::Poll { seq }
        | Unicast::GetIdentity { seq }
        | Unicast::Blink { seq, .. }
        | Unicast::SetSlot { seq, .. }
        | Unicast::Reboot { seq, .. }
        | Unicast::SetConfig { seq, .. }
        | Unicast::OtaQuery { seq, .. }
        | Unicast::OtaCommit { seq, .. } => *seq = new,
    }
    u
}

fn reply_echo(r: &Reply) -> u8 {
    match r {
        Reply::Status(s) => s.echo,
        Reply::Identity(i) => i.echo,
        Reply::OtaBitmap(o) => o.echo,
        Reply::ConfigResult { echo, .. } => *echo,
    }
}

// ---------------------------------------------------------------------------
// Scheduler internals
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy)]
enum TxCtx {
    Show,
    Poll { addr: u8 },
    UniCmd { id: u64 },
    UniPoll { id: u64 },
    Bcast { job: u64 },
}

struct InFlight {
    ctx: TxCtx,
    slot: u8,
    sent: Instant,
    addr: u8,
    seq: u8,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Stage {
    SendCmd,
    CmdWait,
    SendPoll,
    PollWait,
}

struct ActiveUni {
    req: UniReq,
    cmd_seq: u8,
    stage: Stage,
    tries: u8,
    cmd_acked: bool,
    any_ack: bool,
    created: Instant,
}

struct BcastJob {
    id: u64,
    packets: VecDeque<Payload>,
    rate: Option<f32>,
    gapless: bool,
    start: Option<Instant>,
    sent: u32,
    in_flight: u32,
    failed: u32,
    reply: Sender<Result<u32, LinkError>>,
}

struct PollEntry {
    addr: u8,
    online: bool,
    next: Instant,
}

const TX_SLOTS: usize = nrf24_usb_proto::TX_ADDR_SLOTS as usize;

/// Bridge TX address slots: 0 = broadcast, 1.. = LRU cache of unicast addresses.
struct TxSlots {
    addr: [Option<u8>; TX_SLOTS],
    refs: [u32; TX_SLOTS],
    used: [u64; TX_SLOTS],
    clock: u64,
}

impl TxSlots {
    fn new() -> Self {
        let mut addr = [None; TX_SLOTS];
        addr[0] = Some(lp::BROADCAST_LSB);
        Self {
            addr,
            refs: [0; TX_SLOTS],
            used: [0; TX_SLOTS],
            clock: 0,
        }
    }
    /// Slot for `addr`, emitting `SetTxAddr` into `out` if it must be loaded.
    fn acquire(&mut self, addr: u8, out: &mut Vec<Command>) -> Option<u8> {
        self.clock += 1;
        if let Some(i) = (1..self.addr.len()).find(|&i| self.addr[i] == Some(addr)) {
            self.refs[i] += 1;
            self.used[i] = self.clock;
            return Some(i as u8);
        }
        let i = (1..self.addr.len())
            .filter(|&i| self.refs[i] == 0)
            .min_by_key(|&i| self.used[i])?;
        self.addr[i] = Some(addr);
        self.refs[i] = 1;
        self.used[i] = self.clock;
        out.push(Command::SetTxAddr {
            slot: i as u8,
            addr: lp::address(addr),
        });
        Some(i as u8)
    }
    fn release(&mut self, slot: u8) {
        if slot != 0 {
            let r = &mut self.refs[slot as usize];
            *r = r.saturating_sub(1);
        }
    }
}

#[derive(Default, Clone, Copy)]
struct Counters {
    ticks: u64,
    show_packets: u64,
    polls: u64,
    polls_ok: u64,
}

/// (command kind, sent at, reply) awaiting a `CmdResult`.
type CmdWaiter = (u8, Instant, Sender<Result<(), LinkError>>);

pub(crate) struct Scheduler {
    shared: Arc<Shared>,
    connector: Connector,
    transport: Option<Box<dyn Transport>>,
    req_rx: Receiver<RadioReq>,
    radio: RadioSettings,
    depth: usize,
    out: Vec<Command>,
    // TX bookkeeping
    next_tag: u16,
    in_flight: HashMap<u16, InFlight>,
    uni_in_flight: usize,
    slots: TxSlots,
    seq: u8,
    // show
    next_tick: Instant,
    idle_ticks: u32,
    /// Next layer to send when a `Layers` output has more than fit a tick.
    layer_rr: usize,
    pending_show: VecDeque<Payload>,
    show_seq: u8,
    /// Show packets the bridge reported transmitted (TxDone order = air order).
    show_done: u64,
    // unicast exchanges
    queue: VecDeque<UniReq>,
    active: HashMap<u64, ActiveUni>,
    next_id: u64,
    poll_addrs: HashSet<u8>,
    // broadcasts
    bjobs: VecDeque<BcastJob>,
    // telemetry polling
    poll_sched: Vec<PollEntry>,
    poll_tokens: f32,
    last_token: Instant,
    seq_frames: HashMap<u8, VecDeque<(u8, u64)>>,
    // misc
    cmd_waiters: VecDeque<CmdWaiter>,
    clock: ClockEstimator,
    next_ping: Instant,
    pings_fast: u32,
    next_stats: Instant,
    next_house: Instant,
    next_history: Instant,
    next_rates: Instant,
    rates_ref: (Instant, Counters),
    next_connect: Instant,
    counters: Counters,
    tx_total: u64,
    tx_acked: u64,
    tx_failed: u64,
    tx_lost: u64,
    show_dropped: u64,
    max_in_flight: usize,
}

const SETUP_TIMEOUT: Duration = Duration::from_millis(1500);
const TXDONE_TIMEOUT: Duration = Duration::from_secs(1);
const HOUSEKEEPING: Duration = Duration::from_millis(50);

impl Scheduler {
    pub fn new(shared: Arc<Shared>, connector: Connector, req_rx: Receiver<RadioReq>) -> Self {
        let now = Instant::now();
        let radio = shared.cfg.radio;
        let epoch = shared.epoch;
        Self {
            shared,
            connector,
            transport: None,
            req_rx,
            radio,
            depth: 4,
            out: Vec::new(),
            next_tag: 0,
            in_flight: HashMap::new(),
            uni_in_flight: 0,
            slots: TxSlots::new(),
            seq: 0,
            next_tick: now,
            idle_ticks: 0,
            layer_rr: 0,
            pending_show: VecDeque::new(),
            show_seq: 0,
            show_done: 0,
            queue: VecDeque::new(),
            active: HashMap::new(),
            next_id: 1,
            poll_addrs: HashSet::new(),
            bjobs: VecDeque::new(),
            poll_sched: Vec::new(),
            poll_tokens: 0.0,
            last_token: now,
            seq_frames: HashMap::new(),
            cmd_waiters: VecDeque::new(),
            clock: ClockEstimator::new(epoch),
            next_ping: now,
            pings_fast: 0,
            next_stats: now,
            next_house: now,
            next_history: now,
            next_rates: now + Duration::from_secs(1),
            rates_ref: (now, Counters::default()),
            next_connect: now,
            counters: Counters::default(),
            tx_total: 0,
            tx_acked: 0,
            tx_failed: 0,
            tx_lost: 0,
            show_dropped: 0,
            max_in_flight: 0,
        }
    }

    pub fn run(mut self) {
        while !self.shared.stopping() {
            let now = Instant::now();
            if self.transport.is_none() {
                self.drain_requests_disconnected();
                if now >= self.next_connect {
                    self.connect();
                    self.next_connect = Instant::now() + Duration::from_secs(1);
                }
                if self.transport.is_none() {
                    self.housekeeping(Instant::now());
                    std::thread::sleep(Duration::from_millis(20));
                    continue;
                }
            }
            self.accept_requests();
            self.timers(now);
            self.show_tick(now);
            self.fill(now);
            self.flush();
            if now >= self.next_house {
                self.housekeeping(now);
            }
            // Wait for events, at most until the next show tick / 1 ms.
            let mut wait = Duration::from_millis(1);
            if self.show_active() {
                wait = wait.min(self.next_tick.saturating_duration_since(Instant::now()));
            }
            let Some(t) = self.transport.as_mut() else {
                continue;
            };
            match t.recv(wait) {
                Ok(Some(ev)) => {
                    self.on_event(ev);
                    // Drain whatever else is already there.
                    for _ in 0..64 {
                        let Some(t) = self.transport.as_mut() else {
                            break;
                        };
                        match t.recv(Duration::ZERO) {
                            Ok(Some(ev)) => self.on_event(ev),
                            Ok(None) => break,
                            Err(e) => {
                                self.disconnect(e.to_string());
                                break;
                            }
                        }
                    }
                }
                Ok(None) => {}
                Err(e) => self.disconnect(e.to_string()),
            }
        }
        self.fail_all(LinkError::Shutdown);
    }

    fn show_active(&self) -> bool {
        !self.shared.show_paused.load(Ordering::Relaxed)
    }

    fn cap(&self) -> usize {
        self.depth.min(self.shared.cfg.max_in_flight).max(1)
    }

    // -- connection ---------------------------------------------------------

    fn connect(&mut self) {
        self.shared.lock().bridge.state = BridgeState::Connecting;
        let mut t = match (self.connector)() {
            Ok(t) => t,
            Err(e) => {
                self.set_disconnected(e.to_string(), false);
                return;
            }
        };
        match self.setup(&mut *t) {
            Ok(info) => {
                self.depth = (info.tx_queue_depth as usize).max(1);
                self.slots = TxSlots::new();
                self.clock.reset();
                self.pings_fast = 0;
                self.next_ping = Instant::now();
                self.next_tick = Instant::now();
                let desc = t.describe();
                self.transport = Some(t);
                self.shared.connected.store(true, Ordering::Relaxed);
                let mut st = self.shared.lock();
                st.bridge.state = BridgeState::Connected;
                st.bridge.info = Some(info);
                st.bridge.description = desc.clone();
                st.bridge.radio = self.radio;
                st.log(
                    LogLevel::Info,
                    None,
                    format!(
                        "connected to {desc} (fw {}, queue depth {}, channel {})",
                        info.fw_version, info.tx_queue_depth, self.radio.channel
                    ),
                );
            }
            Err(e) => self.set_disconnected(format!("bridge setup failed: {e}"), true),
        }
    }

    fn setup(&self, t: &mut dyn Transport) -> Result<nrf24_usb_proto::Info, String> {
        t.send(&[Command::GetInfo]).map_err(|e| e.to_string())?;
        let deadline = Instant::now() + SETUP_TIMEOUT;
        let info = loop {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return Err("no Info from bridge".into());
            }
            if let Some(Event::Info(i)) = t.recv(left).map_err(|e| e.to_string())? {
                break i;
            }
        };
        if info.protocol_version != nrf24_usb_proto::PROTOCOL_VERSION {
            return Err(format!(
                "bridge speaks protocol v{}, host v{}",
                info.protocol_version,
                nrf24_usb_proto::PROTOCOL_VERSION
            ));
        }
        if !info.radio_present {
            return Err("bridge reports no nRF24 radio".into());
        }
        let cmds = [
            Command::Configure(self.radio.to_radio_config()),
            Command::SetTxAddr {
                slot: 0,
                addr: lp::BROADCAST_ADDR,
            },
            Command::SetMode(Mode::Standby),
        ];
        t.send(&cmds).map_err(|e| e.to_string())?;
        let mut expect: VecDeque<u8> = cmds.iter().map(|c| c.kind()).collect();
        while let Some(&k) = expect.front() {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return Err(format!("no CmdResult for command kind {k:#04x}"));
            }
            if let Some(Event::CmdResult { kind, status }) =
                t.recv(left).map_err(|e| e.to_string())?
                && kind == k
            {
                expect.pop_front();
                if status != CmdStatus::Ok {
                    return Err(format!("command kind {k:#04x} failed: {status:?}"));
                }
            }
        }
        Ok(info)
    }

    fn set_disconnected(&mut self, err: String, log_it: bool) {
        let mut st = self.shared.lock();
        let changed = st.bridge.state != BridgeState::Disconnected(err.clone());
        st.bridge.state = BridgeState::Disconnected(err.clone());
        st.bridge.clock_synced = false;
        if log_it || changed {
            st.log(LogLevel::Error, None, err);
        }
    }

    fn disconnect(&mut self, err: String) {
        self.transport = None;
        self.shared.connected.store(false, Ordering::Relaxed);
        self.fail_all(LinkError::NotConnected);
        self.set_disconnected(format!("bridge lost: {err}"), true);
        self.next_connect = Instant::now() + Duration::from_millis(500);
    }

    fn fail_all(&mut self, e: LinkError) {
        for (_, a) in self.active.drain() {
            let _ = a.req.reply.send(Err(e.clone()));
        }
        for r in self.queue.drain(..) {
            let _ = r.reply.send(Err(e.clone()));
        }
        for j in self.bjobs.drain(..) {
            let _ = j.reply.send(Err(e.clone()));
        }
        for (_, _, w) in self.cmd_waiters.drain(..) {
            let _ = w.send(Err(e.clone()));
        }
        while let Ok(r) = self.req_rx.try_recv() {
            reject(r, e.clone());
        }
        self.in_flight.clear();
        self.uni_in_flight = 0;
        self.poll_addrs.clear();
        self.pending_show.clear();
        self.out.clear();
    }

    fn drain_requests_disconnected(&mut self) {
        while let Ok(r) = self.req_rx.try_recv() {
            match r {
                // Remember new radio settings for the next connection.
                RadioReq::Reconfigure { radio, reply } => {
                    self.radio = radio;
                    self.shared.lock().bridge.radio = radio;
                    let _ = reply.send(Ok(()));
                }
                other => reject(other, LinkError::NotConnected),
            }
        }
    }

    fn flush(&mut self) {
        if self.out.is_empty() {
            return;
        }
        let Some(t) = self.transport.as_mut() else {
            self.out.clear();
            return;
        };
        let r = t.send(&self.out);
        self.out.clear();
        if let Err(e) = r {
            self.disconnect(e.to_string());
        }
    }

    // -- requests -----------------------------------------------------------

    fn accept_requests(&mut self) {
        loop {
            match self.req_rx.try_recv() {
                Ok(RadioReq::Unicast(r)) => self.queue.push_back(r),
                Ok(RadioReq::Broadcast(b)) => {
                    let id = self.next_id;
                    self.next_id += 1;
                    self.bjobs.push_back(BcastJob {
                        id,
                        packets: b.packets.into(),
                        rate: b.rate,
                        gapless: b.gapless,
                        start: None,
                        sent: 0,
                        in_flight: 0,
                        failed: 0,
                        reply: b.reply,
                    });
                }
                Ok(RadioReq::Reconfigure { radio, reply }) => {
                    self.radio = radio;
                    let cmd = Command::Configure(radio.to_radio_config());
                    self.cmd_waiters
                        .push_back((cmd.kind(), Instant::now(), reply));
                    self.out.push(cmd);
                    self.shared.lock().bridge.radio = radio;
                }
                Err(_) => break,
            }
        }
    }

    // -- timers -------------------------------------------------------------

    fn timers(&mut self, now: Instant) {
        if now >= self.next_ping {
            let seq = self.clock.ping_sent(now);
            self.out.push(Command::Ping { seq });
            self.pings_fast += 1;
            self.next_ping = now
                + if self.pings_fast < 10 {
                    Duration::from_millis(30)
                } else {
                    Duration::from_millis(500)
                };
        }
        if now >= self.next_stats {
            self.out.push(Command::GetStats);
            self.next_stats = now + Duration::from_secs(1);
        }
    }

    fn show_tick(&mut self, now: Instant) {
        if !self.show_active() {
            if !self.pending_show.is_empty() {
                self.pending_show.clear();
            }
            self.shared.show_events.store(0, Ordering::Relaxed);
            self.next_tick = now;
            return;
        }
        if now < self.next_tick {
            return;
        }
        let period = self.shared.cfg.show_period;
        self.next_tick += period;
        if self.next_tick <= now {
            self.next_tick = now + period;
        }
        // Low latency beats completeness: drop what the last tick couldn't send.
        self.show_dropped += self.pending_show.len() as u64;
        self.pending_show.clear();

        let output = self.shared.show.load();
        // Edge flags latched since the last tick replace the frame's own.
        let events = self.shared.show_events.swap(0, Ordering::Relaxed);
        let latch = |f: &lp::MusicFrame| lp::MusicFrame {
            flags: (f.flags & !EDGE_FLAGS) | events,
            ..*f
        };
        // `seq` is set when the packet is sent, `time_us` stamped by the bridge.
        let (seq, time_us) = (0u8, 0u32);
        let mut push = |b: Broadcast| self.pending_show.push_back(encode(Packet::Broadcast(b)));
        match &**output {
            ShowOutput::Idle => {
                self.idle_ticks += 1;
                let every = (self.shared.cfg.idle_sync_period.as_micros()
                    / period.as_micros().max(1))
                .max(1) as u32;
                if self.idle_ticks >= every {
                    self.idle_ticks = 0;
                    push(Broadcast::Sync { seq, time_us });
                } else {
                    return;
                }
            }
            ShowOutput::Music(frame) => push(Broadcast::Music {
                seq,
                time_us,
                frame: latch(frame),
            }),
            ShowOutput::Layers(layers) => {
                if layers.is_empty() {
                    push(Broadcast::Sync { seq, time_us });
                } else if layers.len() <= crate::types::MAX_LAYERS_PER_TICK {
                    for frame in layers {
                        push(Broadcast::Music {
                            seq,
                            time_us,
                            frame: latch(frame),
                        });
                    }
                } else {
                    // Too many for one tick: rotate, keeping the order
                    // within the tick (later layers still win overlaps).
                    let n = layers.len();
                    let start = self.layer_rr % n;
                    let mut pick: Vec<usize> = (0..crate::types::MAX_LAYERS_PER_TICK)
                        .map(|k| (start + k) % n)
                        .collect();
                    pick.sort_unstable();
                    self.layer_rr = (start + crate::types::MAX_LAYERS_PER_TICK) % n;
                    for i in pick {
                        push(Broadcast::Music {
                            seq,
                            time_us,
                            frame: latch(&layers[i]),
                        });
                    }
                }
            }
            ShowOutput::Blackout => push(Broadcast::Blackout { seq, time_us }),
            ShowOutput::Solid(colors) => {
                for (i, chunk) in colors
                    .chunks(8)
                    .enumerate()
                    .take(lp::MAX_SLOTS as usize / 8)
                {
                    let mut c = [lp::Rgb::BLACK; 8];
                    c[..chunk.len()].copy_from_slice(chunk);
                    push(Broadcast::Solid {
                        seq,
                        time_us,
                        first_slot: (i * 8) as u8,
                        count: chunk.len() as u8,
                        colors: c,
                    });
                }
            }
            ShowOutput::Leds(leds) => {
                for (i, pair) in leds.chunks(2).enumerate().take(lp::MAX_SLOTS as usize / 2) {
                    let slots = [
                        (i * 2) as u8,
                        if pair.len() > 1 {
                            (i * 2 + 1) as u8
                        } else {
                            lp::UNASSIGNED_SLOT
                        },
                    ];
                    let colors = [pair[0], pair.get(1).copied().unwrap_or([lp::Rgb::BLACK; 4])];
                    push(Broadcast::Leds {
                        seq,
                        time_us,
                        slots,
                        colors,
                    });
                }
            }
        }
        self.counters.ticks += 1;
    }

    // -- transmission -------------------------------------------------------

    #[allow(clippy::too_many_arguments)]
    fn push_tx(
        &mut self,
        ctx: TxCtx,
        slot: u8,
        flags: u8,
        ts_offset: u8,
        payload: Payload,
        addr: u8,
        seq: u8,
    ) {
        let mut tag = self.next_tag;
        while self.in_flight.contains_key(&tag) {
            tag = tag.wrapping_add(1);
        }
        self.next_tag = tag.wrapping_add(1);
        self.out.push(Command::Tx {
            tag,
            slot,
            flags,
            ts_offset,
            payload,
        });
        self.in_flight.insert(
            tag,
            InFlight {
                ctx,
                slot,
                sent: Instant::now(),
                addr,
                seq,
            },
        );
        if slot != 0 {
            self.uni_in_flight += 1;
        }
        self.max_in_flight = self.max_in_flight.max(self.in_flight.len());
    }

    fn next_seq(&mut self) -> u8 {
        // Skip 0: a freshly booted gift reports echo 0.
        self.seq = if self.seq == 255 { 1 } else { self.seq + 1 };
        self.seq
    }

    fn send_unicast(&mut self, addr: u8, msg: Unicast, ctx: TxCtx) -> bool {
        let Some(slot) = self.slots.acquire(addr, &mut self.out) else {
            return false;
        };
        let seq = msg.seq();
        self.push_tx(ctx, slot, 0, 0, encode(Packet::Unicast(msg)), addr, seq);
        true
    }

    fn addr_busy(&self, addr: u8) -> bool {
        self.poll_addrs.contains(&addr) || self.active.values().any(|a| a.req.addr == addr)
    }

    fn fill(&mut self, now: Instant) {
        if self.transport.is_none() {
            return;
        }
        let cap = self.cap();
        // 1. Show packets go first and ignore the unicast limits.
        while self.in_flight.len() < cap {
            let Some(mut p) = self.pending_show.pop_front() else {
                break;
            };
            p.as_mut_slice()[1] = self.show_seq;
            self.show_seq = self.show_seq.wrapping_add(1);
            self.counters.show_packets += 1;
            self.push_tx(
                TxCtx::Show,
                0,
                tx_flags::NO_ACK | tx_flags::STAMP,
                lp::SHOW_TIMESTAMP_OFFSET,
                p,
                0,
                0,
            );
        }
        if !self.pending_show.is_empty() {
            return;
        }
        let guarded = self.show_active()
            && self.next_tick.saturating_duration_since(now) < self.shared.cfg.tick_guard;
        // 2. Broadcast jobs (OTA chunks etc.). With the show paused they may use the whole bridge queue.
        let bcap = if self.show_active() {
            cap
        } else {
            self.depth.max(cap)
        };
        let job = self.bjobs.iter_mut().find(|j| !j.packets.is_empty());
        if guarded && !job.as_ref().is_some_and(|j| j.gapless) {
            return;
        }
        if let Some(job) = job {
            let start = *job.start.get_or_insert(now);
            let allowed = match job.rate {
                None => u32::MAX,
                Some(r) => (now.duration_since(start).as_secs_f32() * r) as u32 + 1,
            };
            let id = job.id;
            let mut batch = Vec::new();
            while job.sent < allowed && self.in_flight.len() + batch.len() < bcap {
                let Some(p) = job.packets.pop_front() else {
                    break;
                };
                job.sent += 1;
                job.in_flight += 1;
                batch.push(p);
            }
            for p in batch {
                self.push_tx(TxCtx::Bcast { job: id }, 0, tx_flags::NO_ACK, 0, p, 0, 0);
            }
            self.finish_bcast_if_done();
        }
        if guarded {
            return;
        }
        let mut ucap = self.shared.cfg.max_unicast_in_flight.max(1);
        if self.shared.ota_running.load(Ordering::Relaxed) && !self.show_active() {
            // Firmware update: no show frames to protect, so keep more
            // queries in flight to hide the USB round trips.
            ucap = ucap.max(4);
        }
        // 3. Advance active exchanges.
        let mut ids: Vec<u64> = self
            .active
            .iter()
            .filter(|(_, a)| matches!(a.stage, Stage::SendCmd | Stage::SendPoll))
            .map(|(id, _)| *id)
            .collect();
        ids.sort_unstable();
        for id in ids {
            if self.uni_in_flight >= ucap || self.in_flight.len() >= cap {
                return;
            }
            self.send_exchange_step(id);
        }
        // 4. Start queued exchanges (one per address at a time).
        let mut i = 0;
        while i < self.queue.len() && self.active.len() < ucap {
            if self.uni_in_flight >= ucap || self.in_flight.len() >= cap {
                return;
            }
            if self.addr_busy(self.queue[i].addr) {
                i += 1;
                continue;
            }
            let req = self.queue.remove(i).unwrap();
            let id = self.next_id;
            self.next_id += 1;
            self.active.insert(
                id,
                ActiveUni {
                    req,
                    cmd_seq: 0,
                    stage: Stage::SendCmd,
                    tries: 0,
                    cmd_acked: false,
                    any_ack: false,
                    created: now,
                },
            );
            self.send_exchange_step(id);
        }
        // 5. Telemetry polls in the remaining gaps.
        if self.shared.polling_paused.load(Ordering::Relaxed) {
            return;
        }
        let dt = now.duration_since(self.last_token).as_secs_f32();
        self.last_token = now;
        let rate = self.shared.cfg.max_polls_per_s.max(1.0);
        self.poll_tokens = (self.poll_tokens + dt * rate).min((rate * 0.02).max(2.0));
        while self.poll_tokens >= 1.0 && self.uni_in_flight < ucap && self.in_flight.len() < cap {
            let pick = self
                .poll_sched
                .iter()
                .enumerate()
                .filter(|(_, e)| e.next <= now && !self.addr_busy(e.addr))
                .min_by_key(|(_, e)| e.next)
                .map(|(i, _)| i);
            let Some(i) = pick else { break };
            let addr = self.poll_sched[i].addr;
            let seq = self.next_seq();
            if !self.send_unicast(addr, Unicast::Poll { seq }, TxCtx::Poll { addr }) {
                break;
            }
            let e = &mut self.poll_sched[i];
            e.next = now
                + if e.online {
                    self.shared.cfg.poll_interval
                } else {
                    self.shared.cfg.offline_poll_interval
                };
            self.poll_addrs.insert(addr);
            self.poll_tokens -= 1.0;
            self.counters.polls += 1;
        }
    }

    fn send_exchange_step(&mut self, id: u64) {
        let Some(a) = self.active.get(&id) else {
            return;
        };
        let (addr, base_msg) = (a.req.addr, a.req.msg);
        match a.stage {
            Stage::SendCmd => {
                let seq = self.next_seq();
                let msg = with_seq(base_msg, seq);
                if self.send_unicast(addr, msg, TxCtx::UniCmd { id }) {
                    let a = self.active.get_mut(&id).unwrap();
                    a.cmd_seq = seq;
                    a.stage = Stage::CmdWait;
                }
            }
            Stage::SendPoll => {
                let seq = self.next_seq();
                if self.send_unicast(addr, Unicast::Poll { seq }, TxCtx::UniPoll { id }) {
                    self.active.get_mut(&id).unwrap().stage = Stage::PollWait;
                }
            }
            _ => {}
        }
    }

    fn finish_uni(&mut self, id: u64, r: UniResult) {
        if let Some(a) = self.active.remove(&id) {
            let _ = a.req.reply.send(r);
        }
    }

    fn finish_bcast_if_done(&mut self) {
        while let Some(job) = self.bjobs.front() {
            if job.packets.is_empty() && job.in_flight == 0 {
                let job = self.bjobs.pop_front().unwrap();
                let _ = job.reply.send(Ok(job.sent - job.failed));
            } else {
                break;
            }
        }
    }

    // -- events -------------------------------------------------------------

    fn on_event(&mut self, ev: Event) {
        let now = Instant::now();
        match ev {
            Event::TxDone {
                tag,
                status,
                retries,
                ack_payload,
                ..
            } => self.on_tx_done(tag, status, retries, ack_payload, now),
            Event::Pong { seq, time_us } => {
                self.clock.pong(seq, time_us, now);
                self.shared.clock.store(Arc::new(self.clock.current));
            }
            Event::Stats(s) => self.shared.lock().bridge.stats = s,
            Event::Info(i) => {
                self.depth = (i.tx_queue_depth as usize).max(1);
                self.shared.lock().bridge.info = Some(i);
            }
            Event::CmdResult { kind, status } => {
                if let Some(pos) = self.cmd_waiters.iter().position(|w| w.0 == kind) {
                    let (_, _, reply) = self.cmd_waiters.remove(pos).unwrap();
                    let _ = reply.send(if status == CmdStatus::Ok {
                        Ok(())
                    } else {
                        Err(LinkError::Rejected(format!("bridge: {status:?}")))
                    });
                } else if status != CmdStatus::Ok {
                    self.shared.lock().log(
                        LogLevel::Warn,
                        None,
                        format!("bridge command {kind:#04x} failed: {status:?}"),
                    );
                }
            }
            Event::Rx { .. } | Event::ScanResult { .. } => {}
        }
    }

    fn on_tx_done(&mut self, tag: u16, status: TxStatus, retries: u8, ack: Payload, now: Instant) {
        let Some(f) = self.in_flight.remove(&tag) else {
            return;
        };
        self.slots.release(f.slot);
        if f.slot != 0 {
            self.uni_in_flight = self.uni_in_flight.saturating_sub(1);
        }
        self.tx_total += 1;
        match status {
            TxStatus::Acked => self.tx_acked += 1,
            TxStatus::MaxRetries | TxStatus::Failed => self.tx_failed += 1,
            TxStatus::Sent => {}
        }
        let acked = status == TxStatus::Acked;
        let reply = if acked {
            Reply::decode(ack.as_slice()).ok()
        } else {
            None
        };
        if acked {
            // Remember how many show packets preceded this packet on air, for
            // the broadcast-reception estimate of the status answering it.
            let q = self.seq_frames.entry(f.addr).or_default();
            q.push_back((f.seq, self.show_done));
            if q.len() > 8 {
                q.pop_front();
            }
            let status_reply = match reply {
                Some(Reply::Status(s)) => Some(s),
                _ => None,
            };
            let at_echo = status_reply.and_then(|s| {
                self.seq_frames
                    .get(&f.addr)
                    .and_then(|q| q.iter().rev().find(|e| e.0 == s.echo).map(|e| e.1))
            });
            let mut st = self.shared.lock();
            if let TxCtx::Poll { .. } = f.ctx {
                st.record_poll(f.addr, true, retries, now);
            } else {
                st.touch(f.addr, now);
            }
            if let Some(s) = status_reply {
                st.apply_status(f.addr, s, at_echo, now);
            }
        }
        match f.ctx {
            TxCtx::Show => {
                if status == TxStatus::Sent {
                    self.show_done += 1;
                }
            }
            TxCtx::Bcast { job } => {
                if let Some(j) = self.bjobs.iter_mut().find(|j| j.id == job) {
                    j.in_flight = j.in_flight.saturating_sub(1);
                    if status == TxStatus::Failed {
                        j.failed += 1;
                    }
                }
                self.finish_bcast_if_done();
            }
            TxCtx::Poll { addr, .. } => {
                self.poll_addrs.remove(&addr);
                if acked {
                    self.counters.polls_ok += 1;
                } else {
                    self.shared.lock().record_poll(addr, false, retries, now);
                }
            }
            TxCtx::UniCmd { id } | TxCtx::UniPoll { id } => {
                let is_poll = matches!(f.ctx, TxCtx::UniPoll { .. });
                let Some(a) = self.active.get_mut(&id) else {
                    return;
                };
                a.any_ack |= acked;
                let done = match (a.req.mode, is_poll) {
                    (UniMode::PollOnly, _) => acked.then_some(reply),
                    (UniMode::Collect { .. }, false) => {
                        // A gift that handles the command within the nRF's
                        // ~130 µs ack turnaround answers in this very ack.
                        if let Some(r) = reply.filter(|r| reply_echo(r) == a.cmd_seq) {
                            Some(Some(r))
                        } else {
                            a.cmd_acked = acked;
                            a.stage = Stage::SendPoll;
                            return;
                        }
                    }
                    (UniMode::Collect { accept_cmd_ack }, true) => {
                        match reply.filter(|r| reply_echo(r) == a.cmd_seq) {
                            Some(r) => Some(Some(r)),
                            None => (accept_cmd_ack && a.cmd_acked).then_some(None),
                        }
                    }
                };
                if let Some(reply) = done {
                    self.finish_uni(id, Ok(reply));
                    return;
                }
                a.tries += 1;
                if a.tries < a.req.attempts {
                    a.stage = Stage::SendCmd;
                } else {
                    // A PollOnly exchange that saw an ack has already succeeded.
                    let e = if a.any_ack {
                        LinkError::NoReply
                    } else {
                        LinkError::NoAck
                    };
                    self.finish_uni(id, Err(e));
                }
            }
        }
    }

    // -- housekeeping -------------------------------------------------------

    fn housekeeping(&mut self, now: Instant) {
        self.next_house = now + HOUSEKEEPING;
        // Reclaim credits of TxDone events that never came (bridge dropped events).
        let lost: Vec<u16> = self
            .in_flight
            .iter()
            .filter(|(_, f)| now.duration_since(f.sent) > TXDONE_TIMEOUT)
            .map(|(t, _)| *t)
            .collect();
        for tag in lost {
            self.tx_lost += 1;
            self.on_tx_done(tag, TxStatus::Failed, 0, Payload::default(), now);
        }
        while let Some(w) = self.cmd_waiters.front() {
            if now.duration_since(w.1) > SETUP_TIMEOUT {
                let (_, _, reply) = self.cmd_waiters.pop_front().unwrap();
                let _ = reply.send(Err(LinkError::Timeout("bridge command".into())));
            } else {
                break;
            }
        }
        // Exchanges stuck for too long (should not happen).
        let stuck: Vec<u64> = self
            .active
            .iter()
            .filter(|(_, a)| now.duration_since(a.created) > REQ_TIMEOUT)
            .map(|(id, _)| *id)
            .collect();
        for id in stuck {
            self.finish_uni(id, Err(LinkError::Timeout("exchange".into())));
        }

        let sample = now >= self.next_history;
        if sample {
            self.next_history = now + self.shared.cfg.history_interval;
        }
        let rates = now >= self.next_rates;
        let polling_paused = self.shared.polling_paused.load(Ordering::Relaxed);
        let show_paused = !self.show_active();
        let shared = self.shared.clone();
        let mut st = shared.lock();
        st.housekeeping(&shared.cfg, shared.epoch, now, polling_paused, sample);

        // Refresh the poll schedule from the registry.
        let targets = st.poll_targets();
        self.poll_sched
            .retain(|e| targets.iter().any(|t| t.0 == e.addr));
        for (addr, online) in targets {
            match self.poll_sched.iter_mut().find(|e| e.addr == addr) {
                Some(e) => e.online = online,
                None => self.poll_sched.push(PollEntry {
                    addr,
                    online,
                    next: now,
                }),
            }
        }

        let l = &mut st.link;
        l.uptime = now.duration_since(shared.epoch);
        l.tx_total = self.tx_total;
        l.tx_acked = self.tx_acked;
        l.tx_failed = self.tx_failed;
        l.tx_lost = self.tx_lost;
        l.show_packets = self.counters.show_packets;
        l.show_dropped = self.show_dropped;
        l.in_flight = self.in_flight.len();
        l.max_in_flight = self.max_in_flight;
        if rates {
            let (t0, c0) = self.rates_ref;
            let dt = now.duration_since(t0).as_secs_f32().max(1e-3);
            let c = self.counters;
            l.frames_per_s = (c.ticks - c0.ticks) as f32 / dt;
            l.show_packets_per_s = (c.show_packets - c0.show_packets) as f32 / dt;
            l.polls_per_s = (c.polls - c0.polls) as f32 / dt;
            l.poll_ok_per_s = (c.polls_ok - c0.polls_ok) as f32 / dt;
            self.rates_ref = (now, c);
            self.next_rates = now + Duration::from_secs(1);
        }
        let clock = self.clock.current;
        st.bridge.clock_synced = clock.is_synced() && self.transport.is_some();
        st.bridge.ping_rtt_us = clock.is_synced().then_some(clock.rtt_us());
        let snap = st.build_snapshot(show_paused, now);
        drop(st);
        shared.snapshot.store(Arc::new(snap));
    }
}

fn reject(r: RadioReq, e: LinkError) {
    match r {
        RadioReq::Unicast(u) => {
            let _ = u.reply.send(Err(e));
        }
        RadioReq::Broadcast(b) => {
            let _ = b.reply.send(Err(e));
        }
        RadioReq::Reconfigure { reply, .. } => {
            let _ = reply.send(Err(e));
        }
    }
}
