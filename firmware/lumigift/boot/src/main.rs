//! Lumigift radio bootloader (docs/ARCHITECTURE.md, "Firmware update").
//!
//! 1. Latch the reset reason into `BootShared`, start the IWDG, make sure
//!    the bootloader pages are write-protected (once per power cycle; the
//!    option-byte reload resets the chip).
//! 2. Validate the image in the app region.
//! 3. No valid image, `REQ_STAY_IN_BOOT` or too many failed boots: recovery
//!    mode. Otherwise listen ~150 ms for `BootHold` on the default channel,
//!    then count the attempt and jump to the app.
//! 4. Recovery mode: `lumigift_logic::recovery::Recovery` (Poll /
//!    GetIdentity / Blink / Reboot, SetNodeAddr and the OTA receiver), with
//!    the radio drained into RAM rings even while the flash is busy.
#![no_std]
#![no_main]

mod tiny_builtins;

use cortex_m_rt::entry;
use lumigift_board::config::ConfigStore;
use lumigift_board::fastrx::{self, Rings};
use lumigift_board::flash::{self, AppRegion, AppRegionRead};
use lumigift_board::pac::RCC;
use lumigift_board::radio::{self, Radio};
use lumigift_board::{self as board, gpio, iwdg, sys, time};
use lumigift_logic::boot::{self, Decision};
use lumigift_logic::bootpkt::{self, BootPacket};
use lumigift_logic::channel::{self, ChannelPlan};
use lumigift_logic::ota::validate_image;
use lumigift_logic::recovery::{Action, BOOT_VERSION, Ctx, HOLD_WINDOW_MS, Recovery, Tick};
use lumigift_proto::layout::{APP_VECTORS, BootShared};
use lumigift_proto::{MAX_PAYLOAD, Reply};

/// Re-probe a missing radio / check a running one this often.
const RADIO_CHECK_MS: u32 = 1000;

#[unsafe(link_section = ".bootinfo")]
#[used]
static BOOT_INFO: [u32; 4] = [board::BOOT_INFO_MAGIC, BOOT_VERSION as u32, 0, 0];

/// RX rings filled by the radio drain (also from the RAM flash loops).
static mut RINGS: Rings = Rings::new();

