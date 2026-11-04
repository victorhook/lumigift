//! Audio → light-show pipeline for Lumigift.
//!
//! ```text
//!  file ──► load_track (Job) ──► LoadedTrack { audio, analysis }
//!                                   │              │
//!                         Player::load(audio)      ├─► Show::auto_generate(&analysis) ─► edit ─► .lgshow
//!                                   │              │
//!                          player.position() ──► ShowEngine(analysis, show).frame_for_playback(pos, slots)
//!                                                                 │
//!                                                        MusicFrame @ 100 Hz ─► lumigift-core
//! ```
//!
//! * [`load_track`] reads, hashes, decodes ([`decode`], symphonia) and analyses
//!   ([`analysis`]) a file on a background [`Job`] with progress and cancel.
//!   Results are cached on disk by content hash ([`AnalysisCache`]).
//! * [`Player`] plays through cpal and reports [`Player::position`]: the media
//!   time being *heard right now*, compensated for output latency and smooth
//!   at any query rate.
//! * [`Analysis`] holds per-frame features (~100 fps), onsets, the beat grid
//!   with downbeats, tempo, sections, drops, plus waveform/spectrogram
//!   overviews for drawing. It implements [`FeatureProvider`].
//! * [`Show`] is the editable cue list (JSON `*.lgshow`) with
//!   [`Show::auto_generate`].
//! * [`ShowEngine`] combines a [`FeatureProvider`] and a [`Show`] into
//!   [`lumigift_proto::MusicFrame`]s. See its module docs for look-ahead.
//! * [`live`] has the real-time side: the [`live::LiveSource`] trait, a
//!   [`live::CpalInputSource`], and the causal [`live::RealtimeAnalyzer`] whose
//!   output ([`live::LiveFeatures`]) is also a [`FeatureProvider`], so the same
//!   engine drives lights from live input.
//!
//! # Example
//!
//! ```no_run
//! use std::sync::Arc;
//! use lumigift_audio::*;
//!
//! let track = load_track("song.flac", AnalysisCache::default_location()).wait()?;
//! let player = Player::new()?;
//! player.load(track.audio.clone());
//! let mut engine = ShowEngine::new(track.analysis.clone(), Show::auto_generate(&track.analysis));
//! engine.set_lookahead(0.020); // radio + render latency
//! player.play();
//! loop {
//!     let frame = engine.frame_for_playback(player.position(), 10);
//!     // core.send_music(frame) ...
//!     std::thread::sleep(std::time::Duration::from_millis(10));
//! #   break;
//! }
//! # Ok::<(), lumigift_audio::Error>(())
//! ```
//!
//! All slow work happens on background threads; every method meant for a GUI
//! thread (`Player::*`, `ShowEngine::frame_at`, `Analysis` lookups,
//! `Job::progress`) is cheap.

pub mod analysis;
pub mod decode;
mod device;
#[doc(hidden)]
pub mod eagerness;
pub mod engine;
pub mod features;
pub mod job;
pub mod live;
mod loader;
pub mod player;
pub mod show;
#[doc(hidden)]
pub mod synth;

pub use analysis::{
    ANALYSIS_VERSION, Analysis, AnalysisCache, Beat, DropEvent, Onset, Section, SectionKind, Snap,
    Spectrogram, WaveBucket, Waveform,
};
pub use decode::{CoverArt, DecodedAudio, TrackMetadata, content_hash, decode_bytes, decode_file};
pub use engine::ShowEngine;
pub use features::{BeatPosition, FeatureFrame, FeatureProvider, NOTE_NAMES};
pub use job::{Job, JobContext};
pub use loader::{LoadedTrack, analyze_async, load_track};
pub use player::{OutputDevice, PlaybackState, Player};
pub use show::{BeatSync, Cue, OneShot, OneShotKind, Show, ShowSettings, SlotOverride, TrackRef};

/// Errors from this crate.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("decode error: {0}")]
    Decode(String),
    #[error("audio device error: {0}")]
    Device(String),
    #[error("serialization error: {0}")]
    Serde(String),
    #[error("cancelled")]
    Cancelled,
    #[error("{0}")]
    Other(String),
}

pub type Result<T> = std::result::Result<T, Error>;
