//! Low-power doze through the whole host stack (real time, simulated gifts):
//! `Sleep` / `Wake`, auto-doze without a host, and the link's automatic
//! wakes (on connect, before an OTA, when a look starts).

use std::time::{Duration, Instant};

use lumigift_core::sim::{SimConfig, SimHandle, SimTransport, build_test_image};
use lumigift_core::*;
use lumigift_proto::MusicFrame;

fn cfg(wake_on_connect: bool) -> LinkConfig {
    LinkConfig {
        metadata: MetadataLocation::Disabled,
        wake_on_connect,
        ..Default::default()
    }
}

/// A link to an already-created simulator (its gifts have been running,
/// without a host, since `SimTransport::new`).
fn connect(t: SimTransport, wake_on_connect: bool) -> Link {
    let mut t = Some(t);
    Link::start(cfg(wake_on_connect), move || {
        t.take()
            .map(|t| Box::new(t) as Box<dyn nrf24_usb::Transport>)
            .ok_or_else(|| nrf24_usb::Error::Usb("simulator already in use".into()))
    })
}

fn wait_online(link: &Link, n: usize, secs: u64) -> Duration {
    let t0 = Instant::now();
    link.wait_for(Duration::from_secs(secs), |s| {
        s.discovery.initial_sweep_done && s.online_count() == n && s.gifts.len() == n
    })
    .unwrap_or_else(|| {
        let s = link.snapshot();
        for e in s.events.iter() {
            eprintln!("{:?} {}", e.level, e.message);
        }
        panic!("{} of {n} gifts online", s.online_count());
    });
    t0.elapsed()
}

fn dozing(sim: &SimHandle) -> usize {
    sim.gifts().iter().filter(|g| g.dozing).count()
}

fn music() -> ShowOutput {
    ShowOutput::Music(MusicFrame {
        effect: lumigift_fx::effect::SOLID,
        brightness: 255,
        palette: lumigift_proto::PALETTE_CUSTOM,
        colors: [Rgb::new(255, 80, 0), Rgb::BLACK],
        ..Default::default()
    })
}

/// Online gifts, put to sleep.
fn asleep(n: usize, show: ShowOutput) -> (Link, SimHandle) {
    let (link, sim) = Link::simulated(cfg(false), SimConfig::new(n));
    wait_online(&link, n, 15);
    link.set_show(show);
    std::thread::sleep(Duration::from_millis(300));
    let slept = link.sleep_all().wait().unwrap();
    assert_eq!(slept, n);
    assert_eq!(dozing(&sim), n);
    let s = link.snapshot();
    assert_eq!(s.asleep_count(), n);
    assert_eq!(s.online_count(), 0);
    (link, sim)
}

#[test]
fn sleep_all_and_show_frames_do_not_wake() {
    let n = 6;
    // A look is already running, so nothing starts and nothing auto-wakes.
    let (link, sim) = asleep(n, music());
    let failed0: u64 = link.snapshot().gifts.iter().map(|g| g.polls_failed).sum();
    // Show frames at 100 Hz land in every listen window, and do not wake.
    std::thread::sleep(Duration::from_millis(2500));
    assert_eq!(dozing(&sim), n, "show frames woke a gift");
    for g in sim.gifts() {
        assert_eq!(g.leds, [Rgb::BLACK; 4], "a dozing gift is dark");
    }
    let s = link.snapshot();
    assert_eq!(s.asleep_count(), n, "asleep, not offline");
    assert!(s.gifts.iter().all(|g| g.asleep && !g.online));
    // Asleep gifts are not polled.
    let failed: u64 = s.gifts.iter().map(|g| g.polls_failed).sum();
    assert!(failed <= failed0 + 2 * n as u64, "asleep gifts were polled");
}

