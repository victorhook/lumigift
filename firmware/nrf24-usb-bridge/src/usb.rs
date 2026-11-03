//! USB device: a vendor interface (bulk OUT 0x01 / bulk IN 0x81) and a DFU
//! runtime interface (`DFU_DETACH` -> reset into the USB bootloader), plus
//! MS OS 2.0 descriptors so Windows binds WinUSB to both without an .inf.

use embassy_stm32::pac::{BKP, PWR, RCC};
use embassy_stm32::peripherals::USB;
use embassy_stm32::usb::{Endpoint, In, Out};
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::signal::Signal;
use embassy_time::Timer;
use embassy_usb::control::{InResponse, OutResponse, Recipient, Request, RequestType};
use embassy_usb::driver::{Endpoint as _, EndpointIn as _, EndpointOut as _};
use embassy_usb::msos::{self, windows_version};
use embassy_usb::types::InterfaceNumber;
use embassy_usb::{Builder, Handler, UsbDevice};
use nrf24_usb_proto as proto;
use proto::dfu;
use proto::PacketBuilder;
use static_cell::StaticCell;

use crate::board::UsbDriver;
use crate::queues::{CMDS, EVENTS, OutPacket};

/// DeviceInterfaceGUID advertised to Windows (random, fixed for this device type).
const DEVICE_INTERFACE_GUIDS: &[&str] = &["{9B5C2B1E-4D7A-4F0C-9C61-2A8E5F3D7B40}"];
/// bRequest used for the MS OS 2.0 descriptor-set vendor request.
const MSOS_VENDOR_CODE: u8 = 0x20;

pub struct Usb {
    pub device: UsbDevice<'static, UsbDriver>,
    pub ep_out: Endpoint<'static, USB, Out>,
    pub ep_in: Endpoint<'static, USB, In>,
}

pub fn build(driver: UsbDriver, serial: &'static str) -> Usb {
    let mut config = embassy_usb::Config::new(proto::USB_VID, proto::USB_PID);
    config.manufacturer = Some("nrf24-usb");
    config.product = Some(proto::USB_PRODUCT);
    config.serial_number = Some(serial);
    config.device_class = 0x00; // per-interface
    config.device_sub_class = 0x00;
    config.device_protocol = 0x00;
    config.composite_with_iads = false;
    config.device_release = crate::FW_VERSION;
    config.max_power = 250; // MCU + nRF24L01+PA+LNA peaks ~150 mA
    config.max_packet_size_0 = 64;
    // bcd_usb stays 2.1 (default) so Windows reads the BOS / MS OS 2.0 set.

    static CONFIG_DESC: StaticCell<[u8; 64]> = StaticCell::new();
    static BOS_DESC: StaticCell<[u8; 64]> = StaticCell::new();
    static MSOS_DESC: StaticCell<[u8; 256]> = StaticCell::new();
    static CONTROL_BUF: StaticCell<[u8; 64]> = StaticCell::new();

    let mut builder = Builder::new(
        driver,
        config,
        CONFIG_DESC.init([0; 64]),
        BOS_DESC.init([0; 64]),
        MSOS_DESC.init([0; 256]),
        CONTROL_BUF.init([0; 64]),
    );

    // Two interfaces make this a composite device: WinUSB is bound per
    // function (MS OS 2.0 function subsets).
    builder.msos_descriptor(windows_version::WIN8_1, MSOS_VENDOR_CODE);

    let (ep_out, ep_in) = {
        let mut func = builder.function(proto::USB_INTERFACE_CLASS, 0, 0);
        func.msos_feature(msos::CompatibleIdFeatureDescriptor::new("WINUSB", ""));
        func.msos_feature(msos::RegistryPropertyFeatureDescriptor::new(
            "DeviceInterfaceGUIDs",
            msos::PropertyData::RegMultiSz(DEVICE_INTERFACE_GUIDS),
        ));
        let mut iface = func.interface();
        let mut alt = iface.alt_setting(proto::USB_INTERFACE_CLASS, 0, 0, None);
        let ep_out = alt.endpoint_bulk_out(Some(proto::EP_OUT.into()), proto::USB_PACKET_SIZE as u16);
        let ep_in = alt.endpoint_bulk_in(Some(proto::EP_IN.into()), proto::USB_PACKET_SIZE as u16);
        (ep_out, ep_in)
    };

    let dfu_if = {
        let mut func = builder.function(dfu::CLASS, dfu::SUBCLASS, dfu::PROTOCOL_RUNTIME);
        func.msos_feature(msos::CompatibleIdFeatureDescriptor::new("WINUSB", ""));
        let mut iface = func.interface();
        let if_num = iface.interface_number();
        let mut alt = iface.alt_setting(dfu::CLASS, dfu::SUBCLASS, dfu::PROTOCOL_RUNTIME, None);
        // DFU functional descriptor: will detach + can upload + can download,
        // 1 s detach timeout, 256-byte transfers, DfuSe (bcdDFU 1.1a).
        alt.descriptor(dfu::FUNCTIONAL_DESCRIPTOR, &[0x0B, 0xE8, 0x03, 0x00, 0x01, 0x1A, 0x01]);
        if_num
    };
    static DFU_RUNTIME: StaticCell<DfuRuntime> = StaticCell::new();
    builder.handler(DFU_RUNTIME.init(DfuRuntime { if_num: dfu_if }));

    Usb { device: builder.build(), ep_out, ep_in }
}

