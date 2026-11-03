//! Shared state: gift registry, telemetry bookkeeping, event log, snapshots.

use std::collections::{BTreeMap, BTreeSet, HashMap, VecDeque};
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant, SystemTime};

use arc_swap::ArcSwap;
use crossbeam_channel::Sender;
use lumigift_proto::{Identity, ResetReason, Status, UNASSIGNED_SLOT, status_flags};

use crate::clock::ClockSync;
use crate::config::LinkConfig;
use crate::meta::MetaStore;
use crate::types::*;

/// Window for poll success / retries statistics.
const POLL_WINDOW: Duration = Duration::from_secs(2);
/// Window for broadcast reception statistics.
const BCAST_WINDOW: Duration = Duration::from_secs(5);

pub(crate) struct Shared {
    pub cfg: LinkConfig,
    pub epoch: Instant,
    pub state: Mutex<State>,
    pub snapshot: ArcSwap<Snapshot>,
    pub clock: ArcSwap<ClockSync>,
    pub show: ArcSwap<ShowOutput>,
    /// One-shot music flags (`EDGE_FLAGS`) set since the last show tick, so
    /// each beat/onset goes out exactly once whatever rate `set_show` runs at.
    pub show_events: AtomicU8,
    pub show_paused: AtomicBool,
    pub polling_paused: AtomicBool,
    pub discovery_paused: AtomicBool,
    pub ota_running: AtomicBool,
    pub ota_cancel: AtomicBool,
    pub connected: AtomicBool,
    pub shutdown: AtomicBool,
}

impl Shared {
    pub fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|p| p.into_inner())
    }
    pub fn stopping(&self) -> bool {
        self.shutdown.load(Ordering::Relaxed)
    }

    /// Rebuild and publish the snapshot now.
    pub fn publish(&self) {
        let paused = self.show_paused.load(Ordering::Relaxed);
        let snap = self.lock().build_snapshot(paused, Instant::now());
        self.snapshot.store(Arc::new(snap));
    }
}

pub(crate) struct Gift {
    pub uid: Uid,
    pub node_addr: u8,
    pub slot: u8,
    pub hw_rev: u8,
    pub boot_version: u16,
    /// `app_crc` of the installed image (from `Identity`).
    pub app_crc: Option<u32>,
    pub fw_version: u32,
    pub in_bootloader: bool,
    pub online: bool,
    /// Put to sleep by this host and not heard from since (never polled).
    pub asleep: bool,
    pub first_seen: Instant,
    pub last_seen: Option<Instant>,
    pub last_identified: Instant,
    pub status: Option<Status>,
    pub status_at: Option<Instant>,
    pub reboots_seen: u32,
    pub last_reboot: Option<Instant>,
    /// When it last went from offline / asleep to online.
    pub online_since: Option<Instant>,
    polls: VecDeque<(Instant, bool, u8)>,
    pub polls_ok: u64,
    pub polls_failed: u64,
    /// (time, sent, received, lost) deltas.
    bcast: VecDeque<(Instant, u32, u32, u32)>,
    /// (bcast_rx, bcast_lost, host show packets) at the previous status.
    bcast_ref: Option<(u16, u16, u64)>,
    history: Arc<Vec<HistorySample>>,
}

impl Gift {
    fn new(uid: Uid, addr: u8, now: Instant) -> Self {
        Self {
            uid,
            node_addr: addr,
            slot: UNASSIGNED_SLOT,
            hw_rev: 0,
            boot_version: 0,
            app_crc: None,
            fw_version: 0,
            in_bootloader: false,
            online: false,
            asleep: false,
            first_seen: now,
            last_seen: None,
            last_identified: now,
            status: None,
            status_at: None,
            reboots_seen: 0,
            last_reboot: None,
            online_since: None,
            polls: VecDeque::new(),
            polls_ok: 0,
            polls_failed: 0,
            bcast: VecDeque::new(),
            bcast_ref: None,
            history: Arc::new(Vec::new()),
        }
    }

    fn poll_stats(&self, now: Instant) -> (f32, f32) {
        let recent = self
            .polls
            .iter()
            .filter(|p| now.duration_since(p.0) <= POLL_WINDOW);
        let (mut n, mut ok, mut retries) = (0u32, 0u32, 0u32);
        for p in recent {
            n += 1;
            if p.1 {
                ok += 1;
                retries += p.2 as u32;
            }
        }
        let pct = if n == 0 {
            f32::NAN
        } else {
            ok as f32 * 100.0 / n as f32
        };
        let avg = if ok == 0 {
            f32::NAN
        } else {
            retries as f32 / ok as f32
        };
        (pct, avg)
    }

