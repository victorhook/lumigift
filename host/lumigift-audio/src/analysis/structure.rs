//! Section boundaries (Foote novelty on smoothed block features) and drop
//! detection (sharp energy rise after a quieter build-up).

use super::dsp::{FLUX_FLOOR_DB, N_BANDS, mean, moving_average, percentile, std_dev};
use crate::features::FeatureFrame;

/// Energy curve used for drops and section energy (see [`FeatureFrame::energy`]).
pub fn energy_curve(frames: &[FeatureFrame]) -> Vec<f32> {
    frames.iter().map(FeatureFrame::energy).collect()
}

/// A detected drop: frame index and strength (energy contrast, 0..1).
pub struct DropCandidate {
    pub frame: f64,
    pub strength: f32,
}

/// Drops: points where the energy over the next ~3 s clearly exceeds the
/// energy over the previous ~6 s, the build-up was quieter, and the music
/// after is loud. The exact time is the steepest rise of the energy.
pub fn detect_drops(energy: &[f32], fps: f64) -> Vec<DropCandidate> {
    let n = energy.len();
    let pre_len = (6.0 * fps) as usize;
    let post_len = (3.0 * fps) as usize;
    let gap = (0.15 * fps) as usize;
    if n < pre_len + post_len + 2 * gap {
        return Vec::new();
    }
    let smooth = moving_average(energy, (0.1 * fps) as usize);
    let mut prefix = vec![0f64; n + 1];
    for i in 0..n {
        prefix[i + 1] = prefix[i] + smooth[i] as f64;
    }
    let avg = |a: usize, b: usize| ((prefix[b] - prefix[a]) / (b - a).max(1) as f64) as f32;
    let mut contrast = vec![0f32; n];
    for (i, c) in contrast
        .iter_mut()
        .enumerate()
        .take(n - post_len - gap)
        .skip(pre_len + gap)
    {
        let pre = avg(i - pre_len - gap, i - gap);
        let post = avg(i + gap, i + gap + post_len);
        if post > 0.45 && pre < 0.75 * post {
            *c = post - pre;
        }
    }
    let half = (4.0 * fps) as usize;
    let mut out: Vec<DropCandidate> = Vec::new();
    let min_spacing = 12.0 * fps;
    let mut i = 0;
    while i < n {
        let c = contrast[i];
        if c > 0.25 {
            let a = i.saturating_sub(half);
            let b = (i + half + 1).min(n);
            let is_max = contrast[a..b].iter().all(|&v| v <= c);
            if is_max {
                // Steepest rise of the fast energy near the contrast peak.
                let w = (0.75 * fps) as usize;
                let (lo, hi) = (i.saturating_sub(w).max(1), (i + w).min(n - 1));
                let mut best = i;
                let mut best_d = f32::NEG_INFINITY;
                let k = (0.05 * fps).max(1.0) as usize;
                for j in lo..hi {
                    let after = mean(&energy[j..(j + k).min(n)]);
                    let before = mean(&energy[j.saturating_sub(k)..j]);
                    let d = after - before;
                    if d > best_d {
                        best_d = d;
                        best = j;
                    }
                }
                let cand = DropCandidate {
                    frame: best as f64,
                    strength: c.min(1.0),
                };
                // Within `min_spacing` of the previous drop the stronger wins.
                match out.last_mut() {
                    Some(last) if best as f64 - last.frame < min_spacing => {
                        if c > last.strength {
                            *last = cand;
                        }
                    }
                    _ => out.push(cand),
                }
                i += half;
                continue;
            }
        }
        i += 1;
    }
    out
}

