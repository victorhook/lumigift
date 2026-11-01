//! Causal (no look-ahead) feature extraction for live audio.

use std::collections::VecDeque;

use lumigift_proto::music_flags;

use crate::analysis::dsp::{self, Frontend};
use crate::features::{BeatPosition, FeatureFrame, SILENCE_DB};

/// One analysed live frame (~10 ms).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RealtimeFrame {
    /// Centre of the analysis window on the source's sample clock, seconds
    /// since the analyzer started.
    pub time: f64,
    pub features: FeatureFrame,
    /// `BEAT | DOWNBEAT | ONSET | DROP` detected in this frame.
    pub events: u8,
    /// Predicted beat grid position at `time`, once a tempo is locked.
    pub beat: Option<BeatPosition>,
}

/// Automatic gain control for one dB-valued feature: tracks a high
/// percentile with a slow release so quiet and loud material both span 0..=1.
#[derive(Debug, Clone, Copy, Default)]
struct Agc {
    hi: f32,
    init: bool,
}

impl Agc {
    fn map(&mut self, v: f32, silent: bool, dt: f32) -> f32 {
        if silent {
            return 0.0;
        }
        if !self.init {
            self.hi = v + 3.0;
            self.init = true;
        }
        if v > self.hi {
            self.hi += (v - self.hi) * 0.05;
        } else {
            self.hi -= 0.5 * dt;
        }
        dsp::map_db(v, self.hi - dsp::NORM_RANGE_DB, self.hi)
    }
}

/// Running tempo estimate + beat oscillator.
struct Tempo {
    /// Beat curve history (newest last).
    hist: VecDeque<f32>,
    /// Period in frames (0 = unknown).
    period: f32,
    candidate: f32,
    confidence: f32,
    /// Next predicted beat (seconds).
    next_beat: f64,
    last_beat: f64,
    beat_count: i64,
    slow_mean: f32,
    /// Kick-weighted curve used for the beat phase (newest last).
    phase_hist: VecDeque<f32>,
    slow_bass: f32,
    /// Consecutive large phase disagreements and their last error (s).
    disagree: u8,
    disagree_e: f64,
    /// Per-slot kick energy for downbeat estimation.
    slot_energy: [f32; 4],
}

/// Causal analyzer producing [`RealtimeFrame`]s at ~100 Hz from any sample stream.
pub struct RealtimeAnalyzer {
    sample_rate: u32,
    fps: f64,
    hop: usize,
    fe: Frontend,
    pending: Vec<f32>,
    frame_index: u64,
    agc: [Agc; 4],
    flux_ref: f32,
    onset_prev: [f32; 2],
    onset_avg: f32,
    last_onset: u64,
    tempo: Tempo,
    energy: VecDeque<f32>,
    last_drop: f64,
    /// When sound first appeared (drop detection waits for the AGC to settle).
    first_sound: Option<f64>,
    bass_flux_recent: VecDeque<f32>,
}

const HIST_SECONDS: f64 = 6.0;
const TEMPO_EVERY_FRAMES: u64 = 25;

impl RealtimeAnalyzer {
    pub fn new(sample_rate: u32) -> Self {
        let fe = Frontend::new(sample_rate);
        let hop = dsp::hop_for(sample_rate);
        let fps = sample_rate as f64 / hop as f64;
        Self {
            sample_rate,
            fps,
            hop,
            // Half a window of zeros so frame i is centred at i * hop.
            pending: vec![0.0; fe.window_len / 2],
            fe,
            frame_index: 0,
            agc: [Agc::default(); 4],
            flux_ref: 1.0,
            onset_prev: [0.0; 2],
            onset_avg: 0.0,
            last_onset: 0,
            tempo: Tempo {
                hist: VecDeque::with_capacity((HIST_SECONDS * fps) as usize + 1),
                period: 0.0,
                candidate: 0.0,
                confidence: 0.0,
                next_beat: f64::INFINITY,
                last_beat: 0.0,
                beat_count: -1,
                slow_mean: 0.0,
                phase_hist: VecDeque::with_capacity((HIST_SECONDS * fps) as usize + 1),
                slow_bass: 0.0,
                disagree: 0,
                disagree_e: 0.0,
                slot_energy: [0.0; 4],
            },
            energy: VecDeque::new(),
            last_drop: f64::NEG_INFINITY,
            first_sound: None,
            bass_flux_recent: VecDeque::new(),
        }
    }

