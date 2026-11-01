//! Background loading: read + hash + decode + analyse (with cache).

use std::path::PathBuf;
use std::sync::Arc;

use crate::Result;
use crate::analysis::{Analysis, AnalysisCache, analyze_with};
use crate::decode::{DecodedAudio, decode_file_with};
use crate::job::{Job, JobContext};

/// A decoded and analysed track, ready for [`crate::Player::load`] and
/// [`crate::ShowEngine`].
#[derive(Clone)]
pub struct LoadedTrack {
    pub path: PathBuf,
    pub audio: Arc<DecodedAudio>,
    pub analysis: Arc<Analysis>,
    /// True if the analysis came from the on-disk cache.
    pub from_cache: bool,
}

/// Decode and analyse `path` in the background. With a cache, a previously
/// analysed file (same content hash) skips the analysis step.
///
/// ```no_run
/// # use lumigift_audio::*;
/// let mut job = load_track("song.mp3", AnalysisCache::default_location());
/// loop {
///     if let Some(result) = job.try_take() {
///         let track = result?;
///         println!("{} BPM", track.analysis.bpm);
///         break;
///     }
///     println!("{} {:.0}%", job.stage(), job.progress() * 100.0);
///     std::thread::sleep(std::time::Duration::from_millis(100));
/// }
/// # Ok::<(), lumigift_audio::Error>(())
/// ```
pub fn load_track(path: impl Into<PathBuf>, cache: Option<AnalysisCache>) -> Job<LoadedTrack> {
    let path = path.into();
    Job::spawn("lumigift-load", move |ctx| {
        let audio = Arc::new(decode_file_with(&path, &ctx.sub(0.0, 0.35))?);
        let (analysis, from_cache) = analyze_cached(&audio, cache.as_ref(), &ctx.sub(0.35, 1.0))?;
        Ok(LoadedTrack {
            path,
            audio,
            analysis,
            from_cache,
        })
    })
}

/// Analyse already decoded audio in the background (uses the cache if the
/// audio has a content hash).
pub fn analyze_async(audio: Arc<DecodedAudio>, cache: Option<AnalysisCache>) -> Job<Arc<Analysis>> {
    Job::spawn("lumigift-analyze", move |ctx| {
        Ok(analyze_cached(&audio, cache.as_ref(), ctx)?.0)
    })
}

fn analyze_cached(
    audio: &DecodedAudio,
    cache: Option<&AnalysisCache>,
    ctx: &JobContext,
) -> Result<(Arc<Analysis>, bool)> {
    if let Some(a) = cache.and_then(|c| c.load(&audio.content_hash)) {
        return Ok((Arc::new(a), true));
    }
    ctx.set_stage("Mixing down");
    let mono = audio.mono();
    ctx.check_cancel()?;
    let mut a = analyze_with(&mono, audio.sample_rate, ctx)?;
    a.content_hash = audio.content_hash.clone();
    if let Some(c) = cache
        && !a.content_hash.is_empty()
        && let Err(e) = c.store(&a)
    {
        log::warn!("could not write analysis cache: {e}");
    }
    Ok((Arc::new(a), false))
}
