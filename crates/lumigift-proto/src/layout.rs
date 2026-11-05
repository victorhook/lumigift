//! Flash and RAM layout of the STM32C011F6 (32 KiB flash, 2 KiB pages, 6 KiB RAM).
//!
//! ```text
//! 0x0800_0000  bootloader     10 KiB  (pages 0..=4)   never written over the air,
//!                                                     write-protected (WRP area A)
//!   0x0800_27F0  boot info block (16 B, last bytes of the bootloader)
//! 0x0800_2800  app region     20 KiB  (pages 5..=14)  ImageHeader + app
//!   0x0800_2800  ImageHeader  (256 B, keeps the vector table 256-aligned)
//!   0x0800_2900  app vector table + code
//! 0x0800_7800  config          2 KiB  (page 15)       append-only ConfigRecord log
//!
//! 0x2000_0000  RAM            6 KiB - 16 B
//! 0x2000_17F0  BootShared     16 B, not zeroed by either image
//! ```
//!
//! Who may write what (enforced in `lumigift-board`'s flash driver, and the
//! bootloader pages additionally by the option-byte write protection):
//!
//! | Image | May erase/program |
//! |---|---|
//! | bootloader | app region + config page |
//! | app | config page only |

pub const FLASH_BASE: u32 = 0x0800_0000;
pub const FLASH_SIZE: u32 = 32 * 1024;
pub const FLASH_END: u32 = FLASH_BASE + FLASH_SIZE;
pub const PAGE_SIZE: u32 = 2048;
/// Flash programming granularity (one double word).
pub const WRITE_SIZE: u32 = 8;

pub const BOOT_BASE: u32 = FLASH_BASE;
pub const BOOT_SIZE: u32 = 10 * 1024;
/// Pages `0..BOOT_PAGES` hold the bootloader (covered by WRP area A).
pub const BOOT_PAGES: u32 = BOOT_SIZE / PAGE_SIZE;
/// 16-byte boot info block in the last bytes of the bootloader region:
/// `[BOOT_INFO_MAGIC, boot_version, 0, 0]` (u32 each).
pub const BOOT_INFO_ADDR: u32 = BOOT_BASE + BOOT_SIZE - 16;
pub const BOOT_INFO_MAGIC: u32 = 0x4F42_474C; // "LGBO"

pub const APP_REGION_BASE: u32 = BOOT_BASE + BOOT_SIZE;
pub const APP_REGION_SIZE: u32 = 20 * 1024;
/// Pages in the app region.
pub const APP_PAGES: u32 = APP_REGION_SIZE / PAGE_SIZE;
pub const IMAGE_HEADER_SIZE: u32 = 256;
/// Where the app is linked; its vector table starts here.
pub const APP_VECTORS: u32 = APP_REGION_BASE + IMAGE_HEADER_SIZE;
pub const APP_MAX_SIZE: u32 = APP_REGION_SIZE - IMAGE_HEADER_SIZE;

pub const CONFIG_BASE: u32 = APP_REGION_BASE + APP_REGION_SIZE;
pub const CONFIG_SIZE: u32 = PAGE_SIZE;

pub const RAM_BASE: u32 = 0x2000_0000;
pub const RAM_SIZE: u32 = 6 * 1024;
pub const BOOT_SHARED_SIZE: u32 = 16;
pub const BOOT_SHARED_ADDR: u32 = RAM_BASE + RAM_SIZE - BOOT_SHARED_SIZE;

const _: () = assert!(CONFIG_BASE + CONFIG_SIZE == FLASH_END);
const _: () = assert!(BOOT_SIZE.is_multiple_of(PAGE_SIZE));
const _: () =
    assert!(APP_REGION_BASE.is_multiple_of(PAGE_SIZE) && APP_REGION_SIZE.is_multiple_of(PAGE_SIZE));

/// Flash page index of an absolute address.
pub const fn page_of(addr: u32) -> u32 {
    (addr - FLASH_BASE) / PAGE_SIZE
}

/// Maximum number of OTA chunks for a full app region.
pub const MAX_OTA_CHUNKS: u32 = APP_REGION_SIZE.div_ceil(crate::OTA_CHUNK as u32);

/// State shared between bootloader and app across warm resets
/// (lives in RAM excluded from both linker scripts).
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BootShared {
    /// [`BootShared::MAGIC`] when the other fields are valid.
    pub magic: u32,
    /// [`BootShared::REQ_STAY_IN_BOOT`] or 0.
    pub request: u32,
    /// Incremented by the bootloader before starting the app, cleared by
    /// the app once healthy. At [`BootShared::MAX_BOOT_ATTEMPTS`] the
    /// bootloader stays in recovery mode.
    pub boot_attempts: u16,
    /// Warm resets since power-on.
    pub resets: u16,
    /// Last reset reason as `ResetReason`, latched by the bootloader
    /// (it clears the RCC flags).
    pub reset_reason: u8,
    /// Bootloader-private flags ([`BootShared::FLAG_WRP_TRIED`]), kept
    /// across warm resets and cleared on power-on.
    pub flags: u8,
    pub _pad: [u8; 2],
}

