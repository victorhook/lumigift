//! Algorithm accuracy on synthetic signals with known ground truth.

use lumigift_audio::synth::{self, Part};
use lumigift_audio::{Analysis, FeatureProvider, SectionKind, Snap};

fn beat_times(a: &Analysis) -> Vec<f64> {
    a.beats.iter().map(|b| b.time).collect()
}

#[test]
fn steady_120_bpm_tempo_and_beats() {
    let t = synth::render(&[Part::groove(64.0, 120.0, 0.8)], 44_100);
    let a = Analysis::analyze(&t.samples, t.sample_rate);
    assert!((a.bpm - 120.0).abs() <= 2.0, "bpm {}", a.bpm);
    let f = synth::f_measure(&beat_times(&a), &t.beats, 0.07);
    assert!(f > 0.9, "F={f}");
    // Timing accuracy: matched beats within 15 ms.
    for b in beat_times(&a) {
        let d = t
            .beats
            .iter()
            .map(|x| (x - b).abs())
            .fold(f64::MAX, f64::min);
        assert!(d < 0.015, "beat {b:.3} off by {:.1} ms", d * 1000.0);
    }
}

#[test]
fn demo_track_tempo_change_silence_and_drop() {
    let t = synth::demo_track(44_100);
    let a = Analysis::analyze(&t.samples, t.sample_rate);

    // Tempo per part: 120 BPM until ~58 s, then 128 BPM.
    assert!(
        (a.tempo_at(20.0) - 120.0).abs() <= 2.0,
        "tempo at 20 s: {}",
        a.tempo_at(20.0)
    );
    assert!(
        (a.tempo_at(40.0) - 120.0).abs() <= 2.0,
        "tempo at 40 s: {}",
        a.tempo_at(40.0)
    );
    assert!(
        (a.tempo_at(75.0) - 128.0).abs() <= 2.0,
        "tempo at 75 s: {}",
        a.tempo_at(75.0)
    );

    // Beats.
    let f = synth::f_measure(&beat_times(&a), &t.beats, 0.07);
    assert!(f > 0.9, "beat F-measure {f}");
    let downs: Vec<f64> = a.downbeats().map(|b| b.time).collect();
    let fd = synth::f_measure(&downs, &t.downbeats, 0.07);
    assert!(fd > 0.9, "downbeat F-measure {fd}");

    // Leading silence: no beats or onsets, SILENT features.
    assert!(a.beats[0].time > 1.9, "first beat {}", a.beats[0].time);
    assert!(a.onsets[0].time > 1.9);
    assert!(a.features_at(1.0).is_silent());

    // Exactly one drop, within ±0.5 s of the truth.
    assert_eq!(t.drops.len(), 1);
    assert_eq!(a.drops.len(), 1, "drops: {:?}", a.drops);
    assert!(
        (a.drops[0].time - t.drops[0]).abs() <= 0.5,
        "drop at {} vs {}",
        a.drops[0].time,
        t.drops[0]
    );

    // Sections: a boundary at the build-up (18 s) and at the drop (26 s); the
    // drop section is the most intense.
    let starts: Vec<f64> = a.sections.iter().map(|s| s.start).collect();
    for want in [18.0, 26.0] {
        assert!(
            starts.iter().any(|s| (s - want).abs() < 0.6),
            "no boundary near {want}: {starts:?}"
        );
    }
    let (_, drop_sec) = a.section_at(30.0).unwrap();
    assert!(drop_sec.starts_with_drop);
    assert_eq!(drop_sec.kind, SectionKind::Intense);
    assert!(
        a.sections
            .iter()
            .all(|s| s.energy <= drop_sec.energy + 1e-6)
    );
    let (_, quiet) = a.section_at(10.0).unwrap();
    assert!(quiet.energy < drop_sec.energy);
}

#[test]
fn tempo_ramp_is_followed() {
    let ramp = Part {
        bpm: (110.0, 135.0),
        ..Part::groove(96.0, 110.0, 0.8)
    };
    let t = synth::render(
        &[
            Part::groove(16.0, 110.0, 0.8),
            ramp,
            Part::groove(32.0, 135.0, 0.8),
        ],
        44_100,
    );
    let a = Analysis::analyze(&t.samples, t.sample_rate);
    let f = synth::f_measure(&beat_times(&a), &t.beats, 0.07);
    assert!(f > 0.9, "F={f}");
    let end = a.duration - 5.0;
    assert!(
        (a.tempo_at(end) - 135.0).abs() <= 2.0,
        "tempo at end {}",
        a.tempo_at(end)
    );
}