    fn bcast_stats(&self, now: Instant) -> (Option<f32>, u32) {
        let (mut sent, mut rx, mut lost) = (0u32, 0u32, 0u32);
        for b in self
            .bcast
            .iter()
            .filter(|b| now.duration_since(b.0) <= BCAST_WINDOW)
        {
            sent += b.1;
            rx += b.2;
            lost += b.3;
        }
        // The gift's own counters (frames received, sequence gaps) are exact.
        // Comparing with the frames sent instead depends on matching each
        // status to the right moment, which a status refreshed between polls
        // skews: single windows then read low and kept "Missing frames" lit.
        // The sent count only covers a gift that hears nothing at all.
        let pct = if rx + lost > 0 {
            Some(rx as f32 * 100.0 / (rx + lost) as f32)
        } else {
            (sent > 0).then_some(0.0)
        };
        (pct, lost)
    }
}

pub(crate) struct State {
    pub gifts: BTreeMap<Uid, Gift>,
    /// The gift last confirmed (alone) at each node address; telemetry from
    /// an address is attributed to its owner.
    pub addr_owner: HashMap<u8, Uid>,
    /// Addresses where several UIDs answered, awaiting resolution.
    pub conflicts: BTreeMap<u8, BTreeSet<Uid>>,
    /// Addresses whose identity should be re-checked soon.
    pub needs_identify: BTreeSet<u8>,
    log: VecDeque<LogEvent>,
    log_arc: Arc<Vec<LogEvent>>,
    log_dirty: bool,
    log_len: usize,
    pub log_subs: Vec<Sender<LogEvent>>,
    pub meta: MetaStore,
    pub ota: Option<OtaProgress>,
    pub discovery: DiscoveryView,
    pub bridge: BridgeView,
    pub link: LinkStats,
    /// A wake (burst + polls) is running; see `ops::wake`.
    pub waking: bool,
}

impl State {
    pub fn new(cfg: &LinkConfig) -> Self {
        let (meta, warn) = MetaStore::open(&cfg.metadata);
        let mut s = Self {
            gifts: BTreeMap::new(),
            addr_owner: HashMap::new(),
            conflicts: BTreeMap::new(),
            needs_identify: BTreeSet::new(),
            log: VecDeque::new(),
            log_arc: Arc::new(Vec::new()),
            log_dirty: false,
            log_len: cfg.log_len.max(1),
            log_subs: Vec::new(),
            meta,
            ota: None,
            discovery: DiscoveryView::default(),
            bridge: BridgeView {
                radio: cfg.radio,
                ..Default::default()
            },
            link: LinkStats::default(),
            waking: false,
        };
        if let Some(w) = warn {
            s.log(LogLevel::Warn, None, w);
        }
        s
    }

    pub fn log(&mut self, level: LogLevel, gift: Option<Uid>, message: impl Into<String>) {
        let message = message.into();
        match level {
            LogLevel::Info => log::info!("{message}"),
            LogLevel::Warn => log::warn!("{message}"),
            LogLevel::Error => log::error!("{message}"),
        }
        let ev = LogEvent {
            at: Instant::now(),
            wall: SystemTime::now(),
            level,
            gift,
            message,
        };
        self.log_subs.retain(|s| s.send(ev.clone()).is_ok());
        self.log.push_back(ev);
        while self.log.len() > self.log_len {
            self.log.pop_front();
        }
        self.log_dirty = true;
    }

    pub fn label(&self, uid: &Uid) -> String {
        self.meta.get(uid).name.unwrap_or_else(|| uid.short())
    }

    /// (addr, online) of every pollable address. Asleep gifts are left
    /// out: they cannot answer, and a poll that happens to land in a listen
    /// window would wake them.
    pub fn poll_targets(&self) -> Vec<(u8, bool)> {
        self.addr_owner
            .iter()
            .filter(|(a, _)| !self.conflicts.contains_key(a))
            .filter_map(|(a, u)| self.gifts.get(u).map(|g| (*a, g)))
            .filter(|(_, g)| !g.asleep)
            .map(|(a, g)| (a, g.online))
            .collect()
    }

    /// Addresses owned by asleep gifts (discovery leaves them alone).
    pub fn asleep_addrs(&self) -> BTreeSet<u8> {
        self.gifts
            .values()
            .filter(|g| g.asleep)
            .map(|g| g.node_addr)
            .collect()
    }

