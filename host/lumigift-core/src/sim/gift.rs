//! One simulated gift: MCU timeline, nRF24 PRX, app and bootloader.
//!
//! The bootloader side runs the firmware's own hardware-independent code
//! (`lumigift_logic`: boot decision, `BootShared` latching, `Recovery`,
//! `Ota`, `ChannelPlan`, `RxRing`), so its behaviour is the real one. What
//! the simulator adds is the hardware timing the firmware lives with:
//!
//! * flash: page erase 22 ms (typ.; configurable), 85 µs per programmed
//!   double word, CRC unit ~0.15 µs per byte;
//! * the bootloader's main loop is a sequence of actions with those
//!   durations. During flash operations its RAM code keeps draining the nRF
//!   into the RX rings (as the firmware does); during pure CPU work (CRC)
//!   packets queue in the 3-deep nRF RX FIFO, and a full FIFO means no ack;
//! * radio start-up (2 ms warm, 100 ms after power-on), the 150 ms
//!   `BootHold` window on the default channel, app start-up (radio up
//!   ~3 ms after the jump), the IWDG (2 s) and app faults (crash loop, hang,
//!   deaf radio).

use std::cell::Cell;
use std::collections::VecDeque;
use std::time::Duration;

use lumigift_fx::Renderer;
use lumigift_logic::boot::{self as lboot, Decision, csr2};
use lumigift_logic::channel::{self, ChannelPlan};
use lumigift_logic::ota::{RegionFlash, RegionRead, validate_image};
use lumigift_logic::recovery::{
    Action, BCAST_SLOTS, Ctx, HOLD_WINDOW_MS, Recovery, Tick, UNI_SLOTS,
};
use lumigift_logic::rxring::RxRing;
use lumigift_proto::layout::{self, BootShared};
use lumigift_proto::{
    self as lp, Broadcast, Identity, ImageHeader, LEDS_PER_GIFT, OTA_BITMAP_BYTES, OtaBitmap,
    OtaState, Packet, Reply, ResetReason, Rgb, Status, UNASSIGNED_SLOT, Unicast, config_key,
    image::crc32, status_flags,
};
use nrf24_usb_proto::Payload;

use super::{MS, Pipe, WorldParams};

/// nRF24L01+ RX FIFO depth.
const FIFO_DEPTH: usize = 3;
/// The nRF answers SPI only up to 100 ms after power-on (the bootloader's
/// `radio::start` retries for 150 ms).
const RADIO_POR_US: u64 = 100 * MS;
/// `radio::start` on a warm chip: init + Tpd2stby (1.5 ms) + RX settling.
const RADIO_START_US: u64 = 2 * MS;
/// Bootloader init before the image check: clocks, IWDG, latch, config scan.
const BOOT_INIT_US: u64 = 500;
/// App init before its `radio::start` (board init, config, header read).
const APP_INIT_US: u64 = MS;
/// HW CRC unit fed byte by byte: ~7 cycles per byte at 48 MHz.
const CRC_NS_PER_BYTE: u64 = 150;
/// SPI read of one packet out of the nRF by the main loop.
const DRAIN_US: u64 = 50;
/// Bootloader CPU time per handled packet (decode, logic, flash register
/// setup, ack preload), on top of the flash operations themselves.
const UNICAST_US: u64 = 60;
const BCAST_US: u64 = 30;
const IWDG_US: u64 = 2_000 * MS;
const HEALTHY_US: u64 = 5_000 * MS;
const APP_REBOOT_DELAY_US: u64 = 20 * MS;

/// A dozing app's receiver schedule: on for `LISTEN_US` every `period`
/// (its LSI-timed period), the first window starting at `start`.
#[derive(Debug, Clone, Copy)]
struct Doze {
    start: u64,
    period: u64,
}

impl Doze {
    fn listening(&self, t: u64) -> bool {
        t >= self.start && (t - self.start) % self.period < lp::doze::LISTEN_US as u64
    }
}

/// Something wrong with the app image a gift runs (it is fixed by
/// installing another image).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SimAppFault {
    #[default]
    None,
    /// Crashes (HardFault → reset) this long after starting.
    CrashAfter(Duration),
    /// Hangs right after starting: never feeds the IWDG (reset after ~2 s).
    Hang,
    /// Runs fine (feeds the IWDG, marks itself healthy) but its radio
    /// never receives anything. Only a power cycle with `BootHold` helps.
    DeafRadio,
}

