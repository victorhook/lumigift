//! SYSCLK = HCLK = PCLK = 48 MHz from HSI48 (RM0490 §6).
//!
//! After reset SYSCLK is HSISYS = HSI48 / 4 = 12 MHz. Above 24 MHz the
//! flash needs one wait state, which must be set before raising the clock.

use crate::pac::flash::vals::Latency;
use crate::pac::rcc::vals::{Hpre, Hsidiv, Ppre, Sw};
use crate::pac::{FLASH, RCC};

pub const SYSCLK_HZ: u32 = 48_000_000;

pub fn init() {
    FLASH.acr().modify(|w| {
        w.set_latency(Latency::WS1);
        w.set_prften(true);
        w.set_icen(true);
    });
    while FLASH.acr().read().latency() != Latency::WS1 {}

    RCC.cr().modify(|w| w.set_hsion(true));
    while !RCC.cr().read().hsirdy() {}
    RCC.cfgr().modify(|w| {
        w.set_sw(Sw::HSISYS);
        w.set_hpre(Hpre::DIV1);
        w.set_ppre(Ppre::DIV1);
    });
    while RCC.cfgr().read().sws() != Sw::HSISYS {}
    RCC.cr().modify(|w| w.set_hsidiv(Hsidiv::DIV1));
}
