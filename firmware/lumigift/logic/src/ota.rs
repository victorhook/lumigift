//! OTA receiver state machine of the bootloader, independent of the flash
//! hardware (see [`RegionFlash`]).
//!
//! Chunk `i` holds the image bytes `[i * 24, i * 24 + 24)` relative to the
//! app region base. 24 is a multiple of the 8-byte flash double word and the
//! region base is page aligned, so every double word belongs to exactly one
//! chunk. A received-chunk bitmap guarantees each chunk (and therefore each
//! double word) is programmed at most once per erase, which the hardware
//! requires (a non-erased double word cannot be reprogrammed).
//!
//! Erasing: `OtaBegin` only records the session. The pages the image covers
//! (`ceil(image_len / 2048)`, not the whole region) are erased strictly in
//! order, page 0 first: either ahead of time while the receiver is idle
//! ([`Ota::next_erase`]) or on demand when a chunk for a not yet erased page
//! comes up ([`ChunkNeed::Erase`]). Page 0 holds the old image header, so the
//! old image stops being bootable with the very first erase, and a partly
//! written new image never validates (header CRC + app CRC + image CRC).
//!
//! When the last chunk is stored the image is verified right away
//! (`Verified` or `Failed`), without waiting for `OtaCommit`.

use lumigift_proto::image::ImageHeader;
use lumigift_proto::layout::{
    APP_REGION_SIZE, APP_VECTORS, BOOT_SHARED_ADDR, IMAGE_HEADER_SIZE, MAX_OTA_CHUNKS, PAGE_SIZE, RAM_BASE, WRITE_SIZE,
};
use lumigift_proto::{OTA_BITMAP_BYTES, OTA_CHUNK, OTA_WINDOW_CHUNKS, OtaBegin, OtaBitmap, OtaChunk, OtaState};

const WORDS: usize = (MAX_OTA_CHUNKS as usize).div_ceil(32);

/// Number of chunks covering `image_len` bytes.
pub fn chunk_count(image_len: u32) -> u16 {
    image_len.div_ceil(OTA_CHUNK as u32) as u16
}

/// Number of app-region pages an image of `image_len` bytes occupies.
pub fn pages_for(image_len: u32) -> u32 {
    image_len.div_ceil(PAGE_SIZE)
}

/// Last page chunk `index` writes to. Bytes past `image_len` are padding and
/// never programmed, so they never require an erase.
pub fn chunk_last_page(index: u16, image_len: u32) -> u32 {
    let start = index as u32 * OTA_CHUNK as u32;
    let end = (start + OTA_CHUNK as u32).min(image_len).max(start + 1);
    (end - 1) / PAGE_SIZE
}

/// Bitmap of received chunks (108 bytes for the full region).
pub struct ChunkMap {
    bits: [u32; WORDS],
    len: u16,
    received: u16,
}

impl Default for ChunkMap {
    fn default() -> Self {
        Self::new()
    }
}

fn set(bits: &mut [u32; WORDS], i: u16) {
    bits[i as usize / 32] |= 1 << (i % 32);
}

fn get(bits: &[u32; WORDS], i: u16) -> bool {
    bits[i as usize / 32] & (1 << (i % 32)) != 0
}

impl ChunkMap {
    pub const fn new() -> Self {
        Self { bits: [0; WORDS], len: 0, received: 0 }
    }

    /// Clear and track `len` chunks (clamped to [`MAX_OTA_CHUNKS`]).
    pub fn reset(&mut self, len: u16) {
        self.bits = [0; WORDS];
        self.len = len.min(MAX_OTA_CHUNKS as u16);
        self.received = 0;
    }