impl BootShared {
    pub const MAGIC: u32 = 0xB007_5EED;
    pub const REQ_STAY_IN_BOOT: u32 = 0x5741_4954;
    pub const MAX_BOOT_ATTEMPTS: u16 = 3;
    /// The bootloader already tried to program its write protection during
    /// this power cycle (so a failing attempt can never cause a reset loop).
    pub const FLAG_WRP_TRIED: u8 = 1 << 0;

    /// Freshly initialised state (after power-on).
    pub const fn new() -> Self {
        Self {
            magic: Self::MAGIC,
            request: 0,
            boot_attempts: 0,
            resets: 0,
            reset_reason: 0,
            flags: 0,
            _pad: [0; 2],
        }
    }
}

impl Default for BootShared {
    fn default() -> Self {
        Self::new()
    }
}

const _: () = assert!(core::mem::size_of::<BootShared>() == BOOT_SHARED_SIZE as usize);

/// `raw + raw * gain / 32768 + 2 * offset_2mv`, in mV (the battery
/// calibration stored in [`ConfigRecord`]).
pub fn calibrate_vbat_mv(raw_mv: u16, gain: i16, offset_2mv: i8) -> u16 {
    let mv = raw_mv as i32;
    let v = mv + ((mv * gain as i32) >> 15) + 2 * offset_2mv as i32;
    v.clamp(0, u16::MAX as i32) as u16
}

/// One 16-byte record of the config log. The last record whose `magic`
/// and `crc` match is current; erased slots read all-ones. When the page
/// is full it is erased and the current record rewritten first.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ConfigRecord {
    pub magic: u16,
    pub node_addr: u8,
    pub slot: u8,
    pub max_brightness: u8,
    pub channel: u8,
    pub idle_timeout_100ms: u8,
    /// Battery measurement calibration (0 = none, the bytes were reserved
    /// and zero before): `mv + mv * vbat_gain / 32768 + 2 * vbat_offset_2mv`,
    /// see [`ConfigRecord::calibrate_vbat`].
    pub vbat_offset_2mv: i8,
    pub power_budget_ma: u16,
    pub vbat_gain: i16,
    /// CRC-32 of the preceding 12 bytes.
    pub crc: u32,
}

impl ConfigRecord {
    pub const MAGIC: u16 = 0xC0F1;
    pub const SIZE: usize = 16;

    pub fn defaults(uid: &[u8; 12]) -> Self {
        let mut r = Self {
            magic: Self::MAGIC,
            node_addr: crate::default_node_addr(uid),
            slot: crate::UNASSIGNED_SLOT,
            max_brightness: 160,
            channel: crate::DEFAULT_CHANNEL,
            idle_timeout_100ms: 10,
            vbat_offset_2mv: 0,
            power_budget_ma: 250,
            vbat_gain: 0,
            crc: 0,
        };
        r.seal();
        r
    }

    pub fn to_bytes(&self) -> [u8; 16] {
        let mut b = [0u8; 16];
        b[0..2].copy_from_slice(&self.magic.to_le_bytes());
        b[2] = self.node_addr;
        b[3] = self.slot;
        b[4] = self.max_brightness;
        b[5] = self.channel;
        b[6] = self.idle_timeout_100ms;
        b[7] = self.vbat_offset_2mv as u8;
        b[8..10].copy_from_slice(&self.power_budget_ma.to_le_bytes());
        b[10..12].copy_from_slice(&self.vbat_gain.to_le_bytes());
        b[12..16].copy_from_slice(&self.crc.to_le_bytes());
        b
    }

    pub fn from_bytes(b: &[u8; 16]) -> Option<Self> {
        let r = Self {
            magic: u16::from_le_bytes([b[0], b[1]]),
            node_addr: b[2],
            slot: b[3],
            max_brightness: b[4],
            channel: b[5],
            idle_timeout_100ms: b[6],
            vbat_offset_2mv: b[7] as i8,
            power_budget_ma: u16::from_le_bytes([b[8], b[9]]),
            vbat_gain: i16::from_le_bytes([b[10], b[11]]),
            crc: u32::from_le_bytes([b[12], b[13], b[14], b[15]]),
        };
        (r.magic == Self::MAGIC && r.crc == r.compute_crc()).then_some(r)
    }

    /// A raw battery reading (mV) corrected with this gift's calibration.
    pub fn calibrate_vbat(&self, raw_mv: u16) -> u16 {
        calibrate_vbat_mv(raw_mv, self.vbat_gain, self.vbat_offset_2mv)
    }

    /// Recompute after changing fields.
    pub fn compute_crc(&self) -> u32 {
        // The CRC field is the last 4 bytes, so it never covers itself.
        crate::image::crc32(&self.to_bytes()[..12])
    }

    pub fn seal(&mut self) {
        self.crc = self.compute_crc();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_roundtrip() {
        let mut c = ConfigRecord::defaults(&[1; 12]);
        assert_eq!(ConfigRecord::from_bytes(&c.to_bytes()), Some(c));
        c.slot = 3;
        assert_eq!(
            ConfigRecord::from_bytes(&c.to_bytes()),
            None,
            "stale crc rejected"
        );
        c.seal();
        assert_eq!(ConfigRecord::from_bytes(&c.to_bytes()), Some(c));
        assert_eq!(ConfigRecord::from_bytes(&[0xFF; 16]), None);
    }
}
