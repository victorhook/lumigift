//! Reset-reason latching and the bootloader's boot decision
//! (docs/ARCHITECTURE.md, "Firmware update").

use lumigift_proto::ResetReason;
use lumigift_proto::layout::{APP_REGION_BASE, BOOT_PAGES, BootShared, FLASH_END};

/// Reset flags in `RCC_CSR2` (RM0490 §6.4.27).
pub mod csr2 {
    pub const OBLRSTF: u32 = 1 << 25;
    pub const PINRSTF: u32 = 1 << 26;
    /// BOR or POR.
    pub const PWRRSTF: u32 = 1 << 27;
    pub const SFTRSTF: u32 = 1 << 28;
    pub const IWDGRSTF: u32 = 1 << 29;
    pub const WWDGRSTF: u32 = 1 << 30;
    pub const LPWRRSTF: u32 = 1 << 31;
    pub const ALL: u32 = OBLRSTF | PINRSTF | PWRRSTF | SFTRSTF | IWDGRSTF | WWDGRSTF | LPWRRSTF;
}

/// Map the RCC reset flags to a [`ResetReason`].
///
/// Every internal reset source also pulses NRST, so `PINRSTF` accompanies
/// most other flags: check the specific sources first. A POR/BOR flag while
/// the RAM still held a valid [`BootShared`] means the supply only dipped
/// (RAM retained), which we report as `Brownout`.
pub fn reset_reason(flags: u32, shared_was_valid: bool) -> ResetReason {
    use csr2::*;
    if flags & (IWDGRSTF | WWDGRSTF) != 0 {
        ResetReason::Watchdog
    } else if flags & (SFTRSTF | OBLRSTF) != 0 {
        ResetReason::Software
    } else if flags & LPWRRSTF != 0 {
        ResetReason::LowPower
    } else if flags & PWRRSTF != 0 {
        if shared_was_valid { ResetReason::Brownout } else { ResetReason::PowerOn }
    } else if flags & PINRSTF != 0 {
        ResetReason::Pin
    } else {
        ResetReason::Unknown
    }
}

/// Compute the new [`BootShared`] from what survived in RAM and the reset
/// flags. Re-initialises it after power-on (bad magic or POR/BOR flag),
/// otherwise counts one more warm reset.
pub fn latch(prev: BootShared, flags: u32) -> BootShared {
    let valid = prev.magic == BootShared::MAGIC;
    let reason = reset_reason(flags, valid);
    let power_cycle = !valid || flags & csr2::PWRRSTF != 0;
    let mut s = if power_cycle {
        BootShared::new()
    } else {
        let mut s = prev;
        s.resets = s.resets.wrapping_add(1);
        s
    };
    s.reset_reason = reason as u8;
    s
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    /// Stay in recovery mode (OTA receiver).
    Recovery,
    /// Listen briefly for `BootHold`, then start the app.
    ListenThenBoot,
}

pub fn decide(image_valid: bool, s: &BootShared) -> Decision {
    if !image_valid || s.request == BootShared::REQ_STAY_IN_BOOT || s.boot_attempts >= BootShared::MAX_BOOT_ATTEMPTS {
        Decision::Recovery
    } else {
        Decision::ListenThenBoot
    }
}

// ---------------------------------------------------------------------------
// Bootloader write protection (option bytes, WRP area A)
// ---------------------------------------------------------------------------

/// `FLASH_WRP1AR` value protecting the bootloader pages `0..BOOT_PAGES`
/// (RM0490: `WRP1A_STRT` bits 3:0 = first page, `WRP1A_END` bits 19:16 =
/// last page; the area is active while `STRT <= END`).
pub const WRP1AR_BOOT: u32 = (BOOT_PAGES - 1) << 16;

