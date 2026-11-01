//! Tempo estimation (windowed autocorrelation + Viterbi over tempo), beat
//! tracking (dynamic programming after Ellis 2007, with a time-varying
//! period) and downbeat (bar phase) estimation.

use super::dsp::{
    FLUX_FLOOR_DB, N_BANDS, mean, moving_average, parabolic_offset, percentile, std_dev,
};

const MIN_BPM: f64 = 60.0;
const MAX_BPM: f64 = 200.0;
/// Centre / width (octaves) of the log-normal tempo prior.
const PRIOR_BPM: f64 = 120.0;
const PRIOR_OCTAVES: f64 = 1.0;
/// Tempogram window length and hop (seconds).
const TG_WINDOW_S: f64 = 8.0;
const TG_HOP_S: f64 = 1.0;
/// Viterbi cost of a tempo change, per squared octave.
const TEMPO_CHANGE_COST: f64 = 100.0;
/// DP tightness (Ellis' alpha).
const TIGHTNESS: f32 = 100.0;

/// Estimate a smoothly varying beat period (in frames) for every frame of
/// `onset`, from the tempogram.
pub fn estimate_tempo(onset: &[f32], fps: f64) -> Vec<f32> {
    let n = onset.len();
    let lag_min = (60.0 * fps / MAX_BPM).floor().max(1.0) as usize;
    let lag_max = (60.0 * fps / MIN_BPM).ceil() as usize;
    let n_lags = lag_max - lag_min + 1;
    let win = (TG_WINDOW_S * fps) as usize;
    let hop = ((TG_HOP_S * fps) as usize).max(1);
    let n_win = n.div_ceil(hop).max(1);

    let prior: Vec<f64> = (lag_min..=lag_max)
        .map(|l| {
            let bpm = 60.0 * fps / l as f64;
            let o = (bpm / PRIOR_BPM).log2() / PRIOR_OCTAVES;
            (-0.5 * o * o).exp()
        })
        .collect();

    // Emission scores (log) and raw ACF per window.
    let mut emis = vec![0f64; n_win * n_lags];
    let mut acfs = vec![0f64; n_win * n_lags];
    let hann: Vec<f64> = (0..win)
        .map(|i| 0.5 - 0.5 * (2.0 * std::f64::consts::PI * i as f64 / win as f64).cos())
        .collect();
    let mut seg = vec![0f64; win];
    for j in 0..n_win {
        let c = j * hop;
        let a = c as isize - win as isize / 2;
        let m: f64 = {
            let (lo, hi) = (
                a.max(0) as usize,
                ((a + win as isize).max(0) as usize).min(n),
            );
            if hi > lo {
                onset[lo..hi].iter().map(|&v| v as f64).sum::<f64>() / (hi - lo) as f64
            } else {
                0.0
            }
        };
        for (i, s) in seg.iter_mut().enumerate() {
            let k = a + i as isize;
            *s = if k >= 0 && (k as usize) < n {
                (onset[k as usize] as f64 - m) * hann[i]
            } else {
                0.0
            };
        }
        let e0: f64 = seg.iter().map(|v| v * v).sum();
        let row = &mut acfs[j * n_lags..(j + 1) * n_lags];
        if e0 > 1e-9 {
            for (li, l) in (lag_min..=lag_max).enumerate() {
                let s: f64 = seg[..win - l]
                    .iter()
                    .zip(&seg[l..])
                    .map(|(x, y)| x * y)
                    .sum();
                row[li] = s / e0 * win as f64 / (win - l) as f64;
            }
        }
        let erow = &mut emis[j * n_lags..(j + 1) * n_lags];
        for li in 0..n_lags {
            erow[li] = (row[li].max(0.0) * prior[li] + 0.02).ln();
        }
    }

    // Viterbi over lags.
    let lag_log2: Vec<f64> = (lag_min..=lag_max).map(|l| (l as f64).log2()).collect();
    let mut trans = vec![0f64; n_lags * n_lags];
    for a in 0..n_lags {
        for b in 0..n_lags {
            let d = lag_log2[a] - lag_log2[b];
            trans[a * n_lags + b] = -TEMPO_CHANGE_COST * d * d;
        }
    }
    let mut score = emis[..n_lags].to_vec();
    let mut back = vec![0u16; n_win * n_lags];
    let mut next = vec![0f64; n_lags];
    for j in 1..n_win {
        for b in 0..n_lags {
            let mut best = f64::NEG_INFINITY;
            let mut arg = 0;
            for a in 0..n_lags {
                let v = score[a] + trans[a * n_lags + b];
                if v > best {
                    best = v;
                    arg = a;
                }
            }
            next[b] = best + emis[j * n_lags + b];
            back[j * n_lags + b] = arg as u16;
        }
        std::mem::swap(&mut score, &mut next);
    }
    let mut path = vec![0usize; n_win];
    path[n_win - 1] = (0..n_lags)
        .max_by(|&a, &b| score[a].total_cmp(&score[b]))
        .unwrap_or(0);
    for j in (1..n_win).rev() {
        path[j - 1] = back[j * n_lags + path[j]] as usize;
    }

    // Sub-lag refinement, then interpolate to every frame.
    let win_period: Vec<f32> = path
        .iter()
        .enumerate()
        .map(|(j, &li)| {
            let row = &acfs[j * n_lags..(j + 1) * n_lags];
            let off = if li > 0 && li + 1 < n_lags {
                parabolic_offset(row[li - 1] as f32, row[li] as f32, row[li + 1] as f32)
            } else {
                0.0
            };
            (lag_min + li) as f32 + off
        })
        .collect();
    (0..n)
        .map(|i| {
            let x = i as f32 / hop as f32;
            let j = (x.floor() as usize).min(n_win - 1);
            let k = (j + 1).min(n_win - 1);
            let f = (x - j as f32).clamp(0.0, 1.0);
            win_period[j] * (1.0 - f) + win_period[k] * f
        })
        .collect()
}

