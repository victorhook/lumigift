//! Data for drawing: a dense waveform overview and a spectrogram-style lane.

use serde::{Deserialize, Serialize};

/// One waveform bucket.
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
pub struct WaveBucket {
    pub min: f32,
    pub max: f32,
    pub rms: f32,
}

/// Dense min/max/RMS overview of the mono mixdown. Use
/// [`Waveform::buckets_in`] to get exactly as many buckets as there are
/// pixels for any visible time range.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Waveform {
    /// Buckets per second.
    pub buckets_per_second: f64,
    pub buckets: Vec<WaveBucket>,
}

/// Samples per waveform bucket at 44.1/48 kHz (≈ 172–188 buckets/s).
const BUCKET: usize = 256;

impl Waveform {
    pub fn from_mono(mono: &[f32], sample_rate: u32) -> Self {
        let buckets = mono
            .chunks(BUCKET)
            .map(|c| {
                let (mut lo, mut hi, mut sq) = (f32::MAX, f32::MIN, 0f32);
                for &s in c {
                    lo = lo.min(s);
                    hi = hi.max(s);
                    sq += s * s;
                }
                WaveBucket {
                    min: lo,
                    max: hi,
                    rms: (sq / c.len() as f32).sqrt(),
                }
            })
            .collect();
        Self {
            buckets_per_second: sample_rate as f64 / BUCKET as f64,
            buckets,
        }
    }

    /// Aggregate the time range `[t0, t1)` into `n` buckets (for one row of
    /// pixels). Buckets outside the track are zero.
    pub fn buckets_in(&self, t0: f64, t1: f64, n: usize) -> Vec<WaveBucket> {
        let mut out = vec![WaveBucket::default(); n];
        if n == 0 || t1 <= t0 || self.buckets.is_empty() {
            return out;
        }
        let len = self.buckets.len() as f64;
        for (i, o) in out.iter_mut().enumerate() {
            let a = (t0 + (t1 - t0) * i as f64 / n as f64) * self.buckets_per_second;
            let b = (t0 + (t1 - t0) * (i + 1) as f64 / n as f64) * self.buckets_per_second;
            if b <= 0.0 || a >= len {
                continue;
            }
            let ia = a.max(0.0).floor() as usize;
            let ib = (b.min(len).ceil() as usize)
                .max(ia + 1)
                .min(self.buckets.len());
            let (mut lo, mut hi, mut sq) = (f32::MAX, f32::MIN, 0f32);
            for w in &self.buckets[ia..ib] {
                lo = lo.min(w.min);
                hi = hi.max(w.max);
                sq += w.rms * w.rms;
            }
            *o = WaveBucket {
                min: lo,
                max: hi,
                rms: (sq / (ib - ia) as f32).sqrt(),
            };
        }
        out
    }
}

/// Downsampled log-band energies for a spectrogram-style timeline lane.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Spectrogram {
    /// Columns per second (~50).
    pub columns_per_second: f64,
    /// Bands per column (low to high frequency).
    pub bands: usize,
    /// Band edges in Hz (`bands + 1` values).
    pub band_edges_hz: Vec<f32>,
    /// Column-major `u8` intensities: column `c`, band `b` is `data[c * bands + b]`.
    pub data: Vec<u8>,
}

impl Spectrogram {
    pub fn columns(&self) -> usize {
        self.data.len() / self.bands.max(1)
    }

    /// One column (low to high frequency), or `None` past the end.
    pub fn column(&self, c: usize) -> Option<&[u8]> {
        self.data.get(c * self.bands..(c + 1) * self.bands)
    }

    /// Column at time `t` (clamped).
    pub fn column_at(&self, t: f64) -> Option<&[u8]> {
        let n = self.columns();
        if n == 0 {
            return None;
        }
        let c = ((t * self.columns_per_second).max(0.0) as usize).min(n - 1);
        self.column(c)
    }
}
