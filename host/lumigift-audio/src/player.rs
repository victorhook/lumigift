//! Playback through cpal with an accurate "what is being heard now" clock.
//!
//! # Position model
//!
//! Each output callback tells us (via cpal's `OutputStreamTimestamp`) when the
//! first frame of the buffer it is filling will reach the DAC: `playback`,
//! measured against the same clock as `callback`. We pair the callback with
//! [`Instant::now`], so the media time of that first frame will be heard at
//! `now + (playback - callback)`. Between callbacks, [`Player::position`]
//! extrapolates with the monotonic clock. A small slew filter absorbs callback
//! jitter, so the position is smooth and monotonic at any query rate, and the
//! position holds still while silence (pause, end of track, a seek in flight)
//! is still in the output pipeline.
//!
//! The output latency estimate (`playback - callback`, smoothed) is exposed via
//! [`Player::output_latency`]. Backends that under-report latency (Bluetooth!)
//! can be corrected with [`Player::set_latency_offset`].
//!
//! # Resampling
//!
//! The track is played at its original rate through a 32-tap Kaiser-windowed
//! sinc interpolator (polyphase table, linear phase interpolation), which also
//! band-limits when downsampling. Equal rates take a bit-exact fast path.

use std::sync::Arc;
use std::time::{Duration, Instant};

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{FromSample, SampleFormat, SizedSample};
use parking_lot::Mutex;

use crate::decode::DecodedAudio;
use crate::device::{describe, device_error, device_name, find};
use crate::{Error, Result};

// ---------------------------------------------------------------------------
// Resampler
// ---------------------------------------------------------------------------

const TAPS: usize = 32;
const PHASES: usize = 256;

/// Polyphase windowed-sinc interpolator for arbitrary ratios.
#[derive(Clone)]
struct SincTable {
    /// `(PHASES + 1) * TAPS` coefficients; row `p` is for fractional offset `p / PHASES`.
    table: Vec<f32>,
}

impl SincTable {
    /// `cutoff` relative to the source Nyquist (1.0 = no band limiting).
    fn new(cutoff: f64) -> Self {
        let beta = 8.0;
        let i0b = bessel_i0(beta);
        let half = TAPS as f64 / 2.0;
        let mut table = vec![0f32; (PHASES + 1) * TAPS];
        for p in 0..=PHASES {
            let frac = p as f64 / PHASES as f64;
            let row = &mut table[p * TAPS..(p + 1) * TAPS];
            let mut sum = 0.0;
            let mut tmp = [0f64; TAPS];
            for (k, v) in tmp.iter_mut().enumerate() {
                // Tap k reads source sample floor(pos) - half + 1 + k.
                let x = (k as f64 - half + 1.0) - frac;
                let sinc = if x.abs() < 1e-9 {
                    1.0
                } else {
                    (std::f64::consts::PI * x * cutoff).sin() / (std::f64::consts::PI * x * cutoff)
                };
                let r = x / half;
                let w = if r.abs() >= 1.0 {
                    0.0
                } else {
                    bessel_i0(beta * (1.0 - r * r).sqrt()) / i0b
                };
                *v = sinc * w;
                sum += *v;
            }
            for (d, s) in row.iter_mut().zip(tmp) {
                *d = (s / sum) as f32;
            }
        }
        Self { table }
    }

    /// Interpolated sample of channel `c` at fractional source frame `pos`.
    #[inline]
    fn sample(&self, data: &[f32], channels: usize, frames: usize, c: usize, pos: f64) -> f32 {
        let i = pos.floor() as isize;
        let frac = pos - i as f64;
        let fp = frac * PHASES as f64;
        let p = (fp as usize).min(PHASES - 1);
        let t = (fp - p as f64) as f32;
        let r0 = &self.table[p * TAPS..(p + 1) * TAPS];
        let r1 = &self.table[(p + 1) * TAPS..(p + 2) * TAPS];
        let base = i - TAPS as isize / 2 + 1;
        let mut acc = 0.0f32;
        if base >= 0 && (base as usize + TAPS) <= frames {
            let b = base as usize;
            for k in 0..TAPS {
                let coef = r0[k] + (r1[k] - r0[k]) * t;
                acc += data[(b + k) * channels + c] * coef;
            }
        } else {
            for k in 0..TAPS {
                let idx = base + k as isize;
                if idx >= 0 && (idx as usize) < frames {
                    let coef = r0[k] + (r1[k] - r0[k]) * t;
                    acc += data[idx as usize * channels + c] * coef;
                }
            }
        }
        acc
    }
}

