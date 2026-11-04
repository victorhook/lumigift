//! Fleet page: gift tiles / table, event log drawer and the gift inspector
//! (docs/UI_DESIGN.md §3.2 Fleet, §4.1, §4.2).

use std::time::{Duration, Instant};

use egui::{
    Align, Align2, Color32, CornerRadius, Key, Layout, Modifiers, Rect, Sense, Shadow, Stroke,
    StrokeKind, Ui, pos2, vec2,
};
use lumigift_core::{GiftView, LogLevel, Uid, config_key};
use lumigift_proto::{MAX_SLOTS, status_flags};

use crate::app::{App, Confirm, OpKind, Page, default_name, level_icon};
use crate::health::{self, Health, Level, State, fmt_fw, fmt_fw_short, fmt_uptime};
use crate::icons;
use crate::theme::{self, *};
use crate::widgets::{self, GemState, SparkOpts};

pub const SORTS: [&str; 5] = ["Slot", "Name", "Health", "Battery", "Link"];

#[derive(Default)]
pub struct Fleet {
    pub filter: String,
    pub attention_only: bool,
    pub ripple_at: Option<Instant>,
    pub log_level: usize,
    pub log_gift: Option<Uid>,
    pub log_query: String,
    order: Vec<Uid>,
    order_sort: usize,
    resort: bool,
    last_click: Option<Uid>,
    focus_filter: bool,
    cfg_brightness: Option<(Uid, u16)>,
}

fn tile_status(h: &Health) -> (Option<&'static str>, String, Color32, Color32) {
    match &h.state {
        State::Online => (None, "Online".into(), TEXT_MUTED, Color32::TRANSPARENT),
        State::Attention(r) => (Some(icons::WARNING), r.clone(), WARN, WARN_SOFT),
        State::Critical(r) => (Some(icons::WARNING_CIRCLE), r.clone(), ERROR, ERROR_SOFT),
        State::Reconnecting => (Some(icons::SPINNER), "Reconnecting…".into(), TEXT_MUTED, SURFACE_3),
        State::Offline(d) => (
            Some(icons::CIRCLE),
            match d {
                Some(d) => format!("Offline {}", health::fmt_ago(*d).replace(" ago", "")),
                None => "Offline".into(),
            },
            TEXT_FAINT,
            SURFACE_3,
        ),
        State::Asleep => (Some(icons::MOON), "Asleep".into(), TEXT_MUTED, SURFACE_3),
        State::Updating(ph) => (Some(icons::DOWNLOAD), ph.clone(), INFO, INFO_SOFT),
    }
}

/// The status as plain text (online) or a pill (everything else).
fn status_badge(ui: &mut Ui, h: &Health) {
    let (icon, label, fg, bg) = tile_status(h);
    if bg == Color32::TRANSPARENT {
        widgets::text(ui, label, "small", fg);
    } else {
        widgets::pill(ui, icon.filter(|i| *i != icons::SPINNER), &label, fg, bg);
    }
}

/// The last `n` link-quality samples (NaN while offline).
fn link_samples(g: &GiftView, n: usize) -> Vec<f32> {
    let skip = g.history.len().saturating_sub(n);
    g.history.iter().skip(skip).map(|s| if s.online { s.link_pct } else { f32::NAN }).collect()
}

/// Sparkline colour for the latest link sample: red < 70 %, amber < 90 %.
fn link_color(samples: &[f32]) -> Color32 {
    match samples.last().copied() {
        Some(cur) if cur < 70.0 => ERROR,
        Some(cur) if cur < 90.0 => WARN,
        _ => VIZ_LEVEL,
    }
}

impl Fleet {
    fn sorted(&mut self, app_gifts: &[GiftView], health: &std::collections::HashMap<Uid, Health>, sort: usize) -> Vec<Uid> {
        // Stable order: only re-sort on explicit request or when the set of gifts changes.
        let same_set = self.order.len() == app_gifts.len() && app_gifts.iter().all(|g| self.order.contains(&g.uid));
        if same_set && !self.resort && self.order_sort == sort {
            return self.order.clone();
        }
        let mut v: Vec<&GiftView> = app_gifts.iter().collect();
        let batt = |g: &GiftView| health.get(&g.uid).and_then(|h| h.vbat_median_mv).unwrap_or(9999.0);
        match sort {
            1 => v.sort_by_key(|g| g.meta.name.clone().unwrap_or_else(|| default_name(&g.uid)).to_lowercase()),
            2 => v.sort_by_key(|g| health.get(&g.uid).map(|h| h.state.rank()).unwrap_or(9)),
            3 => v.sort_by(|a, b| batt(a).partial_cmp(&batt(b)).unwrap_or(std::cmp::Ordering::Equal)),
            4 => v.sort_by(|a, b| a.poll_success_pct.partial_cmp(&b.poll_success_pct).unwrap_or(std::cmp::Ordering::Equal)),
            _ => v.sort_by_key(|g| (g.slot.unwrap_or(u8::MAX), g.uid)),
        }
        self.order = v.iter().map(|g| g.uid).collect();
        self.order_sort = sort;
        self.resort = false;
        self.order.clone()
    }
}

fn matches_filter(app: &App, g: &GiftView, f: &str, attention: bool) -> bool {
    if attention && !app.health.get(&g.uid).is_some_and(|h| h.state.needs_attention()) {
        return false;
    }
    if f.is_empty() {
        return true;
    }
    let f = f.to_lowercase();
    let name = app.gift_name(&g.uid).to_lowercase();
    let slot = g.slot.map(|s| format!("#{:02} {}", s + 1, s + 1)).unwrap_or_default();
    name.contains(&f) || slot.contains(&f) || g.uid.to_hex().contains(&f)
}

