//! Firmware update robustness and timing through the whole host stack
//! (Link ⇄ simulator). The simulated bootloaders run the firmware's own
//! logic with flash timings, and the simulator runs in real time, so the
//! measured durations are hardware estimates.

use std::collections::BTreeSet;
use std::time::{Duration, Instant};

use lumigift_core::sim::{
    CutPoint, PowerCut, SimAppFault, SimConfig, SimGiftConfig, SimHandle, build_test_image,
};
use lumigift_core::*;
use lumigift_proto::{DEFAULT_CHANNEL, ImageHeader, layout};

/// Size of the current app image (`cargo xtask image`: 256 + 15,532 bytes).
const APP_LEN: usize = 15_532;

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

/// `n` gifts online, discovered, on unique addresses matching the simulator.
fn wait_online(link: &Link, sim: &SimHandle, n: usize, secs: u64) {
    let ok = link.wait_for(Duration::from_secs(secs), |s| {
        let online: Vec<_> = s
            .gifts
            .iter()
            .filter(|g| g.online && !g.address_conflict)
            .collect();
        let addrs: BTreeSet<u8> = online.iter().map(|g| g.node_addr).collect();
        let simg = sim.gifts();
        s.discovery.initial_sweep_done
            && online.len() >= n
            && addrs.len() == online.len()
            && online.iter().all(|g| {
                simg.iter()
                    .any(|x| x.uid == g.uid && x.node_addr == g.node_addr)
            })
    });
    if ok.is_none() {
        dump(link);
        panic!("not all {n} gifts discovered");
    }
}

fn uids(link: &Link) -> Vec<Uid> {
    link.snapshot().gifts.iter().map(|g| g.uid).collect()
}

fn ota(link: &Link, image: &[u8], targets: Vec<Uid>) -> OtaReport {
    let r = link
        .start_ota(image.to_vec(), targets)
        .wait_timeout(Duration::from_secs(60))
        .expect("OTA finished");
    match r {
        Ok(r) => r,
        Err(e) => {
            for ev in link.snapshot().events.iter() {
                eprintln!("{:?} {}", ev.level, ev.message);
            }
            panic!("OTA failed: {e}");
        }
    }
}

fn dump(link: &Link) {
    for e in link.snapshot().events.iter() {
        eprintln!("{:?} {}", e.level, e.message);
    }
}

fn all_run(sim: &SimHandle, fw: u32) -> bool {
    sim.gifts()
        .iter()
        .all(|g| g.image_fw_version == Some(fw) && !g.in_bootloader)
}

#[test]
fn timing_estimate_ten_gifts_at_0_5_20_percent_loss() {
    let mut lines = Vec::new();
    for (k, loss) in [0.0f32, 5.0, 20.0].into_iter().enumerate() {
        let mut sc = SimConfig::with_seed(10, 100 + k as u64);
        sc.loss_pct = loss;
        let (link, sim) = start(sc);
        wait_online(&link, &sim, 10, 30);
        let fw = 1_000 + k as u32;
        let image = build_test_image(fw, APP_LEN, 11);
        let t0 = Instant::now();
        let r = ota(&link, &image, uids(&link));
        let wall = t0.elapsed();
        assert_eq!(r.updated.len(), 10, "loss {loss}%: failed {:?}", r.failed);
        if !all_run(&sim, fw) {
            dump(&link);
            for g in sim.gifts() {
                eprintln!("{g:?}");
            }
            panic!("loss {loss}%");
        }
        let phases: Vec<String> = r
            .phase_times
            .iter()
            .map(|(p, d)| format!("{p:?} {} ms", d.as_millis()))
            .collect();
        lines.push(format!(
            "loss {loss:>4}%: {:.2} s, {} chunks ({} rounds) | {}",
            wall.as_secs_f32(),
            r.chunks_sent,
            r.rounds,
            phases.join(", ")
        ));
        // Generous bounds (the machine may be busy with other tests).
        let limit = [2.0, 3.0, 6.0][k];
        assert!(
            wall.as_secs_f32() < limit,
            "loss {loss}%: {wall:?} > {limit} s"
        );
    }
    for l in lines {
        eprintln!("OTA estimate, 10 gifts, {} B image: {l}", APP_LEN + 256);
    }
}