fn bessel_i0(x: f64) -> f64 {
    let mut sum = 1.0;
    let mut term = 1.0;
    let y = x * x / 4.0;
    for k in 1..50 {
        term *= y / (k * k) as f64;
        sum += term;
        if term < 1e-12 * sum {
            break;
        }
    }
    sum
}

// ---------------------------------------------------------------------------
// Clock
// ---------------------------------------------------------------------------

/// Largest callback-to-prediction error absorbed smoothly; above this the
/// clock re-anchors (seek, xrun, device hiccup).
const RESYNC_THRESHOLD: f64 = 0.030;
/// Fraction of the measured error corrected per callback.
const SLEW: f64 = 0.05;

#[derive(Debug, Clone)]
struct Clock {
    /// Media time `base_media` is heard at `base_instant`; advances 1:1 after.
    base_instant: Option<Instant>,
    base_media: f64,
    /// Never report earlier than this (start of the current contiguous run).
    floor: f64,
    /// Never report later than this (end of the audio written so far).
    ceil: f64,
    /// Smoothed `playback - callback`, seconds.
    latency: f64,
    /// Last value returned (monotonic within a run).
    last: f64,
}

impl Clock {
    fn new() -> Self {
        Self {
            base_instant: None,
            base_media: 0.0,
            floor: 0.0,
            ceil: 0.0,
            latency: 0.0,
            last: 0.0,
        }
    }

    /// Jump to `t` (seek / load): reports exactly `t` until new audio is heard.
    fn hold_at(&mut self, t: f64) {
        self.base_instant = None;
        self.base_media = t;
        self.floor = t;
        self.ceil = t;
        self.last = t;
    }

    fn position(&mut self, now: Instant) -> f64 {
        let p = match self.base_instant {
            None => self.base_media,
            Some(bi) => (self.base_media + signed_secs(now, bi))
                .clamp(self.floor, self.ceil.max(self.floor)),
        };
        let p = p.max(self.last);
        self.last = p;
        p
    }

    /// Called by the audio callback for a buffer whose first frame (media time
    /// `m0`) is heard at `heard_at`; the buffer ends at media time `m1`.
    fn on_audio(&mut self, heard_at: Instant, m0: f64, m1: f64, contiguous: bool, latency: f64) {
        self.update_latency(latency);
        match self.base_instant {
            Some(bi) if contiguous => {
                let predicted = self.base_media + signed_secs(heard_at, bi);
                let err = m0 - predicted;
                if err.abs() > RESYNC_THRESHOLD {
                    self.base_instant = Some(heard_at);
                    self.base_media = m0;
                } else {
                    self.base_media += SLEW * err;
                }
            }
            _ => {
                // New run (start, resume, seek): hold at m0 until it is heard.
                self.base_instant = Some(heard_at);
                self.base_media = m0;
                self.floor = m0;
                self.last = self.last.min(m0);
            }
        }
        self.ceil = m1;
    }

    /// Smooth in a new `playback - callback` measurement.
    fn update_latency(&mut self, latency: f64) {
        self.latency = if self.latency == 0.0 {
            latency
        } else {
            self.latency + 0.1 * (latency - self.latency)
        };
    }
}

/// `a - b` in seconds (negative if `a` is earlier).
fn signed_secs(a: Instant, b: Instant) -> f64 {
    if a >= b {
        (a - b).as_secs_f64()
    } else {
        -(b - a).as_secs_f64()
    }
}

// ---------------------------------------------------------------------------
// Render core (device independent, unit-testable)
// ---------------------------------------------------------------------------

/// Playback state as seen by the UI.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlaybackState {
    /// Nothing loaded.
    Empty,
    Paused,
    Playing,
    /// Reached the end of the track (position holds at the end).
    Ended,
}

struct Core {
    audio: Option<Arc<DecodedAudio>>,
    /// Read position in source frames.
    pos: f64,
    playing: bool,
    /// Declick envelope 0..=1 (fades over ~5 ms on play/pause).
    fade: f32,
    volume: f32,
    gain: f32,
    out_rate: u32,
    out_channels: usize,
    sinc: Option<(u32, Arc<SincTable>)>,
    /// The next buffer starts a new contiguous run.
    discontinuity: bool,
    ended: bool,
}

