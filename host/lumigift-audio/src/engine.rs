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

use std::sync::Arc;

use lumigift_fx::effect;
use lumigift_proto::{MusicFrame, music_flags};

use crate::features::FeatureProvider;
use crate::show::{Cue, OneShotKind, Show};

/// Largest forward step treated as continuous playback. A bigger jump (or
/// any backwards step) is a seek: edge detection restarts without replaying
/// the skipped events.
pub const MAX_CONTINUOUS_STEP: f64 = 0.25;
/// Event window used for the first frame after a reset / seek, seconds.
pub const FRAME_PERIOD: f64 = 0.01;
/// Normalised level below which (or an absolute RMS below
/// [`crate::features::SILENCE_DB`]) the SILENT flag is set.
const SILENT_LEVEL: f32 = 0.01;

/// Combines a [`FeatureProvider`] with a [`Show`]. Deterministic: the same
/// sequence of `t` values yields the same frames.
pub struct ShowEngine {
    provider: Arc<dyn FeatureProvider>,
    show: Show,
    last_t: Option<f64>,
    lookahead: f64,
}

impl ShowEngine {
    pub fn new(provider: Arc<dyn FeatureProvider>, show: Show) -> Self {
        Self {
            provider,
            show,
            last_t: None,
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
        self.last_t = None;
    }

    /// `frame_at(heard_position + lookahead)`: pass [`crate::Player::position`].
    pub fn frame_for_playback(&mut self, heard_position: f64, slot_count: u8) -> MusicFrame {
        self.frame_at(heard_position + self.lookahead, slot_count)
    }

    /// The frame for time `t`. Event flags (BEAT, DOWNBEAT, ONSET, DROP, strobe
    /// ticks) are set when such an event lies in `(t_prev, t]`. Jumping
    /// backwards or more than [`MAX_CONTINUOUS_STEP`] forward is a seek.
    pub fn frame_at(&mut self, t: f64, slot_count: u8) -> MusicFrame {
        let t0 = match self.last_t {
            Some(p) if t >= p && t - p <= MAX_CONTINUOUS_STEP => p,
            _ => t - FRAME_PERIOD,
        };
        self.last_t = Some(t);
        self.compute(t0, t, slot_count)
    }

    /// Stateless variant for previews / scrubbing: events in `(t - 10 ms, t]`.
    pub fn peek_frame(&self, t: f64, slot_count: u8) -> MusicFrame {
        self.compute(t - FRAME_PERIOD, t, slot_count)
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

    fn compute(&self, t0: f64, t: f64, slot_count: u8) -> MusicFrame {
        let p = &*self.provider;
        let s = &self.show.settings;
        let feat = p.features_at(t);
        let u = |x: f32| (x.clamp(0.0, 1.0) * 255.0).round() as u8;

        let mut f = MusicFrame {
            bass: u(feat.bass),
            mid: u(feat.mid),
            treble: u(feat.treble),
            level: u(feat.level),
            slot_count,
            ..Default::default()
        };

        // Beat grid.
        if let Some(b) = p.beat_at(t) {
            f.beat_phase = (b.phase.clamp(0.0, 0.999) * 256.0) as u8;
            f.beat_in_bar = ((b.bar.rem_euclid(64) as u8) << 2) | (b.beat_in_bar & 3);
            f.bpm = b.bpm.round().clamp(0.0, 255.0) as u8;
        }

        // Events.
        let ev = p.events_in(t0, t);
        if s.beat_flags {
            f.flags |= ev & (music_flags::BEAT | music_flags::DOWNBEAT);
        }
        if s.onset_flags {
            f.flags |= ev & music_flags::ONSET;
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
