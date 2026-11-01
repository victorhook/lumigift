//! Flash erase / double-word programming (RM0490 §3.3.6-3.3.8), the write
//! guard, and the bootloader write protection (option bytes, §3.4).
//!
//! # Write guard
//!
//! Every erase/program checks the target against the image's *write
//! floor*: the linker-script symbol `__lumigift_flash_floor` (bootloader:
//! `APP_REGION_BASE`, app: `CONFIG_BASE`), and never below
//! `APP_REGION_BASE` whatever the symbol says. So the app can only touch
//! the config page and nothing can touch the bootloader. The bootloader
//! pages are additionally write-protected in hardware (WRP area A, see
//! [`ensure_boot_protected`]), which also stops code that bypasses this
//! driver.
//!
//! # Stalls and RAM functions
//!
//! Code runs from flash, and any flash read stalls while an erase (22 ms
//! typ, 40 ms max) or a double-word program (~85 µs) runs. So the whole
//! start-and-wait sequence of every erase/program runs from RAM
//! (`.data.ramfunc.*`, copied there by cortex-m-rt's startup code like any
//! `.data`). The `*_draining` variants (bootloader OTA) keep moving packets
//! from the nRF RX FIFO into [`Rings`] while the flash is busy, so
//! reception overlaps erasing and programming; the plain [`erase_page`] /
//! [`program`] (config writes) just wait. The RAM code only touches
//! peripheral registers and RAM: `cargo xtask size` checks that it contains
//! no calls back into flash.

use lumigift_logic::boot::{WRP1AR_BOOT, WrpAction, write_allowed, wrp_action, wrp_covers_boot};
use lumigift_logic::ota::{RegionFlash, RegionRead};
use lumigift_proto::layout::{
    APP_REGION_BASE, APP_REGION_SIZE, BootShared, FLASH_BASE, PAGE_SIZE, WRITE_SIZE, page_of,
};

use crate::fastrx::Rings;
use crate::pac::FLASH;

const KEY1: u32 = 0x4567_0123;
const KEY2: u32 = 0xCDEF_89AB;
const OPTKEY1: u32 = 0x0819_2A3B;
const OPTKEY2: u32 = 0x4C5D_6E7F;
/// OPERR, PROGERR, WRPERR, PGAERR, SIZERR, PGSERR, MISSERR, FASTERR, RDERR, OPTVERR.
const SR_ERRORS: u32 = 0xC3FA;
const SR_EOP: u32 = 1;
const SR_BSY: u32 = 1 << 16;
const SR_CFGBSY: u32 = 1 << 18;

// Raw register addresses for the RAM functions (no PAC calls there).
const FLASH_SR: *mut u32 = 0x4002_2010 as *mut u32;
const FLASH_CR: *mut u32 = 0x4002_2014 as *mut u32;
const FLASH_WRP1AR: *mut u32 = 0x4002_202C as *mut u32;
const CR_PG: u32 = 1 << 0;
const CR_PER: u32 = 1 << 1;
const CR_PNB_SHIFT: u32 = 3;
const CR_STRT: u32 = 1 << 16;
const CR_OPTSTRT: u32 = 1 << 17;
const CR_OBL_LAUNCH: u32 = 1 << 27;

unsafe extern "C" {
    /// Lowest address this image may erase/program (defined in memory.x).
    static __lumigift_flash_floor: u8;
}

/// May this image erase/program `len` bytes at `addr` (see the module docs)?
fn allowed(addr: u32, len: u32) -> bool {
    // SAFETY: only the symbol's address is used.
    let floor = core::ptr::addr_of!(__lumigift_flash_floor) as u32;
    write_allowed(addr, len, floor)
}

fn unlock() {
    if FLASH.cr().read().lock() {
        FLASH.keyr().write_value(KEY1);
        FLASH.keyr().write_value(KEY2);
    }
}

fn lock() {
    FLASH.cr().modify(|w| w.set_lock(true));
}

fn wait_idle() {
    while sr() & (SR_BSY | SR_CFGBSY) != 0 {}
}

/// Unlock and wait for any previous operation (the RAM functions clear the
/// error flags).
fn prepare() {
    unlock();
    wait_idle();
}

/// Erase flash page `page` (0..16), stalling until done. Refused (false)
/// outside the write floor.
pub fn erase_page(page: u32) -> bool {
    erase_with(page, core::ptr::null_mut())
}

/// Program one double word at the 8-aligned absolute address `addr`, which
/// must be erased. Verifies by reading back. Refused outside the write floor.
pub fn program(addr: u32, dw: [u8; 8]) -> bool {
    program_with(addr, dw, core::ptr::null_mut())
}

