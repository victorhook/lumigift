//! Analyse an audio file and print tempo, rhythm confidence, beats, onsets,
//! sections and drops.
//!
//! ```sh
//! cargo run -p lumigift-audio --release --example analyze -- song.mp3 [--cache] [--show out.lgshow]
//! ```
//!
//! `--cache` uses (and fills) the on-disk analysis cache; `--show` writes the
//! auto-generated light show.

use std::time::Instant;

use lumigift_audio::engine::RHYTHM_THRESHOLD;
use lumigift_audio::{AnalysisCache, Show, ShowSettings, decode_file, load_track};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let Some(path) = args.iter().find(|a| !a.starts_with("--")).cloned() else {
        eprintln!("usage: analyze <file> [--cache] [--show out.lgshow]");
        std::process::exit(2);
    };
    let use_cache = args.iter().any(|a| a == "--cache");
    let show_out = args
        .iter()
        .position(|a| a == "--show")
        .and_then(|i| args.get(i + 1))
        .cloned();

    // Decode once on its own to report the split between decoding and analysis.
    let t0 = Instant::now();
    let audio = match decode_file(&path) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("error: {e}");
            std::process::exit(1);
        }
    };
    let decode_s = t0.elapsed().as_secs_f64();
    let m = &audio.metadata;
    println!("File:      {path}");
    println!(
        "Track:     {} — {}{}",
        m.artist.as_deref().unwrap_or("?"),
        m.title.as_deref().unwrap_or("?"),
        m.album
            .as_deref()
            .map(|a| format!(" ({a})"))
            .unwrap_or_default()
    );
    println!(
        "Format:    {} Hz, {} ch, {:.1} s{}",
        audio.sample_rate,
        audio.channels,
        audio.duration(),
        m.cover_art
            .as_ref()
            .map(|c| format!(", cover art {} bytes", c.data.len()))
            .unwrap_or_default()
    );
    println!("Decoded in {decode_s:.3} s");

    // The normal path the GUI uses: background job (+ cache).
    let cache = if use_cache {
        AnalysisCache::default_location()
    } else {
        None
    };
    let t1 = Instant::now();
    let track = match load_track(&path, cache).wait() {
        Ok(t) => t,
        Err(e) => {
            eprintln!("error: {e}");
            std::process::exit(1);
        }
    };
    let a = &track.analysis;
    println!(
        "Analysis:  {:.3} s{} (load_track total incl. decode {:.3} s)",
        a.analysis_seconds,
        if track.from_cache {
            " [from cache]"
        } else {
            ""
        },
        t1.elapsed().as_secs_f64()
    );
    println!();
    println!("Tempo:     {:.1} BPM", a.bpm);
    println!(
        "Rhythm:    confidence {:.2} ({})",
        a.rhythm_confidence,
        if a.rhythm_confidence >= RHYTHM_THRESHOLD {
            "clear pulse: beat sync on"
        } else if a.rhythm.iter().any(|&r| r >= RHYTHM_THRESHOLD) {
            "partly rhythmic: beat sync only in passages with a clear pulse"
        } else {
            "no steady pulse: beat sync off, tempo is a guess"
        }
    );
    if !a.rhythm.is_empty() {
        // One character per ~2 s: ▁ (no pulse) .. █ (clear pulse).
        const BARS: [char; 8] = ['▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];
        let line: String = a
            .rhythm
            .chunks(2)
            .map(|c| {
                let v = c.iter().sum::<f32>() / c.len() as f32;
                BARS[((v * 7.0).round() as usize).min(7)]
            })
            .collect();
        println!("           {line}");
    }
    let default_onsets = a.onsets_at(ShowSettings::DEFAULT_SENSITIVITY).count();
    println!(
        "Beats:     {} ({} bars), onsets: {} at default sensitivity ({:.1}/s; {} at most sensitive)",
        a.beats.len(),
        a.downbeats().count(),
        default_onsets,
        default_onsets as f64 / a.duration.max(1e-3),
        a.onsets.len()
    );
    if let (Some(first), Some(last)) = (a.beats.first(), a.beats.last()) {
        println!(
            "           first beat {:.2} s, last beat {:.2} s",
            first.time, last.time
        );
    }
    let mut tempo_changes = Vec::new();
    let mut prev = 0.0f32;
    for b in &a.beats {
        if (b.bpm - prev).abs() > 3.0 {
            tempo_changes.push(format!("{:.1}s→{:.0}", b.time, b.bpm));
            prev = b.bpm;
        }
    }
    if tempo_changes.len() > 1 {
        println!("Tempo map: {}", tempo_changes.join("  "));
    }
    println!();
    println!("Sections ({}):", a.sections.len());
    for (i, s) in a.sections.iter().enumerate() {
        let bar = "█".repeat((s.energy * 20.0).round() as usize);
        println!(
            "  {:2}  {:>6} – {:>6}  {:<8} {:<20} rhythm {:.2}  {}",
            i + 1,
            mmss(s.start),
            mmss(s.end),
            format!("{:?}", s.kind),
            bar,
            s.rhythm,
            if s.starts_with_drop { "DROP" } else { "" }
        );
    }
    println!();
    if a.drops.is_empty() {
        println!("Drops:     none");
    } else {
        for d in &a.drops {
            println!("Drop:      {} (strength {:.2})", mmss(d.time), d.strength);
        }
    }

    if let Some(out) = show_out {
        let mut show = Show::auto_generate(a);
        show.track.path = path.clone();
        show.track.title = m.title.clone().unwrap_or_default();
        match show.save(&out) {
            Ok(()) => println!(
                "\nWrote {} cues, {} one-shots to {out}",
                show.cues.len(),
                show.events.len()
            ),
            Err(e) => eprintln!("could not write show: {e}"),
        }
    }
}

fn mmss(t: f64) -> String {
    format!("{}:{:05.2}", (t / 60.0) as u32, t % 60.0)
}
