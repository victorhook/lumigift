//! The bootloader's recovery mode (radio OTA receiver), independent of the
//! hardware. The bootloader binary and the host simulator both run this
//! code; the hardware glue only moves packets and executes the returned
//! effects.
//!
//! Main loop (see `boot/src/main.rs`):
//!
//! ```text
//! loop {
//!     drain the nRF RX FIFO into the rings (also done continuously, from
//!     RAM, while any flash operation is busy)
//!     match next_action():
//!         Unicast   -> pop, on_unicast() -> preload the returned reply
//!         Broadcast -> pop, on_broadcast() (may erase/program flash)
//!         Erase     -> erase_ahead() (next page of the session, in order)
//!         Idle
//!     tick(): channel switch / boot the verified app / reboot
//! }
//! ```
//!
//! Unicast packets (queries, polls) go first, so the host gets answers
//! within one flash operation even while hundreds of chunks are buffered.

use lumigift_proto::image::ImageHeader;
use lumigift_proto::{
    Identity, OTA_CHUNK, OtaState, Reply, Status, UNASSIGNED_SLOT, Unicast, kind, status_flags, valid_node_addr,
};

use crate::blink::Blink;
use crate::bootpkt::{self, BootPacket};
use crate::channel::ChannelPlan;
use crate::ota::{Ota, RegionFlash, RegionRead};
use crate::rxring::RxRing;

/// Reported as `Status::fw_version` in recovery and as `Identity::boot_version`.
pub const BOOT_VERSION: u16 = 2;
/// Boot a verified image this long after the verdict if no `Poll` collected it.
pub const BOOT_DELAY_MS: u32 = 200;
/// Let the ack of a `Reboot` go out before resetting.
pub const REBOOT_DELAY_MS: u32 = 20;
/// How long a normal boot listens for `BootHold` (on the default channel).
pub const HOLD_WINDOW_MS: u32 = 150;
/// RX ring sizes. Unicast traffic is light (command, poll); the broadcast
/// ring must absorb what arrives during a worst-case 40 ms page erase at the
/// host's streaming rate (~1,900 chunks/s ≈ 76 chunks).
pub const UNI_SLOTS: usize = 4;
pub const BCAST_SLOTS: usize = 80;
/// Polls that may collect a command reply before the gift falls back to
/// `Status` (covers a poll that arrived before the command was handled).
pub const REPLY_POLLS: u8 = 2;

/// Static facts the replies need.
#[derive(Debug, Clone, Copy)]
pub struct Ctx {
    pub uid: [u8; 12],
    pub hw_rev: u16,
    pub node_addr: u8,
    pub slot: u8,
    pub reset_reason: u8,
    pub resets: u16,
    /// Bootloader pages write-protected (option bytes).
    pub protected: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Unicast,
    Broadcast,
    /// Erase the next page of the running session ahead of time.
    Erase,
    Idle,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tick {
    None,
    /// Retune the radio.
    Channel(u8),
    /// Start the (verified) app.
    Boot,
    /// Reset (`into_bootloader`: set `REQ_STAY_IN_BOOT`, else clear
    /// `boot_attempts` to give the app a fresh set of tries).
    Reboot {
        into_bootloader: bool,
    },
}

pub struct Recovery {
    pub ota: Ota,
    pub channel: ChannelPlan,
    /// Packets dropped because an RX ring was full (set by the caller),
    /// reported as `Status::bcast_lost` for bring-up and tuning.
    pub ring_drops: u16,
    app: Option<ImageHeader>,
    echo: u8,
    bcast_rx: u16,
    pending: Option<Reply>,
    polls_left: u8,
    verdict_out: bool,
    boot_at: Option<u32>,
    boot_now: bool,
    reboot_at: Option<(u32, bool)>,
    blink: Option<Blink>,
}

/// Indices of chunks of `session` waiting in the broadcast ring.
fn ring_chunks<const N: usize>(bc: &RxRing<N>, session: u8) -> impl Iterator<Item = u16> + '_ {
    bc.iter()
        .filter(move |p| p.len() >= 4 + OTA_CHUNK && p[0] == kind::OTA_CHUNK && p[1] == session)
        .map(|p| u16::from_le_bytes([p[2], p[3]]))
}

