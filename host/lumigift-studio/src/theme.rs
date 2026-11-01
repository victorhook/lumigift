//! Design tokens and egui style (docs/UI_DESIGN.md §2).

use egui::{Color32, CornerRadius, FontFamily, FontId, Margin, Shadow, Stroke, TextStyle, Vec2};

const fn c(hex: u32) -> Color32 {
    Color32::from_rgb((hex >> 16) as u8, (hex >> 8) as u8, hex as u8)
}

// Neutrals
pub const STAGE_BG: Color32 = c(0x050507);
pub const BG_SUNKEN: Color32 = c(0x09090C);
pub const BG: Color32 = c(0x0E0E12);
pub const SURFACE_1: Color32 = c(0x141418);
pub const SURFACE_2: Color32 = c(0x1B1B1F);
pub const SURFACE_3: Color32 = c(0x222226);
pub const HOVER: Color32 = c(0x29292D);
pub const ACTIVE: Color32 = c(0x303036);
pub const BORDER_SUBTLE: Color32 = c(0x242428);
pub const BORDER: Color32 = c(0x323237);
pub const BORDER_STRONG: Color32 = c(0x4C4C51);
pub const BORDER_INPUT: Color32 = c(0x636368);
pub const TEXT_DISABLED: Color32 = c(0x68686F);
pub const TEXT_FAINT: Color32 = c(0x888890);
pub const TEXT_MUTED: Color32 = c(0xA7A7AF);
pub const TEXT_SECONDARY: Color32 = c(0xC3C3C9);
pub const TEXT: Color32 = c(0xF0F0F3);
// Accent
pub const ACCENT: Color32 = c(0xBB78FE);
pub const ACCENT_HOVER: Color32 = c(0xC896FF);
pub const ACCENT_PRESSED: Color32 = c(0xA964EB);
pub const ACCENT_TEXT: Color32 = c(0xD3ADFE);
pub const ACCENT_SOFT: Color32 = c(0x37214D);
pub const SELECTION_BG: Color32 = c(0x4B2A6A);
pub const FOCUS: Color32 = c(0xCC9FFF);
pub const CANDLE: Color32 = c(0xFCC771);
pub const ON_ACCENT: Color32 = BG;
// Semantic
pub const OK: Color32 = c(0x4ED589);
pub const OK_SOFT: Color32 = c(0x0E311D);
pub const WARN: Color32 = c(0xFABB41);
pub const WARN_SOFT: Color32 = c(0x3A2805);
pub const ERROR: Color32 = c(0xFF6367);
pub const ERROR_SOFT: Color32 = c(0x4A191A);
pub const INFO: Color32 = c(0x62BBF5);
pub const INFO_SOFT: Color32 = c(0x0F2D40);
// Data viz
pub const VIZ_BASS: Color32 = c(0xF67D51);
pub const VIZ_MID: Color32 = c(0x2FD7B5);
pub const VIZ_TREBLE: Color32 = c(0x73B6FA);
pub const VIZ_LEVEL: Color32 = c(0xD6D6E5);
pub const VIZ_BEAT: Color32 = c(0xE3CA69);
pub const VIZ_SECTION: Color32 = c(0xDC95D5);
pub const GRID: Color32 = c(0x2D2D33);
// Overlays (premultiplied)
pub const SCRIM: Color32 = Color32::from_black_alpha(153);
pub const TOP_HIGHLIGHT: Color32 = Color32::from_rgba_premultiplied(10, 10, 10, 10);

// Spacing
pub const S2: f32 = 6.0;
pub const S3: f32 = 8.0;
pub const S4: f32 = 12.0;
pub const S5: f32 = 16.0;
pub const S6: f32 = 20.0;
pub const S7: f32 = 24.0;
pub const S8: f32 = 32.0;
pub const S9: f32 = 48.0;
pub const CONTROL_H: f32 = 28.0;
pub const CTA_H: f32 = 36.0;
// Radii
pub const R_XS: u8 = 3;
pub const R_SM: u8 = 6;
pub const R_MD: u8 = 8;
pub const R_LG: u8 = 12;
pub const R_PILL: u8 = 255;
// Motion (seconds)
pub const MOTION_FAST: f32 = 0.09;
pub const MOTION_BASE: f32 = 0.16;
pub const MOTION_SLOW: f32 = 0.26;
pub const MOTION_DELIBERATE: f32 = 0.40;
// Layout
pub const RAIL_W: f32 = 64.0;
pub const RAIL_W_COMPACT: f32 = 52.0;
pub const PAGE_HEADER_H: f32 = 48.0;
pub const STATUS_BAR_H: f32 = 28.0;
pub const INSPECTOR_W: f32 = 360.0;
pub const TILE_W_MIN: f32 = 248.0;

