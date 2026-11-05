//! Lighting page: a library of generic LED looks (fades, twinkles, Knight
//! Rider, fire, …) to toggle on all gifts, a selection or named groups,
//! without a show.
//!
//! The looks are `lumigift-fx` effects rendered **on the gifts** from the
//! bridge-synced clock and their slot, so all gifts animate in lockstep and
//! the host only broadcasts parameters: one [`MusicFrame`] per layer every
//! 10 ms ([`ShowOutput::Layers`]). Each layer has a slot mask; later layers
//! win where masks overlap, and gifts outside every mask keep their look.
//!
//! **Output ownership.** Applying a look *arms* the Lighting page: it takes
//! over the show stream (the Studio's LIVE switches off) until it is
//! released (header button, palette, or switching LIVE back on), or until a
//! show starts playing in the Studio, which takes the gifts back (and goes
//! LIVE). The global blackout (`Ctrl+Shift+B`) overrides both. The status
//! bar shows the owner: "Lighting · Fade", "LIVE · 10 gifts" or "Preview".

use std::collections::BTreeSet;
use std::time::{Duration, Instant};

use egui::{
    Align, Align2, Color32, CornerRadius, Key, Layout, Modifiers, Rect, Sense, Stroke, StrokeKind,
    Ui, pos2, vec2,
};
use lumigift_core::{ShowOutput, Uid};
use lumigift_fx::{EFFECTS, Music, PALETTE_NAMES, PALETTES, PARAM_DEFAULTS, effect, render_look};
use lumigift_proto::{
    LEDS_PER_GIFT, MASK_ALL, MusicFrame, PALETTE_CUSTOM, Rgb, TRANSITION_UNIT_MS,
};
use serde::{Deserialize, Serialize};

use crate::app::App;
use crate::icons;
use crate::stage::{StageGift, StageOptions};
use crate::theme::{self, *};
use crate::toasts::ToastKind;
use crate::widgets;

/// The effects offered here (all time-based: no show needed), in display order.
pub const LIBRARY: [u8; 15] = [
    effect::SOLID,
    effect::BREATHE,
    effect::FADE,
    effect::RAINBOW,
    effect::TWINKLE,
    effect::WATERFALL,
    effect::SCANNER,
    effect::COMET,
    effect::THEATER,
    effect::WIPE,
    effect::FIRE,
    effect::BLINK,
    effect::POLICE,
    effect::PULSE_TIMED,
    effect::STROBE_TIMED,
];

/// Short descriptions for the card tooltips, indexed by effect id.
fn blurb(e: u8) -> &'static str {
    match e {
        effect::SOLID => {
            "One constant colour (the palette's first colour, or your primary colour)."
        }
        effect::BREATHE => "Fades in and out. Depth 255 goes all the way to black.",
        effect::FADE => "Smooth colour cycling through the palette. Spread offsets the gifts.",
        effect::RAINBOW => "A rainbow rolling across the gifts.",
        effect::TWINKLE => {
            "Random sparkles over a dim glow, identical in preview and on the gifts."
        }
        effect::WATERFALL => "A colour wave flowing across the row.",
        effect::SCANNER => "A head with a fading tail bouncing across all gifts in slot order.",
        effect::COMET => "A single comet lapping around all gifts.",
        effect::THEATER => "Marquee lights: every few gifts lit, stepping along.",
        effect::WIPE => "Fills the row gift by gift with the next colour.",
        effect::FIRE => "Flickering fire or candle light.",
        effect::BLINK => "On and off. Alternate swaps odd and even gifts.",
        effect::POLICE => "Two colours alternating with double flashes (or smoothly).",
        effect::PULSE_TIMED => "A decaying pulse at a fixed rate.",
        effect::STROBE_TIMED => "A strobe at a fixed rate.",
        _ => "",
    }
}

/// Speed cap for flashing effects when "Limit flashing" is on (≤ ~3 Hz).
const FLASH_SPEED_CAP: u8 = 150;

/// One look: what a layer shows.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Look {
    pub effect: u8,
    /// Index into `PALETTES`, or `PALETTE_CUSTOM`.
    pub palette: u8,
    pub colors: [[u8; 3]; 2],
    pub speed: u8,
    pub size: u8,
    pub hue: u8,
    pub brightness: u8,
}

impl Default for Look {
    fn default() -> Self {
        Look {
            effect: effect::FADE,
            palette: 4,
            colors: [[255, 168, 72], [187, 120, 254]],
            speed: 0,
            size: 0,
            hue: 0,
            brightness: 230,
        }
        .with_effect(effect::FADE)
    }
}

impl Look {
    /// Switch effect, resetting speed/size to that effect's defaults.
    pub fn with_effect(mut self, e: u8) -> Self {
        self.effect = e;
        let d = PARAM_DEFAULTS[e as usize];
        self.speed = d[0].max(1);
        self.size = d[1].max(1);
        self
    }

    fn custom(&self) -> bool {
        self.palette == PALETTE_CUSTOM
    }

    pub fn rgb(&self, i: usize) -> Rgb {
        Rgb::new(self.colors[i][0], self.colors[i][1], self.colors[i][2])
    }

    /// The four palette stops the gifts sample (the custom gradient too).
    pub fn stops(&self) -> [Rgb; 4] {
        if self.custom() {
            let mid = lumigift_fx::blend(self.rgb(0), self.rgb(1), 128);
            [self.rgb(0), mid, self.rgb(1), mid]
        } else {
            PALETTES[self.palette as usize % PALETTES.len()]
        }
    }

    pub fn palette_name(&self) -> &'static str {
        if self.custom() {
            "Custom"
        } else {
            PALETTE_NAMES[self.palette as usize % PALETTE_NAMES.len()]
        }
    }

    pub fn label(&self) -> String {
        format!(
            "{} · {}",
            EFFECTS[self.effect as usize].name,
            self.palette_name()
        )
    }

    /// Apply the photosensitivity limits (speed cap, Police smooth mode).
    fn limited(mut self, limit_flashing: bool) -> Self {
        if limit_flashing && EFFECTS[self.effect as usize].flashing {
            self.speed = self.speed.min(FLASH_SPEED_CAP);
            if self.effect == effect::POLICE {
                self.size = self.size.max(192);
            }
        }
        self
    }

    pub fn frame(&self, slot_count: u8, mask: u32, brightness: u8, transition: u8) -> MusicFrame {
        MusicFrame {
            effect: self.effect,
            brightness,
            palette: self.palette,
            hue: self.hue,
            param: [self.speed.max(1), self.size.max(1)],
            slot_count,
            colors: [self.rgb(0), self.rgb(1)],
            mask,
            transition,
            ..Default::default()
        }
    }
}

/// Which gifts the controls apply to.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub enum TargetSel {
    #[default]
    All,
    Selected,
    Group(String),
}

/// A named set of gifts (UIDs in hex, so it survives slot changes).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Group {
    pub name: String,
    pub uids: Vec<String>,
}

impl Group {
    fn uids(&self) -> BTreeSet<Uid> {
        self.uids.iter().filter_map(|h| h.parse().ok()).collect()
    }
}

/// A saved favourite look.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Preset {
    pub name: String,
    pub look: Look,
}

/// Persisted with the app settings.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct LightingPersist {
    /// The look being edited.
    pub look: Look,
    /// Scales every layer's brightness.
    pub master: u8,
    pub transition_ms: u32,
    pub groups: Vec<Group>,
    pub presets: Vec<Preset>,
}

impl Default for LightingPersist {
    fn default() -> Self {
        let look = |e: u8, palette: u8, colors: [[u8; 3]; 2]| Look {
            palette,
            colors,
            ..Look::default().with_effect(e)
        };
        Self {
            look: Look::default(),
            master: 255,
            transition_ms: 600,
            groups: Vec::new(),
            presets: vec![
                Preset {
                    name: "Warm candles".into(),
                    look: look(
                        effect::FIRE,
                        PALETTE_CUSTOM,
                        [[255, 120, 20], [255, 200, 90]],
                    ),
                },
                Preset {
                    name: "Christmas twinkle".into(),
                    look: look(effect::TWINKLE, 0, [[255, 168, 72], [187, 120, 254]]),
                },
                Preset {
                    name: "Ocean waves".into(),
                    look: look(effect::WATERFALL, 2, [[255, 168, 72], [187, 120, 254]]),
                },
                Preset {
                    name: "KITT".into(),
                    look: look(effect::SCANNER, PALETTE_CUSTOM, [[255, 20, 10], [80, 0, 0]]),
                },
            ],
        }
    }
}

