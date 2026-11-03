//! A simulated bridge + gifts implementing [`Transport`], so the link, the
//! GUI and the CLI run without hardware.
//!
//! The model is meant to be faithful where it matters for the host logic,
//! and timing-faithful enough that a simulated firmware update takes about
//! as long as it would on hardware (the simulator runs in real time against
//! the real host code, so `OtaReport::duration` *is* the estimate):
//!
//! * **Bridge:** processes commands strictly in order on one radio; each `Tx`
//!   produces exactly one `TxDone`, and `STAMP` writes the bridge clock into
//!   the payload. NO_ACK streaming as in the bridge firmware: packets queued
//!   behind a NO_ACK packet to the same slot go out back to back (no PLL
//!   settle), at most `3500 µs / airtime(32 B)` per run; otherwise every
//!   packet pays SPI load + 130 µs settle. Auto-ack Tx: settle, packet, RX/TX
//!   turnaround, ack (+ payload), IRQ service; ARD between attempts.
//!   Airtime = (preamble + 5-byte address + payload + 2-byte CRC) bytes + 9
//!   PCF bits, at 1 or 2 Mbps. One-way USB latency 0.5 ms; 24-deep queue.
//!   The bridge's µs clock starts close to the u32 wrap so wrap handling
//!   gets exercised.
//! * **Radio:** every attempt can be lost independently in each direction
//!   (per-gift loss %, optionally bursty). Two gifts acking the same address
//!   collide; one may win by capture.
//! * **Gifts:** see `sim/gift.rs`. PRX with a 3-deep RX FIFO (full FIFO =
//!   no ack), duplicate suppression by packet ID, and the one-reply-preloaded
//!   ack payload semantics. The **bootloader runs the firmware's own logic**
//!   (`lumigift_logic`: boot decision, recovery mode, OTA receiver, channel
//!   plan) with flash timings (22 ms page erase, 85 µs per double word), its
//!   RX rings filled by the RAM drain during flash work, the 150 ms
//!   `BootHold` window on the default channel, IWDG, `BootShared`. App
//!   faults (crash loop, hang, deaf radio) and power cuts at chosen OTA
//!   phases can be injected.
//! * **LEDs:** each gift renders Music frames with [`lumigift_fx::Renderer`]
//!   (Solid/Leds/Blackout/Blink directly), honouring max brightness and the
//!   power limit. [`SimHandle::gifts`] exposes the current colours.

mod gift;

use std::collections::VecDeque;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use lumigift_proto::{
    self as lp, ImageHeader, LEDS_PER_GIFT, OtaState, Rgb, UNASSIGNED_SLOT, layout,
};
use nrf24_usb::Transport;
use nrf24_usb_proto::{
    CmdStatus, Command, DataRate, Event, Info, PROTOCOL_VERSION, Payload, RadioConfig, Stats,
    TxStatus, tx_flags,
};

use crate::types::Uid;
use gift::SimGift;
pub use gift::{CutPoint, PowerCut, SimAppFault};

// ---------------------------------------------------------------------------
// Configuration
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct SimGiftConfig {
    pub uid: [u8; 12],
    /// `None` = derived from the UID like real gifts (`default_node_addr`).
    pub node_addr: Option<u8>,
    pub slot: u8,
    /// Version of the app image initially in flash.
    pub fw_version: u32,
    /// Start in the bootloader (e.g. recovery mode).
    pub start_in_bootloader: bool,
    /// `false` = no valid app in flash: stays in the bootloader (recovery).
    pub valid_image: bool,
    /// Overrides [`SimConfig::loss_pct`].
    pub loss_pct: Option<f32>,
    pub vbat_mv: u16,
    pub powered: bool,
    /// Radio channel stored in the gift's config page (`None` = default).
    pub channel: Option<u8>,
    /// What is wrong with the installed app, if anything.
    pub app_fault: SimAppFault,
}

