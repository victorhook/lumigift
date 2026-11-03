//! Link ⇄ SimTransport integration tests (real time; each takes a few seconds).

use std::collections::BTreeSet;
use std::time::{Duration, Instant};

use lumigift_core::sim::{SimConfig, SimGiftConfig, SimHandle, build_test_image};
use lumigift_core::*;
use lumigift_proto::{MusicFrame, Rgb, config_key};

fn cfg() -> LinkConfig {
    LinkConfig {
        metadata: MetadataLocation::Disabled,
        // Nothing dozes here; skip the 1.3 s Wake burst on connect.
        wake_on_connect: false,
        ..Default::default()
    }
}

fn start(sim: SimConfig) -> (Link, SimHandle) {
    Link::simulated(cfg(), sim)
}

/// All `n` gifts online, discovered, on unique addresses matching the sim.
fn all_discovered(s: &Snapshot, sim: &SimHandle, n: usize) -> bool {
    let online: Vec<_> = s
        .gifts
        .iter()
        .filter(|g| g.online && !g.address_conflict)
        .collect();
    if online.len() != n || !s.discovery.initial_sweep_done {
        return false;
    }
    let addrs: BTreeSet<u8> = online.iter().map(|g| g.node_addr).collect();
    if addrs.len() != n {
        return false;
    }
    let simg = sim.gifts();
    online.iter().all(|g| {
        simg.iter()
            .any(|x| x.uid == g.uid && x.node_addr == g.node_addr)
    })
}

fn wait_discovered(link: &Link, sim: &SimHandle, n: usize, secs: u64) -> std::sync::Arc<Snapshot> {
    link.wait_for(Duration::from_secs(secs), |s| all_discovered(s, sim, n))
        .unwrap_or_else(|| {
            let s = link.snapshot();
            for e in s.events.iter() {
                eprintln!("{:?} {}", e.level, e.message);
            }
            for g in &s.gifts {
                eprintln!(
                    "{} addr {} online {} conflict {}",
                    g.uid, g.node_addr, g.online, g.address_conflict
                );
            }
            panic!("not all {n} gifts discovered");
        })
}

#[test]
fn discovers_ten_gifts() {
    let (link, sim) = start(SimConfig::new(10));
    let t0 = Instant::now();
    let s = wait_discovered(&link, &sim, 10, 20);
    eprintln!("discovered 10 gifts in {:?}", t0.elapsed());
    assert!(s.bridge.state == BridgeState::Connected);
    assert!(s.bridge.clock_synced);
    for g in &s.gifts {
        assert_eq!(g.fw_version, 100);
        assert!(!g.in_bootloader);
    }
    // Identity UIDs match the simulator's.
    let sim_uids: BTreeSet<Uid> = sim.gifts().iter().map(|g| g.uid).collect();
    let uids: BTreeSet<Uid> = s.gifts.iter().map(|g| g.uid).collect();
    assert_eq!(sim_uids, uids);
}

#[test]
fn resolves_address_collisions() {
    let mut sc = SimConfig::with_seed(5, 77);
    // Three gifts share address 42 from the start.
    for g in &mut sc.gifts[..3] {
        g.node_addr = Some(42);
    }
    let (link, sim) = start(sc);
    let s = wait_discovered(&link, &sim, 5, 30);
    assert!(
        s.discovery.collisions_resolved >= 2,
        "resolved {}",
        s.discovery.collisions_resolved
    );
    assert!(s.events.iter().any(|e| e.message.contains("collision")));

    // A gift appearing later on an address that is already taken.
    let taken = s.gifts[0].node_addr;
    let mut newcomer = SimGiftConfig::new([0xAB; 12]);
    newcomer.node_addr = Some(taken);
    sim.add_gift(newcomer);
    wait_discovered(&link, &sim, 6, 40);
}

