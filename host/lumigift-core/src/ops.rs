//! Operations that run on their own threads and talk to the scheduler through
//! [`Radio`]: gift commands, discovery, collision resolution.

use std::collections::{BTreeMap, BTreeSet, HashMap, VecDeque};
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, Sender};
use lumigift_proto::{self as lp, Broadcast, Identity, Packet, Reply, Rgb, Unicast, config_key};

use crate::scheduler::{Radio, UniMode, UniResult, encode, wait_reply};
use crate::state::Shared;
use crate::types::*;

#[derive(Clone)]
pub(crate) struct Ctx {
    pub shared: Arc<Shared>,
    pub radio: Radio,
}

impl Ctx {
    pub fn log(&self, level: LogLevel, gift: Option<Uid>, msg: impl Into<String>) {
        self.shared.lock().log(level, gift, msg);
    }

    pub fn label(&self, uid: &Uid) -> String {
        self.shared.lock().label(uid)
    }

    fn check_connected(&self) -> Result<(), LinkError> {
        if self.shared.connected.load(Ordering::Relaxed) {
            Ok(())
        } else {
            Err(LinkError::NotConnected)
        }
    }

    /// [`resolve`](Self::resolve), failing first if the bridge is not connected.
    fn connected_targets(&self, t: &Target) -> Result<Vec<(Uid, u8)>, LinkError> {
        self.check_connected()?;
        self.resolve(t)
    }

    /// Resolve a target to (uid, node_addr) pairs.
    pub fn resolve(&self, t: &Target) -> Result<Vec<(Uid, u8)>, LinkError> {
        let st = self.shared.lock();
        let one = |u: &Uid| {
            st.gifts
                .get(u)
                .map(|g| (*u, g.node_addr))
                .ok_or_else(|| LinkError::NoSuchGift(u.to_hex()))
        };
        match t {
            Target::Gift(u) => Ok(vec![one(u)?]),
            Target::Gifts(v) => v.iter().map(one).collect(),
            Target::All => Ok(st
                .gifts
                .values()
                .filter(|g| g.online && st.addr_owner.get(&g.node_addr) == Some(&g.uid))
                .map(|g| (g.uid, g.node_addr))
                .collect()),
        }
    }

    /// Send `msg` to every target concurrently and gather the results.
    fn fan_out(
        &self,
        targets: &[(Uid, u8)],
        mode: UniMode,
        attempts: u8,
        msg: Unicast,
    ) -> Vec<(Uid, UniResult)> {
        self.radio.unicast_many(
            targets.iter().map(|&(u, a)| (u, a, msg)),
            mode,
            attempts,
            Duration::from_secs(15),
            "command",
        )
    }
}

fn report<T>(results: Vec<(Uid, Result<T, LinkError>)>) -> CmdReport {
    let mut r = CmdReport::default();
    for (u, res) in results {
        match res {
            Ok(_) => r.ok.push(u),
            Err(e) => r.failed.push((u, e)),
        }
    }
    r
}

// ---------------------------------------------------------------------------
// Commands
// ---------------------------------------------------------------------------

pub(crate) fn blink(
    ctx: &Ctx,
    target: &Target,
    color: Rgb,
    count: u8,
    period: Duration,
) -> Result<CmdReport, LinkError> {
    let targets = ctx.connected_targets(target)?;
    let period_10ms = (period.as_millis() / 10).clamp(1, 255) as u8;
    let msg = Unicast::Blink {
        seq: 0,
        color,
        count,
        period_10ms,
    };
    Ok(report(ctx.fan_out(&targets, UniMode::COLLECT, 4, msg)))
}