/// Multiply a colour's alpha (works on premultiplied colours).
pub fn alpha(c: Color32, a: f32) -> Color32 {
    c.gamma_multiply(a.clamp(0.0, 1.0))
}

/// Linear blend in sRGB space (good enough for UI cross-fades).
pub fn mix(a: Color32, b: Color32, t: f32) -> Color32 {
    let t = t.clamp(0.0, 1.0);
    let l = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * t).round() as u8;
    Color32::from_rgba_premultiplied(
        l(a.r(), b.r()),
        l(a.g(), b.g()),
        l(a.b(), b.b()),
        l(a.a(), b.a()),
    )
}

pub fn shadow_popover() -> Shadow {
    Shadow {
        offset: [0, 8],
        blur: 24,
        spread: 0,
        color: Color32::from_black_alpha(140),
    }
}
pub fn shadow_modal() -> Shadow {
    Shadow {
        offset: [0, 16],
        blur: 48,
        spread: 0,
        color: Color32::from_black_alpha(166),
    }
}

/// Card/tile frame (level 2).
pub fn card() -> egui::Frame {
    egui::Frame::new()
        .fill(SURFACE_2)
        .stroke(Stroke::new(1.0, BORDER_SUBTLE))
        .corner_radius(CornerRadius::same(R_MD))
        .inner_margin(Margin::same(12))
}
pub fn popover() -> egui::Frame {
    egui::Frame::new()
        .fill(SURFACE_3)
        .stroke(Stroke::new(1.0, BORDER))
        .corner_radius(CornerRadius::same(R_LG))
        .inner_margin(Margin::same(12))
        .shadow(shadow_popover())
}

pub fn font(style: &str) -> FontId {
    let fam = |n: &str| FontFamily::Name(n.into());
    match style {
        "display" => FontId::new(28.0, fam("semibold")),
        "heading" => FontId::new(18.0, fam("semibold")),
        "title" => FontId::new(15.0, fam("semibold")),
        "body" => FontId::new(13.0, FontFamily::Proportional),
        "button" => FontId::new(13.0, fam("medium")),
        "label" => FontId::new(12.0, fam("medium")),
        "small" => FontId::new(11.5, FontFamily::Proportional),
        "small_medium" => FontId::new(11.5, fam("medium")),
        "overline" => FontId::new(11.0, fam("semibold")),
        "metric" => FontId::new(20.0, fam("tnum-medium")),
        "value" => FontId::new(13.0, fam("tnum-medium")),
        "value_small" => FontId::new(11.5, fam("tnum")),
        "timecode" => FontId::new(22.0, fam("mono-medium")),
        "mono" => FontId::new(12.0, FontFamily::Monospace),
        "icon" => FontId::new(16.0, fam("icons")),
        "icon_fill" => FontId::new(16.0, fam("icons-fill")),
        _ => FontId::new(13.0, FontFamily::Proportional),
    }
}