fn erase_with(page: u32, rings: *mut Rings) -> bool {
    if !allowed(FLASH_BASE + page * PAGE_SIZE, PAGE_SIZE) {
        return false;
    }
    prepare();
    // SAFETY: unlocked and idle; `rings` is null or exclusively ours.
    let sr = unsafe { ram_erase(page, rings) };
    lock();
    flush_icache();
    sr & SR_ERRORS == 0
}

fn program_with(addr: u32, dw: [u8; 8], rings: *mut Rings) -> bool {
    if !addr.is_multiple_of(WRITE_SIZE) || !allowed(addr, WRITE_SIZE) {
        return false;
    }
    let lo = u32::from_le_bytes([dw[0], dw[1], dw[2], dw[3]]);
    let hi = u32::from_le_bytes([dw[4], dw[5], dw[6], dw[7]]);
    prepare();
    // SAFETY: unlocked and idle; addr is 8-aligned, allowed and erased.
    let sr = unsafe { ram_program(addr, lo, hi, rings) };
    lock();
    // SAFETY: reading back flash we just programmed.
    let back = unsafe { core::ptr::read_volatile(addr as *const [u32; 2]) };
    sr & SR_ERRORS == 0 && back == [lo, hi]
}

/// Read `out.len()` bytes of flash at `addr`.
pub fn read(addr: u32, out: &mut [u8]) {
    for (i, b) in out.iter_mut().enumerate() {
        // SAFETY: flash is always readable.
        *b = unsafe { core::ptr::read_volatile((addr as usize + i) as *const u8) };
    }
}

/// Drop instruction cache lines after flash contents changed.
fn flush_icache() {
    FLASH.acr().modify(|w| w.set_icen(false));
    FLASH.acr().modify(|w| w.set_icrst(true));
    FLASH.acr().modify(|w| w.set_icrst(false));
    FLASH.acr().modify(|w| w.set_icen(true));
}

// ---------------------------------------------------------------------------
// RAM-resident erase/program, optionally keeping the radio drained
// ---------------------------------------------------------------------------

#[inline(always)]
fn sr() -> u32 {
    // SAFETY: FLASH_SR is a peripheral register.
    unsafe { core::ptr::read_volatile(FLASH_SR) }
}

/// Wait for the flash from RAM; drains the radio meanwhile if `rings` is set.
#[inline(always)]
unsafe fn ram_wait(rings: *mut Rings) {
    while sr() & (SR_BSY | SR_CFGBSY) != 0 {
        if !rings.is_null() {
            // SAFETY: caller guarantees exclusive access.
            crate::fastrx::drain(unsafe { &mut *rings });
        }
    }
}

/// Erase page `page` from RAM. Returns FLASH_SR at the end. The flash must
/// be unlocked and idle.
#[inline(never)]
#[unsafe(link_section = ".data.ramfunc.erase")]
unsafe fn ram_erase(page: u32, rings: *mut Rings) -> u32 {
    // SAFETY (whole body): FLASH_CR/SR writes per RM0490 §3.3.7, from RAM.
    unsafe {
        core::ptr::write_volatile(FLASH_SR, SR_ERRORS | SR_EOP);
        let cr = core::ptr::read_volatile(FLASH_CR) & !(CR_PG | CR_PER | (0x3F << CR_PNB_SHIFT));
        core::ptr::write_volatile(FLASH_CR, cr | CR_PER | (page << CR_PNB_SHIFT));
        core::ptr::write_volatile(FLASH_CR, cr | CR_PER | (page << CR_PNB_SHIFT) | CR_STRT);
        ram_wait(rings);
        core::ptr::write_volatile(FLASH_CR, core::ptr::read_volatile(FLASH_CR) & !CR_PER);
    }
    sr()
}

/// Program one double word from RAM. Interrupts are masked around the two
/// word writes (an interrupt handler running from flash must not slip in
/// between them); the bootloader runs without interrupts anyway.
#[inline(never)]
#[unsafe(link_section = ".data.ramfunc.program")]
unsafe fn ram_program(addr: u32, lo: u32, hi: u32, rings: *mut Rings) -> u32 {
    // SAFETY (whole body): RM0490 §3.3.8; both words written back to back.
    unsafe {
        core::ptr::write_volatile(FLASH_SR, SR_ERRORS | SR_EOP);
        core::ptr::write_volatile(FLASH_CR, (core::ptr::read_volatile(FLASH_CR) & !CR_PER) | CR_PG);
        let primask: u32;
        core::arch::asm!("mrs {}, PRIMASK", "cpsid i", out(reg) primask, options(nomem, nostack, preserves_flags));
        core::ptr::write_volatile(addr as *mut u32, lo);
        core::ptr::write_volatile((addr + 4) as *mut u32, hi);
        if primask & 1 == 0 {
            core::arch::asm!("cpsie i", options(nomem, nostack, preserves_flags));
        }
        ram_wait(rings);
        core::ptr::write_volatile(FLASH_CR, core::ptr::read_volatile(FLASH_CR) & !CR_PG);
    }
    sr()
}

