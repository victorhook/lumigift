//! Wire protocol between a host computer and a generic nRF24L01+ USB bridge.
//!
//! The bridge is a USB full-speed device with one vendor-specific interface
//! (class 0xFF) holding a bulk OUT endpoint ([`EP_OUT`]) and a bulk IN endpoint
//! ([`EP_IN`]), both with 64-byte max packet size.
//!
//! # Framing
//!
//! Every USB packet carries one or more *complete* messages back to back;
//! a message never spans two USB packets. Each message is
//!
//! ```text
//! [len: u8][kind: u8][body: len-1 bytes]
//! ```
//!
//! `len` counts `kind` + `body`. A `len` of 0 terminates parsing of the packet
//! (padding). All multi-byte integers are little-endian.
//!
//! # Semantics
//!
//! The bridge processes host messages strictly in order. A [`Command::Tx`]
//! always produces exactly one [`Event::TxDone`], which hosts use for flow
//! control (keep at most [`Info::tx_queue_depth`] transmissions in flight).
//! Commands other than `Tx`, `Ping`, `GetInfo`, `GetStats` and `Scan`
//! answer with [`Event::CmdResult`].
//!
//! While in [`Mode::Rx`] the radio listens as PRX between transmissions and
//! every received packet is reported as [`Event::Rx`]. Pipe 0's RX address
//! is borrowed during auto-ack transmissions and restored afterwards.
//!
//! All timestamps are the bridge's free-running microsecond clock (u32,
//! wraps every ~71.6 minutes).
#![no_std]

pub const USB_VID: u16 = 0x1209;
pub const USB_PID: u16 = 0x0001;
pub const USB_PRODUCT: &str = "nrf24-usb bridge";
pub const USB_INTERFACE_CLASS: u8 = 0xFF;
pub const EP_OUT: u8 = 0x01;
pub const EP_IN: u8 = 0x81;
pub const USB_PACKET_SIZE: usize = 64;

/// Bumped on any incompatible change to this module.
pub const PROTOCOL_VERSION: u8 = 1;

/// Largest nRF24 payload.
pub const MAX_PAYLOAD: usize = 32;

/// Number of TX address slots the bridge stores (see [`Command::SetTxAddr`]).
pub const TX_ADDR_SLOTS: u8 = 8;

/// Longest encoded message (TxDone with 32-byte ack payload) incl. len byte.
pub const MAX_MESSAGE: usize = 1 + 1 + 8 + MAX_PAYLOAD;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    /// Output buffer too small.
    BufferFull,
    /// Message truncated or body length wrong for its kind.
    Malformed,
    /// Unknown message kind.
    UnknownKind(u8),
    /// A field had an out-of-range value.
    BadValue,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[repr(u8)]
