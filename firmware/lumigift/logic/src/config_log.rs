//! Append-only [`ConfigRecord`] log in the config page.
//!
//! The page holds 128 slots of 16 bytes. Erased slots read all-ones. The
//! last slot holding a valid record (magic + CRC) is current; a torn write
//! leaves an invalid, non-erased slot that is skipped. New records go to the
//! slot after the last non-erased one; when the page is full it is erased
//! and the record written to slot 0.

use lumigift_proto::layout::{CONFIG_SIZE, ConfigRecord};
use lumigift_proto::{MAX_SLOTS, UNASSIGNED_SLOT, config_key, valid_node_addr};

pub const SLOTS: usize = CONFIG_SIZE as usize / ConfigRecord::SIZE;

/// Smallest accepted power budget (the four LEDs' quiescent draw is ~4 mA).
pub const MIN_POWER_BUDGET_MA: u16 = 20;
pub const MAX_POWER_BUDGET_MA: u16 = 2000;
/// Highest nRF24 channel.
pub const MAX_CHANNEL: u8 = 125;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Scan {
    pub current: Option<ConfigRecord>,
    /// Slot for the next append; [`SLOTS`] when the page is full.
    pub next_free: usize,
}

/// Scan the page; `read(i)` returns slot `i`.
pub fn scan(read: impl Fn(usize) -> [u8; 16]) -> Scan {
    let mut s = Scan { current: None, next_free: 0 };
    for i in 0..SLOTS {
        let b = read(i);
        if b != [0xFF; 16] {
            s.next_free = i + 1;
            if let Some(r) = ConfigRecord::from_bytes(&b) {
                s.current = Some(r);
            }
        }
    }
    s
}

/// Where to write the next record: `(erase_first, slot)`.
pub fn next_slot(next_free: usize) -> (bool, usize) {
    if next_free >= SLOTS { (true, 0) } else { (false, next_free) }
}

/// Clamp a loaded record into the ranges the firmware relies on.
pub fn sanitize(mut c: ConfigRecord, defaults: &ConfigRecord) -> ConfigRecord {
    if !valid_node_addr(c.node_addr) {
        c.node_addr = defaults.node_addr;
    }
    if c.slot >= MAX_SLOTS && c.slot != UNASSIGNED_SLOT {
        c.slot = UNASSIGNED_SLOT;
    }
    if c.channel > MAX_CHANNEL {
        c.channel = defaults.channel;
    }
    c.idle_timeout_100ms = c.idle_timeout_100ms.max(1);
    c.power_budget_ma = c.power_budget_ma.clamp(MIN_POWER_BUDGET_MA, MAX_POWER_BUDGET_MA);
    c.seal();
    c
}