#[derive(Clone, PartialEq, Debug)]
enum LayerTarget {
    All,
    Gifts(BTreeSet<Uid>),
    Group(String),
}

#[derive(Clone, Debug)]
struct Layer {
    target: LayerTarget,
    look: Look,
}

impl Layer {
    /// An "Off" layer: its gifts are dark (see [`apply_off`]).
    fn is_off(&self) -> bool {
        self.look.brightness == 0
    }
}

/// Runtime state of the page (not persisted).
pub struct Lighting {
    /// The Lighting page owns the show stream.
    pub armed: bool,
    layers: Vec<Layer>,
    pub target: TargetSel,
    pub fade_out: bool,
    pub blackout: bool,
    /// Send transition 0 until then (cuts for blackout on/off).
    cut_until: Option<Instant>,
    was_playing: bool,
    was_live: bool,
    /// The frames sent this UI frame (for the virtual preview).
    frames: Vec<MusicFrame>,
    epoch: Instant,
    renaming_group: Option<(usize, String)>,
    renaming_preset: Option<(usize, String)>,
    framed_key: usize,
}

impl Default for Lighting {
    fn default() -> Self {
        Self {
            armed: false,
            layers: Vec::new(),
            target: TargetSel::All,
            fade_out: false,
            blackout: false,
            cut_until: None,
            was_playing: false,
            was_live: false,
            frames: Vec::new(),
            epoch: Instant::now(),
            renaming_group: None,
            renaming_preset: None,
            framed_key: usize::MAX,
        }
    }
}

impl Lighting {
    /// "Lighting · Fade" for the status bar.
    pub fn status_label(&self) -> String {
        if self.blackout {
            "Lighting · Blackout".into()
        } else if self.fade_out {
            "Lighting · Faded out".into()
        } else {
            match self.layers.as_slice() {
                [] => "Lighting".into(),
                v if v.iter().all(Layer::is_off) => "Lighting · Off".into(),
                [l] => format!("Lighting · {}", EFFECTS[l.look.effect as usize].name),
                v => format!("Lighting · {} layers", v.len()),
            }
        }
    }

    pub fn release(&mut self) {
        self.armed = false;
    }
}

// ---------------------------------------------------------------------------
// Actions (also used by the command palette)
// ---------------------------------------------------------------------------

fn layer_target(app: &App) -> Option<LayerTarget> {
    match &app.lighting.target {
        TargetSel::All => Some(LayerTarget::All),
        TargetSel::Selected if !app.selection.is_empty() => {
            Some(LayerTarget::Gifts(app.selection.clone()))
        }
        TargetSel::Selected => None,
        TargetSel::Group(n) => Some(LayerTarget::Group(n.clone())),
    }
}

fn target_name(app: &App, t: &LayerTarget) -> String {
    match t {
        LayerTarget::All => "All gifts".into(),
        LayerTarget::Gifts(s) if s.len() == 1 => app.gift_name(s.first().unwrap()),
        LayerTarget::Gifts(s) => format!("{} gifts", s.len()),
        LayerTarget::Group(n) => n.clone(),
    }
}

/// Take over the show stream (switching the Studio's LIVE off).
pub fn arm(app: &mut App) {
    if app.lighting.armed {
        return;
    }
    if app.backend.is_none() {
        return;
    }
    app.lighting.was_live = app.live;
    if app.live {
        app.live = false;
        app.live_engaged_at = None;
    }
    app.lighting.armed = true;
}

/// Give the gifts back (to the Studio's LIVE if it was on before).
pub fn release(app: &mut App) {
    if !app.lighting.armed {
        return;
    }
    app.lighting.release();
    if let Some(l) = app.link() {
        l.set_show(ShowOutput::Idle);
    }
    if app.lighting.was_live && app.backend.is_some() {
        app.live = true;
        app.live_engaged_at = Some(Instant::now());
    }
    app.toasts.info("Lighting released the gifts");
}

/// Apply the edited look to the current target and take over the output.
pub fn apply(app: &mut App) {
    let look = app.persist.lighting.look;
    apply_look(app, look);
}

/// Turn the current target's lights off (fading with the crossfade time)
/// until another look is applied to it. The edited look is kept.
pub fn apply_off(app: &mut App) {
    let look = Look {
        brightness: 0,
        ..app.persist.lighting.look
    };
    apply_look(app, look);
}

fn apply_look(app: &mut App, look: Look) {
    let Some(t) = layer_target(app) else {
        app.toasts.push(
            ToastKind::Warn,
            "Select some gifts first",
            Some("Click gifts on the stage (Ctrl+click adds)".into()),
        );
        return;
    };
    let l = &mut app.lighting;
    match &t {
        // Everyone: one layer.
        LayerTarget::All => l.layers.clear(),
        // A selection replaces earlier selections it fully covers.
        LayerTarget::Gifts(s) => l.layers.retain(|x| match &x.target {
            LayerTarget::Gifts(o) => !o.is_subset(s),
            other => other != &t,
        }),
        LayerTarget::Group(_) => l.layers.retain(|x| x.target != t),
    }
    l.layers.push(Layer { target: t, look });
    l.fade_out = false;
    l.blackout = false;
    if !app.lighting.armed {
        arm(app);
    }
}

pub fn apply_effect(app: &mut App, e: u8) {
    app.persist.lighting.look = app.persist.lighting.look.with_effect(e);
    apply(app);
}

pub fn apply_preset(app: &mut App, i: usize) {
    if let Some(p) = app.persist.lighting.presets.get(i) {
        app.persist.lighting.look = p.look;
        apply(app);
    }
}

pub fn toggle_fade_out(app: &mut App) {
    if !app.lighting.armed {
        if app.lighting.layers.is_empty() {
            return;
        }
        arm(app);
    }
    app.lighting.fade_out = !app.lighting.fade_out;
    app.lighting.blackout = false;
}

pub fn toggle_blackout(app: &mut App) {
    if !app.lighting.armed {
        if app.lighting.layers.is_empty() {
            return;
        }
        arm(app);
    }
    app.lighting.blackout = !app.lighting.blackout;
    app.lighting.fade_out = false;
    app.lighting.cut_until = Some(Instant::now() + Duration::from_millis(250));
}

/// The layers as frames, masks resolved against the current slots.
fn build_frames(app: &App) -> Vec<MusicFrame> {
    let l = &app.lighting;
    let p = &app.persist.lighting;
    let slots = app.slot_count();
    let slot_of = |u: &Uid| app.snap.gift(u).and_then(|g| g.slot);
    let cut = l.cut_until.is_some_and(|t| Instant::now() < t);
    let tr_units = (p.transition_ms / TRANSITION_UNIT_MS).min(255) as u8;
    let (bright_scale, transition) = if l.blackout {
        (0, 0)
    } else if l.fade_out {
        (0, tr_units.max(30))
    } else {
        (p.master, if cut { 0 } else { tr_units })
    };
    l.layers
        .iter()
        .map(|layer| {
            let mask = match &layer.target {
                LayerTarget::All => MASK_ALL,
                LayerTarget::Gifts(s) => MusicFrame::mask_of(s.iter().filter_map(slot_of)),
                LayerTarget::Group(n) => {
                    let uids = p
                        .groups
                        .iter()
                        .find(|g| &g.name == n)
                        .map(|g| g.uids())
                        .unwrap_or_default();
                    MusicFrame::mask_of(uids.iter().filter_map(slot_of))
                }
            };
            let look = layer.look.limited(app.persist.limit_flashing);
            let b = lumigift_fx::scale8u(look.brightness, bright_scale);
            look.frame(slots, mask, b, transition)
        })
        .collect()
}

