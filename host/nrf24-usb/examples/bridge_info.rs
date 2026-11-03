//! Open the first bridge, print its info, ping it and show radio stats.
//!
//! `cargo run -p nrf24-usb --example bridge_info [serial]`

use std::time::{Duration, Instant};

use nrf24_usb::{Bridge, Command, Event, Transport};

fn main() -> Result<(), nrf24_usb::Error> {
    let serial = std::env::args().nth(1);
    let mut b = Bridge::open(serial.as_deref())?;
    let info = b.get_info(Duration::from_secs(1))?;
    println!("{info:#?}");

    let t0 = Instant::now();
    b.send(&[Command::Ping { seq: 7 }, Command::GetStats])?;
    while let Some(ev) = b.recv(Duration::from_millis(300))? {
        match ev {
            Event::Pong { seq, time_us } => {
                println!("pong {seq}: bridge clock {time_us} µs, round trip {:?}", t0.elapsed())
            }
            other => println!("{other:#?}"),
        }
    }
    Ok(())
}