    pub fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    /// Frames per second (~100).
    pub fn fps(&self) -> f64 {
        self.fps
    }

    /// Delay between a sample entering and the frame centred on it coming
    /// out (half a window), seconds.
    pub fn latency(&self) -> f64 {
        (self.fe.window_len / 2) as f64 / self.sample_rate as f64
    }

    /// Current tempo estimate, if locked.
    pub fn bpm(&self) -> Option<f32> {
        (self.tempo.period > 0.0).then(|| (60.0 * self.fps / self.tempo.period as f64) as f32)
    }

    /// Feed interleaved samples; appends finished frames to `out`.
    pub fn push_interleaved(
        &mut self,
        samples: &[f32],
        channels: usize,
        out: &mut Vec<RealtimeFrame>,
    ) {
        if channels <= 1 {
            self.push(samples, out);
        } else {
            self.push(&dsp::mixdown(samples, channels), out);
        }
    }

    /// Feed mono samples; appends finished frames to `out`.
    pub fn push(&mut self, mono: &[f32], out: &mut Vec<RealtimeFrame>) {
        self.pending.extend_from_slice(mono);
        let win = self.fe.window_len;
        let mut start = 0;
        while self.pending.len() - start >= win {
            let raw = self.fe.process(&self.pending[start..start + win]);
            out.push(self.step(raw));
            start += self.hop;
        }
        self.pending.drain(..start);
    }

    fn step(&mut self, raw: dsp::RawFrame) -> RealtimeFrame {
        let i = self.frame_index;
        self.frame_index += 1;
        let t = i as f64 / self.fps;
        let dt = 1.0 / self.fps as f32;
        let silent = raw.rms_db < SILENCE_DB;

        // Features.
        if !silent {
            if raw.flux > self.flux_ref {
                self.flux_ref += (raw.flux - self.flux_ref) * 0.05;
            } else {
                self.flux_ref *= 1.0 - 0.1 * dt;
            }
            self.flux_ref = self.flux_ref.max(0.5);
        }
        let onset = if silent {
            0.0
        } else {
            (raw.flux / self.flux_ref).min(1.0)
        };
        let features = FeatureFrame {
            bass: self.agc[0].map(raw.bass_db, silent, dt),
            mid: self.agc[1].map(raw.mid_db, silent, dt),
            treble: self.agc[2].map(raw.treble_db, silent, dt),
            level: self.agc[3].map(raw.rms_db, silent, dt),
            onset,
            rms_db: raw.rms_db,
        };
        let mut events = 0u8;

        // Onset: the previous frame was a peak above the running average.
        let [p2, p1] = self.onset_prev;
        if p1 > p2
            && p1 >= onset
            && p1 >= 0.15
            && p1 >= self.onset_avg + 0.08
            && i - self.last_onset >= 3
        {
            events |= music_flags::ONSET;
            self.last_onset = i;
        }
        self.onset_avg += (onset - self.onset_avg) * 0.1;
        self.onset_prev = [p1, onset];

        // Kick memory for downbeats.
        self.bass_flux_recent.push_back(raw.bass_flux);
        if self.bass_flux_recent.len() > 6 {
            self.bass_flux_recent.pop_front();
        }

        // Tempo + beats.
        events |= self.tempo_step(raw.flux, raw.bass_flux, t, silent);

        // Drops.
        self.energy.push_back(features.energy());
        if !silent && self.first_sound.is_none() {
            self.first_sound = Some(t);
        }
        let short = (0.3 * self.fps) as usize;
        let long = (6.3 * self.fps) as usize;
        if self.energy.len() > long {
            self.energy.pop_front();
        }
        // Wait for the AGC to settle (this also keeps leading silence out of
        // the comparison window).
        let warmed = self.first_sound.is_some_and(|f| t - f > 8.0);
        if self.energy.len() == long && warmed {
            let s: f32 = self.energy.iter().rev().take(short).sum::<f32>() / short as f32;
            let l: f32 = self.energy.iter().take(long - short).sum::<f32>() / (long - short) as f32;
            if s - l > 0.25 && s > 0.5 && l < 0.75 * s && t - self.last_drop > 12.0 {
                events |= music_flags::DROP;
                self.last_drop = t;
            }
        }

        let beat = self.beat_position(t);
        RealtimeFrame {
            time: t,
            features,
            events,
            beat,
        }
    }

