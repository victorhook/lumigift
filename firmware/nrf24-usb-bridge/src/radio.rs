//! The radio task: executes host commands strictly in order and turns radio
//! activity into events.
//!
//! Chip-state bookkeeping: `cfg`/`pipes`/`slots`/`mode` are what the host
//! asked for; the `chip_*` fields shadow what is actually in the nRF's
//! registers, so pipe-0 borrowing and TX_ADDR changes cost SPI traffic only
//! when something really changes.

use core::sync::atomic::Ordering;

use embassy_futures::select::{Either, select};
use embassy_stm32::exti::ExtiInput;
use embassy_stm32::gpio::Output;
use embassy_stm32::mode::{Async, Blocking};
use embassy_stm32::spi::{self, Spi};
use embassy_time::{Duration, Instant, Timer};
use embedded_hal::spi::{ErrorType, Operation, SpiBus, SpiDevice};
use nrf24::{Nrf24, Status, reg};
use nrf24_usb_proto::{
    self as proto, CmdStatus, Command, DataRate, Event, Info, Mode, Payload, RadioConfig, Stats, TxStatus, features,
    tx_flags,
};

use crate::queues::{CMD_QUEUE, CMDS, EVENTS, OutPacket, RADIO_ACTIVITY, RX_EVENT_LIMIT};

// ---------------------------------------------------------------------------
// SPI device: blocking SPI1 + software CSN.

pub struct SpiDev {
    pub bus: Spi<'static, Blocking, spi::mode::Master>,
    pub csn: Output<'static>,
}

impl ErrorType for SpiDev {
    type Error = spi::Error;
}

impl SpiDevice for SpiDev {
    fn transaction(&mut self, ops: &mut [Operation<'_, u8>]) -> Result<(), spi::Error> {
        self.csn.set_low();
        let mut res = Ok(());
        for op in ops.iter_mut() {
            res = match op {
                Operation::Read(b) => SpiBus::read(&mut self.bus, b),
                Operation::Write(b) => SpiBus::write(&mut self.bus, b),
                Operation::Transfer(r, w) => SpiBus::transfer(&mut self.bus, r, w),
                Operation::TransferInPlace(b) => SpiBus::transfer_in_place(&mut self.bus, b),
                Operation::DelayNs(ns) => {
                    cortex_m::asm::delay(*ns / 13 + 1); // ~13.9 ns per cycle @ 72 MHz
                    Ok(())
                }
            };
            if res.is_err() {
                break;
            }
        }
        let flushed = SpiBus::<u8>::flush(&mut self.bus);
        self.csn.set_high();
        res.and(flushed)
    }
}

type Nrf = Nrf24<SpiDev, Output<'static>>;

/// Any SPI/pin failure talking to the radio.
#[derive(Debug, Clone, Copy)]
struct Fault;

impl<E> From<nrf24::Error<E>> for Fault {
    fn from(_: nrf24::Error<E>) -> Self {
        Fault
    }
}

type R<T> = Result<T, Fault>;

// FIFO_STATUS bits
const TX_FULL: u8 = 1 << 5;
const TX_EMPTY: u8 = 1 << 4;

/// Datasheet Tpd2stby (worst case with a 90 mH crystal inductance).
const POWER_UP_US: u64 = 1500;
/// Tstby2a: PLL settle from standby to TX/RX.
const SETTLE_US: u64 = 130;
/// RPD needs RX settled + 40 µs of AGC/RSSI (datasheet 6.4).
const RPD_MIN_DWELL_US: u64 = 40;
/// Keep each continuous TX-mode run under this (datasheet: never > 4 ms in TX mode).
const TX_RUN_BUDGET_US: u32 = 3500;
/// Abort a NO_ACK burst if the FIFO makes no progress for this long.
const STREAM_STALL_MS: u64 = 20;

fn now_us() -> u32 {
    Instant::now().as_micros() as u32
}

fn activity() {
    RADIO_ACTIVITY.fetch_add(1, Ordering::Relaxed);
}

// ---------------------------------------------------------------------------
// Command source: decodes queued packets one message at a time, with one
// message of lookahead so a NO_ACK burst can be streamed.

enum Item {
    Cmd(Command),
    /// Undecodable message. `kind` is known unless the framing was broken;
    /// `tag` is recovered from a malformed `Tx` so it still gets its TxDone.
    Bad {
        kind: Option<u8>,
        tag: Option<u16>,
    },
}

const KIND_TX: u8 = 0x06;
/// Configure, SetRxPipe, SetTxAddr, SetAckPayload, SetMode, Reset: the kinds
/// that answer with `CmdResult`, so an undecodable one gets `BadArgs`.
const CMD_RESULT_KINDS: [u8; 6] = [0x03, 0x04, 0x05, 0x07, 0x08, 0x0B];

pub struct Source {
    pkt: OutPacket,
    pos: usize,
    peeked: Option<Item>,
}

impl Source {
    pub const fn new() -> Self {
        Self { pkt: OutPacket::new(), pos: 0, peeked: None }
    }

