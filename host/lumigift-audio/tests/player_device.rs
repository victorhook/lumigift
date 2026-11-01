//! Needs a real output device, so it is ignored by default:
//! `cargo test -p lumigift-audio --test player_device -- --ignored --nocapture`.
//! Plays at volume 0, so nothing is audible.

use std::sync::Arc;
use std::time::{Duration, Instant};

use lumigift_audio::{DecodedAudio, PlaybackState, Player};

#[test]
#[ignore]
fn position_tracks_wall_clock_on_real_device() {
    let devices = Player::output_devices().unwrap();
    eprintln!(
        "{} output devices, default: {:?}",
        devices.len(),
        devices.iter().find(|d| d.is_default).map(|d| &d.name)
    );
    let player = Player::new().unwrap();
    player.set_volume(0.0);
    // 44.1 kHz source to exercise the resampler on 48 kHz devices.
    let audio = Arc::new(DecodedAudio::from_samples(
        vec![0.0; 44_100 * 2 * 9],
        44_100,
        2,
    ));
    player.load(audio);
    player.seek(1.0);
    assert_eq!(player.position(), 1.0);
    player.play();
    let start = Instant::now();
    let mut samples = Vec::new();
    let mut last = 0.0;
    while start.elapsed() < Duration::from_secs(5) {
        let p = player.position();
        assert!(p >= last, "position went backwards: {last} -> {p}");
        last = p;
        samples.push((start.elapsed().as_secs_f64(), p));
        std::thread::sleep(Duration::from_millis(5));
    }
    let lat = player.output_latency();
    eprintln!(
        "device {} @ {} Hz, reported output latency {:.1} ms",
        player.device_name(),
        player.device_sample_rate(),
        lat * 1000.0
    );
    // After start-up (PipeWire's ALSA plugin settles its buffer over ~3 s,
    // which the timestamps follow), position advances 1:1 with wall time.
    let moving: Vec<_> = samples.iter().filter(|(t, _)| *t > 3.0).collect();
    let (t0, p0) = *moving[0];
    let (t1, p1) = *moving[moving.len() - 1];
    let rate = (p1 - p0) / (t1 - t0);
    eprintln!("rate {rate:.5}");
    assert!((rate - 1.0).abs() < 0.002, "rate {rate}");
    // Jitter: residual of a 1:1 line through the moving part.
    let max_dev = moving
        .iter()
        .map(|(t, p)| ((p - p0) - (t - t0)).abs())
        .fold(0.0, f64::max);
    eprintln!(
        "max deviation from a straight line: {:.2} ms",
        max_dev * 1000.0
    );
    assert!(max_dev < 0.002);
    // The first audio is heard roughly one output latency after play().
    let first_move = samples.iter().find(|(_, p)| *p > 1.0).unwrap().0;
    eprintln!("first movement after {:.1} ms", first_move * 1000.0);

    player.pause();
    std::thread::sleep(Duration::from_millis(300));
    let a = player.position();
    std::thread::sleep(Duration::from_millis(100));
    assert_eq!(player.position(), a, "position must hold while paused");
    assert_eq!(player.state(), PlaybackState::Paused);
    player.seek(8.5);
    player.play();
    std::thread::sleep(Duration::from_millis(1000));
    assert_eq!(player.state(), PlaybackState::Ended);
    assert!(
        (player.position() - 9.0).abs() < 0.01,
        "{}",
        player.position()
    );
}
