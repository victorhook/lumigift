//! Spectral front-end shared by the offline and the realtime analyzer, plus
//! small numeric helpers.

use std::sync::Arc;

use realfft::num_complex::Complex;
use realfft::{RealFftPlanner, RealToComplex};

/// Number of log-spaced bands used for spectral flux, structure analysis and
/// the spectrogram lane.
pub const N_BANDS: usize = 32;
/// Lowest / highest band edge of the log filterbank.
pub const BANDS_LO_HZ: f32 = 30.0;
pub const BANDS_HI_HZ: f32 = 16_000.0;
/// Feature band limits (Hz).
pub const BASS_HZ: (f32, f32) = (20.0, 150.0);
pub const MID_HZ: (f32, f32) = (150.0, 2_000.0);
pub const TREBLE_HZ: (f32, f32) = (2_000.0, 12_000.0);
/// Band levels are clamped to this floor (dB re. a full-scale sine) before
/// spectral flux is computed, so noise in silence does not produce onsets.
pub const FLUX_FLOOR_DB: f32 = -75.0;
/// Target analysis frame rate.
pub const TARGET_FPS: f64 = 100.0;
/// dB range mapped onto 0..=1 by the feature normaliser.
pub const NORM_RANGE_DB: f32 = 30.0;
/// Minimum dB range even for heavily compressed tracks.
pub const NORM_MIN_RANGE_DB: f32 = 12.0;
/// Exponent applied after the linear dB mapping (makes the LEDs punchier).
pub const NORM_GAMMA: f32 = 1.5;
/// Fundamental range searched by the pitch detector (≈ E2 .. G6).
pub const TONE_F0_HZ: (f32, f32) = (80.0, 1_600.0);
/// Highest partial used by the pitch detector.
pub const TONE_MAX_HZ: f32 = 5_000.0;
/// Pitch candidates per semitone.
const TONE_RES: usize = 3;
/// Harmonics summed per candidate, and the weight decay per harmonic.
const TONE_HARMONICS: usize = 10;
const TONE_DECAY: f32 = 0.85;
/// Spectral peaks considered per frame (the strongest ones).
const TONE_PEAKS: usize = 40;
/// Peaks weaker than this (dB below the frame's strongest) are ignored.
const TONE_PEAK_RANGE_DB: f32 = 45.0;
/// Chroma resolution: thirds of a semitone, folded into one octave.
pub const CHROMA_BINS: usize = 36;
/// Share of the chroma in the matched root class (its bin ±1) mapped from
/// `[lo, hi]` onto clarity 0..=1. Measured shares (median): white noise 0.12,
/// drums 0.24, speech 0.30, a major triad 0.31, band mixes 0.29–0.57, solo
/// violin 0.82, a harmonic tone 0.78, a sine 1.0.
pub const TONE_CLARITY: (f32, f32) = (0.20, 0.70);
/// Chroma smoothing on two time scales: a short one that resolves the notes
/// of a clean melody and a long one that follows a band's harmony (chord
/// changes rather than chord tones). Half-widths of the offline analysis's
/// centred windows in frames (±70 / ±250 ms) and the time constants of the
/// live analyzer's causal ones.
pub const CHROMA_SMOOTH_HALF: (usize, usize) = (7, 25);
pub const CHROMA_SMOOTH_TAU: (f32, f32) = (0.08, 0.3);
/// Clarity of a clean note: the short time scale is used from here up.
/// (`engine::NOTE_CLEAR` is the same threshold.)
pub const TONE_CLEAN: f32 = 0.45;

/// Hop size giving ~100 frames/s (441 at 44.1 kHz, 480 at 48 kHz).
pub fn hop_for(sample_rate: u32) -> usize {
    ((sample_rate as f64 / TARGET_FPS).round() as usize).max(1)
}

/// STFT window: the power of two closest to ~46 ms (2048 at 44.1/48 kHz).
pub fn window_for(sample_rate: u32) -> usize {
    let target = sample_rate as f64 * 0.0464;
    let p = target.log2().round() as u32;
    1usize << p.clamp(8, 15)
}

/// Raw (unnormalised) measurements of one STFT frame.
#[derive(Debug, Clone, Copy)]
pub struct RawFrame {
    pub bass_db: f32,
    pub mid_db: f32,
    pub treble_db: f32,
    pub rms_db: f32,
    pub bands_db: [f32; N_BANDS],
    /// Mean positive change of the floored band levels (dB/band).
    pub flux: f32,
    /// The same, restricted to bands below 150 Hz (kick drums).
    pub bass_flux: f32,
    /// Harmonic pitch-class profile (see [`ToneDetector`]): amplitude-weighted
    /// fundamental votes folded into one octave, bin `j` = pitch class `j/3`
    /// (C = 0). Smooth it over time, then read it with [`tone_of_chroma`].
    pub chroma: [f32; CHROMA_BINS],
}