/// The app region (`ImageHeader` + app) for the OTA logic; erases and
/// programs keep the radio drained into the rings.
pub struct AppRegion<'a>(pub &'a mut Rings);

impl RegionRead for AppRegion<'_> {
    fn read(&self, offset: u32, out: &mut [u8]) {
        AppRegionRead.read(offset, out)
    }
    fn crc32(&self, offset: u32, len: u32) -> u32 {
        AppRegionRead.crc32(offset, len)
    }
}

impl RegionFlash for AppRegion<'_> {
    fn erase_page(&mut self, page: u32) -> bool {
        erase_with(page_of(APP_REGION_BASE) + page, self.0)
    }
    fn program(&mut self, offset: u32, dw: [u8; 8]) -> bool {
        program_with(APP_REGION_BASE + offset, dw, self.0)
    }
}

/// Read-only view of the app region (validation at boot, commit).
pub struct AppRegionRead;

impl RegionRead for AppRegionRead {
    fn read(&self, offset: u32, out: &mut [u8]) {
        read(APP_REGION_BASE + offset, out)
    }
    fn crc32(&self, offset: u32, len: u32) -> u32 {
        let len = len.min(APP_REGION_SIZE.saturating_sub(offset));
        // SAFETY: the range lies inside the app region (clamped above).
        let s = unsafe { core::slice::from_raw_parts((APP_REGION_BASE + offset) as *const u8, len as usize) };
        crate::crc::crc32(s)
    }
}

// ---------------------------------------------------------------------------
// Bootloader write protection (WRP area A, option bytes)
// ---------------------------------------------------------------------------

/// Are the bootloader pages write-protected?
pub fn boot_protected() -> bool {
    // SAFETY: FLASH_WRP1AR is readable at any time.
    wrp_covers_boot(unsafe { core::ptr::read_volatile(FLASH_WRP1AR) })
}

/// Make sure WRP area A covers the bootloader pages (bootloader only, at
/// boot). If it does not and this power cycle has not tried yet, mark the
/// attempt in `BootShared`, program `FLASH_WRP1AR` and launch the option
/// byte reload, which resets the chip (RM0490 §3.4.2: unlock CR, unlock
/// OPTLOCK with OPTKEYR, write the option register, OPTSTRT, wait BSY,
/// OBL_LAUNCH). Only WRP1AR is changed; every other option register is
/// written back with the value loaded at reset. Returns whether the
/// bootloader is protected (it does not return when the reload starts).
///
/// Remove the protection over SWD (RDP level 0) with
/// `STM32_Programmer_CLI -c port=SWD mode=UR -ob WRP1A_STRT=0xF WRP1A_END=0x0`
/// (`cargo xtask unprotect`).
pub fn ensure_boot_protected(shared: &BootShared) -> bool {
    // SAFETY: FLASH_WRP1AR is readable at any time.
    let current = unsafe { core::ptr::read_volatile(FLASH_WRP1AR) };
    match wrp_action(current, shared) {
        WrpAction::Protected => return true,
        WrpAction::GiveUp => return false,
        WrpAction::Program => {}
    }
    crate::sys::update_shared(|s| s.flags |= BootShared::FLAG_WRP_TRIED);
    prepare();
    // SAFETY: FLASH_SR write-1-to-clear.
    unsafe { core::ptr::write_volatile(FLASH_SR, SR_ERRORS | SR_EOP) };
    if FLASH.cr().read().optlock() {
        FLASH.optkeyr().write_value(OPTKEY1);
        FLASH.optkeyr().write_value(OPTKEY2);
    }
    if FLASH.cr().read().optlock() {
        lock();
        return false;
    }
    // SAFETY: option register write with OPTLOCK cleared (RM0490 §3.4.2).
    unsafe {
        core::ptr::write_volatile(FLASH_WRP1AR, (current & !0x003F_003F) | WRP1AR_BOOT);
        core::ptr::write_volatile(FLASH_CR, core::ptr::read_volatile(FLASH_CR) | CR_OPTSTRT);
    }
    wait_idle();
    if sr() & SR_ERRORS != 0 {
        lock(); // also sets OPTLOCK
        return false;
    }
    // Reload the option bytes: this resets the chip. BootShared (RAM) keeps
    // FLAG_WRP_TRIED, so a protection that does not stick costs at most this
    // one reset per power cycle.
    // SAFETY: as above.
    unsafe { core::ptr::write_volatile(FLASH_CR, core::ptr::read_volatile(FLASH_CR) | CR_OBL_LAUNCH) };
    loop {
        cortex_m::asm::nop();
    }
}
