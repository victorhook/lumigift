//! Turns features + a [`Show`] into [`MusicFrame`]s.
//!
//! # Timing / look-ahead
//!
//! Call [`ShowEngine::frame_at`] ~100 times per second with the time the
//! lights should represent **when they light up**. Radio and rendering take
//! time, so for file playback use
//!
//! ```text
//! t = player.position() + radio_latency
//! ```
//!
//! where `player.position()` is the media time being heard right now and
//! `radio_latency` is the delay from `frame_at` returning to the LEDs
//! changing (core queueing + USB + air time + the gift's render period,
//! typically 10–30 ms). [`ShowEngine::set_lookahead`] stores that value and
//! [`ShowEngine::frame_for_playback`] adds it for you. File analysis knows the
//! future, so this lines lights up exactly with the sound. Live providers are
//! causal: there the look-ahead should be 0 and the lights trail the audio by
//! the capture + analysis latency (or delay the audio, see the README).
//!
//! Only add the latency the frame really goes through: a frame rendered
//! locally (the Studio's 3D preview) needs no radio look-ahead, otherwise the
//! preview runs ahead of the sound by that much. Use one engine per
//! destination, or [`ShowEngine::frame_at`] for the radio and
//! [`ShowEngine::peek_frame_at`] with an [`EngineCursor`] for the preview.
//!
//! # Reactivity
//!
//! The show's [`ShowSettings`](crate::ShowSettings) shape how eagerly the
//! lights react, deterministically:
//!
//! * **Beat sync:** BEAT / DOWNBEAT flags, `beat_phase`, `beat_in_bar` and
//!   `bpm` come from the beat grid only where the provider's rhythm
//!   confidence is at least [`RHYTHM_THRESHOLD`] (`Auto`), always (`On`) or
//!   never (`Off`). Without sync the frame carries `bpm = 0` (so the gifts'
//!   phase-wrap beat detection stays off) and a slow free-running phase
//!   ([`FREE_PHASE_PERIOD`]) so phase-driven effects still move, calmly.
//! * **Sensitivity:** an ONSET is flagged only if the strongest onset in the
//!   frame has `strength >= 1 - sensitivity`.
//! * **Smoothing:** bass / mid / treble / level go through attack/release
//!   envelopes stepped on a fixed 10 ms grid (so the result does not depend
//!   on the call rate). Low rhythm confidence (legato) slows both.
//! * **Intensity:** the fast envelope is mixed with a slow phrase envelope:
//!   `slow + depth · (fast - slow)`. Quiet phrases stay quiet, loud ones
//!   loud, but jitter within a phrase is scaled by `depth` (lower for legato
//!   music). Effects that do not follow loudness themselves (Solid, Breathe,
//!   Sparkle) get their brightness modulated by the phrase envelope instead.

use std::sync::Arc;

use lumigift_fx::effect;
use lumigift_proto::{MusicFrame, music_flags};

use crate::features::{FeatureFrame, FeatureProvider};
use crate::show::{BeatSync, Cue, OneShotKind, Show, ShowSettings};

/// Largest forward step treated as continuous playback. A bigger jump (or
/// any backwards step) is a seek: edge detection restarts without replaying
/// the skipped events.
pub const MAX_CONTINUOUS_STEP: f64 = 0.25;
/// Event window used for the first frame after a reset / seek, seconds.
pub const FRAME_PERIOD: f64 = 0.01;
/// Normalised level below which (or an absolute RMS below
/// [`crate::features::SILENCE_DB`]) the SILENT flag is set.
const SILENT_LEVEL: f32 = 0.01;
/// Rhythm confidence at or above which `BeatSync::Auto` follows the beat grid.
pub const RHYTHM_THRESHOLD: f32 = 0.5;
/// Period of the free-running `beat_phase` sent while beat sync is off, s.
pub const FREE_PHASE_PERIOD: f64 = 4.0;
/// Step of the feature envelopes, seconds.
const DYN_STEP: f64 = 0.01;
/// Envelopes are re-primed this many steps before `t` after a seek (3 s,
/// two time constants of the slow phrase envelope).
const DYN_WARMUP: i64 = 300;
/// Time constant of the slow phrase envelope, seconds.
const PHRASE_TAU: f32 = 1.5;

/// Envelope state of the four continuous features (bass, mid, treble, level).
/// The engine keeps one for [`ShowEngine::frame_at`]; previews that need a
/// second time line (see the module docs) keep their own.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct EngineCursor {
    /// Last grid step processed, if any.
    step: Option<i64>,
    /// Fast (attack/release) envelopes.
    fast: [f32; 4],
    /// Slow phrase envelopes.
    slow: [f32; 4],
    /// Previous `t` for event edge detection.
    last_t: Option<f64>,
}