impl SimGiftConfig {
    pub fn new(uid: [u8; 12]) -> Self {
        Self {
            uid,
            node_addr: None,
            slot: UNASSIGNED_SLOT,
            fw_version: 100,
            start_in_bootloader: false,
            valid_image: true,
            loss_pct: None,
            vbat_mv: 1450,
            powered: true,
            channel: None,
            app_fault: SimAppFault::None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct SimConfig {
    pub gifts: Vec<SimGiftConfig>,
    pub seed: u64,
    /// Default per-attempt packet loss in each direction, percent.
    pub loss_pct: f32,
    /// 0 = independent losses. Otherwise the probability (0..1) that a
    /// packet following a lost one is lost too (Gilbert-style bursts; the
    /// average loss stays `loss_pct`).
    pub loss_burstiness: f32,
    /// Random reboots per gift per hour.
    pub reboot_rate_per_hour: f32,
    pub vbat_drain_mv_per_hour: f32,
    pub tx_queue_depth: u8,
    /// One-way USB latency.
    pub usb_latency: Duration,
    /// Chance (percent) that one of several colliding acks is still received.
    pub capture_pct: f32,
    pub boot_version: u16,
    /// Flash programming time of one full 24-byte OTA chunk (3 double
    /// words; ~140 µs each on a real STM32C011 including the bootloader's
    /// bookkeeping, measured).
    pub chunk_write_time: Duration,
    /// Flash page erase time (22 ms typical, 40 ms max per the datasheet).
    pub page_erase_time: Duration,
    /// Chance (percent) that an app handles a unicast before the nRF sends
    /// its ack, so the reply rides on that same ack instead of the next
    /// poll's. Real gifts do this often (it depends on main-loop latency).
    pub fast_reply_pct: f32,
}

impl SimConfig {
    /// `n` gifts with random UIDs (deterministic for a given seed), 2% loss,
    /// no random reboots.
    pub fn new(n: usize) -> Self {
        Self::with_seed(n, 0x5EED_1234)
    }

    pub fn with_seed(n: usize, seed: u64) -> Self {
        let mut rng = fastrand::Rng::with_seed(seed);
        let gifts = (0..n)
            .map(|_| {
                let mut uid = [0u8; 12];
                // Plausible STM32 UID: wafer x/y, lot number, ...
                for b in &mut uid {
                    *b = rng.u8(..);
                }
                let mut g = SimGiftConfig::new(uid);
                g.vbat_mv = rng.u16(1300..1550);
                g
            })
            .collect();
        Self {
            gifts,
            seed,
            loss_pct: 2.0,
            loss_burstiness: 0.0,
            reboot_rate_per_hour: 0.0,
            vbat_drain_mv_per_hour: 15.0,
            // The real bridge reports 24.
            tx_queue_depth: 24,
            usb_latency: Duration::from_micros(500),
            capture_pct: 60.0,
            boot_version: lumigift_logic::recovery::BOOT_VERSION,
            chunk_write_time: Duration::from_micros(420),
            page_erase_time: Duration::from_millis(22),
            fast_reply_pct: 50.0,
        }
    }

    /// Livelier settings for demos: 3% loss, a few random reboots per hour,
    /// faster battery drain.
    pub fn demo(n: usize) -> Self {
        let mut c = Self::new(n);
        c.loss_pct = 3.0;
        c.reboot_rate_per_hour = 4.0;
        c.vbat_drain_mv_per_hour = 60.0;
        c
    }
}

/// Build a valid firmware image (header padded to 256 B + app bytes) with
/// a plausible vector table. `app_len` is clamped to the app region.
pub fn build_test_image(fw_version: u32, app_len: usize, seed: u32) -> Vec<u8> {
    let app_len = app_len.clamp(8, layout::APP_MAX_SIZE as usize);
    let mut x = seed | 1;
    let mut app: Vec<u8> = (0..app_len)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 17;
            x ^= x << 5;
            x as u8
        })
        .collect();
    // Initial SP at the top of RAM (below BootShared), Thumb reset vector.
    app[0..4].copy_from_slice(&layout::BOOT_SHARED_ADDR.to_le_bytes());
    app[4..8].copy_from_slice(&(layout::APP_VECTORS + 0xC1).to_le_bytes());
    let h = ImageHeader::new(&app, 1, fw_version, *b"simulatd");
    let mut img = vec![0xFF; layout::IMAGE_HEADER_SIZE as usize];
    img[..ImageHeader::ENCODED_LEN].copy_from_slice(&h.encode());
    img.extend_from_slice(&app);
    img
}

// ---------------------------------------------------------------------------
// Public views
// ---------------------------------------------------------------------------

/// A simulated gift as the hardware would look right now.
#[derive(Debug, Clone)]
pub struct SimGiftView {
    pub index: usize,
    pub uid: Uid,
    pub powered: bool,
    pub node_addr: u8,
    pub slot: Option<u8>,
    /// Channel the radio is listening on right now.
    pub channel: u8,
    pub in_bootloader: bool,
    /// Version of the image in flash (`None` if invalid).
    pub image_fw_version: Option<u32>,
    pub vbat_mv: u16,
    pub resets: u16,
    /// Current LED colours (after brightness cap and power limit, before gamma).
    pub leds: [Rgb; LEDS_PER_GIFT],
    pub ota_state: Option<OtaState>,
    /// Channel stored in the config page.
    pub config_channel: u8,
    /// `BootShared::boot_attempts`.
    pub boot_attempts: u16,
    pub app_fault: SimAppFault,
    pub reset_reason: lp::ResetReason,
}

#[derive(Debug, Clone, Default)]
pub struct SimStats {
    pub tx: u64,
    pub tx_unicast: u64,
    pub tx_acked: u64,
    /// Most `Tx` the host ever had outstanding (sent, TxDone not yet delivered).
    pub max_outstanding_tx: usize,
    /// Broadcast packets that carried OTA chunks which a gift programmed.
    pub chunks_written: u64,
}

// ---------------------------------------------------------------------------
// World
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Pipe {
    Broadcast,
    Unicast,
}

const MS: u64 = 1000;

struct WorldParams {
    rng: fastrand::Rng,
    loss_pct: f32,
    burstiness: f32,
    reboot_rate: f32,
    drain: f32,
    boot_version: u16,
    /// Flash timings of the gifts.
    erase_us: u64,
    dw_us: u64,
    bridge0: u32,
    chunks_written: u64,
    fast_reply_pct: f32,
}

impl WorldParams {
    fn bridge_raw(&self, t: u64) -> u32 {
        (self.bridge0 as u64 + t) as u32
    }
    fn bridge_ms(&self, t: u64) -> u32 {
        ((self.bridge0 as u64 + t) / 1000) as u32
    }
}

struct World {
    t0: Instant,
    p: WorldParams,
    gifts: Vec<SimGift>,
    capture_pct: f32,
    usb_latency_us: u64,
    depth: u8,
    radio: RadioConfig,
    tx_addr: [[u8; 5]; 8],
    queue: VecDeque<(u64, Command)>,
    radio_free: u64,
    events: VecDeque<(u64, Event)>,
    next_pid: u64,
    stats: Stats,
    sim_stats: SimStats,
    outstanding_tx: usize,
    image: Vec<u8>,
    /// Current NO_ACK run: (slot, packets so far).
    burst: Option<(u8, u32)>,
}

/// A gift in its initial state (`t` = simulation time).
fn make_gift(c: &SimGiftConfig, image: &[u8], seed: u32, t: u64) -> SimGift {
    let mut flash = vec![0xFF; layout::APP_REGION_SIZE as usize];
    if c.valid_image {
        let img = if c.fw_version == 100 {
            image.to_vec()
        } else {
            build_test_image(c.fw_version, 12 * 1024, 7)
        };
        flash[..img.len()].copy_from_slice(&img);
    }
    let node = c.node_addr.unwrap_or_else(|| lp::default_node_addr(&c.uid));
    let mut g = SimGift::new(c.uid, node, c.slot, flash, c.vbat_mv, seed);
    g.loss_pct = c.loss_pct;
    g.stored_channel = c
        .channel
        .filter(|&ch| ch <= 125)
        .unwrap_or(lp::DEFAULT_CHANNEL);
    g.app_fault = c.app_fault;
    if c.powered {
        // Pretend the gift has been running for a while already.
        g.start_settled(t, c.start_in_bootloader);
    } else {
        g.power_off(t);
    }
    g
}

/// On-air time of one packet: preamble (1 B), 5-byte address, 9-bit packet
/// control field, payload, 2-byte CRC.
fn airtime_us(payload_len: usize, rate: DataRate) -> u64 {
    let bits = 8 * (1 + 5 + payload_len as u64 + 2) + 9;
    let bit_us = match rate {
        DataRate::Kbps250 => 4.0,
        DataRate::Mbps1 => 1.0,
        DataRate::Mbps2 => 0.5,
    };
    (bits as f64 * bit_us).ceil() as u64
}

/// nRF24 PLL settling before a transmission from standby (Tstby2a).
const SETTLE_US: u64 = 130;
/// Bridge MCU time to load a packet over SPI before CE goes high.
const BRIDGE_LOAD_US: u64 = 20;
/// PRX RX→TX turnaround before the ack goes out.
const TURNAROUND_US: u64 = 130;
/// Bridge IRQ service + reading the ack payload after an acked packet.
const ACK_SERVICE_US: u64 = 30;

/// Packets per continuous NO_ACK run before the bridge lets the TX FIFO
/// drain (the nRF must not stay in TX mode for more than 4 ms; the bridge
/// caps runs at `3500 µs / airtime(32 B)`, see its README).
fn burst_cap(rate: DataRate) -> u32 {
    (3500 / airtime_us(32, rate)).max(1) as u32
}

impl World {
    fn now(&self) -> u64 {
        self.t0.elapsed().as_micros() as u64
    }

