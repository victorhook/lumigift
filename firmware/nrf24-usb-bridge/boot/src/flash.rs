//! Flash erase / program (RM0008 §3.3 "Embedded flash memory" and PM0075).
//! Every write is checked against the app region first, so a bad host
//! request can never touch the bootloader.

use stm32_metapac::FLASH;

use crate::layout::{PAGE, in_app};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    /// Outside the app region or misaligned.
    Address,
    /// Target not erased before programming.
    NotErased,
    /// The controller reported PGERR / WRPRTERR.
    Prog,
    /// Read back differs from what was written.
    Verify,
}

fn unlock() {
    if FLASH.cr().read().lock() {
        FLASH.keyr().write_value(0x4567_0123);
        FLASH.keyr().write_value(0xCDEF_89AB);
    }
}

fn lock() {
    FLASH.cr().modify(|w| w.set_lock(true));
}

/// Wait for the operation to finish and clear (and report) its flags.
fn finish() -> Result<(), Error> {
    while FLASH.sr().read().bsy() {}
    let sr = FLASH.sr().read();
    FLASH.sr().write(|w| {
        w.set_eop(true);
        w.set_pgerr(true);
        w.set_wrprterr(true);
    });
    if sr.pgerr() || sr.wrprterr() { Err(Error::Prog) } else { Ok(()) }
}

fn bytes(addr: u32, len: usize) -> &'static [u8] {
    // SAFETY: callers only pass ranges inside on-chip flash.
    unsafe { core::slice::from_raw_parts(addr as *const u8, len) }
}

pub fn erased(addr: u32, len: usize) -> bool {
    bytes(addr, len).iter().all(|&b| b == 0xFF)
}

pub fn erase_page(addr: u32) -> Result<(), Error> {
    if !addr.is_multiple_of(PAGE) || !in_app(addr, PAGE) {
        return Err(Error::Address);
    }
    unlock();
    FLASH.cr().modify(|w| w.set_per(true));
    FLASH.ar().write(|w| w.set_far(addr));
    FLASH.cr().modify(|w| w.set_strt(true));
    let r = finish();
    FLASH.cr().modify(|w| w.set_per(false));
    lock();
    r?;
    if erased(addr, PAGE as usize) { Ok(()) } else { Err(Error::Verify) }
}

/// Program `data` at `addr` (half-word aligned; an odd tail is padded with
/// 0xFF). The target must be erased.
pub fn program(addr: u32, data: &[u8]) -> Result<(), Error> {
    if !addr.is_multiple_of(2) || !in_app(addr, data.len().next_multiple_of(2) as u32) {
        return Err(Error::Address);
    }
    if !erased(addr, data.len()) {
        return Err(Error::NotErased);
    }
    unlock();
    FLASH.cr().modify(|w| w.set_pg(true));
    let mut r = Ok(());
    for (i, pair) in data.chunks(2).enumerate() {
        let hw = u16::from_le_bytes([pair[0], *pair.get(1).unwrap_or(&0xFF)]);
        // SAFETY: half-word aligned, inside the (erased) app region.
        unsafe { core::ptr::write_volatile((addr + 2 * i as u32) as *mut u16, hw) };
        r = finish();
        if r.is_err() {
            break;
        }
    }
    FLASH.cr().modify(|w| w.set_pg(false));
    lock();
    r?;
    if bytes(addr, data.len()) == data { Ok(()) } else { Err(Error::Verify) }
}
