//! Stage page: full-bleed stage view with floating arrange tools and a mini
//! transport (docs/UI_DESIGN.md §3.2 Stage, §5.6).

use std::time::Instant;

use egui::{Align, Align2, CornerRadius, Key, Layout, Modifiers, Rect, Sense, Stroke, StrokeKind, Ui, pos2, vec2};
use lumigift_core::Uid;

use crate::app::{App, fmt_timecode};
use crate::icons;
use crate::stage::{Arrangement, CameraPreset, SPACING, Stage, StageGift, StageOptions};
use crate::theme::{self, *};
use crate::widgets::{self, EmptyAction, EmptyState};

pub struct StagePage {
    pub arrange: bool,
    pub preset: usize,
    pub framed_n: usize,
    pub preview_framed_n: usize,
    /// "Set order" in progress (see [`OrderWizard`]).
    pub order: Option<OrderWizard>,
}

/// "Set order": the gifts blink one at a time and the user says where each
/// one stands in the room (counting from the left). Saving assigns slots in
/// that order (spatial looks then run the right way on the real gifts) and
/// moves the stage boxes to match.
pub struct OrderWizard {
    /// Gifts still to place, the one blinking first.
    queue: Vec<Uid>,
    /// Room position (0 = leftmost) → gift.
    placed: Vec<Option<Uid>>,
    /// Placements, for Back.
    history: Vec<(Uid, usize)>,
    next_blink: Instant,
}

impl OrderWizard {
    fn new(gifts: Vec<Uid>) -> Self {
        let n = gifts.len();
        Self { queue: gifts, placed: vec![None; n], history: Vec::new(), next_blink: Instant::now() }
    }
    fn current(&self) -> Option<Uid> {
        self.queue.first().copied()
    }
    fn place(&mut self, pos: usize) {
        if let Some(u) = self.current()
            && self.placed.get(pos).is_some_and(|p| p.is_none())
        {
            self.placed[pos] = Some(u);
            self.queue.remove(0);
            self.history.push((u, pos));
            self.next_blink = Instant::now();
        }
    }
    fn back(&mut self) {
        if let Some((u, pos)) = self.history.pop() {
            self.placed[pos] = None;
            self.queue.insert(0, u);
            self.next_blink = Instant::now();
        }
    }
    fn skip(&mut self) {
        if self.queue.len() > 1 {
            let u = self.queue.remove(0);
            self.queue.push(u);
            self.next_blink = Instant::now();
        }
    }
    fn done(&self) -> bool {
        self.queue.is_empty()
    }
}

/// Marker: the camera was restored from settings, don't auto-frame.
pub const SKIP_AUTO_FRAME: usize = usize::MAX - 1;

impl Default for StagePage {
    fn default() -> Self {
        Self { arrange: false, preset: 2, framed_n: usize::MAX, preview_framed_n: usize::MAX, order: None }
    }
}

/// Stage id of a gift: a hash of the whole UID. (Bytes 4..12 alone are the
/// wafer and lot number, the same for every chip of a batch: three such
/// gifts all got one id, so clicks always selected the first.)
pub fn uid_id(u: &Uid) -> u64 {
    u.0.iter().fold(0xcbf2_9ce4_8422_2325u64, |h, &b| (h ^ b as u64).wrapping_mul(0x100_0000_01b3))
}