    pub fn len(&self) -> u16 {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    pub fn received(&self) -> u16 {
        self.received
    }

    pub fn complete(&self) -> bool {
        self.received == self.len
    }

    pub fn contains(&self, i: u16) -> bool {
        i < self.len && get(&self.bits, i)
    }

    /// Mark chunk `i`; false if out of range or already present.
    pub fn insert(&mut self, i: u16) -> bool {
        if i >= self.len || self.contains(i) {
            return false;
        }
        set(&mut self.bits, i);
        self.received += 1;
        true
    }
}

/// Read access to the app region. Offsets are relative to
/// [`APP_REGION_BASE`](lumigift_proto::layout::APP_REGION_BASE).
pub trait RegionRead {
    fn read(&self, offset: u32, out: &mut [u8]);
    /// CRC-32/ISO-HDLC of `len` bytes at `offset`.
    fn crc32(&self, offset: u32, len: u32) -> u32;
}

/// The app region as seen by the OTA logic.
pub trait RegionFlash: RegionRead {
    /// Erase page `page` (0..[`APP_PAGES`](lumigift_proto::layout::APP_PAGES)) of the region.
    fn erase_page(&mut self, page: u32) -> bool;
    /// Program one double word at the 8-aligned `offset`.
    fn program(&mut self, offset: u32, dw: [u8; 8]) -> bool;
}

/// Check the image in the region: header (magic, version, CRC, bounds),
/// board revision, app CRC, and a sane vector table (initial SP inside RAM
/// below `BootShared`, Thumb reset vector inside the app).
pub fn validate_image(f: &dyn RegionRead, hw_rev: u16) -> Option<ImageHeader> {
    let mut b = [0u8; ImageHeader::ENCODED_LEN];
    f.read(0, &mut b);
    let h = ImageHeader::decode(&b)?;
    if h.hw_rev != hw_rev || h.app_len < 8 {
        return None;
    }
    let mut v = [0u8; 8];
    f.read(IMAGE_HEADER_SIZE, &mut v);
    let sp = u32::from_le_bytes([v[0], v[1], v[2], v[3]]);
    let reset = u32::from_le_bytes([v[4], v[5], v[6], v[7]]);
    let sp_ok = sp > RAM_BASE && sp <= BOOT_SHARED_ADDR && sp % 4 == 0;
    let reset_ok = reset & 1 == 1 && reset > APP_VECTORS && reset < APP_VECTORS + h.app_len;
    if !(sp_ok && reset_ok) {
        return None;
    }
    (f.crc32(IMAGE_HEADER_SIZE, h.app_len) == h.app_crc).then_some(h)
}

/// What a chunk needs before it can be programmed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChunkNeed {
    /// Not for us: other session, not receiving, out of range or a duplicate.
    Drop,
    /// Erase this page first (always the next unerased page, in order).
    Erase(u32),
    Ready,
}

pub struct Ota {
    hw_rev: u16,
    session: u8,
    image_len: u32,
    image_crc: u32,
    state: OtaState,
    pages: u8,
    erased: u8,
    map: ChunkMap,
    verified: Option<ImageHeader>,
}

impl Ota {
    pub const fn new(hw_rev: u16) -> Self {
        Self {
            hw_rev,
            session: 0,
            image_len: 0,
            image_crc: 0,
            state: OtaState::Idle,
            pages: 0,
            erased: 0,
            map: ChunkMap::new(),
            verified: None,
        }
    }

    /// Header of the image this session verified.
    pub fn verified_header(&self) -> Option<ImageHeader> {
        self.verified.filter(|_| self.state == OtaState::Verified)
    }

    pub fn state(&self) -> OtaState {
        self.state
    }

    pub fn session(&self) -> u8 {
        self.session
    }

    pub fn map(&self) -> &ChunkMap {
        &self.map
    }

    /// Pages erased so far in this session.
    pub fn pages_erased(&self) -> u32 {
        self.erased as u32
    }

