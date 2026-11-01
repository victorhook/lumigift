//! Bridge clock ↔ host `Instant` mapping, built from Ping/Pong exchanges.

use std::collections::{HashMap, VecDeque};
use std::time::{Duration, Instant};

/// A linear map between the bridge's µs clock (unwrapped to `u64`) and the
/// host's monotonic clock. `Copy`, so the audio engine can keep its own copy;
/// get a fresh one from [`Link::clock`](crate::Link::clock).
#[derive(Debug, Clone, Copy)]
pub struct ClockSync {
    epoch: Instant,
    /// Bridge µs at host time `epoch`.
    offset_us: f64,
    /// Bridge µs per host µs.
    rate: f64,
    synced: bool,
    rtt_us: u32,
}

impl ClockSync {
    pub(crate) fn unsynced(epoch: Instant) -> Self {
        Self {
            epoch,
            offset_us: 0.0,
            rate: 1.0,
            synced: false,
            rtt_us: 0,
        }
    }

    /// True once at least one Pong has been received.
    pub fn is_synced(&self) -> bool {
        self.synced
    }

    /// Round-trip time of the best recent ping (µs).
    pub fn rtt_us(&self) -> u32 {
        self.rtt_us
    }

    /// Estimated relative drift of the bridge crystal (ppm).
    pub fn drift_ppm(&self) -> f64 {
        (self.rate - 1.0) * 1e6
    }

    /// Current bridge time (unwrapped µs). The low 32 bits are what the
    /// bridge itself reports.
    pub fn bridge_now_us(&self) -> u64 {
        self.from_instant(Instant::now())
    }

    /// Bridge time (unwrapped µs) at a host instant.
    pub fn from_instant(&self, t: Instant) -> u64 {
        let host_us = signed_us(self.epoch, t);
        (self.offset_us + host_us * self.rate).max(0.0) as u64
    }

    /// Host instant at a bridge time (unwrapped µs).
    pub fn to_instant(&self, bridge_us: u64) -> Instant {
        let host_us = (bridge_us as f64 - self.offset_us) / self.rate;
        if host_us >= 0.0 {
            self.epoch + Duration::from_micros(host_us as u64)
        } else {
            self.epoch
                .checked_sub(Duration::from_micros((-host_us) as u64))
                .unwrap_or(self.epoch)
        }
    }

    /// Unwrap a raw 32-bit bridge timestamp (e.g. from `TxDone`) to the
    /// `u64` timeline, choosing the value nearest to now.
    pub fn unwrap(&self, raw: u32) -> u64 {
        let now = self.bridge_now_us();
        let diff = raw.wrapping_sub(now as u32) as i32 as i64;
        (now as i64 + diff).max(0) as u64
    }

    /// Host instant of a raw 32-bit bridge timestamp.
    pub fn raw_to_instant(&self, raw: u32) -> Instant {
        self.to_instant(self.unwrap(raw))
    }
}

fn signed_us(epoch: Instant, t: Instant) -> f64 {
    if t >= epoch {
        t.duration_since(epoch).as_secs_f64() * 1e6
    } else {
        -(epoch.duration_since(t).as_secs_f64() * 1e6)
    }
}

/// Ping bookkeeping + fit (internal to the scheduler).
pub(crate) struct ClockEstimator {
    epoch: Instant,
    outstanding: HashMap<u32, Instant>,
    next_seq: u32,
    /// (host µs at the RTT midpoint, unwrapped bridge µs, rtt µs)
    samples: VecDeque<(f64, u64, u32)>,
    last_raw: Option<u32>,
    unwrapped: u64,
    pub current: ClockSync,
}

const MAX_SAMPLES: usize = 64;

impl ClockEstimator {
    pub fn new(epoch: Instant) -> Self {
        Self {
            epoch,
            outstanding: HashMap::new(),
            next_seq: 1,
            samples: VecDeque::new(),
            last_raw: None,
            unwrapped: 0,
            current: ClockSync::unsynced(epoch),
        }
    }