    /// The gift dozes now (it took a `Sleep` broadcast and stopped answering).
    pub fn mark_asleep(&mut self, uid: Uid) {
        if let Some(g) = self.gifts.get_mut(&uid) {
            g.asleep = true;
            g.online = false;
        }
    }

    fn mark_online(&mut self, uid: Uid, now: Instant) {
        let label = self.label(&uid);
        let Some(g) = self.gifts.get_mut(&uid) else {
            return;
        };
        g.last_seen = Some(now);
        // Anything it answers means it is awake.
        let woke = std::mem::take(&mut g.asleep);
        if !g.online {
            g.online = true;
            g.online_since = Some(now);
            let addr = g.node_addr;
            let what = if woke { "awake" } else { "online" };
            self.log(
                LogLevel::Info,
                Some(uid),
                format!("{label} {what} (addr {addr})"),
            );
        }
    }

    /// Any acknowledged unicast to `addr`.
    pub fn touch(&mut self, addr: u8, now: Instant) {
        if let Some(&uid) = self.addr_owner.get(&addr)
            && !self.conflicts.contains_key(&addr)
        {
            self.mark_online(uid, now);
        }
    }

    /// Outcome of one telemetry poll.
    pub fn record_poll(&mut self, addr: u8, ok: bool, retries: u8, now: Instant) {
        if self.conflicts.contains_key(&addr) {
            return;
        }
        let Some(&uid) = self.addr_owner.get(&addr) else {
            return;
        };
        if let Some(g) = self.gifts.get_mut(&uid) {
            g.polls.push_back((now, ok, retries));
            while g
                .polls
                .front()
                .is_some_and(|p| now.duration_since(p.0) > POLL_WINDOW)
            {
                g.polls.pop_front();
            }
            if ok {
                g.polls_ok += 1;
            } else {
                g.polls_failed += 1;
            }
        }
        if ok {
            self.mark_online(uid, now);
        }
    }

    /// A Status reply from `addr`. `show_packets_at_echo` is the number of
    /// show packets the bridge had transmitted when the packet this status
    /// answers (seq == echo) was transmitted.
    pub fn apply_status(
        &mut self,
        addr: u8,
        st: Status,
        show_packets_at_echo: Option<u64>,
        now: Instant,
    ) {
        if self.conflicts.contains_key(&addr) {
            return;
        }
        let Some(&uid) = self.addr_owner.get(&addr) else {
            self.needs_identify.insert(addr);
            return;
        };
        let label = self.label(&uid);
        let mut logs: Vec<(LogLevel, String)> = Vec::new();
        let mut suspicious = false;
        let Some(g) = self.gifts.get_mut(&uid) else {
            return;
        };
        let in_boot = st.flags & status_flags::IN_BOOTLOADER != 0;
        let mut rebooted = false;
        if let (Some(prev), Some(prev_at)) = (g.status, g.status_at) {
            let elapsed_ms = now.duration_since(prev_at).as_millis() as u64;
            if (st.uptime_ms as u64) + 500 < prev.uptime_ms as u64 {
                if (st.uptime_ms as u64) <= elapsed_ms + 3000 {
                    rebooted = true;
                    g.reboots_seen += 1;
                    g.last_reboot = Some(now);
                    let reason = ResetReason::from_u8(st.reset_reason);
                    logs.push((
                        if matches!(reason, ResetReason::Software) {
                            LogLevel::Info
                        } else {
                            LogLevel::Warn
                        },
                        format!(
                            "{label} rebooted (reason {reason:?}, {} resets since power-on)",
                            st.resets
                        ),
                    ));
                } else {
                    // Uptime jumped to an unrelated value: probably two gifts
                    // answering on the same address.
                    suspicious = true;
                }
            }
            if !suspicious && in_boot != g.in_bootloader {
                logs.push((
                    LogLevel::Info,
                    if in_boot {
                        format!("{label} is in the bootloader")
                    } else {
                        format!("{label} is running the app")
                    },
                ));
            }
            if !suspicious
                && !in_boot
                && !g.in_bootloader
                && st.fw_version != g.fw_version
                && g.fw_version != 0
            {
                logs.push((
                    LogLevel::Info,
                    format!("{label} firmware {} → {}", g.fw_version, st.fw_version),
                ));
            }
        }
        if suspicious {
            self.needs_identify.insert(addr);
            return;
        }
        if st.flags & status_flags::LOW_BATTERY != 0
            && g.status
                .is_some_and(|p| p.flags & status_flags::LOW_BATTERY == 0)
        {
            logs.push((
                LogLevel::Warn,
                format!("{label} battery low ({} mV)", st.vbat_mv),
            ));
        }
        // Broadcast reception accounting.
        if rebooted || in_boot {
            g.bcast_ref = None;
        }
        if let Some(f) = show_packets_at_echo.filter(|_| !in_boot) {
            if let Some((rx0, lost0, f0)) = g.bcast_ref {
                let sent = f.saturating_sub(f0).min(u32::MAX as u64) as u32;
                let rx = st.bcast_rx.wrapping_sub(rx0) as u32;
                let lost = st.bcast_lost.wrapping_sub(lost0) as u32;
                if sent > 0 || rx > 0 {
                    g.bcast.push_back((now, sent, rx, lost));
                }
                while g
                    .bcast
                    .front()
                    .is_some_and(|b| now.duration_since(b.0) > BCAST_WINDOW)
                {
                    g.bcast.pop_front();
                }
            }
            g.bcast_ref = Some((st.bcast_rx, st.bcast_lost, f));
        }
        g.in_bootloader = in_boot;
        g.slot = st.slot;
        if in_boot {
            g.boot_version = st.fw_version as u16;
        } else {
            g.fw_version = st.fw_version;
        }
        g.status = Some(st);
        g.status_at = Some(now);
        for (lvl, m) in logs {
            self.log(lvl, Some(uid), m);
        }
        if rebooted {
            // A fresh boot is a good moment to check nobody else uses the address.
            self.needs_identify.insert(addr);
        }
    }

