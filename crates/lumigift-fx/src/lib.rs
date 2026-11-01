//! LED effects for Lumigift, in integer math so the exact same code runs on
//! the Cortex-M0+ gifts and in the host's live preview / show simulator.
//!
//! Usage: call [`Renderer::on_frame`] whenever a [`MusicFrame`] arrives and
//! [`Renderer::render`] at the LED refresh rate (~100-200 Hz). Rendering is
//! a pure function of the frames received, the slot and the (synced) clock,
//! so every gift renders identically to the preview.
#![no_std]

use lumigift_proto::{LEDS_PER_GIFT, MusicFrame, Rgb, music_flags};

pub mod effect {
    pub const SOLID: u8 = 0;
    pub const PULSE: u8 = 1;
    pub const SPECTRUM: u8 = 2;
    pub const CHASE: u8 = 3;
    pub const STROBE: u8 = 4;
    pub const RAINBOW: u8 = 5;
    pub const SPARKLE: u8 = 6;
    pub const BREATHE: u8 = 7;
    pub const COUNT: u8 = 8;
}

/// Display names, indexed by effect id.
pub const EFFECT_NAMES: [&str; effect::COUNT as usize] = [
    "Solid", "Pulse", "Spectrum", "Chase", "Strobe", "Rainbow", "Sparkle", "Breathe",
];

/// Four-stop gradients, sampled with [`palette`].
pub const PALETTES: [[Rgb; 4]; 8] = [
    // Christmas
    [
        Rgb::new(255, 20, 30),
        Rgb::new(255, 180, 60),
        Rgb::new(20, 200, 60),
        Rgb::new(255, 240, 220),
    ],
    // Fire
    [
        Rgb::new(120, 0, 0),
        Rgb::new(255, 40, 0),
        Rgb::new(255, 140, 0),
        Rgb::new(255, 230, 120),
    ],
    // Ocean
    [
        Rgb::new(0, 20, 80),
        Rgb::new(0, 90, 200),
        Rgb::new(0, 200, 220),
        Rgb::new(160, 255, 240),
    ],
    // Neon
    [
        Rgb::new(255, 0, 140),
        Rgb::new(120, 0, 255),
        Rgb::new(0, 160, 255),
        Rgb::new(0, 255, 160),
    ],
    // Aurora
    [
        Rgb::new(0, 255, 120),
        Rgb::new(0, 180, 200),
        Rgb::new(90, 60, 255),
        Rgb::new(200, 40, 200),
    ],
    // Sunset
    [
        Rgb::new(255, 90, 20),
        Rgb::new(255, 40, 80),
        Rgb::new(160, 20, 160),
        Rgb::new(40, 20, 120),
    ],
    // Candy
    [
        Rgb::new(255, 80, 160),
        Rgb::new(255, 200, 220),
        Rgb::new(120, 220, 255),
        Rgb::new(255, 255, 255),
    ],
    // Ice
    [
        Rgb::new(255, 255, 255),
        Rgb::new(180, 230, 255),
        Rgb::new(80, 160, 255),
        Rgb::new(20, 60, 200),
    ],
];

pub const PALETTE_NAMES: [&str; 8] = [
    "Christmas",
    "Fire",
    "Ocean",
    "Neon",
    "Aurora",
    "Sunset",
    "Candy",
    "Ice",
];

/// Frames older than this are treated as "no signal" and fade out.
pub const FRAME_TIMEOUT_MS: u32 = 600;

pub struct Renderer {
    frame: MusicFrame,
    have_frame: bool,
    last_frame_ms: u32,
    last_render_ms: u32,
    last_phase: u8,
    last_beat_ms: u32,
    /// 0..=65535 envelopes, decaying each render.
    beat_env: u16,
    onset_env: u16,
    drop_env: u16,
    /// Smoothed bands (8.8 fixed point).
    bands: [u16; 4],
    /// Sparkle state per LED.
    sparkle: [u16; LEDS_PER_GIFT],
    sparkle_hue: [u8; LEDS_PER_GIFT],
    /// Fade-out level when the signal is lost.
    presence: u16,
    rng: u32,
}

impl Renderer {
    pub fn new(seed: u32) -> Self {
        Self {
            frame: MusicFrame::default(),
            have_frame: false,
            last_frame_ms: 0,
            last_render_ms: 0,
            last_phase: 0,
            last_beat_ms: 0,
            beat_env: 0,
            onset_env: 0,
            drop_env: 0,
            bands: [0; 4],
            sparkle: [0; LEDS_PER_GIFT],
            sparkle_hue: [0; LEDS_PER_GIFT],
            presence: 0,
            rng: seed | 1,
        }
    }