/// Windowed FFT + band energies + spectral flux, one frame at a time.
pub struct Frontend {
    pub window_len: usize,
    window: Vec<f32>,
    fft: Arc<dyn RealToComplex<f32>>,
    input: Vec<f32>,
    spectrum: Vec<Complex<f32>>,
    scratch: Vec<Complex<f32>>,
    power: Vec<f32>,
    band_bins: Vec<(usize, usize)>,
    n_bass_bands: usize,
    bass_bins: (usize, usize),
    mid_bins: (usize, usize),
    treble_bins: (usize, usize),
    /// 1 / (peak |X| of a full-scale sine)^2.
    power_norm: f32,
    prev: Option<[f32; N_BANDS]>,
    tone: ToneDetector,
}

impl Frontend {
    pub fn new(sample_rate: u32) -> Self {
        let n = window_for(sample_rate);
        let window: Vec<f32> = (0..n)
            .map(|i| 0.5 - 0.5 * (2.0 * std::f32::consts::PI * i as f32 / n as f32).cos())
            .collect();
        let wsum: f32 = window.iter().sum();
        let fft = RealFftPlanner::<f32>::new().plan_fft_forward(n);
        let spectrum = fft.make_output_vec();
        let scratch = fft.make_scratch_vec();
        let df = sample_rate as f32 / n as f32;
        let nyq_bin = n / 2;
        let bin_range = |lo: f32, hi: f32| {
            let a = ((lo / df).ceil() as usize).clamp(1, nyq_bin);
            let b = ((hi / df).ceil() as usize).clamp(a + 1, nyq_bin + 1);
            (a, b)
        };
        let edges = band_edges(sample_rate);
        let band_bins: Vec<(usize, usize)> = edges
            .windows(2)
            .map(|e| {
                let (a, b) = bin_range(e[0], e[1]);
                if (e[0] / df).ceil() >= (e[1] / df).ceil() {
                    // Narrower than a bin: use the nearest bin.
                    let c = ((0.5 * (e[0] + e[1]) / df).round() as usize).clamp(1, nyq_bin);
                    (c, c + 1)
                } else {
                    (a, b)
                }
            })
            .collect();
        let n_bass_bands = edges
            .windows(2)
            .filter(|e| e[1] <= BASS_HZ.1 * 1.05)
            .count()
            .max(1);
        Self {
            window_len: n,
            window,
            fft,
            input: vec![0.0; n],
            spectrum,
            scratch,
            power: vec![0.0; n / 2 + 1],
            band_bins,
            n_bass_bands,
            bass_bins: bin_range(BASS_HZ.0, BASS_HZ.1),
            mid_bins: bin_range(MID_HZ.0, MID_HZ.1),
            treble_bins: bin_range(TREBLE_HZ.0, TREBLE_HZ.1.min(sample_rate as f32 * 0.49)),
            power_norm: 1.0 / (0.5 * wsum).powi(2),
            prev: None,
            tone: ToneDetector::new(df),
        }
    }

    /// Analyse one frame of exactly `window_len` samples.
    pub fn process(&mut self, frame: &[f32]) -> RawFrame {
        let n = self.window_len;
        debug_assert_eq!(frame.len(), n);
        // RMS over the central half of the window.
        let c = &frame[n / 4..3 * n / 4];
        let rms = (c.iter().map(|x| x * x).sum::<f32>() / c.len() as f32).sqrt();
        for ((d, s), w) in self.input.iter_mut().zip(frame).zip(&self.window) {
            *d = s * w;
        }
        // The lengths always match, so this cannot fail.
        let _ =
            self.fft
                .process_with_scratch(&mut self.input, &mut self.spectrum, &mut self.scratch);
        for (p, c) in self.power.iter_mut().zip(&self.spectrum) {
            *p = c.norm_sqr() * self.power_norm;
        }
        let sum = |r: (usize, usize)| {
            self.power[r.0..r.1.min(self.power.len())]
                .iter()
                .sum::<f32>()
        };
        let mut bands_db = [0f32; N_BANDS];
        for (b, r) in bands_db.iter_mut().zip(&self.band_bins) {
            *b = db(sum(*r));
        }
        let (mut flux, mut bass_flux) = (0.0, 0.0);
        if let Some(prev) = &self.prev {
            for (i, (cur, old)) in bands_db.iter().zip(prev).enumerate() {
                let d = (cur.max(FLUX_FLOOR_DB) - old.max(FLUX_FLOOR_DB)).max(0.0);
                flux += d;
                if i < self.n_bass_bands {
                    bass_flux += d;
                }
            }
            flux /= N_BANDS as f32;
            bass_flux /= self.n_bass_bands as f32;
        }
        self.prev = Some(bands_db);
        let chroma = self.tone.detect(&self.power);
        RawFrame {
            bass_db: db(sum(self.bass_bins)),
            mid_db: db(sum(self.mid_bins)),
            treble_db: db(sum(self.treble_bins)),
            rms_db: 20.0 * (rms + 1e-9).log10(),
            bands_db,
            flux,
            bass_flux,
            chroma,
        }
    }
}