    fn parse_next(&mut self) -> Option<Item> {
        let data = self.pkt.as_slice();
        let &len = data.get(self.pos)?;
        if len == 0 {
            self.pos = data.len();
            return None;
        }
        let start = self.pos + 1;
        let end = start + len as usize;
        if end > data.len() {
            self.pos = data.len();
            return Some(Item::Bad { kind: None, tag: None });
        }
        self.pos = end;
        let msg = &data[start..end];
        Some(match Command::decode(msg) {
            Ok(c) => Item::Cmd(c),
            Err(_) => Item::Bad {
                kind: Some(msg[0]),
                tag: (msg[0] == KIND_TX && msg.len() >= 3).then(|| u16::from_le_bytes([msg[1], msg[2]])),
            },
        })
    }

    fn try_next(&mut self) -> Option<Item> {
        if let Some(i) = self.peeked.take() {
            return Some(i);
        }
        loop {
            if let Some(i) = self.parse_next() {
                return Some(i);
            }
            self.pkt = CMDS.try_receive().ok()?;
            self.pos = 0;
        }
    }

    /// Cancel-safe: dropping this future never loses a message.
    async fn next(&mut self) -> Item {
        loop {
            if let Some(i) = self.try_next() {
                return i;
            }
            self.pkt = CMDS.receive().await;
            self.pos = 0;
        }
    }

    /// Look at the next message if one is available right now.
    fn peek(&mut self) -> Option<&Item> {
        if self.peeked.is_none() {
            self.peeked = self.try_next();
        }
        self.peeked.as_ref()
    }

    fn take_peeked(&mut self) -> Option<Item> {
        self.peeked.take()
    }
}

// ---------------------------------------------------------------------------

#[derive(Clone, Copy)]
struct PipeCfg {
    enabled: bool,
    auto_ack: bool,
    addr: [u8; 5],
}

#[derive(Clone, Copy)]
struct TxCmd {
    tag: u16,
    slot: u8,
    flags: u8,
    ts_offset: u8,
    payload: Payload,
}

impl TxCmd {
    fn no_ack(&self) -> bool {
        self.flags & tx_flags::NO_ACK != 0
    }
}

/// Small FIFO of tags whose packets are in the radio's 3-deep TX FIFO.
struct InFlight {
    tags: [u16; 3],
    len: usize,
}

impl InFlight {
    fn push(&mut self, t: u16) {
        self.tags[self.len] = t;
        self.len += 1;
    }
    fn pop(&mut self) -> u16 {
        let t = self.tags[0];
        self.tags.copy_within(1.., 0);
        self.len -= 1;
        t
    }
}

pub struct Bridge {
    nrf: Nrf,
    irq: ExtiInput<'static, Async>,
    present: bool,
    uid: [u8; 12],

    // host-requested state
    cfg: RadioConfig,
    mode: Mode,
    pipes: [PipeCfg; 6],
    slots: [[u8; 5]; proto::TX_ADDR_SLOTS as usize],
    stats: Stats,

    // derived from cfg
    dpl: bool,
    run_limit: u8,

