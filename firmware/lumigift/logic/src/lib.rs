//! Hardware-independent parts of the Lumigift firmware.
//!
//! Everything here is pure logic over plain data (or over the small
//! [`ota::RegionFlash`] trait), so it is unit-tested on the host with
//! `cargo test-host` while the bootloader and app link the same code.
#![cfg_attr(not(test), no_std)]

pub mod adc;
pub mod blink;
pub mod boot;
pub mod bootpkt;
pub mod channel;
pub mod clock;
pub mod config_log;
pub mod ota;
pub mod recovery;
pub mod rxring;
pub mod seq;
pub mod sync;