pub enum DataRate {
    Kbps250 = 0,
    #[default]
    Mbps1 = 1,
    Mbps2 = 2,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[repr(u8)]
pub enum Power {
    /// -18 dBm
    Min = 0,
    /// -12 dBm
    Low = 1,
    /// -6 dBm
    High = 2,
    /// 0 dBm
    #[default]
    Max = 3,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[repr(u8)]
pub enum Mode {
    /// Radio powered down.
    PowerDown = 0,
    /// Standby; transmit only.
    #[default]
    Standby = 1,
    /// Listen (PRX) whenever not transmitting.
    Rx = 2,
}

pub mod features {
    /// Dynamic payload length on all pipes.
    pub const DYN_PAYLOAD: u8 = 1 << 0;
    /// Payloads attached to auto-acks.
    pub const ACK_PAYLOAD: u8 = 1 << 1;
    /// Allow per-packet NO_ACK transmissions.
    pub const DYN_ACK: u8 = 1 << 2;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RadioConfig {
    /// 0..=125, frequency = 2400 + channel MHz.
    pub channel: u8,
    pub data_rate: DataRate,
    pub power: Power,
    /// CRC length in bytes: 1 or 2 (0 is not allowed with auto-ack).
    pub crc_len: u8,
    /// Address width in bytes: 3..=5.
    pub addr_width: u8,
    /// Auto-retransmit count 0..=15.
    pub retries: u8,
    /// Auto-retransmit delay, (n + 1) * 250 µs, n in 0..=15.
    pub retry_delay: u8,
    /// See [`features`].
    pub features: u8,
    /// Static payload width used when DYN_PAYLOAD is off.
    pub payload_len: u8,
}

impl Default for RadioConfig {
    fn default() -> Self {
        Self {
            channel: 76,
            data_rate: DataRate::Mbps1,
            power: Power::Max,
            crc_len: 2,
            addr_width: 5,
            retries: 3,
            retry_delay: 1,
            features: features::DYN_PAYLOAD | features::ACK_PAYLOAD | features::DYN_ACK,
            payload_len: 32,
        }
    }
}

pub mod tx_flags {
    /// Send with the NO_ACK bit; receivers will not acknowledge.
    pub const NO_ACK: u8 = 1 << 0;
    /// Write the bridge's u32 µs timestamp (LE) into the payload at
    /// `ts_offset` immediately before loading the radio FIFO.
    pub const STAMP: u8 = 1 << 1;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum TxStatus {
    /// Transmitted with NO_ACK.
    Sent = 0,
    /// Acknowledged by the receiver.
    Acked = 1,
    /// No acknowledgement after all retries.
    MaxRetries = 2,
    /// Not transmitted (bad slot, payload too long, radio fault).
    Failed = 3,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum CmdStatus {
    Ok = 0,
    BadArgs = 1,
    RadioError = 2,
}

/// Up to 32 bytes stored inline.
#[derive(Clone, Copy, PartialEq, Eq, Default)]
pub struct Payload {
    len: u8,
    buf: [u8; MAX_PAYLOAD],
}

impl Payload {
    pub fn new(data: &[u8]) -> Result<Self, Error> {
        if data.len() > MAX_PAYLOAD {
            return Err(Error::BadValue);
        }
        let mut buf = [0; MAX_PAYLOAD];
        buf[..data.len()].copy_from_slice(data);
        Ok(Self {
            len: data.len() as u8,
            buf,
        })
    }
    pub fn as_slice(&self) -> &[u8] {
        &self.buf[..self.len as usize]
    }
    pub fn as_mut_slice(&mut self) -> &mut [u8] {
        &mut self.buf[..self.len as usize]
    }
    pub fn len(&self) -> usize {
        self.len as usize
    }
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }
}

impl core::fmt::Debug for Payload {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "Payload(")?;
        for b in self.as_slice() {
            write!(f, "{b:02x}")?;
        }
        write!(f, ")")
    }
}

/// Host → bridge.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Command {
    GetInfo,
    Ping {
        seq: u32,
    },
    Configure(RadioConfig),
    /// Configure an RX pipe (0..=5). Pipes 2..=5 only use `addr[0]`;
    /// their other bytes are shared with pipe 1.
    SetRxPipe {
        pipe: u8,
        enabled: bool,
        auto_ack: bool,
        addr: [u8; 5],
    },
    /// Store a destination address for [`Command::Tx`].
    SetTxAddr {
        slot: u8,
        addr: [u8; 5],
    },
    Tx {
        tag: u16,
        slot: u8,
        flags: u8,
        ts_offset: u8,
        payload: Payload,
    },
    /// Queue a payload to attach to the next auto-ack on `pipe` (Rx mode).
    SetAckPayload {
        pipe: u8,
        payload: Payload,
    },
    SetMode(Mode),
    /// Sample the RPD (>-64 dBm) flag on each channel; one ScanResult per channel.
    Scan {
        first: u8,
        last: u8,
        dwell_us: u16,
        repeats: u8,
    },
    GetStats,
    Reset,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Info {
    pub protocol_version: u8,
    pub fw_version: u16,
    pub tx_queue_depth: u8,
    pub radio_present: bool,
    pub uid: [u8; 12],
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Stats {
    pub tx_acked: u32,
    pub tx_sent: u32,
    pub tx_failed: u32,
    pub rx: u32,
    /// Events dropped because the IN endpoint could not keep up.
    pub events_dropped: u32,
    /// Malformed host messages.
    pub bad_commands: u32,
}

/// Bridge → host.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Event {
    Info(Info),
    Pong {
        seq: u32,
        time_us: u32,
    },
    Rx {
        pipe: u8,
        time_us: u32,
        payload: Payload,
    },
    TxDone {
        tag: u16,
        status: TxStatus,
        retries: u8,
        time_us: u32,
        ack_payload: Payload,
    },
    ScanResult {
        channel: u8,
        hits: u16,
        samples: u16,
    },
    Stats(Stats),
    CmdResult {
        kind: u8,
        status: CmdStatus,
    },
}

mod kind {
    pub const GET_INFO: u8 = 0x01;
    pub const PING: u8 = 0x02;
    pub const CONFIGURE: u8 = 0x03;
    pub const SET_RX_PIPE: u8 = 0x04;
    pub const SET_TX_ADDR: u8 = 0x05;
    pub const TX: u8 = 0x06;
    pub const SET_ACK_PAYLOAD: u8 = 0x07;
    pub const SET_MODE: u8 = 0x08;
    pub const SCAN: u8 = 0x09;
    pub const GET_STATS: u8 = 0x0A;
    pub const RESET: u8 = 0x0B;