/// Blink white three times quickly, then red, green and blue.
pub(crate) fn identify(ctx: &Ctx, target: &Target) -> Result<CmdReport, LinkError> {
    let targets = ctx.connected_targets(target)?;
    let blink_msg = |color, count, period_10ms| Unicast::Blink {
        seq: 0,
        color,
        count,
        period_10ms,
    };
    let first = ctx.fan_out(
        &targets,
        UniMode::COLLECT,
        4,
        blink_msg(Rgb::new(255, 255, 255), 3, 12),
    );
    let ok: Vec<(Uid, u8)> = targets
        .iter()
        .copied()
        .filter(|(u, _)| first.iter().any(|(x, r)| x == u && r.is_ok()))
        .collect();
    std::thread::sleep(Duration::from_millis(400));
    for c in [
        Rgb::new(255, 0, 0),
        Rgb::new(0, 255, 0),
        Rgb::new(0, 0, 255),
    ] {
        let _ = ctx.fan_out(&ok, UniMode::COLLECT, 3, blink_msg(c, 1, 40));
        std::thread::sleep(Duration::from_millis(400));
    }
    ctx.log(
        LogLevel::Info,
        None,
        format!("identify: {} gift(s)", ok.len()),
    );
    Ok(report(first))
}

pub(crate) fn set_slot(ctx: &Ctx, uid: Uid, slot: Option<u8>) -> Result<(), LinkError> {
    ctx.check_connected()?;
    let slot = slot.unwrap_or(lp::UNASSIGNED_SLOT);
    if slot != lp::UNASSIGNED_SLOT && slot >= lp::MAX_SLOTS {
        return Err(LinkError::Rejected(format!(
            "slot must be < {}",
            lp::MAX_SLOTS
        )));
    }
    let (_, addr) = ctx.resolve(&Target::Gift(uid))?[0];
    match ctx
        .radio
        .command(addr, Unicast::SetSlot { seq: 0, slot }, 4)?
    {
        Reply::Status(s) if s.slot == slot => {
            let label = ctx.label(&uid);
            let mut st = ctx.shared.lock();
            if let Some(g) = st.gifts.get_mut(&uid) {
                g.slot = slot;
            }
            let _ = st.meta.update(uid, |m| {
                m.last_slot = (slot != lp::UNASSIGNED_SLOT).then_some(slot)
            });
            let txt = if slot == lp::UNASSIGNED_SLOT {
                "unassigned".to_string()
            } else {
                slot.to_string()
            };
            st.log(LogLevel::Info, Some(uid), format!("{label} slot → {txt}"));
            Ok(())
        }
        Reply::Status(s) => Err(LinkError::Rejected(format!("gift kept slot {}", s.slot))),
        other => Err(LinkError::UnexpectedReply(format!("{other:?}"))),
    }
}

pub(crate) fn auto_assign_slots(ctx: &Ctx, mode: SlotAssign) -> Result<CmdReport, LinkError> {
    ctx.check_connected()?;
    let mut gifts: Vec<(Uid, u8)> = {
        let st = ctx.shared.lock();
        st.gifts
            .values()
            .filter(|g| g.online && !g.in_bootloader)
            .map(|g| (g.uid, g.slot))
            .collect()
    };
    gifts.sort();
    let mut plan: BTreeMap<Uid, u8> = BTreeMap::new();
    match mode {
        SlotAssign::ByUid => {
            for (i, (u, _)) in gifts.iter().enumerate().take(lp::MAX_SLOTS as usize) {
                plan.insert(*u, i as u8);
            }
        }
        SlotAssign::KeepExisting => {
            let mut used = BTreeSet::new();
            let mut todo = Vec::new();
            for &(u, s) in &gifts {
                if s < lp::MAX_SLOTS && used.insert(s) {
                    plan.insert(u, s);
                } else {
                    todo.push(u);
                }
            }
            let mut free = (0..lp::MAX_SLOTS).filter(|s| !used.contains(s));
            for u in todo {
                if let Some(s) = free.next() {
                    plan.insert(u, s);
                }
            }
        }
    }
    let mut res = Vec::new();
    for (u, cur) in gifts {
        let Some(&want) = plan.get(&u) else { continue };
        if want == cur {
            res.push((u, Ok(())));
        } else {
            res.push((u, set_slot(ctx, u, Some(want))));
        }
    }
    let r = report(res);
    ctx.log(LogLevel::Info, None, format!("auto slots ({mode:?}): {r}"));
    Ok(r)
}