#[test]
fn up_to_date_targets_are_skipped_unless_forced() {
    let mut sc = SimConfig::new(3);
    sc.loss_pct = 0.0;
    let (link, sim) = start(sc);
    wait_online(&link, &sim, 3, 20);
    let image = build_test_image(500, 6_000, 3);
    let r = ota(&link, &image, uids(&link));
    assert_eq!((r.updated.len(), r.skipped.len()), (3, 0));
    // Same image again: nothing to do.
    let t0 = Instant::now();
    let r = ota(&link, &image, uids(&link));
    assert_eq!((r.updated.len(), r.skipped.len()), (0, 3), "{r:?}");
    assert_eq!(r.chunks_sent, 0);
    assert!(t0.elapsed() < Duration::from_millis(500));
    // Same version number but different bytes is not "up to date".
    let other = build_test_image(500, 6_000, 4);
    let r = ota(&link, &other, uids(&link));
    assert_eq!(r.updated.len(), 3);
    // Forced.
    let r = link
        .start_ota_with(other.clone(), uids(&link), true)
        .wait_timeout(Duration::from_secs(30))
        .unwrap()
        .unwrap();
    assert_eq!((r.updated.len(), r.skipped.len()), (3, 0));
    assert!(all_run(&sim, 500));
}

#[test]
fn host_refuses_bad_images_and_foreign_hardware() {
    let (link, sim) = start(SimConfig::new(2));
    wait_online(&link, &sim, 2, 20);
    let good = build_test_image(700, 4_000, 5);
    let o = layout::IMAGE_HEADER_SIZE as usize;
    let mut bad_header = good.clone();
    bad_header[9] ^= 1;
    let mut bad_app = good.clone();
    bad_app[o + 1000] ^= 1;
    let mut bad_vectors = good.clone();
    bad_vectors[o + 4..o + 8].copy_from_slice(&0x0800_0101u32.to_le_bytes()); // reset into the bootloader
    let h = ImageHeader::new(&bad_vectors[o..], 1, 700, [0; 8]);
    bad_vectors[..32].copy_from_slice(&h.encode());
    for img in [bad_header, bad_app, bad_vectors] {
        assert!(matches!(
            link.start_ota(img, uids(&link)).wait(),
            Err(LinkError::InvalidImage(_))
        ));
    }
    // An image for another board revision is refused per target.
    let mut rev2 = good.clone();
    let h = ImageHeader::new(&rev2[o..], 2, 700, [0; 8]);
    rev2[..32].copy_from_slice(&h.encode());
    let r = link.start_ota(rev2, uids(&link)).wait();
    assert!(r.is_err(), "{r:?}");
    // Nothing was touched.
    assert!(all_run(&sim, 100));
}

/// Power cut at `at` on gift 0; power returns after `off_for`.
fn power_cut_case(at: CutPoint, off_for: Duration) {
    let mut sc = SimConfig::with_seed(4, 7);
    sc.loss_pct = 2.0;
    let (link, sim) = start(sc);
    wait_online(&link, &sim, 4, 20);
    let image = build_test_image(900, APP_LEN, 9);
    sim.schedule_power_cut(0, PowerCut { at, off_for });
    let first = link
        .start_ota(image.clone(), uids(&link))
        .wait_timeout(Duration::from_secs(60))
        .unwrap();
    let g0 = sim.gifts()[0].uid;
    match &first {
        // Short outage: the engine restarted the gift's transfer (or, after
        // `Verified`, the gift came back with the new image).
        Ok(r) if r.failed.is_empty() => {}
        // Long outage: that gift failed; it must sit in its bootloader...
        other => {
            let r = other.as_ref().expect("the other gifts were updated");
            assert_eq!(r.failed.len(), 1, "{at:?}: {r:?}");
            assert_eq!(r.failed[0].0, g0);
            assert_eq!(r.updated.len(), 3);
            link.wait_for(Duration::from_secs(10), |_| sim.gifts()[0].powered)
                .unwrap();
            std::thread::sleep(Duration::from_millis(400));
            let v = &sim.gifts()[0];
            assert!(v.in_bootloader, "{at:?}: {v:?}");
            assert_eq!(
                v.image_fw_version, None,
                "{at:?}: half-written image must not validate"
            );
            // ...and a re-run updates it.
            link.wait_for(Duration::from_secs(5), |s| {
                s.gift(&g0).is_some_and(|g| g.online && g.in_bootloader)
            })
            .unwrap_or_else(|| {
                dump(&link);
                panic!("{at:?}: gift not back online in its bootloader")
            });
            let r = ota(&link, &image, vec![g0]);
            assert_eq!(r.updated, vec![g0], "{at:?}");
        }
    }
    assert!(all_run(&sim, 900), "{at:?}: {:?}", sim.gifts()[0]);
}