    /// Start a session. A repeated `OtaBegin` of the running (or verified)
    /// session (same id, length and CRC) is ignored, so the host may send
    /// Begin several times; anything else restarts. Returns true when a new
    /// session started. Nothing is erased here (see the module docs).
    pub fn begin(&mut self, b: &OtaBegin) -> bool {
        if b.image_len <= IMAGE_HEADER_SIZE || b.image_len > APP_REGION_SIZE {
            return false;
        }
        let running = matches!(self.state, OtaState::Receiving | OtaState::Verified);
        if running && b.session == self.session && b.image_len == self.image_len && b.image_crc == self.image_crc {
            return false;
        }
        self.session = b.session;
        self.image_len = b.image_len;
        self.image_crc = b.image_crc;
        self.state = OtaState::Receiving;
        self.pages = pages_for(b.image_len) as u8;
        self.erased = 0;
        self.map.reset(chunk_count(b.image_len));
        true
    }

    /// Next page to erase ahead of time, if the session still has unerased pages.
    pub fn next_erase(&self) -> Option<u32> {
        (self.state == OtaState::Receiving && self.erased < self.pages).then_some(self.erased as u32)
    }

    /// Erase the next page ([`next_erase`](Self::next_erase)). A flash error
    /// fails the session. Returns false if there was nothing to erase.
    pub fn erase_next<F: RegionFlash>(&mut self, f: &mut F) -> bool {
        let Some(p) = self.next_erase() else { return false };
        if f.erase_page(p) {
            self.erased += 1;
        } else {
            self.state = OtaState::Failed;
        }
        true
    }

    pub fn chunk_need(&self, session: u8, index: u16) -> ChunkNeed {
        if self.state != OtaState::Receiving
            || session != self.session
            || index >= self.map.len()
            || self.map.contains(index)
        {
            return ChunkNeed::Drop;
        }
        if chunk_last_page(index, self.image_len) >= self.erased as u32 {
            ChunkNeed::Erase(self.erased as u32)
        } else {
            ChunkNeed::Ready
        }
    }

    /// Store a chunk, erasing the pages it needs first. Chunks of other
    /// sessions, duplicates, out-of-range indices and chunks arriving
    /// outside `Receiving` are ignored. All-0xFF double words are already in
    /// the erased state and are not programmed; bytes past the image length
    /// are never programmed. A flash error fails the session (the host must
    /// start a new one). The chunk completing the image triggers the
    /// verification. Returns true if the chunk was new.
    pub fn chunk<F: RegionFlash>(&mut self, c: &OtaChunk, f: &mut F) -> bool {
        loop {
            match self.chunk_need(c.session, c.index) {
                ChunkNeed::Drop => return false,
                ChunkNeed::Erase(_) => {
                    self.erase_next(f);
                }
                ChunkNeed::Ready => break,
            }
        }
        let base = c.index as u32 * OTA_CHUNK as u32;
        let (dws, _) = c.data.as_chunks::<{ WRITE_SIZE as usize }>();
        for (k, &dw) in dws.iter().enumerate() {
            let off = base + k as u32 * WRITE_SIZE;
            if off >= self.image_len {
                break;
            }
            if dw != [0xFF; 8] && !f.program(off, dw) {
                self.state = OtaState::Failed;
                return false;
            }
        }
        self.map.insert(c.index);
        if self.map.complete() {
            self.verify(f);
        }
        true
    }

    /// Header, vector table, app CRC (via [`validate_image`]) and the CRC of
    /// the whole image as announced in `OtaBegin`.
    fn verify(&mut self, f: &dyn RegionRead) {
        self.verified = validate_image(f, self.hw_rev)
            .filter(|h| h.image_len() <= self.image_len && f.crc32(0, self.image_len) == self.image_crc);
        self.state = if self.verified.is_some() { OtaState::Verified } else { OtaState::Failed };
    }

    /// Handle `OtaCommit`: the verdict of a complete image (`Verified` /
    /// `Failed`), else the current state. Idempotent.
    pub fn commit(&mut self, session: u8, f: &dyn RegionRead) -> OtaState {
        if session == self.session && self.state == OtaState::Receiving && self.map.complete() && !self.map.is_empty() {
            self.verify(f);
        }
        self.state
    }

