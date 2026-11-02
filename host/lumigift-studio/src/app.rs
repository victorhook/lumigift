//! The application shell: rail, page header, inspector, status bar, command
//! palette, shortcuts, toasts, persistence and the show → light pipeline.

use std::collections::{BTreeSet, HashMap};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use egui::{
    Align, Align2, Color32, CornerRadius, Key, KeyboardShortcut, Layout, Modifiers, Rect, Sense,
    Stroke, Ui, pos2, vec2,
};
use lumigift_core::{BridgeState, LogLevel, Pending, Snapshot, Target, Uid};
use lumigift_proto::Rgb;
use serde::{Deserialize, Serialize};

use crate::backend::{Backend, Detector, LightSource, Lights};
use crate::health::{Health, HealthTracker, plural};
use crate::icons;
use crate::pages::{firmware::Firmware, fleet::Fleet, settings::SettingsPage, stage_page::StagePage, studio::Studio};
use crate::palette::{CommandPalette, PaletteAction};
use crate::screenshot::Screenshotter;
use crate::stage::Stage;
use crate::theme::{self, *};
use crate::toasts::{ToastKind, Toasts};
use crate::widgets;

#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize, Default, Hash)]
pub enum Page {
    #[default]
    Fleet,
    Studio,
    Lighting,
    Stage,
    Firmware,
    Settings,
}

impl Page {
    pub const ALL: [Page; 6] = [Page::Fleet, Page::Studio, Page::Lighting, Page::Stage, Page::Firmware, Page::Settings];
    pub fn name(self) -> &'static str {
        match self {
            Page::Fleet => "Fleet",
            Page::Studio => "Studio",
            Page::Lighting => "Lighting",
            Page::Stage => "Stage",
            Page::Firmware => "Firmware",
            Page::Settings => "Settings",
        }
    }
    pub fn icon(self) -> &'static str {
        match self {
            Page::Fleet => icons::GIFT,
            Page::Studio => icons::WAVEFORM,
            Page::Lighting => icons::LIGHTBULB,
            Page::Stage => icons::CUBE,
            Page::Firmware => icons::DOWNLOAD,
            Page::Settings => icons::GEAR,
        }
    }
}

/// Everything persisted between runs (eframe storage).
#[derive(Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Persist {
    pub page: Page,
    pub inspector_open: bool,
    pub event_log_open: bool,
    pub fleet_list_view: bool,
    pub fleet_sort: usize,
    /// Stage layout per gift UID (hex): [x, z, yaw] in metres / radians.
    #[serde(rename = "stage_layout_m")]
    pub stage_positions: HashMap<String, [f32; 3]>,
    pub camera: Option<crate::stage::CameraState>,
    pub last_show: Option<PathBuf>,
    pub last_audio: Option<PathBuf>,
    pub recent_shows: Vec<PathBuf>,
    pub latency_ms: f32,
    pub default_latency_ms: f32,
    pub zoom: f32,
    pub reduce_motion: bool,
    pub limit_flashing: bool,
    pub follow_continuous: bool,
    pub sim_count: usize,
    pub stage_split_h: f32,
    pub audio_device: Option<String>,
    pub nimh: bool,
    pub lanes_collapsed: [bool; 7],
    pub sim_banner_dismissed: bool,
    pub stage_labels: bool,
    pub list_hint_dismissed: bool,
    /// Lighting page: edited look, groups, favourites.
    pub lighting: crate::pages::lighting::LightingPersist,
}

impl Default for Persist {
    fn default() -> Self {
        Self {
            page: Page::Fleet,
            inspector_open: true,
            event_log_open: true,
            fleet_list_view: false,
            fleet_sort: 0,
            stage_positions: HashMap::new(),
            camera: None,
            last_show: None,
            last_audio: None,
            recent_shows: Vec::new(),
            latency_ms: 42.0,
            default_latency_ms: 42.0,
            zoom: 1.0,
            reduce_motion: false,
            limit_flashing: true,
            follow_continuous: false,
            sim_count: 10,
            stage_split_h: 380.0,
            audio_device: None,
            nimh: false,
            lanes_collapsed: [false; 7],
            sim_banner_dismissed: false,
            stage_labels: true,
            list_hint_dismissed: false,
            lighting: Default::default(),
        }
    }
}

pub struct StartOptions {
    pub sim: Option<usize>,
    pub open_audio: Option<PathBuf>,
    pub open_show: Option<PathBuf>,
    pub open_firmware: Option<PathBuf>,
    pub screenshot_dir: Option<PathBuf>,
}

/// Command operations in flight, resolved into toasts.
pub struct Op {
    pub label: String,
    pub success: String,
    pub pending: OpKind,
}

pub enum OpKind {
    Report(Pending<lumigift_core::CmdReport>),
    Unit(Pending<()>),
    Count(Pending<usize>),
}

#[derive(Clone)]
pub enum Confirm {
    RebootAll,
    BootloaderGift(Uid),
}

pub struct App {
    pub persist: Persist,
    pub backend: Option<Backend>,
    pub detector: Detector,
    pub snap: Arc<Snapshot>,
    pub health: HashMap<Uid, Health>,
    pub tracker: HealthTracker,
    pub lights: Lights,
    pub selection: BTreeSet<Uid>,
    pub focus: Option<Uid>,
    pub live: bool,
    pub live_engaged_at: Option<Instant>,
    pub blackout: bool,
    pub performance: bool,
    perf_last_move: Instant,
    pub toasts: Toasts,
    switch_hw_toast: Option<u64>,
    pub ops: Vec<Op>,
    pub palette: CommandPalette,
    pub cheatsheet: bool,
    pub confirm: Option<Confirm>,
    pub stage: Stage,
    pub stage_preview: Stage,
    pub fleet: Fleet,
    pub firmware: Firmware,
    pub studio: Studio,
    pub stage_page: StagePage,
    pub settings: SettingsPage,
    /// Lighting page state; owns the show output while armed.
    pub lighting: crate::pages::lighting::Lighting,
    pub blink_color: Rgb,
    page_changed_at: Instant,
    last_page: Page,
    fps: f32,
    pub screenshot: Option<Screenshotter>,
    pub compact: bool,
    pub renaming: Option<(Uid, String)>,
    pub slot_popup: Option<Uid>,
    connected_once: bool,
}

