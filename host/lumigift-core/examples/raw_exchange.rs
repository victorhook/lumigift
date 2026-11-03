//! Talk to one gift by hand, below the link manager: send a unicast
//! command, then polls, and print every ack payload decoded.
//!
//! `cargo run -p lumigift-core --example raw_exchange -- <node addr hex> [retry_delay 0..=15] [channel]`

use std::time::Duration;

use lumigift_proto::{self as lp, Packet, Reply, Unicast};
use nrf24_usb::{Bridge, Command, Event, Transport};
use nrf24_usb_proto::{Payload, RadioConfig};

fn send(b: &mut Bridge, tag: u16, p: Packet) -> Result<(), nrf24_usb::Error> {
    let mut buf = [0u8; lp::MAX_PAYLOAD];
    let n = p.encode(&mut buf);
    b.send(&[Command::Tx { tag, slot: 0, flags: 0, ts_offset: 0, payload: Payload::new(&buf[..n]).unwrap() }])?;
    while let Some(ev) = b.recv(Duration::from_millis(100))? {
        if let Event::TxDone { status, retries, ack_payload, .. } = ev {
            let ack = ack_payload.as_slice();
            let decoded = if ack.is_empty() { "-".to_string() } else { format!("{:?}", Reply::decode(ack)) };
            println!("{p:?} -> {status:?} r{retries}  => {decoded}");
            return Ok(());
        }
    }
    println!("{p:?} -> no TxDone");
    Ok(())
}

fn main() -> Result<(), nrf24_usb::Error> {
    let a: Vec<String> = std::env::args().collect();
    let node = u8::from_str_radix(a.get(1).map(String::as_str).unwrap_or("60"), 16).unwrap();
    let retry_delay: u8 = a.get(2).and_then(|s| s.parse().ok()).unwrap_or(1);
    let channel: u8 = a.get(3).and_then(|s| s.parse().ok()).unwrap_or(lp::DEFAULT_CHANNEL);
    println!("retry delay {} µs", (retry_delay as u32 + 1) * 250);
    let mut b = Bridge::open(None)?;
    b.get_info(Duration::from_secs(1))?;
    b.send(&[Command::Configure(RadioConfig { channel, retry_delay, ..Default::default() }), Command::SetTxAddr { slot: 0, addr: lp::address(node) }])?;
    send(&mut b, 1, Packet::Unicast(Unicast::Poll { seq: 1 }))?;
    send(&mut b, 2, Packet::Unicast(Unicast::GetIdentity { seq: 2 }))?;
    for i in 0..4u16 {
        std::thread::sleep(Duration::from_millis(5));
        send(&mut b, 3 + i, Packet::Unicast(Unicast::Poll { seq: 3 + i as u8 }))?;
    }
    Ok(())
}
