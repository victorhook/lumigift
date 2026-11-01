//! Show-stream statistics from the broadcast sequence numbers.

/// Gaps are only counted when frames arrive at most this far apart; after a
/// longer silence the 8-bit sequence number may have wrapped.
pub const MAX_GAP_MS: u32 = 2000;

#[derive(Debug, Default, Clone, Copy)]
pub struct SeqTracker {
    last: Option<(u8, u32)>,
    /// Show broadcasts received (wrapping).
    pub rx: u16,
    /// Show broadcasts missed, from sequence gaps (wrapping).
    pub lost: u16,
}

impl SeqTracker {
    pub const fn new() -> Self {
        Self { last: None, rx: 0, lost: 0 }
    }

    /// Account one show frame. Returns false for a duplicate (same seq as
    /// the previous frame), which the caller should drop.
    pub fn on_frame(&mut self, seq: u8, now_ms: u32) -> bool {
        if let Some((last, t)) = self.last {
            let gap = seq.wrapping_sub(last);
            if gap == 0 {
                return false;
            }
            if now_ms.wrapping_sub(t) <= MAX_GAP_MS {
                self.lost = self.lost.wrapping_add(gap as u16 - 1);
            }
        }
        self.last = Some((seq, now_ms));
        self.rx = self.rx.wrapping_add(1);
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counts_gaps_across_wrap() {
        let mut s = SeqTracker::new();
        let mut t = 0;
        for seq in [250u8, 251, 253, 254, 255, 0, 3] {
            assert!(s.on_frame(seq, t));
            t += 10;
        }
        assert_eq!((s.rx, s.lost), (7, 3));
        assert!(!s.on_frame(3, t), "duplicate");
        assert_eq!(s.rx, 7);
        // Long silence: gap not counted.
        assert!(s.on_frame(100, t + 5000));
        assert_eq!(s.lost, 3);
    }
}
