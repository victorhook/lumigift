//! Music sync quality: rhythm confidence, onset selection, reactivity
//! settings and the "eagerness" of the rendered lights.

use std::sync::Arc;

use lumigift_audio::eagerness::measure;
use lumigift_audio::engine::{EngineCursor, RHYTHM_THRESHOLD};
use lumigift_audio::synth::{self, Part};
use lumigift_audio::{Analysis, BeatSync, OneShotKind, Show, ShowEngine};
use lumigift_fx::effect;
use lumigift_proto::music_flags as mf;

fn legato() -> (synth::LegatoTrack, Arc<Analysis>) {
    let t = synth::legato_track(44_100, 90.0, 7);
    let a = Arc::new(Analysis::analyze(&t.samples, t.sample_rate));
    (t, a)
}

fn demo() -> (synth::SynthTrack, Arc<Analysis>) {
    let t = synth::demo_track(44_100);
    let a = Arc::new(Analysis::analyze(&t.samples, t.sample_rate));
    (t, a)
}

fn show_with(f: impl FnOnce(&mut Show)) -> Show {
    let mut s = Show::default();
    f(&mut s);
    s
}

/// Count frames with `flag` and the set of `bpm` values, at 100 Hz.
fn run(a: &Arc<Analysis>, show: Show) -> (usize, usize, Vec<u8>) {
    let mut e = ShowEngine::new(a.clone(), show);
    let (mut beats, mut onsets, mut bpms) = (0, 0, Vec::new());
    let n = (a.duration * 100.0) as usize;
    for i in 0..n {
        let f = e.frame_at(i as f64 / 100.0, 10);
        beats += (f.flags & mf::BEAT != 0) as usize;
        onsets += (f.flags & mf::ONSET != 0) as usize;
        if !bpms.contains(&f.bpm) {
            bpms.push(f.bpm);
        }
    }
    (beats, onsets, bpms)
}

#[test]
fn rhythm_confidence_separates_pulse_from_rubato() {
    for bpm in [75.0, 120.0, 170.0] {
        let t = synth::render(&[Part::groove(48.0, bpm, 0.8)], 44_100);
        let a = Analysis::analyze(&t.samples, t.sample_rate);
        assert!(
            a.rhythm_confidence > 0.8,
            "{bpm} BPM groove: confidence {}",
            a.rhythm_confidence
        );
    }
    let ramp = Part {
        bpm: (110.0, 135.0),
        ..Part::groove(96.0, 110.0, 0.8)
    };
    let t = synth::render(&[Part::groove(16.0, 110.0, 0.8), ramp], 44_100);
    let a = Analysis::analyze(&t.samples, t.sample_rate);
    assert!(a.rhythm_confidence > 0.8, "ramp: {}", a.rhythm_confidence);

    // The demo track has a pulse wherever it has beats (build-up and quiet
    // outro included).
    let (_, d) = demo();
    assert!(d.rhythm_confidence > 0.9, "demo: {}", d.rhythm_confidence);
    for b in &d.beats {
        assert!(
            d.rhythm_at(b.time) >= RHYTHM_THRESHOLD,
            "demo: confidence {} at beat {:.2}",
            d.rhythm_at(b.time),
            b.time
        );
    }
    for s in d
        .sections
        .iter()
        .filter(|s| s.duration() > 4.0 && s.energy > 0.0)
    {
        assert!(s.rhythm >= RHYTHM_THRESHOLD, "section {s:?}");
    }

    // Rubato legato melody: the beat tracker still invents a grid, but the
    // confidence says it is not real.
    let (_, l) = legato();
    assert!(!l.beats.is_empty());
    assert!(l.rhythm_confidence < 0.2, "legato: {}", l.rhythm_confidence);
    let max = l.rhythm.iter().copied().fold(0.0, f32::max);
    assert!(max < RHYTHM_THRESHOLD, "legato max {max}");
}

#[test]
fn onsets_follow_notes_not_vibrato() {
    let (t, a) = legato();
    let on: Vec<f64> = a.onsets_at(0.5).map(|o| o.time).collect();
    let ratio = on.len() as f64 / t.notes.len() as f64;
    assert!(
        (0.7..=1.3).contains(&ratio),
        "{} onsets for {} notes",
        on.len(),
        t.notes.len()
    );
    // Soft bowed attacks peak ~60 ms after the note starts.
    let f = synth::f_measure(&on, &t.notes, 0.1);
    assert!(f > 0.85, "note F-measure {f}");
    // Even the most sensitive setting does not fire on every vibrato cycle
    // (5.5 Hz over 90 s would be ~500).
    assert!(a.onsets.len() < 2 * t.notes.len(), "{}", a.onsets.len());
}