#[test]
fn polling_stats_and_reboot_detection() {
    let mut sc = SimConfig::new(10);
    sc.loss_pct = 0.0;
    sc.gifts[0].loss_pct = Some(15.0);
    let lossy = Uid(sc.gifts[0].uid);
    let (link, sim) = start(sc);
    wait_discovered(&link, &sim, 10, 20);
    link.set_show(ShowOutput::Music(MusicFrame {
        brightness: 255,
        effect: 1,
        bpm: 120,
        ..Default::default()
    }));
    std::thread::sleep(Duration::from_secs(6));
    let s = link.snapshot();
    eprintln!("link stats: {:?}", s.link);
    assert!(
        (90.0..=105.0).contains(&s.link.frames_per_s),
        "frames/s {}",
        s.link.frames_per_s
    );
    assert!(s.link.polls_per_s > 120.0, "polls/s {}", s.link.polls_per_s);
    assert!(
        s.link.show_dropped * 100 < s.link.show_packets.max(1),
        "too many dropped show frames"
    );
    let depth = s.bridge.info.unwrap().tx_queue_depth as usize;
    assert!(sim.stats().max_outstanding_tx <= depth.min(link.config().max_in_flight));
    for g in &s.gifts {
        eprintln!(
            "{} link {:.1}% retries {:.2} bcast {:?} lost {} vbat {} sync {} hist {}",
            g.uid.short(),
            g.poll_success_pct,
            g.avg_retries,
            g.bcast_rx_pct,
            g.bcast_lost,
            g.vbat_mv,
            g.sync_err_us,
            g.history.len()
        );
        assert!(g.online);
        assert!(g.vbat_mv > 1000);
        assert!(g.history.len() >= 8);
        assert!(g.uptime > Duration::from_secs(5));
        assert!(g.status.unwrap().flags & lumigift_proto::status_flags::TIME_SYNCED != 0);
        let bc = g.bcast_rx_pct.expect("broadcast reception known");
        if g.uid == lossy {
            assert!(g.avg_retries > 0.05);
            assert!((70.0..97.0).contains(&bc), "lossy bcast {bc}");
            assert!(g.bcast_lost > 0);
        } else {
            assert!(g.poll_success_pct > 99.0);
            assert!(g.avg_retries < 0.01);
            assert!(bc > 99.0, "clean bcast {bc}");
        }
    }
    // Music frames make the simulated LEDs light up.
    assert!(
        sim.gifts()
            .iter()
            .any(|g| g.leds.iter().any(|c| *c != Rgb::BLACK))
    );

    // Reboot detection.
    let victim = sim.gifts()[3].uid;
    sim.reboot(3);
    let s = link
        .wait_for(Duration::from_secs(3), |s| {
            s.gift(&victim).is_some_and(|g| g.reboots_seen == 1)
        })
        .expect("reboot detected");
    let g = s.gift(&victim).unwrap();
    assert_eq!(g.reset_reason, lumigift_proto::ResetReason::Watchdog);
    assert!(
        s.events
            .iter()
            .any(|e| e.gift == Some(victim) && e.message.contains("rebooted"))
    );
    assert!(
        link.wait_for(Duration::from_secs(3), |s| s
            .gift(&victim)
            .is_some_and(|g| g.online && !g.in_bootloader))
            .is_some()
    );
}

#[test]
fn offline_detection_and_return() {
    let (link, sim) = start(SimConfig::new(3));
    wait_discovered(&link, &sim, 3, 15);
    let uid = sim.gifts()[1].uid;
    sim.set_powered(1, false);
    let t0 = Instant::now();
    link.wait_for(Duration::from_secs(3), |s| {
        s.gift(&uid).is_some_and(|g| !g.online)
    })
    .expect("went offline");
    let dt = t0.elapsed();
    assert!(
        dt >= Duration::from_millis(900) && dt < Duration::from_millis(2000),
        "offline after {dt:?}"
    );
    assert!(
        link.snapshot()
            .events
            .iter()
            .any(|e| e.gift == Some(uid) && e.message.contains("offline"))
    );
    sim.set_powered(1, true);
    link.wait_for(Duration::from_secs(3), |s| {
        s.gift(&uid).is_some_and(|g| g.online)
    })
    .expect("came back");
}

