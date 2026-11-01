//! Inter-task queues and sizing.

use core::sync::atomic::AtomicU32;

use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::channel::Channel;
use nrf24_usb_proto::{Event, USB_PACKET_SIZE};

/// Raw host packets waiting for the radio task. Every packet holds at least
/// one message, so this many `Tx` always fit without the OUT endpoint NAKing;
/// it is what we advertise as `Info::tx_queue_depth`.
pub const CMD_QUEUE: usize = 24;

/// Events waiting for the IN endpoint. Must be well above `CMD_QUEUE` so a
/// full window of `TxDone`s never blocks the radio while the host is reading.
pub const EVENT_QUEUE: usize = 48;

/// Unsolicited `Rx` events may only use the queue up to this level; the rest
/// is reserved for replies (`TxDone`, `CmdResult`, ...), which are never
/// dropped (the radio task waits for space instead).
pub const RX_EVENT_LIMIT: usize = EVENT_QUEUE - 12;

#[derive(Clone, Copy)]
pub struct OutPacket {
    pub len: u8,
    pub buf: [u8; USB_PACKET_SIZE],
}

impl OutPacket {
    pub const fn new() -> Self {
        Self { len: 0, buf: [0; USB_PACKET_SIZE] }
    }
    pub fn as_slice(&self) -> &[u8] {
        &self.buf[..self.len as usize]
    }
}

pub static CMDS: Channel<CriticalSectionRawMutex, OutPacket, CMD_QUEUE> = Channel::new();
pub static EVENTS: Channel<CriticalSectionRawMutex, Event, EVENT_QUEUE> = Channel::new();

/// Bumped on every radio packet; the LED task flickers when it changes.
pub static RADIO_ACTIVITY: AtomicU32 = AtomicU32::new(0);