    // chip shadow
    powered: bool,
    prx: bool,
    chip_p0: Option<[u8; 5]>,
    chip_tx: Option<[u8; 5]>,
    chip_en_rxaddr: u8,
    chip_en_aa: u8,
}

fn chip_config(c: &RadioConfig) -> nrf24::Config {
    nrf24::Config {
        channel: c.channel,
        data_rate: match c.data_rate {
            DataRate::Kbps250 => nrf24::DataRate::Kbps250,
            DataRate::Mbps1 => nrf24::DataRate::Mbps1,
            DataRate::Mbps2 => nrf24::DataRate::Mbps2,
        },
        power: match c.power {
            proto::Power::Min => nrf24::Power::Min,
            proto::Power::Low => nrf24::Power::Low,
            proto::Power::High => nrf24::Power::High,
            proto::Power::Max => nrf24::Power::Max,
        },
        crc: match c.crc_len {
            0 => nrf24::CrcMode::Off,
            1 => nrf24::CrcMode::OneByte,
            _ => nrf24::CrcMode::TwoBytes,
        },
        addr_width: c.addr_width,
        retries: c.retries,
        retry_delay: c.retry_delay,
        dynamic_payloads: c.features & features::DYN_PAYLOAD != 0,
        ack_payloads: c.features & features::ACK_PAYLOAD != 0,
        dyn_ack: c.features & features::DYN_ACK != 0,
        payload_len: c.payload_len,
    }
}

fn config_valid(c: &RadioConfig) -> bool {
    c.channel <= 125
        && c.crc_len <= 2
        && (3..=5).contains(&c.addr_width)
        && c.retries <= 15
        && c.retry_delay <= 15
        && c.features & !0x07 == 0
        && (1..=32).contains(&c.payload_len)
}

/// On-air time of a maximum-size (32 B) packet in µs.
fn max_airtime_us(c: &RadioConfig) -> u32 {
    let bits = 8 * (1 + c.addr_width as u32 + 32 + c.crc_len as u32) + 9;
    match c.data_rate {
        DataRate::Mbps2 => bits / 2,
        DataRate::Mbps1 => bits,
        DataRate::Kbps250 => bits * 4,
    }
}

fn set_bit(v: u8, bit: u8, on: bool) -> u8 {
    if on { v | (1 << bit) } else { v & !(1 << bit) }
}

impl Bridge {
    pub fn new(spi: SpiDev, ce: Output<'static>, irq: ExtiInput<'static, Async>, uid: [u8; 12]) -> Self {
        let cfg = RadioConfig::default();
        let mut b = Self {
            nrf: Nrf24::new(spi, ce),
            irq,
            present: false,
            uid,
            cfg,
            mode: Mode::Standby,
            pipes: [PipeCfg { enabled: false, auto_ack: false, addr: [0; 5] }; 6],
            slots: [[0; 5]; proto::TX_ADDR_SLOTS as usize],
            stats: Stats::default(),
            dpl: true,
            run_limit: 1,
            powered: false,
            prx: false,
            chip_p0: None,
            chip_tx: None,
            chip_en_rxaddr: 0,
            chip_en_aa: 0,
        };
        b.derive();
        b
    }

    fn derive(&mut self) {
        let f = self.cfg.features;
        self.dpl = f & (features::DYN_PAYLOAD | features::ACK_PAYLOAD) != 0;
        self.run_limit = (TX_RUN_BUDGET_US / max_airtime_us(&self.cfg)).clamp(1, 255) as u8;
    }

    // -- events -------------------------------------------------------------

    /// Replies are never dropped: wait for queue space (backpressure).
    async fn reply(&mut self, ev: Event) {
        EVENTS.send(ev).await;
    }

    async fn cmd_result(&mut self, kind: u8, status: CmdStatus) {
        self.reply(Event::CmdResult { kind, status }).await;
    }

    async fn tx_done(&mut self, tag: u16, status: TxStatus, retries: u8, ack_payload: Payload) {
        match status {
            TxStatus::Sent => self.stats.tx_sent += 1,
            TxStatus::Acked => self.stats.tx_acked += 1,
            TxStatus::MaxRetries | TxStatus::Failed => self.stats.tx_failed += 1,
        }
        let ev = Event::TxDone { tag, status, retries, time_us: now_us(), ack_payload };
        self.reply(ev).await;
    }

    async fn tx_failed(&mut self, tag: u16) {
        self.tx_done(tag, TxStatus::Failed, 0, Payload::default()).await;
    }

    /// Unsolicited events are dropped (and counted) rather than stalling the radio.
    fn emit_rx(&mut self, pipe: u8, time_us: u32, data: &[u8]) {
        self.stats.rx += 1;
        activity();
        let Ok(payload) = Payload::new(data) else {
            return;
        };
        let ev = Event::Rx { pipe, time_us, payload };
        if EVENTS.len() >= RX_EVENT_LIMIT || EVENTS.try_send(ev).is_err() {
            self.stats.events_dropped += 1;
        }
    }

    // -- chip setup -----------------------------------------------------------