/// Where a scheduled power cut hits (see [`SimHandle::schedule_power_cut`](super::SimHandle::schedule_power_cut)).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CutPoint {
    /// In the middle of the `n`-th page erase (0-based) of an OTA session.
    MidErase(u32),
    /// Right after the `n`-th chunk was programmed.
    AfterChunks(u16),
    /// Right after the image was verified (before the app starts).
    AfterVerified,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PowerCut {
    pub at: CutPoint,
    /// Power returns after this long (power-on reset).
    pub off_for: Duration,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Cause {
    PowerOn,
    Software,
    Watchdog,
}

#[derive(Debug, Clone, Copy)]
pub(super) enum LedMode {
    Off,
    Music,
    Static([Rgb; LEDS_PER_GIFT]),
}

enum LedUpdate {
    Music(lp::MusicFrame),
    Static([Rgb; LEDS_PER_GIFT]),
    Off,
}

/// The bootloader in recovery mode.
pub(super) struct BootSim {
    pub rec: Recovery,
    uni: RxRing<UNI_SLOTS>,
    bc: RxRing<BCAST_SLOTS>,
    /// End of the running action.
    op_end: Option<u64>,
    /// The running action keeps the radio drained (flash work from RAM).
    op_drains: bool,
    /// Effects that become visible at the end of the action.
    deferred_reply: Option<Reply>,
    deferred_addr: Option<u8>,
    /// Bootloader time cursor (idle: last event handled).
    t: u64,
    erases: u32,
    /// SPI time owed for packets the main loop (not the RAM drain during a
    /// flash operation) read out of the nRF.
    drain_owed: u64,
}

impl BootSim {
    /// A packet read out of the nRF goes into its RX ring (a full ring
    /// drops it after the nRF acked it).
    fn push(&mut self, pipe: Pipe, p: Payload) {
        match pipe {
            Pipe::Unicast => self.uni.push(p.as_slice()),
            Pipe::Broadcast => self.bc.push(p.as_slice()),
        };
    }
}

pub(super) enum Mode {
    /// Powered off.
    Off,
    /// Bootloader start-up until `at` (init, image check, radio start).
    Starting {
        at: u64,
    },
    /// Listening for `BootHold` on the default channel until `until`.
    Hold {
        until: u64,
    },
    Boot(Box<BootSim>),
    App {
        healthy_at: Option<u64>,
        fault_at: Option<(u64, Cause)>,
    },
}

pub(super) struct SimGift {
    pub uid: [u8; 12],
    pub powered: bool,
    pub loss_pct: Option<f32>,
    /// The last packet to/from this gift was lost (for bursty loss).
    pub prev_lost: bool,
    // config page
    pub node_addr: u8,
    pub slot: u8,
    pub stored_channel: u8,
    pub max_brightness: u8,
    pub idle_timeout_ms: u32,
    pub power_budget_ma: u16,
    // app region
    pub flash: Vec<u8>,
    // RAM that survives warm resets
    pub shared: BootShared,
    // runtime
    pub mode: Mode,
    pub radio_channel: u8,
    radio_on_at: Option<u64>,
    pub boot_time: u64,
    pub t_last: u64,
    preloaded: Option<Payload>,
    /// App: last command reply and how many more polls still return it.
    pending: Option<(Reply, u8)>,
    last_ack: Option<Payload>,
    last_pid: Option<u64>,
    fifo: VecDeque<(u64, Pipe, Payload)>,
    busy_until: u64,
    echo: u8,
    bcast_rx: u16,
    bcast_lost: u16,
    last_bcast_seq: Option<u8>,
    last_show: Option<u64>,
    vbat0: f32,
    temp_c: f32,
    renderer: Renderer,
    pub led_mode: LedMode,
    blink: Option<(Rgb, u8, u64, u64)>,
    pending_reset: Option<(u64, bool)>,
    pending_channel: Option<(u64, u8)>,
    pub app_fault: SimAppFault,
    pub power_cut: Option<PowerCut>,
    off_until: Option<u64>,
    /// Next programmed double word at this offset gets a flipped bit.
    pub flash_glitch: Option<u32>,
    /// The app dozes (see `lumigift_proto::doze`).
    doze: Option<Doze>,
    /// This gift's doze period: `PERIOD_MS` on its ±10 % LSI.
    doze_period_us: u64,
    /// Last decodable packet from a host (the app's auto-doze timer).
    last_host: u64,
}

/// The app region seen by the bootloader logic, with time accounting.
struct SimRegion<'a> {
    mem: &'a mut [u8],
    erase_us: u64,
    dw_us: u64,
    spent: u64,
    crc_bytes: Cell<u64>,
    programs: u32,
    glitch: &'a mut Option<u32>,
    erases: &'a mut u32,
    /// Fires inside the `n`-th erase.
    cut_erase: Option<u32>,
    cut_fired: bool,
}

/// The app region as plain memory (no timing).
struct Mem<'a>(&'a [u8]);

impl RegionRead for Mem<'_> {
    fn read(&self, offset: u32, out: &mut [u8]) {
        let o = offset as usize;
        out.copy_from_slice(&self.0[o..o + out.len()]);
    }
    fn crc32(&self, offset: u32, len: u32) -> u32 {
        let o = offset as usize;
        crc32(&self.0[o..(o + len as usize).min(self.0.len())])
    }
}

impl RegionRead for SimRegion<'_> {
    fn read(&self, offset: u32, out: &mut [u8]) {
        Mem(self.mem).read(offset, out)
    }
    fn crc32(&self, offset: u32, len: u32) -> u32 {
        self.crc_bytes.set(self.crc_bytes.get() + len as u64);
        Mem(self.mem).crc32(offset, len)
    }
}

impl RegionFlash for SimRegion<'_> {
    fn erase_page(&mut self, page: u32) -> bool {
        assert!(page < layout::APP_PAGES);
        let s = (page * layout::PAGE_SIZE) as usize;
        let page_mem = &mut self.mem[s..s + layout::PAGE_SIZE as usize];
        if self.cut_erase == Some(*self.erases) && !self.cut_fired {
            // Power fails half way through: some bytes erased, some not.
            for (i, b) in page_mem.iter_mut().enumerate() {
                if i % 3 != 0 {
                    *b = 0xFF;
                }
            }
            self.cut_fired = true;
            self.spent += self.erase_us / 2;
            return false;
        }
        if self.cut_fired {
            return false;
        }
        page_mem.fill(0xFF);
        *self.erases += 1;
        self.spent += self.erase_us;
        true
    }
    fn program(&mut self, offset: u32, mut dw: [u8; 8]) -> bool {
        if self.cut_fired {
            return false;
        }
        let o = offset as usize;
        if *self.glitch == Some(offset) {
            *self.glitch = None;
            dw[0] ^= 0x01;
        }
        // NOR flash: programming can only clear bits.
        for (m, d) in self.mem[o..o + 8].iter_mut().zip(dw) {
            *m &= d;
        }
        self.programs += 1;
        self.spent += self.dw_us;
        true
    }
}

