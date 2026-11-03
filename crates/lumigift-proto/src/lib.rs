//! Lumigift over-the-air protocol, flash layout and firmware image format.
//!
//! # Radio link
//!
//! Every gift is an nRF24 PRX (never transmits on its own) with
//!
//! * pipe 1: the broadcast address `[BROADCAST_LSB, PREFIX..]`
//! * pipe 2: its unicast address `[node_addr, PREFIX..]`
//!
//! both with auto-ack and dynamic payloads enabled. The host sends broadcasts
//! with the NO_ACK bit set (so nobody answers) and unicast commands with
//! auto-ack. Anything a gift wants to say travels in the **ack payload** of
//! the next unicast packet addressed to it: after handling a unicast packet
//! the gift flushes its TX FIFO and preloads exactly one reply (a
//! [`Reply::Status`] unless the command produced a different reply). Hosts
//! therefore send a command, then a [`Unicast::Poll`] to collect its reply,
//! matching `echo` against the command's `seq`.
//!
//! Discovery: the host polls every node address in `1..=254` with few
//! retries; whoever acks exists. Two gifts sharing a node address are told
//! apart via [`Unicast::GetIdentity`] and separated with
//! [`Broadcast::SetNodeAddr`], which carries the target's UID.
//!
//! Time: every show broadcast carries the bridge's µs clock stamped right
//! before transmission, so gifts track the bridge clock continuously.
//!
//! # Show stream
//!
//! [`Broadcast::Music`] is the main show message: `[kind][seq][time_us: 4]`
//! then the 26-byte [`MusicFrame`] body, 32 bytes in all (the full nRF24
//! payload):
//!
//! | bytes | field |
//! |---|---|
//! | 0..4 | effect, brightness, palette, flags |
//! | 4..8 | bass, mid, treble, level |
//! | 8..12 | beat_phase, beat_in_bar, bpm, hue |
//! | 12..15 | param[0] (speed), param[1] (size), slot_count |
//! | 15..21 | colors[0], colors[1] (RGB, used with [`PALETTE_CUSTOM`]) |
//! | 21..25 | mask (u32 LE, bit n = slot n, [`MASK_ALL`] = everyone incl. unassigned) |
//! | 25 | transition (crossfade, × [`TRANSITION_UNIT_MS`]) |
//!
//! Gifts apply a frame only if its mask covers their slot (other frames just
//! keep the current look alive), so the host can run different looks on
//! groups of gifts with one frame per group per tick. Decoding ignores
//! trailing bytes.
//!
//! # Firmware update (OTA)
//!
//! Only the bootloader receives images (see `docs/ARCHITECTURE.md`):
//!
//! 1. [`Broadcast::EnterBootloader`] (node addresses of the targets) or a
//!    unicast [`Unicast::Reboot`] moves gifts into their bootloader.
//! 2. [`Broadcast::OtaBegin`] starts a session; the bootloader erases only
//!    the pages the image covers, in the background, while already
//!    buffering chunks.
//! 3. [`Broadcast::OtaChunk`]s (24 bytes at `index * 24` from the app region
//!    base), streamed in order.
//! 4. [`Unicast::OtaQuery`] returns an [`OtaBitmap`]: the received bits of
//!    one 192-chunk window (the first window at or after the requested one
//!    that has holes) plus the total missing count. The host re-broadcasts
//!    the union of every target's holes.
//! 5. A complete image is verified by the bootloader on its own
//!    ([`OtaState::Verified`]/[`OtaState::Failed`]); [`Unicast::OtaCommit`]
//!    asks for the verdict explicitly. After `Verified` the bootloader
//!    starts the app as soon as the verdict was collected by a `Poll`.
#![no_std]

pub mod image;
pub mod layout;

pub use image::ImageHeader;

// ---------------------------------------------------------------------------
// Radio parameters
// ---------------------------------------------------------------------------

/// 2476 MHz: above Wi-Fi channel 11 and between the BLE advertising channels.
pub const DEFAULT_CHANNEL: u8 = 76;
/// 1 Mbps: good balance of sensitivity and OTA throughput. (OTA is limited by
/// the gifts' flash write speed, not the air rate, so 2 Mbps would not make
/// updates faster; see `docs/ARCHITECTURE.md`.)
pub const DATA_RATE_MBPS: u8 = 1;
/// Upper four address bytes shared by every Lumigift pipe ("LGFT").
pub const ADDR_PREFIX: [u8; 4] = [0x4C, 0x47, 0x46, 0x54];
pub const BROADCAST_LSB: u8 = 0x00;
/// Never used as a node address.
pub const INVALID_NODE: u8 = 0xFF;
pub const MAX_PAYLOAD: usize = 32;

pub const fn address(lsb: u8) -> [u8; 5] {
    [
        lsb,
        ADDR_PREFIX[0],
        ADDR_PREFIX[1],
        ADDR_PREFIX[2],
        ADDR_PREFIX[3],
    ]
}
pub const BROADCAST_ADDR: [u8; 5] = address(BROADCAST_LSB);

pub fn valid_node_addr(a: u8) -> bool {
    a != BROADCAST_LSB && a != INVALID_NODE
}

/// Default node address derived from the MCU's 96-bit UID.
pub fn default_node_addr(uid: &[u8; 12]) -> u8 {
    let h = image::crc32(uid);
    let a = (h ^ (h >> 8) ^ (h >> 16) ^ (h >> 24)) as u8;
    // Map into 1..=254.
    (a % 254) + 1
}

/// Show slots (positions in a show) go from 0 to `MAX_SLOTS - 1`.
pub const MAX_SLOTS: u8 = 32;
pub const UNASSIGNED_SLOT: u8 = 0xFF;
pub const LEDS_PER_GIFT: usize = 4;

