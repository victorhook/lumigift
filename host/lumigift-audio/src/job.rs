//! Background jobs with progress, stage text and cancellation.
//!
//! Everything slow in this crate (decoding, hashing, analysis) can run through a
//! [`Job`], so a GUI thread only ever polls [`Job::progress`] and
//! [`Job::try_take`] once per frame.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::thread::JoinHandle;

use parking_lot::Mutex;

use crate::{Error, Result};

struct Shared<T> {
    done: AtomicBool,
    result: Mutex<Option<Result<T>>>,
}

/// Progress/cancel handle passed to the work running inside a [`Job`].
#[derive(Clone)]
pub struct JobContext {
    progress: Option<Arc<AtomicU32>>,
    cancel: Option<Arc<AtomicBool>>,
    stage: Option<Arc<Mutex<String>>>,
    /// Maps local 0..=1 progress into a sub-range of the job's progress.
    range: (f32, f32),
}

impl JobContext {
    /// A context that reports nowhere and is never cancelled (for blocking calls).
    pub fn detached() -> Self {
        Self {
            progress: None,
            cancel: None,
            stage: None,
            range: (0.0, 1.0),
        }
    }

    /// A child context whose 0..=1 progress maps to `[a, b]` of this one.
    pub fn sub(&self, a: f32, b: f32) -> Self {
        let (lo, hi) = self.range;
        let w = hi - lo;
        Self {
            range: (lo + a * w, lo + b * w),
            ..self.clone()
        }
    }

    pub fn set_progress(&self, p: f32) {
        if let Some(a) = &self.progress {
            let (lo, hi) = self.range;
            let v = lo + p.clamp(0.0, 1.0) * (hi - lo);
            a.store(v.to_bits(), Ordering::Relaxed);
        }
    }

    pub fn set_stage(&self, s: &str) {
        if let Some(st) = &self.stage {
            let mut g = st.lock();
            g.clear();
            g.push_str(s);
        }
    }

    pub fn is_cancelled(&self) -> bool {
        self.cancel
            .as_ref()
            .is_some_and(|c| c.load(Ordering::Relaxed))
    }

    /// `Err(Error::Cancelled)` once the job has been cancelled.
    pub fn check_cancel(&self) -> Result<()> {
        if self.is_cancelled() {
            Err(Error::Cancelled)
        } else {
            Ok(())
        }
    }
}

/// A unit of work running on its own thread. Dropping the handle cancels it.
pub struct Job<T> {
    shared: Arc<Shared<T>>,
    progress: Arc<AtomicU32>,
    cancel: Arc<AtomicBool>,
    stage: Arc<Mutex<String>>,
    handle: Option<JoinHandle<()>>,
}

impl<T: Send + 'static> Job<T> {
    /// Run `work` on a new named thread.
    pub fn spawn<F>(name: &str, work: F) -> Self
    where
        F: FnOnce(&JobContext) -> Result<T> + Send + 'static,
    {
        let progress = Arc::new(AtomicU32::new(0f32.to_bits()));
        let cancel = Arc::new(AtomicBool::new(false));
        let stage = Arc::new(Mutex::new(String::from("Starting")));
        let shared = Arc::new(Shared {
            done: AtomicBool::new(false),
            result: Mutex::new(None),
        });
        let ctx = JobContext {
            progress: Some(progress.clone()),
            cancel: Some(cancel.clone()),
            stage: Some(stage.clone()),
            range: (0.0, 1.0),
        };
        let sh = shared.clone();
        let handle = std::thread::Builder::new()
            .name(name.to_string())
            .spawn(move || {
                let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| work(&ctx)))
                    .unwrap_or_else(|_| Err(Error::Other("worker thread panicked".into())));
                if r.is_ok() {
                    ctx.set_progress(1.0);
                }
                *sh.result.lock() = Some(r);
                sh.done.store(true, Ordering::Release);
            })
            .expect("failed to spawn job thread");
        Self {
            shared,
            progress,
            cancel,
            stage,
            handle: Some(handle),
        }
    }

    /// 0.0..=1.0.
    pub fn progress(&self) -> f32 {
        f32::from_bits(self.progress.load(Ordering::Relaxed))
    }

    /// Human-readable description of the current step.
    pub fn stage(&self) -> String {
        self.stage.lock().clone()
    }

    /// Ask the work to stop; it finishes with [`Error::Cancelled`] soon after.
    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::Relaxed);
    }

    pub fn is_finished(&self) -> bool {
        self.shared.done.load(Ordering::Acquire)
    }

    /// Take the result once finished (returns `None` while running and after
    /// the result has been taken).
    pub fn try_take(&mut self) -> Option<Result<T>> {
        if !self.is_finished() {
            return None;
        }
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
        self.shared.result.lock().take()
    }

    /// Block until finished and return the result.
    pub fn wait(mut self) -> Result<T> {
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
        self.shared
            .result
            .lock()
            .take()
            .unwrap_or(Err(Error::Cancelled))
    }
}

impl<T> Drop for Job<T> {
    fn drop(&mut self) {
        // Detach; the worker notices the flag and exits on its own.
        self.cancel.store(true, Ordering::Relaxed);
    }
}