impl Recovery {
    /// `app`: the valid image in flash, if any.
    pub fn new(hw_rev: u16, app: Option<ImageHeader>, channel: ChannelPlan) -> Self {
        Self {
            ota: Ota::new(hw_rev),
            channel,
            ring_drops: 0,
            app,
            echo: 0,
            bcast_rx: 0,
            pending: None,
            polls_left: 0,
            verdict_out: false,
            boot_at: None,
            boot_now: false,
            reboot_at: None,
            blink: None,
        }
    }

    pub fn next_action<const U: usize, const B: usize>(&self, uni: &RxRing<U>, bc: &RxRing<B>) -> Action {
        if !uni.is_empty() {
            Action::Unicast
        } else if !bc.is_empty() {
            Action::Broadcast
        } else if self.ota.next_erase().is_some() {
            Action::Erase
        } else {
            Action::Idle
        }
    }

    pub fn erase_ahead<F: RegionFlash>(&mut self, f: &mut F) {
        self.ota.erase_next(f);
    }

    pub fn status(&self, ctx: &Ctx, now: u32) -> Status {
        let mut flags = status_flags::IN_BOOTLOADER;
        if ctx.slot != UNASSIGNED_SLOT {
            flags |= status_flags::SLOT_ASSIGNED;
        }
        if self.blink.is_some() {
            flags |= status_flags::BLINKING;
        }
        if ctx.protected {
            flags |= status_flags::BOOT_PROTECTED;
        }
        Status {
            echo: self.echo,
            flags,
            slot: ctx.slot,
            uptime_ms: now,
            fw_version: BOOT_VERSION as u32,
            bcast_rx: self.bcast_rx,
            bcast_lost: self.ring_drops,
            reset_reason: ctx.reset_reason,
            resets: ctx.resets,
            ..Default::default()
        }
    }

    /// Handle a unicast packet; returns the reply to preload as the next
    /// ack payload. `bc` is the broadcast ring (buffered chunks count as
    /// received in bitmaps). Flash is only read here.
    pub fn on_unicast<const B: usize>(
        &mut self,
        data: &[u8],
        bc: &RxRing<B>,
        now: u32,
        f: &dyn RegionRead,
        ctx: &Ctx,
    ) -> Reply {
        self.channel.on_rx(now);
        let BootPacket::Unicast(u) = bootpkt::decode(data) else {
            return Reply::Status(self.status(ctx, now));
        };
        let seq = u.seq();
        if let Unicast::Poll { .. } = u {
            // The ack of this poll carried the verdict (or it is gone anyway).
            self.boot_now |= self.verdict_out;
            if let Some(r) = self.pending
                && self.polls_left > 0
            {
                self.polls_left -= 1;
                return r;
            }
            self.pending = None;
            self.echo = seq;
            return Reply::Status(self.status(ctx, now));
        }
        self.echo = seq;
        let reply = match u {
            Unicast::GetIdentity { .. } => {
                let mut flags = status_flags::IN_BOOTLOADER;
                if ctx.protected {
                    flags |= status_flags::BOOT_PROTECTED;
                }
                Some(Reply::Identity(Identity {
                    echo: seq,
                    uid: ctx.uid,
                    hw_rev: ctx.hw_rev as u8,
                    fw_version: self.app.map_or(0, |h| h.fw_version),
                    boot_version: BOOT_VERSION,
                    slot: ctx.slot,
                    node_addr: ctx.node_addr,
                    flags,
                    app_crc: self.app.map_or(0, |h| h.app_crc),
                }))
            }
            Unicast::Blink { count, period_10ms, .. } => {
                self.blink = Blink::new(now, count, period_10ms);
                None
            }
            Unicast::Reboot { into_bootloader, .. } => {
                self.reboot_at = Some((now, into_bootloader));
                None
            }
            Unicast::OtaQuery { window, .. } => {
                let mut pending = ring_chunks(bc, self.ota.session());
                Some(Reply::OtaBitmap(self.ota.bitmap(seq, window, &mut pending)))
            }
            Unicast::OtaCommit { session, .. } => {
                self.ota.commit(session, f);
                self.after_ota(now);
                let mut pending = ring_chunks(bc, self.ota.session());
                Some(Reply::OtaBitmap(self.ota.bitmap(seq, 0, &mut pending)))
            }
            // Not decoded by the bootloader (app-only commands).
            Unicast::SetConfig { .. } | Unicast::SetSlot { .. } | Unicast::Poll { .. } => None,
        };
        match reply {
            Some(r) => {
                if matches!(r, Reply::OtaBitmap(b) if b.state == OtaState::Verified) {
                    self.verdict_out = true;
                }
                self.pending = Some(r);
                self.polls_left = REPLY_POLLS;
                r
            }
            None => {
                self.pending = None;
                Reply::Status(self.status(ctx, now))
            }
        }
    }