/// Dynamic-programming beat tracker over frames `[start, end)` of `onset`
/// using the per-frame `period`. Returns fractional beat frame positions.
pub fn track_beats(onset: &[f32], period: &[f32], start: usize, end: usize) -> Vec<f64> {
    let n = end.saturating_sub(start);
    if n < 4 {
        return Vec::new();
    }
    let o = &onset[start..end];
    let sd = std_dev(o).max(1e-6);
    // Light smoothing makes the DP robust to 1-frame jitter.
    let local: Vec<f32> = {
        let k = [0.25f32, 0.5, 0.25];
        (0..n)
            .map(|i| {
                let g = |j: isize| o[(j.clamp(0, n as isize - 1)) as usize];
                (k[0] * g(i as isize - 1) + k[1] * g(i as isize) + k[2] * g(i as isize + 1)) / sd
            })
            .collect()
    };
    let mut cum = vec![0f32; n];
    let mut back = vec![usize::MAX; n];
    for i in 0..n {
        let p = period[start + i].max(2.0);
        let lo = (p / 2.0).round().max(1.0) as usize;
        let hi = (2.0 * p).round() as usize;
        let mut best = f32::NEG_INFINITY;
        let mut arg = usize::MAX;
        if i >= lo {
            for lag in lo..=hi.min(i) {
                let r = (lag as f32 / p).ln();
                let v = cum[i - lag] - TIGHTNESS * r * r;
                if v > best {
                    best = v;
                    arg = i - lag;
                }
            }
        }
        if arg != usize::MAX && best > 0.0 {
            cum[i] = local[i] + best;
            back[i] = arg;
        } else {
            cum[i] = local[i];
        }
    }
    // Last beat: the best-scoring local maximum of the cumulative score
    // within the final two beat periods (on-grid beats accumulate more score
    // than off-beat peaks in the tail).
    let maxima: Vec<usize> = (1..n.saturating_sub(1))
        .filter(|&i| cum[i] > cum[i - 1] && cum[i] >= cum[i + 1])
        .collect();
    let Some(&final_max) = maxima.last() else {
        return Vec::new();
    };
    let tail_from = final_max.saturating_sub((2.0 * period[start + final_max]) as usize);
    let last = maxima
        .iter()
        .copied()
        .filter(|&i| i >= tail_from)
        .max_by(|&a, &b| cum[a].total_cmp(&cum[b]))
        .unwrap_or(final_max);
    let mut beats = vec![last];
    let mut i = last;
    while back[i] != usize::MAX {
        i = back[i];
        beats.push(i);
    }
    beats.reverse();

    // Trim weak leading/trailing beats (librosa-style).
    let rms = (beats.iter().map(|&b| local[b] * local[b]).sum::<f32>() / beats.len() as f32).sqrt();
    let thr = 0.5 * rms;
    let first_ok = beats.iter().position(|&b| local[b] >= thr).unwrap_or(0);
    let last_ok = beats
        .iter()
        .rposition(|&b| local[b] >= thr)
        .unwrap_or(beats.len() - 1);
    let beats = &beats[first_ok..=last_ok.max(first_ok)];

    // Sub-frame refinement on the smoothed onset curve.
    beats
        .iter()
        .map(|&b| {
            let off = if b > 0 && b + 1 < n && local[b] >= local[b - 1] && local[b] >= local[b + 1]
            {
                parabolic_offset(local[b - 1], local[b], local[b + 1]) as f64
            } else {
                0.0
            };
            (start + b) as f64 + off
        })
        .collect()
}