pub fn page(app: &mut App, ui: &mut Ui) {
    let total = app.snap.gifts.len();
    let online = app.snap.online_count();
    let asleep = app.snap.asleep_count();
    let attention = app.health.values().filter(|h| h.state.needs_attention()).count();

    // Keyboard (only when no text field has focus).
    let typing = ui.ctx().egui_wants_keyboard_input();
    if !typing && !app.palette.is_open() {
        if ui.input_mut(|i| i.consume_key(Modifiers::COMMAND, Key::R)) {
            app.rescan();
        }
        if ui.input_mut(|i| i.consume_key(Modifiers::COMMAND, Key::A)) {
            app.selection = app.snap.gifts.iter().map(|g| g.uid).collect();
        }
        if ui.input_mut(|i| i.consume_key(Modifiers::NONE, Key::B)) {
            let s: Vec<Uid> = app.selection.iter().copied().collect();
            app.blink(s);
        }
        if ui.input_mut(|i| i.consume_key(Modifiers::NONE, Key::F2))
            && let Some(u) = app.focus {
                app.renaming = Some((u, app.gift_name(&u)));
                app.persist.inspector_open = true;
            }
        if ui.input_mut(|i| i.consume_key(Modifiers::NONE, Key::S))
            && let Some(u) = app.focus {
                app.slot_popup = Some(u);
                app.persist.inspector_open = true;
            }
        if ui.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Slash)) {
            app.fleet.focus_filter = true;
        }
        if ui.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Enter)) {
            app.persist.inspector_open = true;
        }
        if ui.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Escape)) {
            app.selection.clear();
            app.focus = None;
        }
    }

    App::page_header(ui, "Fleet", |ui| {
        ui.spacing_mut().item_spacing.x = 6.0;
        let summary = if asleep > 0 { format!("{total} gifts · {online} online · {asleep} asleep") } else { format!("{total} gifts · {online} online") };
        widgets::text(ui, summary, "body", TEXT_MUTED);
        if attention > 0 {
            widgets::text(ui, "·", "body", TEXT_MUTED);
            let r = widgets::text_button(ui, &format!("{attention} need{} attention", if attention == 1 { "s" } else { "" }), WARN);
            if r.clicked() {
                app.fleet.attention_only = !app.fleet.attention_only;
            }
        }
        if app.fleet.attention_only {
            widgets::chip(ui, "Attention only", WARN, WARN_SOFT);
        }
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            ui.spacing_mut().item_spacing.x = 8.0;
            let sweeping = app.snap.discovery.sweeping;
            if sweeping {
                if widgets::secondary_button(ui, Some(icons::STOP), "Stop", true).clicked() {
                    // The core has no cancel for a sweep; it finishes in a second or two.
                    app.toasts.info("The sweep finishes on its own in a moment");
                }
            } else if widgets::secondary_button(ui, Some(icons::REBOOT), "Rescan", app.connected())
                .on_hover_ui(|ui| widgets::tooltip_with_keys(ui, "Sweep all addresses for gifts", Some("Ctrl+R")))
                .clicked()
            {
                app.rescan();
            }
            // Doze: "Wake all" while any gift sleeps (or a wake runs), else "Sleep all".
            let busy = app.link().is_some_and(|l| l.ota_running());
            if app.snap.waking {
                let _ = widgets::secondary_button(ui, Some(icons::SPINNER), "Waking…", false);
            } else if asleep > 0 {
                if widgets::secondary_button(ui, Some(icons::SUN), "Wake all", app.connected() && !busy)
                    .on_hover_text("Wake every dozing gift (takes about 1.5 s)")
                    .clicked()
                {
                    app.wake_all();
                }
            } else if widgets::secondary_button(ui, Some(icons::MOON), "Sleep all", app.connected() && online > 0 && !busy)
                .on_hover_text("Put every gift to sleep to save batteries: LEDs off, radio mostly off")
                .clicked()
            {
                app.sleep_all();
            }
            let mut view = app.persist.fleet_list_view as usize;
            if widgets::segmented(ui, "fleet-view", &mut view, &[icons::GRID, icons::ROWS]) {
                app.persist.fleet_list_view = view == 1;
            }
            let sort = app.persist.fleet_sort;
            egui::ComboBox::from_id_salt("fleet-sort")
                .selected_text(widgets::rich(format!("Sort: {}", SORTS[sort]), "button", TEXT_SECONDARY))
                .width(120.0)
                .show_ui(ui, |ui| {
                    for (i, s) in SORTS.iter().enumerate() {
                        if ui.selectable_label(sort == i, *s).clicked() {
                            app.persist.fleet_sort = i;
                            app.fleet.resort = true;
                        }
                    }
                });
            let te = egui::TextEdit::singleline(&mut app.fleet.filter)
                .hint_text(widgets::rich(format!("{}  Filter", icons::SEARCH), "body", TEXT_FAINT))
                .desired_width(if app.compact { 110.0 } else { 160.0 })
                .margin(vec2(8.0, 5.0));
            let r = ui.add(te);
            if app.fleet.focus_filter {
                r.request_focus();
                app.fleet.focus_filter = false;
            }
        });
    });
    // Discovery progress line.
    if app.snap.discovery.sweeping {
        let r = ui.min_rect();
        let y = r.top() + PAGE_HEADER_H - 2.0;
        let w = ui.available_width();
        let left = ui.max_rect().left();
        let frac = app.snap.discovery.sweep_progress;
        ui.painter().rect_filled(
            Rect::from_min_size(pos2(left, y), vec2(w * frac, 2.0)),
            CornerRadius::ZERO,
            INFO,
        );
    }

    // Event drawer at the bottom.
    let drawer_open = app.persist.event_log_open;
    egui::Panel::bottom("event-drawer")
        .exact_size(if drawer_open { 220.0 } else { 34.0 })
        .resizable(false)
        .show_separator_line(false)
        .frame(egui::Frame::new().fill(BG))
        .show(ui, |ui| event_drawer(app, ui));

    if app.snap.gifts.is_empty() {
        looking_for_gifts(app, ui);
        return;
    }

    let unassigned = app.snap.gifts.iter().filter(|g| g.online && g.slot.is_none()).count();
    if unassigned > 0 && app.connected() {
        ui.add_space(16.0);
        ui.horizontal(|ui| {
            ui.add_space(S7);
            egui::Frame::new().fill(WARN_SOFT).corner_radius(CornerRadius::same(R_SM)).inner_margin(egui::Margin::symmetric(12, 6)).show(ui, |ui| {
                ui.set_width(ui.available_width() - S7 - 24.0);
                ui.horizontal(|ui| {
                    widgets::text(ui, icons::TAG, "body", WARN);
                    let s = if unassigned == 1 { "1 gift has no show slot and won't take part in the show.".to_string() } else { format!("{unassigned} gifts have no show slot and won't take part in the show.") };
                    widgets::text(ui, s, "small", TEXT_SECONDARY);
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if widgets::secondary_button_h(ui, Some(icons::LIST_NUMBERS), "Auto-assign slots", true, 24.0).clicked() {
                            app.auto_assign();
                        }
                    });
                });
            });
        });
    }
    let list = app.persist.fleet_list_view;
    if !list && total > 20 && !app.persist.list_hint_dismissed {
        ui.add_space(12.0);
        ui.horizontal(|ui| {
            ui.add_space(S7);
            egui::Frame::new().fill(INFO_SOFT).corner_radius(CornerRadius::same(R_SM)).inner_margin(egui::Margin::symmetric(12, 6)).show(ui, |ui| {
                ui.horizontal(|ui| {
                    widgets::text(ui, icons::INFO, "body", INFO);
                    widgets::text(ui, format!("{total} gifts: the list view shows them all at once."), "small", TEXT_SECONDARY);
                    if widgets::text_button(ui, "Switch to list", ACCENT_TEXT).clicked() {
                        app.persist.fleet_list_view = true;
                    }
                    if widgets::ghost_button_sized(ui, icons::X, "Dismiss", None, 20.0, false).clicked() {
                        app.persist.list_hint_dismissed = true;
                    }
                });
            });
        });
    }

    let sort = app.persist.fleet_sort;
    let gifts = app.snap.gifts.clone();
    let order = app.fleet.sorted(&gifts, &app.health, sort);
    let filter = app.fleet.filter.clone();
    let att = app.fleet.attention_only;
    let visible: Vec<Uid> = order
        .into_iter()
        .filter(|u| app.snap.gift(u).is_some_and(|g| matches_filter(app, g, &filter, att)))
        .collect();
    if visible.is_empty() {
        ui.add_space(S8);
        ui.vertical_centered(|ui| {
            let what = if filter.is_empty() { "No gifts need attention".to_string() } else { format!("No gifts match '{filter}'") };
            widgets::text(ui, what, "body", TEXT_MUTED);
            ui.add_space(8.0);
            if widgets::secondary_button(ui, Some(icons::X), "Clear filter", true).clicked() {
                app.fleet.filter.clear();
                app.fleet.attention_only = false;
            }
        });
        return;
    }
    if list {
        table(app, ui, &visible);
    } else {
        grid(app, ui, &visible);
    }
}

fn looking_for_gifts(app: &mut App, ui: &mut Ui) {
    let avail = ui.available_rect_before_wrap();
    let c = pos2(avail.center().x, avail.top() + avail.height() * 0.36);
    let t = ui.input(|i| i.time) as f32;
    let p = ui.painter();
    if !app.persist.reduce_motion {
        for k in 0..3 {
            let ph = ((t / 2.0) + k as f32 / 3.0).fract();
            p.circle_stroke(c, 20.0 + ph * 70.0, Stroke::new(1.5, theme::alpha(ACCENT, 0.3 * (1.0 - ph))));
        }
        ui.ctx().request_repaint();
    }
    p.circle_filled(c, 22.0, SURFACE_3);
    widgets::paint_icon(p, c, icons::BROADCAST, 22.0, ACCENT_TEXT);
    let mut ui2 = ui.new_child(egui::UiBuilder::new().max_rect(Rect::from_center_size(pos2(c.x, c.y + 150.0), vec2(440.0, 160.0))));
    ui2.vertical_centered(|ui| {
        widgets::text(ui, "Looking for gifts…", "title", TEXT);
        ui.add_space(6.0);
        widgets::text(ui, "Switch your gifts on and keep them within a few metres.", "body", TEXT_MUTED);
        ui.add_space(6.0);
        let d = &app.snap.discovery;
        let s = if d.sweeping { format!("Scanning addresses {} / 254", (d.sweep_progress * 254.0) as u32) } else { "Listening…".into() };
        widgets::text(ui, s, "value_small", TEXT_FAINT);
        ui.add_space(12.0);
        if !app.is_sim() && widgets::text_button(ui, "Use simulator instead", ACCENT_TEXT).clicked() {
            app.start_sim(app.persist.sim_count);
        }
    });
}

// ---------------------------------------------------------------------------
// Grid of tiles
// ---------------------------------------------------------------------------

