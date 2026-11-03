//! Bench test of a bridge's radio wiring, with no other node needed.
//!
//! * NO_ACK burst: every packet must come back `Sent`; the rate shows that the
//!   IRQ line works (without it the bridge falls back to polling, so each
//!   packet takes milliseconds instead of a few hundred µs).
//! * Acked packets to an address nobody listens on: must end in `MaxRetries`
//!   after exactly the configured retries (exercises CE, auto-retransmit, IRQ).
//! * Channel scan: received-power hits per channel (Wi-Fi shows up on 1..=60).
//!
//! `cargo run -p nrf24-usb --example radio_selftest [serial]`

use std::time::{Duration, Instant};

use nrf24_usb::{Bridge, Command, Event, Transport};
use nrf24_usb_proto::{Payload, RadioConfig, TxStatus, tx_flags};

fn main() -> Result<(), nrf24_usb::Error> {
    let serial = std::env::args().nth(1);
    let mut b = Bridge::open(serial.as_deref())?;
    let info = b.get_info(Duration::from_secs(1))?;
    println!("radio present: {}, tx queue depth {}", info.radio_present, info.tx_queue_depth);
    let cfg = RadioConfig::default();
    b.send(&[
        Command::Configure(cfg),
        Command::SetTxAddr { slot: 0, addr: *b"TEST1" },
    ])?;

    // NO_ACK burst, keeping at most the queue depth in flight.
    let n = 2000u16;
    let payload = Payload::new(&[0x55; 32]).unwrap();
    let (mut sent, mut done, mut ok, mut other) = (0u16, 0u16, 0u32, Vec::new());
    let (mut first_us, mut last_us) = (None, 0u32);
    let t0 = Instant::now();
    while done < n && t0.elapsed() < Duration::from_secs(20) {
        let room = (info.tx_queue_depth as u16).saturating_sub(sent - done).min(n - sent);
        let cmds: Vec<_> = (0..room)
            .map(|i| Command::Tx { tag: sent + i, slot: 0, flags: tx_flags::NO_ACK, ts_offset: 0, payload })
            .collect();
        if !cmds.is_empty() {
            b.send(&cmds)?;
            sent += room;
        }
        while let Some(ev) = b.recv(Duration::from_millis(50))? {
            if let Event::TxDone { status, time_us, .. } = ev {
                done += 1;
                first_us.get_or_insert(time_us);
                last_us = time_us;
                if status == TxStatus::Sent { ok += 1 } else { other.push(status) }
            }
            if sent - done < info.tx_queue_depth as u16 / 2 {
                break;
            }
        }
    }
    let span_us = last_us.wrapping_sub(first_us.unwrap_or(last_us)).max(1);
    println!(
        "NO_ACK burst: {ok}/{n} Sent, {} other {:?}; {:.0} packets/s on air ({:.0} µs each)",
        other.len(),
        other.iter().take(5).collect::<Vec<_>>(),
        (done.saturating_sub(1)) as f64 * 1e6 / span_us as f64,
        span_us as f64 / done.saturating_sub(1).max(1) as f64
    );

    // Acked packets to nobody: expect MaxRetries after `retries` retransmits.
    let ack: Vec<_> = (0..5u16)
        .map(|i| Command::Tx { tag: 10_000 + i, slot: 0, flags: 0, ts_offset: 0, payload })
        .collect();
    b.send(&ack)?;
    let mut results = Vec::new();
    let t1 = Instant::now();
    while results.len() < 5 && t1.elapsed() < Duration::from_secs(2) {
        if let Some(Event::TxDone { status, retries, .. }) = b.recv(Duration::from_millis(100))? {
            results.push((status, retries));
        }
    }
    println!("acked to nobody: {results:?} (expect MaxRetries with retries = {})", cfg.retries);

    // Scan.
    b.send(&[Command::Scan { first: 0, last: 125, dwell_us: 300, repeats: 20 }])?;
    let mut hits = vec![0u16; 126];
    let t2 = Instant::now();
    let mut got = 0;
    while got < 126 && t2.elapsed() < Duration::from_secs(5) {
        if let Some(Event::ScanResult { channel, hits: h, .. }) = b.recv(Duration::from_millis(200))? {
            hits[channel as usize] = h;
            got += 1;
        }
    }
    let band = |r: std::ops::Range<usize>| r.clone().map(|c| hits[c] as u32).sum::<u32>() as f64 / r.len() as f64;
    println!(
        "scan ({got}/126 channels): mean hits 2400-2460 MHz {:.1}, 2461-2525 MHz {:.1}",
        band(0..61),
        band(61..126)
    );
    let line: String = hits
        .iter()
        .map(|&h| match h { 0 => '.', 1..=2 => ':', 3..=6 => '+', _ => '#' })
        .collect();
    println!("  {line}");

    b.send(&[Command::GetStats])?;
    while let Some(ev) = b.recv(Duration::from_millis(200))? {
        if let Event::Stats(s) = ev {
            println!("{s:?}");
        }
    }
    Ok(())
}