    pub const INFO: u8 = 0x81;
    pub const PONG: u8 = 0x82;
    pub const RX: u8 = 0x83;
    pub const TX_DONE: u8 = 0x84;
    pub const SCAN_RESULT: u8 = 0x85;
    pub const STATS: u8 = 0x86;
    pub const CMD_RESULT: u8 = 0x87;
}

impl Command {
    /// Message kind byte, as echoed in [`Event::CmdResult`].
    pub fn kind(&self) -> u8 {
        use kind::*;
        match self {
            Command::GetInfo => GET_INFO,
            Command::Ping { .. } => PING,
            Command::Configure(_) => CONFIGURE,
            Command::SetRxPipe { .. } => SET_RX_PIPE,
            Command::SetTxAddr { .. } => SET_TX_ADDR,
            Command::Tx { .. } => TX,
            Command::SetAckPayload { .. } => SET_ACK_PAYLOAD,
            Command::SetMode(_) => SET_MODE,
            Command::Scan { .. } => SCAN,
            Command::GetStats => GET_STATS,
            Command::Reset => RESET,
        }
    }

    /// Encode as one framed message; returns bytes written.
    pub fn encode(&self, out: &mut [u8]) -> Result<usize, Error> {
        let mut w = Writer::new(out);
        w.begin(self.kind())?;
        match *self {
            Command::GetInfo | Command::GetStats | Command::Reset => {}
            Command::Ping { seq } => w.u32(seq)?,
            Command::Configure(c) => {
                w.bytes(&[
                    c.channel,
                    c.data_rate as u8,
                    c.power as u8,
                    c.crc_len,
                    c.addr_width,
                    c.retries,
                    c.retry_delay,
                    c.features,
                    c.payload_len,
                ])?;
            }
            Command::SetRxPipe {
                pipe,
                enabled,
                auto_ack,
                addr,
            } => {
                w.bytes(&[pipe, enabled as u8 | (auto_ack as u8) << 1])?;
                w.bytes(&addr)?;
            }
            Command::SetTxAddr { slot, addr } => {
                w.u8(slot)?;
                w.bytes(&addr)?;
            }
            Command::Tx {
                tag,
                slot,
                flags,
                ts_offset,
                payload,
            } => {
                w.u16(tag)?;
                w.bytes(&[slot, flags, ts_offset])?;
                w.bytes(payload.as_slice())?;
            }
            Command::SetAckPayload { pipe, payload } => {
                w.u8(pipe)?;
                w.bytes(payload.as_slice())?;
            }
            Command::SetMode(m) => w.u8(m as u8)?,
            Command::Scan {
                first,
                last,
                dwell_us,
                repeats,
            } => {
                w.bytes(&[first, last])?;
                w.u16(dwell_us)?;
                w.u8(repeats)?;
            }
        }
        w.finish()
    }