/// Bytes of image data per OTA chunk (multiple of the 8-byte flash word).
pub const OTA_CHUNK: usize = 24;
/// Chunks per [`OtaBitmap`] window.
pub const OTA_WINDOW_CHUNKS: usize = 192;
pub const OTA_BITMAP_BYTES: usize = OTA_WINDOW_CHUNKS / 8;
/// Most node addresses in one [`Broadcast::EnterBootloader`].
pub const ENTER_BOOT_MAX: usize = 30;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Rgb {
    pub r: u8,
    pub g: u8,
    pub b: u8,
}

impl Rgb {
    pub const BLACK: Rgb = Rgb::new(0, 0, 0);
    pub const fn new(r: u8, g: u8, b: u8) -> Self {
        Self { r, g, b }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    BufferFull,
    Malformed,
    UnknownKind(u8),
}

// ---------------------------------------------------------------------------
// Message kinds
// ---------------------------------------------------------------------------

pub mod kind {
    // Show broadcasts: [kind][seq][time_us: u32]...
    pub const SYNC: u8 = 0x01;
    pub const MUSIC: u8 = 0x02;
    pub const SOLID: u8 = 0x03;
    pub const LEDS: u8 = 0x04;
    pub const BLACKOUT: u8 = 0x05;
    // Management broadcasts.
    pub const SET_NODE_ADDR: u8 = 0x10;
    pub const BOOT_HOLD: u8 = 0x11;
    pub const ENTER_BOOT: u8 = 0x12;
    pub const SLEEP: u8 = 0x13;
    pub const WAKE: u8 = 0x14;
    // OTA (valid broadcast or unicast).
    pub const OTA_BEGIN: u8 = 0x20;
    pub const OTA_CHUNK: u8 = 0x21;
    // Unicast commands: [kind][seq]...
    pub const POLL: u8 = 0x40;
    pub const GET_IDENTITY: u8 = 0x41;
    pub const BLINK: u8 = 0x42;
    pub const SET_SLOT: u8 = 0x43;
    pub const REBOOT: u8 = 0x44;
    pub const SET_CONFIG: u8 = 0x45;
    pub const OTA_QUERY: u8 = 0x46;
    pub const OTA_COMMIT: u8 = 0x47;
    // Replies (ack payloads).
    pub const STATUS: u8 = 0xC0;
    pub const IDENTITY: u8 = 0xC1;
    pub const OTA_BITMAP: u8 = 0xC2;
    pub const CONFIG_RESULT: u8 = 0xC3;
}

/// Byte offset of the bridge timestamp inside show broadcasts
/// (for the bridge's STAMP flag).
pub const SHOW_TIMESTAMP_OFFSET: u8 = 2;

// ---------------------------------------------------------------------------
// Messages
// ---------------------------------------------------------------------------

pub mod music_flags {
    /// A beat starts in this frame.
    pub const BEAT: u8 = 1 << 0;
    /// The first beat of a bar.
    pub const DOWNBEAT: u8 = 1 << 1;
    /// Any strong onset (snare, hit).
    pub const ONSET: u8 = 1 << 2;
    /// Big energy rise ("drop").
    pub const DROP: u8 = 1 << 3;
    /// Audio is silent / paused.
    pub const SILENT: u8 = 1 << 4;
}

/// [`MusicFrame::palette`] value selecting the frame's own two colours
/// ([`MusicFrame::colors`]) instead of a built-in palette: a gradient from the
/// primary to the secondary colour and back, so `hue == 0` is exactly the
/// primary colour (e.g. `Solid` shows it unchanged).
pub const PALETTE_CUSTOM: u8 = 0xFF;
/// [`MusicFrame::mask`] addressing every gift, including gifts without a slot.
pub const MASK_ALL: u32 = u32::MAX;
/// Unit of [`MusicFrame::transition`].
pub const TRANSITION_UNIT_MS: u32 = 50;

/// Audio features + rendering parameters, sent ~100 times per second.
/// Gifts render the selected effect locally (see `lumigift-fx`).
///
/// The music fields (`bass` … `bpm`, `flags`) are all zero for purely
/// time-based looks; the effects run from the synced clock alone.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MusicFrame {
    pub effect: u8,
    /// Global brightness 0..=255 (before the power limiter).
    pub brightness: u8,
    /// Index into `lumigift_fx::PALETTES`, or [`PALETTE_CUSTOM`].
    pub palette: u8,
    /// See [`music_flags`].
    pub flags: u8,
    /// Band energies 0..=255, already normalised by the host.
    pub bass: u8,
    pub mid: u8,
    pub treble: u8,
    /// Overall loudness 0..=255.
    pub level: u8,
    /// Position within the current beat, 0..=255.
    pub beat_phase: u8,
    /// Beat index within the bar, low 2 bits; upper bits: bar counter.
    pub beat_in_bar: u8,
    /// Tempo in BPM (0 = unknown).
    pub bpm: u8,
    /// Base hue 0..=255.
    pub hue: u8,
    /// Effect-specific parameters: `[0]` speed, `[1]` size / width /
    /// density (0 = the effect's default, see `lumigift_fx::EFFECTS`).
    pub param: [u8; 2],
    /// Number of slots taking part (for spatial effects).
    pub slot_count: u8,
    /// Primary / secondary colour, used when `palette == PALETTE_CUSTOM`.
    pub colors: [Rgb; 2],
    /// Slot bitmask (bit `n` = slot `n`). A gift applies the frame only if
    /// its slot's bit is set; gifts without a slot only when the mask is
    /// [`MASK_ALL`]. Other gifts keep rendering their previous frame, so
    /// one frame per group gives different looks on different gifts.
    pub mask: u32,
    /// Crossfade time in [`TRANSITION_UNIT_MS`] units, applied when the look
    /// (effect, palette, colours, params, hue, brightness) changes. 0 = cut.
    pub transition: u8,
}

