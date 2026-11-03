//! Host driver for the generic nRF24L01+ USB bridge (`firmware/nrf24-usb-bridge`).
//!
//! The wire protocol lives in [`nrf24_usb_proto`]; this crate moves its
//! [`Command`]s and [`Event`]s over USB using [`nusb`] (pure Rust, no libusb).
//! It knows nothing about Lumigift and can drive the bridge for any project.
//!
//! * [`list`] enumerates connected bridges, [`Bridge::open`] opens one (optionally
//!   by serial number) and claims its vendor interface.
//! * [`Bridge::send`] packs commands into 64-byte packets (never splitting a
//!   message) and submits them as a single bulk OUT transfer without waiting
//!   for completion, so the caller never blocks on USB.
//! * A reader thread keeps several bulk IN transfers queued at all times and
//!   forwards decoded [`Event`]s to a channel ([`Bridge::recv`] /
//!   [`Bridge::events`]).
//! * Unplugging the bridge turns every later call into [`Error::Disconnected`];
//!   drop the `Bridge` and call [`Bridge::open`] again to reconnect.
//! * The [`Transport`] trait abstracts all of the above so a simulator can stand
//!   in for the hardware, and [`TxFlow`] helps keep the number of in-flight
//!   [`Command::Tx`] within [`Info::tx_queue_depth`].
//! * [`dfu::update`] replaces the bridge's own firmware over USB.
//!
//! ```no_run
//! use nrf24_usb::{Bridge, Transport};
//! use nrf24_usb_proto::{Command, Event};
//! use std::time::Duration;
//!
//! let mut bridge = Bridge::open(None)?;
//! let info = bridge.get_info(Duration::from_secs(1))?;
//! println!("bridge fw {} queue depth {}", info.fw_version, info.tx_queue_depth);
//! bridge.send(&[Command::Ping { seq: 1 }])?;
//! while let Some(ev) = bridge.recv(Duration::from_millis(100))? {
//!     if let Event::Pong { time_us, .. } = ev {
//!         println!("bridge clock: {time_us} µs");
//!     }
//! }
//! # Ok::<(), nrf24_usb::Error>(())
//! ```

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, RecvTimeoutError, Sender};
use nrf24_usb_proto::{self as proto, PacketBuilder, USB_PACKET_SIZE};
use nusb::MaybeFuture;
use nusb::transfer::{Bulk, In, Out, TransferError};

pub use nrf24_usb_proto::{Command, Event, Info, Stats};

pub mod dfu;

/// Bulk IN transfers kept queued by the reader thread.
const IN_TRANSFERS: usize = 8;
/// OUT transfers allowed to be pending before `send` waits for one to finish.
const MAX_PENDING_OUT: usize = 16;
const OUT_TIMEOUT: Duration = Duration::from_secs(1);

#[derive(Debug, Clone, thiserror::Error)]
pub enum Error {
    #[error("no nrf24-usb bridge found{}", .0.as_ref().map(|s| format!(" with serial {s}")).unwrap_or_default())]
    NotFound(Option<String>),
    #[error("bridge disconnected")]
    Disconnected,
    #[error("USB error: {0}")]
    Usb(String),
    #[error("protocol error: {0}")]
    Protocol(String),
    #[error("firmware update failed: {0}")]
    Dfu(String),
    #[error("timed out waiting for the bridge")]
    Timeout,
    #[error(
        "permission denied opening the bridge (see the udev rule in the nrf24-usb README): {0}"
    )]
    PermissionDenied(String),
}

pub type Result<T, E = Error> = std::result::Result<T, E>;

impl From<nusb::Error> for Error {
    fn from(e: nusb::Error) -> Self {
        match e.kind() {
            nusb::ErrorKind::Disconnected => Error::Disconnected,
            nusb::ErrorKind::PermissionDenied => Error::PermissionDenied(e.to_string()),
            _ => Error::Usb(e.to_string()),
        }
    }
}

impl From<TransferError> for Error {
    fn from(e: TransferError) -> Self {
        match e {
            TransferError::Disconnected => Error::Disconnected,
            other => Error::Usb(other.to_string()),
        }
    }
}