fn grid(app: &mut App, ui: &mut Ui, visible: &[Uid]) {
    let compact = visible.len() > 16 || ui.ctx().content_rect().height() < 760.0;
    let tile_h = if compact { 112.0 } else { 176.0 };
    let avail_w = ui.available_width() - 2.0 * S7;
    let cols = (((avail_w + 16.0) / (TILE_W_MIN + 16.0)).floor() as usize).clamp(1, 6);
    let tile_w = (avail_w - (cols as f32 - 1.0) * 16.0) / cols as f32;

    // Arrow-key navigation across tiles.
    if !ui.ctx().egui_wants_keyboard_input() && !app.palette.is_open() {
        let cur = app.focus.and_then(|f| visible.iter().position(|u| *u == f));
        let mut next = None;
        ui.input(|i| {
            let n = visible.len() as isize;
            let c = cur.map(|c| c as isize);
            let step = |d: isize| Some(c.map(|c| (c + d).clamp(0, n - 1)).unwrap_or(0) as usize);
            if i.key_pressed(Key::ArrowRight) { next = step(1); }
            if i.key_pressed(Key::ArrowLeft) { next = step(-1); }
            if i.key_pressed(Key::ArrowDown) { next = step(cols as isize); }
            if i.key_pressed(Key::ArrowUp) { next = step(-(cols as isize)); }
        });
        if let Some(n) = next {
            app.focus = Some(visible[n]);
            app.selection = [visible[n]].into();
        }
    }

    egui::ScrollArea::vertical().auto_shrink(false).show(ui, |ui| {
        ui.add_space(S7);
        let rows = visible.len().div_ceil(cols);
        for r in 0..rows {
            ui.horizontal(|ui| {
                ui.add_space(S7);
                ui.spacing_mut().item_spacing.x = 16.0;
                for c in 0..cols {
                    let i = r * cols + c;
                    if i >= visible.len() {
                        break;
                    }
                    tile(app, ui, visible, visible[i], vec2(tile_w, tile_h), compact);
                }
            });
            ui.add_space(16.0 - ui.spacing().item_spacing.y);
        }
        ui.add_space(S5);
    });
}

fn tile(app: &mut App, ui: &mut Ui, visible: &[Uid], uid: Uid, size: egui::Vec2, compact: bool) {
    let Some(g) = app.snap.gift(&uid).cloned() else { return };
    let h = app.health.get(&uid).cloned().unwrap_or_else(|| Health {
        state: State::Online,
        link_level: Level::Normal,
        battery_level: Level::Normal,
        bcast_level: Level::Normal,
        vbat_median_mv: None,
        sentence: String::new(),
    });
    let (rect, resp) = ui.allocate_exact_size(size, Sense::click());
    if !ui.is_rect_visible(rect) {
        return;
    }
    let selected = app.selection.contains(&uid);
    // Asleep tiles dim like offline ones (but keep a solid border).
    let offline = h.state.is_dormant();
    let id = egui::Id::unique(("tile", uid));
    let hov_t = ui.ctx().animate_bool_with_time(id.with("h"), resp.hovered(), MOTION_FAST);
    let sel_t = ui.ctx().animate_bool_with_time(id.with("s"), selected, MOTION_BASE);

    // Discovered recently → fade + scale in.
    let backend_start = app.backend.as_ref().map(|b| b.started).unwrap_or_else(Instant::now);
    let is_new = g.first_seen > backend_start + Duration::from_secs(4) && g.first_seen.elapsed() < Duration::from_secs(10);
    let appear = if is_new {
        let a = (g.first_seen.elapsed().as_secs_f32() / 0.2).min(1.0);
        if a < 1.0 {
            ui.ctx().request_repaint();
        }
        egui::emath::easing::cubic_out(a)
    } else {
        1.0
    };
    let rect = Rect::from_center_size(rect.center(), rect.size() * (0.96 + 0.04 * appear));

    let p = ui.painter().clone();
    let fill = if offline {
        SURFACE_1
    } else if resp.is_pointer_button_down_on() {
        ACTIVE
    } else {
        theme::mix(SURFACE_2, HOVER, hov_t)
    };
    paint_tile_frame(&p, rect, &h.state, theme::alpha(fill, appear), hov_t, sel_t);
    if resp.has_focus() {
        widgets::focus_ring(&p, rect, R_MD);
    }

    let pad = 12.0;
    let inner = rect.shrink(pad);
    let gem_s = if compact { 32.0 } else { 44.0 };
    let gem_r = Rect::from_min_size(inner.min, vec2(gem_s, gem_s));
    let leds = app.lights.get(&uid);
    widgets::gem(&p, gem_r, &leds, if offline { GemState::Offline } else { GemState::Live });
    // Blink ripple.
    if g.flags & status_flags::BLINKING != 0
        && let Some(t0) = app.fleet.ripple_at {
            let a = t0.elapsed().as_secs_f32() / 0.3;
            if a < 1.0 && selected {
                let r = 22.0 + 18.0 * a;
                p.circle_stroke(gem_r.center(), r * gem_s / 44.0, Stroke::new(1.5, theme::alpha(ACCENT, 1.0 - a)));
                ui.ctx().request_repaint();
            }
        }

    // Name + slot chip.
    let name = app.gift_name(&uid);
    let text_x = gem_r.right() + 12.0;
    let slot_label = g.slot.map(health::fmt_slot);
    let chip_font = theme::font("value_small");
    let chip_w = slot_label.as_ref().map(|s| widgets::text_width(ui, s, chip_font.clone()) + 10.0).unwrap_or(widgets::text_width(ui, "No slot", chip_font.clone()) + 12.0);
    let chip_r = Rect::from_min_size(pos2(inner.right() - chip_w, inner.top()), vec2(chip_w, 18.0));
    match &slot_label {
        Some(s) => {
            p.rect_filled(chip_r, CornerRadius::same(R_XS), SURFACE_3);
            p.text(chip_r.center(), Align2::CENTER_CENTER, s, chip_font.clone(), TEXT_MUTED);
        }
        None => {
            widgets::dashed_rect(&p, chip_r, Stroke::new(1.0, theme::alpha(WARN, 0.8)), 3.0, 2.0);
            p.text(chip_r.center(), Align2::CENTER_CENTER, "No slot", chip_font.clone(), WARN);
        }
    }
    let name_font = if compact { theme::font("label") } else { theme::font("title") };
    let name_max = chip_r.left() - text_x - 8.0;
    let name_t = widgets::truncate(ui, &name, &name_font, name_max);
    let name_col = if offline { TEXT_MUTED } else { TEXT };
    p.text(pos2(text_x, inner.top() + if compact { 8.0 } else { 9.0 }), Align2::LEFT_CENTER, name_t, name_font, name_col);
    let mut badges_x = text_x;
    let status_y = inner.top() + if compact { 26.0 } else { 32.0 };
    // Status line.
    let (icon, label, fg, bg) = tile_status(&h);
    let small = theme::font("small_medium");
    if bg == Color32::TRANSPARENT {
        p.text(pos2(text_x, status_y), Align2::LEFT_CENTER, &label, theme::font("small"), fg);
        badges_x += widgets::text_width(ui, &label, theme::font("small")) + 8.0;
    } else {
        let lw = widgets::text_width(ui, &label, small.clone());
        let maxw = inner.right() - text_x;
        let lw = lw.min(maxw - 34.0);
        let label = widgets::truncate(ui, &label, &small, lw);
        let pr = Rect::from_min_size(pos2(text_x, status_y - 10.0), vec2(lw + 34.0, 20.0));
        if icon == Some(icons::SPINNER) {
            p.rect_filled(pr, CornerRadius::same(R_PILL), bg);
            widgets::paint_spinner(ui, pos2(pr.left() + 15.0, pr.center().y), 5.0, fg);
            p.text(pos2(pr.left() + 26.0, pr.center().y), Align2::LEFT_CENTER, label, small.clone(), fg);
        } else {
            widgets::paint_pill(&p, pr, icon, &label, fg, bg, small.clone());
        }
        badges_x = pr.right() + 6.0;
    }
    if app.is_sim() && !compact {
        let sr = Rect::from_min_size(pos2(badges_x, status_y - 9.0), vec2(32.0, 18.0));
        if sr.right() < inner.right() {
            p.rect_filled(sr, CornerRadius::same(R_XS), SURFACE_3);
            p.text(sr.center(), Align2::CENTER_CENTER, "SIM", theme::font("small"), TEXT_FAINT);
            badges_x = sr.right() + 6.0;
        }
    }
    if is_new {
        let nr = Rect::from_min_size(pos2(badges_x, status_y - 9.0), vec2(44.0, 18.0));
        if nr.right() < inner.right() {
            widgets::paint_pill(&p, nr, Some(icons::SPARKLE), "New", INFO, INFO_SOFT, theme::font("small"));
        }
    }

    let value_font = theme::font("value");
    let val_col = if offline { TEXT_DISABLED } else { TEXT_SECONDARY };
    // Link row.
    let link_y = if compact { inner.bottom() - 24.0 } else { gem_r.bottom() + 16.0 };
    let link_pct = if g.online { Some(g.poll_success_pct) } else { None };
    let meter_c = h.link_level.color(TEXT_SECONDARY);
    if compact {
        // Compact: link meter + %, battery + V on one row.
        widgets::link_meter(&p, pos2(text_x, link_y + 3.0), link_pct, if offline { BORDER_STRONG } else { meter_c });
        let lt = link_pct.map(|v| format!("{v:.0}%")).unwrap_or("—".into());
        p.text(pos2(text_x + 22.0, link_y + 10.0), Align2::LEFT_CENTER, lt, value_font.clone(), val_col);
        let bx = text_x + 72.0;
        let (v, lvl) = health::battery_state(Some(&h), app.persist.nimh);
        let bc = battery_color(lvl);
        widgets::battery(&p, pos2(bx, link_y + 4.5), if offline { None } else { lvl }, bc);
        let vt = v.map(|v| format!("{v:.2} V")).unwrap_or("—".into());
        p.text(pos2(bx + 28.0, link_y + 10.0), Align2::LEFT_CENTER, vt, value_font.clone(), val_col);
    } else {
        let lx = inner.left();
        widgets::link_meter(&p, pos2(lx, link_y + 3.0), link_pct, if offline { BORDER_STRONG } else { meter_c });
        let lt = link_pct.map(|v| format!("{v:.0}%")).unwrap_or("—".into());
        p.text(pos2(lx + 24.0, link_y + 10.0), Align2::LEFT_CENTER, lt, value_font.clone(), if h.link_level == Level::Normal { val_col } else { meter_c });
        let spark = Rect::from_min_max(pos2(lx + 72.0, link_y), pos2(inner.right(), link_y + 20.0));
        let samples = link_samples(&g, 120);
        let sc = link_color(&samples);
        let sa = if sc == VIZ_LEVEL { 0.7 } else { 1.0 };
        widgets::sparkline(&p, spark, &samples, 120, &SparkOpts { color: sc, line_alpha: sa, threshold: None, glide: 0.0, y_range: (0.0, 100.0) });

        // Battery row.
        let by = link_y + 34.0;
        let (v, lvl) = health::battery_state(Some(&h), app.persist.nimh);
        let bc = battery_color(lvl);
        if h.battery_level == Level::Critical && !offline {
            widgets::paint_icon(&p, pos2(lx + 11.0, by + 7.0), icons::BATTERY_WARNING, 16.0, ERROR);
        } else {
            widgets::battery(&p, pos2(lx, by + 1.5), if offline { None } else { lvl }, bc);
        }
        let vt = v.map(|v| format!("{v:.2}")).unwrap_or("—".into());
        let vr = p.text(pos2(lx + 30.0, by + 7.0), Align2::LEFT_CENTER, &vt, value_font.clone(), if h.battery_level == Level::Normal { val_col } else { bc });
        let ur = p.text(pos2(vr.right() + 3.0, by + 7.0), Align2::LEFT_CENTER, "V", value_font.clone(), TEXT_FAINT);
        if let Some(l) = lvl
            && !offline {
                p.text(pos2(ur.right() + 8.0, by + 7.0), Align2::LEFT_CENTER, format!("~{:.0}%", l * 100.0), theme::font("value_small"), TEXT_FAINT);
            }
        let newest = app.firmware.newest_version(&app.snap);
        let fw_up = newest.is_some_and(|n| n > g.fw_version) && !g.in_bootloader;
        let fw = format!("{}{}", if fw_up { format!("{}  ", icons::DOWNLOAD) } else { String::new() }, fmt_fw_short(g.fw_version));
        p.text(pos2(inner.right(), by + 7.0), Align2::RIGHT_CENTER, fw, theme::font("value_small"), if fw_up { INFO } else { TEXT_FAINT });

        // Meta row, or quick actions on hover.
        let my = inner.bottom() - 9.0;
        if hov_t < 1.0 {
            let meta = if offline {
                g.last_seen.map(|t| format!("last seen {}", health::fmt_ago(t.elapsed()))).unwrap_or("never seen".into())
            } else {
                let rx = g.bcast_rx_pct.map(|v| format!("RX {v:.1}%")).unwrap_or("RX —".into());
                format!("{} · {} · ±{:.2} ms · {:.1} retries", fmt_uptime(g.uptime), rx, g.sync_err_us as f32 / 1000.0, g.avg_retries)
            };
            let meta = widgets::truncate(ui, &meta, &theme::font("value_small"), inner.width());
            p.text(pos2(inner.left(), my), Align2::LEFT_CENTER, meta, theme::font("value_small"), theme::alpha(if offline { TEXT_DISABLED } else { TEXT_FAINT }, 1.0 - hov_t));
        }
        if hov_t > 0.0 && !offline {
            let row = Rect::from_min_max(pos2(inner.left(), my - 12.0), pos2(inner.right(), my + 12.0));
            tile_hover_actions(app, ui, uid, row, hov_t);
        }
    }
    // Updating: progress line along the bottom.
    if let State::Updating(_) = &h.state
        && let Some(t) = app.snap.ota.as_ref().and_then(|o| o.targets.iter().find(|t| t.uid == uid)) {
            let w = rect.width() * (t.percent / 100.0).clamp(0.0, 1.0);
            p.rect_filled(Rect::from_min_size(pos2(rect.left(), rect.bottom() - 2.0), vec2(w, 2.0)), CornerRadius::ZERO, INFO);
        }

    // Interaction.
    if resp.clicked() {
        let m = ui.input(|i| i.modifiers);
        if m.command {
            if !app.selection.remove(&uid) {
                app.selection.insert(uid);
            }
        } else if m.shift {
            if let Some(a) = app.fleet.last_click.and_then(|a| visible.iter().position(|u| *u == a)) {
                let b = visible.iter().position(|u| *u == uid).unwrap();
                let (lo, hi) = (a.min(b), a.max(b));
                app.selection.extend(visible[lo..=hi].iter().copied());
            }
        } else {
            app.selection = [uid].into();
        }
        app.focus = Some(uid);
        app.fleet.last_click = Some(uid);
    }
    if resp.double_clicked() {
        app.persist.inspector_open = true;
    }
    resp.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, format!("Gift {name}")));
    resp.context_menu(|ui| context_menu(app, ui, uid));
}