impl App {
    pub fn new(cc: &eframe::CreationContext<'_>, opts: StartOptions) -> Self {
        crate::fonts::install(&cc.egui_ctx);
        theme::apply(&cc.egui_ctx);
        let mut persist: Persist = cc
            .storage
            .and_then(|s| eframe::get_value(s, "lumigift-studio"))
            .unwrap_or_default();
        if opts.screenshot_dir.is_some() {
            persist = Persist::default();
        }
        cc.egui_ctx.set_zoom_factor(persist.zoom.clamp(0.8, 1.5));
        let detector = Detector::default();
        detector.start(cc.egui_ctx.clone());

        let backend = match opts.sim {
            Some(n) => {
                persist.sim_count = n;
                Some(Backend::sim(n))
            }
            None if Detector::probe_now() => Some(Backend::usb()),
            None => None,
        };
        let rs = cc.wgpu_render_state.as_ref().expect("Lumigift Studio needs the wgpu backend");
        let mut stage = Stage::new(rs);
        if let Some(cam) = persist.camera {
            stage.set_camera(cam);
        }
        let stage_preview = Stage::new(rs);
        let mut app = Self {
            studio: Studio::new(&persist),
            persist,
            backend,
            detector,
            snap: Arc::new(Snapshot::default()),
            health: HashMap::new(),
            tracker: HealthTracker::default(),
            lights: Lights::default(),
            selection: BTreeSet::new(),
            focus: None,
            live: false,
            live_engaged_at: None,
            blackout: false,
            performance: false,
            perf_last_move: Instant::now(),
            toasts: Toasts::default(),
            switch_hw_toast: None,
            ops: Vec::new(),
            palette: CommandPalette::default(),
            cheatsheet: false,
            confirm: None,
            stage,
            stage_preview,
            fleet: Fleet::default(),
            firmware: Firmware::default(),
            stage_page: StagePage::default(),
            settings: SettingsPage::default(),
            lighting: Default::default(),
            blink_color: Rgb::new(0xFF, 0xF0, 0xDC),
            page_changed_at: Instant::now(),
            last_page: Page::Fleet,
            fps: 60.0,
            screenshot: opts.screenshot_dir.map(Screenshotter::new),
            compact: false,
            renaming: None,
            slot_popup: None,
            connected_once: false,
        };
        app.last_page = app.persist.page;
        if app.persist.camera.is_some() {
            app.stage_page.framed_n = crate::pages::stage_page::SKIP_AUTO_FRAME;
        }
        app.tracker.nimh = app.persist.nimh;
        if let Some(p) = opts.open_audio {
            app.studio.open_audio(p);
        } else if let Some(p) = opts.open_show.clone().or(app.persist.last_show.clone())
            && (opts.open_show.is_some() || p.exists()) {
                app.studio.open_show(p, &mut app.toasts);
            }
        if let Some(p) = opts.open_firmware {
            app.firmware.load(p);
        }
        app
    }

    // -- helpers ------------------------------------------------------------

    pub fn link(&self) -> Option<&Arc<lumigift_core::Link>> {
        self.backend.as_ref().map(|b| &b.link)
    }

    pub fn is_sim(&self) -> bool {
        self.backend.as_ref().is_some_and(|b| b.is_sim())
    }

    pub fn connected(&self) -> bool {
        matches!(self.snap.bridge.state, BridgeState::Connected)
    }

    pub fn gift_name(&self, uid: &Uid) -> String {
        self.snap
            .gift(uid)
            .map(|g| g.meta.name.clone().unwrap_or_else(|| default_name(uid)))
            .unwrap_or_else(|| default_name(uid))
    }

    pub fn slot_count(&self) -> u8 {
        let max = self.snap.gifts.iter().filter_map(|g| g.slot).max().map(|s| s + 1).unwrap_or(0);
        max.max(self.snap.gifts.len() as u8).max(1)
    }

    /// Select exactly one gift and focus it.
    pub fn select(&mut self, uid: Uid) {
        self.selection = [uid].into();
        self.focus = Some(uid);
    }

    pub fn set_page(&mut self, p: Page) {
        self.persist.page = p;
    }

    /// Shut down the current link (if any) and switch to `backend`.
    fn replace_backend(&mut self, backend: Backend) {
        if let Some(b) = self.backend.take() {
            b.link.shutdown();
        }
        self.backend = Some(backend);
        self.selection.clear();
        self.focus = None;
        self.live = false;
        self.lighting.release();
    }

    pub fn start_sim(&mut self, n: usize) {
        self.persist.sim_count = n;
        self.replace_backend(Backend::sim(n));
        self.toasts.info(format!("Simulator started with {n} virtual gifts"));
    }

    pub fn start_usb(&mut self) {
        self.replace_backend(Backend::usb());
    }

    /// Set the UI zoom (clamped to 80–150 %).
    fn set_zoom(&mut self, ctx: &egui::Context, zoom: f32) {
        self.persist.zoom = zoom.clamp(0.8, 1.5);
        ctx.set_zoom_factor(self.persist.zoom);
    }

    /// "Gift ABCD" for one gift, "N gifts" for several.
    fn targets_label(&self, uids: &[Uid]) -> String {
        if uids.len() == 1 { self.gift_name(&uids[0]) } else { format!("{} gifts", uids.len()) }
    }

    pub fn push_op(&mut self, label: impl Into<String>, success: impl Into<String>, pending: OpKind) {
        self.ops.push(Op { label: label.into(), success: success.into(), pending });
    }

    pub fn blink(&mut self, uids: Vec<Uid>) {
        let Some(link) = self.link().cloned() else { return };
        if uids.is_empty() {
            return;
        }
        let label = self.targets_label(&uids);
        let p = link.blink(Target::Gifts(uids), self.blink_color, 6, Duration::from_millis(250));
        self.push_op(format!("Blink {label}"), String::new(), OpKind::Report(p));
        self.fleet.ripple_at = Some(Instant::now());
    }

    pub fn reboot(&mut self, uids: Vec<Uid>, bootloader: bool) {
        let Some(link) = self.link().cloned() else { return };
        let label = self.targets_label(&uids);
        let p = link.reboot(Target::Gifts(uids), bootloader);
        let what = if bootloader { " into the bootloader" } else { "" };
        self.toasts.info(format!("Rebooting {label}{what}…"));
        self.push_op(format!("Reboot {label}"), format!("{label} rebooted"), OpKind::Report(p));
    }

    pub fn set_slot(&mut self, uid: Uid, slot: Option<u8>) {
        let Some(link) = self.link().cloned() else { return };
        let name = self.gift_name(&uid);
        let p = link.set_slot(uid, slot);
        let s = slot.map(crate::health::fmt_slot).unwrap_or("no slot".into());
        self.push_op(format!("Assign {name} to {s}"), format!("{name} is now {s}"), OpKind::Unit(p));
    }

