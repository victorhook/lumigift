//! Firmware image format: an [`ImageHeader`] padded to
//! [`IMAGE_HEADER_SIZE`](crate::layout::IMAGE_HEADER_SIZE) bytes, followed by
//! the app binary linked at [`APP_VECTORS`](crate::layout::APP_VECTORS).
//! The whole image is written to the app region starting at its base.

use crate::layout::{APP_MAX_SIZE, IMAGE_HEADER_SIZE};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ImageHeader {
    pub magic: u32,
    pub header_version: u16,
    /// Board revision the image targets.
    pub hw_rev: u16,
    /// App binary length (excluding the header).
    pub app_len: u32,
    /// CRC-32 of the app binary.
    pub app_crc: u32,
    /// Monotonic version, e.g. a build timestamp.
    pub fw_version: u32,
    /// First 8 bytes of the git commit hash.
    pub git_hash: [u8; 8],
    /// CRC-32 of the preceding 28 bytes.
    pub header_crc: u32,
}

impl ImageHeader {
    pub const MAGIC: u32 = 0x5446_474C; // "LGFT"
    pub const VERSION: u16 = 1;
    pub const ENCODED_LEN: usize = 32;

    pub fn new(app: &[u8], hw_rev: u16, fw_version: u32, git_hash: [u8; 8]) -> Self {
        let mut h = Self {
            magic: Self::MAGIC,
            header_version: Self::VERSION,
            hw_rev,
            app_len: app.len() as u32,
            app_crc: crc32(app),
            fw_version,
            git_hash,
            header_crc: 0,
        };
        h.header_crc = crc32(&h.encode()[..28]);
        h
    }

    pub fn encode(&self) -> [u8; Self::ENCODED_LEN] {
        let mut b = [0u8; Self::ENCODED_LEN];
        b[0..4].copy_from_slice(&self.magic.to_le_bytes());
        b[4..6].copy_from_slice(&self.header_version.to_le_bytes());
        b[6..8].copy_from_slice(&self.hw_rev.to_le_bytes());
        b[8..12].copy_from_slice(&self.app_len.to_le_bytes());
        b[12..16].copy_from_slice(&self.app_crc.to_le_bytes());
        b[16..20].copy_from_slice(&self.fw_version.to_le_bytes());
        b[20..28].copy_from_slice(&self.git_hash);
        b[28..32].copy_from_slice(&self.header_crc.to_le_bytes());
        b
    }

    /// Parse and validate magic, version, header CRC and size bounds.
    /// The app CRC must be checked separately against the flash contents.
    pub fn decode(b: &[u8]) -> Option<Self> {
        if b.len() < Self::ENCODED_LEN {
            return None;
        }
        let u32_at = |o: usize| u32::from_le_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]]);
        let h = Self {
            magic: u32_at(0),
            header_version: u16::from_le_bytes([b[4], b[5]]),
            hw_rev: u16::from_le_bytes([b[6], b[7]]),
            app_len: u32_at(8),
            app_crc: u32_at(12),
            fw_version: u32_at(16),
            git_hash: b[20..28].try_into().unwrap(),
            header_crc: u32_at(28),
        };
        let ok = h.magic == Self::MAGIC
            && h.header_version == Self::VERSION
            && h.app_len > 0
            && h.app_len <= APP_MAX_SIZE
            && h.header_crc == crc32(&b[..28]);
        ok.then_some(h)
    }

    /// Total image length (header block + app) as written to flash.
    pub fn image_len(&self) -> u32 {
        IMAGE_HEADER_SIZE + self.app_len
    }
}

/// CRC-32/ISO-HDLC (the common zlib/Ethernet CRC). Small table-less
/// implementation; firmware may use the STM32 CRC unit configured with
/// polynomial 0x04C11DB7, init 0xFFFFFFFF, input/output reversal and a
/// final XOR of 0xFFFFFFFF to get the same result.
pub fn crc32(data: &[u8]) -> u32 {
    crc32_update(0xFFFF_FFFF, data) ^ 0xFFFF_FFFF
}

/// Incremental form: start with `0xFFFF_FFFF`, finish by XOR-ing `0xFFFF_FFFF`.
pub fn crc32_update(mut crc: u32, data: &[u8]) -> u32 {
    for &b in data {
        crc ^= b as u32;
        for _ in 0..8 {
            crc = (crc >> 1) ^ (0xEDB8_8320 & (crc & 1).wrapping_neg());
        }
    }
    crc
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crc_known_value() {
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
    }

    #[test]
    fn header_roundtrip() {
        let app = [0x55u8; 1000];
        let h = ImageHeader::new(&app, 1, 42, *b"abcdef01");
        let enc = h.encode();
        assert_eq!(ImageHeader::decode(&enc), Some(h));
        let mut bad = enc;
        bad[17] ^= 1;
        assert_eq!(ImageHeader::decode(&bad), None);
    }
}