#[test]
fn power_loss_mid_erase() {
    power_cut_case(CutPoint::MidErase(0), Duration::from_millis(300));
    power_cut_case(CutPoint::MidErase(4), Duration::from_secs(5));
}

#[test]
fn power_loss_mid_transfer() {
    power_cut_case(CutPoint::AfterChunks(300), Duration::from_millis(300));
    power_cut_case(CutPoint::AfterChunks(100), Duration::from_secs(5));
}

#[test]
fn power_loss_right_after_verified() {
    // The image is complete and verified: the gift boots it after power-on.
    power_cut_case(CutPoint::AfterVerified, Duration::from_millis(300));
}

#[test]
fn flash_write_error_fails_verification_then_recovers() {
    let mut sc = SimConfig::new(3);
    sc.loss_pct = 0.0;
    let (link, sim) = start(sc);
    wait_online(&link, &sim, 3, 20);
    sim.glitch_flash_write(1, 5000);
    let image = build_test_image(910, 9_000, 2);
    let r = ota(&link, &image, uids(&link));
    assert_eq!(r.updated.len(), 3, "{r:?}");
    assert!(
        link.snapshot()
            .events
            .iter()
            .any(|e| e.message.contains("verification failed"))
    );
    assert!(all_run(&sim, 910));
}

#[test]
fn crash_looping_app_ends_in_recovery_and_is_fixed_over_the_air() {
    let mut sc = SimConfig::new(2);
    sc.loss_pct = 0.0;
    let (link, sim) = start(sc);
    wait_online(&link, &sim, 2, 20);
    // A bad app that crashes 300 ms after every start.
    sim.set_app_fault(0, SimAppFault::CrashAfter(Duration::from_millis(300)));
    sim.reboot(0);
    // boot_attempts counts the starts; at 3 the bootloader stays in recovery.
    link.wait_for(Duration::from_secs(5), |_| {
        let g = &sim.gifts()[0];
        g.in_bootloader && g.boot_attempts >= 3 && g.ota_state.is_some()
    })
    .expect("crash loop ends in recovery");
    let uid = sim.gifts()[0].uid;
    link.wait_for(Duration::from_secs(5), |s| {
        s.gift(&uid).is_some_and(|g| g.online && g.in_bootloader)
    })
    .expect("host sees it in the bootloader");
    let r = ota(&link, &build_test_image(920, 8_000, 1), vec![uid]);
    assert_eq!(r.updated, vec![uid]);
    std::thread::sleep(Duration::from_millis(600));
    let g = &sim.gifts()[0];
    assert!(
        !g.in_bootloader && g.app_fault == SimAppFault::None,
        "{g:?}"
    );
}

#[test]
fn hanging_app_is_reset_by_the_watchdog_into_recovery() {
    let mut sc = SimConfig::new(1);
    sc.loss_pct = 0.0;
    let (link, sim) = start(sc);
    wait_online(&link, &sim, 1, 20);
    sim.set_app_fault(0, SimAppFault::Hang);
    sim.reboot(0);
    // Three starts, each ended by the ~2 s IWDG, then recovery.
    let t0 = Instant::now();
    link.wait_for(Duration::from_secs(10), |_| {
        let g = &sim.gifts()[0];
        g.in_bootloader && g.boot_attempts >= 3 && g.ota_state.is_some()
    })
    .expect("IWDG resets end in recovery");
    assert!(t0.elapsed() > Duration::from_secs(5));
    assert_eq!(
        sim.gifts()[0].reset_reason,
        lumigift_proto::ResetReason::Watchdog
    );
    let uid = sim.gifts()[0].uid;
    link.wait_for(Duration::from_secs(5), |s| {
        s.gift(&uid).is_some_and(|g| g.online && g.in_bootloader)
    })
    .expect("reachable");
    let r = ota(&link, &build_test_image(930, 8_000, 1), vec![uid]);
    assert_eq!(r.updated, vec![uid]);
}