    /// Is this packet (one direction, one attempt) lost on the way to/from `gift`?
    fn lost(&mut self, gift: usize) -> bool {
        let p = self.gifts[gift].loss_pct.unwrap_or(self.p.loss_pct) / 100.0;
        if p <= 0.0 {
            return false;
        }
        let b = self.p.burstiness.clamp(0.0, 0.95);
        // Two-state (Gilbert) model with stationary loss probability p.
        let chance = if b > 0.0 && self.gifts[gift].prev_lost {
            b
        } else if b > 0.0 {
            (p * (1.0 - b) / (1.0 - p).max(1e-3)).min(1.0)
        } else {
            p
        };
        let l = self.p.rng.f32() < chance;
        self.gifts[gift].prev_lost = l;
        l
    }

    /// Gifts that hear `addr` at time `t` (each first advanced to `t`).
    fn listeners(&mut self, addr: &[u8; 5], t: u64) -> Vec<(usize, Pipe)> {
        self.advance_all(t);
        let ch = self.radio.channel;
        self.gifts
            .iter()
            .enumerate()
            .filter_map(|(i, g)| g.listens(t, addr, ch).map(|p| (i, p)))
            .collect()
    }

    fn emit(&mut self, at: u64, ev: Event) {
        self.events.push_back((at + self.usb_latency_us, ev));
    }