impl Core {
    fn new() -> Self {
        Self {
            audio: None,
            pos: 0.0,
            playing: false,
            fade: 0.0,
            volume: 1.0,
            gain: 1.0,
            out_rate: 48_000,
            out_channels: 2,
            sinc: None,
            discontinuity: true,
            ended: false,
        }
    }

    /// Silence and rewind to the start; the next buffer starts a new run.
    fn stop(&mut self) {
        self.playing = false;
        self.fade = 0.0;
        self.pos = 0.0;
        self.ended = false;
        self.discontinuity = true;
    }

    fn src_rate(&self) -> f64 {
        self.audio
            .as_ref()
            .map(|a| a.sample_rate as f64)
            .unwrap_or(self.out_rate as f64)
    }

    fn table(&mut self) -> Option<Arc<SincTable>> {
        let src = self.audio.as_ref()?.sample_rate;
        if src == self.out_rate {
            return None;
        }
        if self.sinc.as_ref().is_none_or(|(r, _)| *r != src) {
            let cutoff = (self.out_rate as f64 / src as f64).min(1.0) * 0.96;
            self.sinc = Some((src, Arc::new(SincTable::new(cutoff))));
        }
        self.sinc.as_ref().map(|s| s.1.clone())
    }

    /// Fill an interleaved f32 buffer of the output layout. Returns the media
    /// times `(m0, m1)` of the buffer's start and end and whether it carried
    /// audio (as opposed to pure silence while paused).
    fn render(&mut self, out: &mut [f32]) -> Option<(f64, f64, bool)> {
        out.fill(0.0);
        let och = self.out_channels.max(1);
        let n_out = out.len() / och;
        let audible = self.playing || self.fade > 0.0;
        let audio = self.audio.clone()?;
        if !audible {
            return None;
        }
        let src_rate = audio.sample_rate as f64;
        let step = src_rate / self.out_rate as f64;
        let table = self.table();
        let ich = audio.channels as usize;
        let frames = audio.frames();
        let data = &audio.samples[..];
        let fade_step = 1.0 / (0.005 * self.out_rate as f32);
        let gain_step = 1.0 / (0.02 * self.out_rate as f32);
        let m0 = self.pos / src_rate;
        let contiguous = !self.discontinuity;
        self.discontinuity = false;
        for frame in out.chunks_exact_mut(och).take(n_out) {
            if self.pos >= frames as f64 {
                self.playing = false;
                self.fade = 0.0;
                self.ended = true;
                break;
            }
            let target = if self.playing { 1.0 } else { 0.0 };
            if self.fade < target {
                self.fade = (self.fade + fade_step).min(1.0);
            } else if self.fade > target {
                self.fade = (self.fade - fade_step).max(0.0);
            }
            if self.gain < self.volume {
                self.gain = (self.gain + gain_step).min(self.volume);
            } else if self.gain > self.volume {
                self.gain = (self.gain - gain_step).max(self.volume);
            }
            let g = self.gain * self.fade;
            for (c, o) in frame.iter_mut().enumerate() {
                let sc = c % ich;
                let v = match &table {
                    None => data[self.pos as usize * ich + sc],
                    Some(t) => t.sample(data, ich, frames, sc, self.pos),
                };
                *o = v * g;
            }
            self.pos += step;
            if self.fade == 0.0 && !self.playing {
                // Pause fade finished: the rest of the buffer is silence.
                self.discontinuity = true;
                break;
            }
        }
        if self.pos >= frames as f64 {
            self.playing = false;
            self.ended = true;
        }
        Some((m0, self.pos / src_rate, contiguous))
    }
}

// ---------------------------------------------------------------------------
// Player
// ---------------------------------------------------------------------------

/// An audio output device.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutputDevice {
    /// Stable id to pass to [`Player::with_device`] / [`Player::set_device`].
    pub id: String,
    /// Human-readable name.
    pub name: String,
    pub is_default: bool,
}

struct Shared {
    core: Mutex<Core>,
    clock: Mutex<Clock>,
    latency_offset: Mutex<f64>,
    error: Mutex<Option<String>>,
}