/// Stage gifts for every known gift (positions from the persisted layout;
/// unknown gifts are auto-placed in a row on the right).
pub fn stage_gifts(app: &mut App) -> Vec<StageGift> {
    let mut out = Vec::new();
    let mut max_x = f32::MIN;
    for p in app.persist.stage_positions.values() {
        max_x = max_x.max(p[0]);
    }
    let gifts = app.snap.gifts.clone();
    let n = gifts.len();
    let first_run = app.persist.stage_positions.is_empty();
    if max_x == f32::MIN {
        max_x = -SPACING;
    }
    let mut placed_new = 0;
    for (i, g) in gifts.iter().enumerate() {
        let key = g.uid.to_hex();
        let pos = match app.persist.stage_positions.get(&key) {
            Some(p) => *p,
            None => {
                let p = if first_run {
                    // First run: a centred row.
                    [(i as f32 - (n as f32 - 1.0) / 2.0) * SPACING, 0.0, 0.0]
                } else {
                    placed_new += 1;
                    [max_x + SPACING * placed_new as f32, 0.0, 0.0]
                };
                app.persist.stage_positions.insert(key, p);
                p
            }
        };
        let name = app.gift_name(&g.uid);
        let label = match g.slot {
            Some(s) => format!("{}  {}{}", crate::health::fmt_slot(s), name, if app.is_sim() { " · SIM" } else { "" }),
            None => name,
        };
        out.push(StageGift {
            id: uid_id(&g.uid),
            position: [pos[0], pos[1]],
            yaw: pos[2],
            leds: app.lights.get(&g.uid),
            online: g.online,
            selected: app.selection.contains(&g.uid),
            label,
        });
    }
    out
}

pub fn apply_arrangement(app: &mut App, arr: Arrangement) {
    let gifts = stage_gifts(app);
    // Arrange in slot order so spatial effects run left→right.
    let mut order: Vec<usize> = (0..gifts.len()).collect();
    let snap = app.snap.clone();
    order.sort_by_key(|&i| snap.gifts[i].slot.unwrap_or(u8::MAX));
    let sorted: Vec<StageGift> = order.iter().map(|&i| gifts[i].clone()).collect();
    let pos = Stage::auto_layout_with_yaw(&sorted, arr);
    for (k, &i) in order.iter().enumerate() {
        let key = snap.gifts[i].uid.to_hex();
        let (_, p, yaw) = pos[k];
        app.persist.stage_positions.insert(key, [p[0], p[1], yaw]);
    }
    let g = stage_gifts(app);
    app.stage.frame_all(&g);
}

/// Assign slots by x position (left→right).
pub fn number_left_to_right(app: &mut App) {
    let mut v: Vec<(f32, Uid)> = app
        .snap
        .gifts
        .iter()
        .filter(|g| g.online)
        .map(|g| (app.persist.stage_positions.get(&g.uid.to_hex()).map(|p| p[0]).unwrap_or(0.0), g.uid))
        .collect();
    v.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
    for (i, (_, u)) in v.iter().enumerate() {
        app.set_slot(*u, Some(i as u8));
    }
    app.toasts.info(format!("Numbering {} gifts left→right", v.len()));
}

