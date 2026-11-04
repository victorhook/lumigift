//! ShowEngine and Show model behaviour.

use std::sync::Arc;

use lumigift_audio::show::palette;
use lumigift_audio::synth;
use lumigift_audio::{Analysis, Cue, OneShot, OneShotKind, Show, ShowEngine, Snap};
use lumigift_fx::effect;
use lumigift_proto::music_flags as mf;

fn demo() -> Arc<Analysis> {
    let t = synth::demo_track(44_100);
    Arc::new(Analysis::analyze(&t.samples, t.sample_rate))
}

#[test]
fn every_beat_flagged_exactly_once_at_100hz() {
    let a = demo();
    let mut e = ShowEngine::new(a.clone(), Show::default());
    let mut beats = 0;
    let mut downs = 0;
    let mut drops = 0;
    let mut t = 0.0;
    // Jittery steps around 10 ms.
    let mut k = 0u32;
    while t < a.duration {
        let f = e.frame_at(t, 10);
        beats += (f.flags & mf::BEAT != 0) as usize;
        downs += (f.flags & mf::DOWNBEAT != 0) as usize;
        drops += (f.flags & mf::DROP != 0) as usize;
        assert_eq!(f.slot_count, 10);
        k = k.wrapping_mul(1_103_515_245).wrapping_add(12345);
        t += 0.008 + (k >> 16) as f64 % 5.0 / 1000.0;
    }
    assert_eq!(beats, a.beats.len());
    assert_eq!(downs, a.downbeats().count());
    assert_eq!(drops, a.drops.len());
}

#[test]
fn beat_phase_and_bar_fields() {
    let a = demo();
    let e = ShowEngine::new(a.clone(), Show::default());
    let b = a.beats[20];
    let next = a.beats[21].time;
    let f = e.peek_frame(b.time + (next - b.time) * 0.5, 10);
    assert!(
        (f.beat_phase as i32 - 128).abs() <= 3,
        "phase {}",
        f.beat_phase
    );
    assert_eq!(f.beat_in_bar & 3, b.beat_in_bar);
    assert_eq!(f.beat_in_bar >> 2, (b.bar % 64) as u8);
    assert!((f.bpm as i32 - 120).abs() <= 2);
    let f = e.peek_frame(b.time + 0.001, 10);
    assert!(f.beat_phase < 5);
}

#[test]
fn seeking_backwards_resets_edges_without_bursts() {
    let a = demo();
    let mut e = ShowEngine::new(a.clone(), Show::default());
    let mut t = 30.0;
    while t < 40.0 {
        e.frame_at(t, 10);
        t += 0.01;
    }
    // Jump back 20 s: the first frame only looks at the last 10 ms.
    let b = a.beats.iter().find(|b| b.time > 10.0).unwrap().time;
    let f = e.frame_at(b - 0.05, 10);
    assert_eq!(f.flags & mf::BEAT, 0);
    let f = e.frame_at(b + 0.001, 10);
    assert_ne!(f.flags & mf::BEAT, 0);
    // A large forward jump over many beats yields no BEAT burst either.
    let f = e.frame_at(b + 20.0 + 0.3, 10);
    let expect_beat = a
        .beats
        .iter()
        .any(|x| x.time > b + 20.29 && x.time <= b + 20.3);
    assert_eq!(f.flags & mf::BEAT != 0, expect_beat);
}

#[test]
fn deterministic() {
    let a = demo();
    let show = Show::auto_generate(&a);
    let mut e1 = ShowEngine::new(a.clone(), show.clone());
    let mut e2 = ShowEngine::new(a.clone(), show);
    for i in 0..5000 {
        let t = i as f64 * 0.0137;
        assert_eq!(e1.frame_at(t, 10), e2.frame_at(t, 10));
    }
}

#[test]
fn silence_flag_and_features() {
    let a = demo();
    let e = ShowEngine::new(a.clone(), Show::default());
    let f = e.peek_frame(1.0, 10);
    assert_ne!(f.flags & mf::SILENT, 0);
    assert_eq!(f.bass, 0);
    let f = e.peek_frame(30.0, 10);
    assert_eq!(f.flags & mf::SILENT, 0);
    assert!(f.level > 50);
    assert_ne!(e.peek_frame(a.duration + 1.0, 10).flags & mf::SILENT, 0);
}

