//! USB DFU bootloader for the nrf24-usb bridge (STM32F103C8).
//!
//! Lives in the first 16 KiB of flash and never writes there, so a failed or
//! interrupted update can only break the app, and the bootloader can always
//! install a new one. It stays in DFU mode when:
//!
//! * the app asked for it (`DFU_DETACH` on the app's DFU runtime interface
//!   writes `dfu::BKP_REQUEST` to backup register DR1, then resets);
//! * there is no valid app (empty, or an update was interrupted: the app's
//!   first page, with its vector table, is only written at the very end);
//! * the reset button is pressed twice within half a second.
//!
//! Otherwise it jumps to the app right away (on power-up without delay; after
//! a single reset-button press after the 0.5 s double-press window).
//!
//! In DFU mode it enumerates as `1209:0002` with the app's serial number and
//! speaks DFU 1.1 with ST's DfuSe extensions:
//!
//! ```text
//! dfu-util -d 1209:0002 -a 0 -s 0x08004000:leave -D nrf24-usb-bridge.bin
//! ```
//!
//! LED: double blink = waiting in DFU mode, fast blink = no valid app.
#![no_std]
#![no_main]

mod flash;
mod hw;
mod layout;
mod usb;

use core::panic::PanicInfo;

use cortex_m_rt::entry;
use nrf24_usb_proto::dfu;

use crate::hw::ResetCause;

/// DR1 value during the double-press window.
const DOUBLE_PRESS: u16 = 0xD0B1;

#[entry]
fn main() -> ! {
    let cause = hw::take_reset_cause();
    let request = hw::backup();
    if request != 0 {
        hw::set_backup(0);
    }
    let app_ok = layout::app_valid();
    let stay = !app_ok || request == dfu::BKP_REQUEST || (cause == ResetCause::Button && request == DOUBLE_PRESS);
    if !stay {
        if cause == ResetCause::Button {
            // A second press during this window lands in DFU mode.
            hw::set_backup(DOUBLE_PRESS);
            hw::led_init();
            hw::led(true);
            cortex_m::asm::delay(hw::RESET_HZ / 2);
            hw::led(false);
            hw::set_backup(0);
        }
        hw::jump_to_app();
    }
    usb::run(app_ok)
}

#[panic_handler]
fn panic(_: &PanicInfo) -> ! {
    cortex_m::peripheral::SCB::sys_reset()
}