/// A bidirectional command/event pipe to a bridge (real or simulated).
///
/// Implementations must preserve command order and deliver events in the
/// order the bridge produced them.
pub trait Transport: Send {
    /// Queue commands for the bridge. Must not block for long.
    fn send(&mut self, cmds: &[Command]) -> Result<()>;
    /// Next event, waiting at most `timeout` (`Ok(None)` on timeout).
    fn recv(&mut self, timeout: Duration) -> Result<Option<Event>>;
    /// Human-readable description (serial number, "simulator", ...).
    fn describe(&self) -> String {
        String::from("nrf24-usb transport")
    }
}

impl<T: Transport + ?Sized> Transport for Box<T> {
    fn send(&mut self, cmds: &[Command]) -> Result<()> {
        (**self).send(cmds)
    }
    fn recv(&mut self, timeout: Duration) -> Result<Option<Event>> {
        (**self).recv(timeout)
    }
    fn describe(&self) -> String {
        (**self).describe()
    }
}

/// A connected bridge as seen during enumeration.
#[derive(Debug, Clone)]
pub struct DeviceInfo {
    pub serial: Option<String>,
    pub product: Option<String>,
    pub manufacturer: Option<String>,
    pub bus: String,
    pub address: u8,
    info: nusb::DeviceInfo,
}

/// All connected bridges (matching VID/PID).
pub fn list() -> Result<Vec<DeviceInfo>> {
    let devs = nusb::list_devices().wait()?;
    Ok(devs
        .filter(|d| d.vendor_id() == proto::USB_VID && d.product_id() == proto::USB_PID)
        .map(|d| DeviceInfo {
            serial: d.serial_number().map(str::to_owned),
            product: d.product_string().map(str::to_owned),
            manufacturer: d.manufacturer_string().map(str::to_owned),
            bus: d.bus_id().to_owned(),
            address: d.device_address(),
            info: d,
        })
        .collect())
}

/// Pack commands into USB packets (each ≤ 64 bytes, messages never split).
pub fn pack_commands(cmds: &[Command]) -> Result<Vec<PacketBuilder>> {
    let mut packets = Vec::new();
    let mut cur = PacketBuilder::new();
    for c in cmds {
        if cur.push_command(c) {
            continue;
        }
        // Full: start a new packet (a command that fits no packet is an error).
        if !cur.is_empty() {
            packets.push(std::mem::take(&mut cur));
            if cur.push_command(c) {
                continue;
            }
        }
        return Err(Error::Protocol(format!("command does not encode: {c:?}")));
    }
    if !cur.is_empty() {
        packets.push(cur);
    }
    Ok(packets)
}

/// Decode every message of one IN packet.
pub fn unpack_events(packet: &[u8]) -> impl Iterator<Item = Result<Event>> + '_ {
    proto::messages(packet).map(|m| {
        let m = m.map_err(|e| Error::Protocol(format!("{e:?}")))?;
        Event::decode(m).map_err(|e| Error::Protocol(format!("{e:?}")))
    })
}

/// An open bridge.
pub struct Bridge {
    out: nusb::Endpoint<Bulk, Out>,
    events: Receiver<Result<Event>>,
    stop: Arc<AtomicBool>,
    reader: Option<JoinHandle<()>>,
    dead: bool,
    serial: Option<String>,
    _interface: nusb::Interface,
}

impl Bridge {
    /// Open the first bridge, or the one with the given serial number.
    pub fn open(serial: Option<&str>) -> Result<Self> {
        let devs = list()?;
        let dev = devs
            .into_iter()
            .find(|d| serial.is_none_or(|s| d.serial.as_deref() == Some(s)))
            .ok_or_else(|| Error::NotFound(serial.map(str::to_owned)))?;
        Self::open_device(&dev)
    }

