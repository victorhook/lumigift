//! Custom-painted widgets (docs/UI_DESIGN.md §4.0, §4.2). Everything paints
//! with `theme` tokens; egui's own widgets are used where `Visuals` suffice.

use egui::{
    Align2, Color32, CornerRadius, FontId, Mesh, Pos2, Rect, Response, RichText, Sense, Shape,
    Stroke, StrokeKind, Ui, Vec2, WidgetInfo, pos2, vec2,
};
use lumigift_proto::Rgb;

use crate::icons;
use crate::theme::{self, *};

// ---------------------------------------------------------------------------
// Text helpers
// ---------------------------------------------------------------------------

pub fn rich(s: impl Into<String>, style: &str, color: Color32) -> RichText {
    RichText::new(s).font(theme::font(style)).color(color)
}

pub fn text(ui: &mut Ui, s: impl Into<String>, style: &str, color: Color32) -> Response {
    ui.add(egui::Label::new(rich(s, style, color)).selectable(false))
}

/// Selectable value text (inspector values are copyable).
pub fn value_text(ui: &mut Ui, s: impl Into<String>, style: &str, color: Color32) -> Response {
    ui.add(egui::Label::new(rich(s, style, color)).selectable(true))
}

/// UPPERCASE section header with letter spacing in TEXT_FAINT.
pub fn overline(ui: &mut Ui, s: &str) -> Response {
    ui.add(
        egui::Label::new(
            RichText::new(s.to_uppercase())
                .font(theme::font("overline"))
                .color(TEXT_FAINT)
                .extra_letter_spacing(0.6),
        )
        .selectable(false),
    )
}

/// `value` + `unit` (unit in TEXT_FAINT at the same size).
pub fn value_unit(ui: &mut Ui, value: &str, unit: &str, style: &str, color: Color32) {
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 3.0;
        text(ui, value, style, color);
        if !unit.is_empty() {
            let unit_style = if style == "metric" { "value" } else { style };
            text(ui, unit, unit_style, TEXT_FAINT);
        }
    });
}

pub fn paint_icon(painter: &egui::Painter, pos: Pos2, icon: &str, size: f32, color: Color32) -> Rect {
    painter.text(
        pos,
        Align2::CENTER_CENTER,
        icon,
        FontId::new(size, egui::FontFamily::Name("icons".into())),
        color,
    )
}

pub fn paint_icon_fill(painter: &egui::Painter, pos: Pos2, icon: &str, size: f32, color: Color32) -> Rect {
    painter.text(
        pos,
        Align2::CENTER_CENTER,
        icon,
        FontId::new(size, egui::FontFamily::Name("icons-fill".into())),
        color,
    )
}

/// Width of a text in a given style.
pub fn text_width(ui: &Ui, s: &str, font: FontId) -> f32 {
    ui.fonts_mut(|f| f.layout_no_wrap(s.to_owned(), font, TEXT).size().x)
}

/// Truncate `s` with an ellipsis so it fits `max_w`.
pub fn truncate(ui: &Ui, s: &str, font: &FontId, max_w: f32) -> String {
    if text_width(ui, s, font.clone()) <= max_w {
        return s.to_owned();
    }
    let chars: Vec<char> = s.chars().collect();
    let mut n = chars.len();
    while n > 0 {
        n -= 1;
        let t: String = chars[..n].iter().collect::<String>() + "…";
        if text_width(ui, &t, font.clone()) <= max_w {
            return t;
        }
    }
    "…".into()
}

/// The 1 px "lit from above" line inside the top edge of level 2–4 frames.
pub fn top_highlight(painter: &egui::Painter, rect: Rect, radius: f32) {
    painter.hline(
        (rect.left() + radius)..=(rect.right() - radius),
        rect.top() + 1.0,
        Stroke::new(1.0, TOP_HIGHLIGHT),
    );
}

// ---------------------------------------------------------------------------
// Buttons
// ---------------------------------------------------------------------------

fn hover_t(ui: &Ui, resp: &Response) -> f32 {
    ui.ctx()
        .animate_bool_with_time(resp.id.with("hov"), resp.hovered(), MOTION_FAST)
}

pub struct ButtonStyle {
    pub fill: Color32,
    pub fill_hover: Color32,
    pub fill_active: Color32,
    pub text: Color32,
    pub text_hover: Color32,
    pub stroke: Option<Color32>,
}

