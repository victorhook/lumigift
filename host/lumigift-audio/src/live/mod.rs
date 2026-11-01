//! Real-time sources (capture, streaming) and causal analysis.
//!
//! ```text
//!  LiveSource ──blocks──► LiveAnalysis thread ──► RealtimeAnalyzer ──► LiveFeatures
//!  (CpalInputSource,       (bounded channel,        (causal STFT,         (implements
//!   MemorySource, later     never blocks the         AGC, onsets,          FeatureProvider)
//!   PipeWire / librespot)   capture thread)          tempo + beat PLL)          │
//!                                                                    ShowEngine.frame_at(features.now())
//! ```
//!
//! Live features are causal, so lights trail the audio by the capture buffer
//! plus half an analysis window (~25–50 ms). For perfect sync, delay the audio
//! instead (see the crate README: PipeWire null sink + delayed passthrough).

mod cpal_input;
mod realtime;

use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use parking_lot::Mutex;

pub use cpal_input::{CpalInputSource, InputDevice};
pub use realtime::{RealtimeAnalyzer, RealtimeFrame};

use crate::Result;
use crate::features::{BeatPosition, FeatureFrame, FeatureProvider};

/// Receives interleaved `f32` blocks from a [`LiveSource`]; called on the
/// source's (possibly real-time) thread, so it must not block.
pub type BlockSink = Box<dyn FnMut(&[f32]) + Send + 'static>;

/// A real-time audio source.
///
/// Implement this for new inputs (PipeWire monitor capture, librespot's audio
/// sink, a network stream, ...). Sources deliver interleaved `f32` at a fixed
/// rate and channel count.
pub trait LiveSource: Send {
    /// Human-readable description for the UI.
    fn name(&self) -> String;
    fn sample_rate(&self) -> u32;
    fn channels(&self) -> u16;
    /// Start delivering audio to `sink` until [`LiveSource::stop`].
    fn start(&mut self, sink: BlockSink) -> Result<()>;
    fn stop(&mut self);
}

/// Seconds of live frames kept for lookups.
const KEEP_SECONDS: f64 = 10.0;

#[derive(Default)]
struct LiveState {
    frames: VecDeque<RealtimeFrame>,
    /// When the newest frame was pushed.
    pushed_at: Option<Instant>,
    /// Frames pushed so far.
    total_pushed: u64,
    /// Index (in push order) of the next frame whose events were not reported.
    reported: u64,
    last_now: f64,
}

/// Live features shared between the analysis thread and the engine.
///
/// Its time line is the source's sample clock in seconds since the analysis
/// started. Use [`LiveFeatures::now`] as the engine time.
#[derive(Default)]
pub struct LiveFeatures {
    state: Mutex<LiveState>,
}

impl LiveFeatures {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// Append analysed frames (called by the analysis thread).
    pub fn push(&self, frames: &[RealtimeFrame]) {
        if frames.is_empty() {
            return;
        }
        let mut s = self.state.lock();
        s.frames.extend(frames.iter().copied());
        s.total_pushed += frames.len() as u64;
        s.pushed_at = Some(Instant::now());
        let newest = s.frames.back().map(|f| f.time).unwrap_or(0.0);
        while s
            .frames
            .front()
            .is_some_and(|f| f.time < newest - KEEP_SECONDS)
        {
            s.frames.pop_front();
        }
    }

    /// The live "now" on the feature time line: the newest frame's time plus
    /// the wall time since it arrived (at most 100 ms, so a stalled source
    /// freezes the clock). Monotonic.
    pub fn now(&self) -> f64 {
        let mut s = self.state.lock();
        let Some(newest) = s.frames.back().map(|f| f.time) else {
            return s.last_now;
        };
        let extra = s
            .pushed_at
            .map(|p| p.elapsed().as_secs_f64().min(0.1))
            .unwrap_or(0.0);
        let t = (newest + extra).max(s.last_now);
        s.last_now = t;
        t
    }

    /// The newest frame.
    pub fn latest(&self) -> Option<RealtimeFrame> {
        self.state.lock().frames.back().copied()
    }

    /// Current tempo, if locked.
    pub fn bpm(&self) -> Option<f32> {
        self.latest().and_then(|f| f.beat).map(|b| b.bpm)
    }
}

impl FeatureProvider for LiveFeatures {
    fn features_at(&self, t: f64) -> FeatureFrame {
        let s = self.state.lock();
        let k = s.frames.partition_point(|f| f.time <= t);
        match k.checked_sub(1).and_then(|i| s.frames.get(i)) {
            Some(f) => f.features,
            None => s
                .frames
                .front()
                .map_or(FeatureFrame::SILENT, |f| f.features),
        }
    }

    fn beat_at(&self, t: f64) -> Option<BeatPosition> {
        let s = self.state.lock();
        let k = s.frames.partition_point(|f| f.time <= t);
        let f = s.frames.get(k.checked_sub(1)?)?;
        let mut b = f.beat?;
        // Extrapolate the phase from the frame to t.
        let period = 60.0 / b.bpm.max(1.0) as f64;
        let x = b.phase as f64 + (t - f.time) / period;
        let whole = x.floor();
        b.phase = (x - whole) as f32;
        if whole >= 1.0 {
            let w = whole as i64;
            let pos = b.beat_in_bar as i64 + w;
            b.beat_index += w;
            b.beat_in_bar = pos.rem_euclid(4) as u8;
            b.bar += pos.div_euclid(4);
        }
        Some(b)
    }