pub fn page(app: &mut App, ui: &mut Ui) {
    let typing = ui.ctx().egui_wants_keyboard_input();
    // Set order owns the keys (digits = positions) while it runs.
    let ordering = app.stage_page.order.is_some();
    if ordering && !typing && !app.palette.is_open() {
        order_keys(app, ui);
    }
    if !ordering && !typing && !app.palette.is_open() {
        if ui.input_mut(|i| i.consume_key(Modifiers::NONE, Key::A)) {
            app.stage_page.arrange = !app.stage_page.arrange;
            if app.stage_page.arrange {
                app.stage.set_preset(CameraPreset::Top);
                app.stage_page.preset = 1;
            }
        }
        for (k, i) in [(Key::Num1, 0usize), (Key::Num2, 1), (Key::Num3, 2)] {
            if ui.input_mut(|inp| inp.consume_key(Modifiers::NONE, k)) {
                app.stage.set_preset([CameraPreset::Front, CameraPreset::Top, CameraPreset::ThreeQuarter][i]);
                app.stage_page.preset = i;
            }
        }
        if ui.input_mut(|i| i.consume_key(Modifiers::NONE, Key::H)) {
            app.persist.stage_labels = !app.persist.stage_labels;
        }
        if ui.input_mut(|i| i.consume_key(Modifiers::NONE, Key::F)) {
            let g = stage_gifts(app);
            app.stage.frame_all(&g);
        }
        // Gift actions, as on the Fleet page.
        if ui.input_mut(|i| i.consume_key(Modifiers::SHIFT, Key::B)) {
            blink_in_sequence(app);
        }
        if ui.input_mut(|i| i.consume_key(Modifiers::NONE, Key::B)) {
            let s: Vec<Uid> = app.selection.iter().copied().collect();
            if s.is_empty() {
                app.toasts.info("Click gifts to select them, or Shift+B to blink all in sequence");
            }
            app.blink(s);
        }
        if ui.input_mut(|i| i.consume_key(Modifiers::NONE, Key::S))
            && let Some(u) = app.focus
        {
            app.slot_popup = Some(u);
            app.persist.inspector_open = true;
        }
        if ui.input_mut(|i| i.consume_key(Modifiers::COMMAND, Key::A)) {
            app.selection = app.snap.gifts.iter().map(|g| g.uid).collect();
        }
        if ui.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Escape)) {
            app.selection.clear();
            app.focus = None;
        }
    }
    let ctx = ui.ctx().clone();
    App::page_header(ui, "Stage", |ui| {
        widgets::text(ui, "Living room", "body", TEXT_MUTED);
        ui.add_space(S5);
        let mut mode = app.stage_page.arrange as usize;
        if widgets::segmented(ui, "stage-mode", &mut mode, &["View", "Arrange"]) {
            app.stage_page.arrange = mode == 1;
            if mode == 1 {
                app.stage.set_preset(CameraPreset::Top);
                app.stage_page.preset = 1;
            }
        }
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            ui.spacing_mut().item_spacing.x = 6.0;
            if widgets::secondary_button(ui, Some(icons::FULLSCREEN), "Perform", true)
                .on_hover_ui(|ui| widgets::tooltip_with_keys(ui, "Performance mode", Some("F11")))
                .clicked()
            {
                app.set_performance(&ctx, true);
            }
            if widgets::ghost_button_sized(ui, icons::TAG, "Toggle labels", Some("H"), CONTROL_H, app.persist.stage_labels).clicked() {
                app.persist.stage_labels = !app.persist.stage_labels;
            }
            if widgets::ghost_button(ui, icons::CROSSHAIR, "Frame all", Some("F")).clicked() {
                let g = stage_gifts(app);
                app.stage.frame_all(&g);
            }
            let can_order = app.connected() && app.snap.gifts.iter().filter(|g| g.online).count() >= 2;
            if widgets::ghost_button_sized(ui, icons::LIST_NUMBERS, "Set order: say where each blinking gift stands in the room", None, CONTROL_H, app.stage_page.order.is_some()).clicked() && can_order {
                start_order(app);
            }
            let seq_tip = if app.blink_seq.is_some() {
                "Stop the blink sequence"
            } else if app.selection.is_empty() {
                "Blink all gifts one after the other, left to right"
            } else {
                "Blink the selected gifts one after the other, left to right"
            };
            if widgets::ghost_button_sized(ui, icons::LIGHTNING, seq_tip, Some("Shift+B"), CONTROL_H, app.blink_seq.is_some()).clicked() {
                blink_in_sequence(app);
            }
            ui.add_space(8.0);
            let mut cam = usize::MAX;
            let mut shown = app.stage_page.preset;
            if widgets::segmented(ui, "cam", &mut shown, &["Front", "Top", "¾"]) {
                cam = shown;
            }
            app.stage_page.preset = shown;
            widgets::text(ui, "Camera", "small", TEXT_FAINT);
            if cam != usize::MAX {
                app.stage.set_preset([CameraPreset::Front, CameraPreset::Top, CameraPreset::ThreeQuarter][cam]);
            }
        });
    });

    if app.snap.gifts.is_empty() {
        let act = widgets::empty_state(
            ui,
            &EmptyState {
                headline: "Your stage is empty",
                body: "Gifts appear here as soon as they are discovered.",
                primary: if app.is_sim() { None } else { Some(("Start simulator", Some(icons::GIFT))) },
                secondary: None,
                big: false,
            },
            |_| {},
        );
        if let EmptyAction::Primary = act {
            app.start_sim(app.persist.sim_count);
        }
        return;
    }

    let rect = ui.available_rect_before_wrap();
    let gifts = stage_gifts(app);
    let opts = StageOptions {
        labels: app.persist.stage_labels,
        edit_layout: app.stage_page.arrange,
        show_floor_grid: true,
        bloom: 1.0,
        ..Default::default()
    };
    if app.stage_page.framed_n != SKIP_AUTO_FRAME && app.stage_page.framed_n != gifts.len() {
        app.stage_page.framed_n = gifts.len();
        app.stage.frame_all(&gifts);
    }
    let mut child = ui.new_child(egui::UiBuilder::new().max_rect(rect));
    let resp = app.stage.show(&mut child, &gifts, &opts);
    for (id, pos) in &resp.moved {
        if let Some(g) = app.snap.gifts.iter().find(|g| uid_id(&g.uid) == *id) {
            let key = g.uid.to_hex();
            let yaw = app.persist.stage_positions.get(&key).map(|p| p[2]).unwrap_or(0.0);
            app.persist.stage_positions.insert(key, [pos[0], pos[1], yaw]);
        }
    }
    if resp.clicked_background {
        app.selection.clear();
        app.focus = None;
    } else if let Some(id) = resp.clicked
        && let Some(g) = app.snap.gifts.iter().find(|g| uid_id(&g.uid) == id)
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
    }

    if app.stage_page.order.is_some() {
        order_panel(app, ui, rect);
    }

    // Blink sequence progress (top-centre).
    if let Some(seq) = &app.blink_seq
        && let Some((uid, n)) = seq.current
    {
        let txt = format!("{}  Blinking {n} / {} · {}", icons::LIGHTNING, seq.total, app.gift_name(&uid));
        ui.painter().text(
            pos2(rect.center().x, rect.top() + 14.0),
            Align2::CENTER_TOP,
            txt,
            theme::font("small_medium"),
            ACCENT_TEXT,
        );
    }

    // Floating arrange panel (top-left).
    if app.stage_page.arrange {
        let pr = Rect::from_min_size(rect.min + vec2(12.0, 12.0), vec2(176.0, 248.0));
        floating(ui, pr, |ui| {
            widgets::overline(ui, "Arrange");
            ui.add_space(4.0);
            ui.add(egui::Label::new(widgets::rich("Drag boxes to move (snaps to 5 cm, Shift places freely).", "small", TEXT_FAINT)).wrap());
            ui.add_space(8.0);
            egui::Grid::new("arr").spacing(vec2(6.0, 6.0)).show(ui, |ui| {
                for (k, (n, a)) in [("Row", Arrangement::Row), ("Grid", Arrangement::Grid), ("Arc", Arrangement::Arc), ("Circle", Arrangement::Circle), ("Pyramid", Arrangement::Pyramid)].iter().enumerate() {
                    let (r, resp) = ui.allocate_exact_size(vec2(72.0, 28.0), Sense::click());
                    let p = ui.painter();
                    p.rect_filled(r, CornerRadius::same(R_SM), if resp.hovered() { HOVER } else { SURFACE_3 });
                    p.text(r.center(), Align2::CENTER_CENTER, *n, theme::font("button"), TEXT_SECONDARY);
                    if resp.clicked() {
                        apply_arrangement(app, *a);
                    }
                    if k % 2 == 1 {
                        ui.end_row();
                    }
                }
            });
            ui.add_space(8.0);
            if widgets::secondary_button(ui, Some(icons::LIST_NUMBERS), "Number L→R", app.connected()).clicked() {
                number_left_to_right(app);
            }
        });
    }

    // Mini transport (bottom centre) when a show is loaded.
    if app.studio.has_track() {
        let w = 460.0f32.min(rect.width() - 24.0);
        let tr = Rect::from_min_size(pos2(rect.center().x - w / 2.0, rect.bottom() - 12.0 - 48.0), vec2(w, 48.0));
        floating(ui, tr, |ui| {
            ui.horizontal_centered(|ui| {
                let playing = app.studio.is_playing();
                let (r, resp) = ui.allocate_exact_size(vec2(28.0, 28.0), Sense::click());
                ui.painter().circle_filled(r.center(), 14.0, if resp.hovered() { TEXT_SECONDARY } else { TEXT });
                widgets::paint_icon_fill(ui.painter(), r.center(), if playing { icons::PAUSE } else { icons::PLAY }, 14.0, BG);
                if resp.clicked() {
                    app.studio.toggle_play();
                }
                let (pos, dur) = app.studio.position_duration();
                widgets::text(ui, fmt_timecode(pos), "value", TEXT);
                let (br, bresp) = ui.allocate_exact_size(vec2(ui.available_width() - 140.0, 20.0), Sense::click_and_drag());
                let track = Rect::from_center_size(br.center(), vec2(br.width(), 4.0));
                widgets::progress_bar(ui.painter(), track, (pos / dur.max(0.001)) as f32, TEXT_MUTED);
                let hx = track.left() + track.width() * (pos / dur.max(0.001)) as f32;
                ui.painter().circle_filled(pos2(hx, track.center().y), 6.0, TEXT);
                if let Some(pp) = bresp.interact_pointer_pos()
                    && (bresp.dragged() || bresp.clicked()) {
                        let f = ((pp.x - track.left()) / track.width()).clamp(0.0, 1.0) as f64;
                        app.studio.seek(f * dur);
                    }
                let title = app.studio.title();
                widgets::text(ui, widgets::truncate(ui, &title, &theme::font("small"), 120.0), "small", TEXT_MUTED);
            });
        });
    }
    if app.studio.is_playing() || app.lights.animating {
        ui.ctx().request_repaint();
    }
}

