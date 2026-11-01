//! Minimal nRF24 RX FIFO drain on raw SPI1/GPIOA registers.
//!
//! [`drain`] and its SPI byte transfer live in RAM (`.data.ramfunc.*`): the
//! RAM flash loops of [`crate::flash`] call them while the flash is busy,
//! and the bootloader's main loop calls the same code when it is idle. So
//! they must not call anything that lives in flash: no PAC accessors, no
//! bounds checks that could panic, no `memcpy`, no division. Per packet: STATUS (NOP),
//! `R_RX_PL_WID`, `R_RX_PAYLOAD` straight into the ring slot, then clear
//! RX_DR. About 50 µs per 28-byte chunk at 6 MHz SPI, well under the
//! ~300 µs a packet takes on air at 1 Mbps.

use lumigift_logic::recovery::{BCAST_SLOTS, UNI_SLOTS};
use lumigift_logic::rxring::RxRing;

use crate::radio::{PIPE_BROADCAST, PIPE_UNICAST};

const SPI1_SR: *const u32 = 0x4001_3008 as *const u32;
const SPI1_DR8: *mut u8 = 0x4001_300C as *mut u8;
const GPIOA_BSRR: *mut u32 = 0x5000_0018 as *mut u32;
const CSN_HIGH: u32 = 1 << crate::gpio::PIN_CSN;
const CSN_LOW: u32 = 1 << (crate::gpio::PIN_CSN + 16);

const R_RX_PL_WID: u8 = 0x60;
const R_RX_PAYLOAD: u8 = 0x61;
const FLUSH_RX: u8 = 0xE2;
const W_STATUS: u8 = 0x20 | 0x07;
const RX_DR: u8 = 0x40;
const NOP: u8 = 0xFF;

/// Where received packets go: unicast (pipe 2) and broadcast (pipe 1).
#[derive(Default)]
pub struct Rings {
    pub uni: RxRing<UNI_SLOTS>,
    pub bc: RxRing<BCAST_SLOTS>,
}

impl Rings {
    pub const fn new() -> Self {
        Self { uni: RxRing::new(), bc: RxRing::new() }
    }
}

/// Chip select: CSN low while `on`.
#[inline(always)]
fn select(on: bool) {
    // SAFETY: GPIOA_BSRR is a write-only set/reset register.
    unsafe { core::ptr::write_volatile(GPIOA_BSRR, if on { CSN_LOW } else { CSN_HIGH }) }
}

#[inline(never)]
#[unsafe(link_section = ".data.ramfunc.xfer")]
fn xfer(b: u8) -> u8 {
    // SAFETY: SPI1 SR/DR (8-bit access = one frame), as in `spi::xfer`.
    unsafe {
        while core::ptr::read_volatile(SPI1_SR) & 2 == 0 {}
        core::ptr::write_volatile(SPI1_DR8, b);
        while core::ptr::read_volatile(SPI1_SR) & 1 == 0 {}
        core::ptr::read_volatile(SPI1_DR8)
    }
}

#[inline(always)]
fn cmd(c: u8) -> u8 {
    select(true);
    let s = xfer(c);
    select(false);
    s
}

/// Pull every waiting packet out of the nRF RX FIFO into the rings
/// (dropped, but still read out, when the matching ring is full).
#[inline(never)]
#[unsafe(link_section = ".data.ramfunc.drain")]
pub fn drain(rings: &mut Rings) {
    loop {
        let status = cmd(NOP);
        let pipe = (status >> 1) & 7;
        if pipe == 7 {
            return; // RX FIFO empty
        }
        select(true);
        xfer(R_RX_PL_WID);
        let w = xfer(NOP);
        select(false);
        if w == 0 || w > 32 {
            cmd(FLUSH_RX); // corrupt width: the datasheet says flush
        } else {
            let dst: *mut u8 = if pipe == PIPE_UNICAST && !rings.uni.is_full() {
                rings.uni.slot().as_mut_ptr()
            } else if pipe == PIPE_BROADCAST && !rings.bc.is_full() {
                rings.bc.slot().as_mut_ptr()
            } else {
                core::ptr::null_mut()
            };
            select(true);
            xfer(R_RX_PAYLOAD);
            let mut i = 0u8;
            while i < w {
                let b = xfer(NOP);
                if !dst.is_null() {
                    // SAFETY: i < w <= 32 = slot size.
                    unsafe { core::ptr::write_volatile(dst.add(i as usize), b) };
                }
                i += 1;
            }
            select(false);
            if dst.is_null() {
                if pipe == PIPE_UNICAST {
                    rings.uni.dropped = rings.uni.dropped.wrapping_add(1);
                } else {
                    rings.bc.dropped = rings.bc.dropped.wrapping_add(1);
                }
            } else if pipe == PIPE_UNICAST {
                rings.uni.commit(w);
            } else {
                rings.bc.commit(w);
            }
        }
        select(true);
        xfer(W_STATUS);
        xfer(RX_DR);
        select(false);
    }
}