fn crc_us(bytes: u64) -> u64 {
    bytes * CRC_NS_PER_BYTE / 1000
}

/// Header of a valid image in `flash` (the bootloader's check: header,
/// board revision, vector table, app CRC).
fn verify_flash(flash: &[u8]) -> Option<ImageHeader> {
    validate_image(&Mem(flash), 1)
}

impl SimGift {
    pub fn new(
        uid: [u8; 12],
        node_addr: u8,
        slot: u8,
        flash: Vec<u8>,
        vbat_mv: u16,
        seed: u32,
    ) -> Self {
        Self {
            uid,
            powered: true,
            loss_pct: None,
            prev_lost: false,
            node_addr,
            slot,
            stored_channel: lp::DEFAULT_CHANNEL,
            max_brightness: 160,
            idle_timeout_ms: 1000,
            power_budget_ma: 250,
            flash,
            shared: BootShared::new(),
            mode: Mode::Off,
            radio_channel: lp::DEFAULT_CHANNEL,
            radio_on_at: None,
            boot_time: 0,
            t_last: 0,
            preloaded: None,
            pending: None,
            last_ack: None,
            last_pid: None,
            fifo: VecDeque::new(),
            busy_until: 0,
            echo: 0,
            bcast_rx: 0,
            bcast_lost: 0,
            last_bcast_seq: None,
            last_show: None,
            vbat0: vbat_mv as f32,
            temp_c: 21.0,
            renderer: Renderer::new(seed),
            led_mode: LedMode::Off,
            blink: None,
            pending_reset: None,
            pending_channel: None,
            app_fault: SimAppFault::None,
            power_cut: None,
            off_until: None,
            flash_glitch: None,
            doze: None,
            doze_period_us: {
                let lsi = 0.9 + 0.2 * fastrand::Rng::with_seed(seed as u64 ^ 0xD0_2E).f64();
                (lp::doze::PERIOD_MS as f64 * MS as f64 * lsi) as u64
            },
            last_host: 0,
        }
    }

    pub fn dozing(&self) -> bool {
        self.doze.is_some()
    }

    /// Doze from `t` on: LEDs off, receiver only in the listen windows.
    fn enter_doze(&mut self, t: u64, w: &mut WorldParams) {
        self.fifo.clear();
        self.preloaded = None;
        self.pending = None;
        self.blink = None;
        self.led_mode = LedMode::Off;
        let period = self.doze_period_us;
        self.doze = Some(Doze {
            start: t + w.rng.u64(0..period),
            period,
        });
    }

    /// Start as if the gift had been running for a while: in the app (if
    /// its image is valid) or in its bootloader.
    pub fn start_settled(&mut self, t: u64, in_bootloader: bool) {
        self.shared = BootShared::new();
        self.boot_time = t;
        self.t_last = t;
        if in_bootloader || self.image_header().is_none() {
            self.shared.request = BootShared::REQ_STAY_IN_BOOT;
            self.enter_recovery(t, self.stored_channel);
        } else {
            self.jump_to_app(t, true);
        }
    }

    pub fn image_header(&self) -> Option<ImageHeader> {
        verify_flash(&self.flash)
    }

    pub fn image_fw(&self) -> Option<u32> {
        self.image_header().map(|h| h.fw_version)
    }

    pub fn in_boot(&self) -> bool {
        matches!(
            self.mode,
            Mode::Boot(_) | Mode::Hold { .. } | Mode::Starting { .. }
        )
    }

    pub fn ota_state(&self) -> Option<OtaState> {
        match &self.mode {
            Mode::Boot(b) => Some(b.rec.ota.state()),
            _ => None,
        }
    }

    /// Which pipe of this gift `addr` hits right now, if it is listening.
    pub fn listens(&self, t: u64, addr: &[u8; 5], channel: u8) -> Option<Pipe> {
        if !self.powered
            || addr[1..] != lp::ADDR_PREFIX
            || self.radio_channel != channel
            || self.radio_on_at.is_none_or(|a| a > t)
            || self.doze.is_some_and(|d| !d.listening(t))
        {
            return None;
        }
        if addr[0] == lp::BROADCAST_LSB {
            Some(Pipe::Broadcast)
        } else if addr[0] == self.node_addr {
            Some(Pipe::Unicast)
        } else {
            None
        }
    }

    fn ms(&self, t: u64) -> u32 {
        (t.saturating_sub(self.boot_time) / MS) as u32
    }

    // -- power and resets ------------------------------------------------------

    pub fn power_off(&mut self, t: u64) {
        self.powered = false;
        self.mode = Mode::Off;
        self.radio_on_at = None;
        self.fifo.clear();
        self.t_last = self.t_last.max(t);
    }

    pub fn power_on(&mut self, t: u64) {
        self.powered = true;
        self.off_until = None;
        self.reset(t, Cause::PowerOn);
    }