fn paint_button(
    ui: &mut Ui,
    icon: Option<&str>,
    label: &str,
    height: f32,
    enabled: bool,
    style: &ButtonStyle,
    min_w: f32,
) -> Response {
    let font = theme::font("button");
    let label_w = if label.is_empty() { 0.0 } else { text_width(ui, label, font.clone()) };
    let icon_w = if icon.is_some() { 16.0 } else { 0.0 };
    let gap = if icon.is_some() && !label.is_empty() { 6.0 } else { 0.0 };
    let pad = if label.is_empty() { (height - 16.0) / 2.0 } else { 12.0 };
    let w = (label_w + icon_w + gap + pad * 2.0).max(min_w).max(height);
    let sense = if enabled { Sense::click() } else { Sense::hover() };
    let (rect, resp) = ui.allocate_exact_size(vec2(w, height), sense);
    resp.widget_info(|| WidgetInfo::labeled(egui::Role::Button, enabled, label));
    if ui.is_rect_visible(rect) {
        let t = if enabled { hover_t(ui, &resp) } else { 0.0 };
        let pressed = enabled && resp.is_pointer_button_down_on();
        let fill = if !enabled {
            SURFACE_3
        } else if pressed {
            style.fill_active
        } else {
            theme::mix(style.fill, style.fill_hover, t)
        };
        let fg = if !enabled {
            TEXT_DISABLED
        } else {
            theme::mix(style.text, style.text_hover, t)
        };
        let p = ui.painter();
        let cr = CornerRadius::same(R_SM);
        p.rect_filled(rect, cr, fill);
        if let Some(s) = style.stroke {
            p.rect_stroke(rect, cr, Stroke::new(1.0, s), StrokeKind::Inside);
        }
        let total = icon_w + gap + label_w;
        let mut x = rect.center().x - total / 2.0;
        if let Some(ic) = icon {
            paint_icon(p, pos2(x + 8.0, rect.center().y), ic, 16.0, fg);
            x += icon_w + gap;
        }
        if !label.is_empty() {
            p.text(pos2(x, rect.center().y), Align2::LEFT_CENTER, label, font, fg);
        }
        if resp.has_focus() {
            focus_ring(p, rect, R_SM);
        }
    }
    resp
}

pub fn focus_ring(p: &egui::Painter, rect: Rect, r: u8) {
    p.rect_stroke(
        rect.expand(2.0),
        CornerRadius::same(r.saturating_add(2)),
        Stroke::new(2.0, FOCUS),
        StrokeKind::Outside,
    );
}

pub fn primary_button(ui: &mut Ui, icon: Option<&str>, label: &str, enabled: bool) -> Response {
    primary_button_h(ui, icon, label, enabled, CONTROL_H)
}

pub fn primary_button_h(ui: &mut Ui, icon: Option<&str>, label: &str, enabled: bool, h: f32) -> Response {
    paint_button(
        ui,
        icon,
        label,
        h,
        enabled,
        &ButtonStyle {
            fill: ACCENT,
            fill_hover: ACCENT_HOVER,
            fill_active: ACCENT_PRESSED,
            text: ON_ACCENT,
            text_hover: ON_ACCENT,
            stroke: None,
        },
        0.0,
    )
}

pub fn secondary_button(ui: &mut Ui, icon: Option<&str>, label: &str, enabled: bool) -> Response {
    secondary_button_h(ui, icon, label, enabled, CONTROL_H)
}

pub fn secondary_button_h(ui: &mut Ui, icon: Option<&str>, label: &str, enabled: bool, h: f32) -> Response {
    let r = paint_button(
        ui,
        icon,
        label,
        h,
        true,
        &ButtonStyle {
            fill: SURFACE_3,
            fill_hover: HOVER,
            fill_active: ACTIVE,
            text: TEXT_SECONDARY,
            text_hover: TEXT,
            stroke: Some(BORDER_SUBTLE),
        },
        0.0,
    );
    if !enabled {
        // Secondary buttons dim with alpha instead of greying out.
        ui.painter().rect_filled(r.rect, CornerRadius::same(R_SM), theme::alpha(BG, 0.55));
        return r.interact(Sense::hover()).with_new_rect(r.rect);
    }
    r
}

pub fn danger_button(ui: &mut Ui, icon: Option<&str>, label: &str, solid: bool) -> Response {
    let st = if solid {
        ButtonStyle {
            fill: ERROR,
            fill_hover: theme::mix(ERROR, TEXT, 0.15),
            fill_active: theme::mix(ERROR, BG, 0.15),
            text: BG,
            text_hover: BG,
            stroke: None,
        }
    } else {
        ButtonStyle {
            fill: SURFACE_3,
            fill_hover: ERROR_SOFT,
            fill_active: ERROR_SOFT,
            text: ERROR,
            text_hover: ERROR,
            stroke: Some(BORDER_SUBTLE),
        }
    };
    paint_button(ui, icon, label, CONTROL_H, true, &st, 0.0)
}

/// Transparent 28×28 icon button with a mandatory tooltip.
pub fn ghost_button(ui: &mut Ui, icon: &str, tooltip: &str, shortcut: Option<&str>) -> Response {
    ghost_button_sized(ui, icon, tooltip, shortcut, CONTROL_H, false)
}

pub fn ghost_button_sized(
    ui: &mut Ui,
    icon: &str,
    tooltip: &str,
    shortcut: Option<&str>,
    size: f32,
    active: bool,
) -> Response {
    let (rect, resp) = ui.allocate_exact_size(Vec2::splat(size), Sense::click());
    resp.widget_info(|| WidgetInfo::labeled(egui::Role::Button, true, tooltip));
    if ui.is_rect_visible(rect) {
        let t = hover_t(ui, &resp);
        let p = ui.painter();
        let fill = if resp.is_pointer_button_down_on() {
            ACTIVE
        } else if active {
            ACCENT_SOFT
        } else {
            theme::alpha(HOVER, t)
        };
        p.rect_filled(rect, CornerRadius::same(R_SM), fill);
        let fg = if active { ACCENT_TEXT } else { theme::mix(TEXT_MUTED, TEXT, t) };
        paint_icon(p, rect.center(), icon, 16.0, fg);
    }
    let tooltip = tooltip.to_owned();
    let shortcut = shortcut.map(str::to_owned);
    resp.on_hover_ui(move |ui| tooltip_with_keys(ui, &tooltip, shortcut.as_deref()))
}

