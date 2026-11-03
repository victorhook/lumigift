//! Flash map. Must agree with `memory.x` here and in the app.

/// 1 KiB pages (STM32F103 medium density).
pub const PAGE: u32 = 1024;
pub const FLASH_BASE: u32 = 0x0800_0000;
/// First byte of the app (its vector table).
pub const APP_BASE: u32 = 0x0800_4000;
/// 64 KiB parts. (Many C8s really have 128 KiB; the app never needs it.)
pub const FLASH_END: u32 = 0x0801_0000;
const RAM_START: u32 = 0x2000_0000;
const RAM_END: u32 = 0x2000_5000;

/// DfuSe memory layout string: bootloader pages read-only, app pages
/// readable, erasable and writable.
pub const MEM_INFO: &str = "@Flash/0x08000000/16*001Ka,48*001Kg";
const _: () = assert!((APP_BASE - FLASH_BASE) / PAGE == 16);
const _: () = assert!((FLASH_END - APP_BASE) / PAGE == 48);

/// `[addr, addr + len)` lies inside the app region.
pub fn in_app(addr: u32, len: u32) -> bool {
    addr >= APP_BASE && addr.checked_add(len).is_some_and(|end| end <= FLASH_END)
}

/// A plausible vector table at `APP_BASE`: the initial stack pointer points
/// into RAM and the reset vector is a Thumb address inside the app.
pub fn app_valid() -> bool {
    // SAFETY: both words are inside on-chip flash.
    let (sp, reset) = unsafe {
        (
            core::ptr::read_volatile(APP_BASE as *const u32),
            core::ptr::read_volatile((APP_BASE + 4) as *const u32),
        )
    };
    (RAM_START + 4..=RAM_END).contains(&sp) && sp % 4 == 0 && reset & 1 == 1 && in_app(reset & !1, 2)
}