#[test]
fn ota_ten_gifts_with_ten_percent_loss() {
    let mut sc = SimConfig::new(10);
    sc.loss_pct = 10.0;
    // One extra gift stuck in recovery (no valid image) is a valid target too.
    let mut rec = SimGiftConfig::new([0x42; 12]);
    rec.valid_image = false;
    rec.start_in_bootloader = true;
    sc.gifts.push(rec);
    let (link, sim) = start(sc);
    wait_discovered(&link, &sim, 11, 30);
    assert!(
        link.snapshot()
            .gift(&Uid([0x42; 12]))
            .unwrap()
            .in_bootloader
    );

    // Invalid images are refused before anything is sent.
    let mut bad = build_test_image(200, 20_000, 9);
    bad[300] ^= 0xFF;
    assert!(matches!(
        link.start_ota(bad, vec![]).wait(),
        Err(LinkError::InvalidImage(_))
    ));

    let image = build_test_image(200, 20_000, 9);
    let targets: Vec<Uid> = link.snapshot().gifts.iter().map(|g| g.uid).collect();
    let t0 = Instant::now();
    let mut p = link.start_ota(image.clone(), targets.clone());
    // A second update can't start while one runs.
    assert!(matches!(
        link.start_ota(image, targets).wait(),
        Err(LinkError::Busy(_))
    ));
    let report = p
        .wait_timeout(Duration::from_secs(90))
        .expect("OTA finished")
        .expect("OTA ok");
    eprintln!(
        "OTA: {:?}, {} chunks, {} rounds, failed {:?}",
        t0.elapsed(),
        report.chunks_sent,
        report.rounds,
        report.failed
    );
    assert_eq!(report.updated.len(), 11, "failed: {:?}", report.failed);
    for g in sim.gifts() {
        assert_eq!(g.image_fw_version, Some(200));
        assert!(!g.in_bootloader);
    }
    let s = link
        .wait_for(Duration::from_secs(1), |s| {
            !s.show_paused && s.ota.as_ref().unwrap().phase == OtaPhase::Done
        })
        .unwrap();
    let ota = s.ota.as_ref().unwrap();
    assert!(
        ota.targets
            .iter()
            .all(|t| t.phase == OtaTargetPhase::Done && t.percent == 100.0)
    );
    assert!(!s.show_paused);
    for g in &s.gifts {
        assert_eq!(g.fw_version, 200);
    }
}

#[test]
fn ota_cancel_leaves_link_usable() {
    let (link, sim) = start(SimConfig::new(2));
    wait_discovered(&link, &sim, 2, 15);
    let targets: Vec<Uid> = link.snapshot().gifts.iter().map(|g| g.uid).collect();
    let mut p = link.start_ota(build_test_image(300, 20_000, 3), targets);
    link.wait_for(Duration::from_secs(10), |s| {
        s.ota
            .as_ref()
            .is_some_and(|o| o.phase == OtaPhase::Streaming)
    })
    .unwrap();
    link.cancel_ota();
    assert_eq!(
        p.wait_timeout(Duration::from_secs(10))
            .unwrap()
            .unwrap_err(),
        LinkError::Cancelled
    );
    assert!(!link.ota_running());
    assert!(
        link.wait_for(Duration::from_secs(1), |s| !s.show_paused)
            .is_some()
    );
    // Gifts stay in the bootloader (recovery) and remain reachable.
    link.wait_for(Duration::from_secs(3), |s| {
        s.gifts.iter().all(|g| g.online && g.in_bootloader)
    })
    .expect("in bootloader");
}