/// Text-only button (e.g. "Select: All · Outdated · None").
pub fn text_button(ui: &mut Ui, label: &str, color: Color32) -> Response {
    let font = theme::font("button");
    let w = text_width(ui, label, font.clone());
    let (rect, resp) = ui.allocate_exact_size(vec2(w + 4.0, 20.0), Sense::click());
    let c = if resp.hovered() { theme::mix(color, TEXT, 0.4) } else { color };
    ui.painter().text(rect.center(), Align2::CENTER_CENTER, label, font, c);
    if resp.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    resp
}

pub fn tooltip_with_keys(ui: &mut Ui, label: &str, keys: Option<&str>) {
    ui.horizontal(|ui| {
        text(ui, label, "small", TEXT_SECONDARY);
        if let Some(k) = keys {
            ui.add_space(8.0);
            keycaps(ui, k);
        }
    });
}

/// Keycaps for a shortcut string like "Ctrl+Shift+B".
pub fn keycaps(ui: &mut Ui, keys: &str) {
    ui.spacing_mut().item_spacing.x = 3.0;
    for k in keys.split('+') {
        keycap(ui, k);
    }
}

pub fn keycap(ui: &mut Ui, k: &str) -> Response {
    let font = theme::font("small");
    let w = text_width(ui, k, font.clone()).max(8.0) + 10.0;
    let (rect, resp) = ui.allocate_exact_size(vec2(w, 18.0), Sense::hover());
    let p = ui.painter();
    p.rect_filled(rect, CornerRadius::same(R_XS), SURFACE_3);
    p.rect_stroke(rect, CornerRadius::same(R_XS), Stroke::new(1.0, BORDER), StrokeKind::Inside);
    p.text(rect.center(), Align2::CENTER_CENTER, k, font, TEXT_MUTED);
    resp
}

// ---------------------------------------------------------------------------
// Segmented control, toggle, pills, chips
// ---------------------------------------------------------------------------

/// Pill segmented control; returns true if changed.
pub fn segmented(ui: &mut Ui, id: impl std::hash::Hash + std::fmt::Debug, selected: &mut usize, items: &[&str]) -> bool {
    segmented_h(ui, id, selected, items, CONTROL_H)
}

pub fn segmented_h(
    ui: &mut Ui,
    id: impl std::hash::Hash + std::fmt::Debug,
    selected: &mut usize,
    items: &[&str],
    h: f32,
) -> bool {
    let font = theme::font("button");
    let is_icon = |s: &str| s.chars().count() == 1 && s.chars().next().is_some_and(|c| c as u32 >= 0xE000);
    let widths: Vec<f32> = items
        .iter()
        .map(|s| if is_icon(s) { h + 4.0 } else { text_width(ui, s, font.clone()) + 24.0 })
        .collect();
    let total: f32 = widths.iter().sum::<f32>() + 4.0;
    let (rect, _) = ui.allocate_exact_size(vec2(total, h), Sense::hover());
    let id = ui.scope_id().with(id);
    let p = ui.painter().clone();
    p.rect_filled(rect, CornerRadius::same(R_PILL), BG_SUNKEN);
    // Animated thumb.
    let mut xs = Vec::with_capacity(items.len());
    let mut x = rect.left() + 2.0;
    for w in &widths {
        xs.push(x);
        x += w;
    }
    let sel = (*selected).min(items.len().saturating_sub(1));
    let tx = ui.ctx().animate_value_with_time(id.with("x"), xs[sel], MOTION_BASE);
    let tw = ui.ctx().animate_value_with_time(id.with("w"), widths[sel], MOTION_BASE);
    let thumb = Rect::from_min_size(pos2(tx, rect.top() + 2.0), vec2(tw, h - 4.0));
    p.rect_filled(thumb, CornerRadius::same(R_PILL), SURFACE_3);
    p.rect_stroke(thumb, CornerRadius::same(R_PILL), Stroke::new(1.0, BORDER), StrokeKind::Inside);
    let mut changed = false;
    for (i, item) in items.iter().enumerate() {
        let r = Rect::from_min_size(pos2(xs[i], rect.top()), vec2(widths[i], h));
        let resp = ui.interact(r, id.with(i), Sense::click());
        resp.widget_info(|| WidgetInfo::selected(egui::Role::RadioButton, true, i == sel, *item));
        if resp.clicked() && i != sel {
            *selected = i;
            changed = true;
        }
        if resp.hovered() && i != sel {
            p.rect_filled(r.shrink(2.0), CornerRadius::same(R_PILL), theme::alpha(HOVER, 0.6));
        }
        let c = if i == sel { TEXT } else if resp.hovered() { TEXT_SECONDARY } else { TEXT_MUTED };
        if is_icon(item) {
            paint_icon(&p, r.center(), item, 16.0, c);
        } else {
            p.text(r.center(), Align2::CENTER_CENTER, *item, font.clone(), c);
        }
    }
    changed
}

