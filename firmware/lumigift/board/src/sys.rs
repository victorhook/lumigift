//! Reset flags, `BootShared`, UID, reset, panic and fault handlers.

use lumigift_proto::layout::{BOOT_SHARED_ADDR, BootShared};

use crate::pac::RCC;

/// Read the RCC_CSR2 reset flags and clear them (RMVF).
pub fn take_reset_flags() -> u32 {
    let v = RCC.csr2().read().0;
    RCC.csr2().modify(|w| w.set_rmvf(true));
    v
}

/// `BootShared` in the 16 bytes of RAM excluded from both linker scripts.
#[inline(never)]
pub fn shared() -> BootShared {
    // SAFETY: BOOT_SHARED_ADDR is reserved RAM, aligned, never used by Rust objects.
    unsafe { core::ptr::read_volatile(BOOT_SHARED_ADDR as *const BootShared) }
}

#[inline(never)]
pub fn set_shared(s: &BootShared) {
    // SAFETY: see `shared`.
    unsafe { core::ptr::write_volatile(BOOT_SHARED_ADDR as *mut BootShared, *s) }
}

#[inline(always)]
pub fn update_shared(f: impl FnOnce(&mut BootShared)) {
    let mut s = shared();
    f(&mut s);
    set_shared(&s);
}

/// 96-bit unique device ID (RM0490 §37.1, 0x1FFF_7550).
pub fn uid() -> [u8; 12] {
    let mut u = [0u8; 12];
    crate::flash::read(crate::pac::UID.as_ptr() as u32, &mut u);
    u
}

pub fn reset() -> ! {
    cortex_m::peripheral::SCB::sys_reset()
}

#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    reset()
}

#[cortex_m_rt::exception]
unsafe fn HardFault(_: &cortex_m_rt::ExceptionFrame) -> ! {
    reset()
}