/// Tile background, state border (dashed when offline) and selection glow.
fn paint_tile_frame(p: &egui::Painter, rect: Rect, state: &State, fill: Color32, hov_t: f32, sel_t: f32) {
    let cr = CornerRadius::same(R_MD);
    if sel_t > 0.0 {
        p.add(Shadow { offset: [0, 0], blur: 10, spread: 3, color: theme::alpha(ACCENT, 0.22 * sel_t) }.as_shape(rect, cr));
    }
    p.rect_filled(rect, cr, fill);
    if state.is_offline() {
        widgets::dashed_rect(p, rect.shrink(0.5), Stroke::new(1.0, BORDER), 4.0, 3.0);
    } else {
        let border = match state {
            State::Attention(_) => theme::alpha(WARN, 0.5),
            State::Critical(_) => ERROR,
            _ => theme::mix(BORDER_SUBTLE, BORDER_STRONG, hov_t * 0.6),
        };
        p.rect_stroke(rect, cr, Stroke::new(1.0, border), StrokeKind::Inside);
        widgets::top_highlight(p, rect, R_MD as f32);
    }
    if sel_t > 0.0 {
        p.rect_stroke(rect, cr, Stroke::new(1.5, theme::alpha(ACCENT, sel_t)), StrokeKind::Inside);
    }
}

/// Blink / Slot / More buttons that fade in over the tile's meta row on hover.
fn tile_hover_actions(app: &mut App, ui: &mut Ui, uid: Uid, row: Rect, hov_t: f32) {
    let mut child = ui.new_child(egui::UiBuilder::new().max_rect(row).layout(Layout::left_to_right(Align::Center)));
    child.set_opacity(hov_t);
    child.spacing_mut().item_spacing.x = 4.0;
    if widgets::secondary_button_h(&mut child, Some(icons::HAND_TAP), "Blink", true, 24.0).clicked() {
        app.blink(vec![uid]);
    }
    if widgets::secondary_button_h(&mut child, Some(icons::TAG), "Slot", true, 24.0).clicked() {
        app.select(uid);
        app.slot_popup = Some(uid);
        app.persist.inspector_open = true;
    }
    let more = widgets::ghost_button_sized(&mut child, icons::DOTS, "More", None, 24.0, false);
    if more.clicked() {
        app.select(uid);
    }
    egui::Popup::menu(&more).show(|ui| context_menu(app, ui, uid));
}