/// Choose the bar phase (0..4) of a run of beats.
///
/// Per beat it combines low-frequency onset strength (kick), bass energy and
/// the harmonic change between the four beats before and after (which is
/// largest on bar lines because drum patterns cancel out over a full bar).
/// Returns the index (0..4) of the first downbeat within `beat_frames`.
#[allow(clippy::needless_range_loop)] // k indexes a band inside nested arrays.
pub fn downbeat_phase(
    beat_frames: &[usize],
    bass_flux: &[f32],
    bass: &[f32],
    bands: &[[f32; N_BANDS]],
) -> usize {
    let nb = beat_frames.len();
    if nb < 8 {
        return 0;
    }
    let nf = bass_flux.len();
    let win = |f: usize, a: isize, b: isize| {
        let lo = (f as isize + a).clamp(0, nf as isize - 1) as usize;
        let hi = (f as isize + b).clamp(lo as isize + 1, nf as isize) as usize;
        lo..hi
    };
    let kick: Vec<f32> = beat_frames
        .iter()
        .map(|&f| bass_flux[win(f, -2, 4)].iter().copied().fold(0.0, f32::max))
        .collect();
    let low: Vec<f32> = beat_frames
        .iter()
        .map(|&f| mean(&bass[win(f, 0, 12)]))
        .collect();
    // Mean band spectrum per inter-beat interval.
    let seg_mean: Vec<[f32; N_BANDS]> = (0..nb)
        .map(|i| {
            let a = beat_frames[i];
            let b = if i + 1 < nb {
                beat_frames[i + 1]
            } else {
                (a + (a - beat_frames[i - 1])).min(nf)
            };
            let b = b.max(a + 1).min(nf);
            let mut m = [0f32; N_BANDS];
            for row in &bands[a..b] {
                for (x, y) in m.iter_mut().zip(row) {
                    *x += y.max(FLUX_FLOOR_DB);
                }
            }
            let inv = 1.0 / (b - a).max(1) as f32;
            m.iter_mut().for_each(|x| *x *= inv);
            m
        })
        .collect();
    let change: Vec<f32> = (0..nb)
        .map(|i| {
            if i < 4 || i + 4 > nb {
                return f32::NAN;
            }
            let mut d = 0.0;
            for k in 0..N_BANDS {
                let before: f32 = (i - 4..i).map(|j| seg_mean[j][k]).sum::<f32>() / 4.0;
                let after: f32 = (i..i + 4).map(|j| seg_mean[j][k]).sum::<f32>() / 4.0;
                d += (after - before).abs();
            }
            d / N_BANDS as f32
        })
        .collect();
    let z = |v: &[f32]| -> Vec<f32> {
        let valid: Vec<f32> = v.iter().copied().filter(|x| x.is_finite()).collect();
        let (m, s) = (mean(&valid), std_dev(&valid).max(1e-6));
        v.iter()
            .map(|x| if x.is_finite() { (x - m) / s } else { 0.0 })
            .collect()
    };
    let (kz, lz, cz) = (z(&kick), z(&low), z(&change));
    let mut score = [0f32; 4];
    let mut count = [0f32; 4];
    for i in 0..nb {
        score[i % 4] += kz[i] + 0.5 * lz[i] + 1.5 * cz[i];
        count[i % 4] += 1.0;
    }
    for k in 0..4 {
        score[k] /= count[k].max(1.0);
    }
    (0..4)
        .max_by(|&a, &b| score[a].total_cmp(&score[b]))
        .unwrap_or(0)
}

/// Contiguous non-silent regions `[start, end)` in frames, merging gaps
/// shorter than `min_gap` frames and dropping regions shorter than `min_len`.
pub fn active_regions(silent: &[bool], min_gap: usize, min_len: usize) -> Vec<(usize, usize)> {
    let mut regions: Vec<(usize, usize)> = Vec::new();
    let mut i = 0;
    let n = silent.len();
    while i < n {
        if silent[i] {
            i += 1;
            continue;
        }
        let s = i;
        while i < n && !silent[i] {
            i += 1;
        }
        match regions.last_mut() {
            Some(last) if s - last.1 < min_gap => last.1 = i,
            _ => regions.push((s, i)),
        }
    }
    regions.retain(|(a, b)| b - a >= min_len);
    regions
}

/// Local tempo (BPM) at each beat from the median of nearby inter-beat intervals.
pub fn local_bpm(beat_times: &[f64]) -> Vec<f32> {
    let n = beat_times.len();
    if n < 2 {
        return vec![0.0; n];
    }
    let ibi: Vec<f64> = beat_times.windows(2).map(|w| w[1] - w[0]).collect();
    (0..n)
        .map(|i| {
            let a = i.saturating_sub(4);
            let b = (i + 4).min(ibi.len());
            let mut v: Vec<f32> = ibi[a..b].iter().map(|&x| x as f32).collect();
            let m = percentile(&mut v, 50.0);
            if m > 0.0 { 60.0 / m } else { 0.0 }
        })
        .collect()
}

/// Smooth an onset curve for beat tracking: subtract a slow local mean.
pub fn beat_onset_curve(flux: &[f32], fps: f64) -> Vec<f32> {
    let slow = moving_average(flux, (0.5 * fps) as usize);
    flux.iter()
        .zip(&slow)
        .map(|(f, s)| (f - s).max(0.0))
        .collect()
}