pub(crate) fn reboot(
    ctx: &Ctx,
    target: &Target,
    into_bootloader: bool,
) -> Result<CmdReport, LinkError> {
    let targets = ctx.connected_targets(target)?;
    let res = ctx.fan_out(
        &targets,
        UniMode::Collect {
            accept_cmd_ack: true,
        },
        3,
        Unicast::Reboot {
            seq: 0,
            into_bootloader,
        },
    );
    let r = report(res);
    let what = if into_bootloader {
        "reboot into bootloader"
    } else {
        "reboot"
    };
    ctx.log(LogLevel::Info, None, format!("{what}: {r}"));
    Ok(r)
}

pub(crate) fn set_config(
    ctx: &Ctx,
    target: &Target,
    key: u8,
    value: u16,
) -> Result<CmdReport, LinkError> {
    let targets = ctx.connected_targets(target)?;
    let res = ctx.fan_out(
        &targets,
        UniMode::COLLECT,
        4,
        Unicast::SetConfig { seq: 0, key, value },
    );
    let res = res
        .into_iter()
        .map(|(u, r)| {
            let r = r.and_then(|reply| match reply {
                Some(Reply::ConfigResult { ok: true, .. }) => Ok(()),
                Some(Reply::ConfigResult {
                    ok: false,
                    value: cur,
                    ..
                }) => Err(LinkError::Rejected(format!(
                    "gift rejected key {key} = {value} (current {cur})"
                ))),
                other => Err(LinkError::UnexpectedReply(format!("{other:?}"))),
            });
            (u, r)
        })
        .collect();
    let r = report(res);
    ctx.log(
        LogLevel::Info,
        None,
        format!("config key {key} = {value}: {r}"),
    );
    Ok(r)
}

/// Move every gift, then the bridge, to another channel.
pub(crate) fn set_channel(ctx: &Ctx, channel: u8) -> Result<CmdReport, LinkError> {
    ctx.check_connected()?;
    if channel > 125 {
        return Err(LinkError::Rejected("channel must be 0..=125".into()));
    }
    let offline: Vec<String> = {
        let st = ctx.shared.lock();
        st.gifts
            .values()
            .filter(|g| !g.online)
            .map(|g| st.label(&g.uid))
            .collect()
    };
    if !offline.is_empty() {
        ctx.log(
            LogLevel::Warn,
            None,
            format!(
                "channel change: offline gifts stay on the old channel: {}",
                offline.join(", ")
            ),
        );
    }
    let r = set_config(ctx, &Target::All, config_key::CHANNEL, channel as u16)?;
    if r.ok.is_empty() && !r.failed.is_empty() {
        return Err(LinkError::Other(format!(
            "no gift accepted the channel change: {r}"
        )));
    }
    // The gifts switch CHANNEL_SWITCH_DELAY_MS after replying.
    std::thread::sleep(Duration::from_millis(
        config_key::CHANNEL_SWITCH_DELAY_MS as u64 + 150,
    ));
    let mut radio = ctx.shared.lock().bridge.radio;
    let old = radio.channel;
    radio.channel = channel;
    ctx.radio.reconfigure(radio)?;
    ctx.log(
        LogLevel::Info,
        None,
        format!("radio channel {old} → {channel} ({r})"),
    );
    Ok(r)
}

// ---------------------------------------------------------------------------
// Doze (see `lumigift_proto::doze` and ARCHITECTURE.md "Power / doze")
// ---------------------------------------------------------------------------

/// Sets an `AtomicBool` for as long as it lives, then restores its old value
/// (so a wake inside an OTA leaves the OTA's own pause flags alone).
struct FlagGuard<'a> {
    flag: &'a std::sync::atomic::AtomicBool,
    old: bool,
}

impl<'a> FlagGuard<'a> {
    fn set(flag: &'a std::sync::atomic::AtomicBool) -> Self {
        let old = flag.swap(true, Ordering::Relaxed);
        Self { flag, old }
    }
}

