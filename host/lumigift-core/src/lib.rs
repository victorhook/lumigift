//! Lumigift link manager: everything between the GUI/CLI and the radio.
//!
//! A [`Link`] owns one bridge (USB via [`nrf24_usb`], or the built-in
//! [simulator](sim)) and runs:
//!
//! * **Show stream:** every 10 ms the current [`ShowOutput`] (set with
//!   [`Link::set_show`]) is broadcast with NO_ACK + bridge timestamp and an
//!   incrementing sequence number. Solid/Leds outputs for many slots become
//!   several packets per tick, and [`ShowOutput::Layers`] (one Music frame
//!   per group of gifts, selected by the frame's slot mask) sends every
//!   layer each tick (round-robin beyond [`MAX_LAYERS_PER_TICK`]). Show
//!   packets always go first and stale ones are dropped rather than queued.
//! * **Telemetry:** known gifts are polled round-robin between show frames;
//!   each poll's ack payload carries a `Status`. Per-gift link quality,
//!   battery, reboots, broadcast reception, histories, etc. end up in
//!   [`GiftView`]s inside the [`Snapshot`].
//! * **Discovery:** a full sweep of node addresses on connect, a slow
//!   background sweep (new gifts appear within ~10 s), [`Link::rescan`], and
//!   automatic resolution of node-address collisions.
//! * **Commands:** [`Link::blink`], [`Link::identify`], [`Link::set_slot`],
//!   [`Link::auto_assign_slots`], [`Link::reboot`], [`Link::set_config`],
//!   [`Link::set_channel`], plus host-side metadata ([`Link::rename`],
//!   [`Link::update_meta`]) persisted in `~/.config/lumigift/gifts.json`.
//! * **Firmware updates:** [`Link::start_ota`] / [`Link::cancel_ota`], with
//!   per-target progress in [`Snapshot::ota`].
//! * **Clock sync:** [`Link::clock`] maps the bridge's µs clock to host
//!   [`Instant`](std::time::Instant)s for scheduling audio against the lights.
//!
//! # Threading model
//!
//! * `lumigift-link`: the scheduler. It alone owns the [`Transport`](nrf24_usb::Transport),
//!   sends show frames, polls, and executes radio requests queued by
//!   operation threads, keeping the bridge queue short (low latency).
//! * `lumigift-discovery`: sweeps, re-identification, collision resolution.
//! * One short-lived thread per command / OTA (they block on the scheduler
//!   through channels, so their code is straight-line).
//! * `nrf24-usb-rx`: the USB reader (real bridge only).
//!
//! The GUI only touches lock-free things: [`Link::snapshot`] (an
//! `ArcSwap`, published ~20×/s), [`Link::set_show`] (also `ArcSwap`) and
//! [`Pending`] results it can poll every frame.
//!
//! # Example
//!
//! ```no_run
//! use lumigift_core::{Link, LinkConfig, ShowOutput, sim::SimConfig};
//! use lumigift_proto::{MusicFrame, Rgb};
//! use std::time::Duration;
//!
//! // Hardware: Link::open_usb(LinkConfig::default(), None)
//! let (link, sim) = Link::simulated(LinkConfig::default(), SimConfig::new(10));
//! link.wait_for(Duration::from_secs(5), |s| s.discovery.initial_sweep_done);
//!
//! for g in &link.snapshot().gifts {
//!     println!("{} slot {:?} {} mV {:.0}% link", g.label(), g.slot, g.vbat_mv, g.poll_success_pct);
//! }
//! link.auto_assign_slots(lumigift_core::SlotAssign::ByUid).wait()?;
//! link.set_show(ShowOutput::Music(MusicFrame { brightness: 200, effect: 1, ..Default::default() }));
//!
//! let first = link.snapshot().gifts[0].uid;
//! let mut pending = link.blink(first, Rgb::new(255, 0, 0), 3, Duration::from_millis(300));
//! // In a GUI: poll `pending.try_take()` each frame instead of waiting.
//! println!("{}", pending.wait_timeout(Duration::from_secs(5)).unwrap()?);
//!
//! // What would the hardware show right now?
//! for g in sim.gifts() {
//!     println!("{:?}: {:?}", g.slot, g.leds);
//! }
//! # Ok::<(), lumigift_core::LinkError>(())
//! ```

mod clock;
mod config;
mod link;
mod meta;
mod ops;
mod ota;
mod scheduler;
mod select;
pub mod sim;
mod state;
mod types;

pub use clock::ClockSync;
pub use config::{LinkConfig, MetadataLocation, OtaOptions, RadioSettings};
pub use link::Link;
pub use meta::default_path as default_metadata_path;
pub use types::*;

pub use lumigift_proto::{self as proto, MusicFrame, Rgb, config_key};
pub use nrf24_usb;
pub use nrf24_usb_proto::{DataRate, Power};