    pub fn frame(&self) -> &MusicFrame {
        &self.frame
    }

    /// Feed a received frame. `now_ms` is the local (synced) clock.
    pub fn on_frame(&mut self, f: &MusicFrame, now_ms: u32) {
        let phase_wrapped = self.have_frame
            && f.bpm != 0
            && f.beat_phase < self.last_phase
            && self.last_phase - f.beat_phase > 128;
        let beat = f.flags & music_flags::BEAT != 0 || phase_wrapped;
        // Debounce: a beat flag and a phase wrap of the same beat count once.
        if beat && now_ms.wrapping_sub(self.last_beat_ms) > 120 {
            self.last_beat_ms = now_ms;
            self.beat_env = if f.flags & music_flags::DOWNBEAT != 0 {
                65535
            } else {
                52000
            };
        }
        if f.flags & music_flags::ONSET != 0 {
            self.onset_env = 65535;
            let i = (self.next_rand() % LEDS_PER_GIFT as u32) as usize;
            self.sparkle[i] = 65535;
            self.sparkle_hue[i] = self.next_rand() as u8;
        }
        if f.flags & music_flags::DROP != 0 {
            self.drop_env = 65535;
        }
        self.last_phase = f.beat_phase;
        self.frame = *f;
        self.have_frame = true;
        self.last_frame_ms = now_ms;
    }

    /// Render the four LEDs of the gift at `slot`.
    pub fn render(&mut self, now_ms: u32, slot: u8, out: &mut [Rgb; LEDS_PER_GIFT]) {
        let dt = now_ms.wrapping_sub(self.last_render_ms).min(100);
        self.last_render_ms = now_ms;
        self.decay(dt);

        let alive = self.have_frame && now_ms.wrapping_sub(self.last_frame_ms) < FRAME_TIMEOUT_MS;
        let target = if alive { 65535u32 } else { 0 };
        // ~250 ms fade in/out.
        let step = dt * 262;
        self.presence = approach(self.presence as u32, target, step) as u16;

        let f = self.frame;
        let raw = [f.bass, f.mid, avg(f.mid, f.treble), f.treble];
        for (b, &r) in self.bands.iter_mut().zip(raw.iter()) {
            // Fast attack, slower release, in 8.8.
            let t = (r as u32) << 8;
            let cur = *b as u32;
            *b = if t > cur {
                t
            } else {
                cur - ((cur - t) * dt.min(50) / 60)
            } as u16;
        }

        let pal = &PALETTES[(f.palette as usize) % PALETTES.len()];
        let slots = f.slot_count.max(1);
        let slot_pos = ((slot as u32 % slots as u32) * 256 / slots as u32) as u8;
        let beat8 = (self.beat_env >> 8) as u8;
        let t = now_ms;

        for (i, px) in out.iter_mut().enumerate() {
            let led_pos = (i as u8) * 64;
            *px = match f.effect {
                effect::SOLID => palette(pal, f.hue),
                effect::PULSE => {
                    let base = scale8(
                        palette(pal, f.hue.wrapping_add(slot_pos / 4)),
                        40 + (self.bands[0] >> 8) as u8 / 2,
                    );
                    let flash = palette(pal, f.hue.wrapping_add(96));
                    add(base, scale8(flash, beat8))
                }
                effect::SPECTRUM => {
                    let v = (self.bands[i] >> 8) as u8;
                    scale8(
                        palette(pal, led_pos.wrapping_add(f.hue)),
                        qadd8(v, beat8 / 4),
                    )
                }
                effect::CHASE => {
                    // A bright head travels across all slots once per beat.
                    let head = f.beat_phase;
                    let pos = slot_pos.wrapping_add((i as u8) * (64 / slots).max(1));
                    let d = head.wrapping_sub(pos);
                    let tail = 255u8.saturating_sub(d.saturating_mul(3));
                    let c = palette(pal, f.hue.wrapping_add(pos));
                    add(scale8(c, tail), scale8(c, 20 + (self.bands[0] >> 9) as u8))
                }
                effect::STROBE => {
                    let v = qadd8(beat8, (self.onset_env >> 8) as u8 / 2);
                    let white = Rgb::new(255, 240, 220);
                    scale8(
                        blend(
                            white,
                            palette(pal, f.hue),
                            (self.drop_env >> 8) as u8,
                        ),
                        v,
                    )
                }
                effect::RAINBOW => {
                    let h = f
                        .hue
                        .wrapping_add((t / 12) as u8)
                        .wrapping_add(slot_pos)
                        .wrapping_add(led_pos / 2);
                    scale8(hsv(h, 240, 255), qadd8(60, qadd8(f.level / 2, beat8 / 2)))
                }
                effect::SPARKLE => {
                    let s = (self.sparkle[i] >> 8) as u8;
                    let bg = scale8(palette(pal, f.hue.wrapping_add(slot_pos)), 25);
                    add(bg, scale8(palette(pal, self.sparkle_hue[i]), s))
                }
                _ => {
                    // Breathe: slow sine on the gift, phase-shifted per slot.
                    let ph = ((t / 16) as u8).wrapping_add(slot_pos / 2);
                    scale8(
                        palette(pal, f.hue.wrapping_add(led_pos / 4)),
                        30 + scale8u(sin8(ph), 200),
                    )
                }
            };
        }

        let bright = scale8u(f.brightness, (self.presence >> 8) as u8);
        for px in out.iter_mut() {
            *px = scale8(*px, bright);
        }
    }

