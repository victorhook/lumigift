//! Decoding, cache and background jobs.

use std::path::{Path, PathBuf};
use std::process::Command;

use lumigift_audio::synth;
use lumigift_audio::{AnalysisCache, Error, decode_file, load_track};

fn temp_dir(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("lg-audio-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// 16-bit PCM WAV writer.
fn write_wav(path: &Path, samples: &[f32], rate: u32, channels: u16) {
    let data: Vec<u8> = samples
        .iter()
        .flat_map(|s| ((s.clamp(-1.0, 1.0) * 32767.0).round() as i16).to_le_bytes())
        .collect();
    let mut b = Vec::new();
    b.extend_from_slice(b"RIFF");
    b.extend_from_slice(&(36 + data.len() as u32).to_le_bytes());
    b.extend_from_slice(b"WAVEfmt ");
    b.extend_from_slice(&16u32.to_le_bytes());
    b.extend_from_slice(&1u16.to_le_bytes());
    b.extend_from_slice(&channels.to_le_bytes());
    b.extend_from_slice(&rate.to_le_bytes());
    b.extend_from_slice(&(rate * channels as u32 * 2).to_le_bytes());
    b.extend_from_slice(&(channels * 2).to_le_bytes());
    b.extend_from_slice(&16u16.to_le_bytes());
    b.extend_from_slice(b"data");
    b.extend_from_slice(&(data.len() as u32).to_le_bytes());
    b.extend_from_slice(&data);
    std::fs::write(path, b).unwrap();
}

fn demo_wav(dir: &Path) -> (PathBuf, synth::SynthTrack) {
    let t = synth::render(&[synth::Part::groove(32.0, 120.0, 0.8)], 44_100);
    let stereo: Vec<f32> = t.samples.iter().flat_map(|&s| [s, s * 0.5]).collect();
    let p = dir.join("demo.wav");
    write_wav(&p, &stereo, 44_100, 2);
    (p, t)
}

#[test]
fn decode_wav() {
    let dir = temp_dir("wav");
    let (p, t) = demo_wav(&dir);
    let a = decode_file(&p).unwrap();
    assert_eq!(a.sample_rate, 44_100);
    assert_eq!(a.channels, 2);
    assert_eq!(a.frames(), t.samples.len());
    assert!((a.metadata.duration - t.samples.len() as f64 / 44_100.0).abs() < 1e-6);
    assert_eq!(a.metadata.title.as_deref(), Some("demo"));
    assert_eq!(a.content_hash.len(), 64);
    let err = (a.samples[2000] - t.samples[1000]).abs();
    assert!(err < 1e-4, "{err}");
    assert!(matches!(
        decode_file(dir.join("missing.wav")),
        Err(Error::Io(_))
    ));
    std::fs::write(dir.join("junk.mp3"), b"definitely not audio").unwrap();
    assert!(matches!(
        decode_file(dir.join("junk.mp3")),
        Err(Error::Decode(_))
    ));
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn decode_compressed_formats_if_ffmpeg_available() {
    if Command::new("ffmpeg").arg("-version").output().is_err() {
        eprintln!("ffmpeg not found; skipping");
        return;
    }
    let dir = temp_dir("fmt");
    let (wav, t) = demo_wav(&dir);
    let dur = t.samples.len() as f64 / 44_100.0;
    for (ext, args) in [
        ("mp3", vec!["-c:a", "libmp3lame", "-b:a", "192k"]),
        ("flac", vec!["-c:a", "flac"]),
        ("ogg", vec!["-c:a", "libvorbis", "-q:a", "5"]),
        ("m4a", vec!["-c:a", "aac", "-b:a", "192k"]),
    ] {
        let out = dir.join(format!("demo.{ext}"));
        let mut cmd = Command::new("ffmpeg");
        cmd.args(["-loglevel", "error", "-y", "-i"])
            .arg(&wav)
            .args(&args);
        cmd.args([
            "-metadata",
            "title=Synthetic Groove",
            "-metadata",
            "artist=Lumigift",
        ])
        .arg(&out);
        let ok = cmd.status().map(|s| s.success()).unwrap_or(false);
        if !ok {
            eprintln!("ffmpeg could not encode {ext}; skipping it");
            continue;
        }
        let a = decode_file(&out).unwrap_or_else(|e| panic!("{ext}: {e}"));
        assert_eq!(a.channels, 2, "{ext}");
        assert!(
            (a.duration() - dur).abs() < 0.15,
            "{ext}: {} vs {dur}",
            a.duration()
        );
        assert_eq!(
            a.metadata.title.as_deref(),
            Some("Synthetic Groove"),
            "{ext}"
        );
        assert_eq!(a.metadata.artist.as_deref(), Some("Lumigift"), "{ext}");
        let an = lumigift_audio::Analysis::analyze(&a.mono(), a.sample_rate);
        assert!((an.bpm - 120.0).abs() <= 2.0, "{ext}: bpm {}", an.bpm);
        let beats: Vec<f64> = an.beats.iter().map(|b| b.time).collect();
        // Encoder delay is small but nonzero: compare with ±70 ms.
        let f = synth::f_measure(&beats, &t.beats, 0.07);
        assert!(f > 0.9, "{ext}: F={f}");
    }
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn load_track_job_and_cache() {
    let dir = temp_dir("job");
    let (p, _) = demo_wav(&dir);
    let cache = AnalysisCache::new(dir.join("cache"));
    let mut job = load_track(&p, Some(cache.clone()));
    let mut saw_progress = false;
    let track = loop {
        if let Some(r) = job.try_take() {
            break r.unwrap();
        }
        let pr = job.progress();
        assert!((0.0..=1.0).contains(&pr));
        saw_progress |= pr > 0.0;
        std::thread::sleep(std::time::Duration::from_millis(2));
    };
    let _ = saw_progress;
    assert!(!track.from_cache);
    assert!((track.analysis.bpm - 120.0).abs() <= 2.0);
    assert_eq!(track.analysis.content_hash, track.audio.content_hash);
    assert!(cache.load(&track.audio.content_hash).is_some());

    let again = load_track(&p, Some(cache.clone())).wait().unwrap();
    assert!(again.from_cache);
    assert_eq!(*again.analysis, *track.analysis);

    // Cancel: the job ends with Error::Cancelled (or finished first).
    let job = load_track(&p, None);
    job.cancel();
    match job.wait() {
        Err(Error::Cancelled) | Ok(_) => {}
        Err(e) => panic!("unexpected {e}"),
    }
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn analyze_async_on_decoded_audio() {
    let t = synth::render(&[synth::Part::groove(16.0, 120.0, 0.8)], 44_100);
    let audio = std::sync::Arc::new(lumigift_audio::DecodedAudio::from_samples(
        t.samples, 44_100, 1,
    ));
    let a = lumigift_audio::analyze_async(audio, None).wait().unwrap();
    assert!(!a.beats.is_empty());
    assert!(a.content_hash.is_empty());
}