pub fn apply(ctx: &egui::Context) {
    ctx.set_theme(egui::ThemePreference::Dark);
    let mut style = (*ctx.style_of(egui::Theme::Dark)).clone();

    style.text_styles = [
        (TextStyle::Name("display".into()), font("display")),
        (TextStyle::Heading, font("heading")),
        (TextStyle::Name("title".into()), font("title")),
        (TextStyle::Body, font("body")),
        (TextStyle::Button, font("button")),
        (TextStyle::Name("label".into()), font("label")),
        (TextStyle::Small, font("small")),
        (TextStyle::Name("overline".into()), font("overline")),
        (TextStyle::Name("metric".into()), font("metric")),
        (TextStyle::Name("value".into()), font("value")),
        (TextStyle::Name("value_small".into()), font("value_small")),
        (TextStyle::Name("timecode".into()), font("timecode")),
        (TextStyle::Monospace, font("mono")),
    ]
    .into();

    let sp = &mut style.spacing;
    sp.item_spacing = Vec2::new(S3, S2);
    sp.button_padding = Vec2::new(10.0, 4.0);
    sp.interact_size = Vec2::new(28.0, CONTROL_H);
    sp.menu_margin = Margin::same(6);
    sp.window_margin = Margin::same(16);
    sp.indent = 16.0;
    sp.icon_width = 16.0;
    sp.icon_width_inner = 8.0;
    sp.icon_spacing = 8.0;
    sp.slider_width = 160.0;
    sp.slider_rail_height = 4.0;
    sp.combo_height = 320.0;
    sp.tooltip_width = 320.0;
    sp.menu_width = 220.0;
    sp.scroll = egui::style::ScrollStyle::floating();

    style.animation_time = MOTION_BASE;
    style.interaction.tooltip_delay = 0.35;
    style.interaction.selectable_labels = false;

    let v = &mut style.visuals;
    v.dark_mode = true;
    v.override_text_color = None;
    v.weak_text_color = Some(TEXT_MUTED);
    v.hyperlink_color = ACCENT_TEXT;
    v.faint_bg_color = SURFACE_1;
    v.extreme_bg_color = BG_SUNKEN;
    v.text_edit_bg_color = Some(BG_SUNKEN);
    v.code_bg_color = SURFACE_1;
    v.warn_fg_color = WARN;
    v.error_fg_color = ERROR;
    v.panel_fill = BG;
    v.window_fill = SURFACE_3;
    v.window_stroke = Stroke::new(1.0, BORDER);
    v.window_corner_radius = CornerRadius::same(R_LG);
    v.window_shadow = shadow_modal();
    v.window_highlight_topmost = false;
    v.menu_corner_radius = CornerRadius::same(R_LG);
    v.popup_shadow = shadow_popover();
    v.selection.bg_fill = SELECTION_BG;
    v.selection.stroke = Stroke::new(1.0, TEXT);
    v.text_cursor.stroke = Stroke::new(2.0, ACCENT);
    v.button_frame = true;
    v.collapsing_header_frame = false;
    v.indent_has_left_vline = false;
    v.striped = false;
    v.slider_trailing_fill = true;
    v.handle_shape = egui::style::HandleShape::Circle;
    v.disabled_alpha = 0.45;

    let r = CornerRadius::same(R_SM);
    let w = &mut v.widgets;
    w.noninteractive.bg_fill = SURFACE_1;
    w.noninteractive.weak_bg_fill = SURFACE_1;
    w.noninteractive.bg_stroke = Stroke::new(1.0, BORDER_SUBTLE);
    w.noninteractive.fg_stroke = Stroke::new(1.0, TEXT_SECONDARY);
    w.noninteractive.corner_radius = r;
    w.noninteractive.expansion = 0.0;

    w.inactive.bg_fill = BG_SUNKEN;
    w.inactive.weak_bg_fill = SURFACE_3;
    w.inactive.bg_stroke = Stroke::new(1.0, BORDER);
    w.inactive.fg_stroke = Stroke::new(1.0, TEXT_SECONDARY);
    w.inactive.corner_radius = r;
    w.inactive.expansion = 0.0;

    w.hovered.bg_fill = HOVER;
    w.hovered.weak_bg_fill = HOVER;
    w.hovered.bg_stroke = Stroke::new(1.0, BORDER_STRONG);
    w.hovered.fg_stroke = Stroke::new(1.5, TEXT);
    w.hovered.corner_radius = r;
    w.hovered.expansion = 0.0;

    w.active.bg_fill = ACTIVE;
    w.active.weak_bg_fill = ACTIVE;
    w.active.bg_stroke = Stroke::new(1.0, BORDER_INPUT);
    w.active.fg_stroke = Stroke::new(1.5, TEXT);
    w.active.corner_radius = r;
    w.active.expansion = 0.0;

    w.open.bg_fill = SURFACE_3;
    w.open.weak_bg_fill = ACTIVE;
    w.open.bg_stroke = Stroke::new(1.0, BORDER_STRONG);
    w.open.fg_stroke = Stroke::new(1.0, TEXT);
    w.open.corner_radius = r;

    ctx.set_style_of(egui::Theme::Dark, style);
}
