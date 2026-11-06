//! LED effects for Lumigift, in integer math so the exact same code runs on
//! the Cortex-M0+ gifts and in the host's live preview / show simulator.
//!
//! Usage: call [`Renderer::on_frame_for`] whenever a [`MusicFrame`] arrives
//! and [`Renderer::render`] at the LED refresh rate (~100-200 Hz). Rendering
//! is a pure function of the frames received, the slot and the (synced)
//! clock, so every gift renders identically to the preview.
//!
//! # Effects
//!
//! Ids 0..[`effect::COUNT`] are the classic music-show set; the rest
//! ([`effect::FADE`] ..) are generic lighting looks. [`EFFECTS`] describes
//! each one for UIs (name, which parameters matter, defaults, whether it
//! reacts to music). The generic looks are purely time-based: they animate
//! from the synced clock and the gift's slot, so every gift runs in lockstep
//! with no per-frame radio traffic beyond the parameters. They work with all
//! music fields at 0; a few react to audio when it is present (marked
//! [`Music::Optional`]).
//!
//! Parameters: `param[0]` is the speed and `param[1]` the size / width /
//! density; 0 means the effect's default ([`PARAM_DEFAULTS`]). Speed `s`
//! advances `1 + s²/128` / 65536 cycles per ms (a 22 s cycle at `s = 16`,
//! 0.5 s at 128, 0.13 s at 255).
//!
//! A gift always lights its four LEDs in one colour: they sit together in a
//! frosted box, where different colours only blend into a muddy mix.
//! Patterns move across the gifts instead. Spatial looks (Knight Rider,
//! Comet, Waterfall, Theater chase, Colour wipe, the Spectrum equaliser)
//! treat the gifts in slot order as one row: all `slot_count` slots for a
//! frame to everyone, or only the gifts in the frame's mask for a group
//! frame (so a Knight Rider on a group bounces across that group). Per-slot
//! offsets (Fade spread, Blink alternate, …) follow the same positions.
//!
//! # Groups, custom colours, crossfades
//!
//! * [`MusicFrame::mask`]: a gift only applies frames addressed to its slot
//!   ([`Renderer::on_frame_for`]); other frames just keep it alive, so it
//!   keeps rendering its previous look.
//! * [`PALETTE_CUSTOM`]: the frame's two colours as a primary → secondary →
//!   primary gradient (hue 0 = the primary colour exactly).
//! * [`MusicFrame::transition`]: when the look changes, the renderer renders
//!   the old and the new look and blends them over `transition × 50 ms`.
//!   Sending brightness 0 with a transition is a fade to black.
#![no_std]

use lumigift_proto::{LEDS_PER_GIFT, MusicFrame, Rgb, music_flags};
pub use lumigift_proto::{MASK_ALL, PALETTE_CUSTOM, TRANSITION_UNIT_MS};

pub mod effect {
    pub const SOLID: u8 = 0;
    pub const PULSE: u8 = 1;
    pub const SPECTRUM: u8 = 2;
    pub const CHASE: u8 = 3;
    pub const STROBE: u8 = 4;
    pub const RAINBOW: u8 = 5;
    pub const SPARKLE: u8 = 6;
    pub const BREATHE: u8 = 7;
    /// Number of classic music-show effects (ids `0..COUNT`), the set the
    /// show generator and cue editor use. See [`ALL`] for every effect.
    pub const COUNT: u8 = 8;
    /// Smooth colour cycling through the palette, optionally offset per slot.
    pub const FADE: u8 = 8;
    /// A colour wave flowing across the gifts.
    pub const WATERFALL: u8 = 9;
    /// Knight Rider: a head with a fading tail bouncing across all gifts.
    pub const SCANNER: u8 = 10;
    /// On/off; `param[1] >= 128` alternates odd/even gifts.
    pub const BLINK: u8 = 11;
    /// Random (deterministic per slot) twinkles over a dim background.
    pub const TWINKLE: u8 = 12;
    /// A single head with a tail lapping around all gifts.
    pub const COMET: u8 = 13;
    pub const THEATER: u8 = 14;
    /// Fire / candle flicker.
    pub const FIRE: u8 = 15;
    /// Two colours alternating; `param[1] >= 128` swaps smoothly instead of
    /// double flashes.
    pub const POLICE: u8 = 16;
    /// A strobe at a fixed rate, independent of beats.
    pub const STROBE_TIMED: u8 = 17;
    /// A decaying pulse at a fixed rate, independent of beats.
    pub const PULSE_TIMED: u8 = 18;
    /// Fills the row gift by gift with the next colour.
    pub const WIPE: u8 = 19;
    /// Number of effects (ids `0..ALL`).
    pub const ALL: u8 = 20;
}

/// Display names, indexed by effect id.
pub const EFFECT_NAMES: [&str; effect::ALL as usize] = {
    let mut n = [""; effect::ALL as usize];
    let mut i = 0;
    while i < n.len() {
        n[i] = EFFECTS[i].name;
        i += 1;
    }
    n
};