impl EngineCursor {
    pub fn new() -> Self {
        Self::default()
    }

    /// Forget the time line (next frame starts fresh).
    pub fn reset(&mut self) {
        *self = Self::default();
    }

    /// The event window `(t0, t]` for a frame at `t`, updating the edge state.
    fn window(&mut self, t: f64) -> f64 {
        let t0 = match self.last_t {
            Some(p) if t >= p && t - p <= MAX_CONTINUOUS_STEP => p,
            _ => t - FRAME_PERIOD,
        };
        self.last_t = Some(t);
        t0
    }

    /// Advance the envelopes to the grid step at or before `t`.
    fn advance(&mut self, p: &dyn FeatureProvider, s: &ShowSettings, t: f64) {
        let target = (t / DYN_STEP + 1e-6).floor() as i64;
        let start = match self.step {
            Some(k) if k <= target && target - k <= DYN_WARMUP => k,
            _ => {
                let k0 = target - DYN_WARMUP;
                let x = bands(&p.features_at(k0 as f64 * DYN_STEP));
                self.fast = x;
                self.slow = x;
                k0
            }
        };
        for k in start + 1..=target {
            let tk = k as f64 * DYN_STEP;
            let x = bands(&p.features_at(tk));
            let c = p.rhythm_confidence_at(tk).unwrap_or(1.0);
            let (att, rel) = envelope_coeffs(s.smoothing, c);
            let slow_a = 1.0 - (-(DYN_STEP as f32) / PHRASE_TAU).exp();
            for ((&x, fast), slow) in x.iter().zip(&mut self.fast).zip(&mut self.slow) {
                let a = if x > *fast { att } else { rel };
                *fast += (x - *fast) * a;
                *slow += (x - *slow) * slow_a;
            }
        }
        self.step = Some(target);
    }
}

fn bands(f: &FeatureFrame) -> [f32; 4] {
    [f.bass, f.mid, f.treble, f.level]
}

/// Per-step attack and release coefficients for `smoothing` (0..=1) at
/// rhythm confidence `c`. smoothing 0 ≈ raw (2 ms / 20 ms); the default 0.5
/// gives 8 ms / 80 ms on rhythmic music (crisp, like the gifts' own band
/// smoothing) and 56 ms / 480 ms on legato music (flowing).
fn envelope_coeffs(smoothing: f32, c: f32) -> (f32, f32) {
    let s = smoothing.clamp(0.0, 1.0);
    let legato = 1.0 - c.clamp(0.0, 1.0);
    let tau_att = (0.002 + 0.012 * s) * (1.0 + 6.0 * legato);
    let tau_rel = (0.02 + 0.12 * s) * (1.0 + 5.0 * legato);
    let k = |tau: f32| 1.0 - (-(DYN_STEP as f32) / tau).exp();
    (k(tau_att), k(tau_rel))
}

/// Modulation depth of the fast envelope around the phrase envelope.
/// intensity 0.5 → 1.0 (unchanged) on rhythmic music, 0.5 on legato.
fn modulation_depth(intensity: f32, c: f32) -> f32 {
    2.0 * intensity.clamp(0.0, 1.0) * (0.5 + 0.5 * c.clamp(0.0, 1.0))
}

/// Combines a [`FeatureProvider`] with a [`Show`]. Deterministic: the same
/// sequence of `t` values yields the same frames.
pub struct ShowEngine {
    provider: Arc<dyn FeatureProvider>,
    show: Show,
    cursor: EngineCursor,
    lookahead: f64,
}

impl ShowEngine {
    pub fn new(provider: Arc<dyn FeatureProvider>, show: Show) -> Self {
        Self {
            provider,
            show,
            cursor: EngineCursor::default(),
            lookahead: 0.0,
        }
    }

    pub fn show(&self) -> &Show {
        &self.show
    }

    /// Mutable access for live editing (takes effect on the next frame).
    pub fn show_mut(&mut self) -> &mut Show {
        &mut self.show
    }

    pub fn set_show(&mut self, show: Show) {
        self.show = show;
    }

    pub fn provider(&self) -> &Arc<dyn FeatureProvider> {
        &self.provider
    }

    pub fn set_provider(&mut self, provider: Arc<dyn FeatureProvider>) {
        self.provider = provider;
        self.reset();
    }

    /// Seconds added by [`ShowEngine::frame_for_playback`] (radio + render latency).
    pub fn set_lookahead(&mut self, secs: f64) {
        self.lookahead = secs;
    }