    /// Decode a message body (kind + body, without the len byte).
    pub fn decode(msg: &[u8]) -> Result<Self, Error> {
        use kind::*;
        let (&k, body) = msg.split_first().ok_or(Error::Malformed)?;
        let mut r = Reader(body);
        let cmd = match k {
            GET_INFO => Command::GetInfo,
            GET_STATS => Command::GetStats,
            RESET => Command::Reset,
            PING => Command::Ping { seq: r.u32()? },
            CONFIGURE => {
                let b = r.take(9)?;
                Command::Configure(RadioConfig {
                    channel: b[0],
                    data_rate: match b[1] {
                        0 => DataRate::Kbps250,
                        1 => DataRate::Mbps1,
                        2 => DataRate::Mbps2,
                        _ => return Err(Error::BadValue),
                    },
                    power: match b[2] {
                        0 => Power::Min,
                        1 => Power::Low,
                        2 => Power::High,
                        3 => Power::Max,
                        _ => return Err(Error::BadValue),
                    },
                    crc_len: b[3],
                    addr_width: b[4],
                    retries: b[5],
                    retry_delay: b[6],
                    features: b[7],
                    payload_len: b[8],
                })
            }
            SET_RX_PIPE => {
                let pipe = r.u8()?;
                let f = r.u8()?;
                Command::SetRxPipe {
                    pipe,
                    enabled: f & 1 != 0,
                    auto_ack: f & 2 != 0,
                    addr: r.array()?,
                }
            }
            SET_TX_ADDR => Command::SetTxAddr {
                slot: r.u8()?,
                addr: r.array()?,
            },
            TX => {
                let tag = r.u16()?;
                let slot = r.u8()?;
                let flags = r.u8()?;
                let ts_offset = r.u8()?;
                Command::Tx {
                    tag,
                    slot,
                    flags,
                    ts_offset,
                    payload: Payload::new(r.rest())?,
                }
            }
            SET_ACK_PAYLOAD => {
                let pipe = r.u8()?;
                Command::SetAckPayload {
                    pipe,
                    payload: Payload::new(r.rest())?,
                }
            }
            SET_MODE => Command::SetMode(match r.u8()? {
                0 => Mode::PowerDown,
                1 => Mode::Standby,
                2 => Mode::Rx,
                _ => return Err(Error::BadValue),
            }),
            SCAN => Command::Scan {
                first: r.u8()?,
                last: r.u8()?,
                dwell_us: r.u16()?,
                repeats: r.u8()?,
            },
            other => return Err(Error::UnknownKind(other)),
        };
        r.end()?;
        Ok(cmd)
    }
}

impl Event {
    pub fn encode(&self, out: &mut [u8]) -> Result<usize, Error> {
        use kind::*;
        let mut w = Writer::new(out);
        match *self {
            Event::Info(i) => {
                w.begin(INFO)?;
                w.u8(i.protocol_version)?;
                w.u16(i.fw_version)?;
                w.u8(i.tx_queue_depth)?;
                w.u8(i.radio_present as u8)?;
                w.bytes(&i.uid)?;
            }
            Event::Pong { seq, time_us } => {
                w.begin(PONG)?;
                w.u32(seq)?;
                w.u32(time_us)?;
            }
            Event::Rx {
                pipe,
                time_us,
                payload,
            } => {
                w.begin(RX)?;
                w.u8(pipe)?;
                w.u32(time_us)?;
                w.bytes(payload.as_slice())?;
            }
            Event::TxDone {
                tag,
                status,
                retries,
                time_us,
                ack_payload,
            } => {
                w.begin(TX_DONE)?;
                w.u16(tag)?;
                w.u8(status as u8)?;
                w.u8(retries)?;
                w.u32(time_us)?;
                w.bytes(ack_payload.as_slice())?;
            }
            Event::ScanResult {
                channel,
                hits,
                samples,
            } => {
                w.begin(SCAN_RESULT)?;
                w.u8(channel)?;
                w.u16(hits)?;
                w.u16(samples)?;
            }
            Event::Stats(s) => {
                w.begin(STATS)?;
                for v in [
                    s.tx_acked,
                    s.tx_sent,
                    s.tx_failed,
                    s.rx,
                    s.events_dropped,
                    s.bad_commands,
                ] {
                    w.u32(v)?;
                }
            }
            Event::CmdResult { kind, status } => {
                w.begin(CMD_RESULT)?;
                w.bytes(&[kind, status as u8])?;
            }
        }
        w.finish()
    }