#[test]
fn commands_slots_config_metadata_channel() {
    let dir = tempfile::tempdir().unwrap();
    let meta_path = dir.path().join("gifts.json");
    let lc = LinkConfig {
        metadata: MetadataLocation::Path(meta_path.clone()),
        wake_on_connect: false,
        ..Default::default()
    };
    let (link, sim) = Link::simulated(lc, SimConfig::new(4));
    let s = wait_discovered(&link, &sim, 4, 15);
    let uids: Vec<Uid> = {
        let mut v: Vec<Uid> = s.gifts.iter().map(|g| g.uid).collect();
        v.sort();
        v
    };

    // Blink everyone; LEDs show it.
    let r = link
        .blink(
            Target::All,
            Rgb::new(0, 0, 255),
            2,
            Duration::from_millis(200),
        )
        .wait()
        .unwrap();
    assert_eq!(r.ok.len(), 4, "{r}");
    std::thread::sleep(Duration::from_millis(50));
    assert!(
        sim.gifts()
            .iter()
            .all(|g| g.leds[0].b > 0 && g.leds[0].r == 0)
    );
    assert!(link.identify(uids[0]).wait().unwrap().all_ok());

    // Slots.
    let r = link.auto_assign_slots(SlotAssign::ByUid).wait().unwrap();
    assert!(r.all_ok(), "{r}");
    for (i, u) in uids.iter().enumerate() {
        let simg = &sim.gifts()[sim.index_of(u).unwrap()];
        assert_eq!(simg.slot, Some(i as u8));
    }
    link.set_slot(uids[0], Some(7)).wait().unwrap();
    link.set_slot(uids[1], None).wait().unwrap();
    assert!(link.set_slot(uids[1], Some(200)).wait().is_err());
    let r = link
        .auto_assign_slots(SlotAssign::KeepExisting)
        .wait()
        .unwrap();
    assert!(r.all_ok());
    let s = link
        .wait_for(Duration::from_secs(2), |s| {
            s.gift(&uids[0]).unwrap().slot == Some(7)
        })
        .unwrap();
    assert_eq!(
        s.gift(&uids[1]).unwrap().slot,
        Some(0),
        "gap filled with the lowest free slot"
    );

    // Solid colours per slot reach the right gifts.
    let mut colors = vec![Rgb::BLACK; 8];
    colors[7] = Rgb::new(200, 0, 0);
    colors[0] = Rgb::new(0, 200, 0);
    link.set_show(ShowOutput::Solid(colors));
    std::thread::sleep(Duration::from_millis(200));
    let g0 = &sim.gifts()[sim.index_of(&uids[0]).unwrap()];
    let g1 = &sim.gifts()[sim.index_of(&uids[1]).unwrap()];
    assert!(g0.leds[0].r > 0 && g0.leds[0].g == 0, "{:?}", g0.leds);
    assert!(g1.leds[0].g > 0 && g1.leds[0].r == 0, "{:?}", g1.leds);

    // Config: accepted and rejected values.
    assert!(
        link.set_config(Target::All, config_key::MAX_BRIGHTNESS, 100)
            .wait()
            .unwrap()
            .all_ok()
    );
    let r = link
        .set_config(uids[2], config_key::POWER_BUDGET_MA, 5)
        .wait()
        .unwrap();
    assert!(matches!(r.failed[0].1, LinkError::Rejected(_)));

    // Metadata persists and names resolve.
    link.rename(uids[2], Some("Kitchen".into())).unwrap();
    link.update_meta(
        uids[2],
        MetaUpdate {
            color: Some(Some([1, 2, 3])),
            ..Default::default()
        },
    )
    .unwrap();
    let json = std::fs::read_to_string(&meta_path).unwrap();
    assert!(json.contains("Kitchen") && json.contains(&uids[2].to_hex()));
    let s = link
        .wait_for(Duration::from_secs(1), |s| {
            s.gift(&uids[2]).unwrap().meta.name.is_some()
        })
        .unwrap();
    assert_eq!(s.resolve("kitchen").unwrap(), vec![uids[2]]);
    assert_eq!(s.resolve("s7").unwrap(), vec![uids[0]]);
    assert_eq!(
        s.resolve(&format!("@{}", s.gift(&uids[3]).unwrap().node_addr))
            .unwrap(),
        vec![uids[3]]
    );
    assert_eq!(s.resolve(&uids[1].to_hex()[..8]).unwrap(), vec![uids[1]]);
    assert_eq!(s.resolve("all").unwrap().len(), 4);
    assert!(s.resolve("nobody").is_err());

    // Channel change: gifts first, then the bridge; everyone stays online.
    let r = link.set_channel(90).wait().unwrap();
    assert!(r.all_ok(), "{r}");
    assert_eq!(sim.bridge_channel(), 90);
    assert!(sim.gifts().iter().all(|g| g.channel == 90));
    assert_eq!(link.snapshot().bridge.radio.channel, 90);
    std::thread::sleep(Duration::from_millis(300));
    link.wait_for(Duration::from_secs(3), |s| s.gifts.iter().all(|g| g.online))
        .expect("online on the new channel");

    // Reboot into the bootloader and back.
    assert!(link.reboot(uids[3], true).wait().unwrap().all_ok());
    link.wait_for(Duration::from_secs(3), |s| {
        s.gift(&uids[3]).unwrap().in_bootloader
    })
    .expect("in bootloader");
    assert!(link.reboot(uids[3], false).wait().unwrap().all_ok());
    link.wait_for(Duration::from_secs(3), |s| {
        let g = s.gift(&uids[3]).unwrap();
        !g.in_bootloader && g.online
    })
    .expect("back in app");
}

