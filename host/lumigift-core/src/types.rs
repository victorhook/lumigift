//! Public data types: identifiers, show output, snapshot views, results.

use std::fmt;
use std::str::FromStr;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use crossbeam_channel::{Receiver, RecvTimeoutError, TryRecvError};
use lumigift_proto::{LEDS_PER_GIFT, MusicFrame, ResetReason, Rgb, Status};
use nrf24_usb_proto::{Info, Stats};

use crate::config::RadioSettings;

// ---------------------------------------------------------------------------
// Identifiers
// ---------------------------------------------------------------------------

/// A gift's 96-bit MCU unique ID. Displays / parses as 24 hex digits.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct Uid(pub [u8; 12]);

impl Uid {
    pub fn to_hex(&self) -> String {
        self.0.iter().map(|b| format!("{b:02x}")).collect()
    }
    /// The last 3 bytes in hex, handy for compact tables.
    pub fn short(&self) -> String {
        self.0[9..].iter().map(|b| format!("{b:02x}")).collect()
    }
}

impl fmt::Display for Uid {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.to_hex())
    }
}

impl fmt::Debug for Uid {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Uid({})", self.to_hex())
    }
}

impl FromStr for Uid {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, String> {
        let s = s.trim();
        if s.len() != 24 {
            return Err(format!("UID must be 24 hex digits, got {:?}", s));
        }
        let mut out = [0u8; 12];
        for (i, o) in out.iter_mut().enumerate() {
            *o = u8::from_str_radix(&s[i * 2..i * 2 + 2], 16)
                .map_err(|e| format!("bad UID {s:?}: {e}"))?;
        }
        Ok(Uid(out))
    }
}

// ---------------------------------------------------------------------------
// Show output
// ---------------------------------------------------------------------------

/// Most Music layers broadcast per 10 ms show tick (see [`ShowOutput::Layers`]).
pub const MAX_LAYERS_PER_TICK: usize = 8;

/// Music flags that mark a moment rather than a state. The link sends each
/// one exactly once (on the next show tick after the frame that set it), so
/// a UI updating faster or slower than the show rate neither repeats nor
/// drops beats and onsets.
pub const EDGE_FLAGS: u8 = lumigift_proto::music_flags::BEAT
    | lumigift_proto::music_flags::DOWNBEAT
    | lumigift_proto::music_flags::ONSET
    | lumigift_proto::music_flags::DROP;

/// What the show stream broadcasts every tick (latest value wins).
/// Set with [`Link::set_show`](crate::Link::set_show).
#[derive(Debug, Clone, PartialEq, Default)]
pub enum ShowOutput {
    /// Audio features; every gift renders the effect locally.
    Music(MusicFrame),
    /// Several Music frames, one per group of gifts: each gift applies only
    /// the frames whose [`MusicFrame::mask`] includes its slot and keeps
    /// rendering its last look otherwise. Every layer is broadcast each tick
    /// (up to [`MAX_LAYERS_PER_TICK`]; more are sent round-robin). Put the
    /// broadest layer first: a later layer wins on gifts in both masks.
    Layers(Vec<MusicFrame>),
    /// One colour per slot (index = slot). Packed 8 slots per packet.
    Solid(Vec<Rgb>),
    /// Four LED colours per slot (index = slot). Packed 2 slots per packet.
    Leds(Vec<[Rgb; LEDS_PER_GIFT]>),
    /// All LEDs off.
    Blackout,
    /// Nothing to show: only a `Sync` beacon at ~10 Hz keeps clocks locked.
    #[default]
    Idle,
}

impl ShowOutput {
    /// Something the gifts should light up for (not `Idle` / `Blackout`).
    pub fn is_active(&self) -> bool {
        !matches!(self, ShowOutput::Idle | ShowOutput::Blackout)
    }
}

// ---------------------------------------------------------------------------
// Errors and async results
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum LinkError {
    #[error("bridge not connected")]
    NotConnected,
    #[error("no such gift: {0}")]
    NoSuchGift(String),
    #[error("gift did not acknowledge")]
    NoAck,
    #[error("no matching reply from gift")]
    NoReply,
    #[error("unexpected reply: {0}")]
    UnexpectedReply(String),
    #[error("rejected: {0}")]
    Rejected(String),
    #[error("timed out: {0}")]
    Timeout(String),
    #[error("invalid firmware image: {0}")]
    InvalidImage(String),
    #[error("busy: {0}")]
    Busy(String),
    #[error("cancelled")]
    Cancelled,
    #[error("link shut down")]
    Shutdown,
    #[error("{0}")]
    Other(String),
}