/// Called every UI frame after the show pipeline (app.rs): enforces the
/// ownership rules and drives the gifts while armed.
pub fn drive(app: &mut App) {
    let playing = app.studio.is_playing();
    if app.lighting.armed && playing && !app.lighting.was_playing {
        // A show started: the Studio takes the gifts back.
        app.lighting.release();
        if app.backend.is_some() && !app.link().is_some_and(|l| l.ota_running()) {
            app.live = true;
            app.live_engaged_at = Some(Instant::now());
        }
        app.toasts
            .info("Show playing: the Studio took over the lights");
    }
    app.lighting.was_playing = playing;
    if app.backend.is_none() {
        app.lighting.armed = false;
    }
    app.lighting.frames = build_frames(app);
    if !app.lighting.armed || app.blackout {
        return;
    }
    let frames = app.lighting.frames.clone();
    let Some(b) = app.backend.as_ref() else {
        return;
    };
    b.link.set_show(ShowOutput::Layers(frames.clone()));
    let snap = app.snap.clone();
    if b.is_sim() {
        // The simulated gifts render it themselves.
        app.lights.update(
            &snap,
            app.backend.as_ref(),
            crate::backend::LightSource::Mirror,
            app.blink_color,
        );
    } else {
        app.lights.update_layers(&snap, &frames);
    }
}

// ---------------------------------------------------------------------------
// Page
// ---------------------------------------------------------------------------

pub fn page(app: &mut App, ui: &mut Ui) {
    shortcuts(app, ui);
    header(app, ui);

    let rect = ui.available_rect_before_wrap();
    let panel_w = if app.compact { 300.0 } else { 348.0 };
    let main = Rect::from_min_max(rect.min, pos2(rect.right() - panel_w, rect.bottom()));
    let side = Rect::from_min_max(pos2(main.right(), rect.top()), rect.max);

    // Main column: stage on top, effect library below.
    let stage_h = (main.height() * 0.40).clamp(200.0, 380.0);
    let stage_r = Rect::from_min_size(main.min, vec2(main.width(), stage_h));
    {
        let mut c = ui.new_child(egui::UiBuilder::new().max_rect(stage_r));
        stage(app, &mut c, stage_r);
    }
    let lib_r = Rect::from_min_max(pos2(main.left(), stage_r.bottom()), main.max);
    ui.painter().hline(
        lib_r.x_range(),
        lib_r.top() + 0.5,
        Stroke::new(1.0, BORDER_SUBTLE),
    );
    {
        let mut c = ui.new_child(
            egui::UiBuilder::new()
                .max_rect(lib_r)
                .layout(Layout::top_down(Align::Min)),
        );
        egui::ScrollArea::vertical()
            .id_salt("light-lib")
            .auto_shrink(false)
            .show(&mut c, |ui| {
                egui::Frame::new()
                    .inner_margin(egui::Margin::symmetric(S7 as i8, S5 as i8))
                    .show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        library(app, ui);
                    });
            });
    }

    // Side panel: output, target, look, quick actions, favourites.
    ui.painter()
        .rect_filled(side, CornerRadius::ZERO, SURFACE_1);
    ui.painter().vline(
        side.left() + 0.5,
        side.y_range(),
        Stroke::new(1.0, BORDER_SUBTLE),
    );
    let mut c = ui.new_child(
        egui::UiBuilder::new()
            .max_rect(side)
            .layout(Layout::top_down(Align::Min)),
    );
    egui::ScrollArea::vertical()
        .id_salt("light-side")
        .auto_shrink(false)
        .show(&mut c, |ui| {
            egui::Frame::new()
                .inner_margin(egui::Margin::same(16))
                .show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    side_panel(app, ui);
                });
        });

    // The cards and the stage animate.
    if !app.persist.reduce_motion || app.lighting.armed {
        ui.ctx().request_repaint();
    }
}

fn shortcuts(app: &mut App, ui: &mut Ui) {
    if ui.ctx().egui_wants_keyboard_input() || app.palette.is_open() {
        return;
    }
    if ui.input_mut(|i| i.consume_key(Modifiers::NONE, Key::F)) {
        toggle_fade_out(app);
    }
    if ui.input_mut(|i| i.consume_key(Modifiers::NONE, Key::O)) && app.backend.is_some() {
        apply_off(app);
    }
    if ui.input_mut(|i| i.consume_key(Modifiers::NONE, Key::B)) {
        toggle_blackout(app);
    }
    if ui.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Enter)) {
        apply(app);
    }
    if ui.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Escape)) {
        app.selection.clear();
        app.focus = None;
    }
}

fn header(app: &mut App, ui: &mut Ui) {
    App::page_header(ui, "Lighting", |ui| {
        widgets::text(
            ui,
            "Generic looks, rendered on the gifts in lockstep",
            "body",
            TEXT_MUTED,
        );
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            ui.spacing_mut().item_spacing.x = 8.0;
            owner_pill(app, ui);
        });
    });
}

/// The ownership pill: accent when the Lighting page drives the gifts.
fn owner_pill(app: &mut App, ui: &mut Ui) {
    let armed = app.lighting.armed;
    let enabled = app.backend.is_some();
    let label = if armed {
        if app.is_sim() {
            "Driving gifts (SIM)"
        } else {
            "Driving gifts"
        }
    } else {
        "Take over"
    };
    let font = theme::font("button");
    let w = widgets::text_width(ui, label, font.clone()) + 40.0;
    let (r, resp) = ui.allocate_exact_size(
        vec2(w, 28.0),
        if enabled {
            Sense::click()
        } else {
            Sense::hover()
        },
    );
    let p = ui.painter();
    if armed {
        p.rect_filled(
            r,
            CornerRadius::same(R_PILL),
            if resp.hovered() { ACCENT_HOVER } else { ACCENT },
        );
        let a = crate::app::breathe(ui);
        p.circle_filled(
            pos2(r.left() + 16.0, r.center().y),
            4.0,
            theme::alpha(ON_ACCENT, a),
        );
        p.text(
            pos2(r.left() + 27.0, r.center().y),
            Align2::LEFT_CENTER,
            label,
            font,
            ON_ACCENT,
        );
    } else {
        p.rect_filled(
            r,
            CornerRadius::same(R_PILL),
            if resp.hovered() { HOVER } else { SURFACE_3 },
        );
        p.rect_stroke(
            r,
            CornerRadius::same(R_PILL),
            Stroke::new(1.0, BORDER_SUBTLE),
            StrokeKind::Inside,
        );
        p.circle_stroke(
            pos2(r.left() + 16.0, r.center().y),
            3.5,
            Stroke::new(1.2, TEXT_MUTED),
        );
        p.text(
            pos2(r.left() + 27.0, r.center().y),
            Align2::LEFT_CENTER,
            label,
            font,
            if enabled { TEXT_MUTED } else { TEXT_DISABLED },
        );
    }
    let tip = if !enabled {
        "Connect the bridge or start the simulator to drive the gifts"
    } else if armed {
        "The Lighting page drives the gifts. Click to release them."
    } else if app.live {
        "The Studio show is LIVE. Click (or apply a look) to take the gifts over."
    } else {
        "Apply a look, or click to drive the gifts with the current layers"
    };
    if resp.on_hover_text(tip).clicked() {
        if armed {
            release(app);
        } else if app.lighting.layers.is_empty() {
            apply(app);
        } else {
            arm(app);
        }
    }
}

// -- stage --------------------------------------------------------------------