    /// `OtaBitmap` reply: the first window at or after `window` with holes
    /// (else `window` itself). `pending` lists chunk indices received but
    /// not yet programmed (still in the RX ring); they count as received.
    pub fn bitmap(&self, echo: u8, window: u8, pending: &mut dyn Iterator<Item = u16>) -> OtaBitmap {
        const WIN_WORDS: usize = OTA_WINDOW_CHUNKS / 32;
        let len = self.map.len();
        let mut bits = self.map.bits;
        if self.state == OtaState::Receiving {
            for i in pending {
                if i < len {
                    set(&mut bits, i);
                }
            }
        }
        // Chunks past the image read as received.
        for i in len..(WORDS * 32) as u16 {
            set(&mut bits, i);
        }
        let ones: u32 = bits.iter().map(|w| w.count_ones()).sum();
        let missing = (WORDS * 32) as u32 - ones;
        let word = |k: usize| bits.get(k).copied().unwrap_or(u32::MAX);
        let holes = |w: usize| (w * WIN_WORDS..(w + 1) * WIN_WORDS).any(|k| word(k) != u32::MAX);
        let windows = WORDS.div_ceil(WIN_WORDS);
        let mut w = window as usize;
        while w < windows && !holes(w) {
            w += 1;
        }
        if w >= windows {
            w = window as usize;
        }
        let mut out = [0u8; OTA_BITMAP_BYTES];
        for (k, b) in out.as_chunks_mut::<4>().0.iter_mut().enumerate() {
            *b = word(w * WIN_WORDS + k).to_le_bytes();
        }
        OtaBitmap {
            echo,
            session: self.session,
            state: self.state,
            window: w as u8,
            missing_count: missing as u16,
            bits: out,
        }
    }
}

#[cfg(test)]
pub(crate) mod mock {
    use super::*;
    use lumigift_proto::image::crc32;
    pub use lumigift_proto::layout::APP_PAGES;

    /// Flash model that enforces the STM32 rules (program only erased,
    /// never-programmed double words), counts operations, and can lose
    /// power: after `ops_left` reaches 0 the interrupted operation leaves
    /// garbage behind and every later operation fails.
    pub struct MockFlash {
        pub mem: Vec<u8>,
        programmed: Vec<bool>,
        erased: Vec<bool>,
        pub fail_program_at: Option<u32>,
        pub fail_erase_page: Option<u32>,
        pub ops: usize,
        pub erases: usize,
        pub ops_left: Option<usize>,
        pub dead: bool,
        noise: u32,
    }

    impl MockFlash {
        pub fn new() -> Self {
            let n = APP_REGION_SIZE as usize;
            Self {
                mem: vec![0xA5; n],
                programmed: vec![false; n / 8],
                erased: vec![false; n / 8],
                fail_program_at: None,
                fail_erase_page: None,
                ops: 0,
                erases: 0,
                ops_left: None,
                dead: false,
                noise: 0x1234_5678,
            }
        }

        fn noise(&mut self) -> u8 {
            self.noise ^= self.noise << 13;
            self.noise ^= self.noise >> 17;
            self.noise ^= self.noise << 5;
            self.noise as u8
        }

        /// True if this operation is cut by the power loss.
        fn cut(&mut self) -> bool {
            if self.dead {
                return true;
            }
            self.ops += 1;
            match &mut self.ops_left {
                Some(0) => {
                    self.dead = true;
                    true
                }
                Some(n) => {
                    *n -= 1;
                    false
                }
                None => false,
            }
        }

        /// A power cycle: the flash keeps its (possibly garbage) contents,
        /// the programmed/erased bookkeeping is lost (a reflash after
        /// erase is always legal).
        pub fn power_cycle(&mut self) {
            self.dead = false;
            self.ops_left = None;
        }
    }

    impl RegionRead for MockFlash {
        fn read(&self, offset: u32, out: &mut [u8]) {
            out.copy_from_slice(&self.mem[offset as usize..offset as usize + out.len()]);
        }
        fn crc32(&self, offset: u32, len: u32) -> u32 {
            crc32(&self.mem[offset as usize..(offset + len) as usize])
        }
    }

