//! Free-running µs clock on TIM14 (16 bit, 1 MHz) extended in software.
//!
//! [`now_us`] folds a pending overflow into the high word itself, so it
//! works polled (bootloader: call at least every 65 ms) and from the
//! TIM14 update interrupt (app: [`enable_irq`] + call [`on_irq`]).

use core::sync::atomic::{AtomicU32, Ordering};

use crate::pac::timer::regs::{ArrCore, Sr1ch};
use crate::pac::timer::vals::Urs;
use crate::pac::{RCC, TIM14};

static OVERFLOWS: AtomicU32 = AtomicU32::new(0);

pub fn init() {
    RCC.apbenr2().modify(|w| w.set_tim14en(true));
    TIM14.cr1().write(|w| w.set_urs(Urs::COUNTER_ONLY));
    TIM14.psc().write_value((crate::clock::SYSCLK_HZ / 1_000_000 - 1) as u16);
    TIM14.arr().write_value(ArrCore(0xFFFF));
    TIM14.egr().write(|w| w.set_ug(true)); // load PSC
    TIM14.sr().write_value(Sr1ch(0));
    TIM14.cr1().modify(|w| w.set_cen(true));
}

/// Raw (overflow count, counter) snapshot, consistent across a wrap.
fn snapshot() -> (u32, u32) {
    cortex_m::interrupt::free(|_| {
        let mut hi = OVERFLOWS.load(Ordering::Relaxed);
        let mut lo = TIM14.cnt().read().cnt() as u32;
        if TIM14.sr().read().uif() {
            // Wrapped and not yet accounted: re-read after the wrap.
            TIM14.sr().write_value(Sr1ch(!1));
            hi = hi.wrapping_add(1);
            OVERFLOWS.store(hi, Ordering::Relaxed);
            lo = TIM14.cnt().read().cnt() as u32;
        }
        (hi, lo)
    })
}

/// µs since boot (wraps after 71.6 minutes).
pub fn now_us() -> u32 {
    let (hi, lo) = snapshot();
    (hi << 16) | lo
}

/// ms since boot, 32 bit (wraps after 49.7 days).
pub fn now_ms() -> u32 {
    let (hi, lo) = snapshot();
    lumigift_logic::clock::ticks_to_ms(hi, lo)
}

pub fn delay_us(us: u32) {
    let t0 = now_us();
    while now_us().wrapping_sub(t0) < us {}
}

/// Enable the TIM14 update interrupt (app).
pub fn enable_irq() {
    TIM14.dier().modify(|w| w.set_uie(true));
    // SAFETY: the handler only touches the atomics above.
    unsafe { cortex_m::peripheral::NVIC::unmask(crate::pac::Interrupt::TIM14) };
}

/// Call from the TIM14 interrupt handler.
pub fn on_irq() {
    let _ = snapshot();
}
