//! Raw radio packet ring of the bootloader.
//!
//! While the flash is busy (page erase 22-40 ms, double word ~85 µs) the
//! bootloader runs a RAM-resident loop that keeps pulling packets out of the
//! nRF's 3-deep RX FIFO into two of these rings (unicast and broadcast), so
//! reception overlaps erasing and programming. The main loop pops and
//! handles them when the flash is idle.
//!
//! Everything the drain loop touches is `#[inline(always)]` and free of
//! division, `memcpy` and panics, so it compiles into the RAM function
//! without calls back into flash (checked by `cargo xtask size`).

/// Bytes per slot (the largest nRF24 payload).
pub const SLOT: usize = 32;

pub struct RxRing<const N: usize> {
    buf: [[u8; SLOT]; N],
    lens: [u8; N],
    head: usize,
    len: usize,
    /// Packets dropped because the ring was full.
    pub dropped: u16,
}

impl<const N: usize> Default for RxRing<N> {
    fn default() -> Self {
        Self::new()
    }
}

impl<const N: usize> RxRing<N> {
    pub const fn new() -> Self {
        Self { buf: [[0; SLOT]; N], lens: [0; N], head: 0, len: 0, dropped: 0 }
    }

    #[inline(always)]
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    #[inline(always)]
    pub fn is_full(&self) -> bool {
        self.len >= N
    }

    #[inline(always)]
    fn wrap(i: usize) -> usize {
        // i < 2 * N always holds; this avoids a division in the RAM loop, and
        // the final clamp lets the compiler drop the bounds-check panic path.
        let j = if i >= N { i - N } else { i };
        if j < N { j } else { 0 }
    }

    /// Slot the next packet goes to. Only valid while `!is_full()`; the
    /// packet becomes visible with [`commit`](Self::commit).
    #[inline(always)]
    pub fn slot(&mut self) -> &mut [u8; SLOT] {
        let i = Self::wrap(self.head + self.len);
        &mut self.buf[i]
    }

    /// Publish the slot returned by [`slot`](Self::slot) with `len` bytes.
    #[inline(always)]
    pub fn commit(&mut self, len: u8) {
        if self.len < N {
            let i = Self::wrap(self.head + self.len);
            self.lens[i] = len;
            self.len += 1;
        }
    }

    /// Copy `data` (at most [`SLOT`] bytes) in. False (and counted) when full.
    pub fn push(&mut self, data: &[u8]) -> bool {
        if self.is_full() || data.len() > SLOT {
            self.dropped = self.dropped.wrapping_add(1);
            return false;
        }
        self.slot()[..data.len()].copy_from_slice(data);
        self.commit(data.len() as u8);
        true
    }

    /// Remove the oldest packet into `out`; returns its length.
    pub fn pop_into(&mut self, out: &mut [u8; SLOT]) -> Option<usize> {
        if self.len == 0 {
            return None;
        }
        let n = self.lens[self.head] as usize;
        *out = self.buf[self.head];
        self.head = Self::wrap(self.head + 1);
        self.len -= 1;
        Some(n)
    }

    pub fn clear(&mut self) {
        self.head = 0;
        self.len = 0;
    }

    /// Oldest to newest.
    pub fn iter(&self) -> impl Iterator<Item = &[u8]> + '_ {
        (0..self.len).map(move |k| {
            let i = Self::wrap(self.head + k);
            &self.buf[i][..self.lens[i] as usize]
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fifo_wraps_and_drops_when_full() {
        let mut r: RxRing<3> = RxRing::new();
        assert!(r.iter().next().is_none());
        for round in 0..5u8 {
            assert!(r.push(&[round, 1]));
            assert!(r.push(&[round, 2, 2]));
            assert!(r.push(&[round]));
            assert!(r.is_full());
            assert!(!r.push(&[9]));
            assert_eq!(r.iter().map(|p| p.len()).collect::<Vec<_>>(), vec![2, 3, 1]);
            let mut out = [0u8; SLOT];
            assert_eq!(r.pop_into(&mut out), Some(2));
            assert_eq!(&out[..2], &[round, 1]);
            assert_eq!(r.iter().next(), Some(&[round, 2, 2][..]));
            r.pop_into(&mut out);
            r.pop_into(&mut out);
            assert!(r.is_empty());
        }
        assert_eq!(r.dropped, 5);
        // Raw slot interface as used by the drain loop.
        r.slot()[..2].copy_from_slice(&[7, 7]);
        r.commit(2);
        assert_eq!(r.iter().next(), Some(&[7, 7][..]));
        assert!(!r.push(&[0; 33]), "oversized");
    }
}