    impl RegionFlash for MockFlash {
        fn erase_page(&mut self, page: u32) -> bool {
            assert!(page < APP_PAGES, "erase outside the app region: page {page}");
            let s = (page * PAGE_SIZE) as usize;
            if self.cut() {
                // Interrupted erase: some bytes erased, others not.
                for i in s..s + PAGE_SIZE as usize {
                    if self.noise() & 1 == 0 {
                        self.mem[i] = 0xFF;
                    }
                }
                return false;
            }
            self.erases += 1;
            if self.fail_erase_page == Some(page) {
                return false;
            }
            self.mem[s..s + PAGE_SIZE as usize].fill(0xFF);
            for d in s / 8..(s + PAGE_SIZE as usize) / 8 {
                self.programmed[d] = false;
                self.erased[d] = true;
            }
            true
        }
        fn program(&mut self, offset: u32, dw: [u8; 8]) -> bool {
            assert_eq!(offset % 8, 0);
            assert!(offset < APP_REGION_SIZE, "write outside region at {offset}");
            let d = offset as usize / 8;
            if self.cut() {
                for i in offset as usize..offset as usize + 8 {
                    let n = self.noise();
                    self.mem[i] &= n;
                }
                return false;
            }
            assert!(self.erased[d], "programming a non-erased double word at {offset}");
            assert!(!self.programmed[d], "double word at {offset} programmed twice");
            if self.fail_program_at == Some(offset) {
                return false;
            }
            self.programmed[d] = true;
            self.mem[offset as usize..offset as usize + 8].copy_from_slice(&dw);
            true
        }
    }

    pub fn make_image_with(app_len: usize, hw_rev: u16, fw_version: u32, seed: usize) -> Vec<u8> {
        let mut app: Vec<u8> = (0..app_len).map(|i| (i * 7 + 3 + seed * 13) as u8).collect();
        app[0..4].copy_from_slice(&0x2000_17F0u32.to_le_bytes());
        app[4..8].copy_from_slice(&(APP_VECTORS + 0x101).to_le_bytes());
        // Some erased-looking runs to exercise the 0xFF skip.
        for b in &mut app[64..96] {
            *b = 0xFF;
        }
        let h = ImageHeader::new(&app, hw_rev, fw_version, *b"deadbeef");
        let mut img = vec![0xFF; IMAGE_HEADER_SIZE as usize];
        img[..32].copy_from_slice(&h.encode());
        img.extend_from_slice(&app);
        img
    }

    pub fn make_image(app_len: usize, hw_rev: u16) -> Vec<u8> {
        make_image_with(app_len, hw_rev, 1234, 0)
    }

    pub fn chunk_of(img: &[u8], session: u8, i: u16) -> OtaChunk {
        let mut data = [0xFF; OTA_CHUNK];
        let s = i as usize * OTA_CHUNK;
        let e = (s + OTA_CHUNK).min(img.len());
        data[..e - s].copy_from_slice(&img[s..e]);
        OtaChunk { session, index: i, data }
    }

    pub fn begin(img: &[u8], session: u8) -> OtaBegin {
        OtaBegin { session, image_len: img.len() as u32, image_crc: crc32(img) }
    }
}

#[cfg(test)]
mod tests {
    use super::mock::*;
    use super::*;