/// Does `wrp1ar` (as read from `FLASH_WRP1AR`) protect every bootloader page?
pub fn wrp_covers_boot(wrp1ar: u32) -> bool {
    let strt = wrp1ar & 0x3F;
    let end = (wrp1ar >> 16) & 0x3F;
    strt == 0 && end >= BOOT_PAGES - 1
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WrpAction {
    /// Already protected.
    Protected,
    /// Program `WRP1AR_BOOT` and reload the option bytes (resets the chip).
    Program,
    /// Not protected, and programming was already tried in this power
    /// cycle: carry on unprotected rather than risk a reset loop.
    GiveUp,
}

/// Decide what to do about the bootloader write protection at boot.
/// `shared.flags & FLAG_WRP_TRIED` survives the option-byte reload reset
/// but not a power cycle, so a failing attempt costs at most one extra
/// reset per power-on.
pub fn wrp_action(wrp1ar: u32, shared: &BootShared) -> WrpAction {
    if wrp_covers_boot(wrp1ar) {
        WrpAction::Protected
    } else if shared.flags & BootShared::FLAG_WRP_TRIED != 0 {
        WrpAction::GiveUp
    } else {
        WrpAction::Program
    }
}

// ---------------------------------------------------------------------------
// Flash write guard
// ---------------------------------------------------------------------------

/// May an image whose lowest writable address is `floor` erase/program
/// `len` bytes at `addr`? Nothing below the app region is ever writable,
/// whatever `floor` says, and nothing outside the flash.
pub fn write_allowed(addr: u32, len: u32, floor: u32) -> bool {
    let floor = floor.max(APP_REGION_BASE);
    len > 0 && addr >= floor && addr.checked_add(len).is_some_and(|end| end <= FLASH_END)
}

#[cfg(test)]
mod tests {
    use super::*;
    use lumigift_proto::layout::{BOOT_SIZE, CONFIG_BASE, FLASH_BASE, PAGE_SIZE};

    fn garbage() -> BootShared {
        BootShared {
            magic: 0x1234_5678,
            request: 7,
            boot_attempts: 9,
            resets: 99,
            reset_reason: 3,
            flags: 0xFF,
            _pad: [0; 2],
        }
    }

    #[test]
    fn flash_guard() {
        // Bootloader image: app region + config page.
        let boot_floor = APP_REGION_BASE;
        assert!(!write_allowed(FLASH_BASE, PAGE_SIZE, boot_floor));
        assert!(!write_allowed(APP_REGION_BASE - 8, 8, boot_floor));
        assert!(!write_allowed(APP_REGION_BASE - 8, 16, boot_floor), "straddling");
        assert!(write_allowed(APP_REGION_BASE, PAGE_SIZE, boot_floor));
        assert!(write_allowed(CONFIG_BASE, PAGE_SIZE, boot_floor));
        assert!(!write_allowed(FLASH_END - 8, 16, boot_floor), "past the end");
        assert!(!write_allowed(u32::MAX - 3, 8, boot_floor), "overflow");
        assert!(!write_allowed(APP_REGION_BASE, 0, boot_floor));
        // App image: config page only.
        assert!(!write_allowed(APP_REGION_BASE, 8, CONFIG_BASE));
        assert!(!write_allowed(CONFIG_BASE - 8, 8, CONFIG_BASE));
        assert!(write_allowed(CONFIG_BASE, 8, CONFIG_BASE));
        // A bogus (too low) floor can never open the bootloader.
        for floor in [0, FLASH_BASE, FLASH_BASE + BOOT_SIZE - 8] {
            for page in 0..BOOT_PAGES {
                assert!(!write_allowed(FLASH_BASE + page * PAGE_SIZE, 8, floor));
            }
        }
    }

    #[test]
    fn wrp_decision_never_loops() {
        assert_eq!(WRP1AR_BOOT, 0x0004_0000);
        assert!(wrp_covers_boot(WRP1AR_BOOT));
        assert!(!wrp_covers_boot(0x0000_000F), "factory default: STRT > END");
        assert!(!wrp_covers_boot(0x0003_0000), "too short");
        assert!(wrp_covers_boot(0x000F_0000), "larger area is fine");
        assert!(!wrp_covers_boot(0x0004_0001), "must start at page 0");
        // Power-on: try once.
        let mut s = latch(garbage(), csr2::PWRRSTF);
        assert_eq!(s.flags, 0);
        assert_eq!(wrp_action(0xF, &s), WrpAction::Program);
        s.flags |= BootShared::FLAG_WRP_TRIED;
        // The option-byte reload reset keeps the flag...
        let s = latch(s, csr2::OBLRSTF | csr2::PINRSTF);
        assert_eq!(wrp_action(WRP1AR_BOOT, &s), WrpAction::Protected);
        assert_eq!(wrp_action(0xF, &s), WrpAction::GiveUp, "failed attempt: no second reset");
        // ...and so do watchdog resets; only a power cycle clears it.
        let s = latch(s, csr2::IWDGRSTF);
        assert_eq!(wrp_action(0xF, &s), WrpAction::GiveUp);
        let s = latch(s, csr2::PWRRSTF);
        assert_eq!(wrp_action(0xF, &s), WrpAction::Program);
    }

    #[test]
    fn power_on_initialises() {
        let s = latch(garbage(), csr2::PWRRSTF | csr2::PINRSTF);
        assert_eq!(s.magic, BootShared::MAGIC);
        assert_eq!((s.request, s.boot_attempts, s.resets), (0, 0, 0));
        assert_eq!(s.reset_reason, ResetReason::PowerOn as u8);
    }

    #[test]
    fn warm_resets_count_and_keep_state() {
        let s0 = latch(garbage(), csr2::PWRRSTF);
        let mut s1 = latch(s0, csr2::SFTRSTF | csr2::PINRSTF);
        assert_eq!(s1.resets, 1);
        assert_eq!(s1.reset_reason, ResetReason::Software as u8);
        s1.request = BootShared::REQ_STAY_IN_BOOT;
        s1.boot_attempts = 2;
        let s2 = latch(s1, csr2::IWDGRSTF | csr2::PINRSTF);
        assert_eq!(s2.resets, 2);
        assert_eq!(s2.request, BootShared::REQ_STAY_IN_BOOT);
        assert_eq!(s2.boot_attempts, 2);
        assert_eq!(s2.reset_reason, ResetReason::Watchdog as u8);
        let s3 = latch(s2, csr2::PINRSTF);
        assert_eq!(s3.reset_reason, ResetReason::Pin as u8);
        // POR flag with retained RAM: brownout, state reset.
        let s4 = latch(s3, csr2::PWRRSTF | csr2::PINRSTF);
        assert_eq!(s4.reset_reason, ResetReason::Brownout as u8);
        assert_eq!((s4.resets, s4.request), (0, 0));
    }

    #[test]
    fn decision_rules() {
        let mut s = latch(garbage(), csr2::PWRRSTF);
        assert_eq!(decide(false, &s), Decision::Recovery);
        assert_eq!(decide(true, &s), Decision::ListenThenBoot);
        s.boot_attempts = 2;
        assert_eq!(decide(true, &s), Decision::ListenThenBoot);
        s.boot_attempts = 3;
        assert_eq!(decide(true, &s), Decision::Recovery);
        s.boot_attempts = 0;
        s.request = BootShared::REQ_STAY_IN_BOOT;
        assert_eq!(decide(true, &s), Decision::Recovery);
    }
}