/// The eventual result of an asynchronous [`Link`](crate::Link) operation.
/// Poll it from a GUI loop with [`Pending::try_take`] or block with
/// [`Pending::wait`]. Dropping it does not cancel the operation.
#[must_use = "the result of the operation is reported through this handle"]
pub struct Pending<T> {
    rx: Receiver<Result<T, LinkError>>,
    done: Option<Result<T, LinkError>>,
}

impl<T> Pending<T> {
    pub(crate) fn new(rx: Receiver<Result<T, LinkError>>) -> Self {
        Self { rx, done: None }
    }
    pub(crate) fn ready(r: Result<T, LinkError>) -> Self {
        let (_tx, rx) = crossbeam_channel::bounded(1);
        Self { rx, done: Some(r) }
    }
    fn fetch(&mut self) {
        if self.done.is_none() {
            match self.rx.try_recv() {
                Ok(r) => self.done = Some(r),
                Err(TryRecvError::Disconnected) => self.done = Some(Err(LinkError::Shutdown)),
                Err(TryRecvError::Empty) => {}
            }
        }
    }
    /// Has the operation finished?
    pub fn is_done(&mut self) -> bool {
        self.fetch();
        self.done.is_some()
    }
    /// Take the result if finished (returns `None` again afterwards).
    pub fn try_take(&mut self) -> Option<Result<T, LinkError>> {
        self.fetch();
        self.done.take()
    }
    /// Block until finished.
    pub fn wait(mut self) -> Result<T, LinkError> {
        if let Some(r) = self.done.take() {
            return r;
        }
        self.rx.recv().unwrap_or(Err(LinkError::Shutdown))
    }
    /// Block at most `timeout`; `None` if still running.
    pub fn wait_timeout(&mut self, timeout: Duration) -> Option<Result<T, LinkError>> {
        if let Some(r) = self.done.take() {
            return Some(r);
        }
        match self.rx.recv_timeout(timeout) {
            Ok(r) => Some(r),
            Err(RecvTimeoutError::Timeout) => None,
            Err(RecvTimeoutError::Disconnected) => Some(Err(LinkError::Shutdown)),
        }
    }
}

/// Which gifts a command applies to.
#[derive(Debug, Clone, PartialEq)]
pub enum Target {
    Gift(Uid),
    Gifts(Vec<Uid>),
    /// Every gift that is currently online.
    All,
}

impl From<Uid> for Target {
    fn from(u: Uid) -> Self {
        Target::Gift(u)
    }
}

impl From<Vec<Uid>> for Target {
    fn from(v: Vec<Uid>) -> Self {
        Target::Gifts(v)
    }
}

/// Per-gift outcome of a (possibly multi-gift) command.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct CmdReport {
    pub ok: Vec<Uid>,
    pub failed: Vec<(Uid, LinkError)>,
}

impl CmdReport {
    pub fn all_ok(&self) -> bool {
        self.failed.is_empty()
    }
}

impl fmt::Display for CmdReport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} ok", self.ok.len())?;
        if !self.failed.is_empty() {
            write!(f, ", {} failed:", self.failed.len())?;
            for (u, e) in &self.failed {
                write!(f, " [{} {e}]", u.short())?;
            }
        }
        Ok(())
    }
}

/// Slot auto-assignment policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SlotAssign {
    /// Slots 0..n in ascending UID order (online gifts only).
    ByUid,
    /// Keep valid, unique slots; give the rest the lowest free slots (in UID order).
    KeepExisting,
}

// ---------------------------------------------------------------------------
// Host-side metadata
// ---------------------------------------------------------------------------

/// User-given data stored in `~/.config/lumigift/gifts.json`, keyed by UID.
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct GiftMeta {
    pub name: Option<String>,
    /// Colour tag for the UI, `[r, g, b]`.
    pub color: Option<[u8; 3]>,
    pub notes: String,
    pub last_node_addr: Option<u8>,
    pub last_slot: Option<u8>,
    pub last_fw_version: Option<u32>,
}

/// A partial update for [`Link::update_meta`](crate::Link::update_meta);
/// `None` fields are left unchanged.
#[derive(Debug, Clone, Default)]
pub struct MetaUpdate {
    /// `Some(None)` clears the name.
    pub name: Option<Option<String>>,
    pub color: Option<Option<[u8; 3]>>,
    pub notes: Option<String>,
}

// ---------------------------------------------------------------------------
// Snapshot
// ---------------------------------------------------------------------------