    /// Live events are reported exactly once: every not-yet-reported frame
    /// with `time <= t1` contributes, regardless of `t0`, so frames arriving
    /// in bursts are never missed. Intended for a single consumer (the engine).
    fn events_in(&self, _t0: f64, t1: f64) -> u8 {
        let mut s = self.state.lock();
        let first_index = s.total_pushed - s.frames.len() as u64;
        let mut flags = 0;
        let mut reported = s.reported.max(first_index);
        for (i, f) in s
            .frames
            .iter()
            .enumerate()
            .skip((reported - first_index) as usize)
        {
            if f.time > t1 {
                break;
            }
            flags |= f.events;
            reported = first_index + i as u64 + 1;
        }
        s.reported = reported;
        flags
    }

    fn duration(&self) -> Option<f64> {
        None
    }
}

/// Runs a [`LiveSource`] through a [`RealtimeAnalyzer`] on a background
/// thread, publishing into a [`LiveFeatures`].
///
/// ```no_run
/// # use std::sync::Arc;
/// # use lumigift_audio::{live::*, ShowEngine, Show};
/// let source = CpalInputSource::new(None)?; // default input / monitor device
/// let live = LiveAnalysis::start(Box::new(source))?;
/// let features = live.features();
/// let mut engine = ShowEngine::new(features.clone(), Show::default());
/// let frame = engine.frame_at(features.now(), 10);
/// # Ok::<(), lumigift_audio::Error>(())
/// ```
pub struct LiveAnalysis {
    features: Arc<LiveFeatures>,
    source: Box<dyn LiveSource>,
    stop: Arc<AtomicBool>,
    worker: Option<JoinHandle<()>>,
    name: String,
}

impl LiveAnalysis {
    pub fn start(mut source: Box<dyn LiveSource>) -> Result<Self> {
        let features = LiveFeatures::new();
        let stop = Arc::new(AtomicBool::new(false));
        let (tx, rx) = mpsc::sync_channel::<Vec<f32>>(1024);
        let channels = source.channels() as usize;
        let sr = source.sample_rate();
        let f2 = features.clone();
        let stop2 = stop.clone();
        let worker = std::thread::Builder::new()
            .name("lumigift-live".into())
            .spawn(move || {
                let mut an = RealtimeAnalyzer::new(sr);
                let mut out = Vec::new();
                while !stop2.load(Ordering::Relaxed) {
                    match rx.recv_timeout(Duration::from_millis(100)) {
                        Ok(block) => {
                            an.push_interleaved(&block, channels, &mut out);
                            f2.push(&out);
                            out.clear();
                        }
                        Err(mpsc::RecvTimeoutError::Timeout) => {}
                        Err(mpsc::RecvTimeoutError::Disconnected) => break,
                    }
                }
            })
            .map_err(crate::Error::Io)?;
        source.start(Box::new(move |block: &[f32]| {
            // Never block the capture thread; drop audio if analysis lags.
            let _ = tx.try_send(block.to_vec());
        }))?;
        let name = source.name();
        Ok(Self {
            features,
            source,
            stop,
            worker: Some(worker),
            name,
        })
    }

    pub fn features(&self) -> Arc<LiveFeatures> {
        self.features.clone()
    }

    pub fn source_name(&self) -> &str {
        &self.name
    }

    pub fn stop(&mut self) {
        self.source.stop();
        self.stop.store(true, Ordering::Relaxed);
        if let Some(w) = self.worker.take() {
            let _ = w.join();
        }
    }
}

impl Drop for LiveAnalysis {
    fn drop(&mut self) {
        self.stop();
    }
}

/// A [`LiveSource`] that plays an in-memory buffer as paced blocks on a
/// thread. `speed` is the playback rate relative to real time (1.0 = real
/// time, like a sound card; tests use e.g. 50.0). Useful for tests and demos.
pub struct MemorySource {
    samples: Arc<Vec<f32>>,
    sample_rate: u32,
    channels: u16,
    speed: f64,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl MemorySource {
    pub fn new(samples: Vec<f32>, sample_rate: u32, channels: u16, speed: f64) -> Self {
        Self {
            samples: Arc::new(samples),
            sample_rate,
            channels: channels.max(1),
            speed: speed.max(1e-3),
            stop: Arc::new(AtomicBool::new(false)),
            thread: None,
        }
    }
}

impl LiveSource for MemorySource {
    fn name(&self) -> String {
        "memory buffer".into()
    }
    fn sample_rate(&self) -> u32 {
        self.sample_rate
    }
    fn channels(&self) -> u16 {
        self.channels
    }
    fn start(&mut self, mut sink: BlockSink) -> Result<()> {
        let samples = self.samples.clone();
        let stop = self.stop.clone();
        let block_frames = 512usize;
        let ch = self.channels as usize;
        let block_dur =
            Duration::from_secs_f64(block_frames as f64 / self.sample_rate as f64 / self.speed);
        self.thread = Some(
            std::thread::Builder::new()
                .name("lumigift-memsrc".into())
                .spawn(move || {
                    let start = Instant::now();
                    for (k, block) in samples.chunks(block_frames * ch).enumerate() {
                        if stop.load(Ordering::Relaxed) {
                            break;
                        }
                        let due = start + block_dur * k as u32;
                        if let Some(w) = due.checked_duration_since(Instant::now()) {
                            std::thread::sleep(w);
                        }
                        sink(block);
                    }
                })
                .map_err(crate::Error::Io)?,
        );
        Ok(())
    }
    fn stop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}