#[test]
fn cue_state_hue_drift_fade_and_one_shots() {
    let a = demo();
    let mut show = Show {
        cues: vec![],
        ..Default::default()
    };
    show.settings.master_brightness = 255;
    // Exact cue hues: keep the notes out of it.
    show.settings.note_color = 0.0;
    show.insert_cue(Cue {
        time: 0.0,
        effect: effect::PULSE,
        palette: palette::FIRE,
        brightness: 100,
        hue: 10,
        ..Default::default()
    });
    show.insert_cue(Cue {
        time: 10.0,
        effect: effect::CHASE,
        palette: palette::OCEAN,
        brightness: 200,
        hue: 100,
        hue_speed: 10.0,
        fade: 2.0,
        params: [7, 9],
        ..Default::default()
    });
    show.insert_event(OneShot {
        time: 20.0,
        duration: 1.0,
        kind: OneShotKind::Blackout,
    });
    show.insert_event(OneShot {
        time: 30.0,
        duration: 1.0,
        kind: OneShotKind::Strobe { rate_hz: 8.0 },
    });
    show.insert_event(OneShot {
        time: 40.0,
        duration: 1.0,
        kind: OneShotKind::Flash {
            hue: 77,
            palette: Some(palette::ICE),
        },
    });
    let mut e = ShowEngine::new(a.clone(), show);

    let f = e.peek_frame(5.0, 10);
    assert_eq!(
        (f.effect, f.palette, f.brightness, f.hue),
        (effect::PULSE, palette::FIRE, 100, 10)
    );
    let f = e.peek_frame(11.0, 10); // half-way through the fade
    assert_eq!(f.effect, effect::CHASE);
    assert!((f.brightness as i32 - 150).abs() <= 1, "{}", f.brightness);
    assert_eq!(f.param, [7, 9]);
    let f = e.peek_frame(15.0, 10); // 5 s of drift at 10/s
    assert_eq!((f.brightness, f.hue), (200, 150));
    assert_eq!(e.peek_frame(20.5, 10).brightness, 0);
    let f = e.peek_frame(40.0, 10);
    assert_eq!(
        (f.effect, f.hue, f.palette, f.brightness),
        (effect::SOLID, 77, palette::ICE, 255)
    );

    // Strobe: ~8 ticks in its one second.
    let mut ticks = 0;
    let mut t = 29.5;
    while t < 31.5 {
        let f = e.frame_at(t, 10);
        if (30.0..31.0).contains(&t) {
            assert_eq!(f.effect, effect::STROBE);
            assert_eq!(f.brightness, 255);
            ticks += (f.flags & mf::ONSET != 0 && f.flags & mf::BEAT != 0) as usize;
        }
        t += 0.01;
    }
    assert!((8..=10).contains(&ticks), "strobe ticks {ticks}");
}

#[test]
fn slot_overrides() {
    let a = demo();
    let mut show = Show::default();
    show.cues[0]
        .slot_overrides
        .push(lumigift_audio::SlotOverride {
            slot: 3,
            effect: Some(effect::SPARKLE),
            hue: Some(42),
            ..Default::default()
        });
    let e = ShowEngine::new(a, show);
    let f = e.peek_frame(5.0, 10);
    assert!(e.has_slot_overrides(5.0));
    let s3 = e.slot_frame(&f, 5.0, 3);
    assert_eq!((s3.effect, s3.hue), (effect::SPARKLE, 42));
    assert_eq!(e.slot_frame(&f, 5.0, 2), f);
}

#[test]
fn lookahead_is_added_for_playback() {
    let a = demo();
    let mut e = ShowEngine::new(a.clone(), Show::default());
    e.set_lookahead(0.025);
    let b = a.beats[30].time;
    // Heard position 20 ms before the beat + 25 ms look-ahead → beat flagged now.
    e.frame_for_playback(b - 0.04, 10);
    let f = e.frame_for_playback(b - 0.02, 10);
    assert_ne!(f.flags & mf::BEAT, 0);
}

