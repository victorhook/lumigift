//! On-disk cache of analysis results.

use std::path::{Path, PathBuf};

use super::{ANALYSIS_VERSION, Analysis};
use crate::{Error, Result};

/// On-disk cache of [`Analysis`] results keyed by file content hash
/// (postcard-encoded, `<hash>-v<ANALYSIS_VERSION>.lga`).
#[derive(Debug, Clone)]
pub struct AnalysisCache {
    dir: PathBuf,
}

impl AnalysisCache {
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self { dir: dir.into() }
    }

    /// The platform cache directory (e.g. `~/.cache/lumigift/analysis`).
    pub fn default_location() -> Option<Self> {
        directories::ProjectDirs::from("", "", "lumigift")
            .map(|d| Self::new(d.cache_dir().join("analysis")))
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    fn path_for(&self, hash: &str) -> PathBuf {
        self.dir.join(format!("{hash}-v{ANALYSIS_VERSION}.lga"))
    }

    /// The cached analysis for `content_hash`, if present and readable.
    pub fn load(&self, content_hash: &str) -> Option<Analysis> {
        if content_hash.is_empty() {
            return None;
        }
        let bytes = std::fs::read(self.path_for(content_hash)).ok()?;
        let a: Analysis = postcard::from_bytes(&bytes).ok()?;
        (a.version == ANALYSIS_VERSION && a.content_hash == content_hash).then_some(a)
    }

    /// Store an analysis (needs a non-empty `content_hash`). Atomic write.
    pub fn store(&self, a: &Analysis) -> Result<()> {
        if a.content_hash.is_empty() {
            return Err(Error::Other("analysis has no content hash".into()));
        }
        std::fs::create_dir_all(&self.dir)?;
        let bytes = postcard::to_stdvec(a).map_err(|e| Error::Serde(e.to_string()))?;
        let path = self.path_for(&a.content_hash);
        let tmp = path.with_extension(format!("tmp{}", std::process::id()));
        std::fs::write(&tmp, bytes)?;
        std::fs::rename(&tmp, &path)?;
        Ok(())
    }
}
