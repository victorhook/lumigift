//! Play a file and print the current bar/beat and the MusicFrame the engine
//! would send, in sync with what you hear.
//!
//! ```sh
//! cargo run -p lumigift-audio --release --example play -- song.mp3 [--seek 60] [--device ID]
//! cargo run -p lumigift-audio --release --example play -- --list
//! ```

use std::io::Write;
use std::time::Duration;

use lumigift_audio::{AnalysisCache, PlaybackState, Player, Show, ShowEngine, load_track};
use lumigift_fx::EFFECT_NAMES;
use lumigift_proto::music_flags as mf;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|a| a == "--list") {
        match Player::output_devices() {
            Ok(devs) => {
                for d in devs {
                    println!(
                        "{} {}  ({})",
                        if d.is_default { "*" } else { " " },
                        d.id,
                        d.name
                    );
                }
            }
            Err(e) => eprintln!("error: {e}"),
        }
        return;
    }
    let opt = |name: &str| {
        args.iter()
            .position(|a| a == name)
            .and_then(|i| args.get(i + 1))
            .cloned()
    };
    let path = args
        .iter()
        .enumerate()
        .find(|(i, a)| !a.starts_with("--") && (*i == 0 || !args[i - 1].starts_with("--")))
        .map(|(_, a)| a.clone());
    let Some(path) = path else {
        eprintln!("usage: play <file> [--seek SECONDS] [--device ID] | play --list");
        std::process::exit(2);
    };

    // Load in the background, showing progress.
    let mut job = load_track(&path, AnalysisCache::default_location());
    let track = loop {
        if let Some(r) = job.try_take() {
            match r {
                Ok(t) => break t,
                Err(e) => {
                    eprintln!("\nerror: {e}");
                    std::process::exit(1);
                }
            }
        }
        print!("\r{:<24} {:3.0}%", job.stage(), job.progress() * 100.0);
        let _ = std::io::stdout().flush();
        std::thread::sleep(Duration::from_millis(50));
    };
    let a = track.analysis.clone();
    println!(
        "\r{}: {:.1} BPM, {} beats, {} sections, {} drops{}",
        track.audio.metadata.title.as_deref().unwrap_or("?"),
        a.bpm,
        a.beats.len(),
        a.sections.len(),
        a.drops.len(),
        if track.from_cache {
            " (cached analysis)"
        } else {
            ""
        }
    );

    let player = match Player::with_device(opt("--device").as_deref()) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("audio output: {e}");
            std::process::exit(1);
        }
    };
    println!(
        "Output: {} @ {} Hz",
        player.device_name(),
        player.device_sample_rate()
    );
    player.load(track.audio.clone());
    if let Some(s) = opt("--seek").and_then(|s| s.parse::<f64>().ok()) {
        player.seek(s);
    }

    let mut engine = ShowEngine::new(a.clone(), Show::auto_generate(&a));
    engine.set_lookahead(0.0); // Printing to a terminal: no radio latency.
    player.play();

    let mut flash = 0u32;
    loop {
        let pos = player.position();
        let f = engine.frame_for_playback(pos, 10);
        if f.flags & mf::BEAT != 0 {
            flash = 8;
        }
        let beat = a.beat_position(pos);
        let (bar, bib) = beat
            .map(|b| (b.bar + 1, b.beat_in_bar + 1))
            .unwrap_or((0, 0));
        let dots: String = (1..=4)
            .map(|i| {
                if i == bib as i64 && flash > 0 {
                    '●'
                } else if i == bib as i64 {
                    '◉'
                } else {
                    '·'
                }
            })
            .collect();
        let sec = a
            .section_at(pos)
            .map(|(i, s)| format!("{} {:?}", i + 1, s.kind))
            .unwrap_or_default();
        let meter = |v: u8| "▮".repeat((v as usize * 8 + 128) / 255);
        print!(
            "\r{:>7.2}s  bar {:>3} {}  {:>3} BPM  {:<8} {:<9} bass {:<8} lvl {:<8}{}{}  lat {:>4.0} ms   ",
            pos,
            bar,
            dots,
            f.bpm,
            EFFECT_NAMES.get(f.effect as usize).unwrap_or(&"?"),
            sec,
            meter(f.bass),
            meter(f.level),
            if f.flags & mf::DROP != 0 {
                " DROP!"
            } else {
                ""
            },
            if f.flags & mf::SILENT != 0 {
                " (silent)"
            } else {
                ""
            },
            player.output_latency() * 1000.0
        );
        let _ = std::io::stdout().flush();
        flash = flash.saturating_sub(1);
        if player.state() == PlaybackState::Ended {
            break;
        }
        if let Some(e) = player.last_error() {
            eprintln!("\naudio error: {e}");
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    println!();
}