#[test]
fn leds_output_two_slots_per_packet() {
    let (link, sim) = start(SimConfig::new(3));
    wait_discovered(&link, &sim, 3, 15);
    assert!(
        link.auto_assign_slots(SlotAssign::ByUid)
            .wait()
            .unwrap()
            .all_ok()
    );
    let leds: Vec<[Rgb; 4]> = (0..3)
        .map(|i| {
            [
                Rgb::new(10 * (i + 1), 0, 0),
                Rgb::BLACK,
                Rgb::BLACK,
                Rgb::new(0, 0, 50),
            ]
        })
        .collect();
    link.set_show(ShowOutput::Leds(leds));
    std::thread::sleep(Duration::from_millis(2100));
    let s = link.snapshot();
    for g in sim.gifts() {
        let slot = g.slot.unwrap();
        assert!(
            g.leds[0].r > 0 && g.leds[3].b > 0,
            "slot {slot}: {:?}",
            g.leds
        );
    }
    // 2 packets per tick for 3 slots.
    assert!(
        s.link.show_packets_per_s > 180.0,
        "{}",
        s.link.show_packets_per_s
    );
    link.set_show(ShowOutput::Blackout);
    std::thread::sleep(Duration::from_millis(100));
    assert!(sim.gifts().iter().all(|g| g.leds == [Rgb::BLACK; 4]));
}

#[test]
fn clock_tracks_bridge() {
    let (link, _sim) = start(SimConfig::new(0));
    link.wait_for(Duration::from_secs(5), |s| s.bridge.clock_synced)
        .expect("synced");
    std::thread::sleep(Duration::from_secs(4)); // the sim bridge clock wraps ~3 s in
    let c = link.clock();
    let now = Instant::now();
    let b = c.from_instant(now);
    // Unwrapped past the u32 boundary, and round-trips.
    assert!(b > u32::MAX as u64, "{b}");
    let back = c.to_instant(b);
    let err = if back > now { back - now } else { now - back };
    assert!(err < Duration::from_micros(5), "{err:?}");
    assert!(c.rtt_us() < 5_000);
    assert_eq!(c.unwrap(b as u32), b);
}

/// A transport that fails while `unplugged` is set, like a pulled USB cable.
struct Flaky {
    inner: lumigift_core::sim::SimTransport,
    unplugged: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

impl nrf24_usb::Transport for Flaky {
    fn send(&mut self, cmds: &[nrf24_usb_proto::Command]) -> nrf24_usb::Result<()> {
        if self.unplugged.load(std::sync::atomic::Ordering::Relaxed) {
            return Err(nrf24_usb::Error::Disconnected);
        }
        self.inner.send(cmds)
    }
    fn recv(&mut self, timeout: Duration) -> nrf24_usb::Result<Option<nrf24_usb_proto::Event>> {
        if self.unplugged.load(std::sync::atomic::Ordering::Relaxed) {
            return Err(nrf24_usb::Error::Disconnected);
        }
        self.inner.recv(timeout)
    }
}

#[test]
fn reconnects_after_unplug() {
    use std::sync::atomic::{AtomicBool, Ordering};
    let (t, sim) = lumigift_core::sim::SimTransport::new(SimConfig::new(3));
    let unplugged = std::sync::Arc::new(AtomicBool::new(false));
    let mut first = Some(t);
    let (u2, sim2) = (unplugged.clone(), sim.clone());
    let link = Link::start(cfg(), move || {
        if u2.load(Ordering::Relaxed) {
            return Err(nrf24_usb::Error::NotFound(None));
        }
        let inner = first.take().unwrap_or_else(|| sim2.transport());
        Ok(Box::new(Flaky {
            inner,
            unplugged: u2.clone(),
        }) as Box<dyn nrf24_usb::Transport>)
    });
    wait_discovered(&link, &sim, 3, 15);
    let uid = link.snapshot().gifts[0].uid;

    unplugged.store(true, Ordering::Relaxed);
    link.wait_for(Duration::from_secs(2), |s| {
        matches!(s.bridge.state, BridgeState::Disconnected(_))
    })
    .expect("noticed unplug");
    // Commands fail cleanly while disconnected.
    let r = link
        .blink(uid, Rgb::new(1, 1, 1), 1, Duration::from_millis(100))
        .wait();
    assert_eq!(r.unwrap_err(), LinkError::NotConnected);
    link.wait_for(Duration::from_secs(3), |s| {
        s.gifts.iter().all(|g| !g.online)
    })
    .expect("gifts offline");

    unplugged.store(false, Ordering::Relaxed);
    link.wait_for(Duration::from_secs(5), |s| {
        s.bridge.state == BridgeState::Connected && s.online_count() == 3
    })
    .expect("reconnected");
    assert!(
        link.blink(uid, Rgb::new(1, 1, 1), 1, Duration::from_millis(100))
            .wait()
            .unwrap()
            .all_ok()
    );
}

#[test]
fn link_is_send_and_sync() {
    fn assert<T: Send + Sync>() {}
    assert::<Link>();
    assert::<Snapshot>();
    assert::<SimHandle>();
}
