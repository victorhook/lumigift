//! Measure how "eager" a light show is: renders a track through
//! `ShowEngine` + `lumigift_fx::Renderer` at 100 Hz and reports, per minute,
//! the BEAT and ONSET flags sent, how often a gift's brightness jumps by more
//! than 40 % within 50 ms, and the mean absolute frame-to-frame brightness
//! change.
//!
//! ```sh
//! cargo run -p lumigift-audio --release --example eagerness -- song.mp3
//! cargo run -p lumigift-audio --release --example eagerness -- --synth    # rhythmic demo track
//! cargo run -p lumigift-audio --release --example eagerness -- --legato   # rubato legato melody
//! ```

use std::sync::Arc;

use lumigift_audio::eagerness::{Eagerness, measure};
use lumigift_audio::{Analysis, Show, decode_file, synth};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let (name, analysis) = if args.iter().any(|a| a == "--legato") {
        let t = synth::legato_track(44_100, 90.0, 7);
        (
            "synthetic legato melody".to_string(),
            Analysis::analyze(&t.samples, t.sample_rate),
        )
    } else if args.iter().any(|a| a == "--synth") || args.is_empty() {
        let t = synth::demo_track(44_100);
        (
            "synthetic demo track".to_string(),
            Analysis::analyze(&t.samples, t.sample_rate),
        )
    } else {
        let path = args.iter().find(|a| !a.starts_with("--")).unwrap().clone();
        let audio = decode_file(&path).unwrap_or_else(|e| {
            eprintln!("error: {e}");
            std::process::exit(1);
        });
        (path, Analysis::analyze(&audio.mono(), audio.sample_rate))
    };
    let a = Arc::new(analysis);
    println!("{name}: {:.1} s", a.duration);
    println!(
        "  analysis: {:.1} BPM, {} beats, rhythm confidence {:.2}",
        a.bpm,
        a.beats.len(),
        a.rhythm_confidence,
    );
    print!("  onsets flagged at sensitivity");
    for s in [0.0f32, 0.25, 0.5, 0.75, 1.0] {
        let c = a.onsets_at(s).count();
        print!("  {s}: {c} ({:.1}/s)", c as f64 / a.duration.max(1e-3));
    }
    println!();
    let auto = Show::auto_generate(&a);
    if args.iter().any(|a| a == "--cues") {
        for c in &auto.cues {
            println!(
                "    cue {:6.1}s  {:<9} brightness {:3}  fade {:.1}  {}",
                c.time,
                lumigift_fx::EFFECT_NAMES
                    .get(c.effect as usize)
                    .copied()
                    .unwrap_or("?"),
                c.brightness,
                c.fade,
                c.label
            );
        }
        println!("    {} one-shots", auto.events.len());
    }
    print_row("auto show", &measure(a.clone(), auto));
    // Fixed looks isolate the engine from the auto generator's choices.
    for (label, fx) in [
        ("Pulse", lumigift_fx::effect::PULSE),
        ("Sparkle", lumigift_fx::effect::SPARKLE),
        ("Spectrum", lumigift_fx::effect::SPECTRUM),
    ] {
        let mut s = Show::default();
        s.cues[0].effect = fx;
        print_row(label, &measure(a.clone(), s));
    }
}

fn print_row(label: &str, e: &Eagerness) {
    println!(
        "  {label:<10} BEAT {:6.1}/min  ONSET {:6.1}/min  jumps>40%/50ms {:6.1}/min/gift  mean |Δbrightness| {:5.2} %/frame  r(brightness, loudness) {:+.2}",
        e.beats_per_min,
        e.onsets_per_min,
        e.jumps_per_min,
        e.mean_abs_delta_pct,
        e.dynamics_correlation
    );
}