fn battery_color(lvl: Option<f32>) -> Color32 {
    match lvl {
        Some(l) if l < 0.10 => ERROR,
        Some(l) if l < 0.25 => WARN,
        _ => TEXT_SECONDARY,
    }
}

fn context_menu(app: &mut App, ui: &mut Ui, uid: Uid) {
    ui.set_min_width(200.0);
    if ui.button(format!("{}   Blink", icons::HAND_TAP)).clicked() {
        app.blink(vec![uid]);
        ui.close();
    }
    if ui.button(format!("{}   Rename…", icons::PENCIL)).clicked() {
        app.renaming = Some((uid, app.gift_name(&uid)));
        app.select(uid);
        app.persist.inspector_open = true;
        ui.close();
    }
    if ui.button(format!("{}   Assign slot…", icons::TAG)).clicked() {
        app.slot_popup = Some(uid);
        app.select(uid);
        app.persist.inspector_open = true;
        ui.close();
    }
    if ui.button(format!("{}   Show in Stage", icons::CUBE)).clicked() {
        app.select(uid);
        app.set_page(Page::Stage);
        ui.close();
    }
    if ui.button(format!("{}   Copy UID", icons::COPY)).clicked() {
        ui.ctx().copy_text(uid.to_hex());
        ui.close();
    }
    ui.separator();
    if ui.button(format!("{}   Reboot", icons::REBOOT)).clicked() {
        app.reboot(vec![uid], false);
        ui.close();
    }
    if ui.button(widgets::rich(format!("{}   Reboot to bootloader…", icons::CPU), "button", ERROR)).clicked() {
        app.confirm = Some(Confirm::BootloaderGift(uid));
        ui.close();
    }
}

// ---------------------------------------------------------------------------
// Table view
// ---------------------------------------------------------------------------

fn table(app: &mut App, ui: &mut Ui, visible: &[Uid]) {
    use egui_extras::{Column, TableBuilder};
    ui.add_space(S5);
    let mut clicked = None;
    egui::Frame::new().inner_margin(egui::Margin::symmetric(24, 0)).show(ui, |ui| {
        ui.visuals_mut().selection.bg_fill = ACTIVE;
        ui.visuals_mut().widgets.hovered.bg_fill = SURFACE_2;
        TableBuilder::new(ui)
            .striped(false)
            .resizable(false)
            .sense(Sense::click())
            .cell_layout(Layout::left_to_right(Align::Center))
            .column(Column::exact(36.0))
            .column(Column::remainder().at_least(120.0))
            .column(Column::exact(64.0))
            .column(Column::exact(140.0))
            .column(Column::exact(150.0))
            .column(Column::exact(64.0))
            .column(Column::exact(96.0))
            .column(Column::exact(64.0))
            .column(Column::exact(72.0))
            .column(Column::exact(56.0))
            .column(Column::exact(64.0))
            .header(28.0, |mut h| {
                for t in ["", "Name", "Slot", "State", "Link", "RX", "Battery", "FW", "Uptime", "Resets", "Sync"] {
                    h.col(|ui| {
                        widgets::text(ui, t, "label", TEXT_FAINT);
                    });
                }
            })
            .body(|body| {
                body.rows(32.0, visible.len(), |mut row| {
                    let uid = visible[row.index()];
                    let Some(g) = app.snap.gift(&uid).cloned() else { return };
                    let h = app.health.get(&uid).cloned();
                    let offline = !g.online;
                    row.set_selected(app.selection.contains(&uid));
                    let vc = if offline { TEXT_DISABLED } else { TEXT_SECONDARY };
                    row.col(|ui| {
                        widgets::gem_ui(ui, 22.0, &app.lights.get(&uid), if offline { GemState::Offline } else { GemState::Live });
                    });
                    row.col(|ui| {
                        widgets::text(ui, app.gift_name(&uid), "body", if offline { TEXT_MUTED } else { TEXT });
                    });
                    row.col(|ui| {
                        widgets::slot_chip(ui, g.slot);
                    });
                    row.col(|ui| {
                        if let Some(h) = &h {
                            status_badge(ui, h);
                        }
                    });
                    row.col(|ui| {
                        let lp = g.online.then_some(g.poll_success_pct);
                        let c = h.as_ref().map(|h| h.link_level.color(TEXT_SECONDARY)).unwrap_or(TEXT_SECONDARY);
                        widgets::link_meter_ui(ui, lp, c);
                        widgets::text(ui, lp.map(|v| format!("{v:.0}%")).unwrap_or("—".into()), "value", vc);
                        let (r, _) = ui.allocate_exact_size(vec2(64.0, 18.0), Sense::hover());
                        let samples = link_samples(&g, 120);
                        widgets::sparkline(ui.painter(), r, &samples, 120, &SparkOpts { color: VIZ_LEVEL, line_alpha: 0.7, threshold: None, glide: 0.0, y_range: (0.0, 100.0) });
                    });
                    row.col(|ui| {
                        widgets::text(ui, g.bcast_rx_pct.filter(|_| g.online).map(|v| format!("{v:.1}%")).unwrap_or("—".into()), "value", vc);
                    });
                    row.col(|ui| {
                        let (v, lvl) = health::battery_state(h.as_ref(), app.persist.nimh);
                        widgets::battery_ui(ui, if offline { None } else { lvl }, battery_color(lvl));
                        widgets::text(ui, v.map(|v| format!("{v:.2} V")).unwrap_or("—".into()), "value", vc);
                    });
                    row.col(|ui| {
                        widgets::text(ui, fmt_fw_short(g.fw_version), "value_small", TEXT_FAINT);
                    });
                    row.col(|ui| {
                        widgets::text(ui, if g.online { fmt_uptime(g.uptime) } else { "—".into() }, "value", vc);
                    });
                    row.col(|ui| {
                        widgets::text(ui, format!("{}", g.resets), "value", vc);
                    });
                    row.col(|ui| {
                        widgets::text(ui, if g.online { format!("±{:.2}", g.sync_err_us as f32 / 1000.0) } else { "—".into() }, "value", vc);
                    });
                    let r = row.response();
                    if r.clicked() {
                        clicked = Some(uid);
                    }
                    r.context_menu(|ui| context_menu(app, ui, uid));
                });
            });
    });
    if let Some(u) = clicked {
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

// ---------------------------------------------------------------------------
// Event drawer
// ---------------------------------------------------------------------------

fn event_drawer(app: &mut App, ui: &mut Ui) {
    let full = ui.max_rect();
    ui.painter().hline(full.x_range(), full.top() + 0.5, Stroke::new(1.0, BORDER_SUBTLE));
    // Header row (32 px).
    let header = Rect::from_min_size(full.min, vec2(full.width(), 34.0));
    ui.scope_builder(egui::UiBuilder::new().max_rect(header.shrink2(vec2(S7, 0.0))).layout(Layout::left_to_right(Align::Center)), |ui| {
        ui.spacing_mut().item_spacing.x = 8.0;
        let open = app.persist.event_log_open;
        let chev = if open { icons::CARET_DOWN } else { icons::CARET_RIGHT };
        let r = ui.add(egui::Label::new(widgets::rich(format!("{chev}  EVENTS"), "overline", TEXT_FAINT)).sense(Sense::click()));
        if r.clicked() {
            app.persist.event_log_open = !open;
        }
        r.on_hover_ui(|ui| widgets::tooltip_with_keys(ui, "Toggle event log", Some("Ctrl+J")));
        if open {
            ui.add_space(8.0);
            let mut lvl = app.fleet.log_level;
            widgets::segmented_h(ui, "log-level", &mut lvl, &["All", "Warnings", "Errors"], 24.0);
            app.fleet.log_level = lvl;
            if let Some(u) = app.fleet.log_gift {
                let name = app.gift_name(&u);
                if widgets::secondary_button_h(ui, Some(icons::X), &name, true, 24.0).clicked() {
                    app.fleet.log_gift = None;
                }
            }
            if !app.fleet.log_query.is_empty() && widgets::secondary_button_h(ui, Some(icons::X), &format!("'{}'", app.fleet.log_query), true, 24.0).clicked() {
                app.fleet.log_query.clear();
            }
        }
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            let ev = app.snap.events.len();
            widgets::text(ui, format!("{ev} events"), "value_small", TEXT_FAINT);
        });
    });
    if !app.persist.event_log_open {
        return;
    }
    let body = Rect::from_min_max(pos2(full.left(), header.bottom()), full.max);
    let mut ui = ui.new_child(egui::UiBuilder::new().max_rect(body.shrink2(vec2(S7 - 8.0, 0.0))));
    let events = app.snap.events.clone();
    let lvl = app.fleet.log_level;
    let gift_f = app.fleet.log_gift;
    let q = app.fleet.log_query.to_lowercase();
    // Coalesce identical consecutive events within 60 s.
    struct Row<'a> {
        ev: &'a lumigift_core::LogEvent,
        count: usize,
    }
    let mut rows: Vec<Row> = Vec::new();
    for e in events.iter() {
        let pass = match lvl {
            1 => e.level != LogLevel::Info,
            2 => e.level == LogLevel::Error,
            _ => true,
        } && gift_f.is_none_or(|u| e.gift == Some(u))
            && (q.is_empty() || e.message.to_lowercase().contains(&q));
        if !pass {
            continue;
        }
        if let Some(last) = rows.last_mut()
            && last.ev.message == e.message && last.ev.gift == e.gift && e.at.duration_since(last.ev.at) < Duration::from_secs(60) {
                last.count += 1;
                last.ev = e;
                continue;
            }
        rows.push(Row { ev: e, count: 1 });
    }
    if rows.is_empty() {
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            ui.add_space(8.0);
            widgets::text(ui, "No events yet. Discoveries, reboots and warnings will appear here.", "body", TEXT_FAINT);
        });
        return;
    }
    let mut select = None;
    egui::ScrollArea::vertical().stick_to_bottom(true).auto_shrink(false).show_rows(&mut ui, 24.0, rows.len(), |ui, range| {
        for r in &rows[range] {
            let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 24.0), Sense::hover());
            if resp.hovered() {
                ui.painter().rect_filled(rect, CornerRadius::same(R_XS), SURFACE_1);
            }
            let p = ui.painter();
            let y = rect.center().y;
            let mut x = rect.left() + 8.0;
            p.text(pos2(x, y), Align2::LEFT_CENTER, health::fmt_wall(r.ev.wall), theme::font("mono"), TEXT_FAINT);
            x += 76.0;
            let msg_l = r.ev.message.to_lowercase();
            let (icon, col) = if msg_l.contains("reboot") {
                (icons::REBOOT, TEXT_MUTED)
            } else if msg_l.contains("ota") || msg_l.contains("firmware") {
                (icons::DOWNLOAD, if r.ev.level == LogLevel::Info { TEXT_MUTED } else { level_icon(r.ev.level).1 })
            } else {
                level_icon(r.ev.level)
            };
            widgets::paint_icon(p, pos2(x + 8.0, y), icon, 15.0, col);
            x += 26.0;
            if let Some(u) = r.ev.gift {
                let name = app.gift_name(&u);
                let w = widgets::text_width(ui, &name, theme::font("small_medium")) + 26.0;
                let cr = Rect::from_min_size(pos2(x, y - 9.0), vec2(w, 18.0));
                let cresp = ui.interact(cr, ui.scope_id().with(("evgift", r.ev.at, u)), Sense::click());
                let p = ui.painter();
                p.rect_filled(cr, CornerRadius::same(R_XS), if cresp.hovered() { HOVER } else { SURFACE_3 });
                let leds = app.lights.get(&u);
                let mean = leds.iter().fold([0u32; 3], |a, c| [a[0] + c.r as u32, a[1] + c.g as u32, a[2] + c.b as u32]);
                let mc = Color32::from_rgb((mean[0] / 4) as u8, (mean[1] / 4) as u8, (mean[2] / 4) as u8);
                p.circle_filled(pos2(cr.left() + 10.0, y), 4.0, if mc == Color32::BLACK { BORDER_STRONG } else { mc });
                p.text(pos2(cr.left() + 19.0, y), Align2::LEFT_CENTER, name, theme::font("small_medium"), TEXT_SECONDARY);
                if cresp.clicked() {
                    select = Some(u);
                }
                x = cr.right() + 10.0;
            } else {
                x += 4.0;
            }
            let p = ui.painter();
            let mut msg = r.ev.message.clone();
            if r.count > 1 {
                msg = format!("{msg} ×{} · last {}", r.count, health::fmt_wall(r.ev.wall));
            }
            p.text(pos2(x, y), Align2::LEFT_CENTER, msg, theme::font("body"), TEXT_SECONDARY);
        }
    });
    if let Some(u) = select {
        app.select(u);
        app.fleet.log_gift = Some(u);
    }
}

