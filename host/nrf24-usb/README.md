# nrf24-usb

Host driver for the generic nRF24L01+ USB bridge (`firmware/nrf24-usb-bridge`),
built on [`nusb`](https://crates.io/crates/nusb) (pure Rust, no libusb). The
wire format is defined in `crates/nrf24-usb-proto`.

```rust
use nrf24_usb::{Bridge, Transport, TxFlow};
use nrf24_usb_proto::{tx_flags, Command, Event, Payload, RadioConfig, Mode};
use std::time::Duration;

let mut bridge = Bridge::open(None)?;                 // or Some("serial")
let info = bridge.get_info(Duration::from_secs(1))?;
bridge.send(&[
    Command::Configure(RadioConfig::default()),
    Command::SetTxAddr { slot: 0, addr: [0x00, 0x4C, 0x47, 0x46, 0x54] },
    Command::SetMode(Mode::Standby),
])?;

let mut flow = TxFlow::new(info.tx_queue_depth as usize);
if let Some(tag) = flow.acquire() {
    bridge.send(&[Command::Tx { tag, slot: 0, flags: tx_flags::NO_ACK, ts_offset: 0,
                                payload: Payload::new(b"hello")? }])?;
}
while let Some(ev) = bridge.recv(Duration::from_millis(10))? {
    if let Event::TxDone { .. } = ev { flow.release(); }
}
```

* `nrf24_usb::list()` enumerates bridges (VID `0x1209`, PID `0x0001`).
* `send()` packs commands into 64-byte packets (messages never span packets) and
  submits them as one bulk transfer without waiting for completion.
* A reader thread keeps 8 bulk IN transfers queued, so events arrive within one
  USB frame.
* Unplugging gives `Error::Disconnected` from `send`/`recv`; drop the bridge and
  `Bridge::open` again.
* The `Transport` trait lets a simulator stand in for the hardware.

## Linux permissions (udev)

Install [`firmware/nrf24-usb-bridge/70-nrf24-usb.rules`](../../firmware/nrf24-usb-bridge/70-nrf24-usb.rules)
(the app, `1209:0001`, and its USB bootloader, `1209:0002`) as
`/etc/udev/rules.d/70-nrf24-usb.rules`. The number must stay below 73: `uaccess`
only takes effect when set before systemd's `73-seat-late.rules`. Then run `sudo udevadm control --reload-rules && sudo udevadm trigger` and replug the
bridge.