    #[test]
    fn erases_only_the_pages_the_image_covers_in_order() {
        let img = make_image(5000, 1); // 5256 bytes -> 3 pages
        let mut f = MockFlash::new();
        let mut ota = Ota::new(1);
        assert!(ota.begin(&begin(&img, 7)));
        assert_eq!(ota.state(), OtaState::Receiving, "no blocking erase phase");
        assert_eq!(f.erases, 0, "Begin itself erases nothing");
        // Chunk 0 needs page 0; a chunk in page 2 needs pages 0, 1, 2 first.
        assert_eq!(ota.chunk_need(7, 0), ChunkNeed::Erase(0));
        assert_eq!(ota.chunk_need(7, 200), ChunkNeed::Erase(0));
        assert!(ota.chunk(&chunk_of(&img, 7, 200), &mut f));
        assert_eq!(ota.pages_erased(), 3);
        assert_eq!(ota.chunk_need(7, 0), ChunkNeed::Ready);
        assert_eq!(ota.next_erase(), None);
        let n = chunk_count(img.len() as u32);
        for i in 0..n {
            ota.chunk(&chunk_of(&img, 7, i), &mut f);
        }
        assert_eq!(ota.state(), OtaState::Verified, "verified on the last chunk");
        assert_eq!(f.erases, 3);
        // Pages past the image were never touched.
        assert!(f.mem[3 * PAGE_SIZE as usize..].iter().all(|&b| b == 0xA5));
    }

    #[test]
    fn erase_ahead_then_chunks_straddling_pages() {
        let img = make_image(6000, 1);
        let mut f = MockFlash::new();
        let mut ota = Ota::new(1);
        ota.begin(&begin(&img, 1));
        // Idle receiver erases ahead, one page per call, in order.
        assert_eq!(ota.next_erase(), Some(0));
        assert!(ota.erase_next(&mut f));
        assert_eq!(ota.next_erase(), Some(1));
        // Chunk 85 covers bytes 2040..2064: pages 0 and 1.
        assert_eq!(chunk_last_page(85, img.len() as u32), 1);
        assert_eq!(ota.chunk_need(1, 85), ChunkNeed::Erase(1));
        assert_eq!(ota.chunk_need(1, 84), ChunkNeed::Ready);
        while ota.erase_next(&mut f) {}
        assert_eq!(ota.pages_erased(), pages_for(img.len() as u32));
        for i in (0..chunk_count(img.len() as u32)).rev() {
            assert!(ota.chunk(&chunk_of(&img, 1, i), &mut f));
        }
        assert_eq!(ota.state(), OtaState::Verified);
    }

    #[test]
    fn padding_of_the_last_chunk_needs_no_extra_page() {
        // Image ends exactly on a page boundary; the last chunk overhangs it.
        let img = make_image(2 * PAGE_SIZE as usize - IMAGE_HEADER_SIZE as usize, 1);
        assert_eq!(img.len() % PAGE_SIZE as usize, 0);
        let last = chunk_count(img.len() as u32) - 1;
        assert!((last as usize + 1) * OTA_CHUNK > img.len());
        assert_eq!(chunk_last_page(last, img.len() as u32), 1);
        let mut f = MockFlash::new();
        let mut ota = Ota::new(1);
        ota.begin(&begin(&img, 1));
        for i in 0..=last {
            ota.chunk(&chunk_of(&img, 1, i), &mut f);
        }
        assert_eq!(f.erases, 2);
        assert_eq!(ota.state(), OtaState::Verified);
    }