/// Interval weights (semitones above the root) of the templates
/// [`tone_of_chroma`] matches: a single note, a major and a minor triad.
const TEMPLATES: [&[(usize, f32)]; 3] = [
    &[(0, 1.0)],
    &[(0, 1.0), (4, 0.6), (7, 0.8)],
    &[(0, 1.0), (3, 0.6), (7, 0.8)],
];

/// Dominant pitch class (0.0..12.0, C = 0) and clarity (0..=1) of a chroma
/// profile.
///
/// The class is the root of the best-matching template (a single note, or a
/// major / minor triad, by cosine similarity over the 12 classes), so a
/// sustained chord keeps its root even when beating partials make its fifth
/// momentarily louder. It is refined within ±1/3 semitone parabolically.
/// Clarity is the share of the profile in that class (its bin ±1), mapped
/// through [`TONE_CLARITY`]: melodies and chords stand out, drums, noise and
/// speech-like glides stay flat.
pub fn tone_of_chroma(c: &[f32; CHROMA_BINS]) -> (f32, f32) {
    let total: f32 = c.iter().sum();
    if total <= 1e-9 {
        return (0.0, 0.0);
    }
    let at = |j: isize| c[j.rem_euclid(CHROMA_BINS as isize) as usize];
    let class: [f32; 12] = std::array::from_fn(|k| {
        let j = 3 * k as isize;
        at(j - 1) + at(j) + at(j + 1)
    });
    let norm = class.iter().map(|v| v * v).sum::<f32>().sqrt().max(1e-12);
    let mut best = (f32::MIN, 0usize);
    for t in TEMPLATES {
        let tn = t.iter().map(|(_, w)| w * w).sum::<f32>().sqrt();
        for root in 0..12 {
            let dot: f32 = t.iter().map(|&(iv, w)| w * class[(root + iv) % 12]).sum();
            let score = dot / (tn * norm);
            if score > best.0 {
                best = (score, root);
            }
        }
    }
    let j = 3 * best.1 as isize;
    let off = parabolic_offset(at(j - 1), at(j), at(j + 1)).clamp(-1.0, 1.0);
    let pc = ((j as f32 + off) / 3.0).rem_euclid(12.0);
    let share = class[best.1] / total;
    let clarity = ((share - TONE_CLARITY.0) / (TONE_CLARITY.1 - TONE_CLARITY.0)).clamp(0.0, 1.0);
    (pc, clarity)
}

/// Centred moving average of chroma frames over `±half` frames (shrinking
/// at the edges).
pub fn smooth_chroma(frames: &[[f32; CHROMA_BINS]], half: usize) -> Vec<[f32; CHROMA_BINS]> {
    let n = frames.len();
    let mut out = vec![[0f32; CHROMA_BINS]; n];
    let mut sum = [0f64; CHROMA_BINS];
    let (mut a, mut b) = (0usize, 0usize); // window [a, b)
    for (i, o) in out.iter_mut().enumerate() {
        let (lo, hi) = (i.saturating_sub(half), (i + half + 1).min(n));
        while b < hi {
            sum.iter_mut().zip(&frames[b]).for_each(|(s, v)| *s += *v as f64);
            b += 1;
        }
        while a < lo {
            sum.iter_mut().zip(&frames[a]).for_each(|(s, v)| *s -= *v as f64);
            a += 1;
        }
        let k = 1.0 / (hi - lo) as f64;
        o.iter_mut().zip(&sum).for_each(|(o, s)| *o = (s * k).max(0.0) as f32);
    }
    out
}