    /// Probe + full configuration. Leaves the chip powered up in the idle
    /// state of the current mode.
    pub async fn probe(&mut self) -> bool {
        self.present = false;
        self.prx = false;
        self.powered = false;
        if self.nrf.init(&chip_config(&self.cfg)).is_err() {
            return false;
        }
        self.chip_en_rxaddr = 0;
        self.chip_en_aa = 0;
        self.chip_p0 = None;
        self.chip_tx = None;
        self.present = true;
        let ok = async {
            self.apply_pipes()?;
            if self.mode != Mode::PowerDown {
                self.ensure_powered().await?;
            }
            self.enter_idle()
        }
        .await;
        self.present = ok.is_ok();
        self.present
    }

    async fn ensure_powered(&mut self) -> R<()> {
        if !self.powered {
            self.nrf.power_up()?;
            self.powered = true;
            Timer::after_micros(POWER_UP_US).await;
        }
        Ok(())
    }

    fn write_addr(&mut self, r: u8, addr: &[u8; 5]) -> R<()> {
        let aw = self.cfg.addr_width.clamp(3, 5) as usize;
        self.nrf.write_reg_bytes(r, &addr[..aw])?;
        Ok(())
    }

    fn set_en_bits(&mut self, rxaddr: u8, aa: u8) -> R<()> {
        if rxaddr != self.chip_en_rxaddr {
            self.nrf.write_reg(reg::EN_RXADDR, rxaddr)?;
            self.chip_en_rxaddr = rxaddr;
        }
        if aa != self.chip_en_aa {
            self.nrf.write_reg(reg::EN_AA, aa)?;
            self.chip_en_aa = aa;
        }
        Ok(())
    }

    /// Program pipe 0 (address + enable bits) if it differs from the chip.
    fn set_p0(&mut self, addr: [u8; 5], enabled: bool, auto_ack: bool) -> R<()> {
        if self.chip_p0 != Some(addr) {
            self.write_addr(reg::RX_ADDR_P0, &addr)?;
            self.chip_p0 = Some(addr);
        }
        let rx = set_bit(self.chip_en_rxaddr, 0, enabled);
        let aa = set_bit(self.chip_en_aa, 0, auto_ack);
        self.set_en_bits(rx, aa)
    }

    fn restore_p0(&mut self) -> R<()> {
        let p = self.pipes[0];
        self.set_p0(p.addr, p.enabled, p.auto_ack)
    }

    /// Write every pipe's configuration (CE must be low).
    fn apply_pipes(&mut self) -> R<()> {
        let p1 = self.pipes[1].addr;
        self.write_addr(reg::RX_ADDR_P1, &p1)?;
        for p in 2..6u8 {
            self.nrf.write_reg(reg::RX_ADDR_P0 + p, self.pipes[p as usize].addr[0])?;
        }
        let (mut rx, mut aa) = (0u8, 0u8);
        for (i, p) in self.pipes.iter().enumerate() {
            rx = set_bit(rx, i as u8, p.enabled);
            aa = set_bit(aa, i as u8, p.auto_ack);
        }
        self.chip_p0 = None;
        self.set_en_bits(rx, aa)?;
        self.restore_p0()
    }

    fn set_tx_addr(&mut self, slot: u8) -> R<()> {
        let addr = self.slots[slot as usize];
        if self.chip_tx != Some(addr) {
            self.write_addr(reg::TX_ADDR, &addr)?;
            self.chip_tx = Some(addr);
        }
        Ok(())
    }

    /// Put the chip in the idle state of `mode`.
    fn enter_idle(&mut self) -> R<()> {
        match self.mode {
            Mode::PowerDown => {
                self.nrf.power_down()?;
                self.powered = false;
                self.prx = false;
            }
            Mode::Standby => {
                self.nrf.standby()?;
                self.prx = false;
            }
            Mode::Rx => {
                if !self.prx {
                    self.restore_p0()?;
                    self.nrf.start_rx()?;
                    self.prx = true;
                }
            }
        }
        Ok(())
    }

    /// Stop listening (CE low, PTX) and report whatever is still in the RX FIFO.
    fn leave_rx(&mut self) -> R<()> {
        if self.prx {
            self.nrf.standby()?;
            self.prx = false;
            self.drain_rx(now_us())?;
        }
        Ok(())
    }

    fn drain_rx(&mut self, t: u32) -> R<()> {
        let mut buf = [0u8; 32];
        // At most 3 payloads can be queued; bound the loop anyway.
        for _ in 0..4 {
            match self.nrf.read_rx(&mut buf) {
                Ok(Some((pipe, len))) => self.emit_rx(pipe, t, &buf[..len]),
                Ok(None) => break,
                Err(nrf24::Error::BadPayloadWidth) => {} // FIFO flushed by the driver
                Err(e) => return Err(e.into()),
            }
        }
        Ok(())
    }