    pub fn rename(&mut self, uid: Uid, name: String) {
        let Some(link) = self.link().cloned() else { return };
        let name = name.trim().chars().take(24).collect::<String>();
        let r = link.rename(uid, if name.is_empty() { None } else { Some(name.clone()) });
        match r {
            Ok(()) => self.toasts.success(format!("Renamed to {name}")),
            Err(e) => self.toasts.error("Couldn't rename the gift", e.to_string()),
        }
    }

    pub fn rescan(&mut self) {
        let Some(link) = self.link().cloned() else { return };
        let p = link.rescan();
        self.push_op("Rescan", String::new(), OpKind::Count(p));
    }

    pub fn auto_assign(&mut self) {
        let Some(link) = self.link().cloned() else { return };
        let p = link.auto_assign_slots(lumigift_core::SlotAssign::KeepExisting);
        self.push_op("Auto-assign slots", "Slots assigned", OpKind::Report(p));
    }

    pub fn toggle_live(&mut self) {
        if !self.live {
            if self.backend.is_none() {
                self.toasts.push(ToastKind::Warn, "Connect the bridge or start the simulator to go LIVE", None);
                return;
            }
            if self.link().is_some_and(|l| l.ota_running()) {
                self.toasts.push(ToastKind::Warn, "LIVE is unavailable during a firmware update", None);
                return;
            }
            self.live = true;
            self.live_engaged_at = Some(Instant::now());
            // Going LIVE takes the gifts back from the Lighting page.
            self.lighting.release();
        } else {
            self.live = false;
            self.live_engaged_at = None;
            if let Some(l) = self.link() {
                l.set_show(lumigift_core::ShowOutput::Idle);
            }
        }
    }

    pub fn toggle_blackout(&mut self) {
        self.blackout = !self.blackout;
        if let Some(l) = self.link() {
            l.set_show(if self.blackout { lumigift_core::ShowOutput::Blackout } else { lumigift_core::ShowOutput::Idle });
        }
        if self.blackout {
            self.toasts.push(ToastKind::Warn, "Blackout: all gifts dark", Some("Press Ctrl+Shift+B again to restore".into()));
        } else {
            self.toasts.info("Blackout released");
        }
    }

