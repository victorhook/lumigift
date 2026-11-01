//! Whole-file decoding with symphonia.
//!
//! Songs are small (a 4-minute stereo track at 44.1 kHz is ~85 MB as f32), so
//! the entire file is decoded into memory as interleaved `f32`, keeping the
//! original sample rate and channel count.

use std::io::Cursor;
use std::path::Path;

use serde::{Deserialize, Serialize};
use symphonia::core::codecs::audio::AudioDecoderOptions;
use symphonia::core::errors::Error as SymError;
use symphonia::core::formats::probe::Hint;
use symphonia::core::formats::{FormatOptions, FormatReader, TrackType};
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::{MetadataOptions, MetadataRevision, StandardTag, StandardVisualKey};

use crate::job::JobContext;
use crate::{Error, Result};

/// Embedded album art, still encoded (usually JPEG or PNG).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CoverArt {
    /// MIME type, e.g. `image/jpeg`, if the container says so.
    pub mime: Option<String>,
    pub data: Vec<u8>,
}

/// Tags for the UI. Every field is optional except the duration.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct TrackMetadata {
    /// Title tag, or the file stem when the file has no title tag.
    pub title: Option<String>,
    pub artist: Option<String>,
    pub album: Option<String>,
    /// Length of the decoded audio in seconds.
    pub duration: f64,
    /// Front cover (or the first picture) if the file embeds one.
    pub cover_art: Option<CoverArt>,
}

/// A fully decoded track.
#[derive(Debug, Clone)]
pub struct DecodedAudio {
    /// Interleaved samples, `frames() * channels` long, nominally in -1..=1.
    pub samples: Vec<f32>,
    pub sample_rate: u32,
    pub channels: u16,
    pub metadata: TrackMetadata,
    /// BLAKE3 hash (hex) of the source file's bytes; the analysis cache key.
    /// Empty for audio that did not come from a file.
    pub content_hash: String,
}

impl DecodedAudio {
    /// Wrap raw interleaved samples (synthetic signals, captured audio, tests).
    pub fn from_samples(samples: Vec<f32>, sample_rate: u32, channels: u16) -> Self {
        let channels = channels.max(1);
        let duration = samples.len() as f64 / channels as f64 / sample_rate as f64;
        Self {
            samples,
            sample_rate,
            channels,
            metadata: TrackMetadata {
                duration,
                ..Default::default()
            },
            content_hash: String::new(),
        }
    }

    /// Number of sample frames (samples per channel).
    pub fn frames(&self) -> usize {
        self.samples.len() / self.channels as usize
    }

    /// Duration in seconds.
    pub fn duration(&self) -> f64 {
        self.frames() as f64 / self.sample_rate as f64
    }

    /// Mono mixdown (channel average) used for analysis.
    pub fn mono(&self) -> Vec<f32> {
        crate::analysis::dsp::mixdown(&self.samples, self.channels as usize)
    }
}

/// BLAKE3 hash of `bytes` as lowercase hex (the analysis cache key).
pub fn content_hash(bytes: &[u8]) -> String {
    blake3::hash(bytes).to_hex().to_string()
}

/// Decode a whole file on the calling thread (blocking; ~0.2–1 s for a song).
/// Use [`crate::load_track`] to do this in the background.
pub fn decode_file(path: impl AsRef<Path>) -> Result<DecodedAudio> {
    decode_file_with(path.as_ref(), &JobContext::detached())
}

pub(crate) fn decode_file_with(path: &Path, ctx: &JobContext) -> Result<DecodedAudio> {
    ctx.set_stage("Reading file");
    let bytes = std::fs::read(path)?;
    ctx.check_cancel()?;
    let ext = path.extension().and_then(|e| e.to_str());
    let mut audio = decode_bytes_with(bytes, ext, ctx)?;
    if audio.metadata.title.is_none() {
        audio.metadata.title = path.file_stem().map(|s| s.to_string_lossy().into_owned());
    }
    Ok(audio)
}

/// Decode an in-memory file. `ext_hint` is the file extension (e.g. `"mp3"`),
/// which speeds up format probing. `content_hash` is computed from `bytes`.
pub fn decode_bytes(bytes: Vec<u8>, ext_hint: Option<&str>) -> Result<DecodedAudio> {
    decode_bytes_with(bytes, ext_hint, &JobContext::detached())
}