impl Drop for FlagGuard<'_> {
    fn drop(&mut self) {
        self.flag.store(self.old, Ordering::Relaxed);
    }
}

/// Put every gift to sleep: broadcast `Sleep`, then check with one poll
/// each that the online gifts stopped answering (those that still answer
/// missed it and get another round). Resolves to the number of gifts now
/// asleep. Gifts in their bootloader do not doze and are left alone.
pub(crate) fn sleep_all(ctx: &Ctx) -> Result<usize, LinkError> {
    ctx.check_connected()?;
    // Keep telemetry polls off the dozing gifts while we check them.
    let _quiet = FlagGuard::set(&ctx.shared.polling_paused);
    let mut todo: Vec<(Uid, u8)> = {
        let st = ctx.shared.lock();
        st.gifts
            .values()
            .filter(|g| g.online && !g.in_bootloader)
            .filter(|g| st.addr_owner.get(&g.node_addr) == Some(&g.uid))
            .map(|g| (g.uid, g.node_addr))
            .collect()
    };
    let sleep = encode(Packet::Broadcast(Broadcast::Sleep));
    let mut asleep = 0usize;
    for round in 0..3 {
        // Unknown or unpollable gifts hear the first round too.
        if todo.is_empty() && round > 0 {
            break;
        }
        ctx.radio.broadcast(vec![sleep; 5], None)?;
        // A gift dozes as soon as it handles the packet; give it a moment.
        std::thread::sleep(Duration::from_millis(20));
        // One exchange per gift (the bridge still retries in hardware). An
        // ack means it is awake; it is then marked online again.
        let res = ctx.fan_out(&todo, UniMode::PollOnly, 1, Unicast::Poll { seq: 0 });
        let mut st = ctx.shared.lock();
        let mut awake = Vec::new();
        for ((uid, addr), (_, r)) in todo.iter().zip(res) {
            if r.is_ok() {
                awake.push((*uid, *addr));
            } else {
                st.mark_asleep(*uid);
                asleep += 1;
            }
        }
        todo = awake;
    }
    let mut st = ctx.shared.lock();
    let mut msg = format!("sleep: {asleep} gift(s) dozing");
    if !todo.is_empty() {
        let names: Vec<String> = todo.iter().map(|(u, _)| st.label(u)).collect();
        msg += &format!(", still awake: {}", names.join(", "));
    }
    st.log(LogLevel::Info, None, msg);
    Ok(asleep)
}

/// Broadcast `Wake` back to back for `WAKE_BURST_MS`, so it falls into
/// every dozing gift's listen window (one per `PERIOD_MS` of its ±10 %
/// clock). Telemetry polls are paused meanwhile: the burst keeps the bridge
/// busy, and a missing poll must not count against online gifts.
fn wake_burst(ctx: &Ctx) -> Result<u32, LinkError> {
    let _quiet = FlagGuard::set(&ctx.shared.polling_paused);
    let wake = encode(Packet::Broadcast(Broadcast::Wake));
    let burst = Duration::from_millis(lp::doze::WAKE_BURST_MS as u64);
    let t0 = Instant::now();
    let mut jobs: VecDeque<Receiver<Result<u32, LinkError>>> = VecDeque::new();
    let mut sent = 0;
    // Two jobs queued at all times so the scheduler never runs dry between
    // them (a gap of a couple of ms could straddle a whole listen window).
    while t0.elapsed() < burst && !ctx.shared.stopping() {
        if jobs.len() < 2 {
            jobs.push_back(ctx.radio.broadcast_gapless(vec![wake; 32]));
        } else if let Some(rx) = jobs.pop_front() {
            sent += wait_reply(&rx, Duration::from_secs(5), "wake burst")?;
        }
    }
    for rx in jobs {
        sent += wait_reply(&rx, Duration::from_secs(5), "wake burst")?;
    }
    Ok(sent)
}

/// Clears `State::waking` when the wake ends, however it ends.
struct Waking<'a>(&'a Ctx);

impl Drop for Waking<'_> {
    fn drop(&mut self) {
        self.0.shared.lock().waking = false;
    }
}

