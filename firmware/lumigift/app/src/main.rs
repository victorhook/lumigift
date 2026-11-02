//! Lumigift gift application.
//!
//! Super-loop, no executor: the radio IRQ (PA4, EXTI falling edge) only
//! timestamps packet arrival for the clock sync, TIM14 extends the µs
//! timer, everything else runs in `App::run`:
//!
//! * drain the nRF RX FIFO whenever IRQ is low (and every 10 ms anyway),
//!   handle show broadcasts and unicast commands, preload one ack payload
//!   after each unicast packet (and refresh the preloaded Status);
//! * render the LEDs every 10 ms from the bridge-synced clock;
//! * measure VBAT / temperature, check the radio, feed the IWDG, clear
//!   `BootShared.boot_attempts` after 5 s healthy.
#![no_std]
#![no_main]

mod adc;
mod ws2812;

use core::sync::atomic::{AtomicU32, Ordering};

use cortex_m_rt::entry;
use lumigift_board::config::ConfigStore;
use lumigift_board::pac::{self, EXTI, interrupt};
use lumigift_board::radio::{self, PIPE_UNICAST, Radio};
use lumigift_board::{self as board, gpio, iwdg, sys, time};
use lumigift_fx::{Renderer, gamma, hsv, power_limit, scale8};
use lumigift_logic::adc::low_battery;
use lumigift_logic::blink::Blink;
use lumigift_logic::config_log::apply_key;
use lumigift_logic::seq::SeqTracker;
use lumigift_logic::sync::{ClockSync, air_time_us};
use lumigift_proto::image::ImageHeader;
use lumigift_proto::layout::{APP_REGION_BASE, BootShared};
use lumigift_proto::{
    Broadcast, Identity, LEDS_PER_GIFT, MAX_PAYLOAD, MAX_SLOTS, OTA_BITMAP_BYTES, OtaBitmap, OtaState, Packet, Reply,
    Rgb, Status, UNASSIGNED_SLOT, Unicast, config_key, status_flags, valid_node_addr,
};

use crate::adc::Adc;
use crate::ws2812::Ws2812;

const FRAME_MS: u32 = 10;
const HEALTHY_AFTER_MS: u32 = 5000;
const BOOT_ANIM_MS: u32 = 700;
const ADC_PERIOD_MS: u32 = 1000;
const RADIO_CHECK_MS: u32 = 1000;
const STATUS_REFRESH_MS: u32 = 100;
const RADIO_POLL_MS: u32 = 10;
const REBOOT_DELAY_MS: u32 = 20;
/// Idle fade: 255 -> 0 in ~250 ms at 100 frames/s.
const FADE_STEP: u8 = 10;

// Radio IRQ edge timestamps (written by the EXTI handler only).
static EDGE_COUNT: AtomicU32 = AtomicU32::new(0);
static EDGE_US: AtomicU32 = AtomicU32::new(0);

#[interrupt]
fn TIM14() {
    time::on_irq();
}

#[interrupt]
fn EXTI4_15() {
    if EXTI.fpr(0).read().line(gpio::PIN_IRQ) {
        EXTI.fpr(0).write(|w| w.set_line(gpio::PIN_IRQ, true));
        EDGE_US.store(time::now_us(), Ordering::Relaxed);
        EDGE_COUNT.store(EDGE_COUNT.load(Ordering::Relaxed).wrapping_add(1), Ordering::Relaxed);
    }
}

/// PA4 falling edge -> EXTI line 4.
fn enable_radio_irq() {
    EXTI.exticr(1).modify(|w| w.set_exti(0, 0)); // EXTICR2 covers lines 4..7; 0 = port A
    EXTI.ftsr(0).modify(|w| w.set_line(gpio::PIN_IRQ, true));
    EXTI.fpr(0).write(|w| w.set_line(gpio::PIN_IRQ, true));
    EXTI.imr(0).modify(|w| w.set_line(gpio::PIN_IRQ, true));
    // SAFETY: the handler only touches the atomics above.
    unsafe { cortex_m::peripheral::NVIC::unmask(pac::Interrupt::EXTI4_15) };
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    Off,
    Music,
    Direct,
}