    fn reset(&mut self, t: u64, cause: Cause) {
        let flags = match cause {
            Cause::PowerOn => csr2::PWRRSTF | csr2::PINRSTF,
            Cause::Software => csr2::SFTRSTF | csr2::PINRSTF,
            Cause::Watchdog => csr2::IWDGRSTF | csr2::PINRSTF,
        };
        let prev = if cause == Cause::PowerOn {
            // RAM content after power-up is garbage.
            BootShared {
                magic: 0,
                ..self.shared
            }
        } else {
            self.shared
        };
        self.shared = lboot::latch(prev, flags);
        self.fifo.clear();
        self.preloaded = None;
        self.pending = None;
        self.last_ack = None;
        self.last_pid = None;
        self.radio_on_at = None;
        self.boot_time = t;
        self.busy_until = t;
        self.echo = 0;
        self.bcast_rx = 0;
        self.bcast_lost = 0;
        self.last_bcast_seq = None;
        self.last_show = None;
        self.led_mode = LedMode::Off;
        self.blink = None;
        self.pending_reset = None;
        self.pending_channel = None;
        let crc = self.image_header().map_or(0, |h| h.app_len as u64);
        let radio = if cause == Cause::PowerOn {
            RADIO_POR_US
        } else {
            RADIO_START_US
        };
        self.mode = Mode::Starting {
            at: t + BOOT_INIT_US + crc_us(crc) + radio,
        };
    }

    fn on_started(&mut self, t: u64) {
        let valid = self.image_header().is_some();
        match lboot::decide(valid, &self.shared) {
            Decision::ListenThenBoot => {
                self.radio_channel = channel::hold_window_channel();
                self.radio_on_at = Some(t);
                self.mode = Mode::Hold {
                    until: t + HOLD_WINDOW_MS as u64 * MS,
                };
            }
            Decision::Recovery => self.enter_recovery(t, self.stored_channel),
        }
    }

    fn enter_recovery(&mut self, t: u64, start_channel: u8) {
        let plan = ChannelPlan::new(self.stored_channel, start_channel, self.ms(t));
        self.radio_channel = plan.current();
        self.radio_on_at = Some(t);
        let rec = Recovery::new(1, self.image_header(), plan);
        let st = rec.status(&self.ctx(), self.ms(t));
        self.preload(Reply::Status(st));
        self.mode = Mode::Boot(Box::new(BootSim {
            rec,
            uni: RxRing::new(),
            bc: RxRing::new(),
            op_end: None,
            op_drains: true,
            deferred_reply: None,
            deferred_addr: None,
            t,
            erases: 0,
            drain_owed: 0,
        }));
    }

    /// The bootloader jumps to the app (`settled`: pretend it has been
    /// running for a while, for the initial state).
    fn jump_to_app(&mut self, t: u64, settled: bool) {
        self.radio_on_at = None;
        self.fifo.clear();
        self.preloaded = None;
        self.pending = None;
        self.boot_time = t;
        self.busy_until = t;
        let (radio, fault_at) = match self.app_fault {
            SimAppFault::None => (true, None),
            SimAppFault::CrashAfter(d) => (true, Some((t + d.as_micros() as u64, Cause::Software))),
            SimAppFault::Hang => (false, Some((t + IWDG_US, Cause::Watchdog))),
            SimAppFault::DeafRadio => (false, None),
        };
        if radio {
            self.radio_channel = self.stored_channel;
            self.radio_on_at = Some(if settled {
                t
            } else {
                t + APP_INIT_US + RADIO_START_US
            });
            let st = self.app_status(t);
            self.preload(Reply::Status(st));
        }
        self.doze = None;
        self.last_host = t;
        let healthy_at = (self.app_fault != SimAppFault::Hang).then_some(t + HEALTHY_US);
        self.mode = Mode::App {
            healthy_at: if settled { None } else { healthy_at },
            fault_at,
        };
    }

    fn ctx(&self) -> Ctx {
        Ctx {
            uid: self.uid,
            hw_rev: 1,
            node_addr: self.node_addr,
            slot: self.slot,
            reset_reason: self.shared.reset_reason,
            resets: self.shared.resets,
            protected: true,
        }
    }

    fn preload(&mut self, r: Reply) {
        let mut buf = [0u8; 32];
        let n = r.encode(&mut buf);
        self.preloaded = Some(Payload::new(&buf[..n]).unwrap());
    }

    // -- reception ---------------------------------------------------------------

