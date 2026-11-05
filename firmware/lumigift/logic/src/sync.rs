//! Lock the local µs clock to the bridge clock stamped into show frames.
//!
//! Each sample is `(remote, local)`: the bridge timestamp (plus a constant
//! air-time compensation) and the local µs time the packet was seen. The
//! local reading is late by a non-negative, jittery delay (IRQ latency,
//! a busy main loop, a flash write), so single samples are biased and
//! occasionally far off.
//!
//! Min-delay envelope filter: the model is `synced = anchor_remote +
//! (local - anchor_local) * (1 + rate)`. Over a window of [`WINDOW`] samples
//! the one with the largest innovation (= the least delayed) is kept. At the
//! end of the window the model is re-anchored towards it (fully while
//! acquiring, then half way), and the innovation divided by the time since
//! the previous anchor corrects `rate` (gain 1/2 while acquiring, then 1/8).
//! The HSI48 is only ±1-2 % accurate (100-200 µs per 10 ms frame), so the
//! rate term matters. Late samples never win a window, so they cannot drag
//! the clock. Simulated with ±2.5 % drift, 0-200 µs jitter and 3 ms stalls
//! the error stays below ~100 µs (see the tests).

/// Rate fixed point: synced advances `dt * (1 + rate / 2^24)`.
const RATE_SHIFT: u32 = 24;
/// Clamp the frequency correction to ±4 %.
const MAX_RATE: i64 = (1 << RATE_SHIFT) / 25;
/// Samples per anchor window (80 ms at 100 frames/s)...
pub const WINDOW: u8 = 8;
/// ...or this much local time, whichever comes first. With the host's 10 Hz
/// idle beacons a count-only window spanned 800 ms: 8+ ms of drift on a gift
/// whose HSI48 is 1 % off, beyond `STEP_US`, so it re-seeded forever and
/// never reported synced (6 of 10 gifts on hardware).
pub const WINDOW_SPAN_US: u32 = 80_000;
/// An innovation above this (bridge restart, a delayed seed) re-seeds.
pub const STEP_US: i32 = 5000;
/// Once locked, a sample this much later than the model is a delayed read.
const LATE_US: i32 = 1000;
/// This many consecutive late samples re-seed.
const MAX_LATE: u8 = 4;
/// Without samples for this long the clock is "not synced" and re-seeds.
pub const LOCK_TIMEOUT_US: u32 = 1_000_000;
/// Anchors using the fast rate gain after a seed.
const ACQUIRE_ANCHORS: u8 = 8;
/// Anchors needed after a seed before reporting synced.
const MIN_ANCHORS: u8 = 3;

#[derive(Debug, Clone, Copy, Default)]
pub struct ClockSync {
    have: bool,
    base_local: u32,
    base_remote: u32,
    rate: i32,
    last_local: u32,
    win_n: u8,
    win_start: u32,
    best_e: i32,
    best_local: u32,
    best_remote: u32,
    late: u8,
    anchors: u8,
    err_avg: u32,
}

impl ClockSync {
    pub const fn new() -> Self {
        Self {
            have: false,
            base_local: 0,
            base_remote: 0,
            rate: 0,
            last_local: 0,
            win_n: 0,
            win_start: 0,
            best_e: 0,
            best_local: 0,
            best_remote: 0,
            late: 0,
            anchors: 0,
            err_avg: 0,
        }
    }

    /// Synced (bridge) µs time at local time `local`. Before the first
    /// sample this is just the local clock.
    pub fn now(&self, local: u32) -> u32 {
        if !self.have {
            return local;
        }
        let dt = local.wrapping_sub(self.base_local);
        let corr = ((dt as i64 * self.rate as i64) >> RATE_SHIFT) as i32;
        self.base_remote.wrapping_add(dt).wrapping_add(corr as u32)
    }

    fn seed(&mut self, remote: u32, local: u32) {
        self.have = true;
        self.base_local = local;
        self.base_remote = remote;
        self.win_n = 0;
        self.late = 0;
        self.anchors = 0;
        self.err_avg = 0;
    }

