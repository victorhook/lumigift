//! Offline analysis of a whole track.
//!
//! Pipeline (all on the mono mixdown, ~100 frames/s):
//!
//! 1. STFT (Hann, ~46 ms window, hop = sample_rate / 100) → bass / mid /
//!    treble / RMS levels in dB, 32 log bands, spectral flux.
//! 2. Track-adaptive normalisation: each level is mapped from
//!    `[max(p5, p97 - 30 dB), p97]` onto 0..=1 (then `^1.5`), so quiet and loud
//!    masters both use the full LED range. Flux is divided by its 99th
//!    percentile.
//! 3. Onsets: peak picking on the flux with a moving-average threshold.
//! 4. Tempo: autocorrelation tempogram (8 s windows, 1 s hop, 60–200 BPM) with
//!    a log-normal prior around 120 BPM against octave errors, decoded with
//!    Viterbi so tempo changes are followed but jitter is not.
//! 5. Beats: Ellis-style dynamic programming with the time-varying period,
//!    separately for every non-silent region, refined to sub-frame precision.
//! 6. Downbeats (4/4): per region, the bar phase whose beats carry the most
//!    kick/bass energy and harmonic change.
//! 7. Drops: a sharp energy rise after a quieter build-up. Sections: Foote
//!    novelty on 0.5 s block features, plus drops and silence edges, snapped
//!    to downbeats, with a per-section energy level.
//! 8. Waveform and spectrogram overviews for drawing.

mod beat;
mod cache;
pub(crate) mod dsp;
mod overview;
mod pipeline;
mod structure;

use lumigift_proto::music_flags;
use serde::{Deserialize, Serialize};

pub use cache::AnalysisCache;
pub use overview::{Spectrogram, WaveBucket, Waveform};
pub(crate) use pipeline::analyze_with;

use crate::features::{BeatPosition, FeatureFrame, FeatureProvider};
use crate::job::JobContext;

/// Bumped whenever the analysis output changes; part of the cache key.
pub const ANALYSIS_VERSION: u32 = 1;

/// Systematic delay of the flux peak relative to the true attack, removed
/// from onset and beat times (measured on synthetic drums).
pub(crate) const ONSET_TIME_CORRECTION: f64 = 0.0095;

/// A detected onset (note/drum attack).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Onset {
    pub time: f64,
    /// Normalised flux at the peak, 0..=1.
    pub strength: f32,
}

/// A beat on the beat grid.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Beat {
    pub time: f64,
    /// 0..=3, 0 = downbeat (4/4 assumed).
    pub beat_in_bar: u8,
    /// Bar counter from the start of the track.
    pub bar: u32,
    /// Local tempo around this beat.
    pub bpm: f32,
}

impl Beat {
    pub fn is_downbeat(&self) -> bool {
        self.beat_in_bar == 0
    }
}

/// A sudden energy rise after a quieter build-up.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct DropEvent {
    pub time: f64,
    /// Energy contrast (after minus before), 0..=1.
    pub strength: f32,
}

/// Coarse energy class of a section.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum SectionKind {
    Silence,
    Calm,
    Medium,
    Intense,
}

/// A structural section (verse, chorus, breakdown, ...; unlabeled).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Section {
    pub start: f64,
    pub end: f64,
    /// Relative energy within the track, 0..=1.
    pub energy: f32,
    pub kind: SectionKind,
    /// True if a [`DropEvent`] marks the start of this section.
    pub starts_with_drop: bool,
}

impl Section {
    pub fn duration(&self) -> f64 {
        self.end - self.start
    }
}

/// Snap targets for [`Analysis::snap`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum Snap {
    #[default]
    Off,
    Beat,
    HalfBeat,
    Downbeat,
}