// ---------------------------------------------------------------------------
// Inspector
// ---------------------------------------------------------------------------

pub fn inspector(app: &mut App, ui: &mut Ui) {
    if app.selection.len() > 1 {
        multi_inspector(app, ui);
        return;
    }
    let uid = app.selection.first().copied().or(app.focus);
    match uid.and_then(|u| app.snap.gift(&u).cloned()) {
        Some(g) => gift_inspector(app, ui, &g),
        None => summary_inspector(app, ui),
    }
}

fn section(ui: &mut Ui, title: &str) {
    ui.add_space(S6);
    widgets::overline(ui, title);
    ui.add_space(6.0);
}

fn summary_inspector(app: &mut App, ui: &mut Ui) {
    widgets::text(ui, "Fleet overview", "title", TEXT);
    ui.add_space(4.0);
    widgets::text(ui, "Select a gift to see its telemetry and actions.", "small", TEXT_MUTED);
    let total = app.snap.gifts.len();
    let online = app.snap.online_count();
    let att: Vec<(Uid, Health)> = app
        .snap
        .gifts
        .iter()
        .filter_map(|g| app.health.get(&g.uid).filter(|h| h.state.needs_attention()).map(|h| (g.uid, h.clone())))
        .collect();
    ui.add_space(S5);
    ui.horizontal(|ui| {
        let asleep = app.snap.asleep_count();
        let mut stats = vec![(format!("{online}"), "online"), (format!("{}", total - online - asleep), "offline")];
        if asleep > 0 {
            stats.push((format!("{asleep}"), "asleep"));
        }
        stats.push((format!("{}", att.len()), "attention"));
        for (v, l) in stats {
            ui.vertical(|ui| {
                ui.set_width(96.0);
                widgets::text(ui, v, "metric", TEXT);
                widgets::text(ui, l, "small", TEXT_FAINT);
            });
        }
    });
    if !att.is_empty() {
        section(ui, "Needs attention");
        for (u, h) in att {
            ui.horizontal(|ui| {
                let (ic, c) = if matches!(h.state, State::Critical(_)) { (icons::WARNING_CIRCLE, ERROR) } else { (icons::WARNING, WARN) };
                widgets::text(ui, ic, "body", c);
                if widgets::text_button(ui, &app.gift_name(&u), TEXT).clicked() {
                    app.select(u);
                }
                widgets::text(ui, h.sentence.split(':').next().unwrap_or(""), "small", TEXT_MUTED);
            });
        }
    }
    section(ui, "Slots");
    let unassigned = app.snap.gifts.iter().filter(|g| g.slot.is_none()).count();
    if unassigned > 0 {
        widgets::text(ui, format!("{unassigned} gift{} without a slot (not part of the show).", health::plural(unassigned)), "small", WARN);
        ui.add_space(4.0);
    } else {
        widgets::text(ui, "Every gift has a show slot.", "small", TEXT_MUTED);
        ui.add_space(4.0);
    }
    if widgets::secondary_button(ui, Some(icons::LIST_NUMBERS), "Auto-assign slots", app.connected()).clicked() {
        app.auto_assign();
    }
    section(ui, "Bridge");
    let l = app.snap.link.clone();
    let lw = 120.0;
    widgets::kv_row(ui, "Show frames", lw, |ui| { widgets::value_unit(ui, &format!("{:.0}", l.frames_per_s), "/s", "value", TEXT_SECONDARY); });
    widgets::kv_row(ui, "Polls", lw, |ui| { widgets::value_unit(ui, &format!("{:.0}", l.polls_per_s), "/s", "value", TEXT_SECONDARY); });
    widgets::kv_text(ui, "TX total", lw, format!("{}", l.tx_total), "value");
    // Of the transmissions that wait for an ack (polls and commands to known
    // gifts); show frames are never acked and discovery probes of empty
    // addresses are counted apart.
    let fail = l.tx_failed as f32 / (l.tx_acked + l.tx_failed).max(1) as f32 * 100.0;
    widgets::kv_row(ui, "TX failed", lw, |ui| { widgets::value_unit(ui, &format!("{fail:.2}"), "%", "value", if fail > 5.0 { WARN } else { TEXT_SECONDARY }); });
    widgets::kv_text(ui, "Dropped show", lw, format!("{}", l.show_dropped), "value");
    widgets::kv_text(ui, "Channel", lw, format!("{}", app.snap.bridge.radio.channel), "value");
    section(ui, "Danger zone");
    if widgets::danger_button(ui, Some(icons::REBOOT), "Reboot all…", false).clicked() {
        app.confirm = Some(Confirm::RebootAll);
    }
}