    /// Feed one sample.
    pub fn sample(&mut self, remote: u32, local: u32) {
        let gap = local.wrapping_sub(self.last_local);
        self.last_local = local;
        if !self.have || gap > LOCK_TIMEOUT_US {
            self.seed(remote, local);
            return;
        }
        // Close a window that has run its time first, so this sample is
        // judged against an anchor at most ~one window old.
        if self.win_n > 0 && local.wrapping_sub(self.win_start) >= WINDOW_SPAN_US {
            self.close_window();
        }
        let e = remote.wrapping_sub(self.now(local)) as i32;
        if e > STEP_US {
            self.seed(remote, local);
            return;
        }
        // Far later than the model: a delayed read (busy main loop, flash
        // write), not drift. Once locked "far" is 1 ms: with one sample per
        // window (10 Hz idle beacons) nothing else would outvote it.
        let late_limit = if self.anchors >= ACQUIRE_ANCHORS { LATE_US } else { STEP_US };
        if e < -late_limit {
            self.late += 1;
            if self.late >= MAX_LATE {
                self.seed(remote, local);
            }
            return;
        }
        self.late = 0;
        if self.win_n == 0 {
            self.win_start = local;
        }
        if self.win_n == 0 || e > self.best_e {
            self.best_e = e;
            self.best_local = local;
            self.best_remote = remote;
        }
        self.win_n += 1;
        if self.win_n >= WINDOW {
            self.close_window();
        }
    }

    /// Re-anchor on the least-delayed sample of the window and update the rate.
    fn close_window(&mut self) {
        self.win_n = 0;
        let span = self.best_local.wrapping_sub(self.base_local);
        if self.anchors > 0 && (20_000..=2 * LOCK_TIMEOUT_US).contains(&span) {
            // Correct half of the measured frequency error per window while
            // acquiring, then 1/8 to average out the anchor jitter.
            let gain_shift = if self.anchors < ACQUIRE_ANCHORS { 1 } else { 3 };
            let d = ((self.best_e as i64) << RATE_SHIFT) / ((span as i64) << gain_shift);
            self.rate = (self.rate as i64 + d).clamp(-MAX_RATE, MAX_RATE) as i32;
        }
        // Move the phase half way to the least-delayed sample (the other
        // half of a real offset follows in the next windows).
        let corr = if self.anchors < ACQUIRE_ANCHORS { self.best_e } else { self.best_e / 2 };
        self.base_remote = self.best_remote.wrapping_sub((self.best_e - corr) as u32);
        self.base_local = self.best_local;
        self.err_avg = (self.err_avg * 3 + self.best_e.unsigned_abs()) / 4;
        self.anchors = self.anchors.saturating_add(1);
    }

    /// True once locked and fed recently.
    pub fn synced(&self, local: u32) -> bool {
        self.have && self.anchors >= MIN_ANCHORS && local.wrapping_sub(self.last_local) < LOCK_TIMEOUT_US
    }

    /// Smoothed correction applied at each anchor, µs (saturating): the
    /// clock's error just before it was corrected. For `Status.sync_err_us`.
    pub fn error_us(&self) -> u16 {
        self.err_avg.min(u16::MAX as u32) as u16
    }

    /// Frequency correction in ppm (diagnostics).
    pub fn rate_ppm(&self) -> i32 {
        ((self.rate as i64 * 1_000_000) >> RATE_SHIFT) as i32
    }
}