    pub fn lookahead(&self) -> f64 {
        self.lookahead
    }

    /// Forget the previous time (next frame starts fresh edge detection).
    pub fn reset(&mut self) {
        self.cursor.reset();
    }

    /// Rhythm confidence at `t` (1.0 if the provider cannot tell).
    pub fn rhythm_at(&self, t: f64) -> f32 {
        self.provider.rhythm_confidence_at(t).unwrap_or(1.0)
    }

    /// True if beat-driven behaviour follows the beat grid at `t` under the
    /// show's [`BeatSync`] setting.
    pub fn beat_sync_active(&self, t: f64) -> bool {
        match self.show.settings.beat_sync {
            BeatSync::On => true,
            BeatSync::Off => false,
            BeatSync::Auto => self.rhythm_at(t) >= RHYTHM_THRESHOLD,
        }
    }

    /// `frame_at(heard_position + lookahead)`: pass [`crate::Player::position`].
    pub fn frame_for_playback(&mut self, heard_position: f64, slot_count: u8) -> MusicFrame {
        self.frame_at(heard_position + self.lookahead, slot_count)
    }

    /// The frame for time `t`. Event flags (BEAT, DOWNBEAT, ONSET, DROP, strobe
    /// ticks) are set when such an event lies in `(t_prev, t]`. Jumping
    /// backwards or more than [`MAX_CONTINUOUS_STEP`] forward is a seek.
    pub fn frame_at(&mut self, t: f64, slot_count: u8) -> MusicFrame {
        let mut cursor = self.cursor;
        let f = self.peek_frame_at(&mut cursor, t, slot_count);
        self.cursor = cursor;
        f
    }

    /// [`ShowEngine::frame_at`] on a caller-owned time line: use it for a
    /// second destination with a different latency (e.g. the local preview
    /// next to the radio stream). Only safe with providers whose
    /// `events_in` is stateless (files); live providers report each event
    /// once, to whoever asks first.
    pub fn peek_frame_at(&self, cursor: &mut EngineCursor, t: f64, slot_count: u8) -> MusicFrame {
        let t0 = cursor.window(t);
        cursor.advance(&*self.provider, &self.show.settings, t);
        self.compute(t0, t, slot_count, cursor)
    }

    /// Stateless variant for previews / scrubbing: events in `(t - 10 ms, t]`.
    pub fn peek_frame(&self, t: f64, slot_count: u8) -> MusicFrame {
        let mut cursor = EngineCursor::default();
        cursor.advance(&*self.provider, &self.show.settings, t);
        self.compute(t - FRAME_PERIOD, t, slot_count, &cursor)
    }

    /// `frame` with the per-slot overrides of the cue active at `t` applied
    /// for `slot` (identity if there are none).
    pub fn slot_frame(&self, frame: &MusicFrame, t: f64, slot: u8) -> MusicFrame {
        let mut f = *frame;
        if let Some(o) = self
            .show
            .cue_at(t)
            .and_then(|c| c.slot_overrides.iter().find(|o| o.slot == slot))
        {
            if let Some(e) = o.effect {
                f.effect = e;
            }
            if let Some(p) = o.palette {
                f.palette = p;
            }
            if let Some(h) = o.hue {
                f.hue = h;
            }
            if let Some(b) = o.brightness {
                f.brightness = scale(b, self.show.settings.master_brightness);
            }
        }
        f
    }

    /// True if the cue active at `t` has any per-slot overrides.
    pub fn has_slot_overrides(&self, t: f64) -> bool {
        self.show
            .cue_at(t)
            .is_some_and(|c| !c.slot_overrides.is_empty())
    }

