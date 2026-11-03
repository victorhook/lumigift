//! Send raw packets and tally how each ended (Sent / Acked / MaxRetries):
//!
//! `raw_tx <addr hex, 5 bytes LSB first> <count> [ack|noack|burst] [channel] [min|low|high|max] [payload hex]`
//!
//! `burst` sends NO_ACK packets back to back (as fast as the bridge's queue
//! allows) instead of one at a time.

use std::time::{Duration, Instant};

use nrf24_usb::{Bridge, Command, Event, Transport};
use nrf24_usb_proto::{Payload, Power, RadioConfig, tx_flags};

fn hex(s: &str) -> Vec<u8> {
    (0..s.len() / 2).map(|i| u8::from_str_radix(&s[2 * i..2 * i + 2], 16).unwrap()).collect()
}

fn main() -> Result<(), nrf24_usb::Error> {
    let a: Vec<String> = std::env::args().collect();
    let arg = |i: usize| a.get(i).map(String::as_str);
    let addr: [u8; 5] = hex(arg(1).unwrap_or("004C474654")).try_into().unwrap();
    let count: u16 = arg(2).and_then(|s| s.parse().ok()).unwrap_or(10);
    let mode = arg(3).unwrap_or("noack");
    let channel: u8 = arg(4).and_then(|s| s.parse().ok()).unwrap_or(76);
    let power = match arg(5) {
        Some("min") => Power::Min,
        Some("low") => Power::Low,
        Some("high") => Power::High,
        _ => Power::Max,
    };
    let payload = Payload::new(&hex(arg(6).unwrap_or("EE00010203040506"))).unwrap();

    let mut b = Bridge::open(None)?;
    let info = b.get_info(Duration::from_secs(1))?;
    b.send(&[
        Command::Configure(RadioConfig { channel, power, ..Default::default() }),
        Command::SetTxAddr { slot: 0, addr },
    ])?;
    let flags = if mode == "ack" { 0 } else { tx_flags::NO_ACK };
    let tx = |tag| Command::Tx { tag, slot: 0, flags, ts_offset: 0, payload };
    let mut tally = std::collections::BTreeMap::new();
    let t0 = Instant::now();
    if mode == "burst" {
        let depth = info.tx_queue_depth as u16;
        let (mut sent, mut done) = (0u16, 0u16);
        while done < count {
            let room = depth.saturating_sub(sent - done).min(count - sent);
            if room > 0 {
                b.send(&(sent..sent + room).map(tx).collect::<Vec<_>>())?;
                sent += room;
            }
            while let Some(Event::TxDone { status, .. }) = b.recv(Duration::from_millis(100))? {
                done += 1;
                *tally.entry(format!("{status:?}")).or_insert(0) += 1;
                if sent - done < depth / 2 {
                    break;
                }
            }
        }
    } else {
        for tag in 0..count {
            b.send(&[tx(tag)])?;
            while let Some(ev) = b.recv(Duration::from_millis(50))? {
                if let Event::TxDone { status, retries, ack_payload: p, .. } = ev {
                    let ack = if p.as_slice().is_empty() { "" } else { " +ackpayload" };
                    *tally.entry(format!("{status:?} r{retries}{ack}")).or_insert(0) += 1;
                    break;
                }
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }
    println!("{} ch{channel} {mode} {power:?}: {tally:?} in {:?}", arg(1).unwrap_or("004C474654"), t0.elapsed());
    Ok(())
}