/// Floating panel: SURFACE_2 at 92%, R_LG, level-3 shadow.
pub fn floating(ui: &mut Ui, rect: Rect, add: impl FnOnce(&mut Ui)) {
    let p = ui.painter();
    p.add(theme::shadow_popover().as_shape(rect, CornerRadius::same(R_LG)));
    p.rect_filled(rect, CornerRadius::same(R_LG), theme::alpha(SURFACE_2, 0.92));
    p.rect_stroke(rect, CornerRadius::same(R_LG), Stroke::new(1.0, BORDER), StrokeKind::Inside);
    widgets::top_highlight(p, rect, R_LG as f32);
    let mut child = ui.new_child(egui::UiBuilder::new().max_rect(rect.shrink(12.0)).layout(Layout::top_down(Align::Min)));
    add(&mut child);
}

// -- Set order ------------------------------------------------------------------

/// Start "Set order" with the online gifts (or stop it if it runs).
pub fn start_order(app: &mut App) {
    if app.stage_page.order.take().is_some() {
        return;
    }
    app.blink_seq = None;
    let mut gifts: Vec<(Option<u8>, Uid)> = app.snap.gifts.iter().filter(|g| g.online).map(|g| (g.slot, g.uid)).collect();
    gifts.sort_by_key(|g| (g.0.is_none(), g.0, g.1));
    if gifts.len() < 2 {
        app.toasts.info("Set order needs at least two gifts online");
        return;
    }
    app.stage_page.order = Some(OrderWizard::new(gifts.into_iter().map(|g| g.1).collect()));
}