    /// A packet reaches this gift's antenna at `t` (the caller checked
    /// [`listens`](Self::listens)). `None` = not accepted (nRF RX FIFO
    /// full: no ack), else the ack payload (unicast) / `Some(None)`.
    pub fn receive(
        &mut self,
        t: u64,
        pipe: Pipe,
        payload: Payload,
        pid: u64,
        w: &mut WorldParams,
    ) -> Option<Option<Payload>> {
        self.advance(t, w);
        if self.radio_on_at.is_none_or(|a| a > t) {
            return None;
        }
        if pipe == Pipe::Unicast && self.last_pid == Some(pid) {
            // Retransmission after a lost ack: the nRF re-acks, no new packet.
            return Some(self.last_ack);
        }
        if self.doze.is_some() && matches!(self.mode, Mode::App { .. }) {
            // Inside a listen window (`listens` checked). `Wake` or a
            // unicast to it wakes it; anything else is dropped unhandled.
            let wake = pipe == Pipe::Unicast
                || matches!(
                    Packet::decode(payload.as_slice()),
                    Ok(Packet::Broadcast(Broadcast::Wake))
                );
            if !wake {
                return Some(None);
            }
            self.doze = None;
            self.last_host = t;
            if pipe == Pipe::Broadcast {
                return Some(None);
            }
            // The waking unicast is then handled like any other (its ack
            // carries no payload: the TX FIFO was flushed on dozing).
        }
        let accepted = match &mut self.mode {
            Mode::Hold { .. } => {
                if pipe == Pipe::Broadcast
                    && lumigift_logic::bootpkt::decode(payload.as_slice())
                        == lumigift_logic::bootpkt::BootPacket::BootHold
                {
                    // Rescued: stay in recovery on the default channel.
                    self.enter_recovery(t, channel::hold_window_channel());
                }
                true
            }
            Mode::Boot(b) => {
                let busy_cpu = b.op_end.is_some_and(|e| e > t) && !b.op_drains;
                if busy_cpu {
                    fifo_push(&mut self.fifo, (t, pipe, payload))
                } else {
                    // Drained right away (main loop or RAM drain loop).
                    // Only the main loop's reads cost the CPU extra time.
                    if b.op_end.is_none_or(|e| e <= t) {
                        b.drain_owed += DRAIN_US;
                    }
                    b.push(pipe, payload);
                    true
                }
            }
            Mode::App { .. } => {
                let ok = fifo_push(&mut self.fifo, (t, pipe, payload));
                // Real gifts often handle a unicast within the nRF's ~130 µs
                // ack turnaround, so the reply rides on this packet's own ack.
                if ok
                    && pipe == Pipe::Unicast
                    && self.fifo.len() == 1
                    && self.busy_until <= t
                    && w.rng.f32() * 100.0 < w.fast_reply_pct
                {
                    let (_, pipe, p) = self.fifo.pop_front().unwrap();
                    self.busy_until = t + app_cost(&p);
                    self.app_handle(t, pipe, p, w);
                }
                ok
            }
            Mode::Off | Mode::Starting { .. } => false,
        };
        if !accepted {
            return None;
        }
        let ack = if pipe == Pipe::Unicast {
            self.last_pid = Some(pid);
            let a = self.preloaded.take();
            self.last_ack = a;
            a
        } else {
            None
        };
        self.advance(t, w);
        Some(ack)
    }

    // -- time --------------------------------------------------------------------

    /// Run the gift up to time `t`.
    pub fn advance(&mut self, t: u64, w: &mut WorldParams) {
        if t > self.t_last && self.powered && w.reboot_rate > 0.0 {
            let dt_h = (t - self.t_last) as f32 / 3.6e9;
            if w.rng.f32() < w.reboot_rate * dt_h {
                let cause = if w.rng.bool() {
                    Cause::Watchdog
                } else {
                    Cause::PowerOn
                };
                self.reset(t, cause);
            }
        }
        if t > self.t_last {
            self.temp_c = (self.temp_c + (w.rng.f32() - 0.5) * 0.02).clamp(15.0, 30.0);
        }
        // Bounded: every iteration consumes an event.
        for _ in 0..100_000 {
            if !self.step(t, w) {
                break;
            }
        }
        self.t_last = self.t_last.max(t);
    }

    /// Handle the earliest pending event at or before `t`. False if none.
    fn step(&mut self, t: u64, w: &mut WorldParams) -> bool {
        if let Some(on) = self.off_until
            && on <= t
            && !self.powered
        {
            self.power_on(on);
            return true;
        }
        match &mut self.mode {
            Mode::Off => false,
            Mode::Starting { at } => {
                let at = *at;
                if at > t {
                    return false;
                }
                self.on_started(at);
                true
            }
            Mode::Hold { until } => {
                let until = *until;
                if until > t {
                    return false;
                }
                self.shared.boot_attempts = self.shared.boot_attempts.saturating_add(1);
                self.jump_to_app(until, false);
                true
            }
            Mode::Boot(_) => self.step_boot(t, w),
            Mode::App { .. } => self.step_app(t, w),
        }
    }

    // -- bootloader ----------------------------------------------------------------

    fn step_boot(&mut self, t: u64, w: &mut WorldParams) -> bool {
        let Mode::Boot(b) = &mut self.mode else {
            return false;
        };
        // 1. The running action ends.
        if let Some(end) = b.op_end {
            if end > t {
                return false;
            }
            b.op_end = None;
            b.t = end;
            // Packets that waited in the nRF FIFO during CPU work.
            while let Some((_, pipe, p)) = self.fifo.pop_front() {
                b.drain_owed += DRAIN_US;
                b.push(pipe, p);
            }
            let (reply, addr) = (b.deferred_reply.take(), b.deferred_addr.take());
            if let Some(r) = reply {
                self.preload(r);
            }
            if let Some(a) = addr {
                self.node_addr = a;
            }
            return true;
        }
        // 2. Idle: start the next action at the bootloader's own time.
        let now = b.t;
        let action = b.rec.next_action(&b.uni, &b.bc);
        if action != Action::Idle {
            self.start_action(action, now, w);
            return true;
        }
        // 3. Timers (boot the verified app, reboot, channel switch).
        let Mode::Boot(b) = &mut self.mode else {
            return false;
        };
        let ms_now = ((now.saturating_sub(self.boot_time)) / MS) as u32;
        let Some(d) = b.rec.next_deadline(ms_now) else {
            b.t = t;
            return false;
        };
        let due = (self.boot_time + (ms_now as u64 + d as u64) * MS).max(now);
        if due > t {
            b.t = t;
            return false;
        }
        b.t = due;
        let tick = b.rec.tick(((due - self.boot_time) / MS) as u32);
        match tick {
            Tick::None => b.t = due + MS,
            Tick::Channel(c) => self.radio_channel = c,
            Tick::Boot => {
                if let Some(ch) = b.rec.channel_to_store(self.stored_channel) {
                    self.stored_channel = ch;
                }
                if b.rec.ota.verified_header().is_some() {
                    // A new image is installed: whatever was wrong with the old app is gone.
                    self.app_fault = SimAppFault::None;
                }
                self.shared.request = 0;
                self.shared.boot_attempts = 1;
                self.jump_to_app(due, false);
            }
            Tick::Reboot { into_bootloader } => {
                if into_bootloader {
                    self.shared.request = BootShared::REQ_STAY_IN_BOOT;
                } else {
                    self.shared.request = 0;
                    self.shared.boot_attempts = 0;
                }
                self.reset(due, Cause::Software);
            }
        }
        true
    }