struct App {
    radio: Option<Radio>,
    leds: Ws2812,
    adc: Adc,
    cfg: ConfigStore,
    uid: [u8; 12],
    fw_version: u32,
    app_crc: u32,
    boot_version: u16,
    boot_protected: bool,
    reset_reason: u8,
    resets: u16,

    echo: u8,
    seq: SeqTracker,
    sync: ClockSync,
    last_edge: u32,
    renderer: Renderer,
    mode: Mode,
    direct: [Rgb; LEDS_PER_GIFT],
    last_show_ms: Option<u32>,
    fade: u8,
    effect: u8,
    blink: Option<(Blink, Rgb)>,
    leds_active: bool,

    vbat_mv: u16,
    low_batt: bool,
    temp_c: i8,

    reply_is_status: bool,
    pending_channel: Option<(u32, u8)>,
    reboot_at: Option<(u32, bool)>,
    healthy: bool,
    t_render: u32,
    t_adc: u32,
    t_radio_check: u32,
    t_radio_poll: u32,
    t_status: u32,
}

#[entry]
fn main() -> ! {
    board::init();
    // Already running when started by the bootloader; this also covers a
    // debug session that starts the app directly.
    iwdg::start();

    let shared = sys::shared();
    let shared_ok = shared.magic == BootShared::MAGIC;
    let uid = sys::uid();
    let cfg = ConfigStore::load(&uid);
    let mut hdr = [0u8; ImageHeader::ENCODED_LEN];
    board::flash::read(APP_REGION_BASE, &mut hdr);
    let header = ImageHeader::decode(&hdr);
    let fw_version = header.map_or(0, |h| h.fw_version);
    let (reset_reason, resets) = if shared_ok { (shared.reset_reason, shared.resets) } else { (0, 0) };

    // Radio first: right after an update the host polls this gift's address
    // to confirm the new version, so answer before anything else (LEDs, ADC).
    let mut radio = radio::start(cfg.current.channel, cfg.current.node_addr);
    if let Some(r) = radio.as_mut() {
        let st = Status { uptime_ms: time::now_ms(), fw_version, reset_reason, resets, ..Default::default() };
        radio::preload(r, &Reply::Status(st));
    }
    let leds = Ws2812::new();
    let adc = Adc::new();
    time::enable_irq();
    enable_radio_irq();
    // SAFETY: all handlers are installed and only touch atomics.
    unsafe { cortex_m::interrupt::enable() };

    let seed = u32::from_le_bytes([uid[0], uid[4], uid[8], uid[11]]) ^ time::now_us();
    let mut app = App {
        radio,
        leds,
        adc,
        cfg,
        uid,
        fw_version,
        app_crc: header.map_or(0, |h| h.app_crc),
        boot_version: board::boot_version(),
        boot_protected: board::flash::boot_protected(),
        reset_reason,
        resets,
        echo: 0,
        seq: SeqTracker::new(),
        sync: ClockSync::new(),
        last_edge: 0,
        renderer: Renderer::new(seed),
        mode: Mode::Off,
        direct: [Rgb::BLACK; LEDS_PER_GIFT],
        last_show_ms: None,
        fade: 0,
        effect: 0,
        blink: None,
        leds_active: false,
        vbat_mv: 0,
        low_batt: false,
        temp_c: 0,
        reply_is_status: false,
        pending_channel: None,
        reboot_at: None,
        healthy: false,
        t_render: 0,
        t_adc: 0,
        t_radio_check: 0,
        t_radio_poll: 0,
        t_status: 0,
    };
    app.measure();
    app.preload_status();
    app.run()
}

/// `now - since >= period`, wrapping.
fn elapsed(now: u32, since: u32, period: u32) -> bool {
    now.wrapping_sub(since) >= period
}