    fn pump(&mut self, now: u64) {
        while let Some(&(arrive, _)) = self.queue.front() {
            let start = arrive.max(self.radio_free);
            if start > now {
                break;
            }
            let (_, cmd) = self.queue.pop_front().unwrap();
            // The bridge keeps its TX FIFO topped up while the next queued
            // message is another NO_ACK Tx to the same slot (see `execute_tx`).
            let queued = arrive <= self.radio_free;
            if !matches!(cmd, Command::Tx { .. }) {
                self.burst = None;
            }
            self.execute(cmd, start, queued);
        }
    }

    fn execute(&mut self, cmd: Command, start: u64, queued: bool) {
        let result = |ok: bool| Event::CmdResult {
            kind: cmd.kind(),
            status: if ok {
                CmdStatus::Ok
            } else {
                CmdStatus::BadArgs
            },
        };
        match cmd {
            Command::GetInfo => self.emit(
                start,
                Event::Info(Info {
                    protocol_version: PROTOCOL_VERSION,
                    fw_version: 1,
                    tx_queue_depth: self.depth,
                    radio_present: true,
                    uid: *b"SIMBRIDGE001",
                }),
            ),
            Command::Ping { seq } => {
                let time_us = self.p.bridge_raw(start);
                self.emit(start, Event::Pong { seq, time_us })
            }
            Command::Configure(c) => {
                let valid = c.channel <= 125
                    && (1..=2).contains(&c.crc_len)
                    && (3..=5).contains(&c.addr_width);
                if valid {
                    self.radio = c;
                }
                self.emit(start, result(valid));
            }
            Command::SetTxAddr { slot, addr } => {
                let valid = (slot as usize) < self.tx_addr.len();
                if valid {
                    self.tx_addr[slot as usize] = addr;
                }
                self.emit(start, result(valid));
            }
            Command::SetRxPipe { .. }
            | Command::SetAckPayload { .. }
            | Command::SetMode(_)
            | Command::Reset => self.emit(start, result(true)),
            Command::GetStats => self.emit(start, Event::Stats(self.stats)),
            Command::Scan {
                first,
                last,
                repeats,
                ..
            } => {
                for ch in first..=last.min(125) {
                    let hits = if (ch as i32 - self.radio.channel as i32).abs() <= 1 {
                        0
                    } else {
                        self.p.rng.u16(0..3)
                    };
                    self.emit(
                        start,
                        Event::ScanResult {
                            channel: ch,
                            hits,
                            samples: repeats as u16,
                        },
                    );
                }
            }
            Command::Tx {
                tag,
                slot,
                flags,
                ts_offset,
                payload,
            } => self.execute_tx(start, queued, tag, slot, flags, ts_offset, payload),
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn execute_tx(
        &mut self,
        start: u64,
        queued: bool,
        tag: u16,
        slot: u8,
        flags: u8,
        ts_offset: u8,
        mut payload: Payload,
    ) {
        self.sim_stats.tx += 1;
        let done = |w: &mut World, end: u64, status, retries, ack: Option<Payload>| {
            w.radio_free = end;
            match status {
                TxStatus::Sent => w.stats.tx_sent += 1,
                TxStatus::Acked => w.stats.tx_acked += 1,
                _ => w.stats.tx_failed += 1,
            }
            let time_us = w.p.bridge_raw(end);
            w.emit(
                end,
                Event::TxDone {
                    tag,
                    status,
                    retries,
                    time_us,
                    ack_payload: ack.unwrap_or_default(),
                },
            );
        };
        if slot as usize >= self.tx_addr.len() || payload.is_empty() {
            self.burst = None;
            done(self, start + 10, TxStatus::Failed, 0, None);
            return;
        }
        let rate = self.radio.data_rate;
        let no_ack = flags & tx_flags::NO_ACK != 0;
        let stamped = flags & tx_flags::STAMP != 0;
        // NO_ACK streaming: a packet that was already queued behind a NO_ACK
        // packet to the same slot goes out back to back (no PLL settle), up
        // to the run cap. A stamped packet only enters an empty FIFO.
        let continues = no_ack
            && !stamped
            && queued
            && self
                .burst
                .is_some_and(|(s, n)| s == slot && n < burst_cap(rate));
        let air_start = if continues {
            start
        } else {
            start + BRIDGE_LOAD_US + SETTLE_US
        };
        self.burst = if no_ack {
            let n = if continues {
                self.burst.map_or(0, |b| b.1)
            } else {
                0
            };
            Some((slot, n + 1))
        } else {
            None
        };
        if stamped {
            let o = ts_offset as usize;
            if o + 4 <= payload.len() {
                let ts = self.p.bridge_raw(air_start);
                payload.as_mut_slice()[o..o + 4].copy_from_slice(&ts.to_le_bytes());
            }
        }
        let addr = self.tx_addr[slot as usize];
        let air = airtime_us(payload.len(), rate);
        let pid = self.next_pid;
        self.next_pid += 1;
        if no_ack {
            let t_rx = air_start + air;
            for (i, pipe) in self.listeners(&addr, t_rx) {
                if !self.lost(i) {
                    self.gifts[i].receive(t_rx, pipe, payload, pid, &mut self.p);
                }
            }
            done(self, t_rx, TxStatus::Sent, 0, None);
            return;
        }
        self.sim_stats.tx_unicast += 1;
        let ard = (self.radio.retry_delay as u64 + 1) * 250;
        let mut cursor = start + BRIDGE_LOAD_US;
        for attempt in 0..=self.radio.retries {
            let t_rx = cursor + SETTLE_US + air;
            let mut acks: Vec<Option<Payload>> = Vec::new();
            for (i, pipe) in self.listeners(&addr, t_rx) {
                if self.lost(i) {
                    continue;
                }
                if let Some(ack) = self.gifts[i].receive(t_rx, pipe, payload, pid, &mut self.p)
                    && !self.lost(i)
                {
                    acks.push(ack);
                }
            }
            let winner = match acks.len() {
                0 => None,
                1 => Some(acks[0]),
                n => (self.p.rng.f32() * 100.0 < self.capture_pct)
                    .then(|| acks[self.p.rng.usize(..n)]),
            };
            if let Some(ack) = winner {
                let ack_len = ack.map(|a| a.len()).unwrap_or(0);
                let end = t_rx + TURNAROUND_US + airtime_us(ack_len, rate) + ACK_SERVICE_US;
                self.sim_stats.tx_acked += 1;
                done(self, end, TxStatus::Acked, attempt, ack);
                return;
            }
            cursor = t_rx + ard;
        }
        done(
            self,
            cursor + ACK_SERVICE_US,
            TxStatus::MaxRetries,
            self.radio.retries,
            None,
        );
    }

    fn advance_all(&mut self, now: u64) {
        for g in &mut self.gifts {
            g.advance(now, &mut self.p);
        }
    }
}

// ---------------------------------------------------------------------------
// Transport + handle
// ---------------------------------------------------------------------------

/// The simulated bridge. Create with [`SimTransport::new`]; pass it to
/// [`Link::start`](crate::Link::start) or use [`Link::simulated`](crate::Link::simulated).
pub struct SimTransport {
    world: Arc<Mutex<World>>,
}

/// Inspect and manipulate the simulated gifts (thread-safe, cloneable).
#[derive(Clone)]
pub struct SimHandle {
    world: Arc<Mutex<World>>,
}

fn lock(w: &Mutex<World>) -> MutexGuard<'_, World> {
    w.lock().unwrap_or_else(|p| p.into_inner())
}

impl SimTransport {
    pub fn new(cfg: SimConfig) -> (Self, SimHandle) {
        let image = build_test_image(100, 16 * 1024, 0xC0FFEE);
        let mut rng = fastrand::Rng::with_seed(cfg.seed ^ 0xA5A5);
        let gifts: Vec<SimGift> = cfg
            .gifts
            .iter()
            .map(|g| make_gift(g, &image, rng.u32(..), 0))
            .collect();
        let world = World {
            t0: Instant::now(),
            p: WorldParams {
                rng,
                loss_pct: cfg.loss_pct,
                burstiness: cfg.loss_burstiness,
                reboot_rate: cfg.reboot_rate_per_hour,
                drain: cfg.vbat_drain_mv_per_hour,
                boot_version: cfg.boot_version,
                erase_us: cfg.page_erase_time.as_micros() as u64,
                dw_us: (cfg.chunk_write_time.as_micros() as u64 / 3).max(1),
                // Wraps ~3 s after start.
                bridge0: u32::MAX - 3_000_000,
                chunks_written: 0,
                fast_reply_pct: cfg.fast_reply_pct,
            },
            gifts,
            capture_pct: cfg.capture_pct,
            usb_latency_us: cfg.usb_latency.as_micros() as u64,
            depth: cfg.tx_queue_depth,
            radio: RadioConfig {
                channel: 2,
                ..RadioConfig::default()
            },
            tx_addr: [[0; 5]; 8],
            queue: VecDeque::new(),
            radio_free: 0,
            events: VecDeque::new(),
            next_pid: 1,
            stats: Stats::default(),
            sim_stats: SimStats::default(),
            outstanding_tx: 0,
            image,
            burst: None,
        };
        let world = Arc::new(Mutex::new(world));
        (
            Self {
                world: world.clone(),
            },
            SimHandle { world },
        )
    }
}

impl Transport for SimTransport {
    fn send(&mut self, cmds: &[Command]) -> nrf24_usb::Result<()> {
        let mut w = lock(&self.world);
        let now = w.now();
        w.pump(now);
        let arrive = now + w.usb_latency_us;
        for c in cmds {
            if matches!(c, Command::Tx { .. }) {
                w.outstanding_tx += 1;
                w.sim_stats.max_outstanding_tx =
                    w.sim_stats.max_outstanding_tx.max(w.outstanding_tx);
            }
            w.queue.push_back((arrive, *c));
        }
        Ok(())
    }