    fn start_action(&mut self, action: Action, t: u64, w: &mut WorldParams) {
        let ctx = self.ctx();
        let ms = self.ms(t);
        let cut_erase = match self.power_cut {
            Some(PowerCut {
                at: CutPoint::MidErase(n),
                ..
            }) => Some(n),
            _ => None,
        };
        let Mode::Boot(b) = &mut self.mode else {
            return;
        };
        let mut region = SimRegion {
            mem: &mut self.flash,
            erase_us: w.erase_us,
            dw_us: w.dw_us,
            spent: 0,
            crc_bytes: Cell::new(0),
            programs: 0,
            glitch: &mut self.flash_glitch,
            erases: &mut b.erases,
            cut_erase,
            cut_fired: false,
        };
        b.rec.ring_drops = b.bc.dropped.wrapping_add(b.uni.dropped);
        let mut buf = [0u8; 32];
        let owed = std::mem::take(&mut b.drain_owed);
        let cpu = owed
            + match action {
                Action::Unicast => {
                    let n = b.uni.pop_into(&mut buf).unwrap_or(0);
                    let r = b.rec.on_unicast(&buf[..n], &b.bc, ms, &region, &ctx);
                    b.deferred_reply = Some(r);
                    UNICAST_US
                }
                Action::Broadcast => {
                    let n = b.bc.pop_into(&mut buf).unwrap_or(0);
                    b.deferred_addr = b.rec.on_broadcast(&buf[..n], ms, &mut region, &ctx);
                    BCAST_US
                }
                Action::Erase => {
                    b.rec.erase_ahead(&mut region);
                    0
                }
                Action::Idle => 0,
            };
        let crc = crc_us(region.crc_bytes.get());
        let end = t + cpu + region.spent + crc;
        b.op_end = Some(end);
        b.op_drains = crc == 0;
        let fired_erase = region.cut_fired;
        if region.programs > 0 {
            w.chunks_written += 1;
        }
        let received = b.rec.ota.map().received();
        let state = b.rec.ota.state();
        // Scheduled power cuts.
        if let Some(cut) = self.power_cut {
            let fire = match cut.at {
                CutPoint::MidErase(_) => fired_erase,
                CutPoint::AfterChunks(n) => state == OtaState::Receiving && received >= n,
                CutPoint::AfterVerified => state == OtaState::Verified,
            };
            if fire {
                self.power_cut = None;
                self.power_cycle(end, cut.off_for);
            }
        }
    }

    // -- app -----------------------------------------------------------------------

    fn step_app(&mut self, t: u64, w: &mut WorldParams) -> bool {
        // Auto-doze: no host packet for `doze_auto_after`.
        let doze_at = self.doze.is_none().then(|| self.last_host + w.doze_auto_us);
        let Mode::App {
            healthy_at,
            fault_at,
        } = &mut self.mode
        else {
            return false;
        };
        enum Due {
            Fault,
            Healthy,
            Reset,
            Channel,
            Packet,
            Doze,
        }
        // The earliest event due by `t` (ties: first listed).
        let fifo_done = self
            .fifo
            .front()
            .map(|(ta, _, p)| (*ta).max(self.busy_until) + app_cost(p));
        let next = [
            (fault_at.map(|f| f.0), Due::Fault),
            (*healthy_at, Due::Healthy),
            (self.pending_reset.map(|r| r.0), Due::Reset),
            (self.pending_channel.map(|c| c.0), Due::Channel),
            (fifo_done, Due::Packet),
            (doze_at, Due::Doze),
        ]
        .into_iter()
        .filter_map(|(at, what)| at.filter(|&at| at <= t).map(|at| (at, what)))
        .reduce(|a, b| if b.0 < a.0 { b } else { a });
        let Some((at, what)) = next else {
            return false;
        };
        match what {
            Due::Fault => {
                let (_, cause) = fault_at.take().unwrap();
                self.reset(at, cause);
            }
            Due::Healthy => {
                *healthy_at = None;
                self.shared.boot_attempts = 0;
            }
            Due::Reset => {
                let (_, stay) = self.pending_reset.take().unwrap();
                self.shared.request = if stay {
                    BootShared::REQ_STAY_IN_BOOT
                } else {
                    0
                };
                self.reset(at, Cause::Software);
            }
            Due::Channel => {
                let (_, ch) = self.pending_channel.take().unwrap();
                self.radio_channel = ch;
                self.stored_channel = ch;
            }
            Due::Packet => {
                let (_, pipe, p) = self.fifo.pop_front().unwrap();
                self.busy_until = at;
                self.app_handle(at, pipe, p, w);
            }
            Due::Doze => self.enter_doze(at, w),
        }
        true
    }

