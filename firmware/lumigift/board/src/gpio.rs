//! Pin setup and the few GPIO operations both images need.

use crate::pac::gpio::Gpio;
use crate::pac::gpio::vals::{Moder, Odr, Ospeedr, Ot, Pupdr};
use crate::pac::{GPIOA, GPIOB, RCC, SYSCFG};

pub const PIN_WS2812: usize = 2; // PA2
pub const PIN_IRQ: usize = 4; // PA4
pub const PIN_SCK: usize = 5; // PA5
pub const PIN_MISO: usize = 6; // PA6
pub const PIN_MOSI: usize = 7; // PA7
pub const PIN_CE: usize = 8; // PA8
pub const PIN_CSN: usize = 11; // PA11
pub const PIN_LED: usize = 6; // PB6

pub fn output(port: Gpio, pin: usize, high: bool) {
    port.odr().modify(|w| w.set_odr(pin, if high { Odr::HIGH } else { Odr::LOW }));
    port.otyper().modify(|w| w.set_ot(pin, Ot::PUSH_PULL));
    port.ospeedr().modify(|w| w.set_ospeedr(pin, Ospeedr::HIGH_SPEED));
    port.moder().modify(|w| w.set_moder(pin, Moder::OUTPUT));
}

pub fn alternate(port: Gpio, pin: usize, af: u8, pull: Pupdr) {
    port.afr(pin / 8).modify(|w| w.set_afr(pin % 8, af));
    port.otyper().modify(|w| w.set_ot(pin, Ot::PUSH_PULL));
    port.ospeedr().modify(|w| w.set_ospeedr(pin, Ospeedr::VERY_HIGH_SPEED));
    port.pupdr().modify(|w| w.set_pupdr(pin, pull));
    port.moder().modify(|w| w.set_moder(pin, Moder::ALTERNATE));
}

#[inline(always)]
pub fn set(port: Gpio, pin: usize, high: bool) {
    port.bsrr().write(|w| if high { w.set_bs(pin, true) } else { w.set_br(pin, true) });
}

pub fn init() {
    RCC.gpioenr().modify(|w| {
        w.set_gpioaen(true);
        w.set_gpioben(true);
    });
    // The TSSOP20 pad "PA11 [PA9]" is PA11 while PA11_RMP = 0 (its reset
    // value); make it explicit in case anything left it set.
    RCC.apbenr2().modify(|w| w.set_syscfgen(true));
    SYSCFG.cfgr1().modify(|w| w.set_pa11_rmp(false));

    output(GPIOB, PIN_LED, false);
    output(GPIOA, PIN_CE, false);
    output(GPIOA, PIN_CSN, true);
    // nRF IRQ: open-drain, active low; the ioc wrongly had it as an output.
    GPIOA.pupdr().modify(|w| w.set_pupdr(PIN_IRQ, Pupdr::PULL_UP));
    GPIOA.moder().modify(|w| w.set_moder(PIN_IRQ, Moder::INPUT));
    alternate(GPIOA, PIN_SCK, 0, Pupdr::FLOATING);
    alternate(GPIOA, PIN_MISO, 0, Pupdr::PULL_UP);
    alternate(GPIOA, PIN_MOSI, 0, Pupdr::FLOATING);
}

/// Red status LED (PB6, active high).
#[inline]
pub fn led(on: bool) {
    set(GPIOB, PIN_LED, on);
}

/// True while the nRF asserts its IRQ (pin low).
#[inline]
pub fn radio_irq() -> bool {
    GPIOA.idr().read().0 & (1 << PIN_IRQ) == 0
}