/// Plays a [`DecodedAudio`] and reports the position currently being heard.
///
/// All methods take `&self` and are thread-safe; wrap it in an `Arc` to share
/// it between the GUI and an engine thread.
pub struct Player {
    shared: Arc<Shared>,
    stream: Mutex<Option<cpal::Stream>>,
    device_name: Mutex<String>,
    device_rate: Mutex<u32>,
}

impl Player {
    /// Output devices of the default host.
    pub fn output_devices() -> Result<Vec<OutputDevice>> {
        let host = cpal::default_host();
        let devices = host.output_devices().map_err(device_error)?;
        Ok(describe(devices, host.default_output_device())
            .into_iter()
            .map(|(id, name, is_default)| OutputDevice {
                id,
                name,
                is_default,
            })
            .collect())
    }

    /// Open the default output device.
    pub fn new() -> Result<Self> {
        Self::with_device(None)
    }

    /// Open the device with the given [`OutputDevice::id`] (`None` = default).
    pub fn with_device(id: Option<&str>) -> Result<Self> {
        let p = Self {
            shared: Arc::new(Shared {
                core: Mutex::new(Core::new()),
                clock: Mutex::new(Clock::new()),
                latency_offset: Mutex::new(0.0),
                error: Mutex::new(None),
            }),
            stream: Mutex::new(None),
            device_name: Mutex::new(String::new()),
            device_rate: Mutex::new(0),
        };
        p.set_device(id)?;
        Ok(p)
    }

    /// Switch to another output device, keeping track, position and state.
    pub fn set_device(&self, id: Option<&str>) -> Result<()> {
        let device = find(id, |h| h.default_output_device(), "output")?;
        let config = device.default_output_config().map_err(device_error)?;
        let fmt = config.sample_format();
        let cfg: cpal::StreamConfig = config.into();
        // Drop the old stream before opening the new one (ALSA hw devices are exclusive).
        *self.stream.lock() = None;
        {
            let mut core = self.shared.core.lock();
            core.out_rate = cfg.sample_rate;
            core.out_channels = cfg.channels as usize;
            core.sinc = None;
            core.discontinuity = true;
            let t = core.pos / core.src_rate();
            self.shared.clock.lock().hold_at(t);
        }
        let stream = match fmt {
            SampleFormat::F32 => build::<f32>(&device, &cfg, self.shared.clone()),
            SampleFormat::I16 => build::<i16>(&device, &cfg, self.shared.clone()),
            SampleFormat::I32 => build::<i32>(&device, &cfg, self.shared.clone()),
            SampleFormat::U16 => build::<u16>(&device, &cfg, self.shared.clone()),
            SampleFormat::U8 => build::<u8>(&device, &cfg, self.shared.clone()),
            SampleFormat::I8 => build::<i8>(&device, &cfg, self.shared.clone()),
            SampleFormat::F64 => build::<f64>(&device, &cfg, self.shared.clone()),
            other => return Err(Error::Device(format!("unsupported sample format {other}"))),
        }?;
        stream.play().map_err(device_error)?;
        *self.stream.lock() = Some(stream);
        *self.device_name.lock() = device_name(&device).unwrap_or_default();
        *self.device_rate.lock() = cfg.sample_rate;
        *self.shared.error.lock() = None;
        Ok(())
    }

    pub fn device_name(&self) -> String {
        self.device_name.lock().clone()
    }

    /// Output sample rate of the device.
    pub fn device_sample_rate(&self) -> u32 {
        *self.device_rate.lock()
    }

    /// Load a track (paused at 0). Cheap: no copy or resampling happens here.
    pub fn load(&self, audio: Arc<DecodedAudio>) {
        let mut core = self.shared.core.lock();
        core.stop();
        core.audio = Some(audio);
        core.sinc = None;
        self.shared.clock.lock().hold_at(0.0);
    }

    /// Unload the track and go silent.
    pub fn unload(&self) {
        let mut core = self.shared.core.lock();
        core.stop();
        core.audio = None;
        self.shared.clock.lock().hold_at(0.0);
    }

    /// The loaded track, if any.
    pub fn audio(&self) -> Option<Arc<DecodedAudio>> {
        self.shared.core.lock().audio.clone()
    }