    /// IRQ while listening: RX_DR (packet) and/or TX_DS (ack payload sent).
    pub async fn service_rx(&mut self) {
        let t = now_us();
        // Clear first, then drain: a packet landing after the clear re-asserts IRQ.
        let r = self.nrf.clear_irqs().map_err(Fault::from).and_then(|st| {
            self.drain_rx(t)?;
            Ok(st)
        });
        match r {
            Ok(st) if st.0 & (Status::RX_DR | Status::TX_DS | Status::MAX_RT) != 0 => {}
            // IRQ low with no flag set (wiring/noise) or SPI fault: don't spin.
            _ => Timer::after_micros(500).await,
        }
    }

    // -- TX -------------------------------------------------------------------

    /// Check a Tx and build the exact bytes to load (padded in static mode).
    fn prepare(&self, tx: &TxCmd) -> Option<([u8; 32], usize)> {
        let data = tx.payload.as_slice();
        if tx.slot >= proto::TX_ADDR_SLOTS || data.is_empty() {
            return None;
        }
        if tx.flags & tx_flags::STAMP != 0 && tx.ts_offset as usize + 4 > data.len() {
            return None;
        }
        if tx.no_ack() && self.cfg.features & features::DYN_ACK == 0 {
            return None; // W_TX_PAYLOAD_NOACK is disabled without EN_DYN_ACK
        }
        let len = if self.dpl {
            data.len()
        } else if data.len() <= self.cfg.payload_len as usize {
            self.cfg.payload_len as usize
        } else {
            return None;
        };
        let mut buf = [0u8; 32];
        buf[..data.len()].copy_from_slice(data);
        Some((buf, len))
    }

    /// Stamp (if requested) and load into the TX FIFO.
    fn load(&mut self, tx: &TxCmd, mut buf: [u8; 32], len: usize) -> R<()> {
        if tx.flags & tx_flags::STAMP != 0 {
            let o = tx.ts_offset as usize;
            buf[o..o + 4].copy_from_slice(&now_us().to_le_bytes());
        }
        let st = self.nrf.write_tx(&buf[..len], tx.no_ack())?;
        // STATUS is clocked out before the payload: TX_FULL means it was dropped.
        // Cannot happen with correct accounting; never report such a packet as sent.
        if st.tx_full() {
            return Err(Fault);
        }
        activity();
        Ok(())
    }

    /// Get the chip into PTX standby with clean flags and an empty TX FIFO.
    async fn prepare_ptx(&mut self) -> R<()> {
        self.ensure_powered().await?;
        self.leave_rx()?;
        self.nrf.ce(false)?;
        // Unsent ack payloads (SetAckPayload) share the TX FIFO and would go
        // out as data packets; leftovers from a fault would too.
        self.nrf.flush_tx()?;
        self.nrf.clear_irqs()?;
        Ok(())
    }

    async fn tx(&mut self, src: &mut Source, first: TxCmd) {
        let Some((buf, len)) = self.prepare(&first).filter(|_| self.present) else {
            self.tx_failed(first.tag).await;
            return;
        };
        if self.prepare_ptx().await.is_err() {
            self.tx_failed(first.tag).await;
            return;
        }
        if first.no_ack() {
            self.tx_stream(src, first, buf, len).await;
        } else {
            self.tx_acked(first, buf, len).await;
        }
        let _ = self.enter_idle();
    }