/// How an effect relates to the music fields of the frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Music {
    /// Purely time-based.
    None,
    /// Time-based; also reacts to audio when a show is playing.
    Optional,
    /// Needs a playing show (beats, bands, onsets) to look like anything.
    Required,
}

/// What a UI needs to know about an effect.
#[derive(Debug, Clone, Copy)]
pub struct EffectInfo {
    pub name: &'static str,
    /// Label of `param[0]`, `None` if the effect ignores it.
    pub speed: Option<&'static str>,
    /// Label of `param[1]`, `None` if the effect ignores it.
    pub size: Option<&'static str>,
    /// `param[1]` is an on/off option (`>= 128` = on) rather than a range.
    pub size_is_toggle: bool,
    /// Uses a second colour (palette position `hue + 128`, or the custom
    /// secondary colour).
    pub two_colors: bool,
    /// Flashes (blinks, strobes): UIs may cap the speed for photosensitivity.
    pub flashing: bool,
    pub music: Music,
}

impl EffectInfo {
    const fn new(
        name: &'static str,
        speed: Option<&'static str>,
        size: Option<&'static str>,
        music: Music,
    ) -> Self {
        Self {
            name,
            speed,
            size,
            size_is_toggle: false,
            two_colors: false,
            flashing: false,
            music,
        }
    }
    const fn toggle(mut self) -> Self {
        self.size_is_toggle = true;
        self
    }
    const fn two(mut self) -> Self {
        self.two_colors = true;
        self
    }
    const fn flashing(mut self) -> Self {
        self.flashing = true;
        self
    }
}

const SPEED: Option<&str> = Some("Speed");

/// Metadata, indexed by effect id (host side; the firmware only uses
/// [`PARAM_DEFAULTS`], so none of this ends up in the gift's flash).
pub const EFFECTS: [EffectInfo; effect::ALL as usize] = [
    EffectInfo::new("Solid", None, None, Music::None),
    EffectInfo::new("Pulse", None, None, Music::Required),
    EffectInfo::new("Spectrum", None, None, Music::Required),
    EffectInfo::new("Chase", None, None, Music::Required),
    EffectInfo::new("Strobe", None, None, Music::Required).flashing(),
    EffectInfo::new("Rainbow", SPEED, None, Music::Optional),
    EffectInfo::new("Sparkle", None, None, Music::Required),
    EffectInfo::new("Breathe", SPEED, Some("Depth"), Music::None),
    EffectInfo::new("Fade", SPEED, Some("Spread"), Music::None),
    EffectInfo::new("Waterfall", SPEED, Some("Length"), Music::None),
    EffectInfo::new("Knight Rider", SPEED, Some("Tail"), Music::Optional),
    EffectInfo::new("Blink", SPEED, Some("Alternate"), Music::None)
        .toggle()
        .two()
        .flashing(),
    EffectInfo::new("Twinkle", SPEED, Some("Density"), Music::Optional),
    EffectInfo::new("Comet", SPEED, Some("Tail"), Music::Optional),
    EffectInfo::new("Theater Chase", SPEED, Some("Spacing"), Music::None).two(),
    EffectInfo::new("Fire", SPEED, Some("Flicker"), Music::Optional),
    EffectInfo::new("Police", SPEED, Some("Smooth"), Music::None)
        .toggle()
        .two()
        .flashing(),
    EffectInfo::new("Timed Strobe", Some("Rate"), Some("Duty"), Music::None).flashing(),
    EffectInfo::new("Timed Pulse", Some("Rate"), Some("Glow"), Music::None),
    EffectInfo::new("Colour Wipe", SPEED, None, Music::None).two(),
];

/// `param` values used when the frame sends 0, indexed by effect id.
pub const PARAM_DEFAULTS: [[u8; 2]; effect::ALL as usize] = [
    [0, 0],
    [0, 0],
    [0, 0],
    [0, 0],
    [0, 0],
    [0, 0],
    [0, 0],
    [0, 0],
    [40, 1],
    [60, 64],
    [50, 64],
    [80, 64],
    [70, 80],
    [60, 96],
    [70, 64],
    [160, 140],
    [90, 64],
    [150, 24],
    [100, 40],
    [60, 0],
];

