//! Deterministic synthetic drum tracks with ground truth, for tests, benches
//! and demos (no audio files needed).

/// Section of a synthetic track.
#[derive(Debug, Clone, Copy)]
pub struct Part {
    /// Length in beats (0 BPM = silence, length then in seconds).
    pub beats: f64,
    /// Tempo at the start / end of the part (linear ramp in BPM).
    pub bpm: (f64, f64),
    /// Overall loudness 0..=1.
    pub gain: f32,
    /// Kick on every beat (`true`) or no kick (build-ups).
    pub kick: bool,
    /// Bass line on the downbeats + sub-bass sustain ("drop" energy).
    pub bass: bool,
    /// Hi-hats on eighth notes.
    pub hats: bool,
    /// Snare on beats 2 and 4.
    pub snare: bool,
}

impl Part {
    pub const fn silence(seconds: f64) -> Self {
        Part {
            beats: seconds,
            bpm: (0.0, 0.0),
            gain: 0.0,
            kick: false,
            bass: false,
            hats: false,
            snare: false,
        }
    }
    pub const fn groove(beats: f64, bpm: f64, gain: f32) -> Self {
        Part {
            beats,
            bpm: (bpm, bpm),
            gain,
            kick: true,
            bass: false,
            hats: true,
            snare: true,
        }
    }
}

/// A rendered synthetic track plus its ground truth.
pub struct SynthTrack {
    pub sample_rate: u32,
    pub samples: Vec<f32>,
    /// Beat times (seconds).
    pub beats: Vec<f64>,
    /// Downbeat times (every 4th beat from the start of each non-silent part).
    pub downbeats: Vec<f64>,
    /// Start times of parts whose `bass` flag switches on after a part without it.
    pub drops: Vec<f64>,
    /// Start time of every part.
    pub part_starts: Vec<f64>,
}

#[derive(Clone, Copy)]
enum Hit {
    Kick,
    Hat,
    Snare,
    Bass,
}

struct Rng(u32);
impl Rng {
    fn next(&mut self) -> f32 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        self.0 = x;
        (x as f32 / u32::MAX as f32) * 2.0 - 1.0
    }
}

