//! The editable light show: cues over time plus one-shot events, stored as
//! JSON (`*.lgshow`).

use std::path::Path;

use lumigift_fx::effect;
use serde::{Deserialize, Serialize};

use crate::analysis::{Analysis, Section, SectionKind};
use crate::{Error, Result};

/// Current `.lgshow` format version.
pub const SHOW_VERSION: u32 = 1;
/// File extension of saved shows.
pub const SHOW_EXTENSION: &str = "lgshow";

/// Palette ids (indices into [`lumigift_fx::PALETTES`]).
pub mod palette {
    pub const CHRISTMAS: u8 = 0;
    pub const FIRE: u8 = 1;
    pub const OCEAN: u8 = 2;
    pub const NEON: u8 = 3;
    pub const AURORA: u8 = 4;
    pub const SUNSET: u8 = 5;
    pub const CANDY: u8 = 6;
    pub const ICE: u8 = 7;
    pub const COUNT: u8 = 8;
}

/// The track a show belongs to.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct TrackRef {
    pub path: String,
    /// See [`crate::content_hash`]; lets the GUI re-find a moved file.
    pub content_hash: String,
    pub title: String,
}

/// Override of some cue parameters for one slot (gift position).
///
/// Over the radio a single [`lumigift_proto::MusicFrame`] is broadcast to every
/// gift, so overrides cannot be expressed in it. The engine applies them via
/// [`crate::ShowEngine::slot_frame`]; the core must render those slots
/// host-side (with `lumigift_fx::Renderer`) and send them as `Leds` packets, or
/// a future protocol revision can carry them.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct SlotOverride {
    pub slot: u8,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effect: Option<u8>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub palette: Option<u8>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub brightness: Option<u8>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hue: Option<u8>,
}

/// The look from `time` until the next cue.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Cue {
    /// Start time, seconds.
    pub time: f64,
    /// Effect id, see [`lumigift_fx::effect`].
    pub effect: u8,
    /// Palette id, see [`palette`].
    pub palette: u8,
    /// 0..=255, scaled by [`ShowSettings::master_brightness`].
    pub brightness: u8,
    /// Base hue 0..=255 at `time`.
    pub hue: u8,
    /// Hue drift in hue steps per second (may be negative); 256 = one full turn.
    #[serde(default)]
    pub hue_speed: f32,
    /// Effect-specific parameters (passed through to the frame).
    #[serde(default)]
    pub params: [u8; 2],
    /// Seconds over which brightness and hue cross-fade from the previous cue.
    #[serde(default)]
    pub fade: f32,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub slot_overrides: Vec<SlotOverride>,
    /// Free text for the editor.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub label: String,
}

impl Default for Cue {
    fn default() -> Self {
        Self {
            time: 0.0,
            effect: effect::PULSE,
            palette: palette::CHRISTMAS,
            brightness: 200,
            hue: 0,
            hue_speed: 0.0,
            params: [0; 2],
            fade: 0.0,
            slot_overrides: Vec::new(),
            label: String::new(),
        }
    }
}

/// Short events layered on top of the cues.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum OneShotKind {
    /// White strobe flashes at `rate_hz` (≤ 8 Hz is what the gifts can show).
    Strobe { rate_hz: f32 },
    /// Everything dark.
    Blackout,
    /// Solid colour that decays back into the running cue.
    Flash {
        hue: u8,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        palette: Option<u8>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct OneShot {
    pub time: f64,
    /// Seconds.
    pub duration: f32,
    #[serde(flatten)]
    pub kind: OneShotKind,
}

/// Whether beat-driven behaviour (BEAT / DOWNBEAT flags, beat phase, tempo)
/// follows the analysed beat grid.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BeatSync {
    /// Follow the beat grid only where the music has a clear pulse
    /// (rhythm confidence ≥ [`crate::engine::RHYTHM_THRESHOLD`]).
    #[default]
    Auto,
    /// Always follow the beat grid.
    On,
    /// Never: no beat flashes, tempo 0, free-running slow phase.
    Off,
}

impl BeatSync {
    pub const ALL: [BeatSync; 3] = [BeatSync::Auto, BeatSync::On, BeatSync::Off];

    pub fn label(self) -> &'static str {
        match self {
            BeatSync::Auto => "Auto",
            BeatSync::On => "On",
            BeatSync::Off => "Off",
        }
    }
}