#[test]
fn wake_all_brings_every_gift_back() {
    let n = 8;
    let (link, sim) = asleep(n, ShowOutput::Idle);
    let t0 = Instant::now();
    let mut p = link.wake_all();
    let back = link
        .wait_for(Duration::from_secs(4), |s| s.online_count() == n)
        .map(|_| t0.elapsed());
    let online = p.wait_timeout(Duration::from_secs(10)).unwrap().unwrap();
    eprintln!(
        "all {n} online after {back:?}, wake_all done after {:?}",
        t0.elapsed()
    );
    if back.is_none() || online != n {
        for e in link.snapshot().events.iter() {
            eprintln!("{:?} {}", e.level, e.message);
        }
        for g in sim.gifts() {
            eprintln!(
                "sim {} addr {} dozing {}",
                g.uid.short(),
                g.node_addr,
                g.dozing
            );
        }
    }
    let back = back.expect("not every gift came back");
    assert!(back < Duration::from_millis(2200), "took {back:?}");
    assert_eq!(online, n);
    assert_eq!(dozing(&sim), 0);
    assert_eq!(link.snapshot().asleep_count(), 0);
}

#[test]
fn auto_doze_without_host_then_wake_on_connect() {
    let n = 5;
    let mut sc = SimConfig::new(n);
    sc.doze_auto_after = Duration::from_millis(1500);
    let (t, sim) = SimTransport::new(sc);
    assert_eq!(dozing(&sim), 0);
    std::thread::sleep(Duration::from_millis(2000));
    assert_eq!(
        dozing(&sim),
        n,
        "no host for doze_auto_after: every gift dozes"
    );
    let link = connect(t, true);
    let took = wait_online(&link, n, 15);
    eprintln!("connect + wake + sweep: {took:?}");
    assert_eq!(dozing(&sim), 0);
    // With a host around (beacons, polls), nobody dozes again.
    std::thread::sleep(Duration::from_millis(2000));
    assert_eq!(dozing(&sim), 0);
    assert_eq!(link.snapshot().online_count(), n);
}

#[test]
fn without_wake_on_connect_dozing_gifts_stay_hidden_until_wake_all() {
    let n = 4;
    let mut sc = SimConfig::new(n);
    sc.doze_auto_after = Duration::from_millis(500);
    let (t, sim) = SimTransport::new(sc);
    std::thread::sleep(Duration::from_millis(800));
    let link = connect(t, false);
    link.wait_for(Duration::from_secs(15), |s| s.discovery.initial_sweep_done)
        .expect("sweep");
    assert_eq!(
        link.snapshot().gifts.len(),
        0,
        "dozing gifts do not answer the sweep"
    );
    // wake_all sweeps afterwards, so it finds gifts it never saw.
    assert_eq!(link.wake_all().wait().unwrap(), n);
    assert_eq!(dozing(&sim), 0);
}

#[test]
fn ota_wakes_asleep_targets() {
    let n = 4;
    let (link, sim) = asleep(n, ShowOutput::Idle);
    let uids: Vec<Uid> = link.snapshot().gifts.iter().map(|g| g.uid).collect();
    let image = build_test_image(101, 12 * 1024, 3);
    let r = link.start_ota(image, uids).wait().expect("ota");
    assert!(r.failed.is_empty(), "failed: {:?}", r.failed);
    assert_eq!(r.updated.len(), n);
    assert!(
        sim.gifts()
            .iter()
            .all(|g| !g.dozing && g.image_fw_version == Some(101))
    );
}

#[test]
fn starting_a_look_wakes_asleep_gifts() {
    let n = 5;
    let (link, sim) = asleep(n, ShowOutput::Blackout);
    let t0 = Instant::now();
    link.set_show(music());
    link.wait_for(Duration::from_secs(5), |s| s.online_count() == n)
        .expect("a starting look wakes the gifts");
    eprintln!("look → all online in {:?}", t0.elapsed());
    std::thread::sleep(Duration::from_millis(200));
    assert!(
        sim.gifts()
            .iter()
            .all(|g| !g.dozing && g.leds != [Rgb::BLACK; 4])
    );
}