fn order_keys(app: &mut App, ui: &mut Ui) {
    let Some(w) = &mut app.stage_page.order else { return };
    let n = w.placed.len();
    let digits = [Key::Num1, Key::Num2, Key::Num3, Key::Num4, Key::Num5, Key::Num6, Key::Num7, Key::Num8, Key::Num9, Key::Num0];
    for (i, k) in digits.into_iter().enumerate() {
        if i < n && ui.input_mut(|inp| inp.consume_key(Modifiers::NONE, k)) {
            w.place(i);
        }
    }
    if ui.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Backspace)) {
        w.back();
    }
    if ui.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Escape)) {
        app.stage_page.order = None;
        return;
    }
    if w.done() && ui.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Enter)) {
        save_order(app);
    }
}

/// Slots 0..n in room order, and the stage boxes moved to match: the
/// existing positions, left to right, go to the gifts in room order (so a
/// curved layout keeps its shape).
fn save_order(app: &mut App) {
    let Some(w) = app.stage_page.order.take() else { return };
    let order: Vec<Uid> = w.placed.into_iter().flatten().collect();
    let mut spots: Vec<[f32; 3]> = order
        .iter()
        .map(|u| app.persist.stage_positions.get(&u.to_hex()).copied().unwrap_or([0.0; 3]))
        .collect();
    spots.sort_by(|a, b| a[0].total_cmp(&b[0]).then(a[1].total_cmp(&b[1])));
    if spots.windows(2).all(|p| (p[1][0] - p[0][0]).abs() < 1e-3 && (p[1][1] - p[0][1]).abs() < 1e-3) {
        // No usable layout yet: a centred row.
        let n = spots.len() as f32;
        for (i, s) in spots.iter_mut().enumerate() {
            *s = [(i as f32 - (n - 1.0) / 2.0) * SPACING, 0.0, 0.0];
        }
    }
    for (u, s) in order.iter().zip(spots) {
        app.persist.stage_positions.insert(u.to_hex(), s);
    }
    if let Some(link) = app.link().cloned() {
        let p = link.assign_slots_in_order(order.clone());
        app.push_op(
            "Set order",
            format!("Slots 1–{} follow the room, left to right", order.len()),
            crate::app::OpKind::Report(p),
        );
    }
    let g = stage_gifts(app);
    app.stage.frame_all(&g);
}