/// nRF24 air time of a 1 Mbps packet with `len` payload bytes (1 B preamble,
/// 5 B address, 9-bit PCF, 2 B CRC) plus the 130 µs TX settling, which
/// elapse between the bridge's stamp and RX_DR on the gift.
pub const fn air_time_us(len: usize) -> u32 {
    130 + ((1 + 5 + 2 + len as u32) * 8 + 9)
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Lcg(u32);
    impl Lcg {
        fn next(&mut self) -> u32 {
            self.0 = self.0.wrapping_mul(1664525).wrapping_add(1013904223);
            self.0 >> 8
        }
    }

    /// A gift whose clock runs `ppm` fast with an arbitrary offset, frames
    /// every 10 ms, read delays of 0..max_delay µs plus a 3 ms stall every
    /// 37th frame (including the very first one). Returns the worst
    /// |synced - true| over the last second, probed between frames.
    fn simulate(ppm: i64, max_delay: u32) -> (u32, ClockSync) {
        simulate_every(ppm, max_delay, 10_000, 1000).0
    }

    /// Like [`simulate`] with samples every `interval` µs (`n` of them);
    /// also returns the fraction of the last half that reported synced.
    fn simulate_every(ppm: i64, max_delay: u32, interval: u32, n: u32) -> ((u32, ClockSync), f32) {
        let start_remote = 4_000_000_000u32; // wraps during the run
        let offset = 123_456_789u32;
        let true_local = |r: u32| -> u32 {
            let el = r.wrapping_sub(start_remote) as i64;
            offset.wrapping_add((el + el * ppm / 1_000_000) as u32)
        };
        let mut s = ClockSync::new();
        let mut rng = Lcg(42);
        let mut worst = 0u32;
        let mut synced = 0u32;
        for k in 0..n {
            let remote = start_remote.wrapping_add(k * interval);
            let mut delay = rng.next() % max_delay;
            if k % 37 == 0 {
                delay += 3000;
            }
            let local = true_local(remote.wrapping_add(delay));
            s.sample(remote, local);
            if k >= n / 2 && s.synced(local) {
                synced += 1;
            }
            if k > n * 9 / 10 {
                let probe = remote.wrapping_add(interval / 2);
                let err = s.now(true_local(probe)).wrapping_sub(probe) as i32;
                worst = worst.max(err.unsigned_abs());
            }
        }
        ((worst, s), synced as f32 / (n - n / 2) as f32)
    }

    /// Idle: the host's `Sync` beacons come at 10 Hz. A gift whose HSI48 is
    /// ±1-2 % off must still lock (on hardware 6 of 10 gifts never did: an
    /// 8-sample window spanned 800 ms = 8+ ms of drift, beyond `STEP_US`, so
    /// they re-seeded forever).
    #[test]
    fn locks_on_idle_beacons() {
        for &ppm in &[0i64, 6_000, -6_000, 10_000, -10_000, 20_000, -20_000] {
            let ((worst, s), synced) = simulate_every(ppm, 200, 100_000, 200);
            assert!(synced > 0.99, "ppm {ppm}: synced {:.0} % of the time: {s:?}", synced * 100.0);
            assert!(worst < 400, "ppm {ppm}: worst error {worst} µs");
        }
    }

    #[test]
    fn locks_with_drift_and_jitter() {
        for &ppm in &[0i64, 10_000, -10_000, 20_000, -25_000] {
            let (worst, s) = simulate(ppm, 200);
            assert!(s.synced(s.last_local), "ppm {ppm}: {s:?}");
            assert!(worst < 120, "ppm {ppm}: worst error {worst} µs");
            let est = s.rate_ppm() as i64;
            let exact = -ppm * 1_000_000 / (1_000_000 + ppm);
            assert!((est - exact).abs() < 300, "ppm {ppm}: rate estimate {est}, exact {exact}");
            assert!(s.error_us() < 120, "ppm {ppm}: reported error {}", s.error_us());
        }
    }

    #[test]
    fn reseeds_on_step_and_reports_unsynced_when_idle() {
        let mut s = ClockSync::new();
        assert_eq!(s.now(77), 77);
        for k in 0..40u32 {
            s.sample(1_000_000 + k * 10_000, 50 + k * 10_000);
        }
        let t = 50 + 39 * 10_000;
        assert!(s.synced(t));
        assert!(!s.synced(t + LOCK_TIMEOUT_US + 1));
        // Bridge restarted (time jumped back): re-seeds after a few samples.
        for k in 0..MAX_LATE as u32 {
            s.sample(10 + k * 10_000, t + (k + 1) * 10_000);
        }
        assert_eq!(s.now(t + MAX_LATE as u32 * 10_000), 10 + (MAX_LATE as u32 - 1) * 10_000);
        assert!(!s.synced(t + MAX_LATE as u32 * 10_000));
    }

    #[test]
    fn air_time() {
        assert_eq!(air_time_us(0), 130 + 73);
        assert_eq!(air_time_us(22), 130 + 249);
    }
}