/// Everything the GUI needs, published ~20 times per second.
/// Obtain with [`Link::snapshot`](crate::Link::snapshot); cloning is cheap-ish
/// (histories and the log are behind `Arc`).
#[derive(Debug, Clone)]
pub struct Snapshot {
    /// When this snapshot was taken.
    pub taken: Instant,
    pub bridge: BridgeView,
    /// Known gifts, sorted by slot (unassigned last), then UID.
    pub gifts: Vec<GiftView>,
    pub link: LinkStats,
    pub discovery: DiscoveryView,
    /// The running or most recent firmware update.
    pub ota: Option<OtaProgress>,
    /// Most recent log events (oldest first).
    pub events: Arc<Vec<LogEvent>>,
    /// True while the show stream is paused (e.g. during OTA).
    pub show_paused: bool,
    /// A `Wake` burst (and the polls after it) is running.
    pub waking: bool,
}

impl Default for Snapshot {
    fn default() -> Self {
        Self {
            taken: Instant::now(),
            bridge: BridgeView::default(),
            gifts: Vec::new(),
            link: LinkStats::default(),
            discovery: DiscoveryView::default(),
            ota: None,
            events: Arc::new(Vec::new()),
            show_paused: false,
            waking: false,
        }
    }
}

impl Snapshot {
    pub fn gift(&self, uid: &Uid) -> Option<&GiftView> {
        self.gifts.iter().find(|g| g.uid == *uid)
    }
    pub fn online_count(&self) -> usize {
        self.gifts.iter().filter(|g| g.online).count()
    }
    pub fn asleep_count(&self) -> usize {
        self.gifts.iter().filter(|g| g.asleep).count()
    }
    /// Resolve a user-typed gift reference to UIDs. Accepted forms:
    ///
    /// * `all` / `*`: every known gift
    /// * `s3` or `slot:3`: by show slot
    /// * `a17`, `@17` or `addr:17`: by node address
    /// * `uid:3f2a` or a hex string of ≥ 4 digits: by UID prefix (or suffix)
    /// * a plain number: slot if some gift has it, else node address
    /// * anything else: gift name (case-insensitive, exact match first, then prefix)
    pub fn resolve(&self, sel: &str) -> Result<Vec<Uid>, LinkError> {
        crate::select::resolve(self, sel)
    }
}

#[derive(Debug, Clone, PartialEq, Default)]
pub enum BridgeState {
    #[default]
    Connecting,
    Connected,
    /// Lost or failed to open; the link retries every second.
    Disconnected(String),
}

#[derive(Debug, Clone, Default)]
pub struct BridgeView {
    pub state: BridgeState,
    /// Transport description ("nrf24-usb bridge <serial>", "simulator").
    pub description: String,
    pub info: Option<Info>,
    pub stats: Stats,
    pub radio: RadioSettings,
    pub clock_synced: bool,
    /// Best recent Ping round-trip, µs.
    pub ping_rtt_us: Option<u32>,
}

/// One 2 Hz telemetry sample for sparklines.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct HistorySample {
    /// Seconds since the link started.
    pub t: f32,
    /// Poll success, 0..=100 (`NaN` if not polled in the window).
    pub link_pct: f32,
    pub avg_retries: f32,
    pub vbat_mv: f32,
    pub sync_err_us: f32,
    /// Broadcast reception, 0..=100 (`NaN` if unknown).
    pub bcast_pct: f32,
    pub online: bool,
}

/// One gift as seen by the host.
#[derive(Debug, Clone)]
pub struct GiftView {
    pub uid: Uid,
    pub meta: GiftMeta,
    pub node_addr: u8,
    /// Show slot (`None` = unassigned).
    pub slot: Option<u8>,
    pub hw_rev: u8,
    pub boot_version: u16,
    /// Running image's version (the bootloader's while in the bootloader).
    pub fw_version: u32,
    pub in_bootloader: bool,
    /// `app_crc` of the installed app image, from the last `Identity`
    /// (`None` before it was identified or without a valid image).
    pub app_crc: Option<u32>,
    pub online: bool,
    /// Dozing (see `lumigift_proto::doze`): this host put it to sleep and
    /// has not heard from it since. `online` is false; asleep gifts are not
    /// polled (they cannot answer) and are not offline either.
    pub asleep: bool,
    pub first_seen: Instant,
    pub last_seen: Option<Instant>,
    /// Latest raw status.
    pub status: Option<Status>,
    pub uptime: Duration,
    pub vbat_mv: u16,
    pub flags: u8,
    pub reset_reason: ResetReason,
    pub resets: u16,
    pub sync_err_us: u16,
    pub temp_c: i8,
    /// Reboots the host noticed (uptime going backwards).
    pub reboots_seen: u32,
    pub last_reboot: Option<Instant>,
    /// Poll success over the last ~2 s, 0..=100.
    pub poll_success_pct: f32,
    /// Average auto-retransmits of successful polls (last ~2 s).
    pub avg_retries: f32,
    /// Show broadcasts received vs sent by the host over the last ~5 s.
    pub bcast_rx_pct: Option<f32>,
    /// Show broadcasts the gift reported lost (sequence gaps), last ~5 s.
    pub bcast_lost: u32,
    pub polls_ok: u64,
    pub polls_failed: u64,
    /// Another gift answered on the same node address and resolution is pending.
    pub address_conflict: bool,
    pub history: Arc<Vec<HistorySample>>,
}