/// Wake dozing gifts: a `Wake` burst, then a poll of every known gift that
/// is asleep or offline (an answer marks it online). Gifts that still do not
/// answer count as offline from then on (they get the normal offline polls).
/// With `sweep`, a full discovery sweep follows, for gifts this host never
/// saw awake. If a wake is already running, this waits for it instead.
/// Resolves to the number of gifts online afterwards.
pub(crate) fn wake(ctx: &Ctx, sweep: bool) -> Result<usize, LinkError> {
    ctx.check_connected()?;
    let online = |ctx: &Ctx| {
        ctx.shared
            .lock()
            .gifts
            .values()
            .filter(|g| g.online)
            .count()
    };
    let already = std::mem::replace(&mut ctx.shared.lock().waking, true);
    if already {
        while ctx.shared.lock().waking && !ctx.shared.stopping() {
            std::thread::sleep(Duration::from_millis(20));
        }
        return Ok(online(ctx));
    }
    let _waking = Waking(ctx);
    ctx.shared.publish();
    let asleep = ctx
        .shared
        .lock()
        .gifts
        .values()
        .filter(|g| g.asleep)
        .count();
    let sent = wake_burst(ctx)?;
    let quiet: Vec<(Uid, u8)> = {
        let st = ctx.shared.lock();
        st.gifts
            .values()
            .filter(|g| !g.online && !st.conflicts.contains_key(&g.node_addr))
            .map(|g| (g.uid, g.node_addr))
            .collect()
    };
    // Every woken gift is listening now; a few tries each covers packet loss.
    let _ = ctx.fan_out(&quiet, UniMode::PollOnly, 3, Unicast::Poll { seq: 0 });
    {
        let mut st = ctx.shared.lock();
        for g in st.gifts.values_mut() {
            // Not answering after a wake: treat as offline (polled again).
            g.asleep = false;
        }
    }
    if sweep {
        full_sweep(ctx);
    }
    let n = online(ctx);
    ctx.log(
        LogLevel::Info,
        None,
        format!("wake: {sent} Wake packets, {asleep} gift(s) were asleep, {n} online"),
    );
    Ok(n)
}

// ---------------------------------------------------------------------------
// Rescue
// ---------------------------------------------------------------------------

/// Puts the link back to normal whatever way the rescue ends.
struct RescueRestore<'a> {
    ctx: &'a Ctx,
    radio: Option<crate::config::RadioSettings>,
}

impl Drop for RescueRestore<'_> {
    fn drop(&mut self) {
        if let Some(r) = self.radio.take() {
            let _ = self.ctx.radio.reconfigure(r);
        }
        self.ctx.shared.show_paused.store(false, Ordering::Relaxed);
        self.ctx
            .shared
            .discovery_paused
            .store(false, Ordering::Relaxed);
    }
}