fn stage(app: &mut App, ui: &mut Ui, rect: Rect) {
    let mut gifts: Vec<StageGift> = crate::pages::stage_page::stage_gifts(app);
    let virtual_row = gifts.is_empty();
    let now = app.lighting.epoch.elapsed().as_millis() as u32;
    if virtual_row {
        // No gifts: a virtual row renders the edited look locally.
        let look = app
            .persist
            .lighting
            .look
            .limited(app.persist.limit_flashing);
        let f = look.frame(
            8,
            MASK_ALL,
            lumigift_fx::scale8u(look.brightness, app.persist.lighting.master),
            0,
        );
        gifts = (0..8u8)
            .map(|i| {
                let mut leds = [Rgb::BLACK; LEDS_PER_GIFT];
                render_look(&f, now, i, &mut leds);
                StageGift {
                    id: 2000 + i as u64,
                    position: [(i as f32 - 3.5) * crate::stage::SPACING, 0.0],
                    yaw: 0.0,
                    leds,
                    online: true,
                    selected: false,
                    label: format!("#{:02}", i + 1),
                }
            })
            .collect();
    }
    let opts = StageOptions {
        labels: false,
        show_floor_grid: true,
        bloom: 1.15,
        ..Default::default()
    };
    let key =
        gifts.len() * 100_000 + (rect.width() as usize / 8) * 300 + rect.height() as usize / 8 + 7;
    if app.lighting.framed_key != key || app.stage_page.preview_framed_n != key {
        app.lighting.framed_key = key;
        app.stage_page.preview_framed_n = key;
        crate::stage::fit_tight(
            &mut app.stage_preview,
            &gifts,
            rect.width() / rect.height().max(1.0),
        );
    }
    let resp = app.stage_preview.show(ui, &gifts, &opts);
    // Click to select gifts (Ctrl+click toggles), for the "Selected" target;
    // clicking the floor clears the selection.
    if resp.clicked_background && !virtual_row {
        app.selection.clear();
        app.focus = None;
    } else if let Some(id) = resp.clicked
        && !virtual_row
        && let Some(g) = app
            .snap
            .gifts
            .iter()
            .find(|g| crate::pages::stage_page::uid_id(&g.uid) == id)
    {
        let u = g.uid;
        if ui.input(|i| i.modifiers.command) {
            if !app.selection.remove(&u) {
                app.selection.insert(u);
            }
        } else {
            app.selection = [u].into();
        }
        app.focus = Some(u);
        app.lighting.target = TargetSel::Selected;
    }
    let p = ui.painter();
    let (txt, col) = if app.lighting.armed {
        (
            format!(
                "{}  {}",
                icons::BROADCAST,
                if app.is_sim() {
                    "Mirroring the simulated gifts"
                } else {
                    "Mirroring the gifts"
                }
            ),
            ACCENT_TEXT,
        )
    } else if virtual_row {
        ("Preview · virtual gifts".to_string(), TEXT_FAINT)
    } else {
        (
            "Gifts as they are now · apply a look to drive them".to_string(),
            TEXT_FAINT,
        )
    };
    p.text(
        rect.left_top() + vec2(14.0, 14.0),
        Align2::LEFT_TOP,
        txt,
        theme::font("small_medium"),
        col,
    );
    if !virtual_row {
        let n = app.selection.len();
        let hint = if n > 0 {
            format!("{n} selected · Ctrl+click to add · click the floor or Esc to clear")
        } else {
            "Click gifts to select them".into()
        };
        p.text(
            rect.right_top() + vec2(-14.0, 14.0),
            Align2::RIGHT_TOP,
            hint,
            theme::font("small"),
            TEXT_FAINT,
        );
    }
}

// -- effect library -------------------------------------------------------------

fn library(app: &mut App, ui: &mut Ui) {
    let target = layer_target(app)
        .map(|t| target_name(app, &t))
        .unwrap_or_else(|| "the selection".into());
    ui.horizontal(|ui| {
        widgets::overline(ui, "Effects");
        ui.add_space(8.0);
        widgets::text(
            ui,
            format!("Click to apply to {target}"),
            "small",
            TEXT_FAINT,
        );
    });
    ui.add_space(10.0);
    let gap = 12.0;
    let avail = ui.available_width();
    let cols = ((avail + gap) / (172.0 + gap)).floor().max(2.0) as usize;
    let w = ((avail - gap * (cols as f32 - 1.0)) / cols as f32).floor();
    let h = 112.0;
    let now = if app.persist.reduce_motion {
        2400
    } else {
        app.lighting.epoch.elapsed().as_millis() as u32
    };
    let current = current_layer(app);
    let off_active = current.is_some_and(Layer::is_off) && app.lighting.armed;
    let active: Option<u8> = current.filter(|l| !l.is_off()).map(|l| l.look.effect);
    let edited = app.persist.lighting.look;
    // `None` is the Off card, first in the library.
    let items: Vec<Option<u8>> = std::iter::once(None).chain(LIBRARY.iter().map(|&e| Some(e))).collect();
    let mut clicked = None;
    for row in items.chunks(cols) {
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = gap;
            for &item in row {
                let Some(e) = item else {
                    if off_card(ui, vec2(w, h), off_active).clicked() {
                        clicked = Some(None);
                    }
                    continue;
                };
                let look = if e == edited.effect {
                    edited
                } else {
                    edited.with_effect(e)
                };
                let look = look.limited(app.persist.limit_flashing);
                let on = active == Some(e) && app.lighting.armed;
                if effect_card(ui, e, &look, vec2(w, h), now, on, e == edited.effect).clicked() {
                    clicked = Some(Some(e));
                }
            }
        });
        ui.add_space(gap);
    }
    match clicked {
        Some(Some(e)) => apply_effect(app, e),
        Some(None) => apply_off(app),
        None => {}
    }
}

/// The layer for the current target, if any.
fn current_layer(app: &App) -> Option<&Layer> {
    let t = layer_target(app)?;
    app.lighting.layers.iter().rev().find(|l| l.target == t)
}

/// The "Off" card: the target's lights fade to black and stay off.
fn off_card(ui: &mut Ui, size: egui::Vec2, on: bool) -> egui::Response {
    let (r, resp) = ui.allocate_exact_size(size, Sense::click());
    if ui.is_rect_visible(r) {
        let t = ui
            .ctx()
            .animate_bool_with_time(resp.id, resp.hovered(), MOTION_FAST);
        let p = ui.painter();
        let fill = theme::mix(SURFACE_2, HOVER, t * 0.6);
        let stroke = if on {
            theme::alpha(ACCENT, 0.9)
        } else {
            theme::mix(BORDER_SUBTLE, BORDER_STRONG, t)
        };
        if on {
            p.rect_filled(
                r.expand(3.0),
                CornerRadius::same(R_MD + 3),
                theme::alpha(ACCENT, 0.16),
            );
        }
        p.rect_filled(r, CornerRadius::same(R_MD), fill);
        p.rect_stroke(
            r,
            CornerRadius::same(R_MD),
            Stroke::new(if on { 1.5 } else { 1.0 }, stroke),
            StrokeKind::Inside,
        );
        widgets::top_highlight(p, r, R_MD as f32);
        let well = Rect::from_min_size(r.min + vec2(8.0, 8.0), vec2(r.width() - 16.0, 58.0));
        p.rect_filled(well, CornerRadius::same(R_SM), BG_SUNKEN);
        paint_row(
            &p.with_clip_rect(well.shrink(1.0)),
            well,
            &[[Rgb::BLACK; LEDS_PER_GIFT]; 8],
        );
        let y = well.bottom() + (r.bottom() - well.bottom()) / 2.0;
        widgets::paint_icon(
            p,
            pos2(r.left() + 18.0, y),
            icons::POWER,
            16.0,
            if on { ACCENT_TEXT } else { theme::mix(TEXT_MUTED, TEXT, t) },
        );
        p.text(
            pos2(r.left() + 32.0, y),
            Align2::LEFT_CENTER,
            "Off",
            theme::font("label"),
            if on { TEXT } else { TEXT_SECONDARY },
        );
    }
    resp.on_hover_ui(|ui| {
        widgets::tooltip_with_keys(
            ui,
            "Turn the target's lights off (fades with the crossfade time) until you apply a look again",
            Some("O"),
        )
    })
}