    fn tempo_step(&mut self, flux: f32, bass_flux: f32, t: f64, silent: bool) -> u8 {
        let fps = self.fps;
        let tp = &mut self.tempo;
        // Beat curve: flux minus a slow mean (as offline).
        let k = 1.0 / (0.5 * fps as f32);
        tp.slow_mean += (flux - tp.slow_mean) * k;
        tp.slow_bass += (bass_flux - tp.slow_bass) * k;
        let (v, vb) = if silent {
            (0.0, 0.0)
        } else {
            (
                (flux - tp.slow_mean).max(0.0),
                (bass_flux - tp.slow_bass).max(0.0),
            )
        };
        tp.hist.push_back(v);
        // Kicks mark beats far more reliably than broadband hats.
        tp.phase_hist.push_back(0.5 * v + vb);
        let cap = (HIST_SECONDS * fps) as usize;
        if tp.hist.len() > cap {
            tp.hist.pop_front();
            tp.phase_hist.pop_front();
        }

        if self.frame_index.is_multiple_of(TEMPO_EVERY_FRAMES) && tp.hist.len() >= cap / 2 {
            self.update_tempo(t);
        }

        let tp = &mut self.tempo;
        let mut ev = 0;
        if tp.period > 0.0 && t >= tp.next_beat {
            let period_s = tp.period as f64 / fps;
            tp.last_beat = tp.next_beat;
            tp.next_beat += period_s;
            if tp.next_beat <= t {
                tp.next_beat = t + period_s;
            }
            tp.beat_count += 1;
            let kick = self.bass_flux_recent.iter().copied().fold(0.0, f32::max);
            let slot = tp.beat_count.rem_euclid(4) as usize;
            tp.slot_energy[slot] += (kick - tp.slot_energy[slot]) * 0.15;
            if tp.confidence > 0.05 {
                ev |= music_flags::BEAT;
                if self.beats_since_downbeat().rem_euclid(4) == 0 {
                    ev |= music_flags::DOWNBEAT;
                }
            }
        }
        ev
    }

