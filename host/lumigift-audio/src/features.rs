//! The common feature interface shared by offline analysis and live sources.
//!
//! [`crate::ShowEngine`] only talks to a [`FeatureProvider`]. The offline
//! [`crate::Analysis`] implements it (with perfect look-ahead), and so does
//! [`crate::live::LiveFeatures`] (causal, fed by a
//! [`crate::live::RealtimeAnalyzer`]). Any future source, such as PipeWire
//! capture or librespot, only has to produce [`FeatureFrame`]s plus events.

use serde::{Deserialize, Serialize};

/// Audio features for one analysis frame (~10 ms). All normalised values are
/// 0.0..=1.0 with track-adaptive scaling (quiet and loud tracks both use the
/// full range).
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
pub struct FeatureFrame {
    /// ~20–150 Hz energy.
    pub bass: f32,
    /// ~150–2000 Hz energy.
    pub mid: f32,
    /// ~2–12 kHz energy.
    pub treble: f32,
    /// Overall loudness (RMS).
    pub level: f32,
    /// Spectral-flux onset strength.
    pub onset: f32,
    /// Absolute RMS level in dBFS (not normalised; used for silence detection).
    pub rms_db: f32,
    /// Dominant pitch class as a point on the pitch circle:
    /// `clarity · (cos θ, sin θ)` with `θ = 2π · class / 12` (C = 0). The
    /// length is how clearly the frame is pitched (0 = noise, drums,
    /// silence). A vector so frames interpolate without jumping from B to C.
    /// See [`FeatureFrame::pitch_class`] and [`FeatureFrame::tone_clarity`].
    #[serde(default)]
    pub tone: [f32; 2],
}

/// Note names by pitch class (C = 0).
pub const NOTE_NAMES: [&str; 12] = [
    "C", "C♯", "D", "D♯", "E", "F", "F♯", "G", "G♯", "A", "A♯", "B",
];

impl FeatureFrame {
    /// The frame reported outside the signal: all zero, RMS at -120 dBFS.
    pub(crate) const SILENT: FeatureFrame = FeatureFrame {
        bass: 0.0,
        mid: 0.0,
        treble: 0.0,
        level: 0.0,
        onset: 0.0,
        rms_db: -120.0,
        tone: [0.0, 0.0],
    };

    /// Linear interpolation between two frames.
    pub fn lerp(&self, o: &FeatureFrame, t: f32) -> FeatureFrame {
        let l = |a: f32, b: f32| a + (b - a) * t;
        FeatureFrame {
            bass: l(self.bass, o.bass),
            mid: l(self.mid, o.mid),
            treble: l(self.treble, o.treble),
            level: l(self.level, o.level),
            onset: l(self.onset, o.onset),
            rms_db: l(self.rms_db, o.rms_db),
            tone: [l(self.tone[0], o.tone[0]), l(self.tone[1], o.tone[1])],
        }
    }

    /// The `tone` vector for a pitch class (0..12) and clarity (0..=1).
    pub fn tone_vector(pitch_class: f32, clarity: f32) -> [f32; 2] {
        let a = pitch_class / 12.0 * std::f32::consts::TAU;
        [clarity * a.cos(), clarity * a.sin()]
    }

    /// How clearly the frame is pitched, 0..=1.
    pub fn tone_clarity(&self) -> f32 {
        self.tone[0].hypot(self.tone[1]).min(1.0)
    }

    /// Dominant pitch class 0.0..12.0 (C = 0), or `None` if the frame is
    /// not pitched at all.
    pub fn pitch_class(&self) -> Option<f32> {
        (self.tone_clarity() > 1e-3)
            .then(|| (self.tone[1].atan2(self.tone[0]) / std::f32::consts::TAU * 12.0).rem_euclid(12.0))
    }

    /// True when the frame is effectively silent.
    pub fn is_silent(&self) -> bool {
        self.rms_db < SILENCE_DB
    }

    /// Bass-weighted loudness (0..=1) used for drops and section energy.
    pub(crate) fn energy(&self) -> f32 {
        0.45 * self.bass + 0.35 * self.level + 0.2 * self.mid
    }
}

/// RMS level (dBFS) below which audio counts as silence.
pub const SILENCE_DB: f32 = -55.0;

/// Where a moment lies on the beat grid.
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
pub struct BeatPosition {
    /// Index of the beat at or before `t` (may be negative before the first beat).
    pub beat_index: i64,
    /// Progress through the current beat, 0.0..1.0.
    pub phase: f32,
    /// Beat within the bar, 0..=3 (0 = downbeat; 4/4 assumed).
    pub beat_in_bar: u8,
    /// Bar counter (may be negative before the first downbeat).
    pub bar: i64,
    /// Local tempo.
    pub bpm: f32,
}

/// Source of features for the [`crate::ShowEngine`].
///
/// Times are media seconds on the provider's own time line: track position for
/// files, the capture clock ([`crate::live::LiveFeatures::now`]) for live input.
pub trait FeatureProvider: Send + Sync {
    /// Features at time `t` (interpolated where the provider can).
    fn features_at(&self, t: f64) -> FeatureFrame;
    /// Beat grid position at `t`, or `None` if no tempo is known.
    fn beat_at(&self, t: f64) -> Option<BeatPosition>;
    /// Union of [`lumigift_proto::music_flags`] `BEAT | DOWNBEAT | ONSET | DROP`
    /// for events falling in the half-open interval `(t0, t1]`.
    fn events_in(&self, t0: f64, t1: f64) -> u8;
    /// Total length for finite sources, `None` for live ones.
    fn duration(&self) -> Option<f64>;

    /// Rhythm confidence (0..=1) at `t`: how clearly the music has a steady
    /// pulse there. `None` if the provider cannot tell (the engine then
    /// trusts its beat grid, as live providers always did).
    fn rhythm_confidence_at(&self, _t: f64) -> Option<f32> {
        None
    }

    /// [`FeatureProvider::events_in`] plus the strength (0..=1) of the
    /// strongest onset in `(t0, t1]` (0 if there is none). The engine calls
    /// only this, once per frame. The default reports strength 1 for any
    /// ONSET, so providers without strengths behave as before.
    fn events_with_strength(&self, t0: f64, t1: f64) -> (u8, f32) {
        let flags = self.events_in(t0, t1);
        let strength = if flags & lumigift_proto::music_flags::ONSET != 0 {
            1.0
        } else {
            0.0
        };
        (flags, strength)
    }
}