    fn recv(&mut self, timeout: Duration) -> nrf24_usb::Result<Option<Event>> {
        let deadline = Instant::now() + timeout;
        loop {
            let wake = {
                let mut w = lock(&self.world);
                let now = w.now();
                w.pump(now);
                if let Some(&(due, ev)) = w.events.front()
                    && due <= now
                {
                    w.events.pop_front();
                    if matches!(ev, Event::TxDone { .. }) {
                        w.outstanding_tx = w.outstanding_tx.saturating_sub(1);
                    }
                    return Ok(Some(ev));
                }
                let mut wake = w.events.front().map(|e| e.0).unwrap_or(u64::MAX);
                if let Some(&(arrive, _)) = w.queue.front() {
                    wake = wake.min(arrive.max(w.radio_free));
                }
                let t = w.t0;
                t + Duration::from_micros(wake.min(now + 1_000_000))
            };
            let now = Instant::now();
            if now >= deadline {
                return Ok(None);
            }
            let until = wake.min(deadline);
            if until > now {
                std::thread::sleep(until - now);
            }
        }
    }

    fn describe(&self) -> String {
        format!("simulator ({} gifts)", lock(&self.world).gifts.len())
    }
}

impl SimHandle {
    /// Another transport to the same simulated bridge, as if it had been
    /// unplugged and plugged in again (for reconnect tests).
    pub fn transport(&self) -> SimTransport {
        let mut w = lock(&self.world);
        // Whatever the old connection had queued is lost with it.
        w.queue.clear();
        w.events.clear();
        w.outstanding_tx = 0;
        SimTransport {
            world: self.world.clone(),
        }
    }