    fn compute(&self, t0: f64, t: f64, slot_count: u8, dynamics: &EngineCursor) -> MusicFrame {
        let p = &*self.provider;
        let s = &self.show.settings;
        let feat = p.features_at(t);
        let rhythm = p.rhythm_confidence_at(t).unwrap_or(1.0);
        let u = |x: f32| (x.clamp(0.0, 1.0) * 255.0).round() as u8;

        // Continuous features: smoothed, modulation depth around the phrase.
        let depth = modulation_depth(s.intensity, rhythm);
        let shaped: [f32; 4] = std::array::from_fn(|i| {
            let (fast, slow) = (dynamics.fast[i], dynamics.slow[i]);
            (slow + depth * (fast - slow)).clamp(0.0, 1.0)
        });
        let mut f = MusicFrame {
            bass: u(shaped[0]),
            mid: u(shaped[1]),
            treble: u(shaped[2]),
            level: u(shaped[3]),
            slot_count,
            ..Default::default()
        };

        // Beat grid, where the music has a pulse.
        let beat_sync = match s.beat_sync {
            BeatSync::On => true,
            BeatSync::Off => false,
            BeatSync::Auto => rhythm >= RHYTHM_THRESHOLD,
        };
        if beat_sync {
            if let Some(b) = p.beat_at(t) {
                f.beat_phase = (b.phase.clamp(0.0, 0.999) * 256.0) as u8;
                f.beat_in_bar = ((b.bar.rem_euclid(64) as u8) << 2) | (b.beat_in_bar & 3);
                f.bpm = b.bpm.round().clamp(0.0, 255.0) as u8;
            }
        } else {
            // bpm stays 0: no phase-wrap beats on the gifts.
            f.beat_phase = ((t / FREE_PHASE_PERIOD).rem_euclid(1.0) * 256.0).min(255.0) as u8;
        }

        // Events.
        let (ev, onset_strength) = p.events_with_strength(t0, t);
        if s.beat_flags && beat_sync {
            f.flags |= ev & (music_flags::BEAT | music_flags::DOWNBEAT);
        }
        if s.onset_flags
            && ev & music_flags::ONSET != 0
            && onset_strength >= 1.0 - s.sensitivity.clamp(0.0, 1.0) - 1e-6
        {
            f.flags |= music_flags::ONSET;
        }
        if s.drop_flags {
            f.flags |= ev & music_flags::DROP;
        }
        let outside = p.duration().is_some_and(|d| t < 0.0 || t > d);
        if outside || feat.is_silent() || feat.level < SILENT_LEVEL {
            f.flags |= music_flags::SILENT;
        }

        // Cue state.
        let (cue, prev) = match self.show.cue_index_at(t) {
            Some(i) => (
                Some(&self.show.cues[i]),
                i.checked_sub(1).map(|j| &self.show.cues[j]),
            ),
            None => (None, None),
        };
        let default = Cue::default();
        let cue = cue.unwrap_or(&default);
        let mut hue = cue_hue(cue, t);
        let mut bright = cue.brightness as f64;
        if let Some(prev) = prev {
            let since = t - cue.time;
            if cue.fade > 0.0 && since >= 0.0 && since < cue.fade as f64 {
                let k = since / cue.fade as f64;
                let ph = cue_hue(prev, t);
                let d = (hue - ph + 384.0).rem_euclid(256.0) - 128.0;
                hue = ph + d * k;
                bright = prev.brightness as f64 + (bright - prev.brightness as f64) * k;
            }
        }
        f.effect = cue.effect;
        f.palette = cue.palette;
        f.hue = hue.rem_euclid(256.0) as u8;
        f.param = cue.params;
        // Effects that ignore loudness follow the phrase dynamics through
        // their brightness instead (quiet phrases dimmer, never dark).
        if matches!(
            cue.effect,
            effect::SOLID | effect::BREATHE | effect::SPARKLE
        ) {
            let amount = 0.8 * s.intensity.clamp(0.0, 1.0);
            let phrase = dynamics.slow[3].clamp(0.0, 1.0).powf(1.0 / 1.5);
            bright *= (1.0 - amount + amount * phrase) as f64;
        }
        let mut brightness = bright.round().clamp(0.0, 255.0) as u8;

        // One-shots (later events win).
        for e in self.show.events_at(t) {
            let progress = ((t - e.time) / e.duration.max(1e-3) as f64).clamp(0.0, 1.0);
            match e.kind {
                OneShotKind::Blackout => brightness = 0,
                OneShotKind::Flash { hue, palette } => {
                    f.effect = effect::SOLID;
                    f.hue = hue;
                    if let Some(pal) = palette {
                        f.palette = pal;
                    }
                    let k = (1.0 - progress).powi(2);
                    brightness = (255.0 * k + brightness as f64 * (1.0 - k)).round() as u8;
                }
                OneShotKind::Strobe { rate_hz } => {
                    f.effect = effect::STROBE;
                    brightness = 255;
                    let period = 1.0 / (rate_hz.max(0.1) as f64);
                    let tick = |x: f64| ((x - e.time) / period).floor();
                    if t0 < e.time || tick(t) > tick(t0) {
                        f.flags |= music_flags::BEAT | music_flags::ONSET;
                    }
                }
            }
        }
        f.brightness = scale(brightness, s.master_brightness);
        f
    }
}

fn cue_hue(c: &Cue, t: f64) -> f64 {
    c.hue as f64 + c.hue_speed as f64 * (t - c.time).max(0.0)
}

fn scale(v: u8, master: u8) -> u8 {
    ((v as u16 * master as u16 + 127) / 255) as u8
}