    pub fn decode(msg: &[u8]) -> Result<Self, Error> {
        use kind::*;
        let (&k, body) = msg.split_first().ok_or(Error::Malformed)?;
        let mut r = Reader(body);
        let ev = match k {
            INFO => Event::Info(Info {
                protocol_version: r.u8()?,
                fw_version: r.u16()?,
                tx_queue_depth: r.u8()?,
                radio_present: r.u8()? != 0,
                uid: r.array()?,
            }),
            PONG => Event::Pong {
                seq: r.u32()?,
                time_us: r.u32()?,
            },
            RX => Event::Rx {
                pipe: r.u8()?,
                time_us: r.u32()?,
                payload: Payload::new(r.rest())?,
            },
            TX_DONE => Event::TxDone {
                tag: r.u16()?,
                status: match r.u8()? {
                    0 => TxStatus::Sent,
                    1 => TxStatus::Acked,
                    2 => TxStatus::MaxRetries,
                    3 => TxStatus::Failed,
                    _ => return Err(Error::BadValue),
                },
                retries: r.u8()?,
                time_us: r.u32()?,
                ack_payload: Payload::new(r.rest())?,
            },
            SCAN_RESULT => Event::ScanResult {
                channel: r.u8()?,
                hits: r.u16()?,
                samples: r.u16()?,
            },
            STATS => Event::Stats(Stats {
                tx_acked: r.u32()?,
                tx_sent: r.u32()?,
                tx_failed: r.u32()?,
                rx: r.u32()?,
                events_dropped: r.u32()?,
                bad_commands: r.u32()?,
            }),
            CMD_RESULT => Event::CmdResult {
                kind: r.u8()?,
                status: match r.u8()? {
                    0 => CmdStatus::Ok,
                    1 => CmdStatus::BadArgs,
                    2 => CmdStatus::RadioError,
                    _ => return Err(Error::BadValue),
                },
            },
            other => return Err(Error::UnknownKind(other)),
        };
        r.end()?;
        Ok(ev)
    }
}

/// Iterate over the message bodies (kind + body) in one USB packet.
pub fn messages(packet: &[u8]) -> Messages<'_> {
    Messages(packet)
}

pub struct Messages<'a>(&'a [u8]);

impl<'a> Iterator for Messages<'a> {
    type Item = Result<&'a [u8], Error>;
    fn next(&mut self) -> Option<Self::Item> {
        let (&len, rest) = self.0.split_first()?;
        if len == 0 {
            self.0 = &[];
            return None;
        }
        let len = len as usize;
        if rest.len() < len {
            self.0 = &[];
            return Some(Err(Error::Malformed));
        }
        let (msg, rest) = rest.split_at(len);
        self.0 = rest;
        Some(Ok(msg))
    }
}

/// Packs framed messages into USB packets without splitting any message.
pub struct PacketBuilder {
    buf: [u8; USB_PACKET_SIZE],
    len: usize,
}

impl Default for PacketBuilder {
    fn default() -> Self {
        Self::new()
    }
}

impl PacketBuilder {
    pub const fn new() -> Self {
        Self {
            buf: [0; USB_PACKET_SIZE],
            len: 0,
        }
    }