    /// A target answered from its new image after an update.
    pub fn on_ota_done(&mut self, uid: Uid, fw_version: u32, app_crc: u32) {
        if let Some(g) = self.gifts.get_mut(&uid) {
            g.fw_version = fw_version;
            g.app_crc = Some(app_crc);
            g.in_bootloader = false;
        }
    }

    /// Identities read from `addr` (one per distinct UID seen).
    pub fn on_identities(&mut self, addr: u8, ids: &[Identity], now: Instant) {
        for id in ids {
            let uid = Uid(id.uid);
            let label = self.label(&uid);
            let in_boot = id.flags & status_flags::IN_BOOTLOADER != 0;
            let is_new = !self.gifts.contains_key(&uid);
            let g = self
                .gifts
                .entry(uid)
                .or_insert_with(|| Gift::new(uid, addr, now));
            let moved = (!is_new && g.node_addr != addr).then_some(g.node_addr);
            g.node_addr = addr;
            g.hw_rev = id.hw_rev;
            g.boot_version = id.boot_version;
            g.app_crc = (id.app_crc != 0).then_some(id.app_crc);
            if !in_boot {
                g.fw_version = id.fw_version;
            }
            g.in_bootloader = in_boot;
            g.slot = id.slot;
            g.last_identified = now;
            let (fw, slot) = (g.fw_version, g.slot);
            if is_new {
                let mode = if in_boot { " [bootloader]" } else { "" };
                self.log(
                    LogLevel::Info,
                    Some(uid),
                    format!("discovered {label} at addr {addr} (fw {fw}){mode}"),
                );
            } else if let Some(old) = moved {
                self.log(
                    LogLevel::Info,
                    Some(uid),
                    format!("{label} now at addr {addr} (was {old})"),
                );
            }
            self.mark_online(uid, now);
            let _ = self.meta.update(uid, |m| {
                m.last_node_addr = Some(addr);
                m.last_slot = (slot != UNASSIGNED_SLOT).then_some(slot);
                if !in_boot {
                    m.last_fw_version = Some(fw);
                }
            });
        }
        let uids: BTreeSet<Uid> = ids.iter().map(|i| Uid(i.uid)).collect();
        match uids.len() {
            0 => {}
            1 => {
                let uid = *uids.iter().next().unwrap();
                if self.addr_owner.insert(addr, uid) != Some(uid)
                    && let Some(g) = self.gifts.get_mut(&uid)
                {
                    g.status = None;
                    g.bcast_ref = None;
                }
                self.conflicts.remove(&addr);
                // Drop stale ownership of the gift's previous address.
                self.addr_owner.retain(|a, u| *a == addr || *u != uid);
            }
            _ => {
                if self.conflicts.get(&addr) != Some(&uids) {
                    let names: Vec<String> = uids.iter().map(|u| self.label(u)).collect();
                    self.log(
                        LogLevel::Warn,
                        None,
                        format!("address collision at {addr}: {}", names.join(", ")),
                    );
                }
                for u in &uids {
                    self.addr_owner.retain(|a, o| *a == addr || o != u);
                }
                self.conflicts.insert(addr, uids);
            }
        }
    }