/// Broadcast `BootHold` on the default channel for `duration` (the user
/// power-cycles the gifts meanwhile), watching known addresses for gifts in
/// their bootloader, then sweep for unknown ones.
pub(crate) fn rescue(ctx: &Ctx, duration: Duration) -> Result<RescueReport, LinkError> {
    ctx.check_connected()?;
    ctx.shared.discovery_paused.store(true, Ordering::Relaxed);
    ctx.shared.show_paused.store(true, Ordering::Relaxed);
    let mut restore = RescueRestore { ctx, radio: None };
    let orig = ctx.shared.lock().bridge.radio;
    if orig.channel != lp::DEFAULT_CHANNEL {
        let mut r = orig;
        r.channel = lp::DEFAULT_CHANNEL;
        ctx.radio.reconfigure(r)?;
        restore.radio = Some(orig);
        ctx.log(
            LogLevel::Warn,
            None,
            format!(
                "rescue: bridge on channel {} (from {}) while broadcasting BootHold",
                lp::DEFAULT_CHANNEL,
                orig.channel
            ),
        );
    }
    ctx.log(
        LogLevel::Info,
        None,
        format!(
            "rescue: broadcasting BootHold for {:.0} s, power-cycle the gift(s) now",
            duration.as_secs_f32()
        ),
    );
    let hold = encode(Packet::Broadcast(Broadcast::BootHold));
    let t0 = Instant::now();
    let mut next_poll = t0;
    let mut found: BTreeSet<Uid> = BTreeSet::new();
    let mut sent = 0u32;
    while t0.elapsed() < duration && !ctx.shared.stopping() {
        // ~3 ms of back-to-back BootHolds: a bootloader's 150 ms window
        // always sees dozens of them.
        sent += ctx.radio.broadcast(vec![hold; 8], None)?;
        let now = Instant::now();
        if now >= next_poll {
            next_poll = now + Duration::from_millis(150);
            let known: Vec<(Uid, u8)> = {
                let st = ctx.shared.lock();
                st.gifts.values().map(|g| (g.uid, g.node_addr)).collect()
            };
            let res = ctx.fan_out(&known, UniMode::PollOnly, 1, Unicast::Poll { seq: 0 });
            for (u, r) in res {
                let in_boot = matches!(
                    r,
                    Ok(Some(Reply::Status(s))) if s.flags & lp::status_flags::IN_BOOTLOADER != 0
                );
                if in_boot && found.insert(u) {
                    let label = ctx.label(&u);
                    ctx.log(
                        LogLevel::Info,
                        Some(u),
                        format!("rescue: {label} is in its bootloader"),
                    );
                }
            }
        }
        std::thread::sleep(Duration::from_millis(1));
    }
    // Gifts the registry does not know (or at another address).
    full_sweep(ctx);
    {
        let st = ctx.shared.lock();
        found.extend(
            st.gifts
                .values()
                .filter(|g| g.online && g.in_bootloader)
                .map(|g| g.uid),
        );
    }
    let report = RescueReport {
        in_bootloader: found.into_iter().collect(),
        duration: t0.elapsed(),
        boot_holds_sent: sent,
    };
    ctx.log(
        LogLevel::Info,
        None,
        format!(
            "rescue done: {} gift(s) in their bootloader",
            report.in_bootloader.len()
        ),
    );
    Ok(report)
}

// ---------------------------------------------------------------------------
// Discovery
// ---------------------------------------------------------------------------

/// GetIdentity `tries` times; register whatever UIDs answered.
pub(crate) fn identify_addr(ctx: &Ctx, addr: u8, tries: u8) -> Vec<Identity> {
    let mut seen: BTreeMap<[u8; 12], Identity> = BTreeMap::new();
    for _ in 0..tries {
        if let Ok(Reply::Identity(i)) = ctx.radio.command(addr, Unicast::GetIdentity { seq: 0 }, 3)
        {
            seen.insert(i.uid, i);
        }
    }
    let ids: Vec<Identity> = seen.into_values().collect();
    if !ids.is_empty() {
        ctx.shared.lock().on_identities(addr, &ids, Instant::now());
    }
    ids
}