/// Everything the light show needs to know about a track. Serialisable, so it
/// can be cached on disk (see [`AnalysisCache`]).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Analysis {
    pub version: u32,
    /// Hash of the source file (see [`crate::content_hash`]); empty if unknown.
    pub content_hash: String,
    pub sample_rate: u32,
    /// Track length in seconds.
    pub duration: f64,
    /// Feature frames per second (`sample_rate / hop`, ~100).
    pub fps: f64,
    /// Feature frame `i` describes the audio around `i / fps` seconds.
    pub frames: Vec<FeatureFrame>,
    pub onsets: Vec<Onset>,
    /// Dominant tempo (0 if none was found).
    pub bpm: f32,
    pub beats: Vec<Beat>,
    pub sections: Vec<Section>,
    pub drops: Vec<DropEvent>,
    pub waveform: Waveform,
    pub spectrogram: Spectrogram,
    /// Wall-clock time the analysis took, seconds.
    pub analysis_seconds: f32,
}

impl Analysis {
    /// Analyse a mono signal on the calling thread (blocking). For files use
    /// [`crate::load_track`]; for decoded audio [`crate::analyze_async`].
    pub fn analyze(mono: &[f32], sample_rate: u32) -> Analysis {
        analyze_with(mono, sample_rate, &JobContext::detached())
            .expect("detached analysis is never cancelled")
    }

    /// Features at time `t`, linearly interpolated (zero outside the track).
    pub fn features_at(&self, t: f64) -> FeatureFrame {
        let n = self.frames.len();
        if n == 0 || !t.is_finite() || t < 0.0 {
            return FeatureFrame::SILENT;
        }
        let x = t * self.fps;
        let i = x.floor() as usize;
        if i + 1 < n {
            self.frames[i].lerp(&self.frames[i + 1], (x - i as f64) as f32)
        } else if i <= n {
            self.frames[n - 1]
        } else {
            FeatureFrame::SILENT
        }
    }

    /// Index of the last beat at or before `t`.
    pub fn beat_index_at(&self, t: f64) -> Option<usize> {
        let k = self.beats.partition_point(|b| b.time <= t);
        k.checked_sub(1)
    }

    /// Beats with `t0 <= time < t1`.
    pub fn beats_in(&self, t0: f64, t1: f64) -> &[Beat] {
        let a = self.beats.partition_point(|b| b.time < t0);
        let b = self.beats.partition_point(|b| b.time < t1);
        &self.beats[a..b.max(a)]
    }

    /// Downbeats (bar starts).
    pub fn downbeats(&self) -> impl Iterator<Item = &Beat> {
        self.beats.iter().filter(|b| b.is_downbeat())
    }

    /// Time of the beat closest to `t`.
    pub fn nearest_beat(&self, t: f64) -> Option<f64> {
        closest(self.beats.iter().map(|b| b.time), t)
    }

    /// Time of the downbeat closest to `t`.
    pub fn nearest_downbeat(&self, t: f64) -> Option<f64> {
        closest(self.downbeats().map(|b| b.time), t)
    }

    /// Snap a time to the beat grid (falls back to `t` when there are no beats).
    pub fn snap(&self, t: f64, snap: Snap) -> f64 {
        match snap {
            Snap::Off => t,
            Snap::Beat => self.nearest_beat(t).unwrap_or(t),
            Snap::Downbeat => self
                .nearest_downbeat(t)
                .or_else(|| self.nearest_beat(t))
                .unwrap_or(t),
            Snap::HalfBeat => {
                let Some(i) = self.beat_index_at(t) else {
                    return self.nearest_beat(t).unwrap_or(t);
                };
                let b0 = self.beats[i].time;
                let b1 = self
                    .beats
                    .get(i + 1)
                    .map(|b| b.time)
                    .unwrap_or(b0 + 60.0 / self.bpm.max(1.0) as f64);
                closest([b0, 0.5 * (b0 + b1), b1], t).unwrap_or(t)
            }
        }
    }

    /// The section containing `t` and its index.
    pub fn section_at(&self, t: f64) -> Option<(usize, &Section)> {
        let k = self.sections.partition_point(|s| s.start <= t);
        let i = k.checked_sub(1)?;
        let s = &self.sections[i];
        (t < s.end || i + 1 == self.sections.len()).then_some((i, s))
    }

