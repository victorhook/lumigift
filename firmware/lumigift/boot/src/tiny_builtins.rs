//! Size-optimised replacements for the compiler-builtins routines the
//! bootloader pulls in. Only the bootloader links this module.
//!
//! compiler-builtins ships speed-optimised versions (`memcpy` ~560 B,
//! `__aeabi_memclr4` ~160 B, the u32 divider ~400 B) that the 10 KiB
//! bootloader cannot afford; its copies and divisions are tiny and rare.
//! compiler-builtins defines these symbols weak, so these strong
//! definitions win at link time. Loops use volatile accesses so LLVM cannot
//! turn them back into calls to themselves.

use core::ffi::c_void;
use core::ptr::{read_volatile, write_volatile};

/// Restoring shift-subtract division: (quotient, remainder).
/// Division by zero returns (u32::MAX, n) instead of trapping.
#[inline(never)]
fn udivmod(n: u32, d: u32) -> (u32, u32) {
    if d == 0 {
        return (u32::MAX, n);
    }
    let mut q = 0u32;
    let mut r = 0u32;
    let mut i = 32;
    while i > 0 {
        i -= 1;
        let carry = r >> 31;
        r = (r << 1) | ((n >> i) & 1);
        if carry != 0 || r >= d {
            r = r.wrapping_sub(d);
            q |= 1 << i;
        }
    }
    (q, r)
}

#[unsafe(no_mangle)]
pub extern "C" fn __aeabi_uidiv(n: u32, d: u32) -> u32 {
    udivmod(n, d).0
}

/// AAPCS returns the 64-bit value in r0 (quotient) : r1 (remainder),
/// which is exactly the `__aeabi_uidivmod` contract.
#[unsafe(no_mangle)]
pub extern "C" fn __aeabi_uidivmod(n: u32, d: u32) -> u64 {
    let (q, r) = udivmod(n, d);
    q as u64 | (r as u64) << 32
}

unsafe fn copy(dst: *mut u8, src: *const u8, n: usize) {
    let mut i = 0;
    while i < n {
        // SAFETY: caller guarantees valid, non-overlapping ranges.
        unsafe { write_volatile(dst.add(i), read_volatile(src.add(i))) };
        i += 1;
    }
}

unsafe fn fill(dst: *mut u8, n: usize, c: u8) {
    let mut i = 0;
    while i < n {
        // SAFETY: caller guarantees a valid range.
        unsafe { write_volatile(dst.add(i), c) };
        i += 1;
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn __aeabi_memcpy(dst: *mut u8, src: *const u8, n: usize) {
    unsafe { copy(dst, src, n) }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __aeabi_memcpy4(dst: *mut u8, src: *const u8, n: usize) {
    unsafe { copy(dst, src, n) }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __aeabi_memcpy8(dst: *mut u8, src: *const u8, n: usize) {
    unsafe { copy(dst, src, n) }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn memcpy(dst: *mut c_void, src: *const c_void, n: usize) -> *mut c_void {
    unsafe { copy(dst.cast(), src.cast(), n) };
    dst
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __aeabi_memclr(dst: *mut u8, n: usize) {
    unsafe { fill(dst, n, 0) }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __aeabi_memclr4(dst: *mut u8, n: usize) {
    unsafe { fill(dst, n, 0) }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __aeabi_memclr8(dst: *mut u8, n: usize) {
    unsafe { fill(dst, n, 0) }
}
/// Note the AEABI argument order: (dest, n, c).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __aeabi_memset(dst: *mut u8, n: usize, c: i32) {
    unsafe { fill(dst, n, c as u8) }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __aeabi_memset4(dst: *mut u8, n: usize, c: i32) {
    unsafe { fill(dst, n, c as u8) }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn memset(dst: *mut c_void, c: i32, n: usize) -> *mut c_void {
    unsafe { fill(dst.cast(), n, c as u8) };
    dst
}
