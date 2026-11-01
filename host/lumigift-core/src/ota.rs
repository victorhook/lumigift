//! Firmware update engine (see ARCHITECTURE.md "Firmware update").
//!
//! 1. **Validating**: check the image; skip targets already running it
//!    (same `fw_version` and app CRC) unless forced; refuse targets of
//!    another board revision.
//! 2. **EnteringBootloader**: one `EnterBootloader` broadcast carrying every
//!    target's node address (repeated a few times), then polling each target
//!    until it reports `IN_BOOTLOADER`; unicast `Reboot` for stragglers.
//! 3. **Beginning**: `OtaBegin` broadcast (no waiting: the bootloaders erase
//!    in the background while buffering chunks).
//! 4. **Streaming**: every chunk in order (best for the flash: pages are
//!    erased in order) at the rate the gifts' flash can absorb.
//! 5. **Repairing**: `OtaQuery` bitmaps (only windows with holes); the union
//!    of every target's missing chunks is re-broadcast until each target
//!    reports `Verified` (the bootloader verifies on its own when the image
//!    is complete). Failed / power-cycled targets are restarted.
//! 6. **Rebooting**: the poll that collected the verdict starts the app; the
//!    host polls each target's address until it reports the new version.

use std::collections::BTreeSet;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use lumigift_proto::{
    Broadcast, ImageHeader, OTA_CHUNK, OtaBegin, OtaBitmap, OtaChunk, OtaState, Packet, Reply,
    Unicast, image::crc32, layout, status_flags,
};
use nrf24_usb_proto::Payload;

use crate::config::OtaOptions;
use crate::ops::{self, Ctx};
use crate::scheduler::{UniMode, encode, wait_reply};
use crate::types::*;

/// A validated image, ready to send.
pub(crate) struct Image {
    pub header: ImageHeader,
    pub bytes: Vec<u8>,
}

pub(crate) fn validate(image: &[u8]) -> Result<Image, LinkError> {
    let header = ImageHeader::decode(image).ok_or_else(|| {
        LinkError::InvalidImage("bad header (magic, version, CRC or size)".into())
    })?;
    let len = header.image_len() as usize;
    if image.len() < len {
        return Err(LinkError::InvalidImage(format!(
            "file has {} bytes, header says {len}",
            image.len()
        )));
    }
    if len > layout::APP_REGION_SIZE as usize {
        return Err(LinkError::InvalidImage(
            "image larger than the app region".into(),
        ));
    }
    let app = &image[layout::IMAGE_HEADER_SIZE as usize..len];
    if crc32(app) != header.app_crc {
        return Err(LinkError::InvalidImage("app CRC mismatch".into()));
    }
    // The same vector-table sanity check the bootloader makes, so an image
    // linked for the wrong address never reaches a gift.
    let word = |o: usize| u32::from_le_bytes(app[o..o + 4].try_into().unwrap());
    let (sp, reset) = (word(0), word(4));
    let sp_ok = sp > layout::RAM_BASE && sp <= layout::BOOT_SHARED_ADDR && sp % 4 == 0;
    let reset_ok = reset & 1 == 1
        && reset > layout::APP_VECTORS
        && reset < layout::APP_VECTORS + header.app_len;
    if !(sp_ok && reset_ok) {
        return Err(LinkError::InvalidImage(format!(
            "no vector table for {:#010x} (SP {sp:#010x}, reset {reset:#010x})",
            layout::APP_VECTORS
        )));
    }
    Ok(Image {
        header,
        bytes: image[..len].to_vec(),
    })
}