impl App {
    fn run(&mut self) -> ! {
        loop {
            iwdg::feed();
            let now = time::now_ms();

            if gpio::radio_irq() || elapsed(now, self.t_radio_poll, RADIO_POLL_MS) {
                self.t_radio_poll = now;
                self.service_radio(now);
            }

            if elapsed(now, self.t_render, FRAME_MS) {
                self.t_render = now;
                self.render(now);
            }

            if elapsed(now, self.t_status, STATUS_REFRESH_MS) {
                self.t_status = now;
                if self.reply_is_status {
                    self.preload_status();
                }
            }

            if elapsed(now, self.t_adc, ADC_PERIOD_MS) {
                self.t_adc = now;
                self.measure();
            }

            if elapsed(now, self.t_radio_check, RADIO_CHECK_MS) {
                self.t_radio_check = now;
                let ok = self.radio.as_mut().is_some_and(radio::healthy);
                if !ok {
                    self.radio = radio::start(self.cfg.current.channel, self.cfg.current.node_addr);
                    self.preload_status();
                }
            }

            if !self.healthy && now >= HEALTHY_AFTER_MS {
                self.healthy = true;
                sys::update_shared(|s| {
                    if s.magic == BootShared::MAGIC {
                        s.boot_attempts = 0;
                    }
                });
            }

            if let Some((t, ch)) = self.pending_channel
                && elapsed(now, t, config_key::CHANNEL_SWITCH_DELAY_MS)
            {
                self.pending_channel = None;
                if let Some(r) = self.radio.as_mut() {
                    radio::set_channel(r, ch);
                }
            }

            if let Some((t, into_boot)) = self.reboot_at
                && elapsed(now, t, REBOOT_DELAY_MS)
            {
                sys::update_shared(|s| {
                    if s.magic != BootShared::MAGIC {
                        *s = BootShared::new();
                    }
                    s.request = if into_boot { BootShared::REQ_STAY_IN_BOOT } else { 0 };
                });
                if let Some(r) = self.radio.as_mut() {
                    radio::shutdown(r);
                }
                sys::reset();
            }
        }
    }

    // -- radio ----------------------------------------------------------------

    fn service_radio(&mut self, now: u32) {
        let Some(mut r) = self.radio.take() else { return };
        // Snapshot the newest IRQ edge before clearing RX_DR: its timestamp
        // belongs to the oldest payload in the FIFO if the edge is new.
        let (count, edge_us) =
            cortex_m::interrupt::free(|_| (EDGE_COUNT.load(Ordering::Relaxed), EDGE_US.load(Ordering::Relaxed)));
        let mut stamp = (count != self.last_edge).then_some(edge_us);
        let _ = r.clear_irqs();
        let mut buf = [0u8; MAX_PAYLOAD];
        while let Ok(Some((pipe, n))) = r.read_rx(&mut buf) {
            self.handle(&mut r, pipe, &buf[..n], stamp.take(), now);
        }
        // Edges raised by payloads read in this pass are consumed.
        self.last_edge = EDGE_COUNT.load(Ordering::Relaxed);
        self.radio = Some(r);
    }

    fn handle(&mut self, r: &mut Radio, pipe: u8, data: &[u8], stamp: Option<u32>, now: u32) {
        let mut reply = None;
        match Packet::decode(data) {
            Ok(Packet::Broadcast(b)) => self.broadcast(r, b, data.len(), stamp, now),
            Ok(Packet::Unicast(u)) => reply = self.unicast(u, now),
            Err(_) => {}
        }
        if pipe == PIPE_UNICAST {
            self.reply_is_status = reply.is_none();
            let reply = reply.unwrap_or_else(|| Reply::Status(self.status()));
            radio::preload(r, &reply);
            self.t_status = now;
        }
    }

