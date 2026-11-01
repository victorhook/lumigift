//! USB device: one vendor interface, bulk OUT 0x01 / bulk IN 0x81, plus
//! MS OS 2.0 descriptors so Windows binds WinUSB without an .inf.

use embassy_stm32::peripherals::USB;
use embassy_stm32::usb::{Endpoint, In, Out};
use embassy_usb::driver::{Endpoint as _, EndpointIn as _, EndpointOut as _};
use embassy_usb::msos::{self, windows_version};
use embassy_usb::{Builder, UsbDevice};
use nrf24_usb_proto as proto;
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

    // Device-level features (not a function subset): correct for a
    // non-composite device, which Windows otherwise refuses.
    builder.msos_descriptor(windows_version::WIN8_1, MSOS_VENDOR_CODE);
    builder.msos_feature(msos::CompatibleIdFeatureDescriptor::new("WINUSB", ""));
    builder.msos_feature(msos::RegistryPropertyFeatureDescriptor::new(
        "DeviceInterfaceGUIDs",
        msos::PropertyData::RegMultiSz(DEVICE_INTERFACE_GUIDS),
    ));

    let (ep_out, ep_in) = {
        let mut func = builder.function(proto::USB_INTERFACE_CLASS, 0, 0);
        let mut iface = func.interface();
        let mut alt = iface.alt_setting(proto::USB_INTERFACE_CLASS, 0, 0, None);
        let ep_out = alt.endpoint_bulk_out(Some(proto::EP_OUT.into()), proto::USB_PACKET_SIZE as u16);
        let ep_in = alt.endpoint_bulk_in(Some(proto::EP_IN.into()), proto::USB_PACKET_SIZE as u16);
        (ep_out, ep_in)
    };

    Usb { device: builder.build(), ep_out, ep_in }
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