    pub fn open_device(dev: &DeviceInfo) -> Result<Self> {
        let device = dev.info.open().wait()?;
        let intf_num = dev
            .info
            .interfaces()
            .find(|i| i.class() == proto::USB_INTERFACE_CLASS)
            .map(|i| i.interface_number())
            .unwrap_or(0);
        let interface = device.detach_and_claim_interface(intf_num).wait()?;
        let out = interface.endpoint::<Bulk, Out>(proto::EP_OUT)?;
        let mut inp = interface.endpoint::<Bulk, In>(proto::EP_IN)?;
        for _ in 0..IN_TRANSFERS {
            let buf = inp.allocate(USB_PACKET_SIZE);
            inp.submit(buf);
        }
        let (tx, rx) = crossbeam_channel::unbounded();
        let stop = Arc::new(AtomicBool::new(false));
        let stop2 = stop.clone();
        let reader = std::thread::Builder::new()
            .name("nrf24-usb-rx".into())
            .spawn(move || reader_loop(inp, tx, stop2))
            .map_err(|e| Error::Usb(e.to_string()))?;
        Ok(Self {
            out,
            events: rx,
            stop,
            reader: Some(reader),
            dead: false,
            serial: dev.serial.clone(),
            _interface: interface,
        })
    }

    pub fn serial(&self) -> Option<&str> {
        self.serial.as_deref()
    }

    /// The raw event channel (for `select!`). Disconnection arrives as
    /// `Err(Error::Disconnected)` followed by the channel closing.
    pub fn events(&self) -> &Receiver<Result<Event>> {
        &self.events
    }

    /// Send `GetInfo` and wait for the answer, discarding other events.
    pub fn get_info(&mut self, timeout: Duration) -> Result<Info> {
        self.send(&[Command::GetInfo])?;
        let deadline = Instant::now() + timeout;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return Err(Error::Timeout);
            }
            if let Some(Event::Info(i)) = self.recv(left)? {
                return Ok(i);
            }
        }
    }

    /// Pack `cmds` into one bulk OUT transfer and submit it without waiting.
    fn submit(&mut self, cmds: &[Command]) -> Result<()> {
        self.reap_out(MAX_PENDING_OUT)?;
        let packets = pack_commands(cmds)?;
        let Some(last) = packets.last() else {
            return Ok(());
        };
        // Every packet but the last is zero-padded to 64 bytes (a zero
        // length byte ends parsing of a packet).
        let total = (packets.len() - 1) * USB_PACKET_SIZE + last.as_slice().len();
        let mut buf = self.out.allocate(total);
        for (i, p) in packets.iter().enumerate() {
            buf.extend_from_slice(p.as_slice());
            if i + 1 < packets.len() {
                buf.extend_fill(USB_PACKET_SIZE - p.as_slice().len(), 0);
            }
        }
        self.out.submit(buf);
        Ok(())
    }

    fn reap_out(&mut self, block_until_below: usize) -> Result<()> {
        while self.out.pending() > 0 {
            let wait = if self.out.pending() >= block_until_below {
                OUT_TIMEOUT
            } else {
                Duration::ZERO
            };
            match self.out.wait_next_complete(wait) {
                Some(c) => c.status.map_err(Error::from)?,
                None if wait.is_zero() => break,
                None => return Err(Error::Timeout),
            }
        }
        Ok(())
    }
}

impl Transport for Bridge {
    fn send(&mut self, cmds: &[Command]) -> Result<()> {
        if self.dead {
            return Err(Error::Disconnected);
        }
        let r = self.submit(cmds);
        if matches!(r, Err(Error::Disconnected)) {
            self.dead = true;
        }
        r
    }

    fn recv(&mut self, timeout: Duration) -> Result<Option<Event>> {
        if self.dead {
            return Err(Error::Disconnected);
        }
        match self.events.recv_timeout(timeout) {
            Ok(Ok(ev)) => Ok(Some(ev)),
            Ok(Err(e)) => {
                if matches!(e, Error::Disconnected) {
                    self.dead = true;
                }
                Err(e)
            }
            Err(RecvTimeoutError::Timeout) => Ok(None),
            Err(RecvTimeoutError::Disconnected) => {
                self.dead = true;
                Err(Error::Disconnected)
            }
        }
    }

    fn describe(&self) -> String {
        match &self.serial {
            Some(s) => format!("nrf24-usb bridge {s}"),
            None => "nrf24-usb bridge".into(),
        }
    }
}

impl Drop for Bridge {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(h) = self.reader.take() {
            let _ = h.join();
        }
    }
}