impl Default for MusicFrame {
    fn default() -> Self {
        Self {
            effect: 0,
            brightness: 0,
            palette: 0,
            flags: 0,
            bass: 0,
            mid: 0,
            treble: 0,
            level: 0,
            beat_phase: 0,
            beat_in_bar: 0,
            bpm: 0,
            hue: 0,
            param: [0; 2],
            slot_count: 0,
            colors: [Rgb::BLACK; 2],
            mask: MASK_ALL,
            transition: 0,
        }
    }
}

impl MusicFrame {
    /// Bytes after the 6-byte show header.
    pub const ENCODED_LEN: usize = 26;

    /// Does this frame apply to the gift at `slot` ([`UNASSIGNED_SLOT`] for none)?
    pub fn targets(&self, slot: u8) -> bool {
        if self.mask == MASK_ALL {
            return true;
        }
        slot < 32 && self.mask & (1 << slot) != 0
    }

    /// Mask with the bits of `slots` set (slots ≥ 32 are ignored).
    pub fn mask_of(slots: impl IntoIterator<Item = u8>) -> u32 {
        slots
            .into_iter()
            .filter(|&s| s < 32)
            .fold(0, |m, s| m | (1 << s))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Broadcast {
    /// Pure time/keepalive beacon.
    Sync {
        seq: u8,
        time_us: u32,
    },
    Music {
        seq: u8,
        time_us: u32,
        frame: MusicFrame,
    },
    /// One colour per slot for `count` consecutive slots (max 8).
    Solid {
        seq: u8,
        time_us: u32,
        first_slot: u8,
        count: u8,
        colors: [Rgb; 8],
    },
    /// Per-LED colours for up to two slots; `slots[1] == UNASSIGNED_SLOT` if unused.
    Leds {
        seq: u8,
        time_us: u32,
        slots: [u8; 2],
        colors: [[Rgb; LEDS_PER_GIFT]; 2],
    },
    Blackout {
        seq: u8,
        time_us: u32,
    },
    /// Change the node address of the gift whose UID matches.
    SetNodeAddr {
        uid: [u8; 12],
        node_addr: u8,
    },
    /// Keep any gift that just booted inside its bootloader. Bootloaders
    /// listen for it during the ~150 ms after reset on [`DEFAULT_CHANNEL`]
    /// (whatever channel is configured), so broadcasting it continuously
    /// while a gift is power-cycled always rescues it.
    BootHold,
    /// Apps whose node address is among `addrs[..count]` reboot into their
    /// bootloader right away (no ack, so the host polls them afterwards).
    EnterBootloader {
        count: u8,
        addrs: [u8; ENTER_BOOT_MAX],
    },
    /// Every gift dozes (see [`doze`]) until it hears [`Broadcast::Wake`].
    Sleep,
    /// Wakes dozing gifts. Sent back to back for [`doze::WAKE_BURST_MS`] so
    /// it falls into every gift's listen window; a no-op for awake gifts.
    Wake,
    OtaBegin(OtaBegin),
    OtaChunk(OtaChunk),
}

impl Broadcast {
    /// `EnterBootloader` for up to [`ENTER_BOOT_MAX`] node addresses (extra ones are ignored).
    pub fn enter_bootloader(targets: &[u8]) -> Self {
        let mut addrs = [0u8; ENTER_BOOT_MAX];
        let n = targets.len().min(ENTER_BOOT_MAX);
        addrs[..n].copy_from_slice(&targets[..n]);
        Broadcast::EnterBootloader {
            count: n as u8,
            addrs,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OtaBegin {
    pub session: u8,
    /// Total image length incl. header, bytes.
    pub image_len: u32,
    pub image_crc: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OtaChunk {
    pub session: u8,
    pub index: u16,
    pub data: [u8; OTA_CHUNK],
}

pub mod config_key {
    /// Max brightness cap 0..=255.
    pub const MAX_BRIGHTNESS: u8 = 1;
    /// Turn LEDs off after this many ms without show frames.
    pub const IDLE_TIMEOUT_MS: u8 = 2;
    /// Power budget for all four LEDs, in mA at 5 V.
    pub const POWER_BUDGET_MA: u8 = 3;
    /// Radio channel 0..=125 (stored in `ConfigRecord::channel`). The gift
    /// replies on the old channel and switches [`CHANNEL_SWITCH_DELAY_MS`]
    /// later, so the host can still collect the `ConfigResult`.
    pub const CHANNEL: u8 = 4;
    pub const CHANNEL_SWITCH_DELAY_MS: u32 = 1000;
}

/// Low-power doze. A dozing gift keeps its LEDs off, its nRF in standby and
/// its MCU on the 32 kHz LSI clock, and turns the receiver on for
/// [`LISTEN_US`] every [`PERIOD_MS`]. Only [`Broadcast::Wake`] (or a unicast
/// to it) wakes it; show frames and beacons do not.
pub mod doze {
    pub const PERIOD_MS: u32 = 1000;
    pub const LISTEN_US: u32 = 2000;
    /// Length of the host's `Wake` burst: a full period plus a window, with
    /// margin for the gifts' ±10 % LSI.
    pub const WAKE_BURST_MS: u32 = 1300;
    /// A gift dozes by itself after this long without any valid packet from
    /// a host (the host polls connected gifts several times a second).
    pub const AUTO_AFTER_MS: u32 = 60_000;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Unicast {
    Poll {
        seq: u8,
    },
    GetIdentity {
        seq: u8,
    },
    Blink {
        seq: u8,
        color: Rgb,
        count: u8,
        period_10ms: u8,
    },
    SetSlot {
        seq: u8,
        slot: u8,
    },
    Reboot {
        seq: u8,
        into_bootloader: bool,
    },
    SetConfig {
        seq: u8,
        key: u8,
        value: u16,
    },
    /// Reply: [`Reply::OtaBitmap`] of the first window `>= window` with
    /// missing chunks (or `window` itself if none has any).
    OtaQuery {
        seq: u8,
        session: u8,
        window: u8,
    },
    OtaCommit {
        seq: u8,
        session: u8,
    },
}

impl Unicast {
    pub fn seq(&self) -> u8 {
        match *self {
            Unicast::Poll { seq }
            | Unicast::GetIdentity { seq }
            | Unicast::Blink { seq, .. }
            | Unicast::SetSlot { seq, .. }
            | Unicast::Reboot { seq, .. }
            | Unicast::SetConfig { seq, .. }
            | Unicast::OtaQuery { seq, .. }
            | Unicast::OtaCommit { seq, .. } => seq,
        }
    }
}

pub mod status_flags {
    pub const IN_BOOTLOADER: u8 = 1 << 0;
    pub const SLOT_ASSIGNED: u8 = 1 << 1;
    pub const TIME_SYNCED: u8 = 1 << 2;
    pub const LOW_BATTERY: u8 = 1 << 3;
    pub const LEDS_ACTIVE: u8 = 1 << 4;
    pub const BLINKING: u8 = 1 << 5;
    /// The bootloader pages are write-protected by the option bytes (WRP).
    pub const BOOT_PROTECTED: u8 = 1 << 6;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum ResetReason {
    Unknown = 0,
    PowerOn = 1,
    Pin = 2,
    Software = 3,
    Watchdog = 4,
    Brownout = 5,
    LowPower = 6,
}

impl ResetReason {
    pub fn from_u8(v: u8) -> Self {
        match v {
            1 => Self::PowerOn,
            2 => Self::Pin,
            3 => Self::Software,
            4 => Self::Watchdog,
            5 => Self::Brownout,
            6 => Self::LowPower,
            _ => Self::Unknown,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Status {
    /// Seq of the last unicast command handled.
    pub echo: u8,
    /// See [`status_flags`].
    pub flags: u8,
    pub slot: u8,
    pub uptime_ms: u32,
    /// Running image's `fw_version` (bootloader version when in bootloader).
    pub fw_version: u32,
    pub vbat_mv: u16,
    /// Show broadcasts received (wrapping).
    pub bcast_rx: u16,
    /// Show broadcasts missed, from sequence gaps (wrapping).
    pub bcast_lost: u16,
    /// [`ResetReason`] as u8.
    pub reset_reason: u8,
    /// Resets since power-on.
    pub resets: u16,
    pub effect: u8,
    /// Estimated clock sync error, µs (saturating).
    pub sync_err_us: u16,
    pub temp_c: i8,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Identity {
    pub echo: u8,
    pub uid: [u8; 12],
    pub hw_rev: u8,
    pub fw_version: u32,
    pub boot_version: u16,
    pub slot: u8,
    pub node_addr: u8,
    pub flags: u8,
    /// `app_crc` of the installed image's header (0 if there is no valid
    /// image), so the host can tell whether an update is needed.
    pub app_crc: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum OtaState {
    /// No session.
    Idle = 0,
    /// Not sent by current bootloaders (they erase in the background while
    /// already `Receiving`); kept for wire compatibility.
    Erasing = 1,
    /// Accepting chunks.
    Receiving = 2,
    /// Image complete and CRC verified; booting it shortly.
    Verified = 3,
    /// CRC mismatch or bad header.
    Failed = 4,
    /// Running the app: OTA is handled by the bootloader.
    NotInBootloader = 5,
}

impl OtaState {
    pub fn from_u8(v: u8) -> Option<Self> {
        Some(match v {
            0 => Self::Idle,
            1 => Self::Erasing,
            2 => Self::Receiving,
            3 => Self::Verified,
            4 => Self::Failed,
            5 => Self::NotInBootloader,
            _ => return None,
        })
    }
}

/// Reply to `OtaQuery` / `OtaCommit`: one window of the received-chunk map.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OtaBitmap {
    pub echo: u8,
    pub session: u8,
    pub state: OtaState,
    /// Window index: `bits` covers chunks `window * 192 .. window * 192 + 192`.
    pub window: u8,
    /// Chunks still missing in the whole image.
    pub missing_count: u16,
    /// Bit `i % 8` of byte `i / 8` set = chunk `window * 192 + i` received
    /// (chunks past the end of the image read as received).
    pub bits: [u8; OTA_BITMAP_BYTES],
}

impl OtaBitmap {
    /// First chunk index of `window`.
    pub const fn window_start(window: u8) -> u32 {
        window as u32 * OTA_WINDOW_CHUNKS as u32
    }

    /// Number of windows for `chunks` chunks.
    pub const fn windows(chunks: u32) -> u32 {
        chunks.div_ceil(OTA_WINDOW_CHUNKS as u32)
    }

    pub fn received(&self, i: usize) -> bool {
        self.bits[i / 8] & (1 << (i % 8)) != 0
    }

    /// Missing chunk indices (absolute) of this window, below `chunks`.
    pub fn missing(&self, chunks: u32) -> impl Iterator<Item = u16> + '_ {
        let start = Self::window_start(self.window);
        (0..OTA_WINDOW_CHUNKS)
            .filter(move |&i| start + (i as u32) < chunks && !self.received(i))
            .map(move |i| (start + i as u32) as u16)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reply {
    Status(Status),
    Identity(Identity),
    OtaBitmap(OtaBitmap),
    ConfigResult {
        echo: u8,
        key: u8,
        ok: bool,
        value: u16,
    },
}

/// Anything the host can send.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Packet {
    Broadcast(Broadcast),
    Unicast(Unicast),
}

// ---------------------------------------------------------------------------
// Encoding
// ---------------------------------------------------------------------------

impl Packet {
    pub fn encode(&self, out: &mut [u8; MAX_PAYLOAD]) -> usize {
        let mut w = W { buf: out, pos: 0 };
        match *self {
            Packet::Broadcast(b) => match b {
                Broadcast::Sync { seq, time_us } => w.show(kind::SYNC, seq, time_us),
                Broadcast::Music {
                    seq,
                    time_us,
                    frame: f,
                } => {
                    w.show(kind::MUSIC, seq, time_us);
                    w.bytes(&[
                        f.effect,
                        f.brightness,
                        f.palette,
                        f.flags,
                        f.bass,
                        f.mid,
                        f.treble,
                        f.level,
                        f.beat_phase,
                        f.beat_in_bar,
                        f.bpm,
                        f.hue,
                        f.param[0],
                        f.param[1],
                        f.slot_count,
                    ]);
                    w.rgb(f.colors[0]);
                    w.rgb(f.colors[1]);
                    w.bytes(&f.mask.to_le_bytes());
                    w.bytes(&[f.transition]);
                }
                Broadcast::Solid {
                    seq,
                    time_us,
                    first_slot,
                    count,
                    colors,
                } => {
                    w.show(kind::SOLID, seq, time_us);
                    let count = count.min(8);
                    w.bytes(&[first_slot, count]);
                    for c in &colors[..count as usize] {
                        w.rgb(*c);
                    }
                }
                Broadcast::Leds {
                    seq,
                    time_us,
                    slots,
                    colors,
                } => {
                    w.show(kind::LEDS, seq, time_us);
                    for i in 0..2 {
                        if i == 1 && slots[1] == UNASSIGNED_SLOT {
                            break;
                        }
                        w.bytes(&[slots[i]]);
                        for c in colors[i] {
                            w.rgb(c);
                        }
                    }
                }
                Broadcast::Blackout { seq, time_us } => w.show(kind::BLACKOUT, seq, time_us),
                Broadcast::SetNodeAddr { uid, node_addr } => {
                    w.bytes(&[kind::SET_NODE_ADDR]);
                    w.bytes(&uid);
                    w.bytes(&[node_addr]);
                }
                Broadcast::BootHold => w.bytes(&[kind::BOOT_HOLD]),
                Broadcast::Sleep => w.bytes(&[kind::SLEEP]),
                Broadcast::Wake => w.bytes(&[kind::WAKE]),
                Broadcast::EnterBootloader { count, addrs } => {
                    let n = (count as usize).min(ENTER_BOOT_MAX);
                    w.bytes(&[kind::ENTER_BOOT, n as u8]);
                    w.bytes(&addrs[..n]);
                }
                Broadcast::OtaBegin(o) => {
                    w.bytes(&[kind::OTA_BEGIN, o.session]);
                    w.bytes(&o.image_len.to_le_bytes());
                    w.bytes(&o.image_crc.to_le_bytes());
                }
                Broadcast::OtaChunk(c) => {
                    w.bytes(&[kind::OTA_CHUNK, c.session]);
                    w.bytes(&c.index.to_le_bytes());
                    w.bytes(&c.data);
                }
            },
            Packet::Unicast(u) => match u {
                Unicast::Poll { seq } => w.bytes(&[kind::POLL, seq]),
                Unicast::GetIdentity { seq } => w.bytes(&[kind::GET_IDENTITY, seq]),
                Unicast::Blink {
                    seq,
                    color,
                    count,
                    period_10ms,
                } => {
                    w.bytes(&[kind::BLINK, seq]);
                    w.rgb(color);
                    w.bytes(&[count, period_10ms]);
                }
                Unicast::SetSlot { seq, slot } => w.bytes(&[kind::SET_SLOT, seq, slot]),
                Unicast::Reboot {
                    seq,
                    into_bootloader,
                } => w.bytes(&[kind::REBOOT, seq, into_bootloader as u8]),
                Unicast::SetConfig { seq, key, value } => {
                    w.bytes(&[kind::SET_CONFIG, seq, key]);
                    w.bytes(&value.to_le_bytes());
                }
                Unicast::OtaQuery {
                    seq,
                    session,
                    window,
                } => w.bytes(&[kind::OTA_QUERY, seq, session, window]),
                Unicast::OtaCommit { seq, session } => w.bytes(&[kind::OTA_COMMIT, seq, session]),
            },
        }
        w.pos
    }

    pub fn decode(b: &[u8]) -> Result<Self, Error> {
        let mut r = R(b);
        let k = r.u8()?;
        let p = match k {
            kind::SYNC | kind::MUSIC | kind::SOLID | kind::LEDS | kind::BLACKOUT => {
                let seq = r.u8()?;
                let time_us = r.u32()?;
                Packet::Broadcast(match k {
                    kind::SYNC => Broadcast::Sync { seq, time_us },
                    kind::BLACKOUT => Broadcast::Blackout { seq, time_us },
                    kind::MUSIC => {
                        let v: [u8; MusicFrame::ENCODED_LEN] = r.array()?;
                        let rgb = |i: usize| Rgb::new(v[i], v[i + 1], v[i + 2]);
                        Broadcast::Music {
                            seq,
                            time_us,
                            frame: MusicFrame {
                                effect: v[0],
                                brightness: v[1],
                                palette: v[2],
                                flags: v[3],
                                bass: v[4],
                                mid: v[5],
                                treble: v[6],
                                level: v[7],
                                beat_phase: v[8],
                                beat_in_bar: v[9],
                                bpm: v[10],
                                hue: v[11],
                                param: [v[12], v[13]],
                                slot_count: v[14],
                                colors: [rgb(15), rgb(18)],
                                mask: u32::from_le_bytes([v[21], v[22], v[23], v[24]]),
                                transition: v[25],
                            },
                        }
                    }
                    kind::SOLID => {
                        let first_slot = r.u8()?;
                        let count = r.u8()?;
                        if count > 8 {
                            return Err(Error::Malformed);
                        }
                        let mut colors = [Rgb::BLACK; 8];
                        for c in &mut colors[..count as usize] {
                            *c = r.rgb()?;
                        }
                        Broadcast::Solid {
                            seq,
                            time_us,
                            first_slot,
                            count,
                            colors,
                        }
                    }
                    _ => {
                        let mut slots = [UNASSIGNED_SLOT; 2];
                        let mut colors = [[Rgb::BLACK; LEDS_PER_GIFT]; 2];
                        for i in 0..2 {
                            if i == 1 && r.0.is_empty() {
                                break;
                            }
                            slots[i] = r.u8()?;
                            for c in &mut colors[i] {
                                *c = r.rgb()?;
                            }
                        }
                        Broadcast::Leds {
                            seq,
                            time_us,
                            slots,
                            colors,
                        }
                    }
                })
            }
            kind::SET_NODE_ADDR => Packet::Broadcast(Broadcast::SetNodeAddr {
                uid: r.array()?,
                node_addr: r.u8()?,
            }),
            kind::BOOT_HOLD => Packet::Broadcast(Broadcast::BootHold),
            kind::SLEEP => Packet::Broadcast(Broadcast::Sleep),
            kind::WAKE => Packet::Broadcast(Broadcast::Wake),
            kind::ENTER_BOOT => {
                let n = r.u8()? as usize;
                if n > ENTER_BOOT_MAX || r.0.len() < n {
                    return Err(Error::Malformed);
                }
                let mut addrs = [0u8; ENTER_BOOT_MAX];
                addrs[..n].copy_from_slice(&r.0[..n]);
                Packet::Broadcast(Broadcast::EnterBootloader {
                    count: n as u8,
                    addrs,
                })
            }
            kind::OTA_BEGIN => Packet::Broadcast(Broadcast::OtaBegin(OtaBegin {
                session: r.u8()?,
                image_len: r.u32()?,
                image_crc: r.u32()?,
            })),
            kind::OTA_CHUNK => Packet::Broadcast(Broadcast::OtaChunk(OtaChunk {
                session: r.u8()?,
                index: r.u16()?,
                data: r.array()?,
            })),
            kind::POLL => Packet::Unicast(Unicast::Poll { seq: r.u8()? }),
            kind::GET_IDENTITY => Packet::Unicast(Unicast::GetIdentity { seq: r.u8()? }),
            kind::BLINK => Packet::Unicast(Unicast::Blink {
                seq: r.u8()?,
                color: r.rgb()?,
                count: r.u8()?,
                period_10ms: r.u8()?,
            }),
            kind::SET_SLOT => Packet::Unicast(Unicast::SetSlot {
                seq: r.u8()?,
                slot: r.u8()?,
            }),
            kind::REBOOT => Packet::Unicast(Unicast::Reboot {
                seq: r.u8()?,
                into_bootloader: r.u8()? != 0,
            }),
            kind::SET_CONFIG => Packet::Unicast(Unicast::SetConfig {
                seq: r.u8()?,
                key: r.u8()?,
                value: r.u16()?,
            }),
            kind::OTA_QUERY => Packet::Unicast(Unicast::OtaQuery {
                seq: r.u8()?,
                session: r.u8()?,
                window: r.u8()?,
            }),
            kind::OTA_COMMIT => Packet::Unicast(Unicast::OtaCommit {
                seq: r.u8()?,
                session: r.u8()?,
            }),
            other => return Err(Error::UnknownKind(other)),
        };
        // Trailing bytes are tolerated so future versions can append fields.
        Ok(p)
    }
}

impl Reply {
    pub fn encode(&self, out: &mut [u8; MAX_PAYLOAD]) -> usize {
        let mut w = W { buf: out, pos: 0 };
        match *self {
            Reply::Status(s) => {
                w.bytes(&[kind::STATUS, s.echo, s.flags, s.slot]);
                w.bytes(&s.uptime_ms.to_le_bytes());
                w.bytes(&s.fw_version.to_le_bytes());
                w.bytes(&s.vbat_mv.to_le_bytes());
                w.bytes(&s.bcast_rx.to_le_bytes());
                w.bytes(&s.bcast_lost.to_le_bytes());
                w.bytes(&[s.reset_reason]);
                w.bytes(&s.resets.to_le_bytes());
                w.bytes(&[s.effect]);
                w.bytes(&s.sync_err_us.to_le_bytes());
                w.bytes(&[s.temp_c as u8]);
            }
            Reply::Identity(i) => {
                w.bytes(&[kind::IDENTITY, i.echo]);
                w.bytes(&i.uid);
                w.bytes(&[i.hw_rev]);
                w.bytes(&i.fw_version.to_le_bytes());
                w.bytes(&i.boot_version.to_le_bytes());
                w.bytes(&[i.slot, i.node_addr, i.flags]);
                w.bytes(&i.app_crc.to_le_bytes());
            }
            Reply::OtaBitmap(o) => {
                w.bytes(&[kind::OTA_BITMAP, o.echo, o.session, o.state as u8, o.window]);
                w.bytes(&o.missing_count.to_le_bytes());
                w.bytes(&o.bits);
            }
            Reply::ConfigResult {
                echo,
                key,
                ok,
                value,
            } => {
                w.bytes(&[kind::CONFIG_RESULT, echo, key, ok as u8]);
                w.bytes(&value.to_le_bytes());
            }
        }
        w.pos
    }

    pub fn decode(b: &[u8]) -> Result<Self, Error> {
        let mut r = R(b);
        Ok(match r.u8()? {
            kind::STATUS => Reply::Status(Status {
                echo: r.u8()?,
                flags: r.u8()?,
                slot: r.u8()?,
                uptime_ms: r.u32()?,
                fw_version: r.u32()?,
                vbat_mv: r.u16()?,
                bcast_rx: r.u16()?,
                bcast_lost: r.u16()?,
                reset_reason: r.u8()?,
                resets: r.u16()?,
                effect: r.u8()?,
                sync_err_us: r.u16()?,
                temp_c: r.u8()? as i8,
            }),
            kind::IDENTITY => Reply::Identity(Identity {
                echo: r.u8()?,
                uid: r.array()?,
                hw_rev: r.u8()?,
                fw_version: r.u32()?,
                boot_version: r.u16()?,
                slot: r.u8()?,
                node_addr: r.u8()?,
                flags: r.u8()?,
                app_crc: r.u32()?,
            }),
            kind::OTA_BITMAP => Reply::OtaBitmap(OtaBitmap {
                echo: r.u8()?,
                session: r.u8()?,
                state: OtaState::from_u8(r.u8()?).ok_or(Error::Malformed)?,
                window: r.u8()?,
                missing_count: r.u16()?,
                bits: r.array()?,
            }),
            kind::CONFIG_RESULT => Reply::ConfigResult {
                echo: r.u8()?,
                key: r.u8()?,
                ok: r.u8()? != 0,
                value: r.u16()?,
            },
            other => return Err(Error::UnknownKind(other)),
        })
    }
}

struct W<'a> {
    buf: &'a mut [u8; MAX_PAYLOAD],
    pos: usize,
}

impl W<'_> {
    // All encoders are statically within 32 bytes (checked by tests).
    fn bytes(&mut self, b: &[u8]) {
        self.buf[self.pos..self.pos + b.len()].copy_from_slice(b);
        self.pos += b.len();
    }
    fn show(&mut self, k: u8, seq: u8, t: u32) {
        self.bytes(&[k, seq]);
        self.bytes(&t.to_le_bytes());
    }
    fn rgb(&mut self, c: Rgb) {
        self.bytes(&[c.r, c.g, c.b]);
    }
}

struct R<'a>(&'a [u8]);

impl<'a> R<'a> {
    fn u8(&mut self) -> Result<u8, Error> {
        let (&v, rest) = self.0.split_first().ok_or(Error::Malformed)?;
        self.0 = rest;
        Ok(v)
    }
    fn u16(&mut self) -> Result<u16, Error> {
        Ok(u16::from_le_bytes(self.array()?))
    }
    fn u32(&mut self) -> Result<u32, Error> {
        Ok(u32::from_le_bytes(self.array()?))
    }
    fn rgb(&mut self) -> Result<Rgb, Error> {
        let [r, g, b] = self.array()?;
        Ok(Rgb { r, g, b })
    }
    fn array<const N: usize>(&mut self) -> Result<[u8; N], Error> {
        if self.0.len() < N {
            return Err(Error::Malformed);
        }
        let (a, rest) = self.0.split_at(N);
        self.0 = rest;
        Ok(a.try_into().unwrap())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rt(p: Packet) -> usize {
        let mut buf = [0u8; MAX_PAYLOAD];
        let n = p.encode(&mut buf);
        assert_eq!(Packet::decode(&buf[..n]).unwrap(), p);
        n
    }

    fn rt_reply(r: Reply) -> usize {
        let mut buf = [0u8; MAX_PAYLOAD];
        let n = r.encode(&mut buf);
        assert_eq!(Reply::decode(&buf[..n]).unwrap(), r);
        n
    }

    #[test]
    fn packets_roundtrip_and_fit() {
        let c = Rgb::new(1, 2, 3);
        rt(Packet::Broadcast(Broadcast::Sync { seq: 1, time_us: 2 }));
        rt(Packet::Broadcast(Broadcast::Music {
            seq: 3,
            time_us: 4,
            frame: MusicFrame {
                bass: 9,
                param: [1, 2],
                ..Default::default()
            },
        }));
        // Every field distinct, and the whole show packet fills the payload.
        let full = MusicFrame {
            effect: 1,
            brightness: 2,
            palette: PALETTE_CUSTOM,
            flags: 4,
            bass: 5,
            mid: 6,
            treble: 7,
            level: 8,
            beat_phase: 9,
            beat_in_bar: 10,
            bpm: 11,
            hue: 12,
            param: [13, 14],
            slot_count: 15,
            colors: [Rgb::new(16, 17, 18), Rgb::new(19, 20, 21)],
            mask: 0x8765_4321,
            transition: 22,
        };
        assert_eq!(
            rt(Packet::Broadcast(Broadcast::Music {
                seq: 3,
                time_us: 4,
                frame: full,
            })),
            32
        );
        assert_eq!(6 + MusicFrame::ENCODED_LEN, MAX_PAYLOAD);
        // Tolerant of trailing bytes, strict about missing ones.
        let mut buf = [0u8; MAX_PAYLOAD];
        let n = Packet::Broadcast(Broadcast::Music {
            seq: 3,
            time_us: 4,
            frame: full,
        })
        .encode(&mut buf);
        assert!(Packet::decode(&buf[..n - 1]).is_err());
        let mut long = [0u8; MAX_PAYLOAD + 2];
        long[..n].copy_from_slice(&buf[..n]);
        assert!(matches!(
            Packet::decode(&long),
            Ok(Packet::Broadcast(Broadcast::Music { frame, .. })) if frame == full
        ));
        assert_eq!(
            rt(Packet::Broadcast(Broadcast::Solid {
                seq: 1,
                time_us: 0,
                first_slot: 0,
                count: 8,
                colors: [c; 8]
            })),
            32
        );
        rt(Packet::Broadcast(Broadcast::Solid {
            seq: 1,
            time_us: 0,
            first_slot: 4,
            count: 2,
            colors: [
                c,
                c,
                Rgb::BLACK,
                Rgb::BLACK,
                Rgb::BLACK,
                Rgb::BLACK,
                Rgb::BLACK,
                Rgb::BLACK,
            ],
        }));
        assert_eq!(
            rt(Packet::Broadcast(Broadcast::Leds {
                seq: 1,
                time_us: 0,
                slots: [0, 1],
                colors: [[c; 4]; 2]
            })),
            32
        );
        rt(Packet::Broadcast(Broadcast::Leds {
            seq: 1,
            time_us: 0,
            slots: [5, UNASSIGNED_SLOT],
            colors: [[c; 4], [Rgb::BLACK; 4]],
        }));
        rt(Packet::Broadcast(Broadcast::Blackout {
            seq: 1,
            time_us: 0,
        }));
        rt(Packet::Broadcast(Broadcast::SetNodeAddr {
            uid: [3; 12],
            node_addr: 9,
        }));
        rt(Packet::Broadcast(Broadcast::BootHold));
        rt(Packet::Broadcast(Broadcast::Sleep));
        rt(Packet::Broadcast(Broadcast::Wake));
        assert_eq!(
            rt(Packet::Broadcast(Broadcast::enter_bootloader(&[7; 40]))),
            32
        );
        rt(Packet::Broadcast(Broadcast::enter_bootloader(&[1, 2, 3])));
        rt(Packet::Broadcast(Broadcast::enter_bootloader(&[])));
        rt(Packet::Broadcast(Broadcast::OtaBegin(OtaBegin {
            session: 1,
            image_len: 1000,
            image_crc: 0xABCD,
        })));
        assert_eq!(
            rt(Packet::Broadcast(Broadcast::OtaChunk(OtaChunk {
                session: 1,
                index: 900,
                data: [7; OTA_CHUNK]
            }))),
            28
        );
        rt(Packet::Unicast(Unicast::Poll { seq: 1 }));
        rt(Packet::Unicast(Unicast::GetIdentity { seq: 1 }));
        rt(Packet::Unicast(Unicast::Blink {
            seq: 1,
            color: c,
            count: 3,
            period_10ms: 20,
        }));
        rt(Packet::Unicast(Unicast::SetSlot { seq: 1, slot: 4 }));
        rt(Packet::Unicast(Unicast::Reboot {
            seq: 1,
            into_bootloader: true,
        }));
        rt(Packet::Unicast(Unicast::SetConfig {
            seq: 1,
            key: 2,
            value: 500,
        }));
        rt(Packet::Unicast(Unicast::OtaQuery {
            seq: 1,
            session: 2,
            window: 3,
        }));
        rt(Packet::Unicast(Unicast::OtaCommit { seq: 1, session: 2 }));
    }

    #[test]
    fn replies_roundtrip_and_fit() {
        rt_reply(Reply::Status(Status {
            echo: 1,
            uptime_ms: 123456,
            temp_c: -5,
            sync_err_us: 40,
            ..Default::default()
        }));
        assert_eq!(
            rt_reply(Reply::Identity(Identity {
                uid: [1; 12],
                fw_version: 7,
                app_crc: 0xDEAD_BEEF,
                ..Default::default()
            })),
            28
        );
        let mut bits = [0u8; OTA_BITMAP_BYTES];
        bits[0] = 0b1111_1101;
        bits[23] = 0x7F;
        let b = OtaBitmap {
            echo: 1,
            session: 1,
            state: OtaState::Receiving,
            window: 2,
            missing_count: 500,
            bits,
        };
        assert_eq!(rt_reply(Reply::OtaBitmap(b)), 31);
        assert_eq!(b.missing(10_000).count(), 1 + 22 * 8 + 1);
        assert_eq!(b.missing(10_000).next(), Some(385));
        assert_eq!(b.missing(10_000).last(), Some(384 + 191));
        // Chunks past the image are never reported missing.
        assert_eq!(b.missing(390).count(), 1);
        rt_reply(Reply::ConfigResult {
            echo: 1,
            key: 1,
            ok: true,
            value: 200,
        });
    }

    #[test]
    fn music_frame_mask() {
        let all = MusicFrame::default();
        assert_eq!(all.mask, MASK_ALL);
        assert!(all.targets(0) && all.targets(31) && all.targets(UNASSIGNED_SLOT));
        let f = MusicFrame {
            mask: MusicFrame::mask_of([0, 5, 31, 40]),
            ..Default::default()
        };
        assert_eq!(f.mask, 1 | 1 << 5 | 1 << 31);
        assert!(f.targets(0) && f.targets(5) && f.targets(31));
        assert!(!f.targets(1) && !f.targets(UNASSIGNED_SLOT) && !f.targets(40));
        let none = MusicFrame {
            mask: 0,
            ..Default::default()
        };
        assert!(!none.targets(0));
    }

    #[test]
    fn node_addr_in_range() {
        for i in 0..=255u8 {
            let a = default_node_addr(&[i; 12]);
            assert!(valid_node_addr(a));
        }
    }

    #[test]
    fn timestamp_offset_matches_encoding() {
        let mut buf = [0u8; MAX_PAYLOAD];
        Packet::Broadcast(Broadcast::Sync {
            seq: 0,
            time_us: 0x11223344,
        })
        .encode(&mut buf);
        let o = SHOW_TIMESTAMP_OFFSET as usize;
        assert_eq!(&buf[o..o + 4], &0x11223344u32.to_le_bytes());
    }
}