#[test]
fn octave_errors_are_avoided() {
    // 170 BPM with hats on eighths and 75 BPM half-time grooves.
    for bpm in [75.0, 95.0, 140.0, 170.0] {
        let t = synth::render(&[Part::groove(48.0, bpm, 0.8)], 44_100);
        let a = Analysis::analyze(&t.samples, t.sample_rate);
        assert!(
            (a.bpm as f64 - bpm).abs() <= 2.0,
            "{bpm} BPM detected as {}",
            a.bpm
        );
    }
}

#[test]
fn normalisation_is_level_independent() {
    let loud = synth::render(&[Part::groove(32.0, 120.0, 1.0)], 44_100);
    let quiet: Vec<f32> = loud.samples.iter().map(|s| s * 0.03).collect(); // -30 dB
    let al = Analysis::analyze(&loud.samples, 44_100);
    let aq = Analysis::analyze(&quiet, 44_100);
    let mean = |a: &Analysis, f: fn(&lumigift_audio::FeatureFrame) -> f32| {
        a.frames.iter().map(f).sum::<f32>() / a.frames.len() as f32
    };
    for (name, get) in [
        (
            "bass",
            (|f: &lumigift_audio::FeatureFrame| f.bass) as fn(&_) -> f32,
        ),
        ("level", |f| f.level),
        ("treble", |f| f.treble),
    ] {
        let (l, q) = (mean(&al, get), mean(&aq, get));
        assert!((l - q).abs() < 0.05, "{name}: loud {l} vs quiet {q}");
    }
    let max_level = aq.frames.iter().map(|f| f.level).fold(0.0, f32::max);
    assert!(
        max_level > 0.95,
        "quiet track should still reach full range, max {max_level}"
    );
    assert_eq!(al.beats.len(), aq.beats.len());
}

#[test]
fn works_at_48k_and_with_stereo_mixdown() {
    let t = synth::render(&[Part::groove(48.0, 126.0, 0.8)], 48_000);
    let a = Analysis::analyze(&t.samples, 48_000);
    assert!((a.fps - 100.0).abs() < 0.01);
    assert!((a.bpm - 126.0).abs() <= 2.0, "bpm {}", a.bpm);
    let stereo: Vec<f32> = t.samples.iter().flat_map(|&s| [s, s]).collect();
    let audio = lumigift_audio::DecodedAudio::from_samples(stereo, 48_000, 2);
    assert_eq!(audio.mono(), t.samples);
}

#[test]
fn silence_and_empty_input_do_not_panic() {
    let a = Analysis::analyze(&[], 44_100);
    assert!(a.beats.is_empty() && a.frames.is_empty());
    assert_eq!(a.beat_at(1.0), None);
    let s = vec![0f32; 44_100 * 5];
    let a = Analysis::analyze(&s, 44_100);
    assert!(a.beats.is_empty() && a.onsets.is_empty() && a.drops.is_empty());
    assert_eq!(a.sections.len(), 1);
    assert_eq!(a.sections[0].kind, SectionKind::Silence);
    let short = vec![0.1f32; 100];
    let _ = Analysis::analyze(&short, 44_100);
}

#[test]
fn lookups_and_snapping() {
    let t = synth::render(&[Part::groove(32.0, 120.0, 0.8)], 44_100);
    let a = Analysis::analyze(&t.samples, 44_100);
    let b5 = a.beats[5].time;
    assert_eq!(a.snap(b5 + 0.1, Snap::Beat), b5);
    let d = a.snap(b5, Snap::Downbeat);
    assert!(a.beats.iter().any(|b| b.time == d && b.is_downbeat()));
    let half = a.snap(b5 + 0.24, Snap::HalfBeat);
    assert!((half - (b5 + 0.25)).abs() < 0.02, "{half}");
    assert_eq!(a.snap(3.21, Snap::Off), 3.21);

    let p = a.beat_position(b5 + 0.125).unwrap();
    assert_eq!(p.beat_index, 5);
    assert!((p.phase - 0.25).abs() < 0.05, "{}", p.phase);
    assert_eq!(p.beat_in_bar, a.beats[5].beat_in_bar);
    // Before the first beat: extrapolated with negative index.
    let p0 = a.beat_position(a.beats[0].time - 0.1).unwrap();
    assert_eq!(p0.beat_index, -1);

    let ev = a.events_in(b5 - 0.005, b5 + 0.005);
    assert_ne!(ev & lumigift_proto::music_flags::BEAT, 0);
    assert_eq!(
        a.events_in(b5 + 0.005, b5 + 0.1) & lumigift_proto::music_flags::BEAT,
        0
    );

    // Waveform / spectrogram helpers.
    let w = a.waveform.buckets_in(0.0, a.duration, 800);
    assert_eq!(w.len(), 800);
    assert!(w.iter().any(|b| b.max > 0.1));
    assert!(a.spectrogram.column_at(1.0).is_some());
    assert!((a.spectrogram.columns_per_second - 50.0).abs() < 0.1);
}