fn reader_loop(mut ep: nusb::Endpoint<Bulk, In>, tx: Sender<Result<Event>>, stop: Arc<AtomicBool>) {
    while !stop.load(Ordering::Relaxed) {
        let Some(c) = ep.wait_next_complete(Duration::from_millis(50)) else {
            continue;
        };
        match c.status {
            Ok(()) => {
                for ev in unpack_events(&c.buffer[..c.actual_len]) {
                    match ev {
                        Ok(ev) => {
                            if tx.send(Ok(ev)).is_err() {
                                return;
                            }
                        }
                        Err(e) => log::warn!("nrf24-usb: bad event from bridge: {e}"),
                    }
                }
            }
            Err(TransferError::Cancelled) if stop.load(Ordering::Relaxed) => return,
            Err(TransferError::Disconnected) => {
                let _ = tx.send(Err(Error::Disconnected));
                return;
            }
            Err(e) => {
                log::warn!("nrf24-usb: IN transfer failed: {e}");
                if matches!(e, TransferError::Stall) {
                    let _ = ep.clear_halt().wait();
                }
                // Back off a little so a persistent fault doesn't spin.
                std::thread::sleep(Duration::from_millis(5));
            }
        }
        let mut buf = c.buffer;
        buf.clear();
        buf.set_requested_len(USB_PACKET_SIZE);
        ep.submit(buf);
    }
    ep.cancel_all();
}

/// Credit-based flow control for [`Command::Tx`]: the bridge answers every Tx
/// with exactly one [`Event::TxDone`], so at most `limit` may be outstanding.
#[derive(Debug, Clone)]
pub struct TxFlow {
    limit: usize,
    in_flight: usize,
    next_tag: u16,
}

impl TxFlow {
    /// `limit` is normally [`Info::tx_queue_depth`] (or less, for lower latency).
    pub fn new(limit: usize) -> Self {
        Self {
            limit: limit.max(1),
            in_flight: 0,
            next_tag: 0,
        }
    }
    pub fn set_limit(&mut self, limit: usize) {
        self.limit = limit.max(1);
    }
    pub fn limit(&self) -> usize {
        self.limit
    }
    pub fn in_flight(&self) -> usize {
        self.in_flight
    }
    pub fn available(&self) -> usize {
        self.limit.saturating_sub(self.in_flight)
    }
    /// Reserve a slot and get a fresh tag, or `None` if the queue is full.
    pub fn acquire(&mut self) -> Option<u16> {
        if self.in_flight >= self.limit {
            return None;
        }
        self.in_flight += 1;
        let t = self.next_tag;
        self.next_tag = self.next_tag.wrapping_add(1);
        Some(t)
    }
    /// Call for every `TxDone` (or when giving up on a lost one).
    pub fn release(&mut self) {
        self.in_flight = self.in_flight.saturating_sub(1);
    }
    pub fn reset(&mut self) {
        self.in_flight = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proto::Payload;

    #[test]
    fn packing_never_splits() {
        let tx = Command::Tx {
            tag: 1,
            slot: 0,
            flags: 0,
            ts_offset: 0,
            payload: Payload::new(&[0; 32]).unwrap(),
        };
        let cmds = vec![Command::Ping { seq: 1 }, tx, tx, Command::GetStats, tx];
        let packets = pack_commands(&cmds).unwrap();
        assert_eq!(packets.len(), 3);
        let decoded: Vec<Command> = packets
            .iter()
            .flat_map(|p| {
                proto::messages(p.as_slice())
                    .map(|m| Command::decode(m.unwrap()).unwrap())
                    .collect::<Vec<_>>()
            })
            .collect();
        assert_eq!(decoded, cmds);
    }

    #[test]
    fn padded_packets_parse() {
        let mut p = PacketBuilder::new();
        assert!(p.push_event(&Event::Pong { seq: 3, time_us: 9 }));
        let mut buf = p.as_slice().to_vec();
        buf.resize(64, 0);
        let evs: Vec<_> = unpack_events(&buf).collect::<Result<_>>().unwrap();
        assert_eq!(evs, vec![Event::Pong { seq: 3, time_us: 9 }]);
    }

    #[test]
    fn flow_control() {
        let mut f = TxFlow::new(2);
        assert!(f.acquire().is_some());
        assert!(f.acquire().is_some());
        assert!(f.acquire().is_none());
        f.release();
        assert_eq!(f.available(), 1);
    }
}