#[entry]
fn main() -> ! {
    board::init();
    iwdg::start();

    let shared = boot::latch(sys::shared(), sys::take_reset_flags());
    sys::set_shared(&shared);
    // May reset the chip once per power cycle (option-byte reload).
    let protected = flash::ensure_boot_protected(&shared);
    let shared = sys::shared();

    // SAFETY: the only reference to RINGS, taken once; no interrupts are used.
    let rings = unsafe { &mut *core::ptr::addr_of_mut!(RINGS) };
    let header = validate_image(&AppRegionRead, board::HW_REV);
    let uid = sys::uid();
    // A missing or corrupt config record falls back to the defaults.
    let mut cfg = ConfigStore::load(&uid);
    let stored_channel = cfg.current.channel;

    let mut radio = None;
    let mut start_channel = stored_channel;
    if boot::decide(header.is_some(), &shared) == Decision::ListenThenBoot {
        // The hold window always listens on the default channel, whatever
        // the (app-written) config says.
        radio = radio::start(channel::hold_window_channel(), cfg.current.node_addr);
        let mut hold = false;
        if radio.is_some() {
            let t0 = time::now_ms();
            let mut buf = [0u8; MAX_PAYLOAD];
            while !hold && time::now_ms().wrapping_sub(t0) < HOLD_WINDOW_MS {
                fastrx::drain(rings);
                while let Some(n) = rings.bc.pop_into(&mut buf) {
                    hold |= bootpkt::decode(&buf[..n]) == BootPacket::BootHold;
                }
                rings.uni.clear();
            }
        }
        if !hold {
            sys::update_shared(|s| s.boot_attempts = s.boot_attempts.saturating_add(1));
            jump_to_app(radio);
        }
        // Rescued by a host on the default channel: stay there.
        start_channel = channel::hold_window_channel();
        rings.bc.clear();
    }

    let now = time::now_ms();
    let plan = ChannelPlan::new(stored_channel, start_channel, now);
    if radio.is_none() {
        radio = radio::start(plan.current(), cfg.current.node_addr);
    }
    let mut rec = Recovery::new(board::HW_REV, header, plan);
    let mut cx = Ctx {
        uid,
        hw_rev: board::HW_REV,
        node_addr: cfg.current.node_addr,
        slot: cfg.current.slot,
        reset_reason: shared.reset_reason,
        resets: shared.resets,
        protected,
    };
    if let Some(r) = radio.as_mut() {
        // Have a Status ready for the first poll.
        radio::preload(r, &Reply::Status(rec.status(&cx, now)));
    }

    let mut last_check = now;
    let mut buf = [0u8; MAX_PAYLOAD];
    loop {
        iwdg::feed();
        let now = time::now_ms();

        if let Some(r) = radio.as_mut() {
            fastrx::drain(rings);
            rec.ring_drops = rings.bc.dropped.wrapping_add(rings.uni.dropped);
            match rec.next_action(&rings.uni, &rings.bc) {
                Action::Unicast => {
                    if let Some(n) = rings.uni.pop_into(&mut buf) {
                        let reply = rec.on_unicast(&buf[..n], &rings.bc, now, &AppRegionRead, &cx);
                        radio::preload(r, &reply);
                    }
                }
                Action::Broadcast => {
                    if let Some(n) = rings.bc.pop_into(&mut buf)
                        && let Some(addr) = rec.on_broadcast(&buf[..n], now, &mut AppRegion(rings), &cx)
                    {
                        let mut c = cfg.current;
                        c.node_addr = addr;
                        if cfg.store(c) {
                            cx.node_addr = addr;
                            radio::set_node_addr(r, addr);
                        }
                    }
                }
                Action::Erase => rec.erase_ahead(&mut AppRegion(rings)),
                Action::Idle => {}
            }
            if now.wrapping_sub(last_check) > RADIO_CHECK_MS {
                last_check = now;
                if !radio::healthy(r) {
                    radio = radio::start(rec.channel.current(), cfg.current.node_addr);
                }
            }
        } else if now.wrapping_sub(last_check) > RADIO_CHECK_MS {
            last_check = now;
            radio = radio::start(rec.channel.current(), cfg.current.node_addr);
        }

        match rec.tick(now) {
            Tick::None => {}
            Tick::Channel(c) => {
                if let Some(r) = radio.as_mut() {
                    radio::set_channel(r, c);
                }
            }
            Tick::Boot => {
                if let Some(ch) = rec.channel_to_store(cfg.current.channel) {
                    let mut c = cfg.current;
                    c.channel = ch;
                    cfg.store(c);
                }
                sys::update_shared(|s| {
                    s.request = 0;
                    s.boot_attempts = 1;
                });
                jump_to_app(radio.take());
            }
            Tick::Reboot { into_bootloader } => {
                sys::update_shared(|s| {
                    if into_bootloader {
                        s.request = BootShared::REQ_STAY_IN_BOOT;
                    } else {
                        s.request = 0;
                        s.boot_attempts = 0; // give the app a fresh set of tries
                    }
                });
                sys::reset();
            }
        }

        gpio::led(rec.led(now, radio.is_some()));
    }
}

/// Hand over to the app: radio off, used peripherals reset and unclocked,
/// interrupts cleared, VTOR + MSP from the app's vector table, branch to its
/// reset handler. The IWDG keeps running (it cannot be stopped); SYSCLK
/// stays at 48 MHz (the app sets the same clock tree again).
fn jump_to_app(radio: Option<Radio>) -> ! {
    if let Some(mut r) = radio {
        radio::shutdown(&mut r);
    }
    gpio::led(false);
    cortex_m::interrupt::disable();

    RCC.gpiorstr().write(|w| {
        w.set_gpioarst(true);
        w.set_gpiobrst(true);
        w.set_gpiocrst(true);
    });
    RCC.gpiorstr().write(|_| {});
    RCC.apbrstr2().write(|w| {
        w.set_syscfgrst(true);
        w.set_spi1rst(true);
        w.set_tim14rst(true);
    });
    RCC.apbrstr2().write(|_| {});
    RCC.ahbrstr().write(|w| w.set_crcrst(true));
    RCC.ahbrstr().write(|_| {});
    RCC.gpioenr().write(|_| {});
    RCC.apbenr2().write(|_| {});
    RCC.ahbenr().modify(|w| w.set_crcen(false));

    // SAFETY: single-threaded hand-over; nothing of this image runs afterwards.
    unsafe {
        let p = cortex_m::Peripherals::steal();
        p.SYST.csr.write(0);
        p.NVIC.icer[0].write(0xFFFF_FFFF);
        p.NVIC.icpr[0].write(0xFFFF_FFFF);
        p.SCB.vtor.write(APP_VECTORS);
        cortex_m::asm::dsb();
        cortex_m::asm::isb();
        // All interrupt sources are masked in the NVIC; re-enable PRIMASK so
        // the app starts with interrupts globally enabled like after reset.
        cortex_m::interrupt::enable();
        cortex_m::asm::bootload(APP_VECTORS as *const u32)
    }
}