fn effect_card(
    ui: &mut Ui,
    e: u8,
    look: &Look,
    size: egui::Vec2,
    now: u32,
    on: bool,
    edited: bool,
) -> egui::Response {
    let (r, resp) = ui.allocate_exact_size(size, Sense::click());
    let info = EFFECTS[e as usize];
    if ui.is_rect_visible(r) {
        let t = ui
            .ctx()
            .animate_bool_with_time(resp.id, resp.hovered(), MOTION_FAST);
        let p = ui.painter();
        let fill = theme::mix(SURFACE_2, HOVER, t * 0.6);
        let stroke = if on {
            theme::alpha(ACCENT, 0.9)
        } else {
            theme::mix(BORDER_SUBTLE, BORDER_STRONG, t)
        };
        if on {
            p.rect_filled(
                r.expand(3.0),
                CornerRadius::same(R_MD + 3),
                theme::alpha(ACCENT, 0.16),
            );
        }
        p.rect_filled(r, CornerRadius::same(R_MD), fill);
        p.rect_stroke(
            r,
            CornerRadius::same(R_MD),
            Stroke::new(if on { 1.5 } else { 1.0 }, stroke),
            StrokeKind::Inside,
        );
        widgets::top_highlight(p, r, R_MD as f32);
        // Live preview: 8 virtual gifts.
        let well = Rect::from_min_size(r.min + vec2(8.0, 8.0), vec2(r.width() - 16.0, 58.0));
        p.rect_filled(well, CornerRadius::same(R_SM), BG_SUNKEN);
        let f = look.frame(8, MASK_ALL, 255, 0);
        let mut leds = [[Rgb::BLACK; LEDS_PER_GIFT]; 8];
        for (i, l) in leds.iter_mut().enumerate() {
            render_look(&f, now, i as u8, l);
        }
        paint_row(&p.with_clip_rect(well.shrink(1.0)), well, &leds);
        // Name row.
        let y = well.bottom() + (r.bottom() - well.bottom()) / 2.0;
        let ic = if on {
            ACCENT_TEXT
        } else {
            theme::mix(TEXT_MUTED, TEXT, t)
        };
        widgets::paint_icon(
            p,
            pos2(r.left() + 18.0, y),
            icons::EFFECT[e as usize],
            16.0,
            ic,
        );
        p.text(
            pos2(r.left() + 32.0, y),
            Align2::LEFT_CENTER,
            info.name,
            theme::font("label"),
            if on { TEXT } else { TEXT_SECONDARY },
        );
        let mut x = r.right() - 12.0;
        if info.music == Music::Optional {
            widgets::paint_icon(p, pos2(x - 7.0, y), icons::MUSIC_NOTES, 13.0, TEXT_FAINT);
            x -= 20.0;
        }
        if edited && !on {
            p.circle_filled(pos2(x - 3.0, y), 3.0, TEXT_FAINT);
        }
    }
    let tip = format!(
        "{}{}",
        blurb(e),
        if info.music == Music::Optional {
            "\nReacts to the music while a show plays."
        } else {
            ""
        }
    );
    resp.on_hover_ui(|ui| widgets::tooltip_with_keys(ui, &tip, None))
}

/// A row of mini gifts (2×2 LEDs like the gems) with soft glows.
fn paint_row(p: &egui::Painter, well: Rect, leds: &[[Rgb; LEDS_PER_GIFT]]) {
    let n = leds.len() as f32;
    let cell = well.width() / n;
    let s = (cell - 5.0).min(well.height() - 18.0).max(8.0);
    let dot = s * 0.17;
    for (i, g) in leds.iter().enumerate() {
        let c = pos2(well.left() + cell * (i as f32 + 0.5), well.center().y);
        let body = Rect::from_center_size(c, vec2(s, s));
        // Glow of the gift's average colour.
        let avg = g.iter().fold([0u32; 3], |a, l| {
            [a[0] + l.r as u32, a[1] + l.g as u32, a[2] + l.b as u32]
        });
        let avg = Color32::from_rgb((avg[0] / 4) as u8, (avg[1] / 4) as u8, (avg[2] / 4) as u8);
        let lum = (avg.r() as f32 * 0.3 + avg.g() as f32 * 0.6 + avg.b() as f32 * 0.1) / 255.0;
        if lum > 0.01 {
            widgets::soft_glow(p, c, s * 1.15, theme::alpha(avg, (0.25 + lum).min(0.75)));
        }
        p.rect_filled(
            body,
            CornerRadius::same(R_XS),
            theme::alpha(SURFACE_1, 0.85),
        );
        p.rect_stroke(
            body,
            CornerRadius::same(R_XS),
            Stroke::new(1.0, BORDER_SUBTLE),
            StrokeKind::Inside,
        );
        let off = s * 0.22;
        let centers = [
            c + vec2(-off, -off),
            c + vec2(off, -off),
            c + vec2(off, off),
            c + vec2(-off, off),
        ];
        for (k, l) in g.iter().enumerate() {
            let on = l.r as u16 + l.g as u16 + l.b as u16 > 6;
            p.circle_filled(
                centers[k],
                dot,
                if on { widgets::rgb32(*l) } else { SURFACE_3 },
            );
        }
    }
}

// -- side panel -----------------------------------------------------------------

fn section(ui: &mut Ui, title: &str) {
    ui.add_space(S6);
    widgets::overline(ui, title);
    ui.add_space(8.0);
}

fn side_panel(app: &mut App, ui: &mut Ui) {
    output_section(app, ui);
    quick_section(app, ui);
    target_section(app, ui);
    look_section(app, ui);
    favourites_section(app, ui);
}

fn output_section(app: &mut App, ui: &mut Ui) {
    widgets::overline(ui, "Output");
    ui.add_space(8.0);
    // Master scales every layer: at 0 every look renders black.
    if app.persist.lighting.master == 0 {
        widgets::note(
            ui,
            "Master is at 0 %: every look is dark. Raise Master under Quick actions.",
            WARN_SOFT,
            WARN,
            8.0,
        );
        ui.add_space(4.0);
        if widgets::secondary_button_h(ui, None, "Master back to 100 %", true, 24.0).clicked() {
            app.persist.lighting.master = 255;
        }
        ui.add_space(10.0);
    }
    let armed = app.lighting.armed;
    let (icon, line, color) = if app.backend.is_none() {
        (
            icons::PLUGS,
            "No bridge: previewing on virtual gifts".to_string(),
            TEXT_MUTED,
        )
    } else if app.blackout {
        (
            icons::LIGHTNING_SLASH,
            "Global blackout is on (Ctrl+Shift+B)".into(),
            WARN,
        )
    } else if armed {
        (
            icons::BROADCAST,
            "The Lighting page drives the gifts".into(),
            ACCENT_TEXT,
        )
    } else if app.live {
        (
            icons::WAVEFORM,
            "The Studio show is LIVE on the gifts".into(),
            TEXT_SECONDARY,
        )
    } else {
        (icons::CIRCLE, "Not driving the gifts".into(), TEXT_MUTED)
    };
    ui.horizontal(|ui| {
        widgets::text(ui, icon, "body", color);
        widgets::text(ui, line, "body", color);
    });
    if !armed && app.backend.is_some() {
        ui.add_space(4.0);
        widgets::text(
            ui,
            "Click an effect to take over. Playing a show in the Studio takes the gifts back.",
            "small",
            TEXT_FAINT,
        );
    }
    let no_slot = app
        .snap
        .gifts
        .iter()
        .filter(|g| g.online && g.slot.is_none())
        .count();
    if no_slot > 0 {
        ui.add_space(8.0);
        widgets::note(
            ui,
            format!(
                "{no_slot} gift{} without a slot all render like one gift, and only follow \"All gifts\". Spatial looks and groups need slots.",
                if no_slot == 1 { "" } else { "s" }
            ),
            WARN_SOFT,
            WARN,
            8.0,
        );
        ui.add_space(4.0);
        if widgets::secondary_button_h(
            ui,
            Some(icons::LIST_NUMBERS),
            "Auto-assign slots",
            app.connected(),
            24.0,
        )
        .clicked()
        {
            app.auto_assign();
        }
    }
    if !app.lighting.layers.is_empty() {
        ui.add_space(10.0);
        let mut remove = None;
        let n = app.lighting.layers.len();
        for i in 0..n {
            let layer = app.lighting.layers[i].clone();
            let name = target_name(app, &layer.target);
            let (r, resp) =
                ui.allocate_exact_size(vec2(ui.available_width(), 34.0), Sense::click());
            let p = ui.painter();
            p.rect_filled(
                r,
                CornerRadius::same(R_SM),
                if resp.hovered() { HOVER } else { SURFACE_2 },
            );
            widgets::palette_strip(
                p,
                Rect::from_min_size(
                    pos2(r.left() + 8.0, r.top() + 6.0),
                    vec2(4.0, r.height() - 12.0),
                ),
                &layer.look.stops(),
                if layer.is_off() { 0.15 } else if armed { 1.0 } else { 0.5 },
            );
            widgets::paint_icon(
                p,
                pos2(r.left() + 26.0, r.center().y),
                if layer.is_off() { icons::POWER } else { icons::EFFECT[layer.look.effect as usize] },
                14.0,
                TEXT_MUTED,
            );
            p.text(
                pos2(r.left() + 40.0, r.center().y - 7.0),
                Align2::LEFT_CENTER,
                &name,
                theme::font("label"),
                TEXT,
            );
            p.text(
                pos2(r.left() + 40.0, r.center().y + 7.0),
                Align2::LEFT_CENTER,
                if layer.is_off() { "Off".to_string() } else { layer.look.label() },
                theme::font("small"),
                TEXT_MUTED,
            );
            let xr = Rect::from_center_size(pos2(r.right() - 16.0, r.center().y), vec2(22.0, 22.0));
            let xresp = ui.interact(xr, ui.scope_id().with(("rm-layer", i)), Sense::click());
            if resp.hovered() || xresp.hovered() {
                ui.painter().rect_filled(
                    xr,
                    CornerRadius::same(R_XS),
                    if xresp.hovered() {
                        ACTIVE
                    } else {
                        Color32::TRANSPARENT
                    },
                );
                widgets::paint_icon(ui.painter(), xr.center(), icons::X, 13.0, TEXT_MUTED);
            }
            if xresp.on_hover_text("Remove this layer (its gifts keep the last look until another layer covers them)").clicked() {
                remove = Some(i);
            } else if resp.clicked() {
                // Select this layer's target to edit it.
                app.lighting.target = match &layer.target {
                    LayerTarget::All => TargetSel::All,
                    LayerTarget::Gifts(s) => {
                        app.selection = s.clone();
                        TargetSel::Selected
                    }
                    LayerTarget::Group(n) => TargetSel::Group(n.clone()),
                };
                // An Off layer keeps the edited look (not its brightness 0).
                if !layer.is_off() {
                    app.persist.lighting.look = layer.look;
                }
            }
            ui.add_space(4.0);
        }
        if let Some(i) = remove {
            app.lighting.layers.remove(i);
        }
    }
}