/// Section boundaries (frame indices, excluding 0 and the end) from a Foote
/// checkerboard novelty curve over 0.5 s blocks.
pub fn section_boundaries(
    frames: &[FeatureFrame],
    bands: &[[f32; N_BANDS]],
    fps: f64,
    min_section_s: f64,
) -> Vec<usize> {
    let block = ((0.5 * fps) as usize).max(1);
    let nblk = frames.len() / block;
    if nblk < 8 {
        return Vec::new();
    }
    // Block features: 8 coarse spectral bands + 5 scalar features.
    const COARSE: usize = 8;
    let dim = COARSE + 5;
    let mut feats = vec![0f32; nblk * dim];
    for b in 0..nblk {
        let rows = b * block..(b + 1) * block;
        let row = &mut feats[b * dim..(b + 1) * dim];
        for i in rows.clone() {
            for (k, v) in bands[i].iter().enumerate() {
                row[k * COARSE / N_BANDS] += v.max(FLUX_FLOOR_DB);
            }
            let f = &frames[i];
            row[COARSE] += f.bass;
            row[COARSE + 1] += f.mid;
            row[COARSE + 2] += f.treble;
            row[COARSE + 3] += f.level;
            row[COARSE + 4] += f.onset;
        }
        row.iter_mut().for_each(|v| *v /= block as f32);
    }
    // z-score each dimension; scalar features weigh as much as the spectrum.
    for d in 0..dim {
        let col: Vec<f32> = (0..nblk).map(|b| feats[b * dim + d]).collect();
        let (m, s) = (mean(&col), std_dev(&col).max(1e-4));
        let w = if d < COARSE { 0.6 } else { 1.0 };
        for b in 0..nblk {
            feats[b * dim + d] = (feats[b * dim + d] - m) / s * w;
        }
    }
    let dist = |a: usize, b: usize| -> f32 {
        let (x, y) = (
            &feats[a * dim..(a + 1) * dim],
            &feats[b * dim..(b + 1) * dim],
        );
        x.iter()
            .zip(y)
            .map(|(p, q)| (p - q) * (p - q))
            .sum::<f32>()
            .sqrt()
    };
    // Checkerboard kernel with Gaussian taper, half width 8 s.
    let l = ((8.0 * fps) as usize / block).max(2) as isize;
    let sigma = l as f32 / 2.0;
    let mut nov = vec![0f32; nblk];
    for (i, nv) in nov.iter_mut().enumerate() {
        let (mut cross, mut wc, mut within, mut ww) = (0f32, 0f32, 0f32, 0f32);
        for a in -l..l {
            for b in (a + 1)..l {
                let (ia, ib) = (i as isize + a, i as isize + b);
                if ia < 0 || ib < 0 || ia >= nblk as isize || ib >= nblk as isize {
                    continue;
                }
                let ga = ((a as f32 + 0.5) / sigma).powi(2);
                let gb = ((b as f32 + 0.5) / sigma).powi(2);
                let w = (-0.5 * (ga + gb)).exp();
                let d = dist(ia as usize, ib as usize);
                if (a < 0) != (b < 0) {
                    cross += w * d;
                    wc += w;
                } else {
                    within += w * d;
                    ww += w;
                }
            }
        }
        if wc > 0.0 && ww > 0.0 {
            *nv = (cross / wc - within / ww).max(0.0);
        }
    }
    let nov = moving_average(&nov, 1);
    // Peak picking: local maxima above an adaptive threshold, strongest first,
    // at least `min_section_s` apart.
    let mut sorted = nov.clone();
    let med = percentile(&mut sorted, 50.0);
    let p90 = percentile(&mut sorted, 90.0);
    let thr = med + 0.15 * (p90 - med);
    let w = ((3.0 * fps) as usize / block).max(1);
    let mut peaks: Vec<(usize, f32)> = (0..nblk)
        .filter(|&i| {
            let a = i.saturating_sub(w);
            let b = (i + w + 1).min(nblk);
            nov[i] > thr && nov[a..b].iter().all(|&v| v <= nov[i])
        })
        .map(|i| (i, nov[i]))
        .collect();
    peaks.sort_by(|a, b| b.1.total_cmp(&a.1));
    let min_blocks = (min_section_s * fps / block as f64) as usize;
    let mut chosen: Vec<usize> = Vec::new();
    for (i, _) in peaks {
        if i < min_blocks / 2 || i + min_blocks / 2 > nblk {
            continue;
        }
        if chosen.iter().all(|&c| c.abs_diff(i) >= min_blocks) {
            chosen.push(i);
        }
    }
    chosen.sort_unstable();
    chosen.into_iter().map(|b| b * block).collect()
}