    pub fn set_performance(&mut self, ctx: &egui::Context, on: bool) {
        self.performance = on;
        ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(on));
        self.perf_last_move = Instant::now();
    }

    // -- per-frame logic ----------------------------------------------------

    fn update_state(&mut self, ctx: &egui::Context) {
        if let Some(b) = &self.backend {
            self.snap = b.link.snapshot();
        } else {
            self.snap = Arc::new(Snapshot::default());
        }
        if self.connected() {
            self.connected_once = true;
        }
        let now = Instant::now();
        let ota = self.snap.ota.clone();
        self.tracker.nimh = self.persist.nimh;
        self.tracker.show_active = self.snap.link.frames_per_s > 50.0;
        let mut h = HashMap::with_capacity(self.snap.gifts.len());
        for g in &self.snap.gifts {
            h.insert(g.uid, self.tracker.evaluate(g, ota.as_ref(), now));
        }
        self.health = h;
        self.selection.retain(|u| self.snap.gift(u).is_some());
        if self.focus.is_some_and(|u| self.snap.gift(&u).is_none()) {
            self.focus = None;
        }

        // Bridge hot-plug: no backend → open as soon as one appears; in sim → offer to switch.
        let present = self.detector.present();
        match &self.backend {
            None if present => {
                self.start_usb();
                self.toasts.success("Bridge connected");
            }
            Some(b) if b.is_sim() && present && self.switch_hw_toast.is_none() => {
                self.switch_hw_toast = Some(self.toasts.push_action(
                    ToastKind::Info,
                    "A Lumigift bridge was plugged in",
                    Some("Switch from the simulator to real hardware?".into()),
                    Some("Switch to hardware"),
                ));
            }
            _ => {}
        }
        if !present {
            self.switch_hw_toast = None;
        }

        // Resolve operations.
        let mut done = Vec::new();
        for (i, op) in self.ops.iter_mut().enumerate() {
            let res: Option<Result<String, String>> = match &mut op.pending {
                OpKind::Report(p) => p.try_take().map(|r| match r {
                    Ok(rep) if rep.all_ok() => Ok(op.success.clone()),
                    Ok(rep) => Err(format!("{} ok, {} failed: {}", rep.ok.len(), rep.failed.len(),
                        rep.failed.first().map(|f| f.1.to_string()).unwrap_or_default())),
                    Err(e) => Err(e.to_string()),
                }),
                OpKind::Unit(p) => p.try_take().map(|r| r.map(|_| op.success.clone()).map_err(|e| e.to_string())),
                OpKind::Count(p) => p.try_take().map(|r| r.map(|n| if n > 0 { format!("Found {n} new gift{}", plural(n)) } else { "Rescan complete: no new gifts".into() }).map_err(|e| e.to_string())),
            };
            if let Some(r) = res {
                done.push((i, op.label.clone(), r));
            }
        }
        for (i, label, r) in done.into_iter().rev() {
            self.ops.remove(i);
            match r {
                Ok(s) if !s.is_empty() => self.toasts.success(s),
                Ok(_) => {}
                Err(e) => self.toasts.error(format!("{label} failed"), e),
            }
        }

        // Show → light pipeline.
        let slots = self.slot_count();
        // Preview and radio frames are timed separately (see Studio::tick):
        // only the real radio path gets the calibrated latency.
        let radio_path = match (self.live, self.backend.is_some(), self.is_sim()) {
            (true, true, true) => crate::pages::studio::RadioPath::Simulator,
            (true, true, false) => crate::pages::studio::RadioPath::Hardware,
            _ => crate::pages::studio::RadioPath::None,
        };
        let display_dt = ctx.input(|i| i.predicted_dt) as f64;
        let frames = self.studio.tick(&self.persist, slots, display_dt, radio_path);
        let frame = frames.map(|f| f.preview);
        if let Some(link) = self.backend.as_ref().map(|b| b.link.clone()) {
            let out = crate::backend::show_output(self.blackout, self.live, frames.and_then(|f| f.radio));
            if self.live || self.blackout {
                link.set_show(out);
            }
        }
        let use_sim_mirror = self.is_sim() && self.live;
        let src = if self.blackout {
            if self.is_sim() { LightSource::Mirror } else { LightSource::Blackout }
        } else if let (Some(f), false) = (frame, use_sim_mirror) {
            LightSource::Show(f)
        } else {
            LightSource::Mirror
        };
        let snap = self.snap.clone();
        self.lights.update(&snap, self.backend.as_ref(), src, self.blink_color);
        // Lighting page output (ownership rules in pages/lighting.rs).
        crate::pages::lighting::drive(self);

        let dt = ctx.input(|i| i.unstable_dt).max(1e-4);
        self.fps = self.fps * 0.95 + (1.0 / dt) * 0.05;
    }

    // -- shortcuts ----------------------------------------------------------

    fn global_shortcuts(&mut self, ctx: &egui::Context) {
        let typing = ctx.egui_wants_keyboard_input();
        let ctrl = Modifiers::COMMAND;
        let sc = |m: Modifiers, k: Key| KeyboardShortcut::new(m, k);
        let consume = |s: KeyboardShortcut| ctx.input_mut(|i| i.consume_shortcut(&s));
        if consume(sc(ctrl, Key::K)) {
            self.palette.open();
        }
        if self.palette.is_open() {
            return;
        }
        for (k, p) in [(Key::Num1, Page::Fleet), (Key::Num2, Page::Studio), (Key::Num3, Page::Lighting), (Key::Num4, Page::Stage), (Key::Num5, Page::Firmware), (Key::Num6, Page::Settings)] {
            if consume(sc(ctrl, k)) {
                self.set_page(p);
            }
        }
        if consume(sc(ctrl, Key::I)) {
            self.persist.inspector_open = !self.persist.inspector_open;
        }
        if consume(sc(ctrl, Key::J)) {
            self.persist.event_log_open = !self.persist.event_log_open;
        }
        if consume(sc(ctrl, Key::L)) {
            self.toggle_live();
        }
        if consume(sc(ctrl | Modifiers::SHIFT, Key::B)) {
            self.toggle_blackout();
        }
        if consume(sc(ctrl, Key::Equals)) || consume(sc(ctrl, Key::Plus)) {
            self.set_zoom(ctx, self.persist.zoom + 0.1);
        }
        if consume(sc(ctrl, Key::Minus)) {
            self.set_zoom(ctx, self.persist.zoom - 0.1);
        }
        if consume(sc(ctrl, Key::Num0)) {
            self.set_zoom(ctx, 1.0);
        }
        if consume(sc(Modifiers::NONE, Key::F11)) {
            let on = !self.performance;
            self.set_performance(ctx, on);
        }
        if self.performance && consume(sc(Modifiers::NONE, Key::Escape)) {
            self.set_performance(ctx, false);
        }
        if !typing && ctx.input(|i| i.events.iter().any(|e| matches!(e, egui::Event::Text(t) if t == "?"))) {
            self.cheatsheet = !self.cheatsheet;
        }
        if self.cheatsheet && consume(sc(Modifiers::NONE, Key::Escape)) {
            self.cheatsheet = false;
        }
    }

    // -- shell pieces --------------------------------------------------------

    fn rail(&mut self, ui: &mut Ui) {
        let w = if self.compact { RAIL_W_COMPACT } else { RAIL_W };
        egui::Panel::left("rail")
            .exact_size(w)
            .resizable(false)
            .show_separator_line(false)
            .frame(egui::Frame::new().fill(SURFACE_1))
            .show(ui, |ui| {
                let full = ui.max_rect();
                ui.painter().vline(full.right() - 0.5, full.y_range(), Stroke::new(1.0, BORDER_SUBTLE));
                // Logo
                let (lr, _) = ui.allocate_exact_size(vec2(w, 56.0), Sense::hover());
                widgets::brand_glyph(ui.painter(), lr.center(), 22.0);
                let mut y = lr.bottom() + 4.0;
                let item_h = if self.compact { 44.0 } else { 56.0 };
                for (i, p) in Page::ALL.iter().enumerate() {
                    if *p == Page::Settings {
                        y = full.bottom() - item_h - 10.0;
                    }
                    let r = Rect::from_min_size(pos2(full.left(), y), vec2(w, item_h));
                    y += item_h;
                    let resp = ui.interact(r, ui.scope_id().with(("rail", i)), Sense::click());
                    let sel = self.persist.page == *p;
                    let t = ui.ctx().animate_bool_with_time(resp.id, resp.hovered(), MOTION_FAST);
                    let painter = ui.painter();
                    let pill = Rect::from_center_size(
                        pos2(r.center().x, r.top() + if self.compact { item_h / 2.0 } else { 20.0 }),
                        vec2(40.0, 30.0),
                    );
                    if sel {
                        painter.rect_filled(pill, CornerRadius::same(R_SM), ACCENT_SOFT);
                        painter.rect_filled(
                            Rect::from_center_size(pos2(full.left() + 1.5, pill.center().y), vec2(3.0, 16.0)),
                            CornerRadius::same(2),
                            ACCENT,
                        );
                    } else if t > 0.0 {
                        painter.rect_filled(pill, CornerRadius::same(R_SM), theme::alpha(HOVER, t));
                    }
                    let ic = if sel { ACCENT_TEXT } else { theme::mix(TEXT_MUTED, TEXT, t) };
                    if sel {
                        widgets::paint_icon_fill(painter, pill.center(), p.icon(), 20.0, ic);
                    } else {
                        widgets::paint_icon(painter, pill.center(), p.icon(), 20.0, ic);
                    }
                    if !self.compact {
                        painter.text(
                            pos2(r.center().x, pill.bottom() + 9.0),
                            Align2::CENTER_CENTER,
                            p.name(),
                            theme::font("small_medium"),
                            if sel { TEXT } else { TEXT_FAINT },
                        );
                    }
                    let resp = resp.on_hover_ui(|ui| widgets::tooltip_with_keys(ui, p.name(), Some(&format!("Ctrl+{}", i + 1))));
                    if resp.clicked() {
                        self.set_page(*p);
                    }
                }
            });
    }

    fn status_bar(&mut self, ui: &mut Ui) {
        egui::Panel::bottom("status")
            .exact_size(STATUS_BAR_H)
            .resizable(false)
            .show_separator_line(false)
            .frame(egui::Frame::new().fill(SURFACE_1).inner_margin(egui::Margin::symmetric(12, 0)))
            .show(ui, |ui| {
                let full = ui.max_rect().expand2(vec2(12.0, 0.0));
                ui.painter().hline(full.x_range(), full.top() + 0.5, Stroke::new(1.0, BORDER_SUBTLE));
                ui.horizontal_centered(|ui| {
                    ui.spacing_mut().item_spacing.x = 0.0;
                    let seg_gap = |ui: &mut Ui| {
                        ui.add_space(12.0);
                        let (r, _) = ui.allocate_exact_size(vec2(1.0, 14.0), Sense::hover());
                        ui.painter().vline(r.center().x, r.y_range(), Stroke::new(1.0, BORDER));
                        ui.add_space(12.0);
                    };
                    // Bridge segment
                    let (dot, label) = self.bridge_summary();
                    let resp = ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = 6.0;
                        let (r, _) = ui.allocate_exact_size(vec2(8.0, 8.0), Sense::hover());
                        ui.painter().circle_filled(r.center(), 3.5, dot);
                        widgets::text(ui, label, "small", TEXT_MUTED);
                    }).response;
                    let resp = ui.interact(resp.rect, ui.scope_id().with("bridge-seg"), Sense::click());
                    if resp.hovered() {
                        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
                    }
                    egui::Popup::from_toggle_button_response(&resp)
                        .frame(theme::popover())
                        .width(300.0)
                        .show(|ui| self.bridge_popover(ui));
                    seg_gap(ui);
                    let online = self.snap.online_count();
                    let total = self.snap.gifts.len();
                    widgets::text(ui, format!("{online}/{total} online"), "small", TEXT_MUTED);
                    seg_gap(ui);
                    // LIVE / Preview
                    let lighting = self.lighting.armed && !self.blackout;
                    let live_label = if self.blackout {
                        "Blackout".to_string()
                    } else if lighting {
                        self.lighting.status_label()
                    } else if self.live {
                        format!("LIVE · {online} gifts")
                    } else {
                        "Preview".into()
                    };
                    let live_resp = ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = 6.0;
                        let (r, _) = ui.allocate_exact_size(vec2(8.0, 8.0), Sense::hover());
                        if lighting || (self.live && !self.blackout) {
                            let a = breathe(ui);
                            ui.painter().circle_filled(r.center(), 3.5, theme::alpha(ACCENT, a));
                        } else if self.blackout {
                            ui.painter().circle_filled(r.center(), 3.5, WARN);
                        } else {
                            ui.painter().circle_stroke(r.center(), 3.0, Stroke::new(1.0, TEXT_FAINT));
                        }
                        widgets::text(ui, live_label, "small", if self.live || lighting { ACCENT_TEXT } else { TEXT_MUTED });
                    }).response;
                    let tip = if lighting { "The Lighting page drives the gifts: open it" } else { "Toggle LIVE" };
                    let lr = ui.interact(live_resp.rect, ui.scope_id().with("live-seg"), Sense::click())
                        .on_hover_ui(|ui| widgets::tooltip_with_keys(ui, tip, Some(if lighting { "Ctrl+3" } else { "Ctrl+L" })));
                    if lr.clicked() {
                        if lighting {
                            self.set_page(Page::Lighting);
                        } else {
                            self.toggle_live();
                        }
                    }
                    if !self.compact {
                        seg_gap(ui);
                        widgets::text(ui, format!("{}  {}", icons::SPEAKER, self.studio.output_name()), "small", TEXT_MUTED);
                    }
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        ui.spacing_mut().item_spacing.x = 0.0;
                        if !self.compact {
                            widgets::text(ui, format!("{:.0} fps", self.fps.min(999.0)), "value_small", TEXT_FAINT);
                        }
                        if self.is_sim() {
                            ui.add_space(12.0);
                            widgets::chip(ui, "SIM", TEXT_MUTED, SURFACE_3)
                                .on_hover_text("Simulator: these gifts are virtual");
                        }
                        if !self.ops.is_empty() {
                            ui.add_space(12.0);
                            widgets::text(ui, self.ops.last().map(|o| o.label.clone()).unwrap_or_default(), "small", TEXT_FAINT);
                            ui.add_space(6.0);
                            widgets::spinner(ui, 14.0, TEXT_FAINT);
                        }
                    });
                });
            });
    }

    fn bridge_summary(&self) -> (Color32, String) {
        if self.backend.is_none() {
            return (TEXT_FAINT, "No bridge".into());
        }
        let b = &self.snap.bridge;
        match &b.state {
            BridgeState::Connected => {
                // Poll loss over online gifts (sweeps of empty addresses are expected to fail).
                let online: Vec<f32> = self.snap.gifts.iter().filter(|g| g.online).map(|g| g.poll_success_pct).collect();
                let fail = if online.is_empty() { 0.0 } else { 100.0 - online.iter().sum::<f32>() / online.len() as f32 };
                let pps = self.snap.link.show_packets_per_s + self.snap.link.polls_per_s;
                let dot = if fail > 5.0 { WARN } else { TEXT_MUTED };
                let name = if self.is_sim() { "Simulator" } else { "Bridge" };
                (dot, format!("{name} · ch {} · {:.0} pkt/s · {:.1}% loss", b.radio.channel, pps, fail))
            }
            BridgeState::Connecting => (TEXT_FAINT, "Bridge · connecting…".into()),
            BridgeState::Disconnected(_) => (ERROR, "Bridge disconnected: reconnecting…".into()),
        }
    }

    fn bridge_popover(&mut self, ui: &mut Ui) {
        widgets::overline(ui, "Bridge");
        ui.add_space(4.0);
        let b = self.snap.bridge.clone();
        let l = self.snap.link.clone();
        let row = |ui: &mut Ui, label: &str, value: String, style: &str| widgets::kv_text(ui, label, 110.0, value, style);
        row(ui, "Device", if b.description.is_empty() { "—".into() } else { b.description.clone() }, "body");
        let state = match &b.state {
            BridgeState::Connected => "Connected".to_string(),
            BridgeState::Connecting => "Connecting".into(),
            BridgeState::Disconnected(r) => format!("Disconnected: {r}"),
        };
        row(ui, "State", state, "body");
        if let Some(info) = &b.info {
            row(ui, "Firmware", format!("v{}", info.fw_version), "value");
        }
        row(ui, "Channel", format!("{} ({} MHz)", b.radio.channel, 2400 + b.radio.channel as u32), "value");
        row(ui, "Show frames", format!("{:.0}/s", l.frames_per_s), "value");
        row(ui, "Polls", format!("{:.0}/s", l.polls_per_s), "value");
        row(ui, "TX failed", format!("{} of {}", l.tx_failed, l.tx_total), "value");
        row(ui, "Queue", format!("{} / {}", l.in_flight, l.max_in_flight.max(4)), "value");
        if let Some(rtt) = b.ping_rtt_us {
            row(ui, "Ping RTT", format!("{:.2} ms", rtt as f32 / 1000.0), "value");
        }
        ui.add_space(8.0);
        if !self.is_sim() && widgets::secondary_button(ui, Some(icons::REBOOT), "Reconnect", true).clicked() {
            self.start_usb();
        }
    }

    fn inspector(&mut self, ui: &mut Ui) {
        let page = self.persist.page;
        let has_inspector = matches!(page, Page::Fleet | Page::Stage | Page::Firmware) && self.backend.is_some();
        if !has_inspector {
            return;
        }
        let mut open = self.persist.inspector_open;
        egui::Panel::right("inspector")
            .resizable(true)
            .size_range(300.0..=480.0)
            .default_size(INSPECTOR_W)
            .show_separator_line(false)
            .frame(egui::Frame::new().fill(SURFACE_1))
            .show_collapsible(ui, &mut open, |ui| {
                let full = ui.max_rect();
                ui.painter().vline(full.left() + 0.5, full.y_range(), Stroke::new(1.0, BORDER_SUBTLE));
                egui::ScrollArea::vertical().auto_shrink(false).show(ui, |ui| {
                    egui::Frame::new().inner_margin(egui::Margin::same(16)).show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        match page {
                            Page::Firmware => crate::pages::firmware::inspector(self, ui),
                            _ => crate::pages::fleet::inspector(self, ui),
                        }
                    });
                });
            });
        self.persist.inspector_open = open;
    }

    /// The 48 px page header: title + right-aligned controls.
    pub fn page_header(ui: &mut Ui, title: &str, add: impl FnOnce(&mut Ui)) {
        let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), PAGE_HEADER_H), Sense::hover());
        ui.painter().hline(rect.x_range(), rect.bottom() - 0.5, Stroke::new(1.0, BORDER_SUBTLE));
        let inner = rect.shrink2(vec2(S7, 0.0));
        ui.scope_builder(egui::UiBuilder::new().max_rect(inner).layout(Layout::left_to_right(Align::Center)), |ui| {
            widgets::text(ui, title, "heading", TEXT);
            ui.add_space(S4);
            add(ui);
        });
    }

    fn central(&mut self, ui: &mut Ui) {
        let page = self.persist.page;
        if page != self.last_page {
            self.page_changed_at = Instant::now();
            self.last_page = page;
        }
        let fade = (self.page_changed_at.elapsed().as_secs_f32() / 0.12).min(1.0);
        egui::CentralPanel::default()
            .frame(egui::Frame::new().fill(BG))
            .show(ui, |ui| {
                if fade < 1.0 && !self.persist.reduce_motion {
                    ui.set_opacity(egui::emath::easing::cubic_out(fade));
                    ui.ctx().request_repaint();
                }
                if self.backend.is_none() && !matches!(page, Page::Settings | Page::Studio | Page::Lighting) {
                    crate::pages::startup::no_bridge(self, ui);
                    return;
                }
                match page {
                    Page::Fleet => crate::pages::fleet::page(self, ui),
                    Page::Studio => crate::pages::studio::page(self, ui),
                    Page::Lighting => crate::pages::lighting::page(self, ui),
                    Page::Stage => crate::pages::stage_page::page(self, ui),
                    Page::Firmware => crate::pages::firmware::page(self, ui),
                    Page::Settings => crate::pages::settings::page(self, ui),
                }
            });
    }

    fn modals(&mut self, ctx: &egui::Context) {
        let Some(c) = self.confirm.clone() else { return };
        let mut close = false;
        let modal = egui::Modal::new(egui::Id::unique("confirm"))
            .backdrop_color(SCRIM)
            .frame(modal_frame());
        let r = modal.show(ctx, |ui| {
            ui.set_width(420.0);
            let (title, body, warning, icon, action) = match &c {
                Confirm::RebootAll => (
                    format!("Reboot all {} gifts?", self.snap.online_count()),
                    "Every online gift restarts. The lights go dark for about a second.",
                    self.live.then_some("You are LIVE: the show will glitch."),
                    icons::REBOOT,
                    "Reboot all",
                ),
                Confirm::BootloaderGift(uid) => (
                    format!("Reboot {} into the bootloader?", self.gift_name(uid)),
                    "It stays in recovery mode (lights off) until you run a firmware update.",
                    None,
                    icons::CPU,
                    "Reboot to bootloader",
                ),
            };
            match confirm_body(ui, &title, body, warning, icon, action) {
                Some(true) => {
                    match &c {
                        Confirm::RebootAll => {
                            let all: Vec<Uid> = self.snap.gifts.iter().filter(|g| g.online).map(|g| g.uid).collect();
                            self.reboot(all, false);
                        }
                        Confirm::BootloaderGift(uid) => self.reboot(vec![*uid], true),
                    }
                    close = true;
                }
                Some(false) => close = true,
                None => {}
            }
        });
        if r.should_close() || close {
            self.confirm = None;
        }
    }

    fn cheatsheet_overlay(&mut self, ctx: &egui::Context) {
        if !self.cheatsheet {
            return;
        }
        let r = egui::Modal::new(egui::Id::unique("cheatsheet"))
            .backdrop_color(theme::alpha(Color32::BLACK, 0.5))
            .frame(modal_frame())
            .show(ctx, |ui| {
                ui.set_width(760.0);
                ui.horizontal(|ui| {
                    widgets::text(ui, "Keyboard shortcuts", "title", TEXT);
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        widgets::keycap(ui, "Esc");
                    });
                });
                ui.add_space(12.0);
                egui::ScrollArea::vertical().max_height(560.0).show(ui, |ui| {
                    crate::pages::settings::shortcut_columns(ui, "");
                });
            });
        if r.should_close() {
            self.cheatsheet = false;
        }
    }

    fn performance_mode(&mut self, ui: &mut Ui) {
        let ctx = ui.ctx().clone();
        let moved = ctx.input(|i| i.pointer.delta() != egui::Vec2::ZERO || i.pointer.any_down());
        if moved {
            self.perf_last_move = Instant::now();
        }
        let idle = self.perf_last_move.elapsed().as_secs_f32();
        let hud_t = ctx.animate_bool_with_time(egui::Id::unique("hud"), idle < 2.5, MOTION_SLOW);
        if idle > 2.5 {
            ctx.set_cursor_icon(egui::CursorIcon::None);
        }
        if ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Space)) {
            self.studio.toggle_play();
        }
        egui::CentralPanel::default().frame(egui::Frame::new().fill(STAGE_BG)).show(ui, |ui| {
            let gifts = crate::pages::stage_page::stage_gifts(self);
            let opts = crate::stage::StageOptions {
                labels: false,
                performance: true,
                bloom: 1.25,
                show_floor_grid: false,
                ..Default::default()
            };
            let rect = ui.max_rect();
            self.stage.show(ui, &gifts, &opts);
            if hud_t > 0.0 {
                let p = ui.painter();
                let plate = |p: &egui::Painter, r: Rect| p.rect_filled(r, CornerRadius::same(R_LG), Color32::from_black_alpha((115.0 * hud_t) as u8));
                // Bottom-left HUD
                let hr = Rect::from_min_size(pos2(rect.left() + S9, rect.bottom() - S9 - 76.0), vec2(380.0, 76.0));
                plate(p, hr);
                let title = self.studio.title();
                p.text(hr.left_top() + vec2(16.0, 20.0), Align2::LEFT_CENTER, if title.is_empty() { "No show loaded".into() } else { title }, theme::font("title"), theme::alpha(TEXT, hud_t));
                let (pos, dur) = self.studio.position_duration();
                let line = format!("{} / {}", fmt_mmss(pos), fmt_mmss(dur));
                p.text(hr.left_top() + vec2(16.0, 44.0), Align2::LEFT_CENTER, line, theme::font("value"), theme::alpha(TEXT_SECONDARY, hud_t));
                if let Some(next) = self.studio.next_section_in(pos) {
                    p.text(hr.left_top() + vec2(130.0, 44.0), Align2::LEFT_CENTER, format!("Next section in {}", fmt_mmss(next)), theme::font("small"), theme::alpha(TEXT_MUTED, hud_t));
                }
                let online = self.snap.online_count();
                p.text(hr.left_top() + vec2(16.0, 62.0), Align2::LEFT_CENTER, format!("{online}/{} gifts{}", self.snap.gifts.len(), if self.live { " · LIVE" } else { " · Preview" }), theme::font("small"), theme::alpha(if self.live { ACCENT_TEXT } else { TEXT_MUTED }, hud_t));
                // Top-right exit
                let er = Rect::from_min_size(pos2(rect.right() - S9 - 96.0, rect.top() + S9), vec2(96.0, 32.0));
                plate(p, er);
                p.text(er.center() + vec2(14.0, 0.0), Align2::CENTER_CENTER, "Exit", theme::font("button"), theme::alpha(TEXT_SECONDARY, hud_t));
                let kr = Rect::from_center_size(er.left_center() + vec2(26.0, 0.0), vec2(30.0, 18.0));
                p.rect_filled(kr, CornerRadius::same(R_XS), theme::alpha(SURFACE_3, hud_t));
                p.text(kr.center(), Align2::CENTER_CENTER, "Esc", theme::font("small"), theme::alpha(TEXT_MUTED, hud_t));
                if ui.interact(er, egui::Id::unique("perf-exit"), Sense::click()).clicked() {
                    self.set_performance(&ctx, false);
                }
            }
        });
        if self.studio.is_playing() || hud_t > 0.0 && hud_t < 1.0 {
            ctx.request_repaint();
        } else {
            ctx.request_repaint_after(Duration::from_millis(100));
        }
    }

    fn sim_banner(&mut self, ui: &mut Ui) {
        if !self.is_sim() || self.persist.sim_banner_dismissed || self.performance {
            return;
        }
        egui::Panel::top("sim-banner")
            .exact_size(32.0)
            .show_separator_line(false)
            .frame(egui::Frame::new().fill(INFO_SOFT).inner_margin(egui::Margin::symmetric(16, 0)))
            .show(ui, |ui| {
                ui.horizontal_centered(|ui| {
                    widgets::text(ui, icons::INFO, "body", INFO);
                    widgets::text(ui, "Simulator: these gifts are virtual. Plug in a bridge to switch to real hardware.", "small", TEXT_SECONDARY);
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if widgets::ghost_button_sized(ui, icons::X, "Dismiss", None, 24.0, false).clicked() {
                            self.persist.sim_banner_dismissed = true;
                        }
                    });
                });
            });
    }

    fn bridge_lost_banner(&mut self, ui: &mut Ui) {
        if self.backend.is_none() || self.performance || !self.connected_once {
            return;
        }
        if !matches!(self.snap.bridge.state, BridgeState::Disconnected(_)) {
            return;
        }
        egui::Panel::top("bridge-lost")
            .exact_size(36.0)
            .show_separator_line(false)
            .frame(egui::Frame::new().fill(ERROR_SOFT).inner_margin(egui::Margin::symmetric(16, 0)))
            .show(ui, |ui| {
                ui.horizontal_centered(|ui| {
                    widgets::text(ui, icons::PLUGS, "body", ERROR);
                    widgets::text(ui, "Bridge disconnected: reconnecting… Gifts keep their last state and turn off after the idle timeout.", "small", TEXT);
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if widgets::secondary_button_h(ui, None, "Retry now", true, 24.0).clicked() {
                            self.start_usb();
                        }
                    });
                });
            });
    }

    fn handle_palette(&mut self, ctx: &egui::Context) {
        let cmds = crate::palette::build_commands(self);
        if let Some(action) = self.palette.show(ctx, &cmds) {
            self.run_action(ctx, action);
        }
    }

    pub fn run_action(&mut self, ctx: &egui::Context, a: PaletteAction) {
        use PaletteAction as A;
        match a {
            A::Nav(p) => self.set_page(p),
            A::ToggleInspector => self.persist.inspector_open = !self.persist.inspector_open,
            A::ToggleEventLog => self.persist.event_log_open = !self.persist.event_log_open,
            A::ToggleLive => self.toggle_live(),
            A::Blackout => self.toggle_blackout(),
            A::Perform => self.set_performance(ctx, true),
            A::Cheatsheet => self.cheatsheet = true,
            A::Blink(u) => self.blink(vec![u]),
            A::BlinkSelected => {
                let s: Vec<Uid> = self.selection.iter().copied().collect();
                self.blink(s)
            }
            A::Select(u) => {
                self.select(u);
                self.set_page(Page::Fleet);
            }
            A::Rename(u) => {
                self.select(u);
                self.persist.inspector_open = true;
                self.renaming = Some((u, self.gift_name(&u)));
                if self.persist.page != Page::Stage {
                    self.set_page(Page::Fleet);
                }
            }
            A::AssignSlot(u) => {
                self.select(u);
                self.persist.inspector_open = true;
                self.slot_popup = Some(u);
                self.set_page(Page::Fleet);
            }
            A::Reboot(u) => self.reboot(vec![u], false),
            A::RebootAll => self.confirm = Some(Confirm::RebootAll),
            A::Rescan => self.rescan(),
            A::AutoAssign => self.auto_assign(),
            A::StartSim => self.start_sim(self.persist.sim_count),
            A::OpenAudio => self.studio.pick_audio(),
            A::OpenShow => self.studio.pick_show(&mut self.toasts),
            A::SaveShow => self.studio.save(false, &mut self.toasts, &mut self.persist),
            A::SaveShowAs => self.studio.save(true, &mut self.toasts, &mut self.persist),
            A::AutoGenerate => {
                self.set_page(Page::Studio);
                self.studio.auto_generate_now(&mut self.toasts);
            }
            A::PlayPause => self.studio.toggle_play(),
            A::GoToStart => self.studio.seek(0.0),
            A::Seek(t) => {
                self.set_page(Page::Studio);
                self.studio.seek(t)
            }
            A::SeekBar(b) => {
                self.set_page(Page::Studio);
                self.studio.seek_bar(b)
            }
            A::InsertCue => self.studio.insert_cue_at_playhead(),
            A::ChooseFirmware => {
                self.set_page(Page::Firmware);
                self.firmware.pick();
            }
            A::ZoomIn => self.set_zoom(ctx, self.persist.zoom + 0.1),
            A::ZoomOut => self.set_zoom(ctx, self.persist.zoom - 0.1),
            A::ZoomReset => self.set_zoom(ctx, 1.0),
            A::FrameAll => {
                let g = crate::pages::stage_page::stage_gifts(self);
                self.stage.frame_all(&g);
                self.set_page(Page::Stage);
            }
            A::Arrange(arr) => {
                crate::pages::stage_page::apply_arrangement(self, arr);
                self.set_page(Page::Stage);
            }
            A::ReduceMotion => self.persist.reduce_motion = !self.persist.reduce_motion,
            A::SettingsSection(i) => {
                self.settings.section = i;
                self.set_page(Page::Settings);
            }
            A::LightingEffect(e) => {
                crate::pages::lighting::apply_effect(self, e);
                let n = lumigift_fx::EFFECTS[e as usize].name;
                if self.persist.page != Page::Lighting {
                    self.toasts.info(format!("Lighting: {n}"));
                }
            }
            A::LightingPreset(i) => crate::pages::lighting::apply_preset(self, i),
            A::LightingFadeOut => crate::pages::lighting::toggle_fade_out(self),
            A::LightingRelease => crate::pages::lighting::release(self),
            A::SearchLog(q) => {
                self.fleet.log_query = q;
                self.persist.event_log_open = true;
                self.set_page(Page::Fleet);
            }
        }
    }
}