/// Raised by `DFU_DETACH`; [`detach_task`] then resets into the bootloader.
static DETACH: Signal<CriticalSectionRawMutex, ()> = Signal::new();

/// DFU runtime interface (DFU 1.1 §5): answers GETSTATUS / GETSTATE with
/// appIDLE and turns DETACH into a reset into the USB bootloader.
struct DfuRuntime {
    if_num: InterfaceNumber,
}

impl DfuRuntime {
    fn ours(&self, req: &Request) -> bool {
        req.request_type == RequestType::Class
            && req.recipient == Recipient::Interface
            && req.index == u8::from(self.if_num) as u16
    }
}

impl Handler for DfuRuntime {
    fn control_out(&mut self, req: Request, _data: &[u8]) -> Option<OutResponse> {
        if !self.ours(&req) {
            return None;
        }
        Some(match req.request {
            dfu::DETACH => {
                DETACH.signal(());
                OutResponse::Accepted
            }
            _ => OutResponse::Rejected,
        })
    }

    fn control_in<'a>(&'a mut self, req: Request, buf: &'a mut [u8]) -> Option<InResponse<'a>> {
        if !self.ours(&req) {
            return None;
        }
        const APP_IDLE: u8 = 0;
        Some(match req.request {
            // bStatus OK, bwPollTimeout 0, bState appIDLE, iString 0
            dfu::GETSTATUS if buf.len() >= 6 => {
                buf[..6].copy_from_slice(&[0, 0, 0, 0, APP_IDLE, 0]);
                InResponse::Accepted(&buf[..6])
            }
            dfu::GETSTATE if !buf.is_empty() => {
                buf[0] = APP_IDLE;
                InResponse::Accepted(&buf[..1])
            }
            _ => InResponse::Rejected,
        })
    }
}

/// After `DFU_DETACH`: let the control transfer finish, leave the request
/// for the bootloader in backup register DR1 and reset.
#[embassy_executor::task]
pub async fn detach_task() -> ! {
    DETACH.wait().await;
    Timer::after_millis(20).await;
    RCC.apb1enr().modify(|w| {
        w.set_pwren(true);
        w.set_bkpen(true);
    });
    PWR.cr().modify(|w| w.set_dbp(true));
    BKP.dr(0).write(|w| w.set_d(dfu::BKP_REQUEST));
    cortex_m::peripheral::SCB::sys_reset()
}

#[embassy_executor::task]
pub async fn device_task(mut dev: UsbDevice<'static, UsbDriver>) -> ! {
    dev.run().await
}

/// Host -> bridge: read raw packets into the command queue. When the queue is
/// full this task stops reading, so the endpoint NAKs (USB-level backpressure,
/// nothing is lost).
#[embassy_executor::task]
pub async fn out_task(mut ep: Endpoint<'static, USB, Out>) -> ! {
    loop {
        ep.wait_enabled().await;
        loop {
            let mut pkt = OutPacket::new();
            match ep.read(&mut pkt.buf).await {
                Ok(0) => {}
                Ok(n) => {
                    pkt.len = n as u8;
                    CMDS.send(pkt).await;
                }
                Err(_) => break, // disabled (bus reset / unconfigured)
            }
        }
    }
}

/// Bridge -> host: pack as many queued events as fit into each 64-byte packet
/// and send immediately; while a packet waits for the host's IN token, more
/// events pile up and go out together in the next one.
#[embassy_executor::task]
pub async fn in_task(mut ep: Endpoint<'static, USB, In>) -> ! {
    let mut pb = PacketBuilder::new();
    loop {
        ep.wait_enabled().await;
        // Events from before this (re)configuration belong to a dead session.
        EVENTS.clear();
        'session: loop {
            let first = EVENTS.receive().await;
            pb.clear();
            pb.push_event(&first);
            while let Ok(ev) = EVENTS.try_receive() {
                if !pb.push_event(&ev) {
                    if ep.write(pb.as_slice()).await.is_err() {
                        break 'session;
                    }
                    pb.clear();
                    pb.push_event(&ev);
                }
            }
            if ep.write(pb.as_slice()).await.is_err() {
                break 'session;
            }
            // A full-size packet does not end a bulk transfer; if nothing else
            // is queued, terminate it with a ZLP so hosts that submit larger
            // reads (e.g. 512 B) get the data now. An empty packet parses as
            // zero messages, so it is harmless for 64-byte readers.
            if pb.as_slice().len() == proto::USB_PACKET_SIZE && EVENTS.is_empty() && ep.write(&[]).await.is_err() {
                break 'session;
            }
        }
    }
}