/// 32×18 toggle switch.
pub fn toggle(ui: &mut Ui, on: &mut bool) -> Response {
    let (rect, mut resp) = ui.allocate_exact_size(vec2(32.0, 18.0), Sense::click());
    if resp.clicked() {
        *on = !*on;
        resp.mark_changed();
    }
    resp.widget_info(|| WidgetInfo::selected(egui::Role::CheckBox, true, *on, ""));
    let t = ui.ctx().animate_bool_with_time_and_easing(
        resp.id,
        *on,
        MOTION_BASE,
        egui::emath::easing::cubic_out,
    );
    let p = ui.painter();
    let track = theme::mix(if resp.hovered() { HOVER } else { BG_SUNKEN }, ACCENT, t);
    p.rect_filled(rect, CornerRadius::same(R_PILL), track);
    p.rect_stroke(
        rect,
        CornerRadius::same(R_PILL),
        Stroke::new(1.0, theme::mix(BORDER_INPUT, ACCENT, t)),
        StrokeKind::Inside,
    );
    let x = egui::lerp((rect.left() + 9.0)..=(rect.right() - 9.0), t);
    p.circle_filled(pos2(x, rect.center().y), 7.0, theme::mix(TEXT_MUTED, TEXT, t));
    resp
}

/// h-20 status pill: soft fill + solid icon/text.
pub fn pill(ui: &mut Ui, icon: Option<&str>, label: &str, fg: Color32, bg: Color32) -> Response {
    let font = theme::font("small_medium");
    let lw = text_width(ui, label, font.clone());
    let iw = if icon.is_some() { 14.0 + 4.0 } else { 0.0 };
    let (rect, resp) = ui.allocate_exact_size(vec2(lw + iw + 16.0, 20.0), Sense::hover());
    paint_pill(ui.painter(), rect, icon, label, fg, bg, font);
    resp
}

pub fn paint_pill(
    p: &egui::Painter,
    rect: Rect,
    icon: Option<&str>,
    label: &str,
    fg: Color32,
    bg: Color32,
    font: FontId,
) {
    p.rect_filled(rect, CornerRadius::same(R_PILL), bg);
    let mut x = rect.left() + 8.0;
    if let Some(ic) = icon {
        paint_icon(p, pos2(x + 7.0, rect.center().y), ic, 13.0, fg);
        x += 18.0;
    }
    p.text(pos2(x, rect.center().y), Align2::LEFT_CENTER, label, font, fg);
}

/// Small chip like the slot chip `#03` (R_XS, SURFACE_3, value_small).
pub fn chip(ui: &mut Ui, label: &str, fg: Color32, bg: Color32) -> Response {
    let font = theme::font("value_small");
    let w = text_width(ui, label, font.clone()) + 10.0;
    let (rect, resp) = ui.allocate_exact_size(vec2(w, 18.0), Sense::hover());
    let p = ui.painter();
    p.rect_filled(rect, CornerRadius::same(R_XS), bg);
    p.text(rect.center(), Align2::CENTER_CENTER, label, font, fg);
    resp
}

/// Dashed outline chip ("No slot").
pub fn dashed_chip(ui: &mut Ui, label: &str, fg: Color32) -> Response {
    let font = theme::font("value_small");
    let w = text_width(ui, label, font.clone()) + 12.0;
    let (rect, resp) = ui.allocate_exact_size(vec2(w, 18.0), Sense::hover());
    let p = ui.painter();
    dashed_rect(p, rect, Stroke::new(1.0, theme::alpha(fg, 0.8)), 3.0, 2.0);
    p.text(rect.center(), Align2::CENTER_CENTER, label, font, fg);
    resp
}

pub fn slot_chip(ui: &mut Ui, slot: Option<u8>) -> Response {
    match slot {
        Some(s) => chip(ui, &crate::health::fmt_slot(s), TEXT_MUTED, SURFACE_3),
        None => dashed_chip(ui, "No slot", WARN),
    }
}

pub fn dashed_rect(p: &egui::Painter, rect: Rect, stroke: Stroke, dash: f32, gap: f32) {
    let pts = [
        rect.left_top(),
        rect.right_top(),
        rect.right_bottom(),
        rect.left_bottom(),
        rect.left_top(),
    ];
    p.extend(Shape::dashed_line(&pts, stroke, dash, gap));
}

// ---------------------------------------------------------------------------
// Meters
// ---------------------------------------------------------------------------