fn full_sweep(ctx: &Ctx) -> usize {
    {
        let mut st = ctx.shared.lock();
        st.discovery.sweeping = true;
        st.discovery.sweep_progress = 0.0;
    }
    let mut present = Vec::new();
    let mut window: VecDeque<(u8, Receiver<UniResult>)> = VecDeque::new();
    // Asleep gifts cannot answer, and a poll in a listen window would wake them.
    let (asleep, known) = {
        let st = ctx.shared.lock();
        let known: BTreeSet<u8> = st.meta.all().filter_map(|(_, m)| m.last_node_addr).collect();
        (st.asleep_addrs(), known)
    };
    let addrs: Vec<u8> = (1..=254u8)
        .filter(|a| lp::valid_node_addr(*a) && !asleep.contains(a))
        .collect();
    let total = addrs.len();
    let mut done = 0usize;
    let mut drain_one = |w: &mut VecDeque<(u8, Receiver<UniResult>)>, present: &mut Vec<u8>| {
        if let Some((a, rx)) = w.pop_front() {
            if let Ok(Ok(_)) = rx.recv_timeout(Duration::from_secs(5)) {
                present.push(a);
            }
            done += 1;
            if done.is_multiple_of(16) {
                ctx.shared.lock().discovery.sweep_progress = done as f32 / total as f32 * 0.8;
            }
        }
    };
    for a in addrs {
        if ctx.shared.stopping() {
            break;
        }
        // Where a gift lived last time, try harder: missing it means a fresh
        // CLI run does not see (or update) it.
        let attempts = if known.contains(&a) { 3 } else { 1 };
        window.push_back((
            a,
            ctx.radio
                .unicast_async(a, Unicast::Poll { seq: 0 }, UniMode::PollOnly, attempts),
        ));
        if window.len() >= 4 {
            drain_one(&mut window, &mut present);
        }
    }
    while !window.is_empty() {
        drain_one(&mut window, &mut present);
    }
    let n = present.len();
    for (i, a) in present.into_iter().enumerate() {
        identify_addr(ctx, a, 4);
        ctx.shared.lock().discovery.sweep_progress = 0.8 + 0.2 * (i + 1) as f32 / n.max(1) as f32;
    }
    let mut st = ctx.shared.lock();
    st.discovery.sweeping = false;
    st.discovery.sweep_progress = 1.0;
    st.discovery.initial_sweep_done = true;
    st.discovery.last_full_sweep = Some(Instant::now());
    let count = st.gifts.values().filter(|g| g.online).count();
    st.log(
        LogLevel::Info,
        None,
        format!("sweep done: {n} address(es) answered, {count} gift(s) online"),
    );
    count
}

fn pick_free_addr(ctx: &Ctx) -> Option<u8> {
    let st = ctx.shared.lock();
    let mut used: BTreeSet<u8> = st.gifts.values().map(|g| g.node_addr).collect();
    used.extend(st.conflicts.keys().copied());
    used.extend(st.meta.all().filter_map(|(_, m)| m.last_node_addr));
    let free: Vec<u8> = (1..=254u8)
        .filter(|a| lp::valid_node_addr(*a) && !used.contains(a))
        .collect();
    fastrand::choice(free)
}

fn resolve_conflict(ctx: &Ctx, addr: u8, uids: BTreeSet<Uid>) {
    let keep = {
        let st = ctx.shared.lock();
        uids.iter()
            .copied()
            .find(|u| st.meta.get(u).last_node_addr == Some(addr))
            .unwrap_or(*uids.iter().next().unwrap())
    };
    for uid in uids.iter().copied().filter(|u| *u != keep) {
        let Some(new) = pick_free_addr(ctx) else {
            ctx.log(LogLevel::Error, Some(uid), "no free node address left");
            return;
        };
        let pkt = encode(Packet::Broadcast(Broadcast::SetNodeAddr {
            uid: uid.0,
            node_addr: new,
        }));
        let _ = ctx.radio.broadcast(vec![pkt; 5], Some(300.0));
        std::thread::sleep(Duration::from_millis(30));
        let ids = identify_addr(ctx, new, 3);
        let label = ctx.label(&uid);
        if ids.iter().any(|i| i.uid == uid.0) {
            let mut st = ctx.shared.lock();
            st.discovery.collisions_resolved += 1;
            st.log(
                LogLevel::Info,
                Some(uid),
                format!("collision resolved: moved {label} from addr {addr} to {new}"),
            );
        } else {
            ctx.log(
                LogLevel::Warn,
                Some(uid),
                format!("could not move {label} from addr {addr} to {new}; will retry"),
            );
        }
    }
    // Colliding acks: only the strongest of several gifts on one address gets
    // through, so a third gift can hide behind the one we kept. With the
    // others moved away it is only the two of them now; ask often enough to
    // see it (otherwise it waits for the next background sweep, ~20 s).
    identify_addr(ctx, addr, 10);
}

