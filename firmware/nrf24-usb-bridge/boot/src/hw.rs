//! Board bits the bootloader needs: clocks, LED, reset cause, backup
//! register, unique ID and the jump to the app. Register-level, no HAL.

use stm32_metapac::flash::vals::Latency;
use stm32_metapac::gpio::vals::{CnfIn, CnfOut, Mode};
use stm32_metapac::rcc::vals::{Pllmul, Pllsrc, Pllxtpre, Ppre, Sw, Usbpre};
use stm32_metapac::{BKP, FLASH, GPIOA, GPIOC, PWR, RCC};

use crate::layout::APP_BASE;

/// SYSCLK after reset (HSI).
pub const RESET_HZ: u32 = 8_000_000;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum ResetCause {
    /// Only the NRST pin: someone pressed the reset button.
    Button,
    /// Power-on, software (the app or a manifest), watchdog, ...
    Other,
}

/// Read and clear the RCC reset flags.
pub fn take_reset_cause() -> ResetCause {
    let csr = RCC.csr().read();
    RCC.csr().modify(|w| w.set_rmvf(true));
    // A software reset also pulls NRST low, so PINRSTF is set then too.
    let other = csr.porrstf() || csr.sftrstf() || csr.iwdgrstf() || csr.wwdgrstf() || csr.lpwrrstf();
    if csr.pinrstf() && !other { ResetCause::Button } else { ResetCause::Other }
}

/// Backup register DR1: survives resets, cleared on power loss.
pub fn backup() -> u16 {
    RCC.apb1enr().modify(|w| {
        w.set_pwren(true);
        w.set_bkpen(true);
    });
    BKP.dr(0).read().d()
}

pub fn set_backup(v: u16) {
    PWR.cr().modify(|w| w.set_dbp(true));
    BKP.dr(0).write(|w| w.set_d(v));
    PWR.cr().modify(|w| w.set_dbp(false));
}

/// PC13 user LED, active low.
pub fn led_init() {
    RCC.apb2enr().modify(|w| w.set_gpiocen(true));
    GPIOC.bsrr().write(|w| w.set_bs(13, true));
    GPIOC.cr(1).modify(|w| {
        w.set_mode(13 - 8, Mode::OUTPUT2MHZ);
        w.set_cnf_out(13 - 8, CnfOut::PUSH_PULL);
    });
}

pub fn led(on: bool) {
    GPIOC.bsrr().write(|w| if on { w.set_br(13, true) } else { w.set_bs(13, true) });
}

/// 72 MHz from the 8 MHz crystal with USB at 72 / 1.5 = 48 MHz. Without a
/// working crystal, 48 MHz from HSI / 2 * 12 (USB then runs off the ±1 %
/// HSI: out of spec but usually enumerates, and better than no bootloader).
/// Returns SYSCLK in Hz.
pub fn clocks() -> u32 {
    RCC.cr().modify(|w| w.set_hseon(true));
    let mut spins = 0u32;
    while !RCC.cr().read().hserdy() && spins < 200_000 {
        spins += 1;
    }
    let hse = RCC.cr().read().hserdy();
    if !hse {
        RCC.cr().modify(|w| w.set_hseon(false));
    }
    FLASH.acr().modify(|w| {
        w.set_latency(Latency::WS2);
        w.set_prftbe(true);
    });
    RCC.cfgr().modify(|w| {
        w.set_ppre1(Ppre::DIV2); // APB1 <= 36 MHz
        if hse {
            w.set_pllsrc(Pllsrc::HSE_DIV_PREDIV);
            w.set_pllxtpre(Pllxtpre::DIV1);
            w.set_pllmul(Pllmul::MUL9);
            w.set_usbpre(Usbpre::DIV1_5);
        } else {
            w.set_pllsrc(Pllsrc::HSI_DIV2);
            w.set_pllmul(Pllmul::MUL12);
            w.set_usbpre(Usbpre::DIV1);
        }
    });
    RCC.cr().modify(|w| w.set_pllon(true));
    while !RCC.cr().read().pllrdy() {}
    RCC.cfgr().modify(|w| w.set_sw(Sw::PLL1_P));
    while RCC.cfgr().read().sws() != Sw::PLL1_P {}
    if hse { 72_000_000 } else { 48_000_000 }
}

/// D+ has a fixed 1.5 kΩ pull-up: hold it low for 10 ms so the host sees a
/// disconnect and enumerates us again (the app does the same).
pub fn usb_reenumerate(sysclk: u32) {
    RCC.apb2enr().modify(|w| w.set_gpioaen(true));
    GPIOA.bsrr().write(|w| w.set_br(12, true));
    GPIOA.cr(1).modify(|w| {
        w.set_mode(12 - 8, Mode::OUTPUT2MHZ);
        w.set_cnf_out(12 - 8, CnfOut::PUSH_PULL);
    });
    cortex_m::asm::delay(sysclk / 100);
    GPIOA.cr(1).modify(|w| {
        w.set_mode(12 - 8, Mode::INPUT);
        w.set_cnf_in(12 - 8, CnfIn::FLOATING);
    });
}

/// The 96-bit unique ID as 24 upper-case hex digits, the same string the
/// app uses as its USB serial number.
pub fn serial() -> &'static str {
    static mut BUF: [u8; 24] = [0; 24];
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    // SAFETY: called once, before the USB stack exists; nothing else touches BUF.
    let buf = unsafe { &mut *core::ptr::addr_of_mut!(BUF) };
    for i in 0..12 {
        // SAFETY: the UID lives in system memory at 0x1FFF_F7E8.
        let b = unsafe { core::ptr::read_volatile((0x1FFF_F7E8 + i) as *const u8) };
        buf[2 * i] = HEX[(b >> 4) as usize];
        buf[2 * i + 1] = HEX[(b & 15) as usize];
    }
    // SAFETY: ASCII hex digits.
    unsafe { core::str::from_utf8_unchecked(buf) }
}

/// Start the app as if it came straight out of reset: only the LED port and
/// the backup-domain clocks were touched (no PLL, no USB, no interrupts).
pub fn jump_to_app() -> ! {
    GPIOC.cr(1).write_value(stm32_metapac::gpio::regs::Cr(0x4444_4444));
    RCC.apb1enr().write_value(stm32_metapac::rcc::regs::Apb1enr(0));
    RCC.apb2enr().write_value(stm32_metapac::rcc::regs::Apb2enr(0));
    // SAFETY: single-threaded hand-over; nothing of this image runs afterwards.
    unsafe {
        let p = cortex_m::Peripherals::steal();
        p.SCB.vtor.write(APP_BASE);
        cortex_m::asm::dsb();
        cortex_m::asm::isb();
        cortex_m::asm::bootload(APP_BASE as *const u32)
    }
}
