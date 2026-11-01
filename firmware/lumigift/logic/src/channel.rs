//! Which radio channel the bootloader listens on.
//!
//! The configured channel lives in the config page, which the app writes. A
//! buggy app could store a channel the host is not on, so the bootloader
//! never relies on it alone:
//!
//! * The `BootHold` window after reset always listens on
//!   [`DEFAULT_CHANNEL`], so a host broadcasting `BootHold` there while the
//!   gift is power-cycled always catches it.
//! * In recovery mode the bootloader starts on its start channel and, while
//!   it hears nothing, alternates between the configured channel and the
//!   default channel every [`DWELL_MS`]. Any packet received (only
//!   Lumigift-addressed packets with a valid CRC get through the nRF) makes
//!   the current channel sticky; after [`SILENCE_MS`] without traffic it
//!   resumes alternating (the host may have moved, or come back on the
//!   other channel).

use lumigift_proto::DEFAULT_CHANNEL;

/// Time on one channel before switching while nothing is heard.
pub const DWELL_MS: u32 = 1000;
/// Silence after which a sticky channel is given up.
pub const SILENCE_MS: u32 = 3000;

/// Channel of the `BootHold` window, whatever is configured.
pub const fn hold_window_channel() -> u8 {
    DEFAULT_CHANNEL
}

#[derive(Debug, Clone, Copy)]
pub struct ChannelPlan {
    stored: u8,
    current: u8,
    since: u32,
    last_rx: Option<u32>,
}

impl ChannelPlan {
    /// `stored` is the configured channel (anything above 125 counts as
    /// invalid and means the default), `start` the channel to begin on.
    pub fn new(stored: u8, start: u8, now: u32) -> Self {
        let stored = if stored > 125 { DEFAULT_CHANNEL } else { stored };
        let start = if start == stored { stored } else { DEFAULT_CHANNEL };
        Self { stored, current: start, since: now, last_rx: None }
    }

    pub fn current(&self) -> u8 {
        self.current
    }

    /// A packet arrived on the current channel.
    pub fn on_rx(&mut self, now: u32) {
        self.last_rx = Some(now);
    }

    /// Call periodically; returns the channel to switch to, if any.
    pub fn tick(&mut self, now: u32) -> Option<u8> {
        if self.stored == DEFAULT_CHANNEL && self.current == DEFAULT_CHANNEL {
            return None;
        }
        if self.last_rx.is_some_and(|t| now.wrapping_sub(t) < SILENCE_MS) {
            return None;
        }
        if now.wrapping_sub(self.since) < DWELL_MS {
            return None;
        }
        self.current = if self.current == self.stored { DEFAULT_CHANNEL } else { self.stored };
        self.since = now;
        self.last_rx = None;
        Some(self.current)
    }

    /// Milliseconds until [`tick`](Self::tick) may act (for event-driven callers).
    pub fn next_deadline(&self, now: u32) -> Option<u32> {
        if self.stored == DEFAULT_CHANNEL && self.current == DEFAULT_CHANNEL {
            return None;
        }
        let to_dwell = self.since.wrapping_add(DWELL_MS).wrapping_sub(now) as i32;
        let to_quiet = self.last_rx.map_or(0, |t| t.wrapping_add(SILENCE_MS).wrapping_sub(now) as i32);
        Some(to_dwell.max(to_quiet).max(0) as u32)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_channel_never_switches() {
        let mut p = ChannelPlan::new(DEFAULT_CHANNEL, DEFAULT_CHANNEL, 0);
        for t in (0..20_000).step_by(100) {
            assert_eq!(p.tick(t), None);
        }
        assert_eq!(p.current(), DEFAULT_CHANNEL);
        assert_eq!(hold_window_channel(), DEFAULT_CHANNEL);
    }

    #[test]
    fn wrong_stored_channel_alternates_until_heard() {
        let mut p = ChannelPlan::new(90, 90, 0);
        let mut seen = Vec::new();
        for t in (0..5000).step_by(50) {
            if let Some(c) = p.tick(t) {
                seen.push((t, c));
            }
        }
        assert_eq!(seen, vec![(1000, DEFAULT_CHANNEL), (2000, 90), (3000, DEFAULT_CHANNEL), (4000, 90)]);
        // Host traffic on 90 at t=4100: sticks to 90 while traffic continues.
        p.on_rx(4100);
        for t in (4100..10_000).step_by(50) {
            if t % 1000 == 0 {
                p.on_rx(t);
            }
            assert_eq!(p.tick(t), None, "t={t}");
        }
        // Host silent for 3 s after its last packet (t=9000): alternation resumes.
        assert_eq!(p.tick(11_999), None);
        assert_eq!(p.tick(12_000), Some(DEFAULT_CHANNEL));
    }

    #[test]
    fn starts_on_default_after_a_boot_hold_and_rejects_invalid_channels() {
        let p = ChannelPlan::new(90, DEFAULT_CHANNEL, 0);
        assert_eq!(p.current(), DEFAULT_CHANNEL);
        let mut p = ChannelPlan::new(200, 200, 0);
        assert_eq!(p.current(), DEFAULT_CHANNEL);
        assert_eq!(p.tick(5000), None, "invalid stored channel = default only");
    }

    #[test]
    fn deadline_matches_tick() {
        let mut p = ChannelPlan::new(90, 90, 0);
        assert_eq!(p.next_deadline(0), Some(1000));
        p.on_rx(500);
        assert_eq!(p.next_deadline(600), Some(2900));
        assert_eq!(p.tick(3499), None);
        assert!(p.tick(3500).is_some());
    }
}