/// The note of a frame from its short- and long-smoothed chroma: the short
/// one where it shows a clean note, else the long one (the harmony).
pub fn tone_of_scales(short: &[f32; CHROMA_BINS], long: &[f32; CHROMA_BINS]) -> (f32, f32) {
    let fast = tone_of_chroma(short);
    if fast.1 >= TONE_CLEAN {
        fast
    } else {
        let slow = tone_of_chroma(long);
        if slow.1 >= fast.1 { slow } else { fast }
    }
}

/// Harmonic pitch-class profile: harmonic summation over the spectral peaks,
/// folded into one octave.
///
/// Every peak (parabolically refined in frequency) votes for the
/// fundamentals it could be harmonic `h` of, weighted `TONE_DECAY^(h-1)` ×
/// its amplitude, on a 1/3-semitone grid aligned to equal temperament. The
/// votes are folded by pitch class, so octave errors do not matter and a
/// chord or a melody over a band still produces a clear winner, while
/// inharmonic sound (drums, noise) spreads over all classes. Candidates
/// without a peak of their own (pure subharmonics) are gated out.
struct ToneDetector {
    df: f32,
    /// Bin range searched for peaks.
    bins: (usize, usize),
    /// Semitone (MIDI) number of the lowest candidate.
    s_lo: f32,
    salience: Vec<f32>,
    /// The harmonic-1 part of `salience` (a peak at the candidate itself).
    direct: Vec<f32>,
    peaks: Vec<(f32, f32)>,
    /// `12 * log2(h)` for each harmonic.
    h_st: [f32; TONE_HARMONICS],
    h_w: [f32; TONE_HARMONICS],
}

fn midi_of(f: f32) -> f32 {
    69.0 + 12.0 * (f / 440.0).log2()
}

impl ToneDetector {
    fn new(df: f32) -> Self {
        // Grid aligned to whole semitones, so bin 0 of the fold is C.
        let s_lo = midi_of(TONE_F0_HZ.0).floor();
        let n = ((midi_of(TONE_F0_HZ.1) - s_lo) * TONE_RES as f32).ceil() as usize + 1;
        Self {
            df,
            bins: (
                ((TONE_F0_HZ.0 * 0.95 / df).floor() as usize).max(2),
                (TONE_MAX_HZ / df).ceil() as usize,
            ),
            s_lo,
            salience: vec![0.0; n],
            direct: vec![0.0; n],
            peaks: Vec::with_capacity(4 * TONE_PEAKS),
            h_st: std::array::from_fn(|h| 12.0 * ((h + 1) as f32).log2()),
            h_w: std::array::from_fn(|h| TONE_DECAY.powi(h as i32)),
        }
    }

    /// The folded profile of one power spectrum (zeros when there is
    /// nothing to hear).
    fn detect(&mut self, power: &[f32]) -> [f32; CHROMA_BINS] {
        let mut chroma = [0f32; CHROMA_BINS];
        let (a, b) = (self.bins.0, self.bins.1.min(power.len().saturating_sub(1)));
        if b <= a + 2 {
            return chroma;
        }
        let max = power[a..b].iter().copied().fold(0.0f32, f32::max);
        // Below ~-90 dBFS there is nothing to hear.
        if max < 1e-9 {
            return chroma;
        }
        let floor = max * 10f32.powf(-TONE_PEAK_RANGE_DB / 10.0);
        self.peaks.clear();
        for k in a.max(1)..b {
            let p = power[k];
            if p > floor && p > power[k - 1] && p >= power[k + 1] {
                // Parabolic refinement on log power.
                let l = |x: f32| (x + 1e-20).ln();
                let off = parabolic_offset(l(power[k - 1]), l(p), l(power[k + 1]));
                self.peaks.push(((k as f32 + off) * self.df, p.sqrt()));
            }
        }
        if self.peaks.is_empty() {
            return chroma;
        }
        if self.peaks.len() > TONE_PEAKS {
            self.peaks
                .select_nth_unstable_by(TONE_PEAKS - 1, |x, y| y.1.total_cmp(&x.1));
            self.peaks.truncate(TONE_PEAKS);
        }
        self.salience.iter_mut().for_each(|v| *v = 0.0);
        self.direct.iter_mut().for_each(|v| *v = 0.0);
        let n = self.salience.len() as isize;
        let res = TONE_RES as f32;
        for &(f, amp) in &self.peaks {
            let s = midi_of(f);
            for (h, (hst, w)) in self.h_st.iter().zip(&self.h_w).enumerate() {
                // Candidate fundamental if this peak is harmonic h, ±0.5 st
                // (triangular), spread over the neighbouring grid points.
                let x = (s - hst - self.s_lo) * res;
                if x < -res {
                    break;
                }
                let c = x.round() as isize;
                for d in -1..=1isize {
                    let i = c + d;
                    if i < 0 || i >= n {
                        continue;
                    }
                    let k = 1.0 - ((i as f32 - x).abs() / (res * 0.5)).min(1.0);
                    if k > 0.0 {
                        self.salience[i as usize] += w * amp * k;
                        if h == 0 {
                            self.direct[i as usize] += amp * k;
                        }
                    }
                }
            }
        }
        // Only candidates with energy at the pitch itself count: the
        // subharmonics every peak also votes for (a fifth and a third below)
        // would otherwise make a lone note look like part of a chord.
        let base = (self.s_lo as i32).rem_euclid(12) as usize * TONE_RES;
        for (i, (v, d)) in self.salience.iter().zip(&self.direct).enumerate() {
            let gate = (d / (0.3 * v).max(1e-12)).min(1.0);
            chroma[(base + i) % CHROMA_BINS] += v * gate;
        }
        chroma
    }
}