impl GiftView {
    /// Name if set, else the short UID.
    pub fn label(&self) -> String {
        self.meta.name.clone().unwrap_or_else(|| self.uid.short())
    }
}

#[derive(Debug, Clone, Default)]
pub struct LinkStats {
    pub uptime: Duration,
    /// Show ticks per second (≈100 while a show is playing).
    pub frames_per_s: f32,
    /// Show packets per second (several per tick for Solid/Leds).
    pub show_packets_per_s: f32,
    pub polls_per_s: f32,
    pub poll_ok_per_s: f32,
    pub tx_total: u64,
    pub tx_acked: u64,
    pub tx_failed: u64,
    /// Show packets sent since start.
    pub show_packets: u64,
    /// Show packets replaced by a newer tick before they could be sent.
    pub show_dropped: u64,
    /// TxDone events that never arrived (credit reclaimed after timeout).
    pub tx_lost: u64,
    pub in_flight: usize,
    pub max_in_flight: usize,
}

#[derive(Debug, Clone, Default)]
pub struct DiscoveryView {
    pub initial_sweep_done: bool,
    pub sweeping: bool,
    /// 0..=1 progress of the running full sweep.
    pub sweep_progress: f32,
    pub last_full_sweep: Option<Instant>,
    pub collisions_resolved: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogLevel {
    Info,
    Warn,
    Error,
}

#[derive(Debug, Clone)]
pub struct LogEvent {
    pub at: Instant,
    pub wall: SystemTime,
    pub level: LogLevel,
    pub gift: Option<Uid>,
    pub message: String,
}

// ---------------------------------------------------------------------------
// OTA progress
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OtaPhase {
    Validating,
    EnteringBootloader,
    Beginning,
    Streaming,
    Repairing,
    Committing,
    Rebooting,
    Done,
    Failed,
    Cancelled,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OtaTargetPhase {
    Waiting,
    EnteringBootloader,
    Erasing,
    Receiving,
    Committing,
    Verified,
    Booting,
    Done,
    Failed,
    Cancelled,
}

#[derive(Debug, Clone)]
pub struct OtaTargetProgress {
    pub uid: Uid,
    pub label: String,
    pub phase: OtaTargetPhase,
    /// 0..=100.
    pub percent: f32,
    /// Chunks still missing at the last query.
    pub missing: u16,
    /// Command retries / re-sent Begins for this target.
    pub retries: u32,
    pub error: Option<String>,
    /// Already running this exact image: nothing was sent (phase `Done`).
    pub skipped: bool,
}

#[derive(Debug, Clone)]
pub struct OtaProgress {
    pub session: u8,
    pub fw_version: u32,
    pub image_len: u32,
    pub chunks: u16,
    pub phase: OtaPhase,
    pub round: u8,
    /// Chunk broadcasts sent so far (incl. repairs).
    pub chunks_sent: u32,
    pub started: Instant,
    pub finished: Option<Instant>,
    pub targets: Vec<OtaTargetProgress>,
}

/// Final result of [`Link::start_ota`](crate::Link::start_ota).
#[derive(Debug, Clone)]
pub struct OtaReport {
    pub fw_version: u32,
    pub updated: Vec<Uid>,
    pub failed: Vec<(Uid, String)>,
    pub duration: Duration,
    pub chunks_sent: u32,
    pub rounds: u8,
    /// Targets that already ran this exact image (not in `updated`).
    pub skipped: Vec<Uid>,
    /// Time spent in each phase, in order.
    pub phase_times: Vec<(OtaPhase, Duration)>,
    /// Chunk broadcast rate of the first pass, chunks/s.
    pub chunk_rate: f32,
}

/// Result of [`Link::rescue`](crate::Link::rescue).
#[derive(Debug, Clone, Default)]
pub struct RescueReport {
    /// Gifts seen in their bootloader while `BootHold` was being broadcast.
    pub in_bootloader: Vec<Uid>,
    /// How long `BootHold` was broadcast.
    pub duration: Duration,
    pub boot_holds_sent: u32,
}