    fn decay(&mut self, dt: u32) {
        // Beat flash ~180 ms, onset ~120 ms, drop ~1.5 s, sparkle ~400 ms.
        self.beat_env = (self.beat_env as u32).saturating_sub(dt * 364) as u16;
        self.onset_env = (self.onset_env as u32).saturating_sub(dt * 546) as u16;
        self.drop_env = (self.drop_env as u32).saturating_sub(dt * 44) as u16;
        for s in &mut self.sparkle {
            *s = (*s as u32).saturating_sub(dt * 164) as u16;
        }
    }

    fn next_rand(&mut self) -> u32 {
        // xorshift32
        let mut x = self.rng;
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        self.rng = x;
        x
    }
}

/// Scale colours so the four LEDs draw at most `budget_ma` (WS2812B: ~20 mA
/// per channel at full duty plus ~1 mA quiescent each).
pub fn power_limit(leds: &mut [Rgb], budget_ma: u16) {
    let sum: u32 = leds
        .iter()
        .map(|c| c.r as u32 + c.g as u32 + c.b as u32)
        .sum();
    let quiescent = leds.len() as u32;
    let draw = sum * 20 / 255 + quiescent;
    let budget = (budget_ma as u32).max(quiescent + 1);
    if draw > budget {
        let s = ((budget - quiescent) * 255 * 256 / (sum * 20 / 255 * 255).max(1)).min(255 * 256);
        for c in leds {
            c.r = (c.r as u32 * s / (255 * 256)) as u8;
            c.g = (c.g as u32 * s / (255 * 256)) as u8;
            c.b = (c.b as u32 * s / (255 * 256)) as u8;
        }
    }
}

/// Perceptual gamma (≈2.2) applied just before sending to the LEDs.
pub fn gamma(c: Rgb) -> Rgb {
    Rgb::new(
        GAMMA[c.r as usize],
        GAMMA[c.g as usize],
        GAMMA[c.b as usize],
    )
}

/// Sample a 4-stop gradient at `pos` (wraps around).
pub fn palette(p: &[Rgb; 4], pos: u8) -> Rgb {
    let seg = (pos >> 6) as usize;
    let frac = (pos & 63) << 2;
    blend(p[seg], p[(seg + 1) & 3], frac)
}

/// Linear blend: 0 = a, 255 = b.
pub fn blend(a: Rgb, b: Rgb, t: u8) -> Rgb {
    let l = |x: u8, y: u8| ((x as u16 * (255 - t as u16) + y as u16 * t as u16) / 255) as u8;
    Rgb::new(l(a.r, b.r), l(a.g, b.g), l(a.b, b.b))
}

pub fn scale8(c: Rgb, s: u8) -> Rgb {
    Rgb::new(scale8u(c.r, s), scale8u(c.g, s), scale8u(c.b, s))
}

pub fn add(a: Rgb, b: Rgb) -> Rgb {
    Rgb::new(qadd8(a.r, b.r), qadd8(a.g, b.g), qadd8(a.b, b.b))
}