/// 4-bar link meter, 16×14.
pub fn link_meter(p: &egui::Painter, origin: Pos2, pct: Option<f32>, color: Color32) {
    let filled = match pct {
        None => 0,
        Some(v) if v >= 90.0 => 4,
        Some(v) if v >= 75.0 => 3,
        Some(v) if v >= 50.0 => 2,
        Some(v) if v > 0.0 => 1,
        _ => 0,
    };
    let heights = [5.0, 8.0, 11.0, 14.0];
    for (i, h) in heights.iter().enumerate() {
        let x = origin.x + i as f32 * 4.0;
        let r = Rect::from_min_max(pos2(x, origin.y + 14.0 - h), pos2(x + 3.0, origin.y + 14.0));
        let c = if i < filled { color } else { BORDER_STRONG };
        p.rect_filled(r, CornerRadius::same(1), c);
    }
}

pub fn link_meter_ui(ui: &mut Ui, pct: Option<f32>, color: Color32) -> Response {
    let (rect, resp) = ui.allocate_exact_size(vec2(16.0, 14.0), Sense::hover());
    link_meter(ui.painter(), rect.min, pct, color);
    resp
}

/// Battery glyph 22×11. `level` 0..1.
pub fn battery(p: &egui::Painter, origin: Pos2, level: Option<f32>, color: Color32) {
    let body = Rect::from_min_size(origin, vec2(20.0, 11.0));
    p.rect_stroke(body, CornerRadius::same(2), Stroke::new(1.0, TEXT_MUTED), StrokeKind::Inside);
    let nub = Rect::from_min_size(pos2(body.right(), body.center().y - 2.5), vec2(2.0, 5.0));
    p.rect_filled(nub, CornerRadius::same(1), TEXT_MUTED);
    if let Some(l) = level {
        let inner = body.shrink(2.0);
        let w = (inner.width() * l.clamp(0.0, 1.0)).max(if l > 0.0 { 1.0 } else { 0.0 });
        p.rect_filled(
            Rect::from_min_size(inner.min, vec2(w, inner.height())),
            CornerRadius::same(1),
            color,
        );
    }
}

pub fn battery_ui(ui: &mut Ui, level: Option<f32>, color: Color32) -> Response {
    let (rect, resp) = ui.allocate_exact_size(vec2(22.0, 14.0), Sense::hover());
    battery(ui.painter(), pos2(rect.left(), rect.top() + 1.5), level, color);
    resp
}

/// Progress bar, 8 px pill.
pub fn progress_bar(p: &egui::Painter, rect: Rect, frac: f32, fill: Color32) {
    p.rect_filled(rect, CornerRadius::same(R_PILL), BG_SUNKEN);
    let w = rect.width() * frac.clamp(0.0, 1.0);
    if w > 0.5 {
        p.rect_filled(
            Rect::from_min_size(rect.min, vec2(w.max(rect.height()), rect.height())),
            CornerRadius::same(R_PILL),
            fill,
        );
    }
}

/// Indeterminate stripe animation inside `rect`.
pub fn indeterminate(ui: &Ui, rect: Rect, color: Color32) {
    let p = ui.painter();
    p.rect_filled(rect, CornerRadius::same(R_PILL), BG_SUNKEN);
    let t = ui.input(|i| i.time) as f32;
    let w = rect.width() * 0.3;
    let x = rect.left() - w + ((t * 0.8).fract()) * (rect.width() + w);
    let r = Rect::from_min_max(
        pos2(x.max(rect.left()), rect.top()),
        pos2((x + w).min(rect.right()), rect.bottom()),
    );
    if r.width() > 0.0 {
        p.rect_filled(r, CornerRadius::same(R_PILL), color);
    }
    ui.ctx().request_repaint();
}

/// Rotating spinner (circle-notch glyph rotated via a painted arc).
pub fn spinner(ui: &mut Ui, size: f32, color: Color32) -> Response {
    let (rect, resp) = ui.allocate_exact_size(Vec2::splat(size), Sense::hover());
    paint_spinner(ui, rect.center(), size * 0.38, color);
    resp
}

pub fn paint_spinner(ui: &Ui, c: Pos2, r: f32, color: Color32) {
    let t = ui.input(|i| i.time) as f32;
    let start = t * 6.0;
    let pts: Vec<Pos2> = (0..=20)
        .map(|i| {
            let a = start + i as f32 / 20.0 * std::f32::consts::TAU * 0.75;
            c + vec2(a.cos(), a.sin()) * r
        })
        .collect();
    ui.painter().add(Shape::line(pts, Stroke::new(1.5, color)));
    ui.ctx().request_repaint();
}

// ---------------------------------------------------------------------------
// Sparkline
// ---------------------------------------------------------------------------

pub struct SparkOpts {
    pub color: Color32,
    pub line_alpha: f32,
    /// Draw dashed threshold at this value (0..100).
    pub threshold: Option<f32>,
    /// Fractional sample offset for gliding (0..1).
    pub glide: f32,
    pub y_range: (f32, f32),
}

/// Vertical-gradient area between the polyline `pts` and the horizontal line `bottom`.
pub fn fill_under(pts: &[Pos2], bottom: f32, top_c: Color32, bot_c: Color32) -> Mesh {
    let mut mesh = Mesh::default();
    for (k, pt) in pts.iter().enumerate() {
        mesh.colored_vertex(*pt, top_c);
        mesh.colored_vertex(pos2(pt.x, bottom), bot_c);
        if k > 0 {
            let b = (k as u32) * 2;
            mesh.add_triangle(b - 2, b - 1, b);
            mesh.add_triangle(b - 1, b + 1, b);
        }
    }
    mesh
}