/// Bootloader CPU time per chunk outside flash operations (decode, map,
/// flash register setup). The SPI read overlaps the previous flash
/// operation (RAM drain loop).
const CHUNK_CPU: Duration = Duration::from_micros(30);
/// Air time per chunk in a NO_ACK run at 1 Mbps (297 µs + run overhead):
/// the bridge's limit.
const RADIO_CHUNK: Duration = Duration::from_micros(320);
/// Restarts (new Begin + full stream) per target after a failure or power loss.
const MAX_RESTARTS: u32 = 2;
/// A target may stay silent this long during repairs (e.g. power-cycled).
const SILENCE_LIMIT: Duration = Duration::from_secs(3);

/// Chunks/s the gifts' flash can absorb for this image: page erases plus
/// double-word programs plus per-chunk CPU time (the bootloader's RX ring
/// absorbs the bursts while a page erases).
fn auto_rate(img: &Image, o: &OtaOptions) -> f32 {
    let chunks = img.bytes.len().div_ceil(OTA_CHUNK);
    let pages = img.bytes.len().div_ceil(layout::PAGE_SIZE as usize);
    let dws = img
        .bytes
        .chunks(8)
        .filter(|dw| dw.iter().any(|&b| b != 0xFF))
        .count();
    let busy = o.page_erase_time * pages as u32
        + o.double_word_time * dws as u32
        + CHUNK_CPU * chunks as u32;
    (chunks as f32 / busy.as_secs_f32()) * 0.97
}

/// Rate for repair rounds: every page is erased by then, so the gifts only
/// program (3 double words per chunk); the air rate is the other limit.
fn repair_rate(o: &OtaOptions) -> f32 {
    let per_chunk = (o.double_word_time * 3 + CHUNK_CPU).max(RADIO_CHUNK);
    0.9 / per_chunk.as_secs_f32()
}

struct Target {
    uid: Uid,
    label: String,
    addr: u8,
    phase: OtaTargetPhase,
    percent: f32,
    missing: u16,
    retries: u32,
    error: Option<String>,
    skipped: bool,
    /// Bootloader reported `Verified`.
    verified: bool,
    restarts: u32,
    /// Last time the target answered.
    heard: Instant,
}

impl Target {
    fn active(&self) -> bool {
        !matches!(
            self.phase,
            OtaTargetPhase::Failed | OtaTargetPhase::Cancelled | OtaTargetPhase::Done
        )
    }
}

struct Engine<'a> {
    ctx: &'a Ctx,
    img: Image,
    session: u8,
    chunks: u16,
    targets: Vec<Target>,
    phase: OtaPhase,
    phase_started: Instant,
    phase_times: Vec<(OtaPhase, Duration)>,
    round: u8,
    chunks_sent: u32,
    rate: f32,
    first_rate: f32,
    /// The user fixed the rate (`OtaOptions::chunks_per_s`).
    fix_rate: bool,
    started: Instant,
}

/// Restores the link to normal operation whatever way the update ends
/// (`Link` clears `ota_running` once the update thread is done).
struct Resume<'a>(&'a Ctx);

impl Drop for Resume<'_> {
    fn drop(&mut self) {
        let s = &self.0.shared;
        s.show_paused.store(false, Ordering::Relaxed);
        s.polling_paused.store(false, Ordering::Relaxed);
        s.discovery_paused.store(false, Ordering::Relaxed);
    }
}