/// Long-running discovery / maintenance loop.
pub(crate) fn discovery_thread(ctx: Ctx, rescans: Receiver<Sender<Result<usize, LinkError>>>) {
    let cfg = ctx.shared.cfg.clone();
    let mut swept_this_connection = false;
    let mut cursor: u8 = 0;
    let mut next_bg = Instant::now();
    let mut last_conflict_try: HashMap<u8, Instant> = HashMap::new();
    let step = cfg.background_sweep_period / 254;
    while !ctx.shared.stopping() {
        if !ctx.shared.connected.load(Ordering::Relaxed) {
            swept_this_connection = false;
            while let Ok(r) = rescans.try_recv() {
                let _ = r.send(Err(LinkError::NotConnected));
            }
            std::thread::sleep(Duration::from_millis(50));
            continue;
        }
        if ctx.shared.discovery_paused.load(Ordering::Relaxed) {
            while let Ok(r) = rescans.try_recv() {
                let _ = r.send(Err(LinkError::Busy("firmware update in progress".into())));
            }
            std::thread::sleep(Duration::from_millis(50));
            continue;
        }
        let rescan = rescans.try_recv().ok();
        if !swept_this_connection && cfg.wake_on_connect {
            // Gifts doze after a minute without a host (e.g. while no host
            // ran): wake them before the first sweep so it finds them.
            if let Err(e) = wake(&ctx, false) {
                ctx.log(LogLevel::Warn, None, format!("wake on connect: {e}"));
            }
        }
        if rescan.is_some() || (!swept_this_connection && cfg.auto_discover) {
            let n = full_sweep(&ctx);
            swept_this_connection = true;
            if let Some(r) = rescan {
                ctx.shared.publish();
                let _ = r.send(Ok(n));
            }
            continue;
        }
        let now = Instant::now();

        // 1. Address collisions.
        let conflicts: Vec<(u8, BTreeSet<Uid>)> = ctx
            .shared
            .lock()
            .conflicts
            .iter()
            .map(|(a, s)| (*a, s.clone()))
            .collect();
        if let Some((addr, uids)) = conflicts.into_iter().find(|(a, _)| {
            last_conflict_try
                .get(a)
                .is_none_or(|t| now.duration_since(*t) > Duration::from_secs(2))
        }) {
            last_conflict_try.insert(addr, now);
            resolve_conflict(&ctx, addr, uids);
            continue;
        }

        // 2. Addresses flagged for re-identification, then periodic checks.
        let flagged = {
            let mut st = ctx.shared.lock();
            let a = st.needs_identify.iter().next().copied();
            if let Some(a) = a {
                st.needs_identify.remove(&a);
            }
            // Leave dozing gifts alone (a unicast in a listen window wakes them).
            let a = a.filter(|a| !st.asleep_addrs().contains(a));
            a.or_else(|| {
                st.addr_owner
                    .iter()
                    .filter_map(|(a, u)| st.gifts.get(u).map(|g| (*a, g)))
                    .filter(|(_, g)| {
                        g.online && now.duration_since(g.last_identified) > cfg.reidentify_interval
                    })
                    .min_by_key(|(_, g)| g.last_identified)
                    .map(|(a, _)| a)
            })
        };
        if let Some(addr) = flagged {
            identify_addr(&ctx, addr, 4);
            continue;
        }

        // 3. Slow background sweep of unknown addresses.
        if cfg.auto_discover && now >= next_bg {
            next_bg = now + step;
            let known: BTreeSet<u8> = {
                let st = ctx.shared.lock();
                st.addr_owner
                    .keys()
                    .chain(st.conflicts.keys())
                    .copied()
                    .collect()
            };
            for _ in 0..254 {
                cursor = if cursor >= 254 { 1 } else { cursor + 1 };
                if !known.contains(&cursor) {
                    break;
                }
            }
            if !known.contains(&cursor)
                && ctx
                    .radio
                    .unicast(cursor, Unicast::Poll { seq: 0 }, UniMode::PollOnly, 1)
                    .is_ok()
            {
                identify_addr(&ctx, cursor, 4);
            }
            continue;
        }
        std::thread::sleep(
            Duration::from_millis(5)
                .min(next_bg.saturating_duration_since(Instant::now()))
                .max(Duration::from_millis(1)),
        );
    }
}
