//! Connection management (USB bridge / simulator), bridge hot-plug detection
//! and the host-side LED mirror used by tiles, gems and the stage.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use lumigift_core::sim::{SimConfig, SimHandle};
use lumigift_core::{Link, LinkConfig, ShowOutput, Snapshot, Uid};
use lumigift_fx::Renderer;
use lumigift_proto::{LEDS_PER_GIFT, MusicFrame, Rgb, status_flags};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    Usb,
    Sim,
}

pub struct Backend {
    pub link: Arc<Link>,
    pub sim: Option<SimHandle>,
    pub kind: Kind,
    pub started: Instant,
}

impl Backend {
    pub fn usb() -> Self {
        Self {
            link: Arc::new(Link::open_usb(LinkConfig::default(), None)),
            sim: None,
            kind: Kind::Usb,
            started: Instant::now(),
        }
    }

    pub fn sim(n: usize) -> Self {
        let (link, sim) = Link::simulated(LinkConfig::default(), SimConfig::demo(n.clamp(1, 30)));
        Self {
            link: Arc::new(link),
            sim: Some(sim),
            kind: Kind::Sim,
            started: Instant::now(),
        }
    }

    pub fn is_sim(&self) -> bool {
        self.kind == Kind::Sim
    }
}

/// Background thread that enumerates USB bridges once per second.
#[derive(Clone, Default)]
pub struct Detector {
    inner: Arc<DetectorInner>,
}

#[derive(Default)]
struct DetectorInner {
    present: AtomicBool,
    last_error: Mutex<Option<String>>,
    started: AtomicBool,
}

impl Detector {
    pub fn start(&self, ctx: egui::Context) {
        if self.inner.started.swap(true, Ordering::SeqCst) {
            return;
        }
        let inner = self.inner.clone();
        std::thread::Builder::new()
            .name("bridge-detect".into())
            .spawn(move || {
                loop {
                    let (present, err) = match lumigift_core::nrf24_usb::list() {
                        Ok(v) => (!v.is_empty(), None),
                        Err(e) => (false, Some(e.to_string())),
                    };
                    let was = inner.present.swap(present, Ordering::SeqCst);
                    *inner.last_error.lock().unwrap() = err;
                    if was != present {
                        ctx.request_repaint();
                    }
                    std::thread::sleep(Duration::from_millis(1000));
                }
            })
            .ok();
    }

    pub fn probe_now() -> bool {
        lumigift_core::nrf24_usb::list().map(|v| !v.is_empty()).unwrap_or(false)
    }

    pub fn present(&self) -> bool {
        self.inner.present.load(Ordering::SeqCst)
    }
}

/// What the LED mirror should show this frame.
#[derive(Clone, Copy, Debug)]
pub enum LightSource {
    /// A show frame (studio playing / previewing) rendered locally per gift.
    Show(MusicFrame),
    /// Nothing playing: mirror the gifts (simulator LEDs, or the commanded state).
    Mirror,
    Blackout,
}

/// One `lumigift_fx::Renderer` per gift, plus the latest LED colours.
#[derive(Default)]
pub struct Lights {
    renderers: HashMap<Uid, Renderer>,
    pub leds: HashMap<Uid, [Rgb; LEDS_PER_GIFT]>,
    epoch: Option<Instant>,
    /// LED colours changed noticeably this frame (drives repaint policy).
    pub animating: bool,
}

impl Lights {
    pub fn get(&self, uid: &Uid) -> [Rgb; LEDS_PER_GIFT] {
        self.leds.get(uid).copied().unwrap_or([Rgb::BLACK; LEDS_PER_GIFT])
    }

    /// Update every gift's colours. `blink_color` is the colour of the last blink command.
    pub fn update(&mut self, snap: &Snapshot, backend: Option<&Backend>, src: LightSource, blink: Rgb) {
        let epoch = *self.epoch.get_or_insert_with(Instant::now);
        let now_ms = epoch.elapsed().as_millis() as u32;
        let sim_leds: HashMap<Uid, [Rgb; LEDS_PER_GIFT]> = match (src, backend.and_then(|b| b.sim.as_ref())) {
            (LightSource::Mirror, Some(sim)) => sim.gifts().into_iter().map(|g| (g.uid, g.leds)).collect(),
            _ => HashMap::new(),
        };
        let mut changed = false;
        for g in &snap.gifts {
            let blinking = g.online && g.flags & status_flags::BLINKING != 0;
            let mut out = [Rgb::BLACK; LEDS_PER_GIFT];
            match src {
                LightSource::Blackout => {}
                LightSource::Show(frame) => {
                    let r = self.renderers.entry(g.uid).or_insert_with(|| {
                        Renderer::new(u32::from_le_bytes([g.uid.0[8], g.uid.0[9], g.uid.0[10], g.uid.0[11]]))
                    });
                    r.on_frame(&frame, now_ms);
                    let slot = g.slot.unwrap_or(0);
                    r.render(now_ms, slot, &mut out);
                    if !g.online && backend.is_some() {
                        out = [Rgb::BLACK; LEDS_PER_GIFT];
                    }
                }
                LightSource::Mirror => {
                    if let Some(l) = sim_leds.get(&g.uid) {
                        out = *l;
                    } else if blinking {
                        // Hardware: show the commanded blink colour, pulsing at 250 ms.
                        if (now_ms / 125).is_multiple_of(2) {
                            out = [blink; LEDS_PER_GIFT];
                        }
                    }
                }
            }
            let prev = self.leds.insert(g.uid, out);
            if prev.is_some_and(|p| p != out) {
                changed = true;
            }
        }
        self.animating = changed;
    }

    /// Feed renderers without a snapshot gift list (preview with no gifts uses virtual slots).
    pub fn render_virtual(&mut self, frame: &MusicFrame, slots: u8) -> Vec<[Rgb; LEDS_PER_GIFT]> {
        let epoch = *self.epoch.get_or_insert_with(Instant::now);
        let now_ms = epoch.elapsed().as_millis() as u32;
        (0..slots)
            .map(|s| {
                let uid = Uid([0xEE, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, s]);
                let r = self.renderers.entry(uid).or_insert_with(|| Renderer::new(s as u32 + 7));
                r.on_frame(frame, now_ms);
                let mut o = [Rgb::BLACK; LEDS_PER_GIFT];
                r.render(now_ms, s, &mut o);
                o
            })
            .collect()
    }
}

/// Send helper: what the show stream should broadcast.
pub fn show_output(blackout: bool, live: bool, frame: Option<MusicFrame>) -> ShowOutput {
    if blackout {
        ShowOutput::Blackout
    } else if let (true, Some(f)) = (live, frame) {
        ShowOutput::Music(f)
    } else {
        ShowOutput::Idle
    }
}