pub(crate) fn run(
    ctx: &Ctx,
    img: Image,
    targets: Vec<Uid>,
    force: bool,
) -> Result<OtaReport, LinkError> {
    let _resume = Resume(ctx);
    let len = img.bytes.len();
    let chunks = len.div_ceil(OTA_CHUNK) as u16;
    let now = Instant::now();
    let mut tg = Vec::new();
    {
        let st = ctx.shared.lock();
        for u in &targets {
            let g = st
                .gifts
                .get(u)
                .ok_or_else(|| LinkError::NoSuchGift(u.to_hex()))?;
            if tg.iter().any(|t: &Target| t.uid == *u) {
                continue;
            }
            tg.push(Target {
                uid: *u,
                label: st.label(u),
                addr: g.node_addr,
                phase: OtaTargetPhase::Waiting,
                percent: 0.0,
                missing: chunks,
                retries: 0,
                error: None,
                skipped: false,
                verified: false,
                restarts: 0,
                heard: now,
            });
        }
    }
    if tg.is_empty() {
        return Err(LinkError::Rejected("no OTA targets".into()));
    }
    let opts = ctx.shared.cfg.ota.clone();
    let (rate, fix_rate) = if opts.chunks_per_s > 0.0 {
        (opts.chunks_per_s, true)
    } else {
        (auto_rate(&img, &opts), false)
    };
    let mut e = Engine {
        ctx,
        session: fastrand::u8(1..=255),
        chunks,
        targets: tg,
        phase: OtaPhase::Validating,
        phase_started: now,
        phase_times: Vec::new(),
        round: 0,
        chunks_sent: 0,
        rate,
        first_rate: rate,
        fix_rate,
        started: now,
        img,
    };
    ctx.log(
        LogLevel::Info,
        None,
        format!(
            "OTA: fw {} ({} bytes, {} chunks) to {} gift(s), session {}, {:.0} chunks/s",
            e.img.header.fw_version,
            len,
            chunks,
            e.targets.len(),
            e.session,
            rate
        ),
    );
    ctx.shared.show_paused.store(true, Ordering::Relaxed);
    ctx.shared.discovery_paused.store(true, Ordering::Relaxed);
    let r = e.run_phases(force || !opts.skip_up_to_date);
    let report = e.finish(&r);
    r.map(|_| report)
}

