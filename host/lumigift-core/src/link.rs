//! The public [`Link`] handle.

use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use arc_swap::ArcSwap;
use crossbeam_channel::{Receiver, Sender};
use lumigift_proto::Rgb;
use nrf24_usb::Transport;

use crate::clock::ClockSync;
use crate::config::LinkConfig;
use crate::ops::{self, Ctx};
use crate::ota;
use crate::scheduler::{Radio, Scheduler};
use crate::sim::{SimConfig, SimHandle, SimTransport};
use crate::state::{Shared, State};
use crate::types::*;

/// Connection to the gifts through one bridge (real or simulated).
///
/// Cheap to share behind an `Arc`; every method takes `&self` and never
/// blocks on the radio (operations return a [`Pending`]). Dropping the last
/// handle (or calling [`Link::shutdown`]) stops all threads.
pub struct Link {
    shared: Arc<Shared>,
    ctx: Ctx,
    rescan_tx: Sender<Sender<Result<usize, LinkError>>>,
    threads: Mutex<Vec<JoinHandle<()>>>,
}

impl Link {
    /// Start a link. `connect` is called (again, once per second) whenever
    /// no transport is connected, so unplugging and replugging the bridge
    /// recovers automatically.
    pub fn start<F>(cfg: LinkConfig, connect: F) -> Link
    where
        F: FnMut() -> Result<Box<dyn Transport>, nrf24_usb::Error> + Send + 'static,
    {
        let epoch = Instant::now();
        let state = State::new(&cfg);
        let shared = Arc::new(Shared {
            epoch,
            state: Mutex::new(state),
            snapshot: ArcSwap::from_pointee(Snapshot::default()),
            clock: ArcSwap::from_pointee(ClockSync::unsynced(epoch)),
            show: ArcSwap::from_pointee(ShowOutput::Idle),
            show_events: AtomicU8::new(0),
            show_paused: AtomicBool::new(false),
            polling_paused: AtomicBool::new(false),
            discovery_paused: AtomicBool::new(false),
            ota_running: AtomicBool::new(false),
            ota_cancel: AtomicBool::new(false),
            connected: AtomicBool::new(false),
            shutdown: AtomicBool::new(false),
            cfg,
        });
        let (req_tx, req_rx) = crossbeam_channel::unbounded();
        let (rescan_tx, rescan_rx) = crossbeam_channel::unbounded();
        let ctx = Ctx {
            shared: shared.clone(),
            radio: Radio { tx: req_tx },
        };
        let sched = Scheduler::new(shared.clone(), Box::new(connect), req_rx);
        let t1 = std::thread::Builder::new()
            .name("lumigift-link".into())
            .spawn(move || sched.run())
            .expect("spawn link thread");
        let dctx = ctx.clone();
        let t2 = std::thread::Builder::new()
            .name("lumigift-discovery".into())
            .spawn(move || ops::discovery_thread(dctx, rescan_rx))
            .expect("spawn discovery thread");
        Link {
            shared,
            ctx,
            rescan_tx,
            threads: Mutex::new(vec![t1, t2]),
        }
    }

    /// Start a link to a USB bridge (first one found, or by serial number).
    pub fn open_usb(cfg: LinkConfig, serial: Option<String>) -> Link {
        Link::start(cfg, move || {
            let b = nrf24_usb::Bridge::open(serial.as_deref())?;
            Ok(Box::new(b) as Box<dyn Transport>)
        })
    }

    /// Start a link to a simulated bridge + gifts.
    pub fn simulated(cfg: LinkConfig, sim: SimConfig) -> (Link, SimHandle) {
        let (t, h) = SimTransport::new(sim);
        let mut t = Some(t);
        let link = Link::start(cfg, move || {
            t.take()
                .map(|t| Box::new(t) as Box<dyn Transport>)
                .ok_or_else(|| nrf24_usb::Error::Usb("simulator already in use".into()))
        });
        (link, h)
    }

    // -- observation ------------------------------------------------------

    /// The latest snapshot (updated ~20×/s). Very cheap: an `Arc` clone.
    pub fn snapshot(&self) -> Arc<Snapshot> {
        self.shared.snapshot.load_full()
    }

