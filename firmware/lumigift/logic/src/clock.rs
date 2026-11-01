//! Tick arithmetic for the 16-bit µs timer extended by an overflow count.

/// Milliseconds for `overflows * 65536 + counter` µs, wrapping at 2^32 ms,
/// using only 32-bit divisions (a 64-bit division costs ~1 KiB on the M0+).
pub fn ticks_to_ms(overflows: u32, counter: u32) -> u32 {
    let (q, r) = (overflows / 1000, overflows % 1000);
    q.wrapping_mul(65536).wrapping_add((r * 65536 + counter) / 1000)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_u64_math() {
        for &hi in &[0u32, 1, 999, 1000, 1001, 65_535, 1_000_000, 123_456_789, u32::MAX] {
            for &lo in &[0u32, 1, 999, 1000, 65_535] {
                let exact = ((((hi as u64) << 16) | lo as u64) / 1000) as u32;
                assert_eq!(ticks_to_ms(hi, lo), exact, "hi {hi} lo {lo}");
            }
        }
    }
}