    /// One auto-ack transmission. Pipe 0 is borrowed for the ACK; it is
    /// restored when the chip next listens (only then is it observable).
    async fn tx_acked(&mut self, tx: TxCmd, buf: [u8; 32], len: usize) {
        let setup = (|| -> R<()> {
            self.set_tx_addr(tx.slot)?;
            let addr = self.slots[tx.slot as usize];
            self.set_p0(addr, true, true)?;
            self.load(&tx, buf, len)?;
            Ok(self.nrf.ce(true)?)
        })();
        if setup.is_err() {
            let _ = self.nrf.flush_tx();
            self.tx_failed(tx.tag).await;
            return;
        }

        // Worst case: (retries + 1) * (ARD + packet + ACK with payload).
        let per_try = (self.cfg.retry_delay as u64 + 1) * 250 + 2 * max_airtime_us(&self.cfg) as u64 + SETTLE_US;
        let deadline =
            Instant::now() + Duration::from_micros(SETTLE_US + (self.cfg.retries as u64 + 1) * per_try + 2000);
        let st = loop {
            // The IRQ is the fast path; the timer only matters if the IRQ
            // line is broken, in which case we fall back to polling STATUS.
            let _ = select(self.irq.wait_for_low(), Timer::after_millis(2)).await;
            match self.nrf.status() {
                Ok(st) if st.tx_sent() || st.max_retries() => break Some(st),
                Ok(_) if Instant::now() < deadline => continue,
                _ => break None,
            }
        };
        let _ = self.nrf.ce(false);
        let _ = self.nrf.clear_irqs();
        let retries = self.nrf.observe_tx().map(|(_, arc)| arc).unwrap_or(0);

        match st {
            Some(st) if st.tx_sent() => {
                let mut ack = Payload::default();
                let mut b = [0u8; 32];
                if let Ok(Some((_, n))) = self.nrf.read_rx(&mut b) {
                    ack = Payload::new(&b[..n]).unwrap_or_default();
                }
                // Anything else in the RX FIFO is stale for a PTX.
                let _ = self.nrf.flush_rx();
                self.tx_done(tx.tag, TxStatus::Acked, retries, ack).await;
            }
            Some(_) => {
                let _ = self.nrf.flush_tx();
                self.tx_done(tx.tag, TxStatus::MaxRetries, retries, Payload::default()).await;
            }
            None => {
                let _ = self.nrf.flush_tx();
                self.tx_failed(tx.tag).await;
            }
        }
    }

    /// NO_ACK burst: CE stays high and the 3-deep TX FIFO is refilled from the
    /// command queue for as long as the next queued messages are NO_ACK Tx to
    /// the same slot, so packets go out back to back.
    ///
    /// Completion accounting only trusts FIFO_STATUS, never the number of
    /// IRQs (TX_DS flags coalesce): EMPTY means all done, FULL means 3 left,
    /// otherwise at most min(n, 2) are left. Reports are therefore in order
    /// and never early; at worst one packet-time late at the tail of a burst.
    async fn tx_stream(&mut self, src: &mut Source, first: TxCmd, buf: [u8; 32], len: usize) {
        let slot = first.slot;
        let mut inflight = InFlight { tags: [0; 3], len: 0 };
        let mut run: u8 = 0; // packets loaded since the FIFO was last empty

        let started =
            self.set_tx_addr(slot).and_then(|_| self.load(&first, buf, len)).and_then(|_| Ok(self.nrf.ce(true)?));
        if started.is_err() {
            let _ = self.nrf.ce(false);
            let _ = self.nrf.flush_tx();
            self.tx_failed(first.tag).await;
            return;
        }
        inflight.push(first.tag);
        run += 1;

        let mut last_progress = Instant::now();
        loop {
            // Refill while the next message is streamable.
            while inflight.len < 3 && run < self.run_limit {
                let Some(&Item::Cmd(Command::Tx { tag, slot: s, flags, ts_offset, payload })) = src.peek() else {
                    break;
                };
                let next = TxCmd { tag, slot: s, flags, ts_offset, payload };
                // A STAMP packet only goes into an empty FIFO so its stamp-to-air
                // latency stays constant (no queueing behind other packets).
                if !next.no_ack() || next.slot != slot || (next.flags & tx_flags::STAMP != 0 && inflight.len > 0) {
                    break;
                }
                // Invalid ones are handled (in order) after the burst.
                let Some((b, l)) = self.prepare(&next) else {
                    break;
                };
                let _ = src.take_peeked();
                if self.load(&next, b, l).is_err() {
                    // Report everything loaded so far as failed, then this one.
                    self.abort_stream(&mut inflight).await;
                    self.tx_failed(next.tag).await;
                    return;
                }
                inflight.push(next.tag);
                run += 1;
            }
            if inflight.len == 0 {
                break;
            }

            let _ = select(self.irq.wait_for_low(), Timer::after_millis(5)).await;
            // Clear first, then sample the FIFO: a completion after the clear
            // re-asserts IRQ, one before it is visible in FIFO_STATUS.
            let fifo = match self.nrf.clear_irqs().and_then(|_| self.nrf.fifo_status()) {
                Ok(f) => f,
                Err(_) => {
                    self.abort_stream(&mut inflight).await;
                    return;
                }
            };
            let remaining = if fifo & TX_EMPTY != 0 {
                0
            } else if fifo & TX_FULL != 0 {
                3
            } else {
                inflight.len.min(2)
            };
            let done = inflight.len.saturating_sub(remaining);
            if done > 0 {
                last_progress = Instant::now();
            } else if last_progress.elapsed() > Duration::from_millis(STREAM_STALL_MS) {
                // No packet left the FIFO for far longer than 3 packet times.
                self.abort_stream(&mut inflight).await;
                return;
            }
            for _ in 0..done {
                let tag = inflight.pop();
                activity();
                self.tx_done(tag, TxStatus::Sent, 0, Payload::default()).await;
            }
            if fifo & TX_EMPTY != 0 {
                // Standby-II now; the next load re-settles the PLL (130 µs),
                // which starts a fresh TX-mode run.
                run = 0;
            }
        }
        let _ = self.nrf.ce(false);
    }

