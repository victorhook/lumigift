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
        RawFrame {
            bass_db: db(sum(self.bass_bins)),
            mid_db: db(sum(self.mid_bins)),
            treble_db: db(sum(self.treble_bins)),
            rms_db: 20.0 * (rms + 1e-9).log10(),
            bands_db,
            flux,
            bass_flux,
        }
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
