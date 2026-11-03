//! Send raw packets: `raw_tx <addr hex, 5 bytes LSB first> <count> [ack] [channel]`.
//! Prints how each one ended (Sent / Acked / MaxRetries).

use std::time::Duration;

use nrf24_usb::{Bridge, Command, Event, Transport};
use nrf24_usb_proto::{Payload, RadioConfig, tx_flags};

fn main() -> Result<(), nrf24_usb::Error> {
    let a: Vec<String> = std::env::args().collect();
    let hex = a.get(1).map(String::as_str).unwrap_or("004C474654");
    let addr: Vec<u8> = (0..5).map(|i| u8::from_str_radix(&hex[2 * i..2 * i + 2], 16).unwrap()).collect();
    let count: u16 = a.get(2).and_then(|s| s.parse().ok()).unwrap_or(10);
    let ack = a.get(3).is_some_and(|s| s == "ack");
    let channel: u8 = a.get(4).and_then(|s| s.parse().ok()).unwrap_or(76);
    let mut b = Bridge::open(None)?;
    b.get_info(Duration::from_secs(1))?;
    let cfg = RadioConfig { channel, ..Default::default() };
    b.send(&[Command::Configure(cfg), Command::SetTxAddr { slot: 0, addr: addr.clone().try_into().unwrap() }])?;
    let payload = Payload::new(&[0xEE, 0, 1, 2, 3, 4, 5, 6]).unwrap();
    let mut tally = std::collections::BTreeMap::new();
    for tag in 0..count {
        let flags = if ack { 0 } else { tx_flags::NO_ACK };
        b.send(&[Command::Tx { tag, slot: 0, flags, ts_offset: 0, payload }])?;
        while let Some(ev) = b.recv(Duration::from_millis(50))? {
            if let Event::TxDone { status, retries, ack_payload: p, .. } = ev {
                *tally.entry(format!("{status:?} r{retries}{}", if p.as_slice().is_empty() { "" } else { " +ackpayload" })).or_insert(0) += 1;
                break;
            }
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    println!("{hex} ch{channel} ack={ack}: {tally:?}");
    Ok(())
}
