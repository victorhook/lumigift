//! Generic nRF24L01+ <-> USB bridge (STM32F103C8 "Blue Pill"-class board).
//!
//! Tasks:
//! * `usb::device_task` - USB stack (enumeration, control requests, MS OS 2.0).
//! * `usb::out_task`    - bulk OUT -> `CMDS` (raw packets, NAK when full).
//! * `radio::radio_task`- decodes and executes commands strictly in order.
//! * `usb::in_task`     - `EVENTS` -> bulk IN, many events per packet.
//! * `led_task`         - heartbeat + traffic flicker.
//! * `usb::detach_task` - `DFU_DETACH` -> reset into the USB bootloader.
//!
//! See README.md for wiring, protocol notes and the performance design.
#![no_std]
#![no_main]

mod board;
mod queues;
mod radio;
mod usb;

use core::sync::atomic::Ordering;

use embassy_executor::Spawner;
use embassy_stm32::gpio::Output;
use embassy_time::Timer;
use panic_halt as _;

use crate::board::Board;
use crate::queues::RADIO_ACTIVITY;
use crate::radio::{Bridge, SpiDev};

/// `major << 8 | minor`, also used as USB bcdDevice.
pub const FW_VERSION: u16 = {
    const fn parse(s: &str) -> u16 {
        let b = s.as_bytes();
        let mut i = 0;
        let mut v = 0u16;
        while i < b.len() {
            v = v * 10 + (b[i] - b'0') as u16;
            i += 1;
        }
        v
    }
    let major = parse(env!("CARGO_PKG_VERSION_MAJOR"));
    let minor = parse(env!("CARGO_PKG_VERSION_MINOR"));
    (major << 8) | minor
};

#[embassy_executor::main]
async fn main(spawner: Spawner) {
    let p = embassy_stm32::init(board::clocks());
    let b = Board::take(p).await;

    let uid = embassy_stm32::uid::uid();
    let serial = embassy_stm32::uid::uid_hex();

    let u = usb::build(b.usb, serial);
    spawner.spawn(usb::device_task(u.device).unwrap());
    spawner.spawn(usb::out_task(u.ep_out).unwrap());
    spawner.spawn(usb::in_task(u.ep_in).unwrap());
    spawner.spawn(usb::detach_task().unwrap());

    let bridge = Bridge::new(SpiDev { bus: b.spi, csn: b.csn }, b.ce, b.irq, uid);
    spawner.spawn(radio::radio_task(bridge).unwrap());

    spawner.spawn(led_task(b.led).unwrap());
}

/// PC13 LED (active low): 60 ms blip every second; while there is radio
/// traffic it toggles every 25 ms instead.
#[embassy_executor::task]
async fn led_task(mut led: Output<'static>) -> ! {
    let mut seen = RADIO_ACTIVITY.load(Ordering::Relaxed);
    let mut tick: u32 = 0;
    loop {
        Timer::after_millis(25).await;
        tick = tick.wrapping_add(1);
        let now = RADIO_ACTIVITY.load(Ordering::Relaxed);
        if now != seen {
            seen = now;
            led.toggle();
        } else {
            let phase = tick % 40; // 40 * 25 ms = 1 s
            if phase < 3 { led.set_low() } else { led.set_high() }
        }
    }
}