/// Apply a `SetConfig` key. Returns the effective (clamped) value, or
/// `None` for an unknown key. The record is resealed.
pub fn apply_key(c: &mut ConfigRecord, key: u8, value: u16) -> Option<u16> {
    let eff = match key {
        config_key::MAX_BRIGHTNESS => {
            c.max_brightness = value.min(255) as u8;
            c.max_brightness as u16
        }
        config_key::IDLE_TIMEOUT_MS => {
            c.idle_timeout_100ms = (value / 100).clamp(1, 255) as u8;
            c.idle_timeout_100ms as u16 * 100
        }
        config_key::POWER_BUDGET_MA => {
            c.power_budget_ma = value.clamp(MIN_POWER_BUDGET_MA, MAX_POWER_BUDGET_MA);
            c.power_budget_ma
        }
        config_key::CHANNEL => {
            if value > MAX_CHANNEL as u16 {
                return None;
            }
            c.channel = value as u8;
            value
        }
        config_key::VBAT_GAIN => {
            c.vbat_gain = value as i16;
            value
        }
        config_key::VBAT_OFFSET_MV => {
            c.vbat_offset_2mv = ((value as i16) / 2).clamp(-127, 127) as i8;
            (c.vbat_offset_2mv as i16 * 2) as u16
        }
        _ => return None,
    };
    c.seal();
    Some(eff)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vbat_calibration_keys_round_trip() {
        let mut c = ConfigRecord::defaults(&[7; 12]);
        assert_eq!(c.calibrate_vbat(1234), 1234, "uncalibrated by default");
        // Gain -3 % (as i16 bits), offset -25 mV (stored in 2 mV steps).
        let gain = (-0.03f64 * 32768.0).round() as i16;
        assert_eq!(apply_key(&mut c, config_key::VBAT_GAIN, gain as u16), Some(gain as u16));
        assert_eq!(apply_key(&mut c, config_key::VBAT_OFFSET_MV, (-25i16) as u16), Some((-24i16) as u16));
        let back = ConfigRecord::from_bytes(&c.to_bytes()).expect("valid record");
        assert_eq!((back.vbat_gain, back.vbat_offset_2mv), (gain, -12));
        // 1000 mV raw -> 1000 - 30 - 24.
        assert_eq!(back.calibrate_vbat(1000), 946);
    }

    fn page_with(records: &[[u8; 16]]) -> Vec<[u8; 16]> {
        let mut p = vec![[0xFF; 16]; SLOTS];
        p[..records.len()].copy_from_slice(records);
        p
    }

    #[test]
    fn empty_page() {
        let p = page_with(&[]);
        assert_eq!(scan(|i| p[i]), Scan { current: None, next_free: 0 });
        assert_eq!(next_slot(0), (false, 0));
    }

    #[test]
    fn last_valid_wins_and_torn_writes_are_skipped() {
        let a = ConfigRecord::defaults(&[1; 12]);
        let mut b = a;
        b.slot = 5;
        b.seal();
        let mut torn = b.to_bytes();
        torn[12] ^= 0xFF;
        let p = page_with(&[a.to_bytes(), b.to_bytes(), torn]);
        let s = scan(|i| p[i]);
        assert_eq!(s.current, Some(b));
        assert_eq!(s.next_free, 3);
    }

    #[test]
    fn full_page_requests_erase() {
        let a = ConfigRecord::defaults(&[2; 12]);
        let p = vec![a.to_bytes(); SLOTS];
        let s = scan(|i| p[i]);
        assert_eq!(s.next_free, SLOTS);
        assert_eq!(next_slot(s.next_free), (true, 0));
    }

    #[test]
    fn keys_clamp_and_seal() {
        let d = ConfigRecord::defaults(&[3; 12]);
        let mut c = d;
        assert_eq!(apply_key(&mut c, config_key::MAX_BRIGHTNESS, 999), Some(255));
        assert_eq!(apply_key(&mut c, config_key::IDLE_TIMEOUT_MS, 30_000), Some(25_500));
        assert_eq!(apply_key(&mut c, config_key::IDLE_TIMEOUT_MS, 50), Some(100));
        assert_eq!(apply_key(&mut c, config_key::POWER_BUDGET_MA, 1), Some(MIN_POWER_BUDGET_MA));
        assert_eq!(apply_key(&mut c, config_key::CHANNEL, 126), None);
        assert_eq!(apply_key(&mut c, config_key::CHANNEL, 90), Some(90));
        assert_eq!(apply_key(&mut c, 99, 1), None);
        assert_eq!(ConfigRecord::from_bytes(&c.to_bytes()), Some(c));
    }

    #[test]
    fn sanitize_fixes_ranges() {
        let d = ConfigRecord::defaults(&[4; 12]);
        let mut c = d;
        c.node_addr = 0;
        c.slot = 200;
        c.channel = 200;
        c.idle_timeout_100ms = 0;
        c.power_budget_ma = 0;
        let s = sanitize(c, &d);
        assert_eq!(s.node_addr, d.node_addr);
        assert_eq!(s.slot, UNASSIGNED_SLOT);
        assert_eq!(s.channel, d.channel);
        assert_eq!(s.idle_timeout_100ms, 1);
        assert_eq!(s.power_budget_ma, MIN_POWER_BUDGET_MA);
        assert!(ConfigRecord::from_bytes(&s.to_bytes()).is_some());
    }
}
