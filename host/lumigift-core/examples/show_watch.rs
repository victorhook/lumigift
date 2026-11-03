//! Run a show on the real bridge and print each gift's broadcast reception,
//! clock sync and flags once a second (diagnostics for "Missing frames" /
//! "Clock drift").
//!
//! `cargo run -p lumigift-core --example show_watch -- [seconds] [music|layers|solid]`

use std::time::{Duration, Instant};

use lumigift_core::*;

fn main() {
    let secs: u64 = std::env::args().nth(1).and_then(|s| s.parse().ok()).unwrap_or(20);
    let link = Link::open_usb(LinkConfig::default(), None);
    link.wait_for(Duration::from_secs(15), |s| s.gifts.iter().filter(|g| g.online).count() >= 1)
        .expect("no gifts");
    std::thread::sleep(Duration::from_secs(3)); // let discovery finish
    let frame = MusicFrame { brightness: 60, effect: 8, slot_count: 3, ..Default::default() };
    let out = match std::env::args().nth(2).as_deref() {
        Some("layers") => ShowOutput::Layers(vec![
            MusicFrame { mask: 0b011, ..frame },
            MusicFrame { mask: 0b100, effect: 10, ..frame },
        ]),
        Some("solid") => ShowOutput::Solid(vec![Rgb::new(40, 10, 0); 3]),
        _ => ShowOutput::Music(frame),
    };
    println!("{out:?}");
    link.set_show(out);
    let t0 = Instant::now();
    while t0.elapsed() < Duration::from_secs(secs) {
        std::thread::sleep(Duration::from_secs(1));
        let s = link.snapshot();
        let mut line = format!("{:>3}s", t0.elapsed().as_secs());
        for g in s.gifts.iter().filter(|g| g.online) {
            line += &format!(
                " | {} bcast {:>5} sync {:>4}µs synced {} flags {:02x}",
                g.uid.short(),
                g.bcast_rx_pct.map(|p| format!("{p:.1}%")).unwrap_or("-".into()),
                g.sync_err_us,
                g.flags & lumigift_proto::status_flags::TIME_SYNCED != 0,
                g.flags
            );
        }
        println!("{line}");
    }
    link.set_show(ShowOutput::Idle);
    std::thread::sleep(Duration::from_millis(300));
}