#[test]
fn show_editing_and_json_roundtrip() {
    let mut s = Show::default();
    s.track.title = "Test".into();
    let i = s.insert_cue(Cue {
        time: 5.0,
        effect: effect::RAINBOW,
        ..Default::default()
    });
    assert_eq!(i, 1);
    let j = s.insert_cue(Cue {
        time: 2.0,
        effect: effect::CHASE,
        ..Default::default()
    });
    assert_eq!(j, 1);
    assert!(s.cues.windows(2).all(|w| w[0].time <= w[1].time));
    // Same time replaces.
    s.insert_cue(Cue {
        time: 2.0,
        effect: effect::SPARKLE,
        ..Default::default()
    });
    assert_eq!(s.cues.len(), 3);
    assert_eq!(s.cue_at(3.0).unwrap().effect, effect::SPARKLE);
    let k = s.move_cue(1, 9.0).unwrap();
    assert_eq!(k, 2);
    assert_eq!(s.cue_at(6.0).unwrap().effect, effect::RAINBOW);
    let removed = s.remove_cue(2).unwrap();
    assert_eq!(removed.effect, effect::SPARKLE);
    let n = s.split_at(7.0);
    assert_eq!(s.cues[n].time, 7.0);
    assert_eq!(s.cues[n].effect, effect::RAINBOW);
    s.insert_event(OneShot {
        time: 3.0,
        duration: 0.5,
        kind: OneShotKind::Flash {
            hue: 3,
            palette: None,
        },
    });
    s.insert_event(OneShot {
        time: 1.0,
        duration: 0.5,
        kind: OneShotKind::Strobe { rate_hz: 6.0 },
    });
    assert_eq!(s.events[0].time, 1.0);
    assert_eq!(s.events_at(1.2).count(), 1);
    assert_eq!(s.events_at(1.6).count(), 0);

    let json = s.to_json();
    let back = Show::from_json(&json).unwrap();
    assert_eq!(back, s);
    assert!(json.contains("\"type\": \"strobe\""));

    let dir = std::env::temp_dir().join(format!("lg-show-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let p = dir.join("x.lgshow");
    s.save(&p).unwrap();
    assert_eq!(Show::load(&p).unwrap(), s);
    std::fs::remove_dir_all(&dir).ok();

    // Older/minimal files load with defaults.
    let minimal = r#"{"version":1,"track":{"path":"a.mp3","content_hash":"","title":"A"},
        "cues":[{"time":0,"effect":2,"palette":1,"brightness":200,"hue":5}]}"#;
    let m = Show::from_json(minimal).unwrap();
    assert_eq!(m.cues[0].hue_speed, 0.0);
    assert!(m.settings.beat_flags);
    assert!(
        Show::from_json(
            r#"{"version":99,"track":{"path":"","content_hash":"","title":""},"cues":[]}"#
        )
        .is_err()
    );
}

#[test]
fn auto_generated_show_is_sensible() {
    // Long track with several sections so every effect gets a turn.
    let mut parts = vec![synth::Part::silence(1.0)];
    for i in 0..4 {
        parts.push(synth::Part::groove(32.0, 124.0, 0.35));
        parts.push(synth::Part {
            kick: false,
            snare: false,
            ..synth::Part::groove(16.0, 124.0, 0.35)
        });
        parts.push(synth::Part {
            bass: true,
            ..synth::Part::groove(48.0 + 16.0 * i as f64, 124.0, 1.0)
        });
        parts.push(synth::Part::groove(32.0, 124.0, 0.6));
    }
    let t = synth::render(&parts, 44_100);
    let a = Analysis::analyze(&t.samples, 44_100);
    let show = Show::auto_generate(&a);
    assert!(show.cues.windows(2).all(|w| w[0].time <= w[1].time));
    // The original eight effects (newer ones are not required here).
    for fx in [
        effect::SOLID,
        effect::PULSE,
        effect::SPECTRUM,
        effect::CHASE,
        effect::STROBE,
        effect::RAINBOW,
        effect::SPARKLE,
        effect::BREATHE,
    ] {
        assert!(
            show.cues.iter().any(|c| c.effect == fx),
            "effect {fx} unused"
        );
    }
    // A flash at every drop, a strobe burst at strong ones, Strobe cue at the drop.
    for d in &a.drops {
        assert!(
            show.events
                .iter()
                .any(|e| (e.time - d.time).abs() < 0.01
                    && matches!(e.kind, OneShotKind::Flash { .. }))
        );
        assert!(
            d.strength < 0.3
                || show
                    .events
                    .iter()
                    .any(|e| matches!(e.kind, OneShotKind::Strobe { .. })
                        && (e.time - d.time).abs() < 1.0)
        );
        let c = show.cue_at(d.time + 0.01).unwrap();
        assert_eq!(c.effect, effect::STROBE);
    }
    // Brighter in intense sections than in calm ones.
    let bright_at = |t: f64| show.cue_at(t).unwrap().brightness;
    let intense = a
        .sections
        .iter()
        .find(|s| s.kind == lumigift_audio::SectionKind::Intense)
        .unwrap();
    let calm = a
        .sections
        .iter()
        .find(|s| s.kind == lumigift_audio::SectionKind::Calm)
        .unwrap();
    assert!(bright_at(intense.start + 5.0) > bright_at(calm.start + 1.0));
    // Cues sit on downbeats (except the outro fade).
    for c in show
        .cues
        .iter()
        .filter(|c| c.label != "outro" && c.time > 0.5)
    {
        assert!(
            (a.snap(c.time, Snap::Downbeat) - c.time).abs() < 0.6,
            "cue at {} off-grid",
            c.time
        );
    }
    // Deterministic.
    assert_eq!(Show::auto_generate(&a), show);
    // Engine runs over it without panicking.
    let mut e = ShowEngine::new(Arc::new(a.clone()), show);
    let mut t = 0.0;
    while t < a.duration {
        e.frame_at(t, 10);
        t += 0.01;
    }
}

/// A melody of harmonic tones: `(MIDI note, seconds)`, 20 ms fades.
fn melody(notes: &[(f32, f32)], sr: u32) -> Vec<f32> {
    let mut out = Vec::new();
    for &(midi, secs) in notes {
        let f0 = 440.0 * 2f32.powf((midi - 69.0) / 12.0);
        let n = (secs * sr as f32) as usize;
        let fade = (0.02 * sr as f32) as usize;
        for i in 0..n {
            let t = i as f32 / sr as f32;
            let env = (i.min(n - 1 - i) as f32 / fade as f32).min(1.0);
            let v: f32 = (1..=8)
                .map(|h| (std::f32::consts::TAU * f0 * h as f32 * t).sin() / h as f32)
                .sum();
            out.push(0.2 * env * v);
        }
    }
    out
}

fn solid_show(note_color: f32) -> Show {
    let mut show = Show {
        cues: vec![],
        ..Default::default()
    };
    show.settings.note_color = note_color;
    show.insert_cue(Cue {
        time: 0.0,
        effect: effect::SOLID,
        palette: palette::OCEAN,
        brightness: 200,
        hue: 0,
        ..Default::default()
    });
    show
}

#[test]
fn note_colour_follows_the_melody() {
    let sr = 44_100;
    // C4 E4 G4 B4 D5 A3, one second each.
    let notes = [(60.0, 1.0), (64.0, 1.0), (67.0, 1.0), (71.0, 1.0), (74.0, 1.0), (57.0, 1.0)];
    let a = Arc::new(Analysis::analyze(&melody(&notes, sr), sr));

    // Full note colour: each pitch class has its own hue (C = 0).
    let mut e = ShowEngine::new(a.clone(), solid_show(1.0));
    let mut t = 0.0;
    let mut hues = vec![];
    while t < 6.0 {
        let f = e.frame_at(t, 10);
        if (t.fract() - 0.7).abs() < 0.005 {
            hues.push(f.hue);
        }
        t += 0.01;
    }
    assert_eq!(hues.len(), notes.len());
    for (&(midi, _), &h) in notes.iter().zip(&hues) {
        let want = (midi.rem_euclid(12.0) * 256.0 / 12.0) as i32;
        let d = (h as i32 - want).rem_euclid(256).min((want - h as i32).rem_euclid(256));
        assert!(d <= 6, "MIDI {midi}: hue {h}, want {want}");
    }
    assert!((e.note().unwrap() - 9.0).abs() < 0.3, "last note {:?}", e.note());

    // Off: the cue's hue only.
    let mut e = ShowEngine::new(a.clone(), solid_show(0.0));
    let mut t = 0.0;
    while t < 6.0 {
        assert_eq!(e.frame_at(t, 10).hue, 0);
        t += 0.01;
    }
}

#[test]
fn each_note_swells_on_effects_that_ignore_loudness() {
    let sr = 44_100;
    // Same loudness throughout: only the note changes are events.
    let notes: Vec<(f32, f32)> = (0..8).map(|i| (60.0 + (i % 4) as f32 * 2.0, 0.75)).collect();
    let a = Arc::new(Analysis::analyze(&melody(&notes, sr), sr));
    let mut e = ShowEngine::new(a.clone(), solid_show(0.5));
    let mut b = vec![];
    let mut t = 0.0;
    while t < 6.0 {
        b.push(e.frame_at(t, 10).brightness as f32);
        t += 0.01;
    }
    // Brightness just after each note start vs. late in the note.
    let (mut early, mut late) = (0.0, 0.0);
    for k in 2..8 {
        let s = (k as f32 * 75.0) as usize;
        early += b[s + 4..s + 15].iter().copied().fold(0.0, f32::max);
        late += b[s + 55..s + 70].iter().sum::<f32>() / 15.0;
    }
    let (early, late) = (early / 6.0, late / 6.0);
    assert!(early > late * 1.12, "swell: early {early:.0} vs late {late:.0}");
}

#[test]
fn harmony_of_a_band_mix_moves_the_colour_at_chord_changes() {
    let sr = 44_100;
    // C, F, G, C major triads (root position, 6 harmonics), 2 s each, over drums.
    let chords = [[60.0f32, 64.0, 67.0], [65.0, 69.0, 72.0], [67.0, 71.0, 74.0], [60.0, 64.0, 67.0]];
    let mut x: Vec<f32> = Vec::new();
    for ch in &chords {
        let n = 2 * sr as usize;
        for i in 0..n {
            let t = i as f32 / sr as f32;
            let v: f32 = ch
                .iter()
                .map(|&m| {
                    let f0 = 440.0 * 2f32.powf((m - 69.0) / 12.0);
                    (1..=6)
                        .map(|h| (std::f32::consts::TAU * f0 * h as f32 * t).sin() / h as f32)
                        .sum::<f32>()
                })
                .sum();
            x.push(0.08 * v);
        }
    }
    let drums = synth::render(&[synth::Part { bass: false, ..synth::Part::groove(16.0, 120.0, 0.6) }], sr);
    for (a, d) in x.iter_mut().zip(&drums.samples) {
        *a += d;
    }
    let a = Arc::new(Analysis::analyze(&x, sr));
    let mut e = ShowEngine::new(a.clone(), solid_show(1.0));
    let mut hues: Vec<u8> = vec![];
    let mut t = 0.0;
    while t < 8.0 {
        hues.push(e.frame_at(t, 10).hue);
        t += 0.01;
    }
    let circ = |a: u8, b: u8| (a.wrapping_sub(b)).min(b.wrapping_sub(a)) as i32;
    for (k, ch) in chords.iter().enumerate() {
        // Second half of each chord: on the root's colour, and steady.
        let w = &hues[k * 200 + 100..k * 200 + 195];
        let want = ((ch[0] % 12.0) * 256.0 / 12.0) as u8;
        let mid = w[w.len() / 2];
        assert!(circ(mid, want) <= 12, "chord {k}: hue {mid}, root colour {want}");
        let spread = w.iter().map(|&h| circ(h, mid)).max().unwrap();
        assert!(spread <= 10, "chord {k}: hue wobbles by {spread}");
    }
}