/// Edges (N_BANDS + 1 values, Hz) of the log-spaced filterbank.
pub fn band_edges(sample_rate: u32) -> Vec<f32> {
    let hi = BANDS_HI_HZ.min(sample_rate as f32 * 0.475);
    let r = (hi / BANDS_LO_HZ).ln();
    (0..=N_BANDS)
        .map(|i| BANDS_LO_HZ * (r * i as f32 / N_BANDS as f32).exp())
        .collect()
}

#[inline]
pub fn db(power: f32) -> f32 {
    10.0 * (power + 1e-12).log10()
}

/// Maps a dB value into 0..=1 between `lo` and `hi`, with [`NORM_GAMMA`].
#[inline]
pub fn map_db(v: f32, lo: f32, hi: f32) -> f32 {
    ((v - lo) / (hi - lo).max(1e-3))
        .clamp(0.0, 1.0)
        .powf(NORM_GAMMA)
}

/// The `p`-th percentile (0..=100) of `v` (reorders `v`). 0.0 for empty input.
pub fn percentile(v: &mut [f32], p: f32) -> f32 {
    if v.is_empty() {
        return 0.0;
    }
    let k = ((p / 100.0) * (v.len() - 1) as f32).round() as usize;
    let k = k.min(v.len() - 1);
    *v.select_nth_unstable_by(k, |a, b| a.total_cmp(b)).1
}

/// Centred moving average with a window of `2 * half + 1` (shrinks at edges).
pub fn moving_average(x: &[f32], half: usize) -> Vec<f32> {
    let n = x.len();
    let mut prefix = Vec::with_capacity(n + 1);
    prefix.push(0.0f64);
    for &v in x {
        prefix.push(prefix.last().unwrap() + v as f64);
    }
    (0..n)
        .map(|i| {
            let a = i.saturating_sub(half);
            let b = (i + half + 1).min(n);
            ((prefix[b] - prefix[a]) / (b - a) as f64) as f32
        })
        .collect()
}

/// Channel average of interleaved samples.
pub fn mixdown(samples: &[f32], channels: usize) -> Vec<f32> {
    if channels <= 1 {
        return samples.to_vec();
    }
    let inv = 1.0 / channels as f32;
    samples
        .chunks_exact(channels)
        .map(|f| f.iter().sum::<f32>() * inv)
        .collect()
}

pub fn mean(x: &[f32]) -> f32 {
    if x.is_empty() {
        0.0
    } else {
        x.iter().sum::<f32>() / x.len() as f32
    }
}

pub fn std_dev(x: &[f32]) -> f32 {
    if x.len() < 2 {
        return 0.0;
    }
    let m = mean(x);
    (x.iter().map(|v| (v - m) * (v - m)).sum::<f32>() / (x.len() - 1) as f32).sqrt()
}

/// Sub-sample offset (-0.5..0.5) of a peak from three neighbouring values.
#[inline]
pub fn parabolic_offset(a: f32, b: f32, c: f32) -> f32 {
    let d = a - 2.0 * b + c;
    if d.abs() < 1e-12 {
        0.0
    } else {
        (0.5 * (a - c) / d).clamp(-0.5, 0.5)
    }
}