    /// Append an already-framed message. Returns `false` (and appends
    /// nothing) if it does not fit; flush and retry.
    pub fn push_framed(&mut self, msg: &[u8]) -> bool {
        if self.len + msg.len() > USB_PACKET_SIZE {
            return false;
        }
        self.buf[self.len..self.len + msg.len()].copy_from_slice(msg);
        self.len += msg.len();
        true
    }

    pub fn push_command(&mut self, c: &Command) -> bool {
        let mut tmp = [0u8; MAX_MESSAGE];
        match c.encode(&mut tmp) {
            Ok(n) => self.push_framed(&tmp[..n]),
            Err(_) => false,
        }
    }

    pub fn push_event(&mut self, e: &Event) -> bool {
        let mut tmp = [0u8; MAX_MESSAGE];
        match e.encode(&mut tmp) {
            Ok(n) => self.push_framed(&tmp[..n]),
            Err(_) => false,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    pub fn as_slice(&self) -> &[u8] {
        &self.buf[..self.len]
    }

    pub fn clear(&mut self) {
        self.len = 0;
    }
}

struct Writer<'a> {
    out: &'a mut [u8],
    pos: usize,
}

impl<'a> Writer<'a> {
    fn new(out: &'a mut [u8]) -> Self {
        Self { out, pos: 0 }
    }
    fn begin(&mut self, kind: u8) -> Result<(), Error> {
        self.bytes(&[0, kind])
    }
    fn u8(&mut self, v: u8) -> Result<(), Error> {
        self.bytes(&[v])
    }
    fn u16(&mut self, v: u16) -> Result<(), Error> {
        self.bytes(&v.to_le_bytes())
    }
    fn u32(&mut self, v: u32) -> Result<(), Error> {
        self.bytes(&v.to_le_bytes())
    }
    fn bytes(&mut self, b: &[u8]) -> Result<(), Error> {
        let end = self.pos + b.len();
        if end > self.out.len() {
            return Err(Error::BufferFull);
        }
        self.out[self.pos..end].copy_from_slice(b);
        self.pos = end;
        Ok(())
    }
    fn finish(self) -> Result<usize, Error> {
        let len = self.pos - 1;
        if len > u8::MAX as usize {
            return Err(Error::BadValue);
        }
        self.out[0] = len as u8;
        Ok(self.pos)
    }
}

struct Reader<'a>(&'a [u8]);