    #[test]
    fn duplicates_out_of_order_stale_session_and_repeated_begin() {
        let img = make_image(5000, 1);
        let n = chunk_count(img.len() as u32);
        let mut f = MockFlash::new();
        let mut ota = Ota::new(1);
        assert!(ota.begin(&begin(&img, 7)));
        // Odd indices backwards, then everything again (duplicates), skipping 3.
        for i in (1..n).step_by(2).rev().filter(|&i| i != 3) {
            assert!(ota.chunk(&chunk_of(&img, 7, i), &mut f));
        }
        // A Begin repeated mid-transfer is ignored and keeps the progress.
        assert!(!ota.begin(&begin(&img, 7)));
        for i in 0..n {
            if i != 3 {
                let fresh = ota.chunk(&chunk_of(&img, 7, i), &mut f);
                assert_eq!(fresh, i % 2 == 0, "chunk {i}");
            }
        }
        // Stale session (an old host still streaming) is ignored.
        assert!(!ota.chunk(&chunk_of(&img, 6, 3), &mut f));
        let b = ota.bitmap(1, 0, &mut core::iter::empty());
        assert_eq!((b.missing_count, b.window), (1, 0));
        assert_eq!(b.missing(n as u32).collect::<Vec<_>>(), vec![3]);
        // Commit with missing chunks keeps receiving.
        assert_eq!(ota.commit(7, &f), OtaState::Receiving);
        assert!(ota.chunk(&chunk_of(&img, 7, 3), &mut f));
        assert_eq!(ota.state(), OtaState::Verified);
        assert_eq!(ota.commit(9, &f), OtaState::Verified, "verdict stays");
        assert_eq!(ota.commit(7, &f), OtaState::Verified, "idempotent");
        assert_eq!(&f.mem[..img.len()], &img[..]);
        assert!(validate_image(&f, 1).is_some());
        // A Begin of the verified session is ignored, a new session restarts.
        assert!(!ota.begin(&begin(&img, 7)));
        assert!(ota.begin(&begin(&img, 8)));
        assert_eq!(ota.state(), OtaState::Receiving);
        assert_eq!(ota.pages_erased(), 0);
    }

    #[test]
    fn host_vanishes_then_new_session_starts_over() {
        let img = make_image(9000, 1);
        let img2 = make_image_with(7000, 1, 99, 5);
        let mut f = MockFlash::new();
        let mut ota = Ota::new(1);
        ota.begin(&begin(&img, 1));
        for i in 0..150 {
            ota.chunk(&chunk_of(&img, 1, i), &mut f);
        }
        // The first host is gone; another one starts a different image.
        assert!(ota.begin(&begin(&img2, 2)));
        assert_eq!(ota.bitmap(0, 0, &mut core::iter::empty()).missing_count, chunk_count(img2.len() as u32));
        // Late chunks of the old session are ignored.
        assert!(!ota.chunk(&chunk_of(&img, 1, 150), &mut f));
        for i in 0..chunk_count(img2.len() as u32) {
            assert!(ota.chunk(&chunk_of(&img2, 2, i), &mut f));
        }
        assert_eq!(ota.state(), OtaState::Verified);
        assert_eq!(validate_image(&f, 1).unwrap().fw_version, 99);
    }

    #[test]
    fn bitmap_windows_hint_and_pending_chunks() {
        let img = make_image(15_000, 1); // 636 chunks -> 4 windows
        let n = chunk_count(img.len() as u32);
        let mut f = MockFlash::new();
        let mut ota = Ota::new(1);
        ota.begin(&begin(&img, 4));
        for i in 0..n {
            if i != 200 && i != 201 && i != 600 {
                ota.chunk(&chunk_of(&img, 4, i), &mut f);
            }
        }
        // Window 0 is complete: the reply skips ahead to window 1.
        let b = ota.bitmap(9, 0, &mut core::iter::empty());
        assert_eq!((b.echo, b.window, b.missing_count), (9, 1, 3));
        assert_eq!(b.missing(n as u32).collect::<Vec<_>>(), vec![200, 201]);
        let b = ota.bitmap(9, 2, &mut core::iter::empty());
        assert_eq!(b.window, 3);
        assert_eq!(b.missing(n as u32).collect::<Vec<_>>(), vec![600]);
        // Nothing after window 3: the requested window comes back as is.
        let b = ota.bitmap(9, 4, &mut core::iter::empty());
        assert_eq!((b.window, b.missing(n as u32).count()), (4, 0));
        // Chunks still waiting in the RX ring count as received.
        let b = ota.bitmap(9, 0, &mut [200, 201, 9999].into_iter());
        assert_eq!((b.window, b.missing_count), (3, 1));
        // Bits past the image read as received.
        assert!(b.received(n as usize % 192 + 1));
    }

