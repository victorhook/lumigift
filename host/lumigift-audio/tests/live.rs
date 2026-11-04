//! Causal live analysis.

use std::sync::Arc;
use std::time::{Duration, Instant};

use lumigift_audio::live::{
    LiveAnalysis, LiveFeatures, MemorySource, RealtimeAnalyzer, RealtimeFrame,
};
use lumigift_audio::synth::{self, Part};
use lumigift_audio::{FeatureProvider, Show, ShowEngine};
use lumigift_proto::music_flags as mf;

fn run(samples: &[f32], rate: u32) -> (RealtimeAnalyzer, Vec<RealtimeFrame>) {
    let mut an = RealtimeAnalyzer::new(rate);
    let mut out = Vec::new();
    // Feed in irregular block sizes like a sound card would.
    let mut i = 0;
    let mut k = 0;
    while i < samples.len() {
        let n = [256, 441, 1024, 333][k % 4].min(samples.len() - i);
        an.push(&samples[i..i + n], &mut out);
        i += n;
        k += 1;
    }
    (an, out)
}

#[test]
fn realtime_tempo_beats_and_features() {
    let t = synth::render(&[Part::groove(96.0, 124.0, 0.8)], 44_100);
    let (an, frames) = run(&t.samples, 44_100);
    assert!((an.fps() - 100.0).abs() < 0.01);
    let expected = t.samples.len() / 441;
    assert!(
        (frames.len() as i64 - expected as i64).abs() <= 6,
        "{} frames",
        frames.len()
    );
    assert!(frames.windows(2).all(|w| w[1].time > w[0].time));

    let bpm = an.bpm().expect("tempo locked");
    assert!((bpm - 124.0).abs() <= 2.0, "bpm {bpm}");

    // Beats predicted after the tracker has had 8 s to lock.
    let lock = 8.0;
    let beats: Vec<f64> = frames
        .iter()
        .filter(|f| f.events & mf::BEAT != 0 && f.time > lock)
        .map(|f| f.time)
        .collect();
    let truth: Vec<f64> = t
        .beats
        .iter()
        .copied()
        .filter(|&b| b > lock + 0.05 && b < t.beats.last().unwrap() - 0.05)
        .collect();
    let f = synth::f_measure(&beats, &truth, 0.07);
    eprintln!(
        "live beat F-measure after lock: {f:.3} ({} vs {})",
        beats.len(),
        truth.len()
    );
    assert!(f > 0.9, "F={f}");

    // Onsets on (at least) every kick.
    let onsets = frames.iter().filter(|f| f.events & mf::ONSET != 0).count();
    assert!(onsets >= t.beats.len() * 9 / 10, "{onsets} onsets");

    // Features span the range.
    let max_bass = frames.iter().map(|f| f.features.bass).fold(0.0, f32::max);
    let mean_level = frames.iter().map(|f| f.features.level).sum::<f32>() / frames.len() as f32;
    assert!(max_bass > 0.9, "max bass {max_bass}");
    assert!((0.2..0.9).contains(&mean_level), "mean level {mean_level}");
}

#[test]
fn realtime_detects_drop_and_tempo_change() {
    let t = synth::demo_track(44_100);
    let (_, frames) = run(&t.samples, 44_100);
    let drops: Vec<f64> = frames
        .iter()
        .filter(|f| f.events & mf::DROP != 0)
        .map(|f| f.time)
        .collect();
    eprintln!("live drops {drops:?} truth {:?}", t.drops);
    assert_eq!(drops.len(), 1, "{drops:?}");
    // Causal: reported within half a second after the real drop.
    assert!(
        drops[0] >= t.drops[0] - 0.05 && drops[0] <= t.drops[0] + 0.5,
        "{drops:?}"
    );
    let bpm_at = |time: f64| {
        frames
            .iter()
            .find(|f| f.time >= time)
            .and_then(|f| f.beat)
            .map(|b| b.bpm)
            .unwrap()
    };
    assert!((bpm_at(40.0) - 120.0).abs() <= 2.0, "{}", bpm_at(40.0));
    assert!((bpm_at(70.0) - 128.0).abs() <= 2.0, "{}", bpm_at(70.0));
}

#[test]
fn live_features_provider_reports_events_once() {
    let lf = LiveFeatures::new();
    let mk = |time: f64, events: u8| RealtimeFrame {
        time,
        features: Default::default(),
        events,
        beat: None,
    };
    lf.push(&[mk(0.00, 0), mk(0.01, mf::BEAT), mk(0.02, 0)]);
    assert_eq!(lf.events_in(-1.0, 0.005), 0);
    assert_eq!(lf.events_in(0.005, 0.02), mf::BEAT);
    assert_eq!(lf.events_in(0.02, 0.03), 0);
    // A frame arriving late (time already passed) is still reported.
    lf.push(&[mk(0.03, mf::ONSET)]);
    assert_eq!(lf.events_in(0.04, 0.05), mf::ONSET);
    assert!(lf.now() >= 0.03);
    assert_eq!(lf.duration(), None);
}

#[test]
fn live_analysis_end_to_end_with_engine() {
    let t = synth::render(&[Part::groove(32.0, 120.0, 0.8)], 44_100);
    let src = MemorySource::new(t.samples.clone(), 44_100, 1, 50.0);
    let live = LiveAnalysis::start(Box::new(src)).unwrap();
    let features: Arc<LiveFeatures> = live.features();
    let deadline = Instant::now() + Duration::from_secs(10);
    while features.latest().is_none_or(|f| f.time < 15.0) {
        assert!(Instant::now() < deadline, "live analysis did not finish");
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!((features.bpm().unwrap() - 120.0).abs() <= 2.0);
    let mut engine = ShowEngine::new(features.clone(), Show::default());
    let f = engine.frame_at(features.now(), 10);
    assert_eq!(f.bpm, 120);
    // Every beat event is delivered once to the engine.
    let mut engine2 = ShowEngine::new(features.clone(), Show::default());
    let beats = (0..3)
        .map(|_| engine2.frame_at(features.now(), 10))
        .filter(|f| f.flags & mf::BEAT != 0)
        .count();
    assert_eq!(beats, 0, "events were already consumed by the first engine");
}

#[test]
fn realtime_note_follows_a_melody() {
    let sr = 44_100;
    // G4 for 1.5 s, then D5 for 1.5 s (8 harmonics each).
    let tone = |midi: f32, secs: f32| -> Vec<f32> {
        let f0 = 440.0 * 2f32.powf((midi - 69.0) / 12.0);
        (0..(secs * sr as f32) as usize)
            .map(|i| {
                let t = i as f32 / sr as f32;
                0.2 * (1..=8)
                    .map(|h| (std::f32::consts::TAU * f0 * h as f32 * t).sin() / h as f32)
                    .sum::<f32>()
            })
            .collect()
    };
    let mut x = tone(67.0, 1.5);
    x.extend(tone(74.0, 1.5));
    let (_, frames) = run(&x, sr);
    let class_at = |t: f64| {
        let f = frames.iter().find(|f| f.time >= t).unwrap();
        (f.features.pitch_class().unwrap(), f.features.tone_clarity())
    };
    for (t, want) in [(1.2, 7.0f32), (2.7, 2.0)] {
        let (pc, clarity) = class_at(t);
        let d = (pc - want).rem_euclid(12.0);
        assert!(d.min(12.0 - d) < 0.4, "at {t} s: class {pc:.2}, want {want}");
        assert!(clarity > 0.45, "at {t} s: clarity {clarity:.2}");
    }
}