/// Title, body, optional warning and a Cancel / danger button row.
/// Returns `Some(true)` on confirm, `Some(false)` on cancel.
fn confirm_body(ui: &mut Ui, title: &str, body: &str, warning: Option<&str>, icon: &str, action: &str) -> Option<bool> {
    let mut out = None;
    widgets::text(ui, title, "title", TEXT);
    ui.add_space(8.0);
    widgets::text(ui, body, "body", TEXT_MUTED);
    if let Some(w) = warning {
        ui.add_space(6.0);
        widgets::text(ui, format!("{}  {w}", icons::WARNING), "body", WARN);
    }
    ui.add_space(20.0);
    ui.horizontal(|ui| {
        if widgets::secondary_button(ui, None, "Cancel", true).clicked() {
            out = Some(false);
        }
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            if widgets::danger_button(ui, Some(icon), action, true).clicked() {
                out = Some(true);
            }
        });
    });
    out
}

pub fn modal_frame() -> egui::Frame {
    egui::Frame::new()
        .fill(SURFACE_3)
        .stroke(Stroke::new(1.0, BORDER))
        .corner_radius(CornerRadius::same(R_LG))
        .inner_margin(egui::Margin::same(20))
        .shadow(theme::shadow_modal())
}

pub fn default_name(uid: &Uid) -> String {
    let h = uid.to_hex().to_uppercase();
    format!("Gift {}", &h[h.len() - 4..])
}