/// Render `parts` back to back. Beat phase is continuous across tempo ramps.
pub fn render(parts: &[Part], sample_rate: u32) -> SynthTrack {
    let sr = sample_rate as f64;
    let mut events: Vec<(f64, Hit, f32, usize)> = Vec::new(); // (time, hit, gain, beat_in_bar)
    let mut beats = Vec::new();
    let mut downbeats = Vec::new();
    let mut drops = Vec::new();
    let mut part_starts = Vec::new();
    let mut t = 0.0f64;
    let mut prev_bass = false;
    let mut bass_spans: Vec<(f64, f64, f32)> = Vec::new();
    for p in parts {
        part_starts.push(t);
        if p.bpm.0 <= 0.0 {
            t += p.beats;
            prev_bass = false;
            continue;
        }
        if p.bass && !prev_bass && t > 0.0 {
            drops.push(t);
        }
        prev_bass = p.bass;
        let n = p.beats.round() as usize;
        let start = t;
        for i in 0..n {
            let frac = i as f64 / n as f64;
            let bpm = p.bpm.0 + (p.bpm.1 - p.bpm.0) * frac;
            let ibi = 60.0 / bpm;
            let bib = i % 4;
            beats.push(t);
            if bib == 0 {
                downbeats.push(t);
            }
            if p.kick {
                events.push((t, Hit::Kick, p.gain * if bib == 0 { 1.0 } else { 0.8 }, bib));
            }
            if p.hats {
                events.push((t, Hit::Hat, p.gain * 0.35, bib));
                events.push((t + ibi / 2.0, Hit::Hat, p.gain * 0.25, bib));
            }
            if p.snare && (bib == 1 || bib == 3) {
                events.push((t, Hit::Snare, p.gain * 0.6, bib));
            }
            if p.bass && bib == 0 {
                events.push((t, Hit::Bass, p.gain, bib));
            }
            if !p.kick && p.hats {
                // Build-up: rising noise riser and 16th-note hats.
                let g = p.gain * 0.2 * (0.5 + frac as f32);
                events.push((t + ibi / 4.0, Hit::Hat, g, bib));
                events.push((t + 3.0 * ibi / 4.0, Hit::Hat, g, bib));
            }
            t += ibi;
        }
        if p.bass {
            bass_spans.push((start, t, p.gain));
        }
    }
    let total = ((t + 0.5) * sr) as usize;
    let mut out = vec![0f32; total];
    let mut rng = Rng(0x1234_5678);
    for &(et, hit, g, bib) in &events {
        let s0 = (et * sr).round() as usize;
        match hit {
            Hit::Kick => {
                // Kick: pitch sweep 150 → 45 Hz, ~180 ms.
                let len = (0.25 * sr) as usize;
                let mut ph = 0.0f64;
                for i in 0..len {
                    let tt = i as f64 / sr;
                    let f = 45.0 + 105.0 * (-tt / 0.03).exp();
                    ph += 2.0 * std::f64::consts::PI * f / sr;
                    let env = (-tt / 0.12).exp() as f32;
                    if let Some(o) = out.get_mut(s0 + i) {
                        *o += 0.9 * g * env * ph.sin() as f32;
                    }
                }
            }
            Hit::Hat => {
                // Hat: high-passed noise, ~40 ms.
                let len = (0.06 * sr) as usize;
                let mut prev = 0.0;
                for i in 0..len {
                    let tt = i as f64 / sr;
                    let n = rng.next();
                    let hp = n - prev;
                    prev = n;
                    let env = (-tt / 0.015).exp() as f32;
                    if let Some(o) = out.get_mut(s0 + i) {
                        *o += 0.5 * g * env * hp;
                    }
                }
            }
            Hit::Snare => {
                // Snare: noise + 190 Hz body, ~150 ms.
                let len = (0.2 * sr) as usize;
                for i in 0..len {
                    let tt = i as f64 / sr;
                    let env = (-tt / 0.06).exp() as f32;
                    let body = (2.0 * std::f64::consts::PI * 190.0 * tt).sin() as f32;
                    if let Some(o) = out.get_mut(s0 + i) {
                        *o += g * env * (0.5 * rng.next() + 0.4 * body);
                    }
                }
            }
            Hit::Bass => {
                // Bass stab on the downbeat (crash-like accent + low note).
                let len = (0.4 * sr) as usize;
                let f = [55.0, 65.4, 49.0, 73.4][(s0 / 7919 + bib) % 4];
                for i in 0..len {
                    let tt = i as f64 / sr;
                    let env = (-tt / 0.25).exp() as f32;
                    let v = (2.0 * std::f64::consts::PI * f * tt).sin() as f32;
                    let crash = rng.next() * (-tt / 0.15).exp() as f32 * 0.15;
                    if let Some(o) = out.get_mut(s0 + i) {
                        *o += g * env * (0.5 * v + crash);
                    }
                }
            }
        }
    }
    // Sustained sub bass + pad during "drop" parts.
    for &(a, b, g) in &bass_spans {
        let (sa, sb) = ((a * sr) as usize, ((b * sr) as usize).min(total));
        for (i, o) in out[sa..sb].iter_mut().enumerate() {
            let tt = i as f64 / sr;
            let v = (2.0 * std::f64::consts::PI * 41.2 * tt).sin() * 0.25
                + (2.0 * std::f64::consts::PI * 220.0 * tt).sin() * 0.05
                + (2.0 * std::f64::consts::PI * 277.2 * tt).sin() * 0.04;
            *o += g * v as f32;
        }
    }
    let peak = out.iter().fold(0f32, |m, v| m.max(v.abs()));
    if peak > 0.98 {
        let s = 0.98 / peak;
        out.iter_mut().for_each(|v| *v *= s);
    }
    SynthTrack {
        sample_rate,
        samples: out,
        beats,
        downbeats,
        drops,
        part_starts,
    }
}

/// Beat-tracking F-measure with a ±`tol` second window (one-to-one matching).
pub fn f_measure(detected: &[f64], truth: &[f64], tol: f64) -> f64 {
    if detected.is_empty() || truth.is_empty() {
        return 0.0;
    }
    let mut used = vec![false; truth.len()];
    let mut hits = 0usize;
    for &d in detected {
        let k = truth.partition_point(|&t| t < d - tol);
        for j in k..truth.len() {
            if truth[j] > d + tol {
                break;
            }
            if !used[j] {
                used[j] = true;
                hits += 1;
                break;
            }
        }
    }
    let p = hits as f64 / detected.len() as f64;
    let r = hits as f64 / truth.len() as f64;
    if p + r == 0.0 {
        0.0
    } else {
        2.0 * p * r / (p + r)
    }
}

/// The standard test track: silence, a quiet groove, a build-up without
/// kick, a loud "drop" with bass, a tempo change and a calm outro.
pub fn demo_track(sample_rate: u32) -> SynthTrack {
    let quiet = Part::groove(32.0, 120.0, 0.35);
    let build = Part {
        kick: false,
        snare: false,
        ..Part::groove(16.0, 120.0, 0.35)
    };
    let drop = Part {
        bass: true,
        ..Part::groove(32.0, 120.0, 1.0)
    };
    let faster = Part {
        bass: true,
        ..Part::groove(48.0, 128.0, 0.9)
    };
    let outro = Part::groove(16.0, 128.0, 0.3);
    render(
        &[
            Part::silence(2.0),
            quiet,
            build,
            drop,
            faster,
            outro,
            Part::silence(1.0),
        ],
        sample_rate,
    )
}