    /// Handle a broadcast packet (chunks are programmed here, erasing pages
    /// on demand). Returns a node address to adopt (persist + retune pipe 2).
    pub fn on_broadcast<F: RegionFlash>(&mut self, data: &[u8], now: u32, f: &mut F, ctx: &Ctx) -> Option<u8> {
        self.channel.on_rx(now);
        self.bcast_rx = self.bcast_rx.wrapping_add(1);
        match bootpkt::decode(data) {
            BootPacket::OtaBegin(o) => {
                if self.ota.begin(&o) {
                    self.boot_at = None;
                    self.boot_now = false;
                    self.verdict_out = false;
                }
            }
            BootPacket::OtaChunk(c) => {
                self.ota.chunk(&c, f);
                self.after_ota(now);
            }
            BootPacket::SetNodeAddr { uid, node_addr }
                if uid == ctx.uid && valid_node_addr(node_addr) && node_addr != ctx.node_addr =>
            {
                return Some(node_addr);
            }
            _ => {}
        }
        None
    }

    fn after_ota(&mut self, now: u32) {
        if self.boot_at.is_none()
            && let Some(h) = self.ota.verified_header()
        {
            self.boot_at = Some(now);
            self.app = Some(h);
        }
    }

    /// Channel to write to the config before starting a freshly verified
    /// app: the update reached this gift on the current channel, so the
    /// host is there. (A gift rescued on the default channel from an app
    /// that stored a wrong channel would otherwise be unreachable again.)
    pub fn channel_to_store(&self, stored: u8) -> Option<u8> {
        let cur = self.channel.current();
        (self.ota.verified_header().is_some() && cur != stored).then_some(cur)
    }

    pub fn tick(&mut self, now: u32) -> Tick {
        if self.boot_now || self.boot_at.is_some_and(|t| now.wrapping_sub(t) >= BOOT_DELAY_MS) {
            return Tick::Boot;
        }
        if let Some((t, into_bootloader)) = self.reboot_at
            && now.wrapping_sub(t) >= REBOOT_DELAY_MS
        {
            return Tick::Reboot { into_bootloader };
        }
        match self.channel.tick(now) {
            Some(c) => Tick::Channel(c),
            None => Tick::None,
        }
    }

    /// Milliseconds until [`tick`](Self::tick) may return something (for
    /// event-driven callers such as the simulator).
    pub fn next_deadline(&self, now: u32) -> Option<u32> {
        let left = |t: u32, d: u32| d.saturating_sub(now.wrapping_sub(t));
        let mut best = self.channel.next_deadline(now);
        for d in [
            self.boot_now.then_some(0),
            self.boot_at.map(|t| left(t, BOOT_DELAY_MS)),
            self.reboot_at.map(|(t, _)| left(t, REBOOT_DELAY_MS)),
        ]
        .into_iter()
        .flatten()
        {
            best = Some(best.map_or(d, |b| b.min(d)));
        }
        best
    }

