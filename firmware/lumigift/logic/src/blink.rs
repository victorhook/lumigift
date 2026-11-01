//! `Blink` command timing: `count` periods of `period` ms, on for the first
//! half of each period.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Blink {
    start_ms: u32,
    period_ms: u32,
    count: u8,
}

impl Blink {
    /// `None` for `count == 0` (which cancels a running blink).
    pub fn new(now_ms: u32, count: u8, period_10ms: u8) -> Option<Self> {
        (count > 0).then(|| Self { start_ms: now_ms, period_ms: (period_10ms as u32 * 10).max(20), count })
    }

    /// `Some(on)` while running, `None` once finished.
    pub fn phase(&self, now_ms: u32) -> Option<bool> {
        let t = now_ms.wrapping_sub(self.start_ms);
        if t >= self.period_ms * self.count as u32 {
            return None;
        }
        Some(t % self.period_ms < self.period_ms / 2)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blink_pattern() {
        assert!(Blink::new(0, 0, 10).is_none());
        let b = Blink::new(1000, 2, 20).unwrap(); // 200 ms period
        assert_eq!(b.phase(1000), Some(true));
        assert_eq!(b.phase(1099), Some(true));
        assert_eq!(b.phase(1100), Some(false));
        assert_eq!(b.phase(1200), Some(true));
        assert_eq!(b.phase(1399), Some(false));
        assert_eq!(b.phase(1400), None);
    }
}
