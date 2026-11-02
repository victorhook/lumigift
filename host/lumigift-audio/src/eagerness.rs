//! "Eagerness" metrics: how jumpy a show looks on the gifts.
//!
//! [`measure`] plays a show through [`ShowEngine`] and one
//! [`lumigift_fx::Renderer`] per gift at exactly 100 Hz (the radio rate), as
//! a gift would see it, and counts what a viewer perceives as nervous
//! flashing. Used by `examples/eagerness.rs` and the sync tests, so tuning
//! is measured rather than judged by feel.

use std::sync::Arc;

use lumigift_fx::Renderer;
use lumigift_proto::{LEDS_PER_GIFT, Rgb, music_flags};

use crate::engine::ShowEngine;
use crate::features::FeatureProvider;
use crate::show::Show;

/// Gifts simulated by [`measure`].
pub const GIFTS: u8 = 10;
/// A "jump" is a brightness change larger than this fraction of full scale...
pub const JUMP_FRACTION: f32 = 0.4;
/// ...within this many 10 ms frames (50 ms).
pub const JUMP_WINDOW_FRAMES: usize = 5;

/// Result of [`measure`]. Rates are per minute of audio.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Eagerness {
    /// Frames carrying the BEAT flag.
    pub beats_per_min: f64,
    /// Frames carrying the ONSET flag.
    pub onsets_per_min: f64,
    /// Brightness jumps of more than [`JUMP_FRACTION`] within 50 ms, per gift.
    pub jumps_per_min: f64,
    /// Mean absolute brightness change between consecutive 10 ms frames,
    /// percent of full scale, averaged over gifts.
    pub mean_abs_delta_pct: f64,
    /// Pearson correlation between the gifts' mean brightness and the
    /// track's loudness (feature `level`), both averaged per second, over
    /// non-silent seconds: does the show follow the phrasing and dynamics?
    pub dynamics_correlation: f64,
}

/// Perceived brightness of a gift, 0..=1: the mean over its LEDs of the
/// brightest channel (HSV "value", independent of the palette's hue).
pub fn gift_brightness(leds: &[Rgb; LEDS_PER_GIFT]) -> f32 {
    leds.iter()
        .map(|c| c.r.max(c.g).max(c.b) as f32 / 255.0)
        .sum::<f32>()
        / LEDS_PER_GIFT as f32
}

/// Render `show` over the whole of `provider` (which must have a duration)
/// at 100 Hz and measure it. Deterministic.
pub fn measure(provider: Arc<dyn FeatureProvider>, show: Show) -> Eagerness {
    let duration = provider.duration().unwrap_or(0.0);
    let mut engine = ShowEngine::new(provider.clone(), show);
    let mut level = Vec::new();
    let mut renderers: Vec<Renderer> = (0..GIFTS).map(|s| Renderer::new(s as u32 + 7)).collect();
    let n = (duration * 100.0).floor() as usize;
    let mut history: Vec<Vec<f32>> = vec![Vec::with_capacity(n); GIFTS as usize];
    let (mut beats, mut onsets) = (0usize, 0usize);
    for i in 0..n {
        let t = i as f64 / 100.0;
        let ms = i as u32 * 10;
        let f = engine.frame_at(t, GIFTS);
        let feat = provider.features_at(t);
        level.push(if feat.is_silent() {
            f32::NAN
        } else {
            feat.level
        });
        beats += (f.flags & music_flags::BEAT != 0) as usize;
        onsets += (f.flags & music_flags::ONSET != 0) as usize;
        for (slot, r) in renderers.iter_mut().enumerate() {
            r.on_frame(&f, ms);
            let mut out = [Rgb::BLACK; LEDS_PER_GIFT];
            r.render(ms, slot as u8, &mut out);
            history[slot].push(gift_brightness(&out));
        }
    }
    let minutes = (duration / 60.0).max(1e-6);
    let mut jumps = 0usize;
    let mut delta_sum = 0.0f64;
    let mut delta_n = 0usize;
    for h in &history {
        let mut i = JUMP_WINDOW_FRAMES;
        while i < h.len() {
            let w = &h[i - JUMP_WINDOW_FRAMES..=i];
            let (lo, hi) = w
                .iter()
                .fold((f32::MAX, f32::MIN), |(l, u), &v| (l.min(v), u.max(v)));
            if hi - lo > JUMP_FRACTION {
                jumps += 1;
                // One jump per window: skip past it.
                i += JUMP_WINDOW_FRAMES;
            } else {
                i += 1;
            }
        }
        for w in h.windows(2) {
            delta_sum += (w[1] - w[0]).abs() as f64;
            delta_n += 1;
        }
    }
    // Per-second means for the dynamics correlation.
    let per_sec = |v: &[f32]| -> Vec<f64> {
        v.chunks(100)
            .map(|c| c.iter().map(|&x| x as f64).sum::<f64>() / c.len() as f64)
            .collect()
    };
    let mean_bright: Vec<f32> = (0..n)
        .map(|i| history.iter().map(|h| h[i]).sum::<f32>() / GIFTS as f32)
        .collect();
    let (xs, ys): (Vec<f64>, Vec<f64>) = per_sec(&mean_bright)
        .into_iter()
        .zip(per_sec(&level))
        .filter(|(_, l)| l.is_finite())
        .unzip();
    Eagerness {
        dynamics_correlation: pearson(&xs, &ys),
        beats_per_min: beats as f64 / minutes,
        onsets_per_min: onsets as f64 / minutes,
        jumps_per_min: jumps as f64 / GIFTS as f64 / minutes,
        mean_abs_delta_pct: 100.0 * delta_sum / delta_n.max(1) as f64,
    }
}

fn pearson(x: &[f64], y: &[f64]) -> f64 {
    let n = x.len().min(y.len());
    if n < 3 {
        return 0.0;
    }
    let mx = x.iter().sum::<f64>() / n as f64;
    let my = y.iter().sum::<f64>() / n as f64;
    let (mut sxy, mut sxx, mut syy) = (0.0, 0.0, 0.0);
    for i in 0..n {
        let (a, b) = (x[i] - mx, y[i] - my);
        sxy += a * b;
        sxx += a * a;
        syy += b * b;
    }
    if sxx <= 1e-12 || syy <= 1e-12 {
        0.0
    } else {
        sxy / (sxx * syy).sqrt()
    }
}