fn decode_bytes_with(
    bytes: Vec<u8>,
    ext_hint: Option<&str>,
    ctx: &JobContext,
) -> Result<DecodedAudio> {
    let hash = content_hash(&bytes);
    ctx.set_stage("Decoding");
    let mss = MediaSourceStream::new(Box::new(Cursor::new(bytes)), Default::default());
    let mut hint = Hint::new();
    if let Some(e) = ext_hint {
        hint.with_extension(e);
    }
    let mut format = symphonia::default::get_probe()
        .probe(
            &hint,
            mss,
            FormatOptions::default(),
            MetadataOptions::default(),
        )
        .map_err(|e| Error::Decode(format!("unsupported or corrupt file: {e}")))?;

    let track = format
        .default_track(TrackType::Audio)
        .ok_or_else(|| Error::Decode("no audio track".into()))?;
    let track_id = track.id;
    let total_frames = track.num_frames;
    let params = track
        .codec_params
        .as_ref()
        .and_then(|p| p.audio())
        .ok_or_else(|| Error::Decode("track has no audio codec parameters".into()))?
        .clone();
    let mut decoder = symphonia::default::get_codecs()
        .make_audio_decoder(&params, &AudioDecoderOptions::default())
        .map_err(|e| Error::Decode(format!("no decoder: {e}")))?;

    let mut meta = TrackMetadata::default();
    read_metadata(format.as_mut(), track_id, &mut meta);

    let mut samples: Vec<f32> = Vec::new();
    if let Some(n) = total_frames {
        let ch = params.channels.as_ref().map(|c| c.count()).unwrap_or(2);
        samples.reserve(n as usize * ch);
    }
    let mut scratch: Vec<f32> = Vec::new();
    let mut sample_rate = params.sample_rate.unwrap_or(0);
    let mut channels = params
        .channels
        .as_ref()
        .map(|c| c.count() as u16)
        .unwrap_or(0);
    let mut packets = 0u32;

    loop {
        let packet = match format.next_packet() {
            Ok(Some(p)) => p,
            Ok(None) => break,
            Err(SymError::ResetRequired) => break,
            Err(SymError::IoError(e)) if e.kind() == std::io::ErrorKind::UnexpectedEof => break,
            Err(e) => return Err(Error::Decode(e.to_string())),
        };
        if packet.track_id != track_id {
            continue;
        }
        match decoder.decode(&packet) {
            Ok(buf) => {
                let spec = buf.spec();
                if sample_rate == 0 {
                    sample_rate = spec.rate();
                }
                let ch = spec.channels().count() as u16;
                if channels == 0 {
                    channels = ch;
                }
                if ch != channels {
                    // Channel layout changed mid-stream (rare); skip the packet.
                    continue;
                }
                buf.copy_to_vec_interleaved(&mut scratch);
                samples.extend_from_slice(&scratch);
            }
            Err(SymError::DecodeError(_)) => continue,
            Err(e) => return Err(Error::Decode(e.to_string())),
        }
        packets += 1;
        if packets.is_multiple_of(64) {
            ctx.check_cancel()?;
            if let (Some(n), true) = (total_frames, channels > 0) {
                ctx.set_progress(
                    (samples.len() as f64 / channels as f64 / n as f64).min(1.0) as f32
                );
            }
        }
    }

    if channels == 0 || sample_rate == 0 || samples.is_empty() {
        return Err(Error::Decode("file contains no decodable audio".into()));
    }
    // Late metadata (e.g. tags at the end of the file).
    read_metadata(format.as_mut(), track_id, &mut meta);
    for s in &mut samples {
        if !s.is_finite() {
            *s = 0.0;
        }
    }
    meta.duration = samples.len() as f64 / channels as f64 / sample_rate as f64;
    Ok(DecodedAudio {
        samples,
        sample_rate,
        channels,
        metadata: meta,
        content_hash: hash,
    })
}

/// Merge the latest metadata revision of `format` into `meta`.
fn read_metadata(format: &mut dyn FormatReader, track_id: u32, meta: &mut TrackMetadata) {
    if let Some(rev) = format.metadata().skip_to_latest() {
        apply_metadata(rev, track_id, meta);
    }
}

fn apply_metadata(rev: &MetadataRevision, track_id: u32, meta: &mut TrackMetadata) {
    let containers = std::iter::once(&rev.media).chain(
        rev.per_track
            .iter()
            .filter(|p| p.track_id == track_id as u64)
            .map(|p| &p.metadata),
    );
    for c in containers {
        for tag in &c.tags {
            match &tag.std {
                Some(StandardTag::TrackTitle(s)) if meta.title.is_none() => {
                    meta.title = Some(s.to_string())
                }
                Some(StandardTag::Artist(s)) if meta.artist.is_none() => {
                    meta.artist = Some(s.to_string())
                }
                Some(StandardTag::Album(s)) if meta.album.is_none() => {
                    meta.album = Some(s.to_string())
                }
                _ => {}
            }
        }
        let front = c
            .visuals
            .iter()
            .find(|v| v.usage == Some(StandardVisualKey::FrontCover));
        if let Some(v) = front.or(c.visuals.first())
            && (meta.cover_art.is_none() || v.usage == Some(StandardVisualKey::FrontCover))
        {
            meta.cover_art = Some(CoverArt {
                mime: v.media_type.clone(),
                data: v.data.to_vec(),
            });
        }
    }
}
