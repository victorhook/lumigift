//! Independent watchdog: LSI (~32 kHz) / 64, reload 1000 -> ~2 s.
//! Once started it cannot be stopped; it keeps running across the jump from
//! the bootloader to the app, which keeps feeding it.

use crate::pac::{DBGMCU, IWDG, RCC};

const KEY_START: u32 = 0xCCCC;
const KEY_UNLOCK: u32 = 0x5555;
const KEY_FEED: u32 = 0xAAAA;
const PR_DIV64: u32 = 4;
const RELOAD: u32 = 1000;

fn reg(off: usize) -> *mut u32 {
    (IWDG.as_ptr() as usize + off) as *mut u32
}

pub fn start() {
    // Freeze the watchdog while a debugger halts the core (SWD bring-up).
    RCC.apbenr1().modify(|w| w.set_dbgen(true));
    DBGMCU.apb1fzr().modify(|w| w.set_iwdg(true));
    // SAFETY: IWDG KR/PR/RLR/SR at offsets 0/4/8/C (RM0490 §21.7).
    unsafe {
        core::ptr::write_volatile(reg(0x0), KEY_START);
        core::ptr::write_volatile(reg(0x0), KEY_UNLOCK);
        core::ptr::write_volatile(reg(0x4), PR_DIV64);
        core::ptr::write_volatile(reg(0x8), RELOAD - 1);
        while core::ptr::read_volatile(reg(0xC)) & 0x7 != 0 {}
        core::ptr::write_volatile(reg(0x0), KEY_FEED);
    }
}

#[inline]
pub fn feed() {
    // SAFETY: writing the reload key.
    unsafe { core::ptr::write_volatile(reg(0x0), KEY_FEED) };
}