    /// Call when the bridge was (re)connected: its clock may have restarted.
    pub fn reset(&mut self) {
        self.outstanding.clear();
        self.samples.clear();
        self.last_raw = None;
        self.current.synced = false;
    }

    pub fn ping_sent(&mut self, now: Instant) -> u32 {
        let s = self.next_seq;
        self.next_seq = self.next_seq.wrapping_add(1);
        self.outstanding.insert(s, now);
        if self.outstanding.len() > 32 {
            let oldest = *self.outstanding.iter().min_by_key(|(_, t)| **t).unwrap().0;
            self.outstanding.remove(&oldest);
        }
        s
    }

    pub fn pong(&mut self, seq: u32, raw: u32, now: Instant) {
        let Some(sent) = self.outstanding.remove(&seq) else {
            return;
        };
        let rtt = now.duration_since(sent);
        let rtt_us = rtt.as_micros().min(u32::MAX as u128) as u32;
        let bridge = match self.last_raw {
            None => raw as u64,
            Some(last) => {
                let d = raw.wrapping_sub(last) as i32 as i64;
                (self.unwrapped as i64 + d).max(0) as u64
            }
        };
        self.last_raw = Some(raw);
        self.unwrapped = bridge;
        let mid = sent + rtt / 2;
        self.samples
            .push_back((signed_us(self.epoch, mid), bridge, rtt_us));
        while self.samples.len() > MAX_SAMPLES {
            self.samples.pop_front();
        }
        self.fit();
    }

    fn fit(&mut self) {
        let min_rtt = self.samples.iter().map(|s| s.2).min().unwrap_or(0);
        let good: Vec<(f64, f64)> = self
            .samples
            .iter()
            .filter(|s| s.2 <= min_rtt + 300 + min_rtt / 2)
            .map(|s| (s.0, s.1 as f64))
            .collect();
        let n = good.len() as f64;
        let mx = good.iter().map(|g| g.0).sum::<f64>() / n;
        let my = good.iter().map(|g| g.1).sum::<f64>() / n;
        let span = good.iter().map(|g| g.0).fold(f64::MIN, f64::max)
            - good.iter().map(|g| g.0).fold(f64::MAX, f64::min);
        let mut rate = 1.0;
        if good.len() >= 4 && span > 2e6 {
            let sxx: f64 = good.iter().map(|g| (g.0 - mx) * (g.0 - mx)).sum();
            let sxy: f64 = good.iter().map(|g| (g.0 - mx) * (g.1 - my)).sum();
            if sxx > 0.0 {
                rate = (sxy / sxx).clamp(1.0 - 500e-6, 1.0 + 500e-6);
            }
        }
        self.current = ClockSync {
            epoch: self.epoch,
            offset_us: my - mx * rate,
            rate,
            synced: true,
            rtt_us: min_rtt,
        };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unwrap_across_wrap() {
        let epoch = Instant::now();
        let mut e = ClockEstimator::new(epoch);
        let base = u32::MAX - 1000;
        let s = e.ping_sent(epoch);
        e.pong(s, base, epoch + Duration::from_micros(200));
        let s = e.ping_sent(epoch + Duration::from_millis(10));
        e.pong(
            s,
            base.wrapping_add(10_000),
            epoch + Duration::from_micros(10_200),
        );
        let c = e.current;
        let at = c.from_instant(epoch + Duration::from_millis(10));
        assert!(
            (at as i64 - (base as i64 + 10_000 - 100)).abs() < 50,
            "{at}"
        );
        assert_eq!(
            c.unwrap(base.wrapping_add(10_000)) as i64,
            base as i64 + 10_000
        );
        let back = c.to_instant(at);
        let d = back.duration_since(epoch).as_micros() as i64;
        assert!((d - 10_000).abs() < 5);
    }
}