fn target_section(app: &mut App, ui: &mut Ui) {
    section(ui, "Target");
    let mut sel = match app.lighting.target {
        TargetSel::All => 0,
        TargetSel::Selected => 1,
        TargetSel::Group(_) => 2,
    };
    let n_sel = app.selection.len();
    let sel_label = if n_sel > 0 {
        format!("Selected ({n_sel})")
    } else {
        "Selected".into()
    };
    if widgets::segmented(
        ui,
        "light-target",
        &mut sel,
        &["All gifts", &sel_label, "Group"],
    ) {
        app.lighting.target = match sel {
            0 => TargetSel::All,
            1 => TargetSel::Selected,
            _ => TargetSel::Group(
                app.persist
                    .lighting
                    .groups
                    .first()
                    .map(|g| g.name.clone())
                    .unwrap_or_default(),
            ),
        };
        load_target_look(app);
    }
    ui.add_space(8.0);
    match app.lighting.target.clone() {
        TargetSel::All => widgets::text(
            ui,
            "Every gift, including gifts without a slot.",
            "small",
            TEXT_FAINT,
        ),
        TargetSel::Selected => {
            let no_slot = app
                .selection
                .iter()
                .filter(|u| app.snap.gift(u).is_some_and(|g| g.slot.is_none()))
                .count();
            let msg = if n_sel == 0 {
                "Click gifts on the stage to select them.".to_string()
            } else {
                format!(
                    "{n_sel} gift{} selected on the stage or in the Fleet.",
                    if n_sel == 1 { "" } else { "s" }
                )
            };
            let r = widgets::text(ui, msg, "small", TEXT_FAINT);
            if no_slot > 0 {
                ui.add_space(4.0);
                widgets::note(
                    ui,
                    format!(
                        "{no_slot} selected gift(s) have no slot and can only follow \"All gifts\". Assign slots in the Fleet."
                    ),
                    WARN_SOFT,
                    WARN,
                    8.0,
                );
            }
            r
        }
        TargetSel::Group(_) => widgets::text(
            ui,
            "Named groups are saved by gift, so they survive slot changes.",
            "small",
            TEXT_FAINT,
        ),
    };
    // Group chips.
    ui.add_space(8.0);
    let mut select_group = None;
    let mut delete = None;
    let mut update = None;
    let mut rename_start = None;
    let mut rename_done = None;
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing = vec2(6.0, 6.0);
        for (i, g) in app.persist.lighting.groups.iter().enumerate() {
            if let Some((ri, buf)) = app.lighting.renaming_group.as_mut()
                && *ri == i
            {
                let resp = ui.add(
                    egui::TextEdit::singleline(buf)
                        .desired_width(110.0)
                        .font(theme::font("small")),
                );
                resp.request_focus();
                if resp.lost_focus() {
                    rename_done = Some((*ri, buf.trim().to_string()));
                }
                continue;
            }
            let on = app.lighting.target == TargetSel::Group(g.name.clone());
            let label = format!("{}  {}", g.name, g.uids.len());
            let font = theme::font("small_medium");
            let w = widgets::text_width(ui, &label, font.clone()) + 34.0;
            let (r, resp) = ui.allocate_exact_size(vec2(w, 24.0), Sense::click());
            let p = ui.painter();
            p.rect_filled(
                r,
                CornerRadius::same(R_PILL),
                if on {
                    ACCENT_SOFT
                } else if resp.hovered() {
                    HOVER
                } else {
                    SURFACE_3
                },
            );
            if on {
                p.rect_stroke(
                    r,
                    CornerRadius::same(R_PILL),
                    Stroke::new(1.0, theme::alpha(ACCENT, 0.7)),
                    StrokeKind::Inside,
                );
            }
            widgets::paint_icon(
                p,
                pos2(r.left() + 14.0, r.center().y),
                icons::STACK,
                12.0,
                if on { ACCENT_TEXT } else { TEXT_MUTED },
            );
            p.text(
                pos2(r.left() + 25.0, r.center().y),
                Align2::LEFT_CENTER,
                &label,
                font,
                if on { ACCENT_TEXT } else { TEXT_SECONDARY },
            );
            if resp.clicked() {
                select_group = Some(i);
            }
            if resp.double_clicked() {
                rename_start = Some(i);
            }
            resp.on_hover_text("Click to target · double-click to rename · right-click for more")
                .context_menu(|ui| {
                    if ui.button("Rename").clicked() {
                        rename_start = Some(i);
                        ui.close();
                    }
                    if ui.button("Replace with the selection").clicked() {
                        update = Some(i);
                        ui.close();
                    }
                    if ui.button("Select its gifts").clicked() {
                        select_group = Some(usize::MAX - i);
                        ui.close();
                    }
                    ui.separator();
                    if ui.button("Delete group").clicked() {
                        delete = Some(i);
                        ui.close();
                    }
                });
        }
        let enabled = !app.selection.is_empty();
        let resp = widgets::secondary_button_h(
            ui,
            Some(icons::PLUS),
            "Group from selection",
            enabled,
            24.0,
        );
        let resp = resp.on_hover_text(if enabled {
            "Save the selected gifts as a named group"
        } else {
            "Select gifts first (click them on the stage)"
        });
        if resp.clicked() {
            new_group(app);
        }
    });
    if let Some((i, name)) = rename_done {
        app.lighting.renaming_group = None;
        if !name.is_empty() {
            rename_group(app, i, name);
        }
    }
    if let Some(i) = rename_start {
        app.lighting.renaming_group = Some((i, app.persist.lighting.groups[i].name.clone()));
    }
    if let Some(i) = select_group {
        if i > usize::MAX / 2 {
            let g = &app.persist.lighting.groups[usize::MAX - i];
            app.selection = g.uids();
        } else {
            app.lighting.target = TargetSel::Group(app.persist.lighting.groups[i].name.clone());
            load_target_look(app);
        }
    }
    if let Some(i) = update {
        app.persist.lighting.groups[i].uids = app.selection.iter().map(|u| u.to_hex()).collect();
        app.toasts.info(format!(
            "{} now has {} gifts",
            app.persist.lighting.groups[i].name,
            app.selection.len()
        ));
    }
    if let Some(i) = delete {
        let g = app.persist.lighting.groups.remove(i);
        app.lighting
            .layers
            .retain(|l| l.target != LayerTarget::Group(g.name.clone()));
        if app.lighting.target == TargetSel::Group(g.name.clone()) {
            app.lighting.target = TargetSel::All;
        }
        app.toasts.info(format!("Deleted group {}", g.name));
    }
}