    async fn abort_stream(&mut self, inflight: &mut InFlight) {
        let _ = self.nrf.ce(false);
        let _ = self.nrf.flush_tx();
        let _ = self.nrf.clear_irqs();
        while inflight.len > 0 {
            let tag = inflight.pop();
            self.tx_failed(tag).await;
        }
    }

    // -- other commands ---------------------------------------------------------

    async fn scan(&mut self, first: u8, last: u8, dwell_us: u16, repeats: u8) {
        const KIND: u8 = 0x09; // Scan only answers with CmdResult on failure
        if first > last || last > 125 || repeats == 0 {
            return self.cmd_result(KIND, CmdStatus::BadArgs).await;
        }
        if !self.present {
            return self.cmd_result(KIND, CmdStatus::RadioError).await;
        }
        let dwell = SETTLE_US + (dwell_us as u64).max(RPD_MIN_DWELL_US);
        let mut ok = async {
            self.ensure_powered().await?;
            self.leave_rx()?;
            self.nrf.start_rx()?; // PRIM_RX + CE high
            Ok::<(), Fault>(self.nrf.ce(false)?)
        }
        .await
        .is_ok();
        for ch in first..=last {
            let mut hits = 0u16;
            for _ in 0..repeats {
                if !ok {
                    break;
                }
                // RX must be (re)entered for each sample; RPD reflects the
                // current RX session after 130 µs settle + 40 µs.
                ok = self.nrf.set_channel(ch).and_then(|_| self.nrf.ce(true)).is_ok();
                Timer::after_micros(dwell).await;
                match self.nrf.rpd() {
                    Ok(true) => hits += 1,
                    Ok(false) => {}
                    Err(_) => ok = false,
                }
                ok &= self.nrf.ce(false).is_ok();
            }
            if !ok {
                break;
            }
            self.reply(Event::ScanResult { channel: ch, hits, samples: repeats as u16 }).await;
        }
        // Back to the configured channel; packets caught while hopping are noise.
        let channel = self.cfg.channel;
        let restored = (|| -> R<()> {
            self.nrf.standby()?;
            self.nrf.set_channel(channel)?;
            self.nrf.flush_rx()?;
            self.nrf.clear_irqs()?;
            self.enter_idle()
        })();
        if !ok || restored.is_err() {
            self.cmd_result(KIND, CmdStatus::RadioError).await;
        }
    }

    fn configure(&mut self, c: RadioConfig) -> CmdStatus {
        if !config_valid(&c) {
            return CmdStatus::BadArgs;
        }
        self.cfg = c;
        self.derive();
        if !self.present {
            return CmdStatus::RadioError;
        }
        let r = (|| -> R<()> {
            self.leave_rx()?;
            self.nrf.ce(false)?;
            self.nrf.configure(&chip_config(&self.cfg))?;
            self.chip_tx = None;
            self.apply_pipes()?;
            self.enter_idle()
        })();
        if r.is_ok() { CmdStatus::Ok } else { CmdStatus::RadioError }
    }

    fn set_rx_pipe(&mut self, pipe: u8, enabled: bool, auto_ack: bool, addr: [u8; 5]) -> CmdStatus {
        if pipe > 5 {
            return CmdStatus::BadArgs;
        }
        self.pipes[pipe as usize] = PipeCfg { enabled, auto_ack, addr };
        if !self.present {
            return CmdStatus::RadioError;
        }
        let r = (|| -> R<()> {
            self.leave_rx()?;
            self.apply_pipes()?;
            self.enter_idle()
        })();
        if r.is_ok() { CmdStatus::Ok } else { CmdStatus::RadioError }
    }

