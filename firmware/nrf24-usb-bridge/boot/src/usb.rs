//! DFU mode: USB device with one DfuSe interface over the app region.

use core::sync::atomic::{AtomicBool, Ordering};

use cortex_m::peripheral::{DWT, SCB};
use nrf24_usb_proto::{USB_VID, dfu};
use stm32_metapac::RCC;
use stm32_usbd::{MemoryAccess, UsbBus};
use usb_device::prelude::*;
use usbd_dfu::{DFUClass, DFUManifestationError, DFUMemError, DFUMemIO};

use crate::layout::{self, APP_BASE, FLASH_BASE, FLASH_END, PAGE};
use crate::{flash, hw};

/// bcdDevice of the bootloader.
const BOOT_VERSION: u16 = 0x0100;
const TRANSFER: usize = 256;

/// Set once a new app is in place: reset into it shortly (after the host has
/// had its last GETSTATUS answered).
static REBOOT: AtomicBool = AtomicBool::new(false);

struct Usb;

// SAFETY: addresses and memory layout of the STM32F103 USB peripheral
// (RM0008 §23); the packet memory is 512 B of 16-bit words on a 32-bit grid.
unsafe impl stm32_usbd::UsbPeripheral for Usb {
    const REGISTERS: *const () = 0x4000_5C00 as *const ();
    const DP_PULL_UP_FEATURE: bool = false;
    const EP_MEMORY: *const () = 0x4000_6000 as *const ();
    const EP_MEMORY_SIZE: usize = 512;
    const EP_MEMORY_ACCESS: MemoryAccess = MemoryAccess::Word16x1;

    fn enable() {
        RCC.apb1enr().modify(|w| w.set_usben(true));
        RCC.apb1rstr().modify(|w| w.set_usbrst(true));
        RCC.apb1rstr().modify(|w| w.set_usbrst(false));
    }

    fn startup_delay() {
        cortex_m::asm::delay(72); // t_STARTUP = 1 µs
    }
}

pub fn run(app_ok: bool) -> ! {
    let sysclk = hw::clocks();
    hw::led_init();
    // SAFETY: the only use of the core peripherals in DFU mode.
    let mut cp = unsafe { cortex_m::Peripherals::steal() };
    cp.DCB.enable_trace();
    cp.DWT.enable_cycle_counter();
    hw::usb_reenumerate(sysclk);

    let bus = UsbBus::new(Usb);
    let mut dfu = DFUClass::new(&bus, Flash::new());
    let mut dev = UsbDeviceBuilder::new(&bus, UsbVidPid(USB_VID, dfu::USB_PID))
        .strings(&[StringDescriptors::default()
            .manufacturer("nrf24-usb")
            .product(dfu::USB_PRODUCT)
            .serial_number(hw::serial())])
        .unwrap()
        .device_release(BOOT_VERSION)
        .max_packet_size_0(64)
        .unwrap()
        .build();

    // Milliseconds since entering DFU mode, from the cycle counter.
    let per_ms = sysclk / 1000;
    let (mut last, mut ms) = (DWT::cycle_count(), 0u32);
    let mut reboot_since = None;
    loop {
        dev.poll(&mut [&mut dfu]);

        let dt = DWT::cycle_count().wrapping_sub(last);
        ms = ms.wrapping_add(dt / per_ms);
        last = last.wrapping_add(dt / per_ms * per_ms);
        let phase = ms % 1000;
        hw::led(if app_ok { phase < 60 || (180..240).contains(&phase) } else { phase % 200 < 100 });

        // Give the host's last GETSTATUS time to complete, then start the app.
        if REBOOT.load(Ordering::Relaxed) && ms.wrapping_sub(*reboot_since.get_or_insert(ms)) >= 100 {
            SCB::sys_reset();
        }
    }
}

/// The app region seen through DfuSe. The first app page (vector table) is
/// collected in RAM and only written at manifestation, so an interrupted
/// download leaves no valid app and the bootloader keeps waiting.
struct Flash {
    buf: [u8; TRANSFER],
    page0: [u8; PAGE as usize],
    page0_dirty: bool,
}