fn new_group(app: &mut App) {
    let groups = &app.persist.lighting.groups;
    let mut k = groups.len() + 1;
    while groups.iter().any(|g| g.name == format!("Group {k}")) {
        k += 1;
    }
    let name = format!("Group {k}");
    let uids: Vec<String> = app.selection.iter().map(|u| u.to_hex()).collect();
    let n = uids.len();
    app.persist.lighting.groups.push(Group {
        name: name.clone(),
        uids,
    });
    app.lighting.target = TargetSel::Group(name.clone());
    app.toasts.success(format!(
        "Created {name} with {n} gift{}",
        if n == 1 { "" } else { "s" }
    ));
}

fn rename_group(app: &mut App, i: usize, name: String) {
    if app
        .persist
        .lighting
        .groups
        .iter()
        .enumerate()
        .any(|(j, g)| j != i && g.name == name)
    {
        app.toasts.push(
            ToastKind::Warn,
            format!("A group called {name} already exists"),
            None,
        );
        return;
    }
    let old = std::mem::replace(&mut app.persist.lighting.groups[i].name, name.clone());
    for l in &mut app.lighting.layers {
        if l.target == LayerTarget::Group(old.clone()) {
            l.target = LayerTarget::Group(name.clone());
        }
    }
    if app.lighting.target == TargetSel::Group(old) {
        app.lighting.target = TargetSel::Group(name);
    }
}

/// Load the current target's look into the editor (if it has a layer).
fn load_target_look(app: &mut App) {
    if let Some(l) = current_layer(app) {
        app.persist.lighting.look = l.look;
    }
}

fn look_section(app: &mut App, ui: &mut Ui) {
    section(ui, "Look");
    let before = app.persist.lighting.look;
    let lw = 84.0;
    // Palette swatches: 8 palettes + Custom.
    let mut pick = None;
    let w = ((ui.available_width() - 12.0) / 3.0).floor();
    egui::Grid::new("light-pal")
        .spacing(vec2(6.0, 6.0))
        .show(ui, |ui| {
            for i in 0..=PALETTES.len() {
                let custom = i == PALETTES.len();
                let id = if custom { PALETTE_CUSTOM } else { i as u8 };
                let on = before.palette == id;
                let stops = if custom {
                    Look {
                        palette: PALETTE_CUSTOM,
                        ..before
                    }
                    .stops()
                } else {
                    PALETTES[i]
                };
                let (r, resp) = ui.allocate_exact_size(vec2(w, 38.0), Sense::click());
                let p = ui.painter();
                p.rect_filled(
                    r,
                    CornerRadius::same(R_SM),
                    if resp.hovered() { HOVER } else { SURFACE_2 },
                );
                widgets::palette_strip(
                    p,
                    Rect::from_min_size(r.min + vec2(6.0, 6.0), vec2(r.width() - 12.0, 10.0)),
                    &stops,
                    1.0,
                );
                let name = if custom { "Custom" } else { PALETTE_NAMES[i] };
                p.text(
                    pos2(r.left() + 7.0, r.bottom() - 10.0),
                    Align2::LEFT_CENTER,
                    name,
                    theme::font("small"),
                    if on { TEXT } else { TEXT_MUTED },
                );
                if on {
                    p.rect_stroke(
                        r,
                        CornerRadius::same(R_SM),
                        Stroke::new(1.5, ACCENT),
                        StrokeKind::Inside,
                    );
                }
                if resp.clicked() {
                    pick = Some(id);
                }
                if i % 3 == 2 {
                    ui.end_row();
                }
            }
        });
    if let Some(id) = pick {
        app.persist.lighting.look.palette = id;
    }
    let look = &mut app.persist.lighting.look;
    let info = EFFECTS[look.effect as usize];
    ui.add_space(8.0);
    if look.palette == PALETTE_CUSTOM {
        ui.horizontal(|ui| {
            widgets::column_label(ui, "Colours", lw, CONTROL_H, TEXT_MUTED);
            egui::color_picker::color_edit_button_srgb(ui, &mut look.colors[0])
                .on_hover_text("Primary colour");
            ui.add_space(4.0);
            egui::color_picker::color_edit_button_srgb(ui, &mut look.colors[1])
                .on_hover_text("Secondary colour");
            if widgets::ghost_button_sized(ui, icons::ARROW_RIGHT, "Swap", None, 24.0, false)
                .clicked()
            {
                look.colors.swap(0, 1);
            }
        });
        ui.add_space(2.0);
    } else {
        slider(ui, "Hue shift", lw, &mut look.hue, 0..=255, |v| {
            format!("{v}")
        });
    }
    let limit = app.persist.limit_flashing && info.flashing;
    if let Some(l) = info.speed {
        let max = if limit { FLASH_SPEED_CAP } else { 255 };
        look.speed = look.speed.clamp(1, max);
        slider(ui, l, lw, &mut look.speed, 1..=max, speed_label);
    }
    if let Some(l) = info.size {
        if info.size_is_toggle {
            ui.horizontal(|ui| {
                widgets::column_label(ui, l, lw, CONTROL_H, TEXT_MUTED);
                let mut on = look.size >= 128;
                let forced = limit && look.effect == effect::POLICE;
                if forced {
                    on = true;
                }
                let r = widgets::toggle(ui, &mut on);
                if forced {
                    r.on_hover_text("Forced on by Settings › Limit flashing");
                } else {
                    look.size = if on { 192 } else { 64 };
                }
            });
        } else {
            look.size = look.size.max(1);
            slider(ui, l, lw, &mut look.size, 1..=255, |v| format!("{v}"));
        }
    }
    slider(ui, "Brightness", lw, &mut look.brightness, 0..=255, pct);
    if limit {
        ui.add_space(2.0);
        widgets::text(
            ui,
            "Speed capped at ~3 Hz (Settings › Limit flashing).",
            "small",
            TEXT_FAINT,
        );
    }
    let changed = app.persist.lighting.look != before;
    ui.add_space(10.0);
    let target = layer_target(app)
        .map(|t| target_name(app, &t))
        .unwrap_or_else(|| "selection".into());
    let live_edit = app.lighting.armed && current_layer(app).is_some();
    if changed && live_edit {
        apply(app);
    }
    ui.horizontal(|ui| {
        let label = format!("Apply to {target}");
        if widgets::primary_button(ui, Some(icons::LIGHTBULB), &label, true)
            .on_hover_ui(|ui| {
                widgets::tooltip_with_keys(ui, "Apply this look and drive the gifts", Some("Enter"))
            })
            .clicked()
        {
            apply(app);
        }
    });
    if live_edit {
        ui.add_space(4.0);
        widgets::text(
            ui,
            "Live: changes go straight to the gifts.",
            "small",
            TEXT_FAINT,
        );
    }
}