#[test]
fn drum_hits_are_flagged() {
    let (t, a) = demo();
    for (sensitivity, min_recall) in [(0.5, 0.98), (0.0, 0.9)] {
        let on: Vec<f64> = a.onsets_at(sensitivity).map(|o| o.time).collect();
        let hit = t
            .beats
            .iter()
            .filter(|&&b| on.iter().any(|&o| (o - b).abs() < 0.03))
            .count();
        let recall = hit as f64 / t.beats.len() as f64;
        assert!(
            recall >= min_recall,
            "sensitivity {sensitivity}: recall {recall}"
        );
    }
    // Hats on eighth notes are hits too.
    assert!(a.onsets_at(0.5).count() as f64 > 1.8 * t.beats.len() as f64);
}

#[test]
fn sensitivity_moves_the_onset_threshold() {
    let (_, a) = legato();
    let counts: Vec<usize> = [0.0, 0.25, 0.5, 0.75, 1.0]
        .iter()
        .map(|&s| a.onsets_at(s).count())
        .collect();
    assert!(counts.windows(2).all(|w| w[0] <= w[1]), "{counts:?}");
    assert!(counts[0] < counts[4]);
    assert_eq!(counts[4], a.onsets.len());
    // The engine flags exactly those.
    for s in [0.0f32, 0.5, 1.0] {
        let (_, onsets, _) = run(&a, show_with(|x| x.settings.sensitivity = s));
        let want = a.onsets_at(s).count();
        // Two onsets can share a 10 ms frame (never closer than 90 ms here).
        assert_eq!(onsets, want, "sensitivity {s}");
    }
}

#[test]
fn beat_sync_modes() {
    let (_, l) = legato();
    let (beats, _, bpms) = run(&l, Show::default());
    assert_eq!(beats, 0, "no fake beats on rubato music");
    assert_eq!(
        bpms,
        vec![0],
        "bpm 0 keeps the gifts' phase-wrap detection off"
    );
    let (beats, _, _) = run(&l, show_with(|s| s.settings.beat_sync = BeatSync::On));
    assert_eq!(beats, l.beats.len());

    let (_, d) = demo();
    let (beats, _, _) = run(&d, Show::default());
    assert_eq!(
        beats,
        d.beats.len(),
        "Auto keeps every beat of rhythmic music"
    );
    let (beats, _, bpms) = run(&d, show_with(|s| s.settings.beat_sync = BeatSync::Off));
    assert_eq!((beats, bpms), (0, vec![0]));

    // Without sync, beat_phase runs freely and slowly (Chase still moves).
    let e = ShowEngine::new(l.clone(), Show::default());
    let p0 = e.peek_frame(20.0, 10).beat_phase;
    let p1 = e.peek_frame(21.0, 10).beat_phase;
    assert_eq!(p1.wrapping_sub(p0), 64, "{p0} → {p1}");
    assert!(!e.beat_sync_active(20.0));
}

#[test]
fn eagerness_rhythmic_stays_crisp_legato_becomes_calm() {
    let (_, d) = demo();
    let rhythmic = measure(d.clone(), Show::auto_generate(&d));
    assert!(rhythmic.beats_per_min >= 110.0, "{rhythmic:?}");
    assert!(rhythmic.jumps_per_min >= 60.0, "{rhythmic:?}");
    let pulse = measure(d.clone(), show_with(|s| s.cues[0].effect = effect::PULSE));
    assert!(pulse.jumps_per_min >= 80.0, "{pulse:?}");

    let (_, l) = legato();
    let calm = measure(l.clone(), Show::auto_generate(&l));
    assert_eq!(calm.beats_per_min, 0.0, "{calm:?}");
    assert!(calm.onsets_per_min < 90.0, "{calm:?}");
    assert!(calm.jumps_per_min < 2.0, "{calm:?}");
    assert!(calm.mean_abs_delta_pct < 0.5, "{calm:?}");
    // Calm, not frozen: brightness follows the phrase dynamics.
    assert!(calm.dynamics_correlation > 0.4, "{calm:?}");
}

#[test]
fn smoothing_and_intensity() {
    let (_, l) = legato();
    let level_motion = |show: Show| {
        let mut e = ShowEngine::new(l.clone(), show);
        let mut prev = None;
        let mut sum = 0u64;
        for i in 0..6000 {
            let f = e.frame_at(10.0 + i as f64 / 100.0, 10);
            if let Some(p) = prev {
                sum += (f.level as i32 - p as i32).unsigned_abs() as u64;
            }
            prev = Some(f.level);
        }
        sum
    };
    let raw = level_motion(show_with(|s| s.settings.smoothing = 0.0));
    let def = level_motion(Show::default());
    let smooth = level_motion(show_with(|s| s.settings.smoothing = 1.0));
    assert!(raw > def && def > smooth, "{raw} {def} {smooth}");
    let flat = level_motion(show_with(|s| s.settings.intensity = 0.0));
    let wild = level_motion(show_with(|s| s.settings.intensity = 1.0));
    assert!(flat < def && def < wild, "{flat} {def} {wild}");

    // Breathe follows the phrase envelope through its brightness.
    let show = show_with(|s| s.cues[0].effect = effect::BREATHE);
    let e = ShowEngine::new(l.clone(), show);
    let b: Vec<u8> = (0..80)
        .map(|i| e.peek_frame(5.0 + i as f64, 10).brightness)
        .collect();
    let (lo, hi) = (b.iter().min().unwrap(), b.iter().max().unwrap());
    assert!(hi - lo > 40, "brightness range {lo}..{hi}");
    let mut off = Show::default();
    off.cues[0].effect = effect::BREATHE;
    off.settings.intensity = 0.0;
    let e = ShowEngine::new(l.clone(), off);
    assert_eq!(
        e.peek_frame(30.0, 10).brightness,
        200,
        "intensity 0: cue brightness"
    );
}