    fn broadcast(&mut self, r: &mut Radio, b: Broadcast, len: usize, stamp: Option<u32>, now: u32) {
        let (seq, time_us) = match b {
            Broadcast::Sync { seq, time_us }
            | Broadcast::Music { seq, time_us, .. }
            | Broadcast::Solid { seq, time_us, .. }
            | Broadcast::Leds { seq, time_us, .. }
            | Broadcast::Blackout { seq, time_us } => (seq, time_us),
            Broadcast::SetNodeAddr { uid, node_addr } => {
                if uid == self.uid && valid_node_addr(node_addr) && node_addr != self.cfg.current.node_addr {
                    let mut c = self.cfg.current;
                    c.node_addr = node_addr;
                    if self.cfg.store(c) {
                        radio::set_node_addr(r, node_addr);
                    }
                }
                return;
            }
            Broadcast::EnterBootloader { count, addrs } => {
                let me = self.cfg.current.node_addr;
                if addrs[..(count as usize).min(addrs.len())].contains(&me) {
                    // No ack to wait for: reboot on the next loop iteration.
                    self.reboot_at = Some((now.wrapping_sub(REBOOT_DELAY_MS), true));
                }
                return;
            }
            // BootHold and OTA traffic are for the bootloader.
            _ => return,
        };
        if !self.seq.on_frame(seq, now) {
            return;
        }
        if let Some(local) = stamp {
            self.sync.sample(time_us.wrapping_add(air_time_us(len)), local);
        }
        let slot = self.cfg.current.slot;
        match b {
            Broadcast::Music { frame, .. } => {
                // Frames for other groups (mask) only keep the current look alive.
                if self.renderer.on_frame_for(&frame, self.synced_ms(), slot) {
                    self.effect = frame.effect;
                    self.mode = Mode::Music;
                }
            }
            Broadcast::Solid { first_slot, count, colors, .. } => {
                if slot != UNASSIGNED_SLOT
                    && let Some(i) = slot.checked_sub(first_slot).filter(|&i| i < count.min(8))
                {
                    self.direct = [colors[i as usize]; LEDS_PER_GIFT];
                    self.mode = Mode::Direct;
                }
            }
            Broadcast::Leds { slots, colors, .. } => {
                if slot != UNASSIGNED_SLOT
                    && let Some(k) = slots.iter().position(|&s| s == slot)
                {
                    self.direct = colors[k];
                    self.mode = Mode::Direct;
                }
            }
            Broadcast::Blackout { .. } => {
                self.direct = [Rgb::BLACK; LEDS_PER_GIFT];
                self.mode = Mode::Direct;
            }
            _ => return, // Sync: clock only
        }
        self.last_show_ms = Some(now);
    }

    fn unicast(&mut self, u: Unicast, now: u32) -> Option<Reply> {
        let seq = u.seq();
        self.echo = seq;
        match u {
            Unicast::Poll { .. } => None,
            Unicast::GetIdentity { .. } => Some(Reply::Identity(Identity {
                echo: seq,
                uid: self.uid,
                hw_rev: board::HW_REV as u8,
                fw_version: self.fw_version,
                boot_version: self.boot_version,
                slot: self.cfg.current.slot,
                node_addr: self.cfg.current.node_addr,
                flags: self.flags(),
                app_crc: self.app_crc,
            })),
            Unicast::Blink { color, count, period_10ms, .. } => {
                self.blink = Blink::new(now, count, period_10ms).map(|b| (b, color));
                None
            }
            Unicast::SetSlot { slot, .. } => {
                if (slot < MAX_SLOTS || slot == UNASSIGNED_SLOT) && slot != self.cfg.current.slot {
                    let mut c = self.cfg.current;
                    c.slot = slot;
                    self.cfg.store(c);
                }
                None
            }
            Unicast::SetConfig { key, value, .. } => {
                let mut c = self.cfg.current;
                let (ok, value) = match apply_key(&mut c, key, value) {
                    Some(eff) => {
                        let ok = c == self.cfg.current || self.cfg.store(c);
                        if ok && key == config_key::CHANNEL {
                            self.pending_channel = Some((now, eff as u8));
                        }
                        (ok, eff)
                    }
                    None => (false, 0),
                };
                Some(Reply::ConfigResult { echo: seq, key, ok, value })
            }
            Unicast::Reboot { into_bootloader, .. } => {
                self.reboot_at = Some((now, into_bootloader));
                None
            }
            Unicast::OtaQuery { session, .. } | Unicast::OtaCommit { session, .. } => {
                Some(Reply::OtaBitmap(OtaBitmap {
                    echo: seq,
                    session,
                    state: OtaState::NotInBootloader,
                    window: 0,
                    missing_count: 0,
                    bits: [0; OTA_BITMAP_BYTES],
                }))
            }
        }
    }