fn speed_label(v: u8) -> String {
    // Cycle length of the effect clock (see lumigift_fx docs).
    let rate = 1.0 + (v as f32 * v as f32 / 128.0).floor();
    let period_s = 65.536 / rate;
    if period_s >= 10.0 {
        format!("{period_s:.0} s")
    } else if period_s >= 1.0 {
        format!("{period_s:.1} s")
    } else {
        format!("{:.1} Hz", 1.0 / period_s)
    }
}

fn pct(v: u8) -> String {
    format!("{:.0}%", v as f32 / 2.55)
}

/// A labelled u8 slider with a formatted value on the right.
fn slider(
    ui: &mut Ui,
    label: &str,
    lw: f32,
    v: &mut u8,
    range: std::ops::RangeInclusive<u8>,
    fmt: impl Fn(u8) -> String,
) -> bool {
    let mut changed = false;
    ui.horizontal(|ui| {
        widgets::column_label(ui, label, lw, CONTROL_H, TEXT_MUTED);
        let w = (ui.available_width() - 58.0).max(60.0);
        let mut f = *v as f32;
        let r = ui.add_sized(
            vec2(w, 20.0),
            egui::Slider::new(&mut f, *range.start() as f32..=*range.end() as f32)
                .show_value(false),
        );
        if r.changed() {
            *v = f.round() as u8;
            changed = true;
        }
        widgets::text(ui, fmt(*v), "value", TEXT_SECONDARY);
    });
    changed
}

fn quick_section(app: &mut App, ui: &mut Ui) {
    section(ui, "Quick actions");
    let lw = 84.0;
    let p = &mut app.persist.lighting;
    slider(ui, "Master", lw, &mut p.master, 0..=255, pct);
    let mut tr = (p.transition_ms / TRANSITION_UNIT_MS).min(100) as u8;
    if slider(ui, "Crossfade", lw, &mut tr, 0..=100, |v| {
        if v == 0 {
            "Cut".into()
        } else {
            format!("{:.1} s", v as f32 * 0.05)
        }
    }) {
        p.transition_ms = tr as u32 * TRANSITION_UNIT_MS;
    }
    ui.add_space(8.0);
    let can = app.lighting.armed || !app.lighting.layers.is_empty();
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 8.0;
        let w = ((ui.available_width() - 8.0) / 2.0).floor();
        let fade = app.lighting.fade_out;
        if toggle_button(
            ui,
            icons::MOON,
            if fade { "Fade in" } else { "Fade out" },
            fade,
            can,
            w,
        )
        .on_hover_ui(|ui| {
            widgets::tooltip_with_keys(
                ui,
                "Fade every layer to black (at least 1.5 s), and back",
                Some("F"),
            )
        })
        .clicked()
        {
            toggle_fade_out(app);
        }
        let bo = app.lighting.blackout;
        if toggle_button(ui, icons::LIGHTNING_SLASH, "Blackout", bo, can, w)
            .on_hover_ui(|ui| {
                widgets::tooltip_with_keys(
                    ui,
                    "All gifts dark at once; again to restore",
                    Some("B"),
                )
            })
            .clicked()
        {
            toggle_blackout(app);
        }
    });
}

/// A secondary-style button that stays highlighted while `on`.
fn toggle_button(
    ui: &mut Ui,
    icon: &str,
    label: &str,
    on: bool,
    enabled: bool,
    w: f32,
) -> egui::Response {
    let (r, resp) = ui.allocate_exact_size(
        vec2(w, CONTROL_H + 4.0),
        if enabled {
            Sense::click()
        } else {
            Sense::hover()
        },
    );
    let p = ui.painter();
    let fill = if !enabled {
        SURFACE_3
    } else if on {
        ACCENT_SOFT
    } else if resp.hovered() {
        HOVER
    } else {
        SURFACE_3
    };
    p.rect_filled(r, CornerRadius::same(R_SM), fill);
    p.rect_stroke(
        r,
        CornerRadius::same(R_SM),
        Stroke::new(
            1.0,
            if on {
                theme::alpha(ACCENT, 0.7)
            } else {
                BORDER
            },
        ),
        StrokeKind::Inside,
    );
    let fg = if !enabled {
        TEXT_DISABLED
    } else if on {
        ACCENT_TEXT
    } else {
        TEXT_SECONDARY
    };
    let font = theme::font("button");
    let tw = widgets::text_width(ui, label, font.clone());
    let x = r.center().x - (tw + 22.0) / 2.0;
    widgets::paint_icon(p, pos2(x + 8.0, r.center().y), icon, 15.0, fg);
    p.text(
        pos2(x + 22.0, r.center().y),
        Align2::LEFT_CENTER,
        label,
        font,
        fg,
    );
    resp
}

fn favourites_section(app: &mut App, ui: &mut Ui) {
    section(ui, "Favourites");
    let mut apply_i = None;
    let mut delete = None;
    let mut rename_start = None;
    let mut rename_done = None;
    let mut overwrite = None;
    for (i, pr) in app.persist.lighting.presets.iter().enumerate() {
        if let Some((ri, buf)) = app.lighting.renaming_preset.as_mut()
            && *ri == i
        {
            let resp = ui.add(
                egui::TextEdit::singleline(buf)
                    .desired_width(ui.available_width())
                    .font(theme::font("body")),
            );
            resp.request_focus();
            if resp.lost_focus() {
                rename_done = Some((i, buf.trim().to_string()));
            }
            ui.add_space(4.0);
            continue;
        }
        let (r, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 34.0), Sense::click());
        let p = ui.painter();
        p.rect_filled(
            r,
            CornerRadius::same(R_SM),
            if resp.hovered() { HOVER } else { SURFACE_2 },
        );
        widgets::palette_strip(
            p,
            Rect::from_min_size(pos2(r.right() - 64.0, r.center().y - 4.0), vec2(52.0, 8.0)),
            &pr.look.stops(),
            1.0,
        );
        widgets::paint_icon(
            p,
            pos2(r.left() + 16.0, r.center().y),
            icons::EFFECT[pr.look.effect as usize],
            15.0,
            TEXT_MUTED,
        );
        p.text(
            pos2(r.left() + 32.0, r.center().y - 7.0),
            Align2::LEFT_CENTER,
            &pr.name,
            theme::font("label"),
            TEXT,
        );
        p.text(
            pos2(r.left() + 32.0, r.center().y + 7.0),
            Align2::LEFT_CENTER,
            pr.look.label(),
            theme::font("small"),
            TEXT_FAINT,
        );
        if resp.double_clicked() {
            rename_start = Some(i);
        } else if resp.clicked() {
            apply_i = Some(i);
        }
        resp.on_hover_text("Click to apply · double-click to rename · right-click for more")
            .context_menu(|ui| {
                if ui.button("Rename").clicked() {
                    rename_start = Some(i);
                    ui.close();
                }
                if ui.button("Overwrite with the current look").clicked() {
                    overwrite = Some(i);
                    ui.close();
                }
                ui.separator();
                if ui.button("Delete").clicked() {
                    delete = Some(i);
                    ui.close();
                }
            });
        ui.add_space(4.0);
    }
    if let Some((i, name)) = rename_done {
        app.lighting.renaming_preset = None;
        if !name.is_empty() {
            app.persist.lighting.presets[i].name = name;
        }
    }
    if let Some(i) = rename_start {
        app.lighting.renaming_preset = Some((i, app.persist.lighting.presets[i].name.clone()));
    }
    if let Some(i) = overwrite {
        app.persist.lighting.presets[i].look = app.persist.lighting.look;
        app.toasts
            .info(format!("Saved {}", app.persist.lighting.presets[i].name));
    }
    if let Some(i) = apply_i {
        apply_preset(app, i);
    }
    if let Some(i) = delete {
        let p = app.persist.lighting.presets.remove(i);
        app.toasts.info(format!("Deleted {}", p.name));
    }
    ui.add_space(4.0);
    if widgets::secondary_button(ui, Some(icons::STAR), "Save current look", true).clicked() {
        let look = app.persist.lighting.look;
        let name = look.label();
        app.persist.lighting.presets.push(Preset {
            name: name.clone(),
            look,
        });
        app.toasts
            .success(format!("Saved \"{name}\" to favourites"));
    }
}
