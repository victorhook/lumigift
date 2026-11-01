//! Board support for lumigift-rev-a (STM32C011F6P6), shared by the
//! bootloader and the app. Bare `stm32-metapac` register access: small,
//! blocking and explicit.
//!
//! | Signal | Pin | Use |
//! |---|---|---|
//! | WS2812B data | PA2 | TIM1_CH3, AF5 (app only, see the app's `ws2812`) |
//! | VBAT | PA3 | ADC_IN3 (analog, reset state) |
//! | nRF IRQ | PA4 | input, pull-up, active low |
//! | nRF SCK/MISO/MOSI | PA5/PA6/PA7 | SPI1, AF0 |
//! | nRF CE | PA8 | output |
//! | nRF CSN | PA11 | output (pad "PA11 [PA9]", SYSCFG_CFGR1.PA11_RMP = 0) |
//! | Red LED | PB6 | output, active high |
#![no_std]

pub use stm32_metapac as pac;

pub mod clock;
pub mod config;
pub mod crc;
pub mod fastrx;
pub mod flash;
pub mod gpio;
pub mod iwdg;
pub mod radio;
pub mod spi;
pub mod sys;
pub mod time;

pub use lumigift_proto::layout::{BOOT_INFO_ADDR, BOOT_INFO_MAGIC};

/// Board revision checked against `ImageHeader::hw_rev`.
pub const HW_REV: u16 = 1;

/// Version of the installed bootloader (0 if the info block is missing).
pub fn boot_version() -> u16 {
    // SAFETY: plain flash reads inside the bootloader region.
    let w = unsafe { core::ptr::read_volatile(BOOT_INFO_ADDR as *const [u32; 2]) };
    if w[0] == BOOT_INFO_MAGIC { w[1] as u16 } else { 0 }
}

/// Clocks (48 MHz), GPIO, µs timer and SPI1. Call first.
pub fn init() {
    clock::init();
    gpio::init();
    time::init();
    spi::init();
}