/// Samples are at 1 per x-step; NaN breaks the line. Right edge = newest.
pub fn sparkline(p: &egui::Painter, rect: Rect, samples: &[f32], capacity: usize, o: &SparkOpts) {
    // Short histories (just connected) span the full width instead of
    // leaving an empty chart with a dot at the right edge.
    let capacity = capacity.min(samples.len().max(8));
    if capacity < 2 {
        return;
    }
    let step = rect.width() / (capacity - 1) as f32;
    let (y0, y1) = o.y_range;
    let rect = rect.shrink2(vec2(0.0, 2.0));
    let to_y = |v: f32| {
        let n = ((v - y0) / (y1 - y0)).clamp(0.0, 1.0);
        rect.bottom() - n * rect.height()
    };
    if let Some(th) = o.threshold {
        let y = to_y(th);
        p.extend(Shape::dashed_line(
            &[pos2(rect.left(), y), pos2(rect.right(), y)],
            Stroke::new(1.0, BORDER_STRONG),
            3.0,
            3.0,
        ));
    }
    let n = samples.len().min(capacity);
    let samples = &samples[samples.len() - n..];
    let x_of = |i: usize| rect.right() - ((n - 1 - i) as f32 + o.glide) * step;
    let line_c = theme::alpha(o.color, o.line_alpha);
    let clip = p.with_clip_rect(rect.expand(3.0));
    // Split into runs of finite values.
    let mut run: Vec<Pos2> = Vec::new();
    let mut last = None;
    let flush = |run: &mut Vec<Pos2>| {
        if run.len() >= 2 {
            clip.add(Shape::mesh(fill_under(run, rect.bottom(), theme::alpha(o.color, 0.16), Color32::TRANSPARENT)));
            clip.add(Shape::line(run.clone(), Stroke::new(1.5, line_c)));
        }
        run.clear();
    };
    for (i, v) in samples.iter().enumerate() {
        if v.is_finite() {
            let pt = pos2(x_of(i), to_y(*v));
            run.push(pt);
            last = Some(pt);
        } else {
            flush(&mut run);
        }
    }
    flush(&mut run);
    if let Some(pt) = last
        && samples.last().is_some_and(|v| v.is_finite()) {
            clip.circle_filled(pt, 1.5, o.color);
        }
}

// ---------------------------------------------------------------------------
// Gem (LED swatch)
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq)]
pub enum GemState {
    Live,
    Offline,
}

pub fn rgb32(c: Rgb) -> Color32 {
    Color32::from_rgb(c.r, c.g, c.b)
}

fn luma(c: Rgb) -> f32 {
    (0.2126 * c.r as f32 + 0.7152 * c.g as f32 + 0.0722 * c.b as f32) / 255.0
}

/// Radial glow with a mid stop (smoother falloff).
pub fn soft_glow(p: &egui::Painter, c: Pos2, r: f32, color: Color32) {
    let mut mesh = Mesh::default();
    let n = 40u32;
    mesh.colored_vertex(c, color);
    let mid = theme::alpha(color, 0.35);
    for i in 0..n {
        let a = i as f32 / n as f32 * std::f32::consts::TAU;
        let d = vec2(a.cos(), a.sin());
        mesh.colored_vertex(c + d * r * 0.45, mid);
        mesh.colored_vertex(c + d * r, Color32::TRANSPARENT);
    }
    for i in 0..n {
        let j = (i + 1) % n;
        let (m0, o0, m1, o1) = (1 + i * 2, 2 + i * 2, 1 + j * 2, 2 + j * 2);
        mesh.add_triangle(0, m0, m1);
        mesh.add_triangle(m0, o0, m1);
        mesh.add_triangle(m1, o0, o1);
    }
    p.add(Shape::mesh(mesh));
}