    #[test]
    fn corrupt_images_fail_and_the_receiver_stays_updatable() {
        let good = make_image(3000, 1);
        let o = IMAGE_HEADER_SIZE as usize;
        let rehead = |img: &mut Vec<u8>, hw_rev: u16| {
            let h = ImageHeader::new(&img[o..], hw_rev, 1, [0; 8]);
            img[..32].copy_from_slice(&h.encode());
        };
        let mut bad_header = good.clone();
        bad_header[10] ^= 0x01; // header CRC mismatch
        let mut bad_app = good.clone();
        bad_app[o + 500] ^= 0x55; // app CRC mismatch
        let mut bad_hw = good.clone();
        rehead(&mut bad_hw, 2);
        let mut bad_vectors = good.clone();
        bad_vectors[o + 4] &= !1; // reset vector without the Thumb bit
        rehead(&mut bad_vectors, 1);
        let mut bad_sp = good.clone();
        bad_sp[o..o + 4].copy_from_slice(&0x0800_0000u32.to_le_bytes());
        rehead(&mut bad_sp, 1);

        let mut f = MockFlash::new();
        let mut ota = Ota::new(1);
        for (k, img) in [bad_header, bad_app, bad_hw, bad_vectors, bad_sp].iter().enumerate() {
            let s = k as u8 + 1;
            assert!(ota.begin(&begin(img, s)));
            for i in 0..chunk_count(img.len() as u32) {
                ota.chunk(&chunk_of(img, s, i), &mut f);
            }
            assert_eq!(ota.state(), OtaState::Failed, "image {k}");
            assert!(validate_image(&f, 1).is_none(), "image {k}");
            assert_eq!(ota.commit(s, &f), OtaState::Failed);
        }
        // Same session id restarts after a failure; a good image goes through.
        assert!(ota.begin(&begin(&good, 5)));
        for i in 0..chunk_count(good.len() as u32) {
            ota.chunk(&chunk_of(&good, 5, i), &mut f);
        }
        assert_eq!(ota.state(), OtaState::Verified);
    }

    #[test]
    fn full_region_last_chunk_is_clipped() {
        let img = make_image((APP_REGION_SIZE - IMAGE_HEADER_SIZE) as usize, 1);
        assert_eq!(img.len() as u32, APP_REGION_SIZE);
        let mut f = MockFlash::new();
        let mut ota = Ota::new(1);
        ota.begin(&begin(&img, 3));
        let n = chunk_count(img.len() as u32);
        assert_eq!(n as u32, MAX_OTA_CHUNKS);
        for i in (0..n).rev() {
            assert!(ota.chunk(&chunk_of(&img, 3, i), &mut f));
        }
        assert_eq!(ota.state(), OtaState::Verified);
        assert_eq!(f.erases as u32, APP_PAGES);
    }

    #[test]
    fn flash_errors_fail_session() {
        let img = make_image(1000, 1);
        let mut f = MockFlash::new();
        f.fail_program_at = Some(480);
        let mut ota = Ota::new(1);
        ota.begin(&begin(&img, 1));
        assert!(!ota.chunk(&chunk_of(&img, 1, 20), &mut f));
        assert_eq!(ota.state(), OtaState::Failed);
        let mut f = MockFlash::new();
        f.fail_erase_page = Some(0);
        ota.begin(&begin(&img, 2));
        assert!(!ota.chunk(&chunk_of(&img, 2, 0), &mut f));
        assert_eq!(ota.state(), OtaState::Failed);
        assert_eq!(ota.next_erase(), None);
    }

    #[test]
    fn rejects_bad_lengths() {
        let mut ota = Ota::new(1);
        assert!(!ota.begin(&OtaBegin { session: 1, image_len: 100, image_crc: 0 }));
        assert!(!ota.begin(&OtaBegin { session: 1, image_len: APP_REGION_SIZE + 1, image_crc: 0 }));
        assert_eq!(ota.state(), OtaState::Idle);
        assert_eq!(ota.bitmap(5, 0, &mut core::iter::empty()).missing_count, 0);
    }
}