    fn update_tempo(&mut self, t: f64) {
        let fps = self.fps;
        let tp = &mut self.tempo;
        let h: Vec<f32> = tp.hist.iter().copied().collect();
        let n = h.len();
        let m = dsp::mean(&h);
        let x: Vec<f32> = h.iter().map(|v| v - m).collect();
        let e0: f32 = x.iter().map(|v| v * v).sum();
        if e0 < 1e-6 {
            tp.confidence *= 0.5;
            return;
        }
        let lag_min = (60.0 * fps / 200.0).floor() as usize;
        let lag_max = ((60.0 * fps / 60.0).ceil() as usize).min(n / 2);
        let acf: Vec<f32> = (lag_min..=lag_max)
            .map(|l| {
                x[..n - l]
                    .iter()
                    .zip(&x[l..])
                    .map(|(a, b)| a * b)
                    .sum::<f32>()
                    / e0
                    * n as f32
                    / (n - l) as f32
            })
            .collect();
        let score: Vec<f32> = acf
            .iter()
            .enumerate()
            .map(|(k, &a)| {
                let lag = (lag_min + k) as f32;
                let bpm = 60.0 * fps as f32 / lag;
                let o = (bpm / 120.0).log2();
                let mut s = a.max(0.0) * (-0.5 * o * o).exp();
                if tp.period > 0.0 && (lag - tp.period).abs() <= 2.0 {
                    s *= 1.3; // Hysteresis.
                }
                s
            })
            .collect();
        let Some((bi, &best)) = score.iter().enumerate().max_by(|a, b| a.1.total_cmp(b.1)) else {
            return;
        };
        let off = if bi > 0 && bi + 1 < acf.len() {
            dsp::parabolic_offset(acf[bi - 1], acf[bi], acf[bi + 1])
        } else {
            0.0
        };
        let lag = (lag_min + bi) as f32 + off;
        tp.confidence = best;
        if best < 0.05 {
            return;
        }
        if tp.period == 0.0 {
            tp.period = lag;
        } else if (lag - tp.period).abs() / tp.period < 0.06 {
            tp.period += (lag - tp.period) * 0.3;
        } else if tp.candidate > 0.0 && (lag - tp.candidate).abs() / tp.candidate < 0.04 {
            // The same new tempo twice in a row: switch.
            tp.period = lag;
            tp.candidate = 0.0;
        } else {
            tp.candidate = lag;
        }

        // Phase: comb filter over the whole history on the kick-weighted
        // curve, with a continuity prior around the currently predicted grid.
        let ph: Vec<f32> = tp.phase_hist.iter().copied().collect();
        let p = tp.period;
        let period_s = p as f64 / fps;
        let n_phi = (p.round() as usize).max(1);
        let mut best_phi = 0usize;
        let mut best_s = f32::NEG_INFINITY;
        for phi in 0..n_phi {
            let mut s = 0.0;
            let mut k = 0;
            loop {
                let idx = n as f32 - 1.0 - phi as f32 - k as f32 * p;
                if idx < 1.0 {
                    break;
                }
                let j = idx.round() as usize;
                s += ph[j] + 0.5 * (ph[j - 1] + ph.get(j + 1).copied().unwrap_or(0.0));
                k += 1;
            }
            if tp.next_beat.is_finite() {
                // Circular distance (in periods) to the predicted grid.
                let beat_t = t - phi as f64 / fps + crate::analysis::ONSET_TIME_CORRECTION;
                let x = (beat_t - tp.next_beat) / period_s;
                let d = (x - x.round()).abs() as f32;
                s *= 1.0 + 0.5 * (-(d / 0.08).powi(2)).exp();
            }
            if s > best_s {
                best_s = s;
                best_phi = phi;
            }
        }
        let last_beat = t - best_phi as f64 / fps + crate::analysis::ONSET_TIME_CORRECTION;
        let mut cand = last_beat + period_s;
        while cand <= t {
            cand += period_s;
        }
        if !tp.next_beat.is_finite() {
            tp.next_beat = cand;
            return;
        }
        // Wrapped phase error.
        let mut e = cand - tp.next_beat;
        e -= (e / period_s).round() * period_s;
        if e.abs() < 0.2 * period_s {
            tp.next_beat += 0.35 * e;
            tp.disagree = 0;
        } else if tp.disagree > 0 && (e - tp.disagree_e).abs() < 0.1 * period_s {
            // A consistent different phase: accept it after three updates.
            tp.disagree += 1;
            if tp.disagree >= 3 {
                tp.next_beat += e;
                tp.disagree = 0;
            }
        } else {
            tp.disagree = 1;
            tp.disagree_e = e;
        }
    }

    /// Beat count relative to the downbeat slot (the one of four with the most
    /// kick energy).
    fn beats_since_downbeat(&self) -> i64 {
        let tp = &self.tempo;
        let down = (0..4)
            .max_by(|&a, &b| tp.slot_energy[a].total_cmp(&tp.slot_energy[b]))
            .unwrap_or(0) as i64;
        tp.beat_count - down
    }

    fn beat_position(&self, t: f64) -> Option<BeatPosition> {
        let tp = &self.tempo;
        if tp.period <= 0.0 || tp.beat_count < 0 || tp.confidence < 0.05 {
            return None;
        }
        let period_s = tp.period as f64 / self.fps;
        let phase = ((t - tp.last_beat) / period_s).clamp(0.0, 0.999) as f32;
        let rel = self.beats_since_downbeat();
        Some(BeatPosition {
            beat_index: tp.beat_count,
            phase,
            beat_in_bar: rel.rem_euclid(4) as u8,
            bar: rel.div_euclid(4),
            bpm: (60.0 / period_s) as f32,
        })
    }
}