#[test]
fn engine_is_deterministic_and_call_rate_independent() {
    let (_, l) = legato();
    let show = show_with(|s| {
        s.settings.smoothing = 0.8;
        s.settings.intensity = 0.7;
    });
    let mut e1 = ShowEngine::new(l.clone(), show.clone());
    let mut e2 = ShowEngine::new(l.clone(), show.clone());
    for i in 0..3000 {
        let t = 3.0 + i as f64 * 0.0137;
        assert_eq!(e1.frame_at(t, 10), e2.frame_at(t, 10));
    }
    // Envelopes step on a fixed grid: 60 Hz and 100 Hz callers agree.
    let mut a = ShowEngine::new(l.clone(), show.clone());
    let mut b = ShowEngine::new(l.clone(), show.clone());
    let mut t = 10.0;
    while t < 20.0 {
        a.frame_at(t, 10);
        t += 1.0 / 60.0;
    }
    let mut t = 10.0;
    while t < 20.0 {
        b.frame_at(t, 10);
        t += 0.01;
    }
    let (fa, fb) = (a.frame_at(20.0, 10), b.frame_at(20.0, 10));
    assert_eq!((fa.bass, fa.mid, fa.level), (fb.bass, fb.mid, fb.level));

    // A caller-owned cursor gives the same frames as the engine's own one.
    let mut own = ShowEngine::new(l.clone(), show.clone());
    let shared = ShowEngine::new(l.clone(), show);
    let mut cursor = EngineCursor::new();
    for i in 0..2000 {
        let t = 30.0 + i as f64 * 0.01;
        assert_eq!(
            own.frame_at(t, 10),
            shared.peek_frame_at(&mut cursor, t, 10)
        );
    }
}

#[test]
fn auto_show_for_legato_is_calm() {
    let (_, l) = legato();
    let show = Show::auto_generate(&l);
    assert!(show.events.is_empty(), "no one-shots: {:?}", show.events);
    for c in &show.cues {
        assert!(
            ![
                effect::STROBE,
                effect::PULSE,
                effect::CHASE,
                effect::BLINK,
                effect::STROBE_TIMED,
                effect::POLICE,
            ]
            .contains(&c.effect),
            "beat-driven effect {} at {:.1}",
            c.effect,
            c.time
        );
        assert!(c.effect < effect::ALL);
        if c.label != "outro" {
            assert!(c.fade >= 2.0, "cue at {:.1} fades {}", c.time, c.fade);
        }
    }
    assert!(show.cues.len() >= 3);

    // Rhythmic material keeps its energetic, beat-synced looks.
    let (_, d) = demo();
    let show = Show::auto_generate(&d);
    assert!(show.cues.iter().any(|c| c.effect == effect::STROBE));
    assert!(
        show.events
            .iter()
            .any(|e| matches!(e.kind, OneShotKind::Flash { .. }))
    );
}

#[test]
fn reactivity_settings_in_show_files() {
    // Files written before the reactivity settings load with the defaults.
    let old = r#"{"version":1,"track":{"path":"a.mp3","content_hash":"","title":"A"},
        "cues":[{"time":0,"effect":2,"palette":1,"brightness":200,"hue":5}],
        "settings":{"master_brightness":200,"beat_flags":true,"onset_flags":true,"drop_flags":false}}"#;
    let s = Show::from_json(old).unwrap();
    assert_eq!(s.settings.master_brightness, 200);
    assert!(!s.settings.drop_flags);
    assert!(s.settings.reactivity_is_default());
    assert_eq!(s.settings.beat_sync, BeatSync::Auto);
    assert_eq!(s.settings.sensitivity, 0.5);

    let mut s = s;
    s.settings.beat_sync = BeatSync::Off;
    s.settings.smoothing = 0.9;
    let json = s.to_json();
    assert!(json.contains("\"beat_sync\": \"off\""), "{json}");
    assert_eq!(Show::from_json(&json).unwrap(), s);
    s.settings.reset_reactivity();
    assert!(s.settings.reactivity_is_default());
}