/// The 2×2 LED swatch in a sunken well. `size` = well size (44 tiles, 64 inspector, 32 compact).
pub fn gem(p: &egui::Painter, rect: Rect, leds: &[Rgb; 4], state: GemState) {
    let s = rect.width();
    let radius = if s >= 40.0 { R_MD } else if s >= 24.0 { R_SM } else { R_XS };
    p.rect_filled(rect, CornerRadius::same(radius), BG_SUNKEN);
    p.rect_stroke(rect, CornerRadius::same(radius), Stroke::new(1.0, BORDER_SUBTLE), StrokeKind::Inside);
    let k = s / 44.0;
    let dot_r = 7.0 * k;
    let gap = 4.0 * k;
    let c = rect.center();
    let off = dot_r + gap / 2.0;
    // LED order: 0 (−,−) 1 (+,−) 2 (+,+) 3 (−,+)
    let centers = [
        c + vec2(-off, -off),
        c + vec2(off, -off),
        c + vec2(off, off),
        c + vec2(-off, off),
    ];
    let clip = p.with_clip_rect(rect.shrink(1.0));
    if state == GemState::Live {
        for (i, led) in leds.iter().enumerate() {
            let l = luma(*led).max((led.r.max(led.g).max(led.b)) as f32 / 255.0 * 0.6);
            if l > 0.02 {
                let a = (0.55 * l * 1.4).min(0.8);
                soft_glow(&clip, centers[i], 14.0 * k * 1.25, theme::alpha(rgb32(*led), a));
            }
        }
    }
    for (i, led) in leds.iter().enumerate() {
        let ctr = centers[i];
        match state {
            GemState::Live => {
                let on = led.r as u16 + led.g as u16 + led.b as u16 > 6;
                let fill = if on { rgb32(*led) } else { SURFACE_3 };
                p.circle_filled(ctr, dot_r, fill);
                if on {
                    // white-hot core for bright LEDs
                    let l = luma(*led);
                    if l > 0.35 {
                        p.circle_filled(ctr, dot_r * 0.45, theme::alpha(Color32::WHITE, (l - 0.35) * 0.6));
                    }
                }
                p.circle_stroke(
                    ctr,
                    dot_r - 0.5,
                    Stroke::new(1.0, Color32::from_rgba_premultiplied(20, 20, 20, 20)),
                );
            }
            GemState::Offline => {
                p.circle_filled(ctr, dot_r, SURFACE_1);
                // 45° hatching clipped to the circle (approximate with chords).
                let pitch = 3.0;
                let mut d = -dot_r * 1.5;
                while d < dot_r * 1.5 {
                    // line x - y = d, intersect circle
                    let half = (2.0 * dot_r * dot_r - d * d).max(0.0);
                    if half > 0.0 {
                        // param along direction (1,1)/√2
                        let n = vec2(1.0, -1.0) / 2f32.sqrt();
                        let dir = vec2(1.0, 1.0) / 2f32.sqrt();
                        let base = ctr + n * (d / 2f32.sqrt());
                        let dist = (d / 2f32.sqrt()).abs();
                        if dist < dot_r {
                            let h = (dot_r * dot_r - dist * dist).sqrt();
                            p.line_segment([base - dir * h, base + dir * h], Stroke::new(1.0, BORDER));
                        }
                    }
                    d += pitch * 2f32.sqrt();
                }
                p.circle_stroke(ctr, dot_r - 0.5, Stroke::new(1.0, BORDER));
            }
        }
    }
}

pub fn gem_ui(ui: &mut Ui, size: f32, leds: &[Rgb; 4], state: GemState) -> Response {
    let (rect, resp) = ui.allocate_exact_size(Vec2::splat(size), Sense::hover());
    if ui.is_rect_visible(rect) {
        gem(ui.painter(), rect, leds, state);
    }
    resp
}

/// Horizontal gradient mesh through evenly spaced colour stops.
pub fn h_gradient(rect: Rect, stops: &[Color32]) -> Mesh {
    let mut mesh = Mesh::default();
    let n = stops.len();
    for (i, c) in stops.iter().enumerate() {
        let x = rect.left() + rect.width() * i as f32 / (n - 1) as f32;
        mesh.colored_vertex(pos2(x, rect.top()), *c);
        mesh.colored_vertex(pos2(x, rect.bottom()), *c);
        if i > 0 {
            let b = i as u32 * 2;
            mesh.add_triangle(b - 2, b - 1, b);
            mesh.add_triangle(b - 1, b + 1, b);
        }
    }
    mesh
}

/// A clickable `w`×`h` menu row: palette strip (`strip_w` wide) + name.
pub fn palette_option(ui: &mut Ui, name: &str, stops: &[Rgb; 4], w: f32, h: f32, strip_w: f32) -> Response {
    let (r, resp) = ui.allocate_exact_size(vec2(w, h), Sense::click());
    if resp.hovered() {
        ui.painter().rect_filled(r, CornerRadius::same(R_SM), HOVER);
    }
    palette_strip(ui.painter(), Rect::from_min_size(pos2(r.left() + 6.0, r.center().y - 5.0), vec2(strip_w, 10.0)), stops, 1.0);
    ui.painter().text(pos2(r.left() + strip_w + 16.0, r.center().y), Align2::LEFT_CENTER, name, theme::font("body"), TEXT);
    resp
}

/// Palette gradient strip (4 stops).
pub fn palette_strip(p: &egui::Painter, rect: Rect, stops: &[Rgb; 4], brightness: f32) {
    let colors = stops.map(|s| theme::mix(Color32::BLACK, rgb32(s), brightness));
    p.add(Shape::mesh(h_gradient(rect, &colors)));
}

// ---------------------------------------------------------------------------
// Empty state
// ---------------------------------------------------------------------------

/// The brand illustration: the gift glyph with an ACCENT→CANDLE vertical
/// gradient (painted as clipped strips) and a soft glow.
pub fn brand_glyph(p: &egui::Painter, center: Pos2, size: f32) {
    soft_glow(p, center, size * 0.95, theme::alpha(ACCENT, 0.22));
    let font = FontId::new(size, egui::FontFamily::Name("icons-fill".into()));
    let strips = 32;
    let top = center.y - size * 0.55;
    let h = size * 1.1;
    for i in 0..strips {
        let y0 = top + h * i as f32 / strips as f32;
        let y1 = top + h * (i + 1) as f32 / strips as f32 + 0.5;
        let t = i as f32 / (strips - 1) as f32;
        let col = theme::alpha(theme::mix(ACCENT, CANDLE, t), 0.92);
        let clip = p.with_clip_rect(Rect::from_x_y_ranges(
            (center.x - size)..=(center.x + size),
            y0..=y1,
        ));
        clip.text(center, Align2::CENTER_CENTER, icons::GIFT, font.clone(), col);
    }
}