impl Flash {
    fn new() -> Self {
        Self { buf: [0xFF; TRANSFER], page0: [0xFF; PAGE as usize], page0_dirty: false }
    }
}

fn mem_err(e: flash::Error) -> DFUMemError {
    match e {
        flash::Error::Address => DFUMemError::Address,
        flash::Error::NotErased => DFUMemError::CheckErased,
        flash::Error::Prog => DFUMemError::Prog,
        flash::Error::Verify => DFUMemError::Verify,
    }
}

impl DFUMemIO for Flash {
    const INITIAL_ADDRESS_POINTER: u32 = APP_BASE;
    const MEM_INFO_STRING: &'static str = layout::MEM_INFO;
    const MANIFESTATION_TOLERANT: bool = false;
    // Typical F103 timings: page erase ~20 ms, half-word program ~50 µs.
    const PROGRAM_TIME_MS: u32 = 8;
    const ERASE_TIME_MS: u32 = 25;
    const FULL_ERASE_TIME_MS: u32 = 48 * 25;
    const MANIFESTATION_TIME_MS: u32 = 50;
    const TRANSFER_SIZE: u16 = TRANSFER as u16;

    fn store_write_buffer(&mut self, src: &[u8]) -> Result<(), ()> {
        self.buf.get_mut(..src.len()).ok_or(())?.copy_from_slice(src);
        Ok(())
    }

    fn read(&mut self, address: u32, length: usize) -> Result<&[u8], DFUMemError> {
        let end = address.checked_add(length as u32).ok_or(DFUMemError::Address)?;
        if address < FLASH_BASE || end > FLASH_END {
            return Err(DFUMemError::Address);
        }
        // SAFETY: inside on-chip flash.
        Ok(unsafe { core::slice::from_raw_parts(address as *const u8, length) })
    }

    fn program(&mut self, address: u32, length: usize) -> Result<(), DFUMemError> {
        let data = self.buf.get(..length).ok_or(DFUMemError::Unknown)?;
        if !layout::in_app(address, length as u32) {
            return Err(DFUMemError::Address);
        }
        // Part inside the first page: keep for manifestation.
        let page1 = APP_BASE + PAGE;
        let split = (page1.saturating_sub(address) as usize).min(length);
        if split > 0 {
            let off = (address - APP_BASE) as usize;
            self.page0[off..off + split].copy_from_slice(&data[..split]);
            self.page0_dirty = true;
        }
        if split < length {
            flash::program(address + split as u32, &data[split..]).map_err(mem_err)?;
        }
        Ok(())
    }

    fn erase(&mut self, address: u32) -> Result<(), DFUMemError> {
        if address == APP_BASE {
            self.page0 = [0xFF; PAGE as usize];
            self.page0_dirty = false;
        }
        flash::erase_page(address).map_err(mem_err)
    }

    fn erase_all(&mut self) -> Result<(), DFUMemError> {
        self.page0 = [0xFF; PAGE as usize];
        self.page0_dirty = false;
        (APP_BASE..FLASH_END).step_by(PAGE as usize).try_for_each(|a| flash::erase_page(a).map_err(mem_err))
    }

    fn manifestation(&mut self) -> Result<(), DFUManifestationError> {
        if self.page0_dirty {
            flash::erase_page(APP_BASE)
                .and_then(|_| flash::program(APP_BASE, &self.page0))
                .map_err(|_| DFUManifestationError::Unknown)?;
            self.page0_dirty = false;
        }
        if !layout::app_valid() {
            return Err(DFUManifestationError::Firmware);
        }
        REBOOT.store(true, Ordering::Relaxed);
        Ok(())
    }

    fn usb_reset(&mut self) {
        // The host resets the bus after a non-manifestation-tolerant update.
        if REBOOT.load(Ordering::Relaxed) {
            SCB::sys_reset();
        }
    }
}
