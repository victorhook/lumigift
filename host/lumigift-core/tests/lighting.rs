//! Generic lighting looks through the whole host stack and the simulated
//! gifts (which render with `lumigift-fx` like the firmware): groups via
//! `ShowOutput::Layers` + masks, crossfades, lockstep. Real time.

use std::time::{Duration, Instant};

use lumigift_core::sim::{SimConfig, SimHandle};
use lumigift_core::*;
use lumigift_fx::effect;
use lumigift_proto::{MusicFrame, PALETTE_CUSTOM, Rgb};

const RED: Rgb = Rgb::new(200, 0, 0);
const BLUE: Rgb = Rgb::new(0, 0, 200);
const GREEN: Rgb = Rgb::new(0, 200, 0);

/// `slots[i]` = slot of gift `i`; no packet loss.
fn start(slots: &[u8]) -> (Link, SimHandle) {
    let mut sc = SimConfig::new(slots.len());
    sc.loss_pct = 0.0;
    for (g, &s) in sc.gifts.iter_mut().zip(slots) {
        g.slot = s;
    }
    let cfg = LinkConfig {
        metadata: MetadataLocation::Disabled,
        // Nothing dozes here; skip the 1.3 s Wake burst on connect.
        wake_on_connect: false,
        ..Default::default()
    };
    let (link, sim) = Link::simulated(cfg, sc);
    link.wait_for(Duration::from_secs(5), |s| {
        s.bridge.state == BridgeState::Connected && s.bridge.clock_synced
    })
    .expect("bridge connected");
    (link, sim)
}

fn solid(c: Rgb, mask: u32) -> MusicFrame {
    MusicFrame {
        effect: effect::SOLID,
        brightness: 255,
        palette: PALETTE_CUSTOM,
        colors: [c, Rgb::BLACK],
        slot_count: 4,
        mask,
        ..Default::default()
    }
}

/// LEDs of the sim gift at `slot`.
fn leds_at(sim: &SimHandle, slot: u8) -> [Rgb; 4] {
    sim.gifts()
        .into_iter()
        .find(|g| g.slot == Some(slot))
        .unwrap()
        .leds
}

fn dominant(c: Rgb) -> char {
    if c.r > c.g && c.r > c.b {
        'r'
    } else if c.g > c.r && c.g > c.b {
        'g'
    } else if c.b > c.r && c.b > c.g {
        'b'
    } else {
        '-'
    }
}

#[test]
fn layer_mask_only_affects_its_slots() {
    let (link, sim) = start(&[0, 1, 2, 3]);
    // A base layer for everyone, then a group layer for slots 1 and 3.
    link.set_show(ShowOutput::Layers(vec![
        solid(RED, MusicFrame::mask_of([0, 1, 2, 3])),
        solid(BLUE, MusicFrame::mask_of([1, 3])),
    ]));
    std::thread::sleep(Duration::from_millis(500));
    let got: String = (0..4).map(|s| dominant(leds_at(&sim, s)[0])).collect();
    assert_eq!(got, "rbrb");

    // Only slot 0 gets a new look; the others keep theirs well past the
    // renderer's signal timeout (the frames keep them alive).
    link.set_show(ShowOutput::Layers(vec![solid(
        GREEN,
        MusicFrame::mask_of([0]),
    )]));
    std::thread::sleep(Duration::from_millis(1500));
    let got: String = (0..4).map(|s| dominant(leds_at(&sim, s)[0])).collect();
    assert_eq!(got, "gbrb");

    // Two layers per tick on the air.
    let s = link.snapshot();
    assert!(s.link.frames_per_s > 80.0, "{}", s.link.frames_per_s);
}

#[test]
fn many_layers_round_robin() {
    let (link, sim) = start(&[0, 1, 2, 3]);
    // 12 layers (more than fit one tick): every slot still gets its own.
    let mut layers = Vec::new();
    for k in 0..12u8 {
        let c = match k % 4 {
            0 => RED,
            1 => GREEN,
            2 => BLUE,
            _ => Rgb::new(200, 200, 0),
        };
        layers.push(solid(c, MusicFrame::mask_of([k % 4])));
    }
    link.set_show(ShowOutput::Layers(layers));
    std::thread::sleep(Duration::from_millis(500));
    let got: String = (0..4).map(|s| dominant(leds_at(&sim, s)[0])).collect();
    assert_eq!(got, "rgb-");
    let y = leds_at(&sim, 3)[0];
    assert!(y.r > 50 && y.r == y.g && y.b == 0, "{y:?}");
}

#[test]
fn crossfade_completes() {
    let (link, sim) = start(&[0, 1]);
    link.set_show(ShowOutput::Music(solid(RED, MusicFrame::default().mask)));
    std::thread::sleep(Duration::from_millis(500));
    assert_eq!(dominant(leds_at(&sim, 0)[0]), 'r');

    let blue = MusicFrame {
        transition: 20, // 1 s
        ..solid(BLUE, MusicFrame::default().mask)
    };
    let t0 = Instant::now();
    link.set_show(ShowOutput::Music(blue));
    std::thread::sleep(Duration::from_millis(500));
    let mid = leds_at(&sim, 0)[0];
    eprintln!("mid-fade after {:?}: {mid:?}", t0.elapsed());
    assert!(mid.r > 10 && mid.b > 10, "half way: {mid:?}");
    std::thread::sleep(Duration::from_millis(900));
    let end = leds_at(&sim, 0);
    // (The sim gifts' default brightness cap scales 200 down.)
    assert!(end.iter().all(|c| c.r == 0 && c.b > 100), "{end:?}");
    assert_eq!(leds_at(&sim, 1), end);
}

#[test]
fn gifts_with_the_same_slot_run_in_lockstep() {
    // Gifts 0 and 1 share slot 2; gift 2 is elsewhere.
    let (link, sim) = start(&[2, 2, 0]);
    for e in [effect::TWINKLE, effect::SCANNER, effect::FIRE, effect::FADE] {
        link.set_show(ShowOutput::Music(MusicFrame {
            effect: e,
            brightness: 255,
            palette: 3,
            slot_count: 3,
            param: [200, 0],
            ..Default::default()
        }));
        std::thread::sleep(Duration::from_millis(400));
        let mut differs_from_other_slot = false;
        let mut animated = false;
        let mut last = None;
        for _ in 0..20 {
            let g = sim.gifts();
            assert_eq!(g[0].leds, g[1].leds, "effect {e}");
            differs_from_other_slot |= g[0].leds != g[2].leds;
            animated |= last.is_some_and(|l| l != g[0].leds);
            last = Some(g[0].leds);
            std::thread::sleep(Duration::from_millis(37));
        }
        assert!(differs_from_other_slot, "effect {e}: slot has no effect");
        assert!(animated, "effect {e} is static");
    }
}
