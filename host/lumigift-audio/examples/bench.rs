//! Analyse synthetic tracks with known ground truth and report accuracy and
//! speed. Run in release mode:
//!
//! ```sh
//! cargo run -p lumigift-audio --release --example bench
//! ```

use lumigift_audio::Analysis;
use lumigift_audio::synth::{self, Part};

fn report(name: &str, track: &synth::SynthTrack) -> Analysis {
    let a = Analysis::analyze(&track.samples, track.sample_rate);
    let beats: Vec<f64> = a.beats.iter().map(|b| b.time).collect();
    let downs: Vec<f64> = a.downbeats().map(|b| b.time).collect();
    let f = synth::f_measure(&beats, &track.beats, 0.07);
    let fd = synth::f_measure(&downs, &track.downbeats, 0.07);
    // Mean signed beat offset (detected - truth) for matched beats.
    let mut offs = Vec::new();
    for &b in &beats {
        if let Some(t) = track
            .beats
            .iter()
            .copied()
            .min_by(|x, y| (x - b).abs().total_cmp(&(y - b).abs()))
            && (t - b).abs() < 0.07
        {
            offs.push(b - t);
        }
    }
    let mean_off = offs.iter().sum::<f64>() / offs.len().max(1) as f64;
    println!(
        "{name}: {:.1} s audio analysed in {:.3} s | {:.2} BPM | beats {} (truth {}) F={:.3} | downbeat F={:.3} | mean offset {:+.1} ms",
        a.duration,
        a.analysis_seconds,
        a.bpm,
        beats.len(),
        track.beats.len(),
        f,
        fd,
        mean_off * 1000.0
    );
    println!(
        "  drops: {:?} (truth {:?})",
        a.drops
            .iter()
            .map(|d| format!("{:.2}", d.time))
            .collect::<Vec<_>>(),
        track.drops
    );
    for s in &a.sections {
        println!(
            "  section {:7.2}–{:7.2}  {:?} energy {:.2}{}",
            s.start,
            s.end,
            s.kind,
            s.energy,
            if s.starts_with_drop { " DROP" } else { "" }
        );
    }
    a
}

fn main() {
    let demo = synth::demo_track(44_100);
    let a = report("demo", &demo);
    for t in [20.0, 60.0, 80.0, 110.0] {
        println!("  tempo at {t:5.1} s: {:.2}", a.tempo_at(t));
    }

    // A 4-minute track for timing.
    let mut parts = Vec::new();
    parts.push(Part::silence(1.0));
    for i in 0..7 {
        parts.push(Part::groove(32.0, 124.0, 0.4));
        parts.push(Part {
            kick: false,
            snare: false,
            ..Part::groove(16.0, 124.0, 0.4)
        });
        parts.push(Part {
            bass: true,
            ..Part::groove(if i % 2 == 0 { 16.0 } else { 20.0 }, 124.0, 1.0)
        });
    }
    let long = synth::render(&parts, 44_100);
    report("4-minute", &long);
}