    /// Local tempo at `t` (0 if unknown).
    pub fn tempo_at(&self, t: f64) -> f32 {
        match self.beat_index_at(t) {
            Some(i) => self.beats[i].bpm,
            None => self.beats.first().map(|b| b.bpm).unwrap_or(self.bpm),
        }
    }

    /// Beat grid position at `t` (extrapolated before the first and after the
    /// last beat, and across silent gaps).
    pub fn beat_position(&self, t: f64) -> Option<BeatPosition> {
        let n = self.beats.len();
        if n < 2 {
            return None;
        }
        let ibi_at = |i: usize| {
            let i = i.min(n - 2);
            self.beats[i + 1].time - self.beats[i].time
        };
        // Anchor beat and its interval.
        let (anchor, ibi) = match self.beat_index_at(t) {
            None => (0usize, ibi_at(0)),
            Some(i) if i + 1 < n => {
                let d = self.beats[i + 1].time - self.beats[i].time;
                let local = 60.0 / self.beats[i].bpm.max(1.0) as f64;
                if d > 1.6 * local { (i, local) } else { (i, d) }
            }
            Some(i) => (i, 60.0 / self.beats[i].bpm.max(1.0) as f64),
        };
        let b = &self.beats[anchor];
        let x = (t - b.time) / ibi.max(1e-3);
        let k = x.floor();
        let phase = (x - k) as f32;
        let k = k as i64;
        let pos = b.beat_in_bar as i64 + k;
        Some(BeatPosition {
            beat_index: anchor as i64 + k,
            phase,
            beat_in_bar: pos.rem_euclid(4) as u8,
            bar: b.bar as i64 + pos.div_euclid(4),
            bpm: b.bpm,
        })
    }

    /// Smoothed energy (0..=1) at `t`, as used for drops and sections.
    pub fn energy_at(&self, t: f64) -> f32 {
        self.features_at(t).energy()
    }
}

/// The time in `times` closest to `t` (the first one on ties).
fn closest(times: impl IntoIterator<Item = f64>, t: f64) -> Option<f64> {
    times
        .into_iter()
        .min_by(|a, b| (a - t).abs().total_cmp(&(b - t).abs()))
}

/// The time in the sorted `times` closest to `t`, if it is within `tol`.
fn nearest_within(times: &[f64], t: f64, tol: f64) -> Option<f64> {
    let k = times.partition_point(|&x| x < t);
    let neighbours = [k.checked_sub(1), Some(k)].into_iter().flatten();
    closest(neighbours.filter_map(|i| times.get(i).copied()), t).filter(|x| (x - t).abs() <= tol)
}

impl FeatureProvider for Analysis {
    fn features_at(&self, t: f64) -> FeatureFrame {
        Analysis::features_at(self, t)
    }

    fn beat_at(&self, t: f64) -> Option<BeatPosition> {
        if t < -1.0 || t > self.duration + 1.0 {
            return None;
        }
        self.beat_position(t)
    }

    fn events_in(&self, t0: f64, t1: f64) -> u8 {
        if t1 <= t0 {
            return 0;
        }
        let mut flags = 0;
        let a = self.beats.partition_point(|b| b.time <= t0);
        let b = self.beats.partition_point(|b| b.time <= t1);
        for beat in &self.beats[a..b.max(a)] {
            flags |= music_flags::BEAT;
            if beat.is_downbeat() {
                flags |= music_flags::DOWNBEAT;
            }
        }
        let a = self.onsets.partition_point(|o| o.time <= t0);
        if self.onsets.get(a).is_some_and(|o| o.time <= t1) {
            flags |= music_flags::ONSET;
        }
        let a = self.drops.partition_point(|d| d.time <= t0);
        if self.drops.get(a).is_some_and(|d| d.time <= t1) {
            flags |= music_flags::DROP;
        }
        flags
    }

    fn duration(&self) -> Option<f64> {
        Some(self.duration)
    }
}