    /// Current state of every simulated gift, including LED colours.
    pub fn gifts(&self) -> Vec<SimGiftView> {
        let mut w = lock(&self.world);
        let now = w.now();
        w.advance_all(now);
        let World { gifts, p, .. } = &mut *w;
        gifts
            .iter_mut()
            .enumerate()
            .map(|(i, g)| SimGiftView {
                index: i,
                uid: Uid(g.uid),
                powered: g.powered,
                node_addr: g.node_addr,
                slot: (g.slot != UNASSIGNED_SLOT).then_some(g.slot),
                channel: g.radio_channel,
                in_bootloader: g.in_boot(),
                image_fw_version: g.image_fw(),
                vbat_mv: g.vbat(now, p.drain),
                resets: g.shared.resets,
                leds: g.leds(now, p),
                ota_state: g.ota_state(),
                config_channel: g.stored_channel,
                boot_attempts: g.boot_attempts(),
                app_fault: g.app_fault,
                reset_reason: g.reset_reason(),
            })
            .collect()
    }

    pub fn len(&self) -> usize {
        lock(&self.world).gifts.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn index_of(&self, uid: &Uid) -> Option<usize> {
        lock(&self.world).gifts.iter().position(|g| g.uid == uid.0)
    }

    /// Set packet loss for one gift (`Some(i)`) or the default for all (`None`).
    pub fn set_loss_pct(&self, gift: Option<usize>, pct: f32) {
        let mut w = lock(&self.world);
        match gift {
            Some(i) => w.gifts[i].loss_pct = Some(pct),
            None => {
                w.p.loss_pct = pct;
                for g in &mut w.gifts {
                    g.loss_pct = None;
                }
            }
        }
    }

    pub fn set_reboot_rate(&self, per_hour: f32) {
        lock(&self.world).p.reboot_rate = per_hour;
    }

    /// Run `f` on one gift at the current simulation time (every gift
    /// advanced to it first).
    fn at_now(&self, gift: usize, f: impl FnOnce(&mut SimGift, u64)) {
        let mut w = lock(&self.world);
        let now = w.now();
        w.advance_all(now);
        f(&mut w.gifts[gift], now);
    }

    /// Switch a gift off (it vanishes from the air) or on (power-on reset).
    pub fn set_powered(&self, gift: usize, on: bool) {
        self.at_now(gift, |g, now| {
            if on && !g.powered {
                g.power_on(now);
            } else if !on && g.powered {
                g.power_off(now);
            }
        });
    }

    /// Unplug a gift now and plug it back in after `off_for`.
    pub fn power_cycle(&self, gift: usize, off_for: Duration) {
        self.at_now(gift, |g, now| g.power_cycle(now, off_for));
    }

    /// Cut a gift's power at a chosen point of the next firmware update
    /// (the interrupted flash operation leaves garbage behind), and restore
    /// it after `cut.off_for`.
    pub fn schedule_power_cut(&self, gift: usize, cut: PowerCut) {
        lock(&self.world).gifts[gift].power_cut = Some(cut);
    }

    /// Change what is wrong with a gift's installed app (takes effect at its
    /// next start; installing a new image clears it).
    pub fn set_app_fault(&self, gift: usize, fault: SimAppFault) {
        lock(&self.world).gifts[gift].app_fault = fault;
    }

    /// Overwrite the radio channel in a gift's config page (as a buggy app
    /// could). Takes effect at its next reset.
    pub fn set_config_channel(&self, gift: usize, channel: u8) {
        lock(&self.world).gifts[gift].stored_channel = channel;
    }

    /// The next double word programmed at `offset` (relative to the app
    /// region) gets a bit flipped, as if the flash write went wrong.
    pub fn glitch_flash_write(&self, gift: usize, offset: u32) {
        lock(&self.world).gifts[gift].flash_glitch = Some(offset & !7);
    }

    /// Reset a gift as if its watchdog fired.
    pub fn reboot(&self, gift: usize) {
        self.at_now(gift, |g, now| g.watchdog_reset(now));
    }

    /// Change a gift's node address behind the host's back (e.g. to create a collision).
    pub fn set_node_addr(&self, gift: usize, addr: u8) {
        lock(&self.world).gifts[gift].node_addr = addr;
    }

    /// Add a gift (powered on now). Returns its index.
    pub fn add_gift(&self, cfg: SimGiftConfig) -> usize {
        let mut w = lock(&self.world);
        let now = w.now();
        let seed = w.p.rng.u32(..);
        let image = w.image.clone();
        let mut g = make_gift(&cfg, &image, seed, now);
        if cfg.powered {
            g.power_on(now);
        }
        w.gifts.push(g);
        w.gifts.len() - 1
    }

    pub fn stats(&self) -> SimStats {
        let w = lock(&self.world);
        SimStats {
            chunks_written: w.p.chunks_written,
            ..w.sim_stats.clone()
        }
    }

    /// The bridge's current radio channel.
    pub fn bridge_channel(&self) -> u8 {
        lock(&self.world).radio.channel
    }
}
