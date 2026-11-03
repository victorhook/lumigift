//! Low-power doze (`lumigift_proto::doze`): SYSCLK on the 32 kHz LSI with
//! HSI48 off (the MCU draws ~80 µA, like Stop mode, but GPIOs and timers
//! keep working without a wake-up/clock-restore dance), the nRF in standby-I
//! (26 µA) except for a short receive window every period. A window is
//! nothing but GPIO: CE high, wait, CE low, look at the IRQ pin.
//!
//! Interrupts stay masked throughout. WFI still wakes on an interrupt that is
//! pending and enabled in the NVIC, so only TIM16 (the window timer) is
//! enabled meanwhile; the app's TIM14 and radio EXTI interrupts are masked in
//! the NVIC and run after the doze if their flags got set.

use cortex_m::peripheral::NVIC;

use crate::pac::rcc::vals::Sw;
use crate::pac::timer::regs::{ArrCore, CntCore, Sr1chCmp};
use crate::pac::{Interrupt, RCC, TIM16};
use crate::radio::Radio;
use crate::{gpio, iwdg, time};

const LSI_HZ: u32 = 32_000;

/// Doze until a packet is waiting in the nRF RX FIFO after a listen window.
/// The nRF must be running as PRX (CE is toggled, nothing else). Returns the
/// time spent, in µs (nominal: the LSI is only ±10 % accurate); the µs clock
/// is advanced by it.
pub fn doze_until_rx(r: &mut Radio, period_us: u32, listen_us: u32) -> u32 {
    cortex_m::interrupt::free(|_| {
        let start = time::now_us();
        let _ = r.ce(false); // standby-I
        let masked = [Interrupt::TIM14, Interrupt::EXTI4_15];
        let was_enabled = masked.map(NVIC::is_enabled);
        for i in masked {
            NVIC::mask(i);
        }
        RCC.apbenr2().modify(|w| w.set_tim16en(true));
        TIM16.dier().write(|w| w.set_uie(true));
        // SAFETY: interrupts are masked (PRIMASK): this only lets TIM16 end WFI.
        unsafe { NVIC::unmask(Interrupt::TIM16) };
        to_lsi();

        let (sleep, listen) = (ticks(period_us.saturating_sub(listen_us)), ticks(listen_us));
        let mut elapsed = 0u32;
        loop {
            iwdg::feed();
            sleep_ticks(sleep);
            let _ = r.ce(true);
            sleep_ticks(listen);
            let _ = r.ce(false);
            elapsed = elapsed.wrapping_add(period_us);
            if gpio::radio_irq() {
                break;
            }
        }

        to_hsi48();
        NVIC::mask(Interrupt::TIM16);
        NVIC::unpend(Interrupt::TIM16);
        TIM16.cr1().write(|_| {});
        RCC.apbenr2().modify(|w| w.set_tim16en(false));
        time::resume(start.wrapping_add(elapsed));
        for (i, on) in masked.into_iter().zip(was_enabled) {
            if on {
                // SAFETY: restoring the app's own interrupt configuration.
                unsafe { NVIC::unmask(i) };
            }
        }
        // Back in RX for the app.
        let _ = r.ce(true);
        elapsed
    })
}

/// LSI ticks in `us` (≤ 2 s: 16-bit timer).
fn ticks(us: u32) -> u32 {
    (us / 125 * (LSI_HZ / 8000)).clamp(2, 0xFFFF)
}

/// Sleep `n` LSI ticks (one-shot TIM16 update, WFI).
fn sleep_ticks(n: u32) {
    TIM16.cr1().write(|w| w.set_opm(true));
    TIM16.arr().write_value(ArrCore(n - 1));
    TIM16.cnt().write_value(CntCore(0));
    TIM16.sr().write_value(Sr1chCmp(0));
    TIM16.cr1().modify(|w| w.set_cen(true));
    while !TIM16.sr().read().uif() {
        cortex_m::asm::wfi();
    }
    TIM16.sr().write_value(Sr1chCmp(0));
    NVIC::unpend(Interrupt::TIM16);
}

/// SYSCLK from the LSI (already on: the IWDG runs from it), HSI48 off.
fn to_lsi() {
    RCC.csr2().modify(|w| w.set_lsion(true));
    while !RCC.csr2().read().lsirdy() {}
    RCC.cfgr().modify(|w| w.set_sw(Sw::LSI));
    while RCC.cfgr().read().sws() != Sw::LSI {}
    RCC.cr().modify(|w| w.set_hsion(false));
}

/// Back to HSI48 (HSIDIV is still /1: 48 MHz).
fn to_hsi48() {
    RCC.cr().modify(|w| w.set_hsion(true));
    while !RCC.cr().read().hsirdy() {}
    RCC.cfgr().modify(|w| w.set_sw(Sw::HSISYS));
    while RCC.cfgr().read().sws() != Sw::HSISYS {}
}