/// Global show settings. Every field has a default, so older `.lgshow`
/// files load unchanged.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ShowSettings {
    /// Scales every cue's brightness.
    pub master_brightness: u8,
    /// Send BEAT / DOWNBEAT flags from the beat grid.
    pub beat_flags: bool,
    /// Send ONSET flags.
    pub onset_flags: bool,
    /// Send DROP flags.
    pub drop_flags: bool,
    /// Reactivity: onset sensitivity 0..=1. An onset is flagged when its
    /// strength is at least `1 - sensitivity`; 0.5 shows the musically
    /// significant attacks, 1.0 every detected one, 0.0 only the strongest.
    pub sensitivity: f32,
    /// Reactivity: feature smoothing 0..=1 (attack/release time of bass,
    /// mid, treble and level). 0 = raw, 1 = very smooth. Legato music gets
    /// a slower release automatically.
    pub smoothing: f32,
    /// Reactivity: how strongly audio modulates the lights 0..=1. 0 = the
    /// lights only follow the phrase-level dynamics, 0.5 = natural, 1.0 =
    /// exaggerated.
    pub intensity: f32,
    /// Reactivity: beat sync mode.
    pub beat_sync: BeatSync,
}

impl ShowSettings {
    pub const DEFAULT_SENSITIVITY: f32 = 0.5;
    pub const DEFAULT_SMOOTHING: f32 = 0.5;
    pub const DEFAULT_INTENSITY: f32 = 0.5;

    /// Default `[sensitivity, smoothing, intensity]`.
    pub const fn default_reactivity() -> [f32; 3] {
        [
            Self::DEFAULT_SENSITIVITY,
            Self::DEFAULT_SMOOTHING,
            Self::DEFAULT_INTENSITY,
        ]
    }

    /// True if the reactivity settings are all at their defaults.
    pub fn reactivity_is_default(&self) -> bool {
        let d = ShowSettings::default();
        self.sensitivity == d.sensitivity
            && self.smoothing == d.smoothing
            && self.intensity == d.intensity
            && self.beat_sync == d.beat_sync
    }

    /// Reset sensitivity, smoothing, intensity and beat sync.
    pub fn reset_reactivity(&mut self) {
        let d = ShowSettings::default();
        self.sensitivity = d.sensitivity;
        self.smoothing = d.smoothing;
        self.intensity = d.intensity;
        self.beat_sync = d.beat_sync;
    }
}

impl Default for ShowSettings {
    fn default() -> Self {
        Self {
            master_brightness: 255,
            beat_flags: true,
            onset_flags: true,
            drop_flags: true,
            sensitivity: Self::DEFAULT_SENSITIVITY,
            smoothing: Self::DEFAULT_SMOOTHING,
            intensity: Self::DEFAULT_INTENSITY,
            beat_sync: BeatSync::Auto,
        }
    }
}

/// A light show for one track.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Show {
    pub version: u32,
    pub track: TrackRef,
    /// Sorted by time (the editing helpers keep it that way).
    pub cues: Vec<Cue>,
    /// Sorted by time.
    #[serde(default)]
    pub events: Vec<OneShot>,
    #[serde(default)]
    pub settings: ShowSettings,
}

impl Default for Show {
    fn default() -> Self {
        Self {
            version: SHOW_VERSION,
            track: TrackRef::default(),
            cues: vec![Cue::default()],
            events: Vec::new(),
            settings: ShowSettings::default(),
        }
    }
}

impl Show {
    pub fn new(track: TrackRef) -> Self {
        Self {
            track,
            ..Default::default()
        }
    }

    /// Load a `.lgshow` file.
    pub fn load(path: impl AsRef<Path>) -> Result<Self> {
        let s = std::fs::read_to_string(path)?;
        Self::from_json(&s)
    }

    pub fn from_json(s: &str) -> Result<Self> {
        let mut show: Show = serde_json::from_str(s).map_err(|e| Error::Serde(e.to_string()))?;
        if show.version > SHOW_VERSION {
            return Err(Error::Serde(format!(
                "show version {} is newer than supported {SHOW_VERSION}",
                show.version
            )));
        }
        show.version = SHOW_VERSION;
        show.sort();
        Ok(show)
    }

    pub fn to_json(&self) -> String {
        serde_json::to_string_pretty(self).expect("show serialisation cannot fail")
    }