/// The effective `[speed, size]` of a frame (0 replaced by the defaults).
pub fn effective_params(f: &MusicFrame) -> [u8; 2] {
    let d = PARAM_DEFAULTS
        .get(f.effect as usize)
        .copied()
        .unwrap_or([0, 0]);
    [
        if f.param[0] == 0 { d[0] } else { f.param[0] },
        if f.param[1] == 0 { d[1] } else { f.param[1] },
    ]
}

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
    /// Sparkle envelope and colour of the gift.
    sparkle: u16,
    sparkle_hue: u8,
    /// Fade-out level when the signal is lost.
    presence: u16,
    rng: u32,
    /// The look being crossfaded away from.
    prev: MusicFrame,
    xfade_start_ms: u32,
    /// 0 = no crossfade running.
    xfade_len_ms: u32,
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
            sparkle: 0,
            sparkle_hue: 0,
            presence: 0,
            rng: seed | 1,
            prev: MusicFrame::default(),
            xfade_start_ms: 0,
            xfade_len_ms: 0,
        }
    }

    pub fn frame(&self) -> &MusicFrame {
        &self.frame
    }

    /// Feed a received frame if it addresses `slot` (see
    /// [`MusicFrame::targets`]); returns whether it was applied. A frame for
    /// other gifts only keeps the signal alive, so this gift keeps rendering
    /// its previous look.
    pub fn on_frame_for(&mut self, f: &MusicFrame, now_ms: u32, slot: u8) -> bool {
        if f.targets(slot) {
            self.on_frame(f, now_ms);
            true
        } else {
            self.keepalive(now_ms);
            false
        }
    }

    /// A show frame for someone else arrived: the show is still running.
    pub fn keepalive(&mut self, now_ms: u32) {
        if self.have_frame {
            self.last_frame_ms = now_ms;
        }
    }

    /// Feed a received frame (ignoring its mask). `now_ms` is the local
    /// (synced) clock.
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
            // Each gift sparkles on about half the onsets (own random
            // stream), so the row twinkles instead of flashing in unison.
            if self.next_rand() & 1 == 0 {
                self.sparkle = 65535;
                self.sparkle_hue = self.next_rand() as u8;
            }
        }
        if f.flags & music_flags::DROP != 0 {
            self.drop_env = 65535;
        }
        if self.have_frame && look_changed(&self.frame, f) {
            if f.transition == 0 {
                self.xfade_len_ms = 0;
            } else {
                // Mid-fade, fade from whichever look dominates right now.
                if self.xfade_progress(now_ms) >= 128 {
                    self.prev = self.frame;
                    self.xfade_start_ms = now_ms;
                }
                self.xfade_len_ms = f.transition as u32 * TRANSITION_UNIT_MS;
            }
        }
        self.last_phase = f.beat_phase;
        self.frame = *f;
        self.have_frame = true;
        self.last_frame_ms = now_ms;
    }

    /// True while a crossfade between two looks is running.
    pub fn transitioning(&self, now_ms: u32) -> bool {
        self.xfade_progress(now_ms) < 256
    }

    /// 0..=256 (256 = done / none).
    fn xfade_progress(&self, now_ms: u32) -> u32 {
        let el = now_ms.wrapping_sub(self.xfade_start_ms);
        if self.xfade_len_ms == 0 || el >= self.xfade_len_ms {
            256
        } else {
            el * 256 / self.xfade_len_ms
        }
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

        let f = &self.frame;
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

        self.paint(&self.frame, now_ms, slot, out);
        let k = self.xfade_progress(now_ms);
        if k < 256 {
            let mut old = [Rgb::BLACK; LEDS_PER_GIFT];
            self.paint(&self.prev, now_ms, slot, &mut old);
            let k = k as u8;
            for (o, p) in out.iter_mut().zip(old.iter()) {
                *o = blend(*p, *o, k);
            }
        } else {
            self.xfade_len_ms = 0;
        }

        let presence = (self.presence >> 8) as u8;
        for px in out.iter_mut() {
            *px = scale8(*px, presence);
        }
    }

    /// One look (effect + palette + params + brightness) at time `t`.
    fn paint(&self, f: &MusicFrame, t: u32, slot: u8, out: &mut [Rgb; LEDS_PER_GIFT]) {
        let custom = f.palette == PALETTE_CUSTOM;
        let pal = if custom {
            let mid = blend(f.colors[0], f.colors[1], 128);
            [f.colors[0], mid, f.colors[1], mid]
        } else {
            PALETTES[(f.palette as usize) % PALETTES.len()]
        };
        let pal = &pal;
        // Position in the row: among all `slot_count` slots, or, for a
        // group frame (mask), among the gifts of the group only.
        let (slot_idx, slots) = if f.mask != MASK_ALL && slot < 32 && f.mask & (1 << slot) != 0 {
            (
                (f.mask & ((1u32 << slot) - 1)).count_ones(),
                f.mask.count_ones(),
            )
        } else {
            let n = f.slot_count.max(1) as u32;
            (slot as u32 % n, n)
        };
        let slot_pos = (slot_idx * 256 / slots) as u8;
        let beat8 = (self.beat_env >> 8) as u8;
        let [speed, size] = effective_params(f);
        // 16.16 fixed-point cycles: wraps continuously.
        let p = t.wrapping_mul(1 + speed as u32 * speed as u32 / 128);
        let phase8 = (p >> 8) as u8;
        let frac16 = p & 0xFFFF;
        let hue = f.hue;
        let first = palette(pal, hue);
        let second = palette(pal, hue.wrapping_add(128));
        // The four LEDs of a gift always show one colour (they blend in the
        // frosted box anyway); patterns move across the gifts. Positions
        // along the row are 8.8 fixed point in gifts.
        let row = slots * 256;
        let x = (slot_idx * 256) as i32;
        // Head of the lapping effects (Comet, Colour wipe).
        let lap = ((frac16 * slots) >> 8) as i32;

        let px = match f.effect {
            effect::SOLID => first,
            effect::PULSE => {
                let base = scale8(
                    palette(pal, hue.wrapping_add(slot_pos / 4)),
                    40 + (self.bands[0] >> 8) as u8 / 2,
                );
                let flash = palette(pal, hue.wrapping_add(96));
                add(base, scale8(flash, beat8))
            }
            effect::SPECTRUM => {
                // An equaliser across the row: bass at one end, treble at
                // the other (a lone gift shows the loudest band).
                let v = if slots < 3 {
                    self.bands.iter().map(|b| (b >> 8) as u8).max().unwrap_or(0)
                } else {
                    let pos = slot_idx * 3 * 256 / (slots - 1);
                    let (k, frac) = ((pos >> 8) as usize, (pos & 255) as u8);
                    let a = (self.bands[k.min(3)] >> 8) as u8;
                    let b = (self.bands[(k + 1).min(3)] >> 8) as u8;
                    blend8(a, b, frac)
                };
                scale8(palette(pal, hue.wrapping_add(slot_pos / 2)), qadd8(v, beat8 / 4))
            }
            effect::CHASE => {
                // A bright head travels across all slots once per beat.
                let d = f.beat_phase.wrapping_sub(slot_pos);
                let tail = 255u8.saturating_sub(d.saturating_mul(3));
                let c = palette(pal, hue.wrapping_add(slot_pos));
                add(scale8(c, tail), scale8(c, 20 + (self.bands[0] >> 9) as u8))
            }
            effect::STROBE => {
                let v = qadd8(beat8, (self.onset_env >> 8) as u8 / 2);
                let white = Rgb::new(255, 240, 220);
                scale8(
                    blend(white, palette(pal, hue), (self.drop_env >> 8) as u8),
                    v,
                )
            }
            effect::RAINBOW => {
                let drift = if f.param[0] == 0 {
                    (t / 12) as u8
                } else {
                    phase8
                };
                let h = hue.wrapping_add(drift).wrapping_add(slot_pos);
                scale8(hsv(h, 240, 255), qadd8(60, qadd8(f.level / 2, beat8 / 2)))
            }
            effect::SPARKLE => {
                let s = (self.sparkle >> 8) as u8;
                let bg = scale8(palette(pal, hue.wrapping_add(slot_pos)), 25);
                add(bg, scale8(palette(pal, self.sparkle_hue), s))
            }
            effect::BREATHE => {
                // Slow sine on the gift, phase-shifted per slot.
                let base = if f.param[0] == 0 {
                    (t / 16) as u8
                } else {
                    phase8
                };
                let ph = base.wrapping_add(slot_pos / 2);
                let (floor, depth) = if f.param[1] == 0 {
                    (30, 200)
                } else {
                    (
                        230 - scale8u(230, f.param[1]),
                        scale8u(225, f.param[1]) + 25,
                    )
                };
                scale8(first, floor + scale8u(sin8(ph), depth))
            }
            effect::FADE => palette(
                pal,
                hue.wrapping_add(phase8)
                    .wrapping_add(scale8u(slot_pos, size)),
            ),
            effect::WATERFALL => {
                // A wave flowing across the gifts (about 1.5–8 gifts long).
                let w = phase8.wrapping_sub((slot_idx * (8 + size as u32)) as u8);
                let s = sin8(w);
                scale8(
                    palette(pal, hue.wrapping_add(w)),
                    60 + scale8u(scale8u(s, s), 195),
                )
            }
            effect::SCANNER => {
                let up = frac16 < 0x8000;
                let tri = if up { frac16 } else { 0xFFFF - frac16 };
                let span = row.saturating_sub(256).max(1);
                let head = ((tri * span) >> 15) as i32;
                let rel = if up { head - x } else { x - head };
                let v = tail(rel, 192 + size as i32 * 4);
                scale8(first, qadd8(v, scale8u(v, beat8 / 2)))
            }
            effect::BLINK => {
                let odd = size >= 128 && slot_idx & 1 == 1;
                if ((p >> 15) & 1 == 1) != odd {
                    Rgb::BLACK
                } else if odd {
                    second
                } else {
                    first
                }
            }
            effect::TWINKLE => {
                // Each gift runs its own staggered cycle; a hash of
                // (cycle, gift) decides whether it twinkles in it.
                let q = p.wrapping_add(hash(slot_idx ^ 0xA5A5_0000));
                let h = hash((q >> 16).wrapping_mul(0x9E37_79B1) ^ slot_idx);
                let density = qadd8(size, (self.onset_env >> 9) as u8);
                let bg = scale8(palette(pal, hue.wrapping_add(slot_pos)), 14);
                if (h as u8) < density {
                    let fr = (q >> 8) as u8;
                    let env = if fr < 32 {
                        fr * 8
                    } else {
                        (((255 - fr as u32) * 292) >> 8) as u8
                    };
                    add(
                        bg,
                        scale8(
                            palette(pal, hue.wrapping_add((h >> 8) as u8)),
                            scale8u(env, env),
                        ),
                    )
                } else {
                    bg
                }
            }
            effect::COMET => {
                let mut rel = lap - x;
                if rel < -256 {
                    rel += row as i32;
                }
                let v = tail(rel, 192 + size as i32 * 6);
                scale8(
                    palette(pal, hue.wrapping_add((p >> 12) as u8)),
                    qadd8(v, scale8u(v, beat8 / 2)),
                )
            }
            effect::THEATER => {
                let spacing = 2 + size as u32 / 64;
                if slot_idx % spacing == (p >> 13) % spacing {
                    palette(pal, hue.wrapping_add((slot_idx * 16) as u8))
                } else {
                    scale8(second, 10)
                }
            }
            effect::FIRE => {
                // (2 × octave 1 + octave 2) / 3
                let n = ((noise(slot_idx, p) as u32 * 2
                    + noise(slot_idx ^ 0x55, p.wrapping_mul(3)) as u32)
                    * 85)
                    >> 8;
                let heat = qadd8(255 - scale8u(255 - n as u8, size), f.level / 4);
                let c = palette(pal, if custom { heat / 2 } else { scale8u(heat, 191) });
                scale8(c, 60 + scale8u(heat, 195))
            }
            effect::POLICE => {
                let side = slot_idx & 1;
                let on = (p >> 15) & 1 == side && (size >= 128 || matches!((p >> 12) & 7, 0 | 2));
                match (on, side) {
                    (false, _) => Rgb::BLACK,
                    (true, 0) => first,
                    _ => second,
                }
            }
            effect::STROBE_TIMED => {
                if phase8 < 8 + size / 2 {
                    first
                } else {
                    Rgb::BLACK
                }
            }
            effect::PULSE_TIMED => {
                let e = 255 - phase8;
                let floor = size / 2;
                scale8(first, floor + scale8u(scale8u(e, e), 255 - floor))
            }
            effect::WIPE => {
                let cycle = (p >> 16) as u8;
                let step = if custom { 128 } else { 64 };
                let new = palette(pal, hue.wrapping_add(cycle.wrapping_mul(step)));
                let old = palette(
                    pal,
                    hue.wrapping_add(cycle.wrapping_sub(1).wrapping_mul(step)),
                );
                let k = (lap - x).clamp(0, 255);
                blend(old, new, k as u8)
            }
            _ => Rgb::BLACK,
        };
        *out = [px; LEDS_PER_GIFT];

        for px in out.iter_mut() {
            *px = scale8(*px, f.brightness);
        }
    }

    fn decay(&mut self, dt: u32) {
        // Beat flash ~180 ms, onset ~120 ms, drop ~1.5 s, sparkle ~400 ms.
        self.beat_env = (self.beat_env as u32).saturating_sub(dt * 364) as u16;
        self.onset_env = (self.onset_env as u32).saturating_sub(dt * 546) as u16;
        self.drop_env = (self.drop_env as u32).saturating_sub(dt * 44) as u16;
        self.sparkle = (self.sparkle as u32).saturating_sub(dt * 164) as u16;
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

/// Stateless preview: the look of `f` at time `t` for the gift at `slot`,
/// as a gift that has been receiving `f` for a while renders it (no
/// crossfade, no signal fade, music envelopes at rest). For UIs.
pub fn render_look(f: &MusicFrame, t: u32, slot: u8, out: &mut [Rgb; LEDS_PER_GIFT]) {
    Renderer::new(1).paint(f, t, slot, out);
}

/// Does going from `a` to `b` change what the gifts show (beyond audio)?
fn look_changed(a: &MusicFrame, b: &MusicFrame) -> bool {
    a.effect != b.effect
        || a.palette != b.palette
        || a.param != b.param
        || a.hue != b.hue
        || a.brightness != b.brightness
        || a.colors != b.colors
}

/// Brightness of a gift `rel` (8.8 gifts) behind a moving head: an
/// antialiased front edge, then a tail fading out over `len`.
fn tail(rel: i32, len: i32) -> u8 {
    let v = if rel < -256 || rel >= len {
        0
    } else if rel <= 0 {
        (256 + rel) * 255 / 256
    } else {
        (len - rel) * 255 / len
    };
    scale8u(v as u8, v as u8)
}

/// Integer hash (lowbias32).
fn hash(mut x: u32) -> u32 {
    x ^= x >> 16;
    x = x.wrapping_mul(0x7FEB_352D);
    x ^= x >> 15;
    x = x.wrapping_mul(0x846C_A68B);
    x ^ (x >> 16)
}

/// Smooth value noise for `seed` at 16.16 position `x`.
fn noise(seed: u32, x: u32) -> u8 {
    let k = (x >> 16).wrapping_mul(0x9E37_79B1) ^ seed;
    let a = hash(k) as u8;
    let b = hash(k.wrapping_add(0x9E37_79B1)) as u8;
    let f = (x >> 8) as u8;
    // Smoothstep 3f² - 2f³ = e + 2e(1 - f) with e = f².
    let e = scale8u(f, f);
    let t = scale8u(e, 255 - f);
    let f = qadd8(e, qadd8(t, t));
    ((a as u16 * (255 - f as u16) + b as u16 * f as u16) / 255) as u8
}

/// Scale colours so the four LEDs draw at most `budget_ma` (WS2812B: ~20 mA
/// per channel at full duty plus ~1 mA quiescent each).
pub fn power_limit(leds: &mut [Rgb], budget_ma: u16) {
    let sum: u32 = leds
        .iter()
        .map(|c| c.r as u32 + c.g as u32 + c.b as u32)
        .sum();
    let quiescent = leds.len() as u32;
    let led_ma = sum * 20 / 255;
    let budget = (budget_ma as u32).max(quiescent + 1);
    if led_ma + quiescent > budget {
        // Scale factor in 1/(255*256) units: the share of the LED draw the
        // budget allows.
        let s = ((budget - quiescent) * 255 * 256 / led_ma.max(1)).min(255 * 256);
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
#[inline(never)]
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

#[inline(never)]
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

/// Linear blend of two levels: 0 = a, 255 = b.
fn blend8(a: u8, b: u8, t: u8) -> u8 {
    ((a as u16 * (255 - t as u16) + b as u16 * t as u16) / 255) as u8
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
    fn every_effect_lights_a_gift_in_one_colour() {
        for e in 0..effect::ALL {
            for slot in 0..10u8 {
                let mut r = Renderer::new(slot as u32 + 3);
                let mut f = MusicFrame {
                    effect: e,
                    brightness: 255,
                    slot_count: 10,
                    bass: 200,
                    mid: 120,
                    treble: 60,
                    level: 150,
                    bpm: 120,
                    ..Default::default()
                };
                for k in 0..300u32 {
                    f.beat_phase = (k * 7) as u8;
                    f.flags = if k % 25 == 0 { music_flags::BEAT | music_flags::ONSET } else { 0 };
                    r.on_frame(&f, k * 10);
                    let mut out = [Rgb::BLACK; LEDS_PER_GIFT];
                    r.render(k * 10, slot, &mut out);
                    assert!(out.iter().all(|c| *c == out[0]), "effect {e} slot {slot} t {k}: {out:?}");
                }
            }
        }
    }

    fn lit(out: &[Rgb; 4]) -> bool {
        out.iter().any(|c| *c != Rgb::BLACK)
    }

    fn timed(effect: u8) -> MusicFrame {
        MusicFrame {
            effect,
            brightness: 255,
            slot_count: 6,
            ..Default::default()
        }
    }

    #[test]
    fn effects_render_without_panic_and_fade_out() {
        for e in 0..effect::ALL {
            for (slots, palette) in [(10, 0), (1, 3), (32, PALETTE_CUSTOM), (0, 7)] {
                let mut r = Renderer::new(1);
                let mut out = [Rgb::BLACK; 4];
                let mut f = MusicFrame {
                    effect: e,
                    brightness: 255,
                    bpm: 120,
                    slot_count: slots,
                    level: 200,
                    bass: 200,
                    palette,
                    colors: [Rgb::new(255, 0, 0), Rgb::new(0, 0, 255)],
                    param: [(e * 13) % 4 * 85, (e * 7) % 3 * 127],
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
                    r.render(t, (t % 40) as u8, &mut out);
                }
                // No frames for a second → dark.
                for t in 2000..3000 {
                    r.render(t, 0, &mut out);
                }
                assert_eq!(out, [Rgb::BLACK; 4], "effect {e} did not fade out");
            }
        }
    }

    #[test]
    fn generic_effects_animate_without_audio() {
        for e in effect::COUNT..effect::ALL {
            let f = timed(e);
            let mut r = Renderer::new(9);
            let mut out = [Rgb::BLACK; 4];
            let mut seen_lit = false;
            let mut distinct = 0;
            let mut last = [Rgb::BLACK; 4];
            // Slow default speeds: watch every slot for 20 s.
            for t in (0..20_000u32).step_by(10) {
                r.on_frame(&f, t);
                for slot in 0..6 {
                    r.render(t, slot, &mut out);
                    seen_lit |= lit(&out);
                }
                if out != last {
                    distinct += 1;
                    last = out;
                }
            }
            assert!(seen_lit, "{} never lights up", EFFECTS[e as usize].name);
            assert!(
                distinct > 4,
                "{} does not animate ({distinct})",
                EFFECTS[e as usize].name
            );
        }
    }

    #[test]
    fn renderers_run_in_lockstep() {
        // Two gifts at the same slot, fed the same frames at the same times,
        // render the same colours (different seeds: only the onset-driven
        // music Sparkle uses randomness).
        for e in 0..effect::ALL {
            let sparkle = e == effect::SPARKLE || (e + 1) % effect::ALL == effect::SPARKLE;
            let seed_b = if sparkle { 1 } else { 0xDEAD_BEEF };
            let mut a = Renderer::new(1);
            let mut b = Renderer::new(seed_b);
            let mut f = timed(e);
            f.palette = 4;
            let (mut oa, mut ob) = ([Rgb::BLACK; 4], [Rgb::BLACK; 4]);
            for t in (1_000_000..1_008_000u32).step_by(10) {
                f.flags = if t % 470 == 0 {
                    music_flags::ONSET | music_flags::BEAT
                } else {
                    0
                };
                a.on_frame_for(&f, t, 3);
                b.on_frame_for(&f, t, 3);
                if t == 1_004_000 {
                    f.effect = (e + 1) % effect::ALL;
                    f.transition = 20;
                }
                a.render(t, 3, &mut oa);
                b.render(t, 3, &mut ob);
                assert_eq!(oa, ob, "effect {e} at {t}");
            }
        }
    }

    #[test]
    fn mask_selects_gifts_and_others_keep_their_look() {
        let mut r = Renderer::new(3);
        let mut out = [Rgb::BLACK; 4];
        let red = MusicFrame {
            palette: PALETTE_CUSTOM,
            colors: [Rgb::new(200, 0, 0), Rgb::BLACK],
            ..timed(effect::SOLID)
        };
        let blue_for_2 = MusicFrame {
            colors: [Rgb::new(0, 0, 200), Rgb::BLACK],
            mask: MusicFrame::mask_of([2]),
            ..red
        };
        for t in (0..500).step_by(10) {
            assert!(r.on_frame_for(&red, t, 5));
            r.render(t, 5, &mut out);
        }
        assert_eq!(out, [Rgb::new(200, 0, 0); 4]);
        // Frames for slot 2 only: slot 5 keeps red, well past the timeout.
        for t in (500..3000).step_by(10) {
            assert!(!r.on_frame_for(&blue_for_2, t, 5));
            r.render(t, 5, &mut out);
        }
        assert_eq!(out, [Rgb::new(200, 0, 0); 4]);
        assert_eq!(r.frame().colors[0], Rgb::new(200, 0, 0));
        // A gift without a slot only takes frames for everyone.
        let mut u = Renderer::new(4);
        assert!(!u.on_frame_for(&blue_for_2, 0, lumigift_proto::UNASSIGNED_SLOT));
        assert!(u.on_frame_for(&red, 0, lumigift_proto::UNASSIGNED_SLOT));
        // Slot 2 does take it.
        let mut s2 = Renderer::new(5);
        assert!(s2.on_frame_for(&blue_for_2, 0, 2));
    }

    #[test]
    fn custom_palette_solid_is_the_primary_colour() {
        let mut r = Renderer::new(1);
        let mut out = [Rgb::BLACK; 4];
        let f = MusicFrame {
            palette: PALETTE_CUSTOM,
            colors: [Rgb::new(12, 200, 99), Rgb::new(1, 2, 3)],
            ..timed(effect::SOLID)
        };
        for t in (0..400).step_by(10) {
            r.on_frame(&f, t);
            r.render(t, 0, &mut out);
        }
        assert_eq!(out, [Rgb::new(12, 200, 99); 4]);
    }

    #[test]
    fn crossfade_blends_then_completes() {
        let mut r = Renderer::new(1);
        let mut out = [Rgb::BLACK; 4];
        let red = MusicFrame {
            palette: PALETTE_CUSTOM,
            colors: [Rgb::new(255, 0, 0), Rgb::BLACK],
            ..timed(effect::SOLID)
        };
        let blue = MusicFrame {
            colors: [Rgb::new(0, 0, 255), Rgb::BLACK],
            transition: 10, // 500 ms
            ..red
        };
        let mut t = 0;
        while t < 400 {
            r.on_frame(&red, t);
            r.render(t, 0, &mut out);
            t += 10;
        }
        let start = t;
        while t < start + 600 {
            r.on_frame(&blue, t);
            r.render(t, 0, &mut out);
            let el = t - start;
            if el == 250 {
                assert!(r.transitioning(t));
                let c = out[0];
                assert!(
                    (100..156).contains(&c.r) && (100..156).contains(&c.b),
                    "midway {c:?}"
                );
            }
            if el < 500 {
                assert!(out[0].r > 0, "at {el} ms");
            }
            t += 10;
        }
        assert!(!r.transitioning(t));
        assert_eq!(out, [Rgb::new(0, 0, 255); 4]);
        // transition 0 cuts immediately.
        let red_cut = MusicFrame {
            transition: 0,
            ..red
        };
        r.on_frame(&red_cut, t);
        r.render(t, 0, &mut out);
        assert_eq!(out, [Rgb::new(255, 0, 0); 4]);
    }

    #[test]
    fn fade_to_black_with_transition() {
        let mut r = Renderer::new(1);
        let mut out = [Rgb::BLACK; 4];
        let on = MusicFrame {
            palette: PALETTE_CUSTOM,
            colors: [Rgb::new(255, 255, 255), Rgb::BLACK],
            ..timed(effect::FADE)
        };
        let off = MusicFrame {
            brightness: 0,
            transition: 20, // 1 s
            ..on
        };
        for t in (0..400).step_by(10) {
            r.on_frame(&on, t);
            r.render(t, 0, &mut out);
        }
        let mut levels = [0u32; 3];
        for t in (400..1600).step_by(10) {
            r.on_frame(&off, t);
            r.render(t, 0, &mut out);
            let sum: u32 = out.iter().map(|c| c.r as u32).sum();
            match t {
                500 => levels[0] = sum,
                900 => levels[1] = sum,
                1390 => levels[2] = sum,
                _ => {}
            }
        }
        assert!(levels[0] > levels[1] && levels[1] > levels[2], "{levels:?}");
        assert!(levels[1] > 0);
        assert_eq!(out, [Rgb::BLACK; 4]);
    }

    #[test]
    fn scanner_bounces_across_all_gifts() {
        let mut r = Renderer::new(1);
        let mut out = [Rgb::BLACK; 4];
        let f = MusicFrame {
            param: [200, 0],
            ..timed(effect::SCANNER)
        };
        let mut peak = [0u8; 6];
        let mut max_lit_gifts = 0;
        for t in (0..5000u32).step_by(5) {
            r.on_frame(&f, t);
            let mut lit_gifts = 0;
            for slot in 0..6 {
                r.render(t, slot, &mut out);
                let m = out.iter().map(|c| c.r.max(c.g).max(c.b)).max().unwrap();
                peak[slot as usize] = peak[slot as usize].max(m);
                lit_gifts += (m > 0) as usize;
            }
            max_lit_gifts = max_lit_gifts.max(lit_gifts);
        }
        assert!(peak.iter().all(|&p| p > 200), "{peak:?}");
        // A head and a short tail: never the whole row at once.
        assert!(max_lit_gifts <= 3, "{max_lit_gifts}");
    }

    #[test]
    fn group_frames_span_only_the_group() {
        // A scanner on the group {2, 5, 9} of a 12-slot row visits each of
        // the three gifts; the head is on one of them most of the time.
        let f = MusicFrame {
            param: [200, 0],
            slot_count: 12,
            mask: MusicFrame::mask_of([2, 5, 9]),
            ..timed(effect::SCANNER)
        };
        let mut out = [Rgb::BLACK; 4];
        let mut peak = [0u8; 3];
        let mut dark = 0;
        for t in (0..3000u32).step_by(5) {
            let mut any = false;
            for (k, slot) in [2u8, 5, 9].into_iter().enumerate() {
                render_look(&f, t, slot, &mut out);
                let m = out.iter().map(|c| c.r.max(c.g).max(c.b)).max().unwrap();
                peak[k] = peak[k].max(m);
                any |= m > 0;
            }
            dark += !any as u32;
        }
        assert!(peak.iter().all(|&p| p > 200), "{peak:?}");
        assert!(dark < 30, "{dark}");
    }

    #[test]
    fn blink_alternates_odd_and_even_gifts() {
        let mut r = Renderer::new(1);
        let (mut a, mut b) = ([Rgb::BLACK; 4], [Rgb::BLACK; 4]);
        let sync = timed(effect::BLINK);
        let alt = MusicFrame {
            param: [0, 200],
            ..sync
        };
        let mut together = 0;
        let mut apart = 0;
        for t in (0..4000u32).step_by(10) {
            r.on_frame(&sync, t);
            r.render(t, 0, &mut a);
            r.render(t, 1, &mut b);
            together += (lit(&a) == lit(&b)) as u32;
        }
        let mut r = Renderer::new(1);
        for t in (0..4000u32).step_by(10) {
            r.on_frame(&alt, t);
            r.render(t, 0, &mut a);
            r.render(t, 1, &mut b);
            apart += (lit(&a) != lit(&b)) as u32;
        }
        assert!(together > 380, "{together}");
        assert!(apart > 380, "{apart}");
    }

    #[test]
    fn metadata_is_consistent() {
        assert_eq!(EFFECT_NAMES[effect::SCANNER as usize], "Knight Rider");
        assert_eq!(EFFECT_NAMES[effect::BREATHE as usize], "Breathe");
        for (i, info) in EFFECTS.iter().enumerate() {
            assert!(!info.name.is_empty());
            let d = PARAM_DEFAULTS[i];
            if i >= effect::COUNT as usize {
                assert!(info.speed.is_some() && d[0] > 0, "{}", info.name);
                assert_ne!(info.music, Music::Required);
            }
        }
        let f = MusicFrame {
            effect: effect::FADE,
            param: [0, 9],
            ..Default::default()
        };
        assert_eq!(effective_params(&f), [PARAM_DEFAULTS[8][0], 9]);
    }

    #[test]
    fn power_limit_caps_draw() {
        let mut leds = [Rgb::new(255, 255, 255); 4];
        power_limit(&mut leds, 100);
        let sum: u32 = leds
            .iter()
            .map(|c| c.r as u32 + c.g as u32 + c.b as u32)
            .sum();
        let draw = sum * 20 / 255 + 4;
        // Scaled down to the budget, not far below it (it once went black).
        assert!((95..=101).contains(&draw), "draw {draw}");
        for budget in [20u16, 60, 170, 240] {
            let mut leds = [Rgb::new(255, 255, 255); 4];
            power_limit(&mut leds, budget);
            let sum: u32 = leds.iter().map(|c| c.r as u32 + c.g as u32 + c.b as u32).sum();
            let draw = sum * 20 / 255 + 4;
            assert!(draw <= budget as u32 + 1 && draw + 3 >= budget as u32, "budget {budget}: draw {draw}");
        }
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