#[inline]
pub fn scale8u(v: u8, s: u8) -> u8 {
    ((v as u16 * (s as u16 + 1)) >> 8) as u8
}

#[inline]
pub fn qadd8(a: u8, b: u8) -> u8 {
    a.saturating_add(b)
}

fn avg(a: u8, b: u8) -> u8 {
    ((a as u16 + b as u16) / 2) as u8
}

fn approach(cur: u32, target: u32, step: u32) -> u32 {
    if cur < target {
        (cur + step).min(target)
    } else {
        cur.saturating_sub(step).max(target)
    }
}

/// Rainbow HSV with 8-bit hue.
pub fn hsv(h: u8, s: u8, v: u8) -> Rgb {
    let region = h / 43;
    let rem = (h - region * 43) as u16 * 6;
    let p = scale8u(v, 255 - s);
    let q = ((v as u16 * (255 - ((s as u16 * rem) >> 8))) >> 8) as u8;
    let t = ((v as u16 * (255 - ((s as u16 * (255 - rem)) >> 8))) >> 8) as u8;
    match region {
        0 => Rgb::new(v, t, p),
        1 => Rgb::new(q, v, p),
        2 => Rgb::new(p, v, t),
        3 => Rgb::new(p, q, v),
        4 => Rgb::new(t, p, v),
        _ => Rgb::new(v, p, q),
    }
}

/// 8-bit sine: 0..=255 in, 0..=255 out (128 at 0).
pub fn sin8(x: u8) -> u8 {
    // Quarter-wave table, 16 entries + interpolation.
    const Q: [u8; 17] = [
        0, 12, 25, 37, 49, 60, 71, 81, 90, 98, 106, 112, 117, 122, 125, 127, 127,
    ];
    let quad = x >> 6;
    let i = (x & 63) as usize;
    let idx = if quad & 1 == 0 { i } else { 63 - i };
    let a = Q[idx >> 2] as u16;
    let b = Q[(idx >> 2) + 1] as u16;
    let v = (a + ((b - a) * (idx as u16 & 3)) / 4) as u8;
    if quad < 2 { 128 + v } else { 128 - v }
}

static GAMMA: [u8; 256] = {
    // Precomputed x^2.2 approximated by x^2 blended with x^3 (const-eval friendly).
    let mut t = [0u8; 256];
    let mut i = 0;
    while i < 256 {
        let x = i as u32;
        let sq = x * x / 255;
        let cu = x * x * x / (255 * 255);
        t[i] = ((sq * 4 + cu) / 5) as u8;
        if i > 0 && t[i] == 0 {
            t[i] = 1;
        }
        i += 1;
    }
    t
};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn effects_render_without_panic_and_fade_out() {
        for e in 0..effect::COUNT {
            let mut r = Renderer::new(1);
            let mut out = [Rgb::BLACK; 4];
            let mut f = MusicFrame {
                effect: e,
                brightness: 255,
                bpm: 120,
                slot_count: 10,
                level: 200,
                bass: 200,
                ..Default::default()
            };
            for t in 0..2000u32 {
                f.beat_phase = (t * 256 / 500) as u8;
                f.flags = if t % 500 == 0 {
                    music_flags::BEAT | music_flags::ONSET
                } else {
                    0
                };
                f.hue = t as u8;
                if t % 10 == 0 {
                    r.on_frame(&f, t);
                }
                r.render(t, (t % 10) as u8, &mut out);
            }
            // No frames for a second → dark.
            for t in 2000..3000 {
                r.render(t, 0, &mut out);
            }
            assert_eq!(out, [Rgb::BLACK; 4], "effect {e} did not fade out");
        }
    }

    #[test]
    fn power_limit_caps_draw() {
        let mut leds = [Rgb::new(255, 255, 255); 4];
        power_limit(&mut leds, 100);
        let sum: u32 = leds
            .iter()
            .map(|c| c.r as u32 + c.g as u32 + c.b as u32)
            .sum();
        assert!(sum * 20 / 255 + 4 <= 101, "draw {}", sum * 20 / 255 + 4);
        let mut dim = [Rgb::new(10, 0, 0); 4];
        power_limit(&mut dim, 100);
        assert_eq!(dim, [Rgb::new(10, 0, 0); 4]);
    }

    #[test]
    fn sin8_shape() {
        assert_eq!(sin8(0), 128);
        assert!(sin8(64) >= 254);
        assert!(sin8(192) <= 2);
    }
}