    /// Save as pretty JSON (atomic write).
    pub fn save(&self, path: impl AsRef<Path>) -> Result<()> {
        let path = path.as_ref();
        let tmp = path.with_extension("lgshow.tmp");
        std::fs::write(&tmp, self.to_json())?;
        std::fs::rename(&tmp, path)?;
        Ok(())
    }

    fn sort(&mut self) {
        self.cues.sort_by(|a, b| a.time.total_cmp(&b.time));
        self.events.sort_by(|a, b| a.time.total_cmp(&b.time));
    }

    // -- cue editing ------------------------------------------------------

    /// Index of the cue active at `t` (the last cue with `time <= t`; the
    /// first cue before that).
    pub fn cue_index_at(&self, t: f64) -> Option<usize> {
        if self.cues.is_empty() {
            return None;
        }
        Some(self.cues.partition_point(|c| c.time <= t).saturating_sub(1))
    }

    pub fn cue_at(&self, t: f64) -> Option<&Cue> {
        self.cue_index_at(t).map(|i| &self.cues[i])
    }

    /// Insert keeping the order; returns the new index. A cue at the same time
    /// (within 1 ms) as an existing one replaces it.
    pub fn insert_cue(&mut self, cue: Cue) -> usize {
        if let Some(i) = self
            .cues
            .iter()
            .position(|c| (c.time - cue.time).abs() < 1e-3)
        {
            self.cues[i] = cue;
            return i;
        }
        let i = self.cues.partition_point(|c| c.time <= cue.time);
        self.cues.insert(i, cue);
        i
    }

    /// Move cue `index` to `time` (clamped at 0); returns its new index.
    pub fn move_cue(&mut self, index: usize, time: f64) -> Option<usize> {
        if index >= self.cues.len() {
            return None;
        }
        let mut c = self.cues.remove(index);
        c.time = time.max(0.0);
        Some(self.insert_cue(c))
    }

    pub fn remove_cue(&mut self, index: usize) -> Option<Cue> {
        (index < self.cues.len()).then(|| self.cues.remove(index))
    }

    /// Insert a copy of the state active at `t` as a new cue at `t`.
    pub fn split_at(&mut self, t: f64) -> usize {
        let mut c = Cue::default();
        if let Some(prev) = self.cue_at(t) {
            let hue = prev.hue as f64 + prev.hue_speed as f64 * (t - prev.time);
            c = Cue {
                hue: hue.rem_euclid(256.0) as u8,
                ..prev.clone()
            };
        }
        c.time = t.max(0.0);
        c.fade = 0.0;
        c.label.clear();
        self.insert_cue(c)
    }

    // -- one-shot editing -------------------------------------------------

    pub fn insert_event(&mut self, e: OneShot) -> usize {
        let i = self.events.partition_point(|x| x.time <= e.time);
        self.events.insert(i, e);
        i
    }

    pub fn move_event(&mut self, index: usize, time: f64) -> Option<usize> {
        if index >= self.events.len() {
            return None;
        }
        let mut e = self.events.remove(index);
        e.time = time.max(0.0);
        Some(self.insert_event(e))
    }

    pub fn remove_event(&mut self, index: usize) -> Option<OneShot> {
        (index < self.events.len()).then(|| self.events.remove(index))
    }

    /// Events active at `t` (`time <= t < time + duration`).
    pub fn events_at(&self, t: f64) -> impl Iterator<Item = &OneShot> {
        let end = self.events.partition_point(|e| e.time <= t);
        self.events[..end]
            .iter()
            .filter(move |e| t < e.time + e.duration as f64)
    }

    // -- auto generation --------------------------------------------------