    /// Red LED: blink command, fast flicker while an update runs, steady on
    /// after `Verified`, a double flash every 1.5 s when idle, slow blink
    /// without a radio.
    pub fn led(&mut self, now: u32, radio_ok: bool) -> bool {
        if let Some(b) = self.blink {
            match b.phase(now) {
                Some(on) => return on,
                None => self.blink = None,
            }
        }
        if !radio_ok {
            return (now / 500).is_multiple_of(2);
        }
        match self.ota.state() {
            OtaState::Erasing | OtaState::Receiving => (now / 50).is_multiple_of(2),
            OtaState::Verified => true,
            _ => {
                let t = now % 1500;
                t < 80 || (200..280).contains(&t)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::boot::{self, Decision, csr2};
    use crate::channel;
    use crate::ota::mock::*;
    use crate::ota::{chunk_count, validate_image};
    use lumigift_proto::layout::BootShared;
    use lumigift_proto::{Broadcast, DEFAULT_CHANNEL, MAX_PAYLOAD, OtaBitmap, Packet};

    const UID: [u8; 12] = [3; 12];

    fn ctx() -> Ctx {
        Ctx { uid: UID, hw_rev: 1, node_addr: 17, slot: 2, reset_reason: 1, resets: 0, protected: true }
    }

    fn enc(p: Packet) -> Vec<u8> {
        let mut b = [0u8; MAX_PAYLOAD];
        let n = p.encode(&mut b);
        b[..n].to_vec()
    }

    fn chunk_pkt(img: &[u8], session: u8, i: u16) -> Vec<u8> {
        enc(Packet::Broadcast(Broadcast::OtaChunk(chunk_of(img, session, i))))
    }

    fn query(seq: u8, session: u8, window: u8) -> Vec<u8> {
        enc(Packet::Unicast(Unicast::OtaQuery { seq, session, window }))
    }

    fn poll(seq: u8) -> Vec<u8> {
        enc(Packet::Unicast(Unicast::Poll { seq }))
    }

    /// A bootloader in recovery mode driven like the firmware's main loop.
    struct Gift {
        rec: Recovery,
        uni: RxRing<UNI_SLOTS>,
        bc: RxRing<BCAST_SLOTS>,
        flash: MockFlash,
        now: u32,
        preloaded: Option<Reply>,
    }

    impl Gift {
        fn boot(flash: MockFlash) -> Self {
            let app = validate_image(&flash, 1);
            let ch = channel::ChannelPlan::new(DEFAULT_CHANNEL, DEFAULT_CHANNEL, 0);
            Self {
                rec: Recovery::new(1, app, ch),
                uni: RxRing::new(),
                bc: RxRing::new(),
                flash,
                now: 0,
                preloaded: None,
            }
        }

        /// Run the main loop until there is nothing left to do.
        fn run(&mut self) {
            loop {
                let mut buf = [0u8; 32];
                match self.rec.next_action(&self.uni, &self.bc) {
                    Action::Unicast => {
                        let n = self.uni.pop_into(&mut buf).unwrap();
                        let r = self.rec.on_unicast(&buf[..n], &self.bc, self.now, &self.flash, &ctx());
                        self.preloaded = Some(r);
                    }
                    Action::Broadcast => {
                        let n = self.bc.pop_into(&mut buf).unwrap();
                        self.rec.on_broadcast(&buf[..n], self.now, &mut self.flash, &ctx());
                    }
                    Action::Erase => self.rec.erase_ahead(&mut self.flash),
                    Action::Idle => break,
                }
                if self.flash.dead {
                    break;
                }
            }
        }

        fn send_bc(&mut self, p: &[u8]) {
            assert!(self.bc.push(p), "ring full");
        }

        /// Command + poll like the host; returns what the poll's ack carried.
        fn exchange(&mut self, cmd: &[u8], poll_seq: u8) -> Option<Reply> {
            self.uni.push(cmd);
            self.run();
            let ack = self.preloaded;
            self.uni.push(&poll(poll_seq));
            self.run();
            ack
        }

        fn bitmap(&mut self, session: u8, window: u8) -> OtaBitmap {
            match self.exchange(&query(50, session, window), 51) {
                Some(Reply::OtaBitmap(b)) => b,
                other => panic!("{other:?}"),
            }
        }

        /// Stream the whole image and repair until complete. Panics on failure.
        fn update(&mut self, img: &[u8], session: u8) {
            self.send_bc(&enc(Packet::Broadcast(Broadcast::OtaBegin(begin(img, session)))));
            let n = chunk_count(img.len() as u32);
            for i in 0..n {
                self.send_bc(&chunk_pkt(img, session, i));
                if self.bc.is_full() {
                    self.run();
                }
            }
            self.run();
            for _ in 0..3 {
                let b = self.bitmap(session, 0);
                if b.state == OtaState::Verified {
                    return;
                }
                assert_eq!(b.state, OtaState::Receiving);
                for i in b.missing(n as u32).collect::<Vec<_>>() {
                    self.send_bc(&chunk_pkt(img, session, i));
                }
                self.run();
            }
            panic!("update did not complete: {:?}", self.rec.ota.state());
        }
    }

    #[test]
    fn unicast_first_and_buffered_chunks_count_as_received() {
        let img = make_image(9000, 1);
        let mut g = Gift::boot(MockFlash::new());
        g.send_bc(&enc(Packet::Broadcast(Broadcast::OtaBegin(begin(&img, 5)))));
        g.run();
        assert_eq!(g.rec.ota.state(), OtaState::Receiving);
        assert!(g.rec.ota.pages_erased() >= 1, "idle receiver erases ahead");
        // 60 chunks buffered (e.g. arrived during an erase), then a query.
        for i in 0..60 {
            g.send_bc(&chunk_pkt(&img, 5, i));
        }
        g.uni.push(&query(7, 5, 0));
        assert_eq!(g.rec.next_action(&g.uni, &g.bc), Action::Unicast);
        let mut buf = [0u8; 32];
        let n = g.uni.pop_into(&mut buf).unwrap();
        let r = g.rec.on_unicast(&buf[..n], &g.bc, 0, &g.flash, &ctx());
        let Reply::OtaBitmap(b) = r else { panic!() };
        assert_eq!(b.missing_count, chunk_count(img.len() as u32) - 60);
        assert_eq!(b.missing(9999).next(), Some(60));
        g.run();
        assert_eq!(g.rec.ota.map().received(), 60);
    }

    #[test]
    fn poll_keeps_a_command_reply_and_boots_after_the_verdict() {
        let img = make_image(4000, 1);
        let mut g = Gift::boot(MockFlash::new());
        g.update(&img, 9);
        assert_eq!(g.rec.ota.state(), OtaState::Verified);
        // A poll processed after a query (it arrived first) re-offers the reply.
        g.uni.push(&query(20, 9, 0));
        g.uni.push(&poll(21));
        g.run();
        assert!(matches!(g.preloaded, Some(Reply::OtaBitmap(b)) if b.echo == 20));
        // The verdict went out with a poll: the app starts now.
        assert_eq!(g.rec.tick(g.now), Tick::Boot);
        // A new Identity reports the new image.
        let id = g.rec.on_unicast(&enc(Packet::Unicast(Unicast::GetIdentity { seq: 3 })), &g.bc, 0, &g.flash, &ctx());
        let Reply::Identity(id) = id else { panic!() };
        assert_eq!(id.fw_version, 1234);
        assert_ne!(id.app_crc, 0);
        assert!(id.flags & status_flags::BOOT_PROTECTED != 0);
        // Polls after REPLY_POLLS fall back to Status.
        for s in 0..REPLY_POLLS + 1 {
            g.uni.push(&poll(30 + s));
            g.run();
        }
        assert!(matches!(g.preloaded, Some(Reply::Status(_))));
    }

    #[test]
    fn verified_image_boots_on_its_own_if_nobody_asks() {
        let img = make_image(3000, 1);
        let mut g = Gift::boot(MockFlash::new());
        g.send_bc(&enc(Packet::Broadcast(Broadcast::OtaBegin(begin(&img, 1)))));
        for i in 0..chunk_count(img.len() as u32) {
            g.send_bc(&chunk_pkt(&img, 1, i));
            g.run();
        }
        assert_eq!(g.rec.ota.state(), OtaState::Verified);
        assert_eq!(g.rec.tick(BOOT_DELAY_MS - 1), Tick::None);
        assert_eq!(g.rec.next_deadline(BOOT_DELAY_MS - 1), Some(1));
        assert_eq!(g.rec.tick(BOOT_DELAY_MS), Tick::Boot);
    }

    #[test]
    fn begin_mid_transfer_and_new_session_cancel_a_pending_boot() {
        let img = make_image(3000, 1);
        let img2 = make_image_with(3500, 1, 77, 2);
        let mut g = Gift::boot(MockFlash::new());
        g.update(&img, 1);
        // A different host starts another session before the app booted.
        g.update(&img2, 2);
        assert_eq!(validate_image(&g.flash, 1).unwrap().fw_version, 77);
    }

    /// Boot the way the firmware does after a reset.
    fn decide_after_reset(flash: &MockFlash, shared: &BootShared) -> Decision {
        boot::decide(validate_image(flash, 1).is_some(), shared)
    }

    #[test]
    fn power_loss_at_every_phase_leaves_an_updatable_gift() {
        let old = make_image_with(12_000, 1, 1, 1);
        let new = make_image_with(15_100, 1, 2, 2);
        // Reference run: count the flash operations of a complete update.
        let mut f = MockFlash::new();
        f.mem[..old.len()].copy_from_slice(&old);
        let mut g = Gift::boot(f);
        g.update(&new, 1);
        let total = g.flash.ops;
        let erases = g.flash.erases;
        assert_eq!(erases as u32, crate::ota::pages_for(new.len() as u32));
        // Cut the power before operation k: during the erases, in the middle
        // of the transfer, on the last double word, and after the verdict.
        let mut cuts: Vec<usize> = (0..erases + 3).collect();
        cuts.extend((erases + 3..total).step_by(5));
        cuts.extend([total - 2, total - 1, total, total + 1]);
        for k in cuts {
            let mut f = MockFlash::new();
            f.mem[..old.len()].copy_from_slice(&old);
            f.ops_left = Some(k);
            let mut g = Gift::boot(f);
            g.send_bc(&enc(Packet::Broadcast(Broadcast::OtaBegin(begin(&new, 1)))));
            for i in 0..chunk_count(new.len() as u32) {
                if g.flash.dead {
                    break;
                }
                g.send_bc(&chunk_pkt(&new, 1, i));
                if g.bc.is_full() {
                    g.run();
                }
            }
            g.run();
            let completed = !g.flash.dead;
            // Power comes back: a fresh bootloader looks at the flash.
            let mut flash = g.flash;
            flash.power_cycle();
            let shared = boot::latch(BootShared::new(), csr2::PWRRSTF);
            let app = validate_image(&flash, 1);
            match decide_after_reset(&flash, &shared) {
                Decision::Recovery => {
                    assert!(app.is_none(), "cut {k}");
                    assert!(!completed, "cut {k}: a complete update must boot");
                }
                Decision::ListenThenBoot => {
                    // Only ever the complete new image, never a mix.
                    assert_eq!(app.unwrap().fw_version, 2, "cut {k}");
                    assert_eq!(&flash.mem[..new.len()], &new[..], "cut {k}");
                }
            }
            // Whatever happened, a new update goes through.
            let mut g = Gift::boot(flash);
            g.update(&new, 2);
            assert_eq!(validate_image(&g.flash, 1).unwrap().fw_version, 2, "cut {k}");
        }
    }

    #[test]
    fn crash_loops_and_hangs_end_in_recovery() {
        let img = make_image(3000, 1);
        let mut f = MockFlash::new();
        f.mem[..img.len()].copy_from_slice(&img);
        // Power on with a valid image: the bootloader listens, then boots.
        let mut s = boot::latch(BootShared::new(), csr2::PWRRSTF);
        let mut boots = 0;
        // Each boot the app dies before marking itself healthy: alternately
        // a HardFault (-> software reset) and a hang (-> IWDG reset).
        while decide_after_reset(&f, &s) == Decision::ListenThenBoot {
            s.boot_attempts += 1; // the bootloader counts the attempt and jumps
            boots += 1;
            assert!(boots <= BootShared::MAX_BOOT_ATTEMPTS, "never reached recovery");
            let flags = if boots % 2 == 0 { csr2::IWDGRSTF } else { csr2::SFTRSTF } | csr2::PINRSTF;
            s = boot::latch(s, flags);
        }
        assert_eq!(boots, BootShared::MAX_BOOT_ATTEMPTS);
        assert_eq!(decide_after_reset(&f, &s), Decision::Recovery);
        // `Reboot { into_bootloader: false }` from recovery gives the app new tries...
        let mut g = Gift::boot(f);
        g.uni.push(&enc(Packet::Unicast(Unicast::Reboot { seq: 1, into_bootloader: false })));
        g.run();
        assert_eq!(g.rec.tick(g.now + REBOOT_DELAY_MS), Tick::Reboot { into_bootloader: false });
        // ...and a new image can be installed; it starts once verified.
        let mut g = Gift::boot(g.flash);
        g.update(&make_image_with(3100, 1, 5, 9), 1);
        assert_eq!(g.rec.tick(g.now), Tick::Boot);
    }

    #[test]
    fn deaf_app_is_rescued_by_boot_hold_on_the_default_channel() {
        // The app runs (feeds the IWDG, clears boot_attempts) but its radio
        // is dead, and its config even points at another channel. After a
        // power cycle the bootloader's hold window listens on the default
        // channel, whatever is configured.
        assert_eq!(channel::hold_window_channel(), DEFAULT_CHANNEL);
        let pkt = enc(Packet::Broadcast(Broadcast::BootHold));
        assert_eq!(bootpkt::decode(&pkt), BootPacket::BootHold);
        // Having heard it, recovery starts on (and sticks to) the default
        // channel even though the stored channel is 90.
        let mut plan = channel::ChannelPlan::new(90, DEFAULT_CHANNEL, 0);
        assert_eq!(plan.current(), DEFAULT_CHANNEL);
        plan.on_rx(10);
        assert_eq!(plan.tick(500), None);
    }

    #[test]
    fn wrong_channel_in_config_is_still_reachable() {
        let mut g = Gift::boot(MockFlash::new());
        g.rec.channel = channel::ChannelPlan::new(90, 90, 0);
        let mut channels = Vec::new();
        for t in (0..4000).step_by(10) {
            if let Tick::Channel(c) = g.rec.tick(t) {
                channels.push(c);
            }
        }
        assert!(channels.contains(&DEFAULT_CHANNEL));
        // The host (on the default channel) gets through and the gift stays there.
        assert_eq!(g.rec.channel.current(), DEFAULT_CHANNEL);
        g.now = 4000;
        g.uni.push(&poll(1));
        g.run();
        assert!(matches!(g.preloaded, Some(Reply::Status(s)) if s.flags & status_flags::IN_BOOTLOADER != 0));
        for t in (4000..6000).step_by(10) {
            assert_eq!(g.rec.tick(t), Tick::None);
        }
        // Nothing to change without an update...
        assert_eq!(g.rec.channel_to_store(90), None);
        // ...but after one, the app gets the channel the host used.
        g.update(&make_image(3000, 1), 3);
        assert_eq!(g.rec.channel_to_store(90), Some(DEFAULT_CHANNEL));
        assert_eq!(g.rec.channel_to_store(DEFAULT_CHANNEL), None);
    }
}
