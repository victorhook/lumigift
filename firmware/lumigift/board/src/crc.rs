//! CRC-32/ISO-HDLC on the CRC unit (poly 0x04C11DB7, init 0xFFFFFFFF,
//! byte-wise input bit reversal, reversed output, final XOR in software),
//! matching `lumigift_proto::image::crc32`. A one-time self-test against
//! the check value falls back to the software CRC if the configuration is
//! ever wrong, so a silicon/config surprise can only cost speed.

use core::sync::atomic::{AtomicU8, Ordering};

use crate::pac::crc::vals::{Polysize, RevIn, RevOut};
use crate::pac::{CRC, RCC};

static HW_OK: AtomicU8 = AtomicU8::new(0); // 0 unknown, 1 ok, 2 broken

fn hw(data: &[u8]) -> u32 {
    RCC.ahbenr().modify(|w| w.set_crcen(true));
    CRC.pol().write_value(0x04C1_1DB7);
    CRC.init().write_value(0xFFFF_FFFF);
    CRC.cr().write(|w| {
        w.set_polysize(Polysize::POLYSIZE32);
        w.set_rev_in(RevIn::BYTE);
        w.set_rev_out(RevOut::REVERSED);
        w.set_reset(true);
    });
    for &b in data {
        CRC.dr8().write_value(b);
    }
    CRC.dr32().read() ^ 0xFFFF_FFFF
}

pub fn crc32(data: &[u8]) -> u32 {
    if HW_OK.load(Ordering::Relaxed) == 0 {
        let ok = hw(b"123456789") == 0xCBF4_3926;
        HW_OK.store(if ok { 1 } else { 2 }, Ordering::Relaxed);
    }
    if HW_OK.load(Ordering::Relaxed) == 1 { hw(data) } else { lumigift_proto::image::crc32(data) }
}