    pub fn play(&self) {
        let mut core = self.shared.core.lock();
        if core.audio.is_none() {
            return;
        }
        if core.ended {
            core.pos = 0.0;
            core.ended = false;
            self.shared.clock.lock().hold_at(0.0);
        }
        if !core.playing {
            core.playing = true;
            if core.fade == 0.0 {
                core.discontinuity = true;
            }
        }
    }

    pub fn pause(&self) {
        self.shared.core.lock().playing = false;
    }

    pub fn toggle(&self) {
        if self.is_playing() {
            self.pause()
        } else {
            self.play()
        }
    }

    /// Pause and rewind to the start.
    pub fn stop(&self) {
        let mut core = self.shared.core.lock();
        core.stop();
        self.shared.clock.lock().hold_at(0.0);
    }

    /// Jump to `secs` (clamped to the track). Keeps playing if playing.
    pub fn seek(&self, secs: f64) {
        let mut core = self.shared.core.lock();
        let Some(a) = core.audio.clone() else { return };
        let t = secs.clamp(0.0, a.duration());
        let sr = a.sample_rate as f64;
        core.pos = if a.sample_rate == core.out_rate {
            (t * sr).round()
        } else {
            t * sr
        };
        core.ended = false;
        core.discontinuity = true;
        self.shared.clock.lock().hold_at(core.pos / sr);
    }

    /// Linear gain 0.0..=2.0 (smoothed, no zipper noise).
    pub fn set_volume(&self, v: f32) {
        self.shared.core.lock().volume = v.clamp(0.0, 2.0);
    }

    pub fn volume(&self) -> f32 {
        self.shared.core.lock().volume
    }

    pub fn state(&self) -> PlaybackState {
        let c = self.shared.core.lock();
        match (&c.audio, c.playing, c.ended) {
            (None, _, _) => PlaybackState::Empty,
            (_, true, _) => PlaybackState::Playing,
            (_, false, true) => PlaybackState::Ended,
            _ => PlaybackState::Paused,
        }
    }

    pub fn is_playing(&self) -> bool {
        self.shared.core.lock().playing
    }

    /// Length of the loaded track in seconds (0 when empty).
    pub fn duration(&self) -> f64 {
        self.shared
            .core
            .lock()
            .audio
            .as_ref()
            .map(|a| a.duration())
            .unwrap_or(0.0)
    }

    /// Media time (seconds) of the audio reaching the listener right now:
    /// output latency compensated, smooth and monotonic between seeks. Cheap
    /// enough to call at several kHz.
    pub fn position(&self) -> f64 {
        let off = *self.shared.latency_offset.lock();
        let p = self.shared.clock.lock().position(Instant::now());
        (p - off).max(0.0)
    }

    /// The position of the read head (what is being written to the device,
    /// i.e. ahead of [`Player::position`] by the output latency).
    pub fn write_position(&self) -> f64 {
        let c = self.shared.core.lock();
        c.pos / c.src_rate()
    }

    /// Output latency reported by the backend (smoothed `playback - callback`),
    /// plus the user offset, in seconds.
    pub fn output_latency(&self) -> f64 {
        self.shared.clock.lock().latency + *self.shared.latency_offset.lock()
    }

    /// Extra latency (seconds) the backend does not know about, e.g. Bluetooth
    /// speakers or an external DSP. Positive values delay [`Player::position`].
    pub fn set_latency_offset(&self, secs: f64) {
        *self.shared.latency_offset.lock() = secs;
    }

    pub fn latency_offset(&self) -> f64 {
        *self.shared.latency_offset.lock()
    }

    /// Last stream error (device unplugged, ...), if any.
    pub fn last_error(&self) -> Option<String> {
        self.shared.error.lock().clone()
    }
}