impl Engine<'_> {
    fn publish(&self) {
        let p = OtaProgress {
            session: self.session,
            fw_version: self.img.header.fw_version,
            image_len: self.img.bytes.len() as u32,
            chunks: self.chunks,
            phase: self.phase,
            round: self.round,
            chunks_sent: self.chunks_sent,
            started: self.started,
            finished: matches!(
                self.phase,
                OtaPhase::Done | OtaPhase::Failed | OtaPhase::Cancelled
            )
            .then(Instant::now),
            targets: self
                .targets
                .iter()
                .map(|t| OtaTargetProgress {
                    uid: t.uid,
                    label: t.label.clone(),
                    phase: t.phase,
                    percent: t.percent,
                    missing: t.missing,
                    retries: t.retries,
                    error: t.error.clone(),
                    skipped: t.skipped,
                })
                .collect(),
        };
        self.ctx.shared.lock().ota = Some(p);
    }

    fn set_phase(&mut self, p: OtaPhase) {
        let now = Instant::now();
        self.phase_times
            .push((self.phase, now.duration_since(self.phase_started)));
        self.phase_started = now;
        self.phase = p;
        self.ctx.log(LogLevel::Info, None, format!("OTA: {p:?}"));
        self.publish();
    }

    fn fail(&mut self, i: usize, why: impl Into<String>) {
        let t = &mut self.targets[i];
        let why = why.into();
        t.phase = OtaTargetPhase::Failed;
        let msg = format!("OTA: {} failed: {why}", t.label);
        let uid = t.uid;
        t.error = Some(why);
        self.ctx.log(LogLevel::Error, Some(uid), msg);
    }

    fn check_cancel(&mut self) -> Result<(), LinkError> {
        if self.ctx.shared.ota_cancel.load(Ordering::Relaxed) {
            for t in self.targets.iter_mut().filter(|t| t.active()) {
                t.phase = OtaTargetPhase::Cancelled;
            }
            self.set_phase(OtaPhase::Cancelled);
            self.ctx.log(
                LogLevel::Warn,
                None,
                "OTA cancelled (targets stay in the bootloader)",
            );
            return Err(LinkError::Cancelled);
        }
        if !self.ctx.shared.connected.load(Ordering::Relaxed) {
            return Err(LinkError::NotConnected);
        }
        Ok(())
    }

    fn active(&self) -> Vec<usize> {
        (0..self.targets.len())
            .filter(|&i| self.targets[i].active())
            .collect()
    }

    /// Err once no target is left that has not failed.
    fn require_active(&self) -> Result<(), LinkError> {
        let alive = self
            .targets
            .iter()
            .any(|t| !matches!(t.phase, OtaTargetPhase::Failed | OtaTargetPhase::Cancelled));
        if alive {
            Ok(())
        } else {
            Err(LinkError::Other("every target failed".into()))
        }
    }

    fn chunk(&self, index: u16) -> Payload {
        let off = index as usize * OTA_CHUNK;
        let mut data = [0xFFu8; OTA_CHUNK];
        let end = (off + OTA_CHUNK).min(self.img.bytes.len());
        data[..end - off].copy_from_slice(&self.img.bytes[off..end]);
        encode(Packet::Broadcast(Broadcast::OtaChunk(OtaChunk {
            session: self.session,
            index,
            data,
        })))
    }

    fn send_begin(&mut self) -> Result<(), LinkError> {
        let p = encode(Packet::Broadcast(Broadcast::OtaBegin(OtaBegin {
            session: self.session,
            image_len: self.img.bytes.len() as u32,
            image_crc: crc32(&self.img.bytes),
        })));
        self.ctx.radio.broadcast(vec![p; 3], None)?;
        Ok(())
    }

    /// Broadcast chunks in order at the current rate. The batches (~25 ms
    /// each) are queued at once and run back to back; waiting on them in
    /// order drives the progress display and cancellation.
    fn stream(&mut self, indices: &[u16], estimate_percent: bool) -> Result<(), LinkError> {
        let rate = self.rate.max(10.0);
        let batch = ((rate / 40.0) as usize).max(8);
        let jobs: Vec<_> = indices
            .chunks(batch)
            .map(|part| {
                let pkts: Vec<Payload> = part.iter().map(|&i| self.chunk(i)).collect();
                self.ctx.radio.broadcast_async(pkts, Some(rate))
            })
            .collect();
        let mut done = 0usize;
        let mut cancelled = Ok(());
        for rx in jobs {
            let r = wait_reply(&rx, Duration::from_secs(10), "broadcast")?;
            self.chunks_sent += r;
            done += batch;
            if estimate_percent {
                let frac = done.min(indices.len()) as f32 / indices.len() as f32;
                for i in self.active() {
                    self.targets[i].percent = 90.0 * frac;
                }
            }
            self.publish();
            // Let queued batches drain on cancel (they are harmless) but stop here.
            if cancelled.is_ok() {
                cancelled = self.check_cancel();
            }
        }
        cancelled
    }

    /// Poll targets once (concurrently); returns the status of those that answered.
    fn poll(&self, idx: &[usize]) -> Vec<(usize, lumigift_proto::Status)> {
        let reqs = idx
            .iter()
            .map(|&i| (i, self.targets[i].addr, Unicast::Poll { seq: 0 }));
        self.ctx
            .radio
            .unicast_many(reqs, UniMode::PollOnly, 2, Duration::from_secs(5), "poll")
            .into_iter()
            .filter_map(|(i, r)| match r {
                Ok(Some(Reply::Status(s))) => Some((i, s)),
                _ => None,
            })
            .collect()
    }

    /// `OtaQuery` to several targets concurrently.
    fn query(&self, q: &[(usize, u8)]) -> Vec<(usize, Result<OtaBitmap, LinkError>)> {
        let reqs = q.iter().map(|&(i, window)| {
            let msg = Unicast::OtaQuery {
                seq: 0,
                session: self.session,
                window,
            };
            (i, self.targets[i].addr, msg)
        });
        self.ctx
            .radio
            .unicast_many(reqs, UniMode::COLLECT, 3, Duration::from_secs(5), "query")
            .into_iter()
            .map(|(i, r)| {
                let r = r.and_then(|reply| match reply {
                    Some(Reply::OtaBitmap(b)) => Ok(b),
                    Some(other) => Err(LinkError::UnexpectedReply(format!("{other:?}"))),
                    None => Err(LinkError::NoReply),
                });
                (i, r)
            })
            .collect()
    }

    fn run_phases(&mut self, force: bool) -> Result<(), LinkError> {
        let opts = self.ctx.shared.cfg.ota.clone();
        self.validate_targets(force);
        self.require_active()?;
        if self.active().is_empty() {
            return Ok(()); // everything already up to date
        }
        self.enter_bootloader(&opts)?;
        self.require_active()?;

        // -- 3. Begin ----------------------------------------------------------
        self.set_phase(OtaPhase::Beginning);
        self.send_begin()?;
        for i in self.active() {
            self.targets[i].phase = OtaTargetPhase::Receiving;
            self.targets[i].heard = Instant::now();
        }

        // -- 4. Stream everything once, in order --------------------------------
        self.ctx
            .shared
            .polling_paused
            .store(true, Ordering::Relaxed);
        self.set_phase(OtaPhase::Streaming);
        let all: Vec<u16> = (0..self.chunks).collect();
        self.stream(&all, true)?;

        // -- 5. Query + repair until every target verified -----------------------
        self.set_phase(OtaPhase::Repairing);
        self.repair(&opts)?;
        self.require_active()?;

        // -- 6. Wait for the new firmware to answer ------------------------------
        self.ctx
            .shared
            .polling_paused
            .store(false, Ordering::Relaxed);
        self.ctx.shared.show_paused.store(false, Ordering::Relaxed);
        self.set_phase(OtaPhase::Rebooting);
        self.confirm_boot(&opts)?;
        self.require_active()
    }

    /// Skip targets already running the image; refuse other board revisions
    /// and targets whose node address is not unique (another gift would
    /// answer for them).
    fn validate_targets(&mut self, force: bool) {
        let want_fw = self.img.header.fw_version;
        let want_crc = self.img.header.app_crc;
        for i in 0..self.targets.len() {
            let (addr, uid) = (self.targets[i].addr, self.targets[i].uid);
            let shared = {
                let st = self.ctx.shared.lock();
                st.conflicts.contains_key(&addr)
                    || st
                        .gifts
                        .values()
                        .any(|g| g.uid != uid && g.node_addr == addr && g.online)
            };
            if shared {
                self.fail(
                    i,
                    format!("node address {addr} is not unique (collision not resolved yet)"),
                );
            }
        }
        for i in 0..self.targets.len() {
            if self.targets[i].phase == OtaTargetPhase::Failed {
                continue;
            }
            let (addr, uid) = (self.targets[i].addr, self.targets[i].uid);
            let snap = |e: &Engine| {
                let st = e.ctx.shared.lock();
                st.gifts
                    .get(&uid)
                    .map(|g| (g.fw_version, g.app_crc, g.hw_rev, g.in_bootloader, g.online))
            };
            let Some((fw, mut crc, mut hw, in_boot, online)) = snap(self) else {
                continue;
            };
            if !force && online && !in_boot && fw == want_fw && crc.is_none() {
                // Learn the installed image's CRC to compare exactly.
                ops::identify_addr(self.ctx, addr, 2);
                if let Some(s) = snap(self) {
                    (crc, hw) = (s.1, s.2);
                }
            }
            if hw != 0 && hw as u16 != self.img.header.hw_rev {
                self.fail(
                    i,
                    format!(
                        "image is for board revision {}, gift is revision {hw}",
                        self.img.header.hw_rev
                    ),
                );
            } else if !force && online && !in_boot && fw == want_fw && crc == Some(want_crc) {
                let t = &mut self.targets[i];
                t.phase = OtaTargetPhase::Done;
                t.skipped = true;
                t.percent = 100.0;
                t.missing = 0;
                let msg = format!("OTA: {} already runs fw {want_fw}, skipped", t.label);
                self.ctx.log(LogLevel::Info, Some(uid), msg);
            }
        }
        self.publish();
    }

    /// Get every active target into its bootloader.
    fn enter_bootloader(&mut self, opts: &OtaOptions) -> Result<(), LinkError> {
        self.set_phase(OtaPhase::EnteringBootloader);
        let t0 = Instant::now();
        let mut ready: BTreeSet<usize> = BTreeSet::new();
        // One broadcast for all targets not known to be in their bootloader
        // already (no unicast round trips), repeated against loss.
        let addrs: Vec<u8> = {
            let st = self.ctx.shared.lock();
            self.active()
                .into_iter()
                .filter(|&i| {
                    !st.gifts
                        .get(&self.targets[i].uid)
                        .is_some_and(|g| g.in_bootloader && g.online)
                })
                .map(|i| self.targets[i].addr)
                .collect()
        };
        for part in addrs.chunks(lumigift_proto::ENTER_BOOT_MAX) {
            let p = encode(Packet::Broadcast(Broadcast::enter_bootloader(part)));
            self.ctx.radio.broadcast(vec![p; 5], None)?;
        }
        let mut next_reboot = t0 + Duration::from_millis(150);
        loop {
            self.check_cancel()?;
            let pending: Vec<usize> = self
                .active()
                .into_iter()
                .filter(|i| !ready.contains(i))
                .collect();
            if pending.is_empty() {
                break;
            }
            for (i, s) in self.poll(&pending) {
                let t = &mut self.targets[i];
                t.heard = Instant::now();
                if s.flags & status_flags::IN_BOOTLOADER != 0 {
                    ready.insert(i);
                    t.phase = OtaTargetPhase::Erasing;
                } else {
                    t.phase = OtaTargetPhase::EnteringBootloader;
                }
            }
            let pending: Vec<usize> = pending.into_iter().filter(|i| !ready.contains(i)).collect();
            if pending.is_empty() {
                break;
            }
            if Instant::now() >= next_reboot {
                // Fallback for targets that missed the broadcast.
                next_reboot = Instant::now() + Duration::from_millis(500);
                let reboot = Unicast::Reboot {
                    seq: 0,
                    into_bootloader: true,
                };
                let reqs: Vec<_> = pending
                    .iter()
                    .map(|&i| {
                        self.targets[i].retries += 1;
                        (i, self.targets[i].addr, reboot)
                    })
                    .collect();
                let mode = UniMode::Collect {
                    accept_cmd_ack: true,
                };
                let timeout = Duration::from_secs(5);
                self.ctx
                    .radio
                    .unicast_many(reqs, mode, 3, timeout, "reboot");
            }
            if t0.elapsed() > opts.bootloader_timeout {
                for i in pending {
                    self.fail(i, "did not enter the bootloader");
                }
                break;
            }
            self.publish();
            std::thread::sleep(Duration::from_millis(3));
        }
        self.publish();
        Ok(())
    }

    /// Restart a target's transfer (new Begin of the same session, full stream).
    fn restart(&mut self, i: usize, why: &str) -> bool {
        let t = &mut self.targets[i];
        if t.restarts >= MAX_RESTARTS {
            let why = format!("{why} (after {} restarts)", t.restarts);
            self.fail(i, why);
            return false;
        }
        t.restarts += 1;
        t.retries += 1;
        t.verified = false;
        t.missing = self.chunks;
        t.percent = 0.0;
        t.phase = OtaTargetPhase::Receiving;
        let msg = format!("OTA: {}: {why}, restarting its transfer", t.label);
        let uid = t.uid;
        self.ctx.log(LogLevel::Warn, Some(uid), msg);
        true
    }

    fn repair(&mut self, opts: &OtaOptions) -> Result<(), LinkError> {
        let windows = OtaBitmap::windows(self.chunks as u32) as u8;
        loop {
            self.check_cancel()?;
            let mut want: BTreeSet<u16> = BTreeSet::new();
            let mut need_begin = false;
            let mut waiting = false;
            // Per target: next window to ask for, holes found so far.
            let mut todo: Vec<(usize, u8, u16)> = self
                .active()
                .into_iter()
                .filter(|&i| !self.targets[i].verified)
                .map(|i| (i, 0u8, 0u16))
                .collect();
            while !todo.is_empty() {
                let q: Vec<(usize, u8)> = todo.iter().map(|&(i, w, _)| (i, w)).collect();
                let mut next = Vec::new();
                for (i, r) in self.query(&q) {
                    let k = todo.iter().position(|e| e.0 == i).unwrap();
                    let (_, _, found) = todo[k];
                    match r {
                        Ok(b) if b.state == OtaState::NotInBootloader => {
                            // Booted already (verified image, nobody asked in time).
                            self.targets[i].verified = true;
                            self.targets[i].heard = Instant::now();
                        }
                        Ok(b) if b.session != self.session || b.state == OtaState::Idle => {
                            self.targets[i].heard = Instant::now();
                            if self.restart(i, "lost the session (missed Begin or power loss)") {
                                need_begin = true;
                                want.extend(0..self.chunks);
                            }
                        }
                        Ok(b) => {
                            let chunks = self.chunks;
                            let t = &mut self.targets[i];
                            t.heard = Instant::now();
                            match b.state {
                                OtaState::Verified => {
                                    t.verified = true;
                                    t.missing = 0;
                                    t.percent = 100.0;
                                    t.phase = OtaTargetPhase::Verified;
                                }
                                OtaState::Failed => {
                                    if self.restart(i, "image verification failed on the gift") {
                                        need_begin = true;
                                        want.extend(0..chunks);
                                    }
                                }
                                _ => {
                                    t.missing = b.missing_count;
                                    t.percent = 100.0
                                        * (chunks - b.missing_count.min(chunks)) as f32
                                        / chunks as f32;
                                    if b.missing_count == 0 {
                                        // Complete; the verdict follows once
                                        // its buffered chunks are programmed.
                                        waiting = true;
                                        continue;
                                    }
                                    let holes: Vec<u16> = b.missing(chunks as u32).collect();
                                    let found = found + holes.len() as u16;
                                    want.extend(holes);
                                    if found < b.missing_count && b.window + 1 < windows {
                                        next.push((i, b.window + 1, found));
                                    }
                                }
                            }
                        }
                        Err(_) => {
                            let t = &mut self.targets[i];
                            t.retries += 1;
                            if t.heard.elapsed() > SILENCE_LIMIT {
                                let why = format!(
                                    "not answering for {:.1} s",
                                    SILENCE_LIMIT.as_secs_f32()
                                );
                                self.fail(i, why);
                            } else {
                                waiting = true;
                            }
                        }
                    }
                }
                todo = next;
            }
            self.publish();
            self.require_active()?;
            let open: Vec<usize> = self
                .active()
                .into_iter()
                .filter(|&i| !self.targets[i].verified)
                .collect();
            if open.is_empty() {
                return Ok(());
            }
            if want.is_empty() {
                if waiting {
                    std::thread::sleep(Duration::from_millis(5));
                }
                continue;
            }
            if self.round >= opts.max_rounds {
                for i in open {
                    let m = self.targets[i].missing;
                    self.fail(
                        i,
                        format!(
                            "{m} chunk(s) still missing after {} rounds",
                            opts.max_rounds
                        ),
                    );
                }
                return Ok(());
            }
            if self.round == 0 && !self.fix_rate {
                // Every page is erased by now: the gifts only program.
                self.rate = repair_rate(opts);
                let worst = open
                    .iter()
                    .map(|&i| self.targets[i].missing)
                    .max()
                    .unwrap_or(0);
                if opts.adaptive && worst as f32 > 0.25 * self.chunks as f32 {
                    // Heavy first-pass loss: RX rings overflowing or a poor
                    // link. Repair more gently.
                    self.rate = self.first_rate * 0.8;
                }
            }
            self.round += 1;
            if need_begin {
                self.send_begin()?;
            }
            let v: Vec<u16> = want.into_iter().collect();
            self.stream(&v, false)?;
        }
    }

    /// Poll every verified target until it answers from the new app.
    fn confirm_boot(&mut self, opts: &OtaOptions) -> Result<(), LinkError> {
        for i in self.active() {
            self.targets[i].phase = OtaTargetPhase::Booting;
        }
        let t0 = Instant::now();
        let want = self.img.header.fw_version;
        loop {
            self.check_cancel()?;
            let waiting: Vec<usize> = self
                .active()
                .into_iter()
                .filter(|&i| self.targets[i].phase == OtaTargetPhase::Booting)
                .collect();
            if waiting.is_empty() {
                break;
            }
            for (i, s) in self.poll(&waiting) {
                if s.flags & status_flags::IN_BOOTLOADER == 0 && s.fw_version == want {
                    let t = &mut self.targets[i];
                    t.phase = OtaTargetPhase::Done;
                    t.percent = 100.0;
                    let uid = t.uid;
                    self.ctx
                        .shared
                        .lock()
                        .on_ota_done(uid, want, self.img.header.app_crc);
                }
            }
            self.publish();
            if t0.elapsed() > opts.boot_timeout {
                for i in waiting {
                    if self.targets[i].phase == OtaTargetPhase::Booting {
                        self.fail(i, format!("did not come back running fw {want}"));
                    }
                }
                break;
            }
            std::thread::sleep(Duration::from_millis(3));
        }
        Ok(())
    }

    fn finish(&mut self, r: &Result<(), LinkError>) -> OtaReport {
        if self.phase != OtaPhase::Cancelled {
            let p = if r.is_ok() && self.targets.iter().all(|t| t.phase == OtaTargetPhase::Done) {
                OtaPhase::Done
            } else {
                OtaPhase::Failed
            };
            self.set_phase(p);
        }
        if let Err(e) = r {
            let left: Vec<usize> = self.active();
            for i in left {
                if !matches!(e, LinkError::Cancelled) {
                    self.fail(i, e.to_string());
                }
            }
        }
        let report = OtaReport {
            fw_version: self.img.header.fw_version,
            updated: self
                .targets
                .iter()
                .filter(|t| t.phase == OtaTargetPhase::Done && !t.skipped)
                .map(|t| t.uid)
                .collect(),
            failed: self
                .targets
                .iter()
                .filter(|t| t.phase != OtaTargetPhase::Done)
                .map(|t| {
                    (
                        t.uid,
                        t.error.clone().unwrap_or_else(|| format!("{:?}", t.phase)),
                    )
                })
                .collect(),
            duration: self.started.elapsed(),
            chunks_sent: self.chunks_sent,
            rounds: self.round,
            skipped: self
                .targets
                .iter()
                .filter(|t| t.skipped)
                .map(|t| t.uid)
                .collect(),
            phase_times: self
                .phase_times
                .iter()
                .copied()
                .filter(|(p, _)| !matches!(p, OtaPhase::Done | OtaPhase::Failed))
                .collect(),
            chunk_rate: self.first_rate,
        };
        self.ctx.log(
            if report.failed.is_empty() {
                LogLevel::Info
            } else {
                LogLevel::Warn
            },
            None,
            format!(
                "OTA {:?}: {} updated, {} skipped, {} failed, {} chunks in {:.2} s ({} repair rounds)",
                self.phase,
                report.updated.len(),
                report.skipped.len(),
                report.failed.len(),
                report.chunks_sent,
                report.duration.as_secs_f32(),
                report.rounds
            ),
        );
        self.publish();
        report
    }
}