    /// Offline detection, history sampling.
    pub fn housekeeping(
        &mut self,
        cfg: &LinkConfig,
        epoch: Instant,
        now: Instant,
        polling_paused: bool,
        sample_history: bool,
    ) {
        let mut went_offline = Vec::new();
        for g in self.gifts.values_mut() {
            let stale = g
                .last_seen
                .is_none_or(|t| now.duration_since(t) > cfg.offline_after);
            if g.online && stale && !polling_paused {
                g.online = false;
                went_offline.push((g.uid, g.node_addr));
            }
            if sample_history {
                let (link, retries) = g.poll_stats(now);
                let (bc, _) = g.bcast_stats(now);
                let st = g.status;
                let h = Arc::make_mut(&mut g.history);
                h.push(HistorySample {
                    t: now.duration_since(epoch).as_secs_f32(),
                    link_pct: link,
                    avg_retries: retries,
                    vbat_mv: st.map_or(f32::NAN, |s| s.vbat_mv as f32),
                    sync_err_us: st.map_or(f32::NAN, |s| s.sync_err_us as f32),
                    bcast_pct: bc.unwrap_or(f32::NAN),
                    online: g.online,
                });
                if h.len() > cfg.history_len {
                    let excess = h.len() - cfg.history_len;
                    h.drain(..excess);
                }
            }
        }
        for (uid, addr) in went_offline {
            let label = self.label(&uid);
            self.log(
                LogLevel::Warn,
                Some(uid),
                format!("{label} offline (addr {addr})"),
            );
        }
    }

    pub fn build_snapshot(&mut self, show_paused: bool, now: Instant) -> Snapshot {
        if self.log_dirty {
            self.log_arc = Arc::new(self.log.iter().cloned().collect());
            self.log_dirty = false;
        }
        let mut gifts: Vec<GiftView> = self
            .gifts
            .values()
            .map(|g| {
                let (pct, retries) = g.poll_stats(now);
                let (bc, lost) = g.bcast_stats(now);
                let st = g.status;
                let s = st.unwrap_or_default();
                GiftView {
                    uid: g.uid,
                    meta: self.meta.get(&g.uid),
                    node_addr: g.node_addr,
                    slot: (g.slot != UNASSIGNED_SLOT).then_some(g.slot),
                    hw_rev: g.hw_rev,
                    boot_version: g.boot_version,
                    app_crc: g.app_crc,
                    fw_version: if g.in_bootloader {
                        s.fw_version
                    } else {
                        g.fw_version
                    },
                    in_bootloader: g.in_bootloader,
                    online: g.online,
                    asleep: g.asleep,
                    first_seen: g.first_seen,
                    last_seen: g.last_seen,
                    status: st,
                    uptime: Duration::from_millis(s.uptime_ms as u64),
                    vbat_mv: s.vbat_mv,
                    flags: s.flags,
                    reset_reason: ResetReason::from_u8(s.reset_reason),
                    resets: s.resets,
                    sync_err_us: s.sync_err_us,
                    temp_c: s.temp_c,
                    reboots_seen: g.reboots_seen,
                    last_reboot: g.last_reboot,
                    online_since: g.online_since,
                    poll_success_pct: if pct.is_nan() { 0.0 } else { pct },
                    avg_retries: if retries.is_nan() { 0.0 } else { retries },
                    bcast_rx_pct: bc,
                    bcast_lost: lost,
                    polls_ok: g.polls_ok,
                    polls_failed: g.polls_failed,
                    address_conflict: self.conflicts.values().any(|c| c.contains(&g.uid)),
                    history: g.history.clone(),
                }
            })
            .collect();
        gifts.sort_by_key(|g| (g.slot.unwrap_or(u8::MAX), g.uid));
        Snapshot {
            taken: now,
            bridge: self.bridge.clone(),
            gifts,
            link: self.link.clone(),
            discovery: self.discovery.clone(),
            ota: self.ota.clone(),
            events: self.log_arc.clone(),
            show_paused,
            waking: self.waking,
        }
    }
}