pub struct EmptyState<'a> {
    pub headline: &'a str,
    pub body: &'a str,
    pub primary: Option<(&'a str, Option<&'a str>)>,
    pub secondary: Option<&'a str>,
    pub big: bool,
}

pub enum EmptyAction {
    None,
    Primary,
    Secondary,
}

/// Centred empty state, max width 440. Returns which action was clicked.
pub fn empty_state(ui: &mut Ui, es: &EmptyState, extra: impl FnOnce(&mut Ui)) -> EmptyAction {
    let mut act = EmptyAction::None;
    let avail = ui.available_rect_before_wrap();
    let est_h = 96.0 + 32.0 + 40.0 + 40.0 + 36.0 + 60.0;
    let top = avail.top() + ((avail.height() - est_h) * 0.42).max(16.0);
    let w = 440.0f32.min(avail.width() - 32.0);
    let rect = Rect::from_min_size(pos2(avail.center().x - w / 2.0, top), vec2(w, est_h + 200.0));
    ui.scope_builder(egui::UiBuilder::new().max_rect(rect), |ui| {
        ui.vertical_centered(|ui| {
            let (r, _) = ui.allocate_exact_size(vec2(96.0, 96.0), Sense::hover());
            brand_glyph(ui.painter(), r.center(), 88.0);
            ui.add_space(S7);
            text(ui, es.headline, if es.big { "display" } else { "title" }, TEXT);
            ui.add_space(S3);
            ui.add(
                egui::Label::new(rich(es.body, "body", TEXT_MUTED))
                    .wrap()
                    .halign(egui::Align::Center),
            );
            ui.add_space(S7);
            if let Some((label, icon)) = es.primary
                && primary_button_h(ui, icon, label, true, CTA_H).clicked() {
                    act = EmptyAction::Primary;
                }
            if let Some(s) = es.secondary {
                ui.add_space(S3);
                if text_button(ui, s, ACCENT_TEXT).clicked() {
                    act = EmptyAction::Secondary;
                }
            }
            extra(ui);
        });
    });
    act
}

// ---------------------------------------------------------------------------
// Layout helpers
// ---------------------------------------------------------------------------

/// A body-font label left-aligned in a fixed `w`×`h` cell (form label column).
pub fn column_label(ui: &mut Ui, label: &str, w: f32, h: f32, color: Color32) {
    let (r, _) = ui.allocate_exact_size(vec2(w, h), Sense::hover());
    ui.painter().text(r.left_center(), Align2::LEFT_CENTER, label, theme::font("body"), color);
}

/// A label/value row with the label in a fixed column.
pub fn kv_row(ui: &mut Ui, label: &str, label_w: f32, add_value: impl FnOnce(&mut Ui)) {
    ui.horizontal(|ui| {
        column_label(ui, label, label_w, 18.0, TEXT_MUTED);
        add_value(ui);
    });
}

/// `kv_row` whose value is a single TEXT_SECONDARY text.
pub fn kv_text(ui: &mut Ui, label: &str, label_w: f32, value: impl Into<String>, style: &str) {
    kv_row(ui, label, label_w, |ui| {
        text(ui, value, style, TEXT_SECONDARY);
    });
}

/// `kv_row` whose value is a single selectable (copyable) TEXT_SECONDARY text.
pub fn kv_value(ui: &mut Ui, label: &str, label_w: f32, value: impl Into<String>, style: &str) {
    kv_row(ui, label, label_w, |ui| {
        value_text(ui, value, style, TEXT_SECONDARY);
    });
}

/// A rounded box of wrapped small text (hints, warnings).
pub fn note(ui: &mut Ui, s: impl Into<String>, fill: Color32, color: Color32, margin: f32) {
    egui::Frame::new().fill(fill).corner_radius(CornerRadius::same(R_SM)).inner_margin(margin).show(ui, |ui| {
        ui.add(egui::Label::new(rich(s, "small", color)).wrap());
    });
}

/// Paint a level-2 card background with the top highlight.
pub fn paint_card(p: &egui::Painter, rect: Rect, fill: Color32, stroke: Color32) {
    p.rect_filled(rect, CornerRadius::same(R_MD), fill);
    p.rect_stroke(rect, CornerRadius::same(R_MD), Stroke::new(1.0, stroke), StrokeKind::Inside);
    top_highlight(p, rect, R_MD as f32);
}

/// A card frame whose content gets the top highlight painted after.
pub fn card_ui<R>(ui: &mut Ui, add: impl FnOnce(&mut Ui) -> R) -> egui::InnerResponse<R> {
    let r = theme::card().show(ui, add);
    top_highlight(ui.painter(), r.response.rect, R_MD as f32);
    r
}