    fn app_handle(&mut self, t: u64, pipe: Pipe, p: Payload, w: &mut WorldParams) {
        let Ok(pkt) = Packet::decode(p.as_slice()) else {
            return;
        };
        // Any decodable packet is host traffic (restarts the auto-doze timer).
        self.last_host = t;
        match (pipe, pkt) {
            (Pipe::Broadcast, Packet::Broadcast(b)) => self.app_broadcast(t, b, w),
            (Pipe::Unicast, Packet::Unicast(u)) => {
                self.echo = u.seq();
                let is_poll = matches!(u, Unicast::Poll { .. });
                // Flush the TX FIFO, then preload exactly one reply. As in the
                // firmware, a command reply survives the next REPLY_POLLS polls.
                self.preloaded = None;
                let r = self.app_unicast(t, u, w);
                let r = match (is_poll, self.pending.as_mut()) {
                    (false, _) if !matches!(r, Reply::Status(_)) => {
                        self.pending = Some((r, lumigift_logic::recovery::REPLY_POLLS));
                        r
                    }
                    (true, Some((rep, left))) if *left > 0 => {
                        *left -= 1;
                        *rep
                    }
                    _ => {
                        self.pending = None;
                        r
                    }
                };
                self.preload(r);
            }
            _ => {}
        }
    }

    fn app_broadcast(&mut self, t: u64, b: Broadcast, w: &mut WorldParams) {
        match b {
            Broadcast::SetNodeAddr { uid, node_addr } => {
                if uid == self.uid && lp::valid_node_addr(node_addr) {
                    self.node_addr = node_addr;
                }
                return;
            }
            Broadcast::Sleep => {
                self.enter_doze(t, w);
                return;
            }
            Broadcast::EnterBootloader { count, addrs } => {
                if addrs[..(count as usize).min(addrs.len())].contains(&self.node_addr) {
                    self.pending_reset = Some((t + 100, true));
                }
                return;
            }
            _ => {}
        }
        let (seq, data) = match b {
            Broadcast::Sync { seq, .. } => (seq, None),
            Broadcast::Music { seq, frame, .. } => (seq, Some(LedUpdate::Music(frame))),
            Broadcast::Solid {
                seq,
                first_slot,
                count,
                colors,
                ..
            } => {
                let s = self.slot;
                let hit =
                    s != UNASSIGNED_SLOT && s >= first_slot && s < first_slot.saturating_add(count);
                (
                    seq,
                    hit.then(|| LedUpdate::Static([colors[(s - first_slot) as usize]; 4])),
                )
            }
            Broadcast::Leds {
                seq, slots, colors, ..
            } => {
                let s = self.slot;
                let i = (s != UNASSIGNED_SLOT)
                    .then(|| slots.iter().position(|&x| x == s))
                    .flatten();
                (seq, i.map(|i| LedUpdate::Static(colors[i])))
            }
            Broadcast::Blackout { seq, .. } => (seq, Some(LedUpdate::Off)),
            _ => return,
        };
        self.bcast_rx = self.bcast_rx.wrapping_add(1);
        if let Some(last) = self.last_bcast_seq {
            let gap = seq.wrapping_sub(last.wrapping_add(1));
            if gap < 128 {
                self.bcast_lost = self.bcast_lost.wrapping_add(gap as u16);
            }
        }
        self.last_bcast_seq = Some(seq);
        self.last_show = Some(t);
        match data {
            Some(LedUpdate::Music(f)) => {
                // Like the firmware: frames for other groups only keep the look alive.
                if self.renderer.on_frame_for(&f, w.bridge_ms(t), self.slot) {
                    self.led_mode = LedMode::Music;
                }
            }
            Some(LedUpdate::Static(c)) => self.led_mode = LedMode::Static(c),
            Some(LedUpdate::Off) => self.led_mode = LedMode::Off,
            None => {}
        }
    }

    fn app_unicast(&mut self, t: u64, u: Unicast, w: &mut WorldParams) -> Reply {
        let echo = u.seq();
        match u {
            Unicast::Poll { .. } => {}
            Unicast::GetIdentity { .. } => {
                let h = self.image_header();
                let flags = self.base_flags();
                return Reply::Identity(Identity {
                    echo,
                    uid: self.uid,
                    hw_rev: 1,
                    fw_version: h.map_or(0, |h| h.fw_version),
                    boot_version: w.boot_version,
                    slot: self.slot,
                    node_addr: self.node_addr,
                    flags,
                    app_crc: h.map_or(0, |h| h.app_crc),
                });
            }
            Unicast::Reboot {
                into_bootloader, ..
            } => {
                self.pending_reset = Some((t + APP_REBOOT_DELAY_US, into_bootloader));
            }
            Unicast::Blink {
                color,
                count,
                period_10ms,
                ..
            } => {
                self.blink = Some((color, count, period_10ms.max(1) as u64 * 10 * MS, t));
            }
            Unicast::SetSlot { slot, .. } => {
                if slot < lp::MAX_SLOTS || slot == UNASSIGNED_SLOT {
                    self.slot = slot;
                }
            }
            Unicast::SetConfig { key, value, .. } => {
                let ok = match key {
                    config_key::MAX_BRIGHTNESS if value <= 255 => {
                        self.max_brightness = value as u8;
                        true
                    }
                    config_key::IDLE_TIMEOUT_MS if (100..=25_500).contains(&value) => {
                        self.idle_timeout_ms = value as u32;
                        true
                    }
                    config_key::POWER_BUDGET_MA if (20..=2000).contains(&value) => {
                        self.power_budget_ma = value;
                        true
                    }
                    config_key::CHANNEL if value <= 125 => {
                        self.pending_channel = Some((
                            t + config_key::CHANNEL_SWITCH_DELAY_MS as u64 * MS,
                            value as u8,
                        ));
                        true
                    }
                    _ => false,
                };
                let cur = match key {
                    config_key::MAX_BRIGHTNESS => self.max_brightness as u16,
                    config_key::IDLE_TIMEOUT_MS => self.idle_timeout_ms as u16,
                    config_key::POWER_BUDGET_MA => self.power_budget_ma,
                    config_key::CHANNEL => {
                        self.pending_channel
                            .map(|c| c.1)
                            .unwrap_or(self.stored_channel) as u16
                    }
                    _ => 0,
                };
                return Reply::ConfigResult {
                    echo,
                    key,
                    ok,
                    value: cur,
                };
            }
            Unicast::OtaQuery { session, .. } | Unicast::OtaCommit { session, .. } => {
                return Reply::OtaBitmap(OtaBitmap {
                    echo,
                    session,
                    state: OtaState::NotInBootloader,
                    window: 0,
                    missing_count: 0,
                    bits: [0; OTA_BITMAP_BYTES],
                });
            }
        }
        Reply::Status(self.app_status(t))
    }