    /// Wait until a snapshot satisfies `pred` (handy for scripts and tests).
    pub fn wait_for(
        &self,
        timeout: Duration,
        mut pred: impl FnMut(&Snapshot) -> bool,
    ) -> Option<Arc<Snapshot>> {
        let deadline = Instant::now() + timeout;
        loop {
            let s = self.snapshot();
            if pred(&s) {
                return Some(s);
            }
            if Instant::now() >= deadline {
                return None;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    /// Subscribe to log events as they happen (in addition to `Snapshot::events`).
    pub fn subscribe_log(&self) -> Receiver<LogEvent> {
        let (tx, rx) = crossbeam_channel::unbounded();
        self.shared.lock().log_subs.push(tx);
        rx
    }

    pub fn is_connected(&self) -> bool {
        self.shared.connected.load(Ordering::Relaxed)
    }

    pub fn config(&self) -> &LinkConfig {
        &self.shared.cfg
    }

    // -- clock --------------------------------------------------------------

    /// The current bridge-clock mapping (copy it; it stays valid for seconds).
    pub fn clock(&self) -> ClockSync {
        **self.shared.clock.load()
    }

    /// Bridge time now, unwrapped µs (low 32 bits = the bridge's own clock).
    pub fn bridge_now_us(&self) -> u64 {
        self.clock().bridge_now_us()
    }

    // -- show -----------------------------------------------------------------

    /// Set what the show stream broadcasts from the next 10 ms tick on.
    /// Call as often as you like (e.g. 100×/s from the audio engine).
    pub fn set_show(&self, out: ShowOutput) {
        let events = match &out {
            ShowOutput::Music(f) => f.flags,
            ShowOutput::Layers(l) => l.iter().fold(0, |a, f| a | f.flags),
            _ => 0,
        } & EDGE_FLAGS;
        if events != 0 {
            self.shared.show_events.fetch_or(events, Ordering::Relaxed);
        }
        self.shared.show.store(Arc::new(out));
    }

    pub fn show(&self) -> Arc<ShowOutput> {
        self.shared.show.load_full()
    }

    // -- discovery --------------------------------------------------------------

    /// Run a full address sweep now; resolves to the number of online gifts.
    pub fn rescan(&self) -> Pending<usize> {
        let (tx, rx) = crossbeam_channel::bounded(1);
        if self.rescan_tx.send(tx).is_err() {
            return Pending::ready(Err(LinkError::Shutdown));
        }
        Pending::new(rx)
    }

    // -- commands -----------------------------------------------------------------

    fn spawn<T: Send + 'static>(
        &self,
        name: &str,
        f: impl FnOnce(&Ctx) -> Result<T, LinkError> + Send + 'static,
    ) -> Pending<T> {
        let (tx, rx) = crossbeam_channel::bounded(1);
        let ctx = self.ctx.clone();
        let r = std::thread::Builder::new()
            .name(format!("lumigift-{name}"))
            .spawn(move || {
                let r = f(&ctx);
                // Make the snapshot reflect the outcome before reporting it.
                ctx.shared.publish();
                let _ = tx.send(r);
            });
        match r {
            Ok(_) => Pending::new(rx),
            Err(e) => Pending::ready(Err(LinkError::Other(e.to_string()))),
        }
    }

    /// Blink `count` times with the given period (resolution 10 ms).
    pub fn blink(
        &self,
        target: impl Into<Target>,
        color: Rgb,
        count: u8,
        period: Duration,
    ) -> Pending<CmdReport> {
        let t = target.into();
        self.spawn("blink", move |c| ops::blink(c, &t, color, count, period))
    }

    /// Blink a distinctive pattern (white ×3, then red, green, blue).
    pub fn identify(&self, target: impl Into<Target>) -> Pending<CmdReport> {
        let t = target.into();
        self.spawn("identify", move |c| ops::identify(c, &t))
    }

    /// Assign a show slot (`None` = unassigned).
    pub fn set_slot(&self, gift: Uid, slot: Option<u8>) -> Pending<()> {
        self.spawn("set-slot", move |c| ops::set_slot(c, gift, slot))
    }

    /// Assign slots to all online gifts.
    pub fn auto_assign_slots(&self, mode: SlotAssign) -> Pending<CmdReport> {
        self.spawn("auto-slots", move |c| ops::auto_assign_slots(c, mode))
    }

    pub fn reboot(&self, target: impl Into<Target>, into_bootloader: bool) -> Pending<CmdReport> {
        let t = target.into();
        self.spawn("reboot", move |c| ops::reboot(c, &t, into_bootloader))
    }

    /// Set a gift config value (see `lumigift_proto::config_key`).
    pub fn set_config(&self, target: impl Into<Target>, key: u8, value: u16) -> Pending<CmdReport> {
        let t = target.into();
        self.spawn("config", move |c| ops::set_config(c, &t, key, value))
    }

    /// Move all online gifts, then the bridge, to another radio channel.
    /// Offline gifts are left behind (a warning is logged).
    pub fn set_channel(&self, channel: u8) -> Pending<CmdReport> {
        self.spawn("channel", move |c| ops::set_channel(c, channel))
    }

    /// Change host-side metadata (name, colour tag, notes). Saved immediately.
    pub fn update_meta(&self, gift: Uid, update: MetaUpdate) -> Result<(), LinkError> {
        let mut st = self.shared.lock();
        let r = st.meta.update(gift, |m| {
            if let Some(n) = update.name {
                m.name = n.filter(|s| !s.trim().is_empty());
            }
            if let Some(c) = update.color {
                m.color = c;
            }
            if let Some(n) = update.notes {
                m.notes = n;
            }
        });
        r.map(|_| ())
            .map_err(|e| LinkError::Other(format!("saving metadata: {e}")))
    }

    /// Shorthand for renaming (`None` clears the name).
    pub fn rename(&self, gift: Uid, name: Option<String>) -> Result<(), LinkError> {
        self.update_meta(
            gift,
            MetaUpdate {
                name: Some(name),
                ..Default::default()
            },
        )
    }

    // -- OTA ------------------------------------------------------------------

    /// Update `targets` with a firmware image (header + app, as produced by
    /// the firmware build). The image is validated before anything is sent;
    /// an invalid image or an update already running is reported by the
    /// returned `Pending` immediately. Progress appears in `Snapshot::ota`.
    ///
    /// Targets that already run this exact image (same `fw_version` and app
    /// CRC) are skipped (`OtaReport::skipped`) when
    /// `OtaOptions::skip_up_to_date` is set; see [`start_ota_with`](Self::start_ota_with).
    pub fn start_ota(&self, image: Vec<u8>, targets: Vec<Uid>) -> Pending<OtaReport> {
        self.start_ota_with(image, targets, false)
    }

    /// [`start_ota`](Self::start_ota); `force` also updates targets that
    /// already run this exact image.
    pub fn start_ota_with(
        &self,
        image: Vec<u8>,
        targets: Vec<Uid>,
        force: bool,
    ) -> Pending<OtaReport> {
        let img = match ota::validate(&image) {
            Ok(i) => i,
            Err(e) => return Pending::ready(Err(e)),
        };
        if !self.is_connected() {
            return Pending::ready(Err(LinkError::NotConnected));
        }
        if self
            .shared
            .ota_running
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_err()
        {
            return Pending::ready(Err(LinkError::Busy(
                "a firmware update is already running".into(),
            )));
        }
        self.shared.ota_cancel.store(false, Ordering::Relaxed);
        // Clears `ota_running` even if the thread never runs.
        let flag = OtaFlag(self.shared.clone());
        self.spawn("ota", move |c| {
            let _flag = flag;
            ota::run(c, img, targets, force)
        })
    }

    /// Rescue gifts whose app cannot be reached (dead radio, wrong channel,
    /// crash at start-up): broadcast `BootHold` continuously on the default
    /// channel for `duration` while the user power-cycles them. Every
    /// bootloader listens there for ~150 ms after power-on, so the gifts
    /// stay in recovery mode, ready for [`start_ota`](Self::start_ota). The
    /// report lists the gifts seen in their bootloader (known node addresses
    /// are polled during the rescue, a discovery sweep runs at the end).
    /// Fails if a firmware update is running.
    pub fn rescue(&self, duration: Duration) -> Pending<RescueReport> {
        if self.ota_running() {
            return Pending::ready(Err(LinkError::Busy("a firmware update is running".into())));
        }
        self.spawn("rescue", move |c| ops::rescue(c, duration))
    }

    /// Ask the running update to stop at the next safe point.
    pub fn cancel_ota(&self) {
        self.shared.ota_cancel.store(true, Ordering::Relaxed);
    }

    pub fn ota_running(&self) -> bool {
        self.shared.ota_running.load(Ordering::Relaxed)
    }

    // -- lifecycle ------------------------------------------------------------

    /// Stop all threads and release the transport.
    pub fn shutdown(&self) {
        self.shared.shutdown.store(true, Ordering::Relaxed);
        self.shared.ota_cancel.store(true, Ordering::Relaxed);
        let threads: Vec<_> = self
            .threads
            .lock()
            .map(|mut t| t.drain(..).collect())
            .unwrap_or_default();
        for t in threads {
            let _ = t.join();
        }
    }
}

struct OtaFlag(Arc<Shared>);

impl Drop for OtaFlag {
    fn drop(&mut self) {
        self.0.ota_running.store(false, Ordering::Relaxed);
    }
}

impl Drop for Link {
    fn drop(&mut self) {
        self.shutdown();
    }
}