    fn set_ack_payload(&mut self, pipe: u8, payload: Payload) -> CmdStatus {
        if pipe > 5 || payload.is_empty() || self.cfg.features & features::ACK_PAYLOAD == 0 {
            return CmdStatus::BadArgs;
        }
        if !self.present {
            return CmdStatus::RadioError;
        }
        // SPI and the FIFOs work in any power state; no need to power up.
        match self.nrf.write_ack_payload(pipe, payload.as_slice()) {
            // STATUS is clocked out before the write: TX_FULL means it was ignored.
            Ok(st) if !st.tx_full() => CmdStatus::Ok,
            _ => CmdStatus::RadioError,
        }
    }

    async fn set_mode(&mut self, m: Mode) -> CmdStatus {
        self.mode = m;
        if !self.present {
            return CmdStatus::RadioError;
        }
        let r = async {
            self.leave_rx()?;
            if m != Mode::PowerDown {
                self.ensure_powered().await?;
            }
            self.enter_idle()
        }
        .await;
        if r.is_ok() { CmdStatus::Ok } else { CmdStatus::RadioError }
    }

    fn info(&self) -> Event {
        Event::Info(Info {
            protocol_version: proto::PROTOCOL_VERSION,
            fw_version: crate::FW_VERSION,
            tx_queue_depth: CMD_QUEUE as u8,
            radio_present: self.present,
            uid: self.uid,
        })
    }

    // -- dispatch -----------------------------------------------------------

    async fn handle(&mut self, src: &mut Source, item: Item) {
        let cmd = match item {
            Item::Cmd(c) => c,
            Item::Bad { kind, tag } => {
                self.stats.bad_commands += 1;
                match (kind, tag) {
                    (_, Some(tag)) => self.tx_failed(tag).await,
                    (Some(k), None) if CMD_RESULT_KINDS.contains(&k) => self.cmd_result(k, CmdStatus::BadArgs).await,
                    _ => {}
                }
                return;
            }
        };
        let status = match cmd {
            Command::GetInfo => {
                if !self.present {
                    self.probe().await; // hot-plugged / fixed wiring
                }
                let ev = self.info();
                return self.reply(ev).await;
            }
            Command::Ping { seq } => {
                return self.reply(Event::Pong { seq, time_us: now_us() }).await;
            }
            Command::GetStats => return self.reply(Event::Stats(self.stats)).await,
            Command::Tx { tag, slot, flags, ts_offset, payload } => {
                return self.tx(src, TxCmd { tag, slot, flags, ts_offset, payload }).await;
            }
            Command::Scan { first, last, dwell_us, repeats } => return self.scan(first, last, dwell_us, repeats).await,
            Command::Configure(c) => self.configure(c),
            Command::SetRxPipe { pipe, enabled, auto_ack, addr } => self.set_rx_pipe(pipe, enabled, auto_ack, addr),
            // Pure bridge state; works without a radio.
            Command::SetTxAddr { slot, addr } if slot < proto::TX_ADDR_SLOTS => {
                self.slots[slot as usize] = addr;
                CmdStatus::Ok
            }
            Command::SetTxAddr { .. } => CmdStatus::BadArgs,
            Command::SetAckPayload { pipe, payload } => self.set_ack_payload(pipe, payload),
            Command::SetMode(m) => self.set_mode(m).await,
            Command::Reset => {
                self.cmd_result(cmd.kind(), CmdStatus::Ok).await;
                // Give the IN task a moment to deliver the reply.
                let t0 = Instant::now();
                while !EVENTS.is_empty() && t0.elapsed() < Duration::from_millis(50) {
                    Timer::after_millis(1).await;
                }
                Timer::after_millis(5).await;
                let _ = self.nrf.power_down();
                cortex_m::peripheral::SCB::sys_reset();
            }
        };
        self.cmd_result(cmd.kind(), status).await;
    }
}

#[embassy_executor::task]
pub async fn radio_task(mut b: Bridge) -> ! {
    b.probe().await;
    let mut src = Source::new();
    loop {
        if b.prx {
            let r = select(b.irq.wait_for_low(), src.next()).await;
            match r {
                Either::First(()) => b.service_rx().await,
                Either::Second(item) => b.handle(&mut src, item).await,
            }
        } else {
            let item = src.next().await;
            b.handle(&mut src, item).await;
        }
    }
}