pub fn breathe(ui: &Ui) -> f32 {
    let t = ui.input(|i| i.time) as f32;
    ui.ctx().request_repaint_after(Duration::from_millis(33));
    0.6 + 0.4 * (0.5 + 0.5 * (t * std::f32::consts::TAU / 1.6).sin())
}

pub fn fmt_mmss(t: f64) -> String {
    let t = t.max(0.0);
    format!("{}:{:02}", (t / 60.0) as u32, (t % 60.0) as u32)
}

pub fn fmt_timecode(t: f64) -> String {
    let t = t.max(0.0);
    let ms = (t * 1000.0).round() as u64;
    format!("{:02}:{:02}.{:03}", ms / 60000, (ms / 1000) % 60, ms % 1000)
}

pub fn level_icon(l: LogLevel) -> (&'static str, Color32) {
    match l {
        LogLevel::Info => (icons::INFO, TEXT_MUTED),
        LogLevel::Warn => (icons::WARNING, WARN),
        LogLevel::Error => (icons::WARNING_CIRCLE, ERROR),
    }
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        let screen = ctx.content_rect();
        self.compact = screen.width() < 1280.0 || screen.height() < 760.0;
        self.update_state(&ctx);
        self.global_shortcuts(&ctx);
        crate::pages::studio::handle_dropped_files(self, &ctx);

        if self.performance {
            self.performance_mode(ui);
        } else {
            self.rail(ui);
            self.status_bar(ui);
            self.sim_banner(ui);
            self.bridge_lost_banner(ui);
            self.inspector(ui);
            self.central(ui);
        }
        self.modals(&ctx);
        self.cheatsheet_overlay(&ctx);
        self.handle_palette(&ctx);
        if let Some(id) = self.toasts.show(&ctx, if self.performance { 0.0 } else { STATUS_BAR_H })
            && Some(id) == self.switch_hw_toast {
                self.start_usb();
            }
        self.studio.post_frame(&mut self.persist);
        self.firmware.post_frame(&mut self.toasts);

        // Repaint policy (§6.2).
        let busy = self.studio.is_playing()
            || self.lights.animating
            || self.live
            || self.studio.is_loading()
            || self.snap.ota.as_ref().is_some_and(|o| o.finished.is_none());
        if busy {
            ctx.request_repaint();
        } else {
            ctx.request_repaint_after(Duration::from_millis(250));
        }

        if let Some(mut s) = self.screenshot.take() {
            s.step(self, &ctx);
            self.screenshot = Some(s);
        }
    }

    fn save(&mut self, storage: &mut dyn eframe::Storage) {
        if self.screenshot.is_some() {
            return;
        }
        self.persist.camera = Some(self.stage.camera());
        eframe::set_value(storage, "lumigift-studio", &self.persist);
    }

    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        STAGE_BG.to_normalized_gamma_f32()
    }

    fn on_exit(&mut self) {
        if let Some(b) = &self.backend {
            b.link.set_show(lumigift_core::ShowOutput::Idle);
            b.link.shutdown();
        }
    }
}