    // -- views -----------------------------------------------------------------------

    pub fn vbat(&self, t: u64, drain_per_hour: f32) -> u16 {
        let hours = t as f32 / 3.6e9;
        let sag = if matches!(self.led_mode, LedMode::Off) {
            0.0
        } else {
            25.0
        };
        (self.vbat0 - drain_per_hour * hours - sag).max(800.0) as u16
    }

    /// Flags the app reports in both `Status` and `Identity`.
    fn base_flags(&self) -> u8 {
        let mut flags = status_flags::BOOT_PROTECTED;
        if self.slot != UNASSIGNED_SLOT {
            flags |= status_flags::SLOT_ASSIGNED;
        }
        flags
    }

    pub fn app_status(&self, t: u64) -> Status {
        let synced = self
            .last_show
            .is_some_and(|s| t.saturating_sub(s) < 1000 * MS);
        let vbat = self.vbat(t, 0.0);
        let mut flags = self.base_flags();
        if synced {
            flags |= status_flags::TIME_SYNCED;
        }
        if vbat < 1050 {
            flags |= status_flags::LOW_BATTERY;
        }
        if synced && !matches!(self.led_mode, LedMode::Off) {
            flags |= status_flags::LEDS_ACTIVE;
        }
        if self.blink.is_some_and(|b| t < b.3 + b.1 as u64 * b.2) {
            flags |= status_flags::BLINKING;
        }
        Status {
            echo: self.echo,
            flags,
            slot: self.slot,
            uptime_ms: (t.saturating_sub(self.boot_time) / MS) as u32,
            fw_version: self.image_fw().unwrap_or(0),
            vbat_mv: vbat,
            bcast_rx: self.bcast_rx,
            bcast_lost: self.bcast_lost,
            reset_reason: self.shared.reset_reason,
            resets: self.shared.resets,
            effect: self.renderer.frame().effect,
            sync_err_us: if synced {
                4 + (t % 37) as u16
            } else {
                u16::MAX
            },
            temp_c: self.temp_c.round() as i8,
        }
    }

    pub fn reset_reason(&self) -> ResetReason {
        ResetReason::from_u8(self.shared.reset_reason)
    }

    pub fn leds(&mut self, t: u64, w: &WorldParams) -> [Rgb; LEDS_PER_GIFT] {
        let t = t.max(self.t_last);
        let black = [Rgb::BLACK; LEDS_PER_GIFT];
        if !self.powered || !matches!(self.mode, Mode::App { .. }) || self.doze.is_some() {
            return black;
        }
        if let Some((c, count, period, start)) = self.blink {
            let el = t.saturating_sub(start);
            if el < count as u64 * period {
                return if el % period < period / 2 {
                    [c; 4]
                } else {
                    black
                };
            }
        }
        if self
            .last_show
            .is_none_or(|s| t.saturating_sub(s) > self.idle_timeout_ms as u64 * MS)
        {
            return black;
        }
        let mut out = match self.led_mode {
            LedMode::Off => black,
            LedMode::Static(c) => c,
            LedMode::Music => {
                let mut o = black;
                // The raw slot, like the firmware (unassigned = 0xFF).
                self.renderer.render(w.bridge_ms(t), self.slot, &mut o);
                o
            }
        };
        for c in &mut out {
            *c = lumigift_fx::scale8(*c, self.max_brightness);
        }
        // Like the firmware: the battery caps the LED budget (the firmware
        // also smooths the cap upwards; the model's voltage is smooth anyway).
        let cap = lumigift_logic::adc::battery_led_budget_ma(self.vbat(t, w.drain));
        lumigift_fx::power_limit(&mut out, self.power_budget_ma.min(cap));
        out
    }

    /// Power off now and back on after `off_for` (e.g. the user replugging it).
    pub fn power_cycle(&mut self, t: u64, off_for: Duration) {
        self.power_off(t);
        self.off_until = Some(t + off_for.as_micros() as u64);
    }

    pub fn boot_attempts(&self) -> u16 {
        self.shared.boot_attempts
    }

    pub fn watchdog_reset(&mut self, t: u64) {
        if self.powered {
            self.reset(t, Cause::Watchdog);
        }
    }
}

/// Into the nRF RX FIFO; false (no ack) if it is full.
fn fifo_push(fifo: &mut VecDeque<(u64, Pipe, Payload)>, pkt: (u64, Pipe, Payload)) -> bool {
    if fifo.len() >= FIFO_DEPTH {
        return false;
    }
    fifo.push_back(pkt);
    true
}

fn app_cost(p: &Payload) -> u64 {
    match p.as_slice().first() {
        Some(&k) if k >= lp::kind::POLL => 80,
        _ => 40,
    }
}