fn multi_inspector(app: &mut App, ui: &mut Ui) {
    let n = app.selection.len();
    widgets::text(ui, format!("{n} gifts selected"), "title", TEXT);
    ui.add_space(8.0);
    ui.horizontal_wrapped(|ui| {
        for u in app.selection.clone() {
            let leds = app.lights.get(&u);
            let on = app.snap.gift(&u).is_some_and(|g| g.online);
            widgets::gem_ui(ui, 28.0, &leds, if on { GemState::Live } else { GemState::Offline })
                .on_hover_text(app.gift_name(&u));
        }
    });
    ui.add_space(S5);
    ui.horizontal(|ui| {
        let sel: Vec<Uid> = app.selection.iter().copied().collect();
        if widgets::secondary_button(ui, Some(icons::HAND_TAP), "Blink all", app.connected()).clicked() {
            app.blink(sel.clone());
        }
        if widgets::secondary_button(ui, Some(icons::REBOOT), "Reboot", app.connected()).clicked() {
            app.reboot(sel, false);
        }
    });
    ui.add_space(8.0);
    if widgets::text_button(ui, "Clear selection", ACCENT_TEXT).clicked() {
        app.selection.clear();
    }
}

fn gift_inspector(app: &mut App, ui: &mut Ui, g: &GiftView) {
    let uid = g.uid;
    let h = app.health.get(&uid).cloned();
    let offline = !g.online;
    let connected = app.connected();
    // Header.
    ui.horizontal(|ui| {
        widgets::gem_ui(ui, 64.0, &app.lights.get(&uid), if offline { GemState::Offline } else { GemState::Live });
        ui.add_space(4.0);
        ui.vertical(|ui| {
            ui.spacing_mut().item_spacing.y = 6.0;
            let renaming = app.renaming.as_ref().is_some_and(|(u, _)| *u == uid);
            if renaming {
                let mut buf = app.renaming.as_ref().unwrap().1.clone();
                let r = ui.add(egui::TextEdit::singleline(&mut buf).char_limit(24).font(theme::font("title")).desired_width(ui.available_width() - 8.0));
                r.request_focus();
                let enter = ui.input(|i| i.key_pressed(Key::Enter));
                let esc = ui.input(|i| i.key_pressed(Key::Escape));
                app.renaming = Some((uid, buf.clone()));
                if enter {
                    app.renaming = None;
                    app.rename(uid, buf);
                } else if esc || r.lost_focus() && !enter {
                    app.renaming = None;
                }
            } else {
                let name = app.gift_name(&uid);
                let r = ui.add(egui::Label::new(widgets::rich(&name, "title", TEXT)).sense(Sense::click()))
                    .on_hover_text("Click to rename (F2)");
                if r.clicked() {
                    app.renaming = Some((uid, name));
                }
            }
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 6.0;
                widgets::slot_chip(ui, g.slot);
                if let Some(h) = &h {
                    status_badge(ui, h);
                }
                if app.is_sim() {
                    widgets::chip(ui, "SIM", TEXT_FAINT, SURFACE_3);
                }
            });
        });
    });
    if let Some(h) = &h {
        ui.add_space(10.0);
        let c = match h.state {
            State::Critical(_) => ERROR,
            State::Attention(_) => WARN,
            _ => TEXT_MUTED,
        };
        ui.add(egui::Label::new(widgets::rich(&h.sentence, "body", if c == TEXT_MUTED { TEXT_MUTED } else { TEXT_SECONDARY })).wrap());
    }
    ui.add_space(S4);
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 6.0;
        if widgets::secondary_button(ui, Some(icons::HAND_TAP), "Blink", connected && !offline)
            .on_hover_ui(|ui| widgets::tooltip_with_keys(ui, "Blink to identify", Some("B")))
            .clicked()
        {
            app.blink(vec![uid]);
        }
        if widgets::secondary_button(ui, Some(icons::TAG), "Slot", connected && !offline)
            .on_hover_ui(|ui| widgets::tooltip_with_keys(ui, "Assign show slot", Some("S")))
            .clicked()
        {
            app.slot_popup = if app.slot_popup == Some(uid) { None } else { Some(uid) };
        }
        if widgets::secondary_button(ui, Some(icons::PENCIL), "Rename", true)
            .on_hover_ui(|ui| widgets::tooltip_with_keys(ui, "Rename", Some("F2")))
            .clicked()
        {
            app.renaming = Some((uid, app.gift_name(&uid)));
        }
    });
    if app.slot_popup == Some(uid) {
        ui.add_space(8.0);
        slot_picker(app, ui, uid);
    }

    // LINK
    section(ui, "Link");
    let lw = 104.0;
    let samples = link_samples(g, usize::MAX);
    ui.horizontal(|ui| {
        widgets::text(ui, "Quality", "body", TEXT_MUTED);
        ui.add_space(lw - 52.0);
        let lp = g.online.then_some(g.poll_success_pct);
        let c = h.as_ref().map(|h| h.link_level.color(TEXT)).unwrap_or(TEXT);
        widgets::value_unit(ui, &lp.map(|v| format!("{v:.0}")).unwrap_or("—".into()), "%", "metric", c);
    });
    let (r, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 48.0), Sense::hover());
    ui.painter().rect_filled(r, CornerRadius::same(R_SM), BG_SUNKEN);
    let col = link_color(&samples);
    let cap = app.backend.as_ref().map(|b| b.link.config().history_len).unwrap_or(480);
    widgets::sparkline(ui.painter(), r.shrink2(vec2(6.0, 6.0)), &samples, cap, &SparkOpts { color: col, line_alpha: if col == VIZ_LEVEL { 0.7 } else { 1.0 }, threshold: Some(90.0), glide: 0.0, y_range: (0.0, 100.0) });
    if let Some(pp) = resp.hover_pos() {
        let inner = r.shrink2(vec2(6.0, 6.0));
        let step = inner.width() / (cap - 1) as f32;
        let back = ((inner.right() - pp.x) / step).round() as usize;
        if back < samples.len() {
            let v = samples[samples.len() - 1 - back];
            ui.painter().vline(pp.x, r.y_range(), Stroke::new(1.0, BORDER_STRONG));
            let secs = back as f32 * 0.5;
            resp.on_hover_text(format!("{:.0}s ago · {}", secs, if v.is_finite() { format!("{v:.0}%") } else { "offline".into() }));
        }
    }
    widgets::text(ui, "Last 4 minutes · dashed line = 90% warn threshold", "small", TEXT_FAINT);
    ui.add_space(6.0);
    widgets::kv_row(ui, "Retries", lw, |ui| { widgets::value_unit(ui, &format!("{:.2}", g.avg_retries), "per poll", "value", TEXT_SECONDARY); });
    widgets::kv_row(ui, "Broadcast RX", lw, |ui| {
        let c = h.as_ref().map(|h| h.bcast_level.color(TEXT_SECONDARY)).unwrap_or(TEXT_SECONDARY);
        widgets::value_unit(ui, &g.bcast_rx_pct.map(|v| format!("{v:.1}")).unwrap_or("—".into()), "%", "value", c);
        widgets::text(ui, format!("· {} lost", g.bcast_lost), "small", TEXT_FAINT);
    });
    widgets::kv_row(ui, "Clock sync", lw, |ui| {
        let synced = g.flags & status_flags::TIME_SYNCED != 0;
        widgets::value_unit(ui, &format!("±{:.2}", g.sync_err_us as f32 / 1000.0), "ms", "value", TEXT_SECONDARY);
        if !synced && g.status.is_some() {
            widgets::text(ui, "not locked", "small", WARN);
        }
    });
    widgets::kv_row(ui, "Temperature", lw, |ui| { widgets::value_unit(ui, &format!("{}", g.temp_c), "°C", "value", TEXT_SECONDARY); });
    widgets::kv_text(ui, "Address", lw, format!("0x{:02X}", g.node_addr), "mono");

    // POWER
    section(ui, "Power");
    let (v, lvl) = health::battery_state(h.as_ref(), app.persist.nimh);
    ui.horizontal(|ui| {
        widgets::battery_ui(ui, lvl, battery_color(lvl));
        widgets::value_unit(ui, &v.map(|v| format!("{v:.2}")).unwrap_or("—".into()), "V", "metric", h.as_ref().map(|h| h.battery_level.color(TEXT)).unwrap_or(TEXT));
        if let Some(l) = lvl {
            widgets::text(ui, format!("~{:.0}%", l * 100.0), "value", TEXT_FAINT);
        }
        widgets::text(ui, if app.persist.nimh { "NiMH" } else { "alkaline" }, "small", TEXT_FAINT);
    });
    let vs: Vec<f32> = g.history.iter().map(|s| if s.vbat_mv > 0.0 { s.vbat_mv / 1000.0 } else { f32::NAN }).collect();
    let (r, _) = ui.allocate_exact_size(vec2(ui.available_width(), 40.0), Sense::hover());
    ui.painter().rect_filled(r, CornerRadius::same(R_SM), BG_SUNKEN);
    widgets::sparkline(ui.painter(), r.shrink2(vec2(6.0, 6.0)), &vs, cap, &SparkOpts { color: VIZ_LEVEL, line_alpha: 0.7, threshold: None, glide: 0.0, y_range: (0.9, 1.6) });
    widgets::text(ui, "Battery voltage, last 4 minutes (0.9–1.6 V)", "small", TEXT_FAINT);

    // HEALTH
    section(ui, "Health");
    widgets::kv_value(ui, "Uptime", lw, if g.online { fmt_uptime(g.uptime) } else { "—".into() }, "value");
    widgets::kv_row(ui, "Resets", lw, |ui| {
        widgets::value_text(ui, format!("{} since power-on", g.resets), "value", TEXT_SECONDARY);
    });
    widgets::kv_row(ui, "Last reset", lw, |ui| {
        let ago = g.last_reboot.map(|t| format!(", {}", health::fmt_ago(t.elapsed()))).unwrap_or_default();
        widgets::value_text(ui, format!("{}{ago}", health::reset_reason_words(g.reset_reason)), "body", TEXT_SECONDARY);
    });
    widgets::kv_value(ui, "Polls", lw, format!("{} ok · {} failed", g.polls_ok, g.polls_failed), "value");

    // FIRMWARE
    section(ui, "Firmware");
    let newest = app.firmware.newest_version(&app.snap);
    widgets::kv_row(ui, "Version", lw, |ui| {
        widgets::value_text(ui, fmt_fw(g.fw_version), "value", TEXT_SECONDARY);
        if newest == Some(g.fw_version) {
            widgets::text(ui, "(latest)", "small", TEXT_FAINT);
        } else if newest.is_some_and(|n| n > g.fw_version) {
            widgets::text(ui, "update available", "small", INFO);
        }
    });
    widgets::kv_value(ui, "Bootloader", lw, format!("v{}", g.boot_version), "value");
    widgets::kv_value(ui, "HW rev", lw, hw_rev_name(g.hw_rev), "value");
    widgets::kv_row(ui, "UID", lw, |ui| {
        let s = health::fmt_uid(&uid);
        widgets::value_text(ui, &s[..19], "mono", TEXT_SECONDARY).on_hover_text(health::fmt_uid(&uid));
        if widgets::ghost_button_sized(ui, icons::COPY, "Copy UID", None, 22.0, false).clicked() {
            ui.ctx().copy_text(uid.to_hex());
            app.toasts.info("UID copied");
        }
    });

    // CONFIG
    section(ui, "Config");
    let mut b = match app.fleet.cfg_brightness {
        Some((u, b)) if u == uid => b,
        _ => 100,
    };
    ui.horizontal(|ui| {
        widgets::text(ui, "Max brightness", "body", TEXT_MUTED);
        ui.add(egui::Slider::new(&mut b, 0..=100).show_value(false));
        widgets::text(ui, format!("{b}%"), "value", TEXT_SECONDARY);
    });
    app.fleet.cfg_brightness = Some((uid, b));
    if widgets::secondary_button(ui, Some(icons::SUN), "Apply to this gift", connected && !offline).clicked()
        && let Some(link) = app.link().cloned() {
            let p = link.set_config(uid, config_key::MAX_BRIGHTNESS, (b as u32 * 255 / 100) as u16);
            app.push_op(format!("Set brightness on {}", app.gift_name(&uid)), "Brightness limit applied", OpKind::Report(p));
        }

    // DANGER ZONE
    section(ui, "Danger zone");
    ui.horizontal(|ui| {
        if widgets::danger_button(ui, Some(icons::REBOOT), "Reboot", false).clicked() {
            if app.live {
                app.confirm = Some(Confirm::RebootAll);
            } else {
                app.reboot(vec![uid], false);
            }
        }
        if widgets::danger_button(ui, Some(icons::CPU), "Reboot to bootloader…", false).clicked() {
            app.confirm = Some(Confirm::BootloaderGift(uid));
        }
    });
}