#[test]
fn deaf_app_is_rescued_with_boot_hold_on_the_default_channel() {
    let mut sc = SimConfig::new(2);
    sc.loss_pct = 2.0;
    // The bad app also stored a channel the host is not on.
    sc.gifts[0].channel = Some(90);
    sc.gifts[0].app_fault = SimAppFault::DeafRadio;
    let uid = Uid(sc.gifts[0].uid);
    let (link, sim) = start(sc);
    wait_online(&link, &sim, 1, 20);
    assert!(link.snapshot().gift(&uid).is_none_or(|g| !g.online));
    // The user power-cycles the gift while the host broadcasts BootHold.
    let mut rescue = link.rescue(Duration::from_millis(2500));
    std::thread::sleep(Duration::from_millis(400));
    sim.power_cycle(0, Duration::from_millis(300));
    let r = rescue
        .wait_timeout(Duration::from_secs(20))
        .unwrap()
        .unwrap();
    assert!(r.in_bootloader.contains(&uid), "{r:?}");
    let v = &sim.gifts()[0];
    assert!(v.in_bootloader);
    assert_eq!(v.channel, DEFAULT_CHANNEL, "rescued on the default channel");
    assert_eq!(v.config_channel, 90);
    // Fix it over the air; the new app runs on the host's channel.
    let r = ota(&link, &build_test_image(940, 8_000, 3), vec![uid]);
    assert_eq!(r.updated, vec![uid]);
    let v = &sim.gifts()[0];
    assert_eq!(
        (v.config_channel, v.app_fault),
        (DEFAULT_CHANNEL, SimAppFault::None)
    );
}

#[test]
fn wrong_channel_in_config_is_reachable_in_recovery() {
    let mut sc = SimConfig::new(2);
    sc.loss_pct = 1.0;
    let mut stuck = SimGiftConfig::new([0x5A; 12]);
    stuck.valid_image = false; // recovery mode
    stuck.channel = Some(90);
    sc.gifts.push(stuck);
    let (link, sim) = start(sc);
    // The bootloader alternates 90 / default until it hears the host.
    link.wait_for(Duration::from_secs(25), |s| {
        s.gift(&Uid([0x5A; 12]))
            .is_some_and(|g| g.online && g.in_bootloader)
    })
    .unwrap_or_else(|| {
        dump(&link);
        panic!("recovery gift on a wrong channel never found")
    });
    let r = ota(
        &link,
        &build_test_image(950, 5_000, 4),
        vec![Uid([0x5A; 12])],
    );
    assert_eq!(r.updated.len(), 1);
    // The bootloader stored the channel the update came on, so the new app
    // is reachable too.
    let v = &sim.gifts()[2];
    assert_eq!(
        (v.channel, v.config_channel),
        (DEFAULT_CHANNEL, DEFAULT_CHANNEL)
    );
    assert!(!v.in_bootloader);
}

#[test]
fn host_vanishes_mid_update_then_a_new_session_completes() {
    let mut sc = SimConfig::new(3);
    sc.loss_pct = 2.0;
    let (link, sim) = start(sc);
    wait_online(&link, &sim, 3, 20);
    let targets = uids(&link);
    let mut p = link.start_ota(build_test_image(960, APP_LEN, 5), targets.clone());
    link.wait_for(Duration::from_secs(10), |_| {
        sim.gifts()
            .iter()
            .any(|g| g.ota_state == Some(lumigift_proto::OtaState::Receiving))
    })
    .unwrap();
    std::thread::sleep(Duration::from_millis(120));
    link.cancel_ota();
    assert_eq!(
        p.wait_timeout(Duration::from_secs(10))
            .unwrap()
            .unwrap_err(),
        LinkError::Cancelled
    );
    // The gifts hold a half-written image and stay in recovery.
    std::thread::sleep(Duration::from_millis(500));
    for g in sim.gifts() {
        assert!(g.in_bootloader, "{g:?}");
        assert_eq!(g.image_fw_version, None);
    }
    // A new session (another image) goes through.
    let r = ota(&link, &build_test_image(961, 7_000, 6), targets);
    assert_eq!(r.updated.len(), 3);
    assert!(all_run(&sim, 961));
    let uniq: BTreeSet<_> = sim.gifts().iter().map(|g| g.image_fw_version).collect();
    assert_eq!(uniq.len(), 1);
}