    fn flags(&self) -> u8 {
        let mut f = 0;
        if self.cfg.current.slot != UNASSIGNED_SLOT {
            f |= status_flags::SLOT_ASSIGNED;
        }
        if self.sync.synced(time::now_us()) {
            f |= status_flags::TIME_SYNCED;
        }
        if self.low_batt {
            f |= status_flags::LOW_BATTERY;
        }
        if self.leds_active {
            f |= status_flags::LEDS_ACTIVE;
        }
        if self.blink.is_some() {
            f |= status_flags::BLINKING;
        }
        if self.boot_protected {
            f |= status_flags::BOOT_PROTECTED;
        }
        f
    }

    fn status(&self) -> Status {
        Status {
            echo: self.echo,
            flags: self.flags(),
            slot: self.cfg.current.slot,
            uptime_ms: time::now_ms(),
            fw_version: self.fw_version,
            vbat_mv: self.vbat_mv,
            bcast_rx: self.seq.rx,
            bcast_lost: self.seq.lost,
            reset_reason: self.reset_reason,
            resets: self.resets,
            effect: self.effect,
            sync_err_us: self.sync.error_us(),
            temp_c: self.temp_c,
        }
    }

    /// Refresh the pending ack payload with current counters.
    fn preload_status(&mut self) {
        let st = Reply::Status(self.status());
        if let Some(r) = self.radio.as_mut() {
            radio::preload(r, &st);
            self.reply_is_status = true;
        }
    }

    fn measure(&mut self) {
        let m = self.adc.read();
        self.vbat_mv = m.vbat_mv;
        self.low_batt = low_battery(self.low_batt, m.vbat_mv);
        self.temp_c = m.temp_c;
    }

    // -- LEDs -----------------------------------------------------------------

    /// Bridge-synced ms clock (the local clock until the first show frame).
    fn synced_ms(&self) -> u32 {
        self.sync.now(time::now_us()) / 1000
    }

    fn render(&mut self, now: u32) {
        let cfg = self.cfg.current;
        let mut px = [Rgb::BLACK; LEDS_PER_GIFT];

        let blink = self.blink.and_then(|(b, c)| b.phase(now).map(|on| if on { c } else { Rgb::BLACK }));
        if blink.is_none() {
            self.blink = None;
        }

        // Show content fades out after the idle timeout without show frames.
        let alive = self.last_show_ms.is_some_and(|t| now.wrapping_sub(t) < cfg.idle_timeout_100ms as u32 * 100);
        self.fade = if alive { self.fade.saturating_add(FADE_STEP) } else { self.fade.saturating_sub(FADE_STEP) };

        if let Some(c) = blink {
            px = [c; LEDS_PER_GIFT];
        } else if self.last_show_ms.is_none() && now < BOOT_ANIM_MS {
            // Boot animation: a rainbow wheel spinning once, fading out.
            let level = (255 - now * 255 / BOOT_ANIM_MS) as u8;
            for (i, p) in px.iter_mut().enumerate() {
                *p = hsv((now * 256 / BOOT_ANIM_MS) as u8 + i as u8 * 64, 255, level / 3);
            }
        } else {
            match self.mode {
                Mode::Music => self.renderer.render(self.synced_ms(), cfg.slot, &mut px),
                Mode::Direct => px = self.direct,
                Mode::Off => {}
            }
            if self.mode != Mode::Off {
                for p in px.iter_mut() {
                    *p = scale8(*p, self.fade);
                }
                if self.fade == 0 {
                    self.mode = Mode::Off;
                }
            }
        }

        power_limit(&mut px, cfg.power_budget_ma);
        for p in px.iter_mut() {
            *p = scale8(gamma(*p), cfg.max_brightness);
        }
        self.leds_active = px.iter().any(|p| *p != Rgb::BLACK);
        self.leds.write(&px);
    }
}
