//! Fleet stability run on the real bridge: phases of idle and a bright look,
//! logging every gift once a second to a CSV and summarising resets, lost
//! sync, offline time and recovery per gift at the end.
//!
//! `cargo run --release -p lumigift-core --example stability -- <csv> [idle_s look_s idle_s]`

use std::collections::BTreeMap;
use std::io::Write;
use std::time::{Duration, Instant};

use lumigift_core::*;
use lumigift_proto::status_flags;

#[derive(Default)]
struct Tally {
    samples: u32,
    offline: u32,
    unsynced: u32,
    in_boot: u32,
    max_sync_err: u16,
    min_bcast: Option<f32>,
    min_vbat: u16,
    reboots0: u32,
    resets0: Option<u16>,
    last_resets: u16,
    last_reason: String,
}

fn main() {
    let a: Vec<String> = std::env::args().collect();
    let path = a.get(1).cloned().unwrap_or_else(|| "stability.csv".into());
    let secs = |i: usize, d: u64| a.get(i).and_then(|s| s.parse().ok()).unwrap_or(d);
    let phases = [("idle", secs(2, 120)), ("look", secs(3, 180)), ("idle", secs(4, 60))];

    let link = Link::open_usb(LinkConfig::default(), None);
    link.wait_for(Duration::from_secs(20), |s| s.gifts.iter().filter(|g| g.online).count() >= 1)
        .expect("no gifts");
    std::thread::sleep(Duration::from_secs(4)); // let discovery settle
    let s0 = link.snapshot();
    println!("{} gifts known, {} online", s0.gifts.len(), s0.gifts.iter().filter(|g| g.online).count());
    let mut tally: BTreeMap<Uid, Tally> = BTreeMap::new();
    for g in &s0.gifts {
        println!(
            "  {} slot {:?} {:>4} mV  resets {:>3}  last {:?}  fw {}{}",
            g.uid.short(),
            g.slot,
            g.vbat_mv,
            g.resets,
            g.reset_reason,
            g.fw_version,
            if g.in_bootloader { "  IN BOOTLOADER" } else { "" }
        );
        tally.insert(g.uid, Tally { reboots0: g.reboots_seen, min_vbat: u16::MAX, ..Default::default() });
    }

    let mut csv = std::fs::File::create(&path).unwrap();
    writeln!(csv, "t_s,phase,uid,online,in_boot,synced,sync_err_us,bcast_pct,vbat_mv,uptime_s,resets,reset_reason,reboots_seen").unwrap();
    let t0 = Instant::now();
    for (name, dur) in phases {
        let out = if name == "look" {
            // A bright, moving look on all four LEDs of every gift.
            ShowOutput::Music(MusicFrame {
                brightness: 255,
                effect: lumigift_fx::effect::RAINBOW,
                slot_count: 10,
                ..Default::default()
            })
        } else {
            ShowOutput::Idle
        };
        println!("-- {name} for {dur} s");
        link.set_show(out);
        let end = Instant::now() + Duration::from_secs(dur);
        while Instant::now() < end {
            std::thread::sleep(Duration::from_secs(1));
            let s = link.snapshot();
            let t = t0.elapsed().as_secs();
            for g in &s.gifts {
                let synced = g.flags & status_flags::TIME_SYNCED != 0;
                writeln!(
                    csv,
                    "{t},{name},{},{},{},{},{},{},{},{},{},{:?},{}",
                    g.uid.short(),
                    g.online as u8,
                    g.in_bootloader as u8,
                    synced as u8,
                    g.sync_err_us,
                    g.bcast_rx_pct.map(|p| format!("{p:.1}")).unwrap_or_default(),
                    g.vbat_mv,
                    g.uptime.as_secs(),
                    g.resets,
                    g.reset_reason,
                    g.reboots_seen
                )
                .unwrap();
                let e = tally.entry(g.uid).or_insert_with(|| Tally { min_vbat: u16::MAX, ..Default::default() });
                e.samples += 1;
                if !g.online {
                    e.offline += 1;
                    continue;
                }
                e.in_boot += g.in_bootloader as u32;
                e.unsynced += (!synced && !g.in_bootloader) as u32;
                e.max_sync_err = e.max_sync_err.max(g.sync_err_us);
                if name == "look" {
                    if let Some(p) = g.bcast_rx_pct {
                        e.min_bcast = Some(e.min_bcast.map_or(p, |m: f32| m.min(p)));
                    }
                }
                if g.vbat_mv > 0 {
                    e.min_vbat = e.min_vbat.min(g.vbat_mv);
                }
                e.resets0.get_or_insert(g.resets);
                e.last_resets = g.resets;
                e.last_reason = format!("{:?}", g.reset_reason);
            }
        }
    }
    link.set_show(ShowOutput::Idle);
    let s = link.snapshot();
    println!("\nGIFT      samples offline unsynced in_boot max_sync_us min_bcast% min_mV reboots(host) resets(gift) last_reason");
    for (uid, e) in &tally {
        let g = s.gift(uid);
        println!(
            "{:<9} {:>7} {:>7} {:>8} {:>7} {:>11} {:>10} {:>6} {:>13} {:>12} {}",
            uid.short(),
            e.samples,
            e.offline,
            e.unsynced,
            e.in_boot,
            e.max_sync_err,
            e.min_bcast.map(|p| format!("{p:.1}")).unwrap_or("-".into()),
            if e.min_vbat == u16::MAX { 0 } else { e.min_vbat },
            g.map_or(0, |g| g.reboots_seen) - e.reboots0,
            e.last_resets as i32 - e.resets0.unwrap_or(e.last_resets) as i32,
            e.last_reason
        );
    }
    println!("\nlink: {:?}", s.link);
    println!("csv: {path}");
}
