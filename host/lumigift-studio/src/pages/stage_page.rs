//! Stage page: full-bleed stage view with floating arrange tools and a mini
//! transport (docs/UI_DESIGN.md §3.2 Stage, §5.6).

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
}

/// Marker: the camera was restored from settings, don't auto-frame.
pub const SKIP_AUTO_FRAME: usize = usize::MAX - 1;

impl Default for StagePage {
    fn default() -> Self {
        Self { arrange: false, preset: 2, framed_n: usize::MAX, preview_framed_n: usize::MAX }
    }
}

fn uid_id(u: &Uid) -> u64 {
    u64::from_le_bytes(u.0[4..12].try_into().unwrap())
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
    if !typing && !app.palette.is_open() {
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
    if let Some(id) = resp.clicked {
        if id == u64::MAX {
            app.selection.clear();
            app.focus = None;
        } else if let Some(g) = app.snap.gifts.iter().find(|g| uid_id(&g.uid) == id) {
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