impl<'a> Reader<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8], Error> {
        if self.0.len() < n {
            return Err(Error::Malformed);
        }
        let (a, b) = self.0.split_at(n);
        self.0 = b;
        Ok(a)
    }
    fn u8(&mut self) -> Result<u8, Error> {
        Ok(self.take(1)?[0])
    }
    fn u16(&mut self) -> Result<u16, Error> {
        Ok(u16::from_le_bytes(self.array()?))
    }
    fn u32(&mut self) -> Result<u32, Error> {
        Ok(u32::from_le_bytes(self.array()?))
    }
    fn array<const N: usize>(&mut self) -> Result<[u8; N], Error> {
        Ok(self.take(N)?.try_into().unwrap())
    }
    fn rest(&mut self) -> &'a [u8] {
        core::mem::take(&mut self.0)
    }
    fn end(&self) -> Result<(), Error> {
        if self.0.is_empty() {
            Ok(())
        } else {
            Err(Error::Malformed)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rt_cmd(c: Command) {
        let mut buf = [0u8; MAX_MESSAGE];
        let n = c.encode(&mut buf).unwrap();
        assert_eq!(buf[0] as usize, n - 1);
        assert_eq!(Command::decode(&buf[1..n]).unwrap(), c);
    }

    fn rt_ev(e: Event) {
        let mut buf = [0u8; MAX_MESSAGE];
        let n = e.encode(&mut buf).unwrap();
        assert_eq!(Event::decode(&buf[1..n]).unwrap(), e);
    }

    #[test]
    fn commands_roundtrip() {
        rt_cmd(Command::GetInfo);
        rt_cmd(Command::Ping { seq: 0xDEADBEEF });
        rt_cmd(Command::Configure(RadioConfig::default()));
        rt_cmd(Command::SetRxPipe {
            pipe: 2,
            enabled: true,
            auto_ack: false,
            addr: [1, 2, 3, 4, 5],
        });
        rt_cmd(Command::SetTxAddr {
            slot: 7,
            addr: [9; 5],
        });
        rt_cmd(Command::Tx {
            tag: 77,
            slot: 1,
            flags: tx_flags::NO_ACK,
            ts_offset: 2,
            payload: Payload::new(&[0xAA; 32]).unwrap(),
        });
        rt_cmd(Command::SetAckPayload {
            pipe: 1,
            payload: Payload::new(b"hi").unwrap(),
        });
        rt_cmd(Command::SetMode(Mode::Rx));
        rt_cmd(Command::Scan {
            first: 0,
            last: 125,
            dwell_us: 200,
            repeats: 10,
        });
        rt_cmd(Command::Reset);
    }

    #[test]
    fn events_roundtrip() {
        rt_ev(Event::Info(Info {
            protocol_version: 1,
            fw_version: 3,
            tx_queue_depth: 16,
            radio_present: true,
            uid: [7; 12],
        }));
        rt_ev(Event::Pong { seq: 1, time_us: 2 });
        rt_ev(Event::Rx {
            pipe: 1,
            time_us: 99,
            payload: Payload::new(&[1; 32]).unwrap(),
        });
        rt_ev(Event::TxDone {
            tag: 5,
            status: TxStatus::Acked,
            retries: 2,
            time_us: 3,
            ack_payload: Payload::new(&[2; 32]).unwrap(),
        });
        rt_ev(Event::ScanResult {
            channel: 3,
            hits: 4,
            samples: 5,
        });
        rt_ev(Event::Stats(Stats {
            tx_acked: 1,
            tx_sent: 2,
            tx_failed: 3,
            rx: 4,
            events_dropped: 5,
            bad_commands: 6,
        }));
        rt_ev(Event::CmdResult {
            kind: 3,
            status: CmdStatus::BadArgs,
        });
    }

    #[test]
    fn largest_message_fits_packet() {
        let e = Event::TxDone {
            tag: 0,
            status: TxStatus::Acked,
            retries: 0,
            time_us: 0,
            ack_payload: Payload::new(&[0; 32]).unwrap(),
        };
        let mut buf = [0u8; MAX_MESSAGE];
        assert_eq!(e.encode(&mut buf).unwrap(), MAX_MESSAGE);
        const { assert!(MAX_MESSAGE <= USB_PACKET_SIZE) };
    }

    #[test]
    fn packing_and_iteration() {
        let mut p = PacketBuilder::new();
        assert!(p.push_command(&Command::Ping { seq: 1 }));
        assert!(p.push_command(&Command::GetStats));
        let tx = Command::Tx {
            tag: 1,
            slot: 0,
            flags: 0,
            ts_offset: 0,
            payload: Payload::new(&[0; 32]).unwrap(),
        };
        assert!(p.push_command(&tx));
        assert!(!p.push_command(&tx), "second 38-byte Tx must not fit");
        let got: [Command; 3] = {
            let mut it = messages(p.as_slice()).map(|m| Command::decode(m.unwrap()).unwrap());
            [it.next().unwrap(), it.next().unwrap(), it.next().unwrap()]
        };
        assert_eq!(got, [Command::Ping { seq: 1 }, Command::GetStats, tx]);
    }

    #[test]
    fn rejects_garbage() {
        assert!(matches!(
            Command::decode(&[0x55]),
            Err(Error::UnknownKind(0x55))
        ));
        assert!(Command::decode(&[kind::PING, 1, 2]).is_err());
        assert!(Command::decode(&[kind::PING, 1, 2, 3, 4, 5]).is_err());
        assert!(messages(&[5, 1]).next().unwrap().is_err());
    }
}