#[test]
fn analysis_of_four_minutes_is_fast() {
    // Debug builds are much slower; the release-mode number is in examples/bench.rs.
    let mut parts = Vec::new();
    for _ in 0..8 {
        parts.push(Part::groove(32.0, 124.0, 0.5));
        parts.push(Part {
            bass: true,
            ..Part::groove(32.0, 124.0, 1.0)
        });
    }
    let t = synth::render(&parts, 44_100);
    assert!(t.samples.len() as f64 / 44_100.0 > 240.0);
    let a = Analysis::analyze(&t.samples, 44_100);
    let limit = if cfg!(debug_assertions) { 30.0 } else { 2.0 };
    assert!(a.analysis_seconds < limit, "took {} s", a.analysis_seconds);
    assert!((a.bpm - 124.0).abs() <= 2.0);
}

/// A tone with `n` harmonics (amplitude 1/h), optional vibrato (semitones).
fn harmonic_tone(f0: f32, secs: f32, sr: u32, n: usize, vibrato_st: f32) -> Vec<f32> {
    let len = (secs * sr as f32) as usize;
    let mut phase = vec![0f64; n];
    (0..len)
        .map(|i| {
            let t = i as f32 / sr as f32;
            let f = f0 * 2f32.powf(vibrato_st * (std::f32::consts::TAU * 5.5 * t).sin() / 12.0);
            let mut v = 0.0;
            for (h, ph) in phase.iter_mut().enumerate() {
                let h = h + 1;
                *ph += std::f64::consts::TAU * (f as f64 * h as f64) / sr as f64;
                v += ph.sin() as f32 / h as f32;
            }
            0.2 * v
        })
        .collect()
}

/// Median pitch class and clarity over the middle of a signal.
fn tone_of(samples: &[f32], sr: u32) -> (Option<f32>, f32) {
    let a = Analysis::analyze(samples, sr);
    let mid = &a.frames[a.frames.len() / 4..3 * a.frames.len() / 4];
    let mut cl: Vec<f32> = mid.iter().map(|f| f.tone_clarity()).collect();
    cl.sort_by(f32::total_cmp);
    let mut pcs: Vec<f32> = mid.iter().filter_map(|f| f.pitch_class()).collect();
    pcs.sort_by(f32::total_cmp);
    (pcs.get(pcs.len() / 2).copied(), cl[cl.len() / 2])
}

fn pc_err(a: f32, b: f32) -> f32 {
    let d = (a - b).rem_euclid(12.0);
    d.min(12.0 - d)
}

#[test]
fn pitch_class_of_tones() {
    let sr = 44_100;
    // (MIDI note, harmonics, vibrato)
    for &(midi, n, vib) in &[
        (43.0f32, 8, 0.0),  // G2, low
        (55.0, 8, 0.0),     // G3, violin's lowest string
        (60.0, 1, 0.0),     // C4, pure sine
        (64.0, 10, 0.25),   // E4 with violin-like vibrato
        (69.0, 6, 0.0),     // A4
        (73.0, 8, 0.2),     // C#5
        (83.0, 4, 0.0),     // B5
    ] {
        let f0 = 440.0 * 2f32.powf((midi - 69.0) / 12.0);
        let (pc, clarity) = tone_of(&harmonic_tone(f0, 3.0, sr, n, vib), sr);
        let pc = pc.expect("a tone has a pitch");
        let want = midi.rem_euclid(12.0);
        assert!(pc_err(pc, want) < 0.35, "MIDI {midi}: got class {pc:.2}, want {want}");
        assert!(clarity > 0.5, "MIDI {midi}: clarity {clarity:.2}");
    }
}

#[test]
fn noise_and_drums_are_not_pitched() {
    let sr = 44_100;
    let mut x = 12345u32;
    let noise: Vec<f32> = (0..sr as usize * 3)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 17;
            x ^= x << 5;
            (x as f32 / u32::MAX as f32 - 0.5) * 0.4
        })
        .collect();
    let (_, clarity) = tone_of(&noise, sr);
    assert!(clarity < 0.2, "white noise clarity {clarity:.2}");

    let drums = synth::render(&[Part { bass: false, ..Part::groove(12.0, 120.0, 0.8) }], sr);
    let (_, clarity) = tone_of(&drums.samples, sr);
    assert!(clarity < 0.35, "drums clarity {clarity:.2}");
}