/// The floating Set order panel (bottom-centre of the stage), and the
/// blinking of the gift being placed.
fn order_panel(app: &mut App, ui: &mut Ui, stage: Rect) {
    let Some(w) = &mut app.stage_page.order else { return };
    // Keep the current gift blinking (3 × 300 ms, re-sent every second).
    if let Some(u) = w.current()
        && Instant::now() >= w.next_blink
    {
        w.next_blink = Instant::now() + std::time::Duration::from_millis(1000);
        if let Some(link) = app.backend.as_ref().map(|b| b.link.clone()) {
            let _ = link.blink(lumigift_core::Target::Gifts(vec![u]), app.blink_color, 3, std::time::Duration::from_millis(300));
        }
    }
    ui.ctx().request_repaint_after(std::time::Duration::from_millis(100));

    let n = w.placed.len();
    let current = w.current();
    let placed_n = n - w.queue.len();
    let placed = w.placed.clone();
    let can_back = !w.history.is_empty();
    let can_skip = w.queue.len() > 1;
    let done = w.done();
    let names: Vec<Option<String>> = placed.iter().map(|p| p.map(|u| app.gift_name(&u))).collect();
    let cur_name = current.map(|u| app.gift_name(&u));

    let width = (n as f32 * 64.0 + 40.0).clamp(420.0, stage.width() - 40.0);
    let mut action: Option<(&str, usize)> = None;
    egui::Area::new(egui::Id::unique("set-order"))
        .fixed_pos(pos2(stage.center().x - width / 2.0, stage.bottom() - 200.0))
        .order(egui::Order::Foreground)
        .show(ui.ctx(), |ui| {
            egui::Frame::new()
                .fill(SURFACE_2)
                .stroke(Stroke::new(1.0, BORDER_STRONG))
                .corner_radius(CornerRadius::same(R_MD))
                .inner_margin(egui::Margin::same(16))
                .show(ui, |ui| {
                    ui.set_width(width - 32.0);
                    ui.horizontal(|ui| {
                        widgets::text(ui, "Set order", "title", TEXT);
                        widgets::text(ui, format!("{placed_n} / {n} placed"), "value_small", TEXT_FAINT);
                    });
                    ui.add_space(4.0);
                    let line = match &cur_name {
                        Some(name) => format!("{}  {name} is blinking. Where does it stand, counting from the left?", icons::LIGHTNING),
                        None => "All gifts placed. Save to assign the slots in this order.".to_string(),
                    };
                    widgets::text(ui, line, "body", if done { TEXT_SECONDARY } else { ACCENT_TEXT });
                    ui.add_space(10.0);
                    ui.horizontal_wrapped(|ui| {
                        ui.spacing_mut().item_spacing = vec2(6.0, 6.0);
                        for (i, name) in names.iter().enumerate() {
                            let key = if i < 9 { format!("{}", i + 1) } else if i == 9 { "0".into() } else { String::new() };
                            let (r, resp) = ui.allocate_exact_size(vec2(58.0, 46.0), if name.is_none() && !done { Sense::click() } else { Sense::hover() });
                            let p = ui.painter();
                            let hov = resp.hovered() && name.is_none();
                            p.rect_filled(r, CornerRadius::same(R_SM), if name.is_some() { ACCENT_SOFT } else if hov { HOVER } else { SURFACE_3 });
                            p.rect_stroke(r, CornerRadius::same(R_SM), Stroke::new(1.0, if hov { ACCENT } else { BORDER_SUBTLE }), StrokeKind::Inside);
                            p.text(pos2(r.center().x, r.top() + 15.0), Align2::CENTER_CENTER, format!("{}", i + 1), theme::font("title"), if name.is_some() { ACCENT_TEXT } else { TEXT });
                            let sub = name.clone().map(|s| s.trim_start_matches("Gift ").chars().take(8).collect::<String>()).unwrap_or(key);
                            p.text(pos2(r.center().x, r.bottom() - 11.0), Align2::CENTER_CENTER, sub, theme::font("small"), TEXT_FAINT);
                            if resp.clicked() {
                                action = Some(("place", i));
                            }
                        }
                    });
                    ui.add_space(12.0);
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = 8.0;
                        if widgets::secondary_button(ui, None, "Back", can_back).on_hover_text("Undo the last placement (Backspace)").clicked() {
                            action = Some(("back", 0));
                        }
                        if !done && widgets::secondary_button(ui, None, "Skip", can_skip).on_hover_text("Place this gift later").clicked() {
                            action = Some(("skip", 0));
                        }
                        if widgets::secondary_button(ui, None, "Cancel", true).on_hover_text("Esc").clicked() {
                            action = Some(("cancel", 0));
                        }
                        if done && widgets::primary_button(ui, None, "Save order", true).on_hover_text("Enter").clicked() {
                            action = Some(("save", 0));
                        }
                    });
                });
        });
    match action {
        Some(("place", i)) => { if let Some(w) = &mut app.stage_page.order { w.place(i) } }
        Some(("back", _)) => { if let Some(w) = &mut app.stage_page.order { w.back() } }
        Some(("skip", _)) => { if let Some(w) = &mut app.stage_page.order { w.skip() } }
        Some(("cancel", _)) => app.stage_page.order = None,
        Some(("save", _)) => save_order(app),
        _ => {}
    }
}

