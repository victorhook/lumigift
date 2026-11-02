//! Onset detection for the light show: musically significant attacks only.
//!
//! The beat tracker keeps using the plain spectral flux (it wants every
//! periodic event). For ONSET flags we want what a listener would clap to,
//! so this detector differs in three ways:
//!
//! 1. **Vibrato-suppressed flux** (after Böck & Widmer's SuperFlux, 2013):
//!    the reference frame is max-filtered across neighbouring log bands, so a
//!    partial wobbling between two bands (vibrato, bow pressure changes) does
//!    not count as new energy. Real attacks raise many bands at once.
//! 2. **Adaptive threshold:** a peak must clear the local median plus a
//!    multiple of the local median absolute deviation (MAD) over a ±1 s
//!    window, plus an absolute floor. Busy passages raise their own bar.
//! 3. **Minimum inter-onset interval** ([`MIN_IOI_S`]): of two candidates
//!    closer than that, the stronger wins.
//!
//! Every onset carries a strength in 0..=1: `1 - s*`, where `s*` is the
//! lowest sensitivity at which its peak clears the threshold. The engine
//! flags an onset when `strength >= 1 - sensitivity`, so the default
//! sensitivity of 0.5 shows exactly the onsets above `median + 4 · MAD +
//! 0.12`, and the show's sensitivity setting moves the threshold
//! continuously without re-analysing.

use super::dsp::{FLUX_FLOOR_DB, N_BANDS, percentile};
use super::{ONSET_TIME_CORRECTION, Onset};

/// Reference-frame lag of the flux, frames (~10 ms each).
const LAG: usize = 1;
/// Lower bound of the normalisation reference (dB per band). Very smooth
/// material (p99 below this) is not stretched, so tiny level wobbles of a
/// pure legato tone cannot reach the threshold. Real recordings sit well
/// above it (solo violin 1.2, jazz 2.9, drums 17).
const MIN_REFERENCE: f32 = 1.0;
/// Half-width of the threshold window, seconds.
const THRESH_WINDOW_S: f64 = 1.0;
/// The threshold at sensitivity `s` (0..=1) on the p99-normalised curve is
/// `median + k(s) · MAD + floor(s)`, with `k` and `floor` interpolated
/// linearly from the `s = 0` (strict) to the `s = 1` (permissive) values.
/// `s = 0.5`, the default, is `median + 4 · MAD + 0.12`.
const MAD_K: (f32, f32) = (6.0, 2.0);
const ABS_FLOOR: (f32, f32) = (0.20, 0.04);
/// A peak must be the maximum within ± this many seconds.
const PEAK_WINDOW_S: f64 = 0.05;
/// Frames after a candidate that may make it count as non-silent.
const ATTACK_FRAMES: usize = 4;
/// Minimum time between two onsets, seconds.
pub const MIN_IOI_S: f64 = 0.09;

/// Vibrato-suppressed onset detection function, normalised so the 99th
/// percentile of non-silent frames is 1 (see [`MIN_REFERENCE`]).
pub fn detection_function(bands: &[[f32; N_BANDS]], silent: &[bool]) -> Vec<f32> {
    let n = bands.len();
    let floor = |v: f32| v.max(FLUX_FLOOR_DB);
    let mut odf = vec![0f32; n];
    for i in LAG..n {
        let (cur, prev) = (&bands[i], &bands[i - LAG]);
        let mut s = 0.0;
        for (k, &c) in cur.iter().enumerate() {
            let lo = k.saturating_sub(1);
            let hi = (k + 1).min(N_BANDS - 1);
            let reference = prev[lo..=hi]
                .iter()
                .copied()
                .map(floor)
                .fold(f32::MIN, f32::max);
            s += (floor(c) - reference).max(0.0);
        }
        odf[i] = s / N_BANDS as f32;
    }
    let mut active: Vec<f32> = odf
        .iter()
        .zip(silent)
        .filter(|(_, s)| !**s)
        .map(|(v, _)| *v)
        .collect();
    let r = percentile(&mut active, 99.0).max(MIN_REFERENCE);
    odf.iter_mut().for_each(|v| *v = (*v / r).min(1.5));
    odf
}

/// Pick onsets from `odf` (see the module docs).
pub fn pick(odf: &[f32], silent: &[bool], fps: f64) -> Vec<Onset> {
    let n = odf.len();
    let half = (THRESH_WINDOW_S * fps).round() as usize;
    let w_peak = (PEAK_WINDOW_S * fps).round().max(1.0) as usize;
    let min_gap = (MIN_IOI_S * fps).round().max(1.0) as usize;

    // Local median and MAD, evaluated every `step` frames and held (the
    // statistics of a 2 s window barely change within 50 ms).
    let step = 5;
    let mut med = vec![0f32; n];
    let mut mad = vec![0f32; n];
    let mut buf = Vec::with_capacity(2 * half + 1);
    let mut i = 0;
    while i < n {
        let a = i.saturating_sub(half);
        let b = (i + half + 1).min(n);
        buf.clear();
        buf.extend_from_slice(&odf[a..b]);
        let m = percentile(&mut buf, 50.0);
        buf.iter_mut().for_each(|v| *v = (*v - m).abs());
        let d = percentile(&mut buf, 50.0);
        for j in i..(i + step).min(n) {
            med[j] = m;
            mad[j] = d;
        }
        i += step;
    }

    // Candidates: local maxima that clear the threshold at some sensitivity.
    let mut cands: Vec<(usize, f32)> = Vec::new();
    for i in 1..n {
        // The attack frame of a quiet hit after a decayed tail can itself be
        // below the silence gate: only skip frames that stay silent.
        if silent[i..(i + ATTACK_FRAMES).min(n)].iter().all(|s| *s) {
            continue;
        }
        let s = required_sensitivity(odf[i], med[i], mad[i]);
        if s > 1.0 {
            continue;
        }
        let a = i.saturating_sub(w_peak);
        let b = (i + w_peak + 1).min(n);
        if odf[a..b].iter().any(|&v| v > odf[i]) || odf[a..i].contains(&odf[i]) {
            continue;
        }
        // Unclamped (may exceed 1), so two very strong peaks still compare.
        let strength = 1.0 - s;
        // Minimum inter-onset interval: the stronger of two close peaks wins.
        match cands.last_mut() {
            Some(last) if i - last.0 < min_gap => {
                if strength > last.1 {
                    *last = (i, strength);
                }
            }
            _ => cands.push((i, strength)),
        }
    }
    cands
        .into_iter()
        .map(|(i, strength)| Onset {
            time: i as f64 / fps + ONSET_TIME_CORRECTION,
            strength: strength.clamp(0.0, 1.0),
        })
        .collect()
}

/// The lowest sensitivity `s` at which `v` clears
/// `med + k(s) · mad + floor(s)` (may be < 0 or > 1).
fn required_sensitivity(v: f32, med: f32, mad: f32) -> f32 {
    let strict = med + MAD_K.0 * mad + ABS_FLOOR.0;
    let per_unit = (MAD_K.0 - MAD_K.1) * mad + (ABS_FLOOR.0 - ABS_FLOOR.1);
    (strict - v) / per_unit
}