fn hw_rev_name(r: u8) -> String {
    if r == 0 { "—".into() } else { format!("{} (rev {})", r, health::hw_rev_letter(r)) }
}

/// Slot grid 1..MAX_SLOTS, 8 per row, 32×28 cells (shown 1-based, sent 0-based).
fn slot_picker(app: &mut App, ui: &mut Ui, uid: Uid) {
    let cur = app.snap.gift(&uid).and_then(|g| g.slot);
    let pend_id = egui::Id::unique("slot-swap");
    let mut swap: Option<(u8, Uid)> = ui.data(|d| d.get_temp(pend_id));
    egui::Frame::new().fill(SURFACE_2).stroke(Stroke::new(1.0, BORDER_SUBTLE)).corner_radius(CornerRadius::same(R_MD)).inner_margin(10).show(ui, |ui| {
        widgets::text(ui, "Assign slot", "label", TEXT_MUTED);
        ui.add_space(6.0);
        egui::Grid::new("slots").spacing(vec2(4.0, 4.0)).show(ui, |ui| {
            for s in 0..MAX_SLOTS {
                let occupant = app.snap.gifts.iter().find(|g| g.slot == Some(s) && g.uid != uid).map(|g| g.uid);
                let (r, resp) = ui.allocate_exact_size(vec2(32.0, 28.0), Sense::click());
                let p = ui.painter();
                let is_cur = cur == Some(s);
                let fill = if is_cur { ACCENT_SOFT } else if resp.hovered() { HOVER } else { SURFACE_3 };
                p.rect_filled(r, CornerRadius::same(R_XS), fill);
                if is_cur {
                    p.rect_stroke(r, CornerRadius::same(R_XS), Stroke::new(1.0, ACCENT), StrokeKind::Inside);
                }
                if let Some(o) = occupant {
                    let leds = app.lights.get(&o);
                    widgets::gem(p, Rect::from_center_size(pos2(r.center().x, r.center().y - 4.0), vec2(14.0, 14.0)), &leds, GemState::Live);
                    p.text(pos2(r.center().x, r.bottom() - 6.0), Align2::CENTER_CENTER, format!("{}", s + 1), theme::font("small"), TEXT_FAINT);
                } else {
                    p.text(r.center(), Align2::CENTER_CENTER, format!("{}", s + 1), theme::font("value_small"), if is_cur { ACCENT_TEXT } else { TEXT_SECONDARY });
                }
                let resp = if let Some(o) = occupant { resp.on_hover_text(app.gift_name(&o)) } else { resp };
                if resp.clicked() && !is_cur {
                    match occupant {
                        Some(o) => swap = Some((s, o)),
                        None => {
                            app.set_slot(uid, Some(s));
                            app.slot_popup = None;
                        }
                    }
                }
                if s % 8 == 7 {
                    ui.end_row();
                }
            }
        });
        if let Some((s, o)) = swap {
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                widgets::text(ui, format!("Swap with {}?", app.gift_name(&o)), "body", TEXT);
                if widgets::primary_button(ui, None, "Swap", true).clicked() {
                    app.set_slot(o, cur);
                    app.set_slot(uid, Some(s));
                    swap = None;
                    app.slot_popup = None;
                }
                if widgets::secondary_button(ui, None, "Cancel", true).clicked() {
                    swap = None;
                }
            });
        }
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            if widgets::text_button(ui, "Clear slot", TEXT_MUTED).clicked() {
                app.set_slot(uid, None);
                app.slot_popup = None;
            }
            ui.add_space(8.0);
            if widgets::text_button(ui, "Number all left→right (from Stage layout)", ACCENT_TEXT).clicked() {
                crate::pages::stage_page::number_left_to_right(app);
                app.slot_popup = None;
            }
        });
    });
    ui.data_mut(|d| match swap {
        Some(v) => {
            d.insert_temp(pend_id, v);
        }
        None => d.remove::<(u8, Uid)>(pend_id),
    });
}