/// Blink the selected gifts (or all) one after the other, ordered left to
/// right on the stage (then front to back), to match the boxes in the room.
/// Stops a running sequence.
pub fn blink_in_sequence(app: &mut App) {
    if app.blink_seq.is_some() {
        app.blink_sequence(Vec::new());
        return;
    }
    let mut uids: Vec<Uid> = if app.selection.is_empty() {
        app.snap.gifts.iter().filter(|g| g.online).map(|g| g.uid).collect()
    } else {
        app.selection.iter().copied().collect()
    };
    let pos = |u: &Uid| app.persist.stage_positions.get(&u.to_hex()).map(|p| (p[0], p[1])).unwrap_or((f32::MAX, 0.0));
    uids.sort_by(|a, b| {
        let (pa, pb) = (pos(a), pos(b));
        pa.0.total_cmp(&pb.0).then(pa.1.total_cmp(&pb.1))
    });
    app.blink_sequence(uids);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gifts_of_one_batch_get_distinct_stage_ids() {
        // Same wafer and lot (bytes 4..12), different die positions: the
        // three gifts that all selected as the first one.
        let lot = [0x0b, 0x38, 0x31, 0x32, 0x30, 0x57, 0x4e, 0x33];
        let uid = |x: u8, y: u8| {
            let mut u = [0u8; 12];
            u[..4].copy_from_slice(&[x, 0, y, 0]);
            u[4..].copy_from_slice(&lot);
            Uid(u)
        };
        let ids: std::collections::BTreeSet<u64> = [uid(0x30, 0xb4), uid(0x2f, 0xb4), uid(0x30, 0xb5)]
            .iter()
            .map(uid_id)
            .collect();
        assert_eq!(ids.len(), 3);
    }
}