fn build<T>(
    device: &cpal::Device,
    cfg: &cpal::StreamConfig,
    shared: Arc<Shared>,
) -> Result<cpal::Stream>
where
    T: SizedSample + FromSample<f32>,
{
    let mut scratch: Vec<f32> = Vec::with_capacity(16384);
    let err_shared = shared.clone();
    device
        .build_output_stream(
            *cfg,
            move |data: &mut [T], info: &cpal::OutputCallbackInfo| {
                let now = Instant::now();
                let ts = info.timestamp();
                let latency = ts
                    .playback
                    .checked_duration_since(ts.callback)
                    .unwrap_or(Duration::ZERO);
                if scratch.len() < data.len() {
                    scratch.resize(data.len(), 0.0);
                }
                let buf = &mut scratch[..data.len()];
                let rendered = shared.core.lock().render(buf);
                for (d, s) in data.iter_mut().zip(buf.iter()) {
                    *d = T::from_sample(*s);
                }
                let mut clock = shared.clock.lock();
                match rendered {
                    Some((m0, m1, contiguous)) => {
                        clock.on_audio(now + latency, m0, m1, contiguous, latency.as_secs_f64())
                    }
                    None => clock.update_latency(latency.as_secs_f64()),
                }
            },
            move |e| {
                log::warn!("audio output error: {e}");
                *err_shared.error.lock() = Some(e.to_string());
            },
            None,
        )
        .map_err(device_error)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sine(rate: u32, f: f64, secs: f64) -> Arc<DecodedAudio> {
        let n = (rate as f64 * secs) as usize;
        let s = (0..n)
            .map(|i| (2.0 * std::f64::consts::PI * f * i as f64 / rate as f64).sin() as f32 * 0.5)
            .collect();
        Arc::new(DecodedAudio::from_samples(s, rate, 1))
    }

    #[test]
    fn resampler_preserves_sine() {
        let mut core = Core::new();
        core.out_rate = 48_000;
        core.out_channels = 1;
        core.audio = Some(sine(44_100, 1000.0, 1.0));
        core.playing = true;
        core.fade = 1.0;
        let mut out = vec![0f32; 4800];
        core.render(&mut out);
        core.render(&mut out);
        // Compare against the ideal 1 kHz sine at 48 kHz.
        let t0 = 4800.0;
        let err: f32 = out
            .iter()
            .enumerate()
            .skip(100)
            .map(|(i, v)| {
                let ideal = (2.0 * std::f64::consts::PI * 1000.0 * (t0 + i as f64) / 48_000.0).sin()
                    as f32
                    * 0.5;
                (v - ideal).abs()
            })
            .fold(0.0, f32::max);
        assert!(err < 2e-3, "max error {err}");
    }

    #[test]
    fn clock_is_smooth_and_latency_compensated() {
        let mut core = Core::new();
        core.out_rate = 48_000;
        core.out_channels = 2;
        core.audio = Some(sine(48_000, 440.0, 10.0));
        core.playing = true;
        let mut clock = Clock::new();
        clock.hold_at(0.0);
        let t0 = Instant::now();
        let latency = 0.040;
        let mut buf = vec![0f32; 480 * 2];
        let mut last = 0.0;
        // Simulate 2 s of callbacks every 10 ms with ±1 ms jitter, querying at 1 kHz.
        for k in 0..200u32 {
            let cb = t0 + Duration::from_micros(k as u64 * 10_000 + (k as u64 * 7919 % 2000));
            let (m0, m1, cont) = core.render(&mut buf).unwrap();
            clock.on_audio(cb + Duration::from_secs_f64(latency), m0, m1, cont, latency);
            for q in 0..10u64 {
                let now = t0 + Duration::from_micros(k as u64 * 10_000 + q * 1000 + 500);
                let p = clock.position(now);
                assert!(p >= last, "position went backwards");
                last = p;
                let ideal = (now - t0).as_secs_f64() - latency;
                if k > 10 {
                    assert!(
                        (p - ideal.max(0.0)).abs() < 0.004,
                        "k={k} p={p} ideal={ideal}"
                    );
                }
            }
        }
    }

    #[test]
    fn clock_holds_on_seek_until_heard() {
        let mut clock = Clock::new();
        let t0 = Instant::now();
        clock.on_audio(t0 + Duration::from_millis(30), 0.0, 0.01, false, 0.03);
        clock.hold_at(5.0);
        assert_eq!(clock.position(t0 + Duration::from_millis(31)), 5.0);
        clock.on_audio(t0 + Duration::from_millis(80), 5.0, 5.01, false, 0.03);
        assert_eq!(clock.position(t0 + Duration::from_millis(60)), 5.0);
        let p = clock.position(t0 + Duration::from_millis(85));
        assert!((p - 5.005).abs() < 1e-6, "{p}");
    }
}