    /// A tasteful default show from the analysis.
    ///
    /// * one cue per section (plus variations every 16 bars in long sections),
    ///   snapped to downbeats;
    /// * calm sections: Breathe / Solid / Sparkle, dimmer, cool palettes, slow
    ///   hue drift; medium: Pulse / Spectrum / Sparkle / Chase; intense: Chase /
    ///   Rainbow / Spectrum / Pulse at full brightness with faster drift;
    /// * drops: a one-beat blackout before strong drops, then a colour flash, a one-bar
    ///   Strobe cue (plus an 8 Hz strobe burst for strong drops), then the
    ///   section's intense look;
    /// * palettes change at every section boundary;
    /// * sections without a clear pulse (rhythm confidence below
    ///   [`crate::engine::RHYTHM_THRESHOLD`]: rubato, solo, ambient) get calm,
    ///   flowing looks instead: Breathe / Solid with slow hue drift when
    ///   calm, plus Spectrum / Sparkle when medium and Rainbow when intense,
    ///   long cross-fades, cues every ~24 s, and no Strobe, Pulse, Chase,
    ///   blackouts or flashes (their "drops" become a slow fade).
    pub fn auto_generate(analysis: &Analysis) -> Show {
        let mut show = Show {
            cues: Vec::new(),
            ..Default::default()
        };
        show.track.content_hash = analysis.content_hash.clone();
        // Deterministic variety per track.
        let seed = analysis
            .content_hash
            .bytes()
            .fold(analysis.beats.len() as u32, |h, b| {
                h.wrapping_mul(31).wrapping_add(b as u32)
            });
        let mut pick = Picker { state: seed | 1 };
        let beat = if analysis.bpm > 0.0 {
            60.0 / analysis.bpm as f64
        } else {
            0.5
        };
        let bar = 4.0 * beat;

        const CALM_FX: [u8; 3] = [effect::BREATHE, effect::SOLID, effect::SPARKLE];
        const MED_FX: [u8; 4] = [
            effect::PULSE,
            effect::SPECTRUM,
            effect::SPARKLE,
            effect::CHASE,
        ];
        const HIGH_FX: [u8; 4] = [
            effect::CHASE,
            effect::RAINBOW,
            effect::SPECTRUM,
            effect::PULSE,
        ];
        const CALM_PAL: [u8; 4] = [
            palette::OCEAN,
            palette::AURORA,
            palette::ICE,
            palette::SUNSET,
        ];
        const WARM_PAL: [u8; 8] = [
            palette::CHRISTMAS,
            palette::FIRE,
            palette::NEON,
            palette::SUNSET,
            palette::CANDY,
            palette::AURORA,
            palette::OCEAN,
            palette::ICE,
        ];

        // Sections without a pulse: no beat-driven or flashing effects.
        const LEGATO_CALM_FX: [u8; 2] = [effect::BREATHE, effect::SOLID];
        const LEGATO_MED_FX: [u8; 4] = [
            effect::BREATHE,
            effect::SPECTRUM,
            effect::SOLID,
            effect::SPARKLE,
        ];
        const LEGATO_HIGH_FX: [u8; 3] = [effect::SPECTRUM, effect::RAINBOW, effect::BREATHE];
        /// Cue length within a long section without a pulse, seconds.
        const LEGATO_VARIATION_S: f64 = 24.0;

        let mut last_fx = u8::MAX;
        let mut last_pal = u8::MAX;
        let mut counters = [0usize; 3];
        let mut legato_counters = [0usize; 3];
        let sections: Vec<_> = if analysis.sections.is_empty() {
            vec![Section {
                start: 0.0,
                end: analysis.duration,
                energy: 0.5,
                kind: SectionKind::Medium,
                starts_with_drop: false,
                rhythm: analysis.rhythm_confidence,
            }]
        } else {
            analysis.sections.clone()
        };

        for sec in &sections {
            let class = match sec.kind {
                SectionKind::Silence | SectionKind::Calm => 0,
                SectionKind::Medium => 1,
                SectionKind::Intense => 2,
            };
            let rhythmic = analysis.bpm > 0.0 && sec.rhythm >= crate::engine::RHYTHM_THRESHOLD;
            let fx_list: &[u8] = if rhythmic {
                [&CALM_FX[..], &MED_FX[..], &HIGH_FX[..]][class]
            } else {
                [&LEGATO_CALM_FX[..], &LEGATO_MED_FX[..], &LEGATO_HIGH_FX[..]][class]
            };
            let pal_list: &[u8] = if class == 0 { &CALM_PAL } else { &WARM_PAL };
            let next_fx = |counters: &mut [usize; 3], last: u8| {
                let mut fx = fx_list[counters[class] % fx_list.len()];
                counters[class] += 1;
                if fx == last {
                    fx = fx_list[counters[class] % fx_list.len()];
                    counters[class] += 1;
                }
                fx
            };
            let mut pal = pal_list[pick.next() as usize % pal_list.len()];
            if pal == last_pal {
                pal = pal_list
                    [(pal_list.iter().position(|&p| p == pal).unwrap_or(0) + 1) % pal_list.len()];
            }
            last_pal = pal;
            let brightness = match sec.kind {
                SectionKind::Silence => 40,
                SectionKind::Calm => 110 + (sec.energy * 80.0) as u8,
                SectionKind::Medium => 170 + (sec.energy * 50.0) as u8,
                SectionKind::Intense => 255,
            };
            let hue_speed = [3.0, 6.0, 12.0][class];
            let mut t = sec.start;

            if !rhythmic {
                // Calm and flowing: follow phrasing and dynamics, not a grid.
                let brightness = match sec.kind {
                    SectionKind::Silence => 40,
                    SectionKind::Calm => 100 + (sec.energy * 60.0) as u8,
                    SectionKind::Medium => 140 + (sec.energy * 60.0) as u8,
                    SectionKind::Intense => 200 + (sec.energy * 40.0) as u8,
                };
                let hue_speed = [2.0, 3.0, 5.0][class];
                let mut first = true;
                while t < sec.end - 8.0 || first {
                    let fx = next_fx(&mut legato_counters, last_fx);
                    last_fx = fx;
                    show.cues.push(Cue {
                        time: t,
                        effect: fx,
                        palette: pal,
                        brightness,
                        hue: pick.next() as u8,
                        hue_speed: if pick.next().is_multiple_of(2) {
                            hue_speed
                        } else {
                            -hue_speed
                        },
                        fade: 3.0,
                        label: if first {
                            format!("{:?}", sec.kind).to_lowercase()
                        } else {
                            String::new()
                        },
                        ..Default::default()
                    });
                    first = false;
                    t += LEGATO_VARIATION_S;
                }
                continue;
            }

            if sec.starts_with_drop {
                let strength = analysis
                    .drops
                    .iter()
                    .find(|d| (d.time - sec.start).abs() < 1.0)
                    .map(|d| d.strength)
                    .unwrap_or(0.5);
                if strength > 0.3 && sec.start > beat {
                    show.events.push(OneShot {
                        time: sec.start - beat,
                        duration: (beat * 0.9) as f32,
                        kind: OneShotKind::Blackout,
                    });
                }
                show.events.push(OneShot {
                    time: sec.start,
                    duration: (beat * 0.75) as f32,
                    kind: OneShotKind::Flash {
                        hue: pick.next() as u8,
                        palette: Some(pal),
                    },
                });
                if strength >= 0.3 {
                    show.events.push(OneShot {
                        time: sec.start + beat,
                        duration: (bar - beat) as f32,
                        kind: OneShotKind::Strobe { rate_hz: 8.0 },
                    });
                }
                show.cues.push(Cue {
                    time: sec.start,
                    effect: effect::STROBE,
                    palette: pal,
                    brightness: 255,
                    hue: pick.next() as u8,
                    hue_speed: 0.0,
                    label: "drop".into(),
                    ..Default::default()
                });
                t = sec.start + bar;
                last_fx = effect::STROBE;
            }

            // The section's main look, with a variation every 16 bars.
            let variation = 16.0 * bar;
            let mut first = true;
            while t < sec.end - bar.max(2.0) || first {
                let fx = next_fx(&mut counters, last_fx);
                last_fx = fx;
                show.cues.push(Cue {
                    time: t,
                    effect: fx,
                    palette: pal,
                    brightness,
                    hue: pick.next() as u8,
                    hue_speed: if pick.next().is_multiple_of(2) {
                        hue_speed
                    } else {
                        -hue_speed
                    },
                    fade: if class == 0 && !sec.starts_with_drop {
                        (bar / 2.0) as f32
                    } else {
                        0.0
                    },
                    label: if first {
                        format!("{:?}", sec.kind).to_lowercase()
                    } else {
                        String::new()
                    },
                    ..Default::default()
                });
                first = false;
                let next = t + variation;
                t = analysis
                    .nearest_downbeat(next)
                    .filter(|&d| d > t + bar)
                    .unwrap_or(next);
            }
        }
        // Gentle fade-out over the last section's tail.
        if analysis.duration > 4.0
            && let Some(last) = show.cues.last().cloned()
        {
            let t = (analysis.duration - 2.0).max(last.time + 0.5);
            show.cues.push(Cue {
                time: t,
                effect: effect::BREATHE,
                brightness: 60,
                fade: 1.5,
                hue_speed: 2.0,
                label: "outro".into(),
                ..last
            });
        }
        show.sort();
        if show.cues.is_empty() {
            show.cues.push(Cue::default());
        }
        show
    }
}

/// Tiny deterministic PRNG for auto generation.
struct Picker {
    state: u32,
}

impl Picker {
    fn next(&mut self) -> u32 {
        let mut x = self.state;
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        self.state = x;
        x >> 8
    }
}
