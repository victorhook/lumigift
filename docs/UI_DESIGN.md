# Lumigift Studio: UI/UX design specification

Status: v1 spec, ready to implement · Target: `host/lumigift-studio` (Rust, eframe/egui 0.37, wgpu 30)
Inputs: `docs/ARCHITECTURE.md`, `lumigift-proto` (`Status`, `Identity`, `OtaBitmap`, `ImageHeader`), `lumigift-fx` (8 effects, 8 palettes), product photos in `images/`.

This document is the single source of truth for the look and behaviour of the app. Every colour, size and
duration in it is final. If something is missing, follow the principles in §1 and the closest existing pattern.

---

## 0. Research summary (why it looks the way it does)

What makes current pro creative tools feel premium. Each point maps to a decision later in the doc:

| Reference | What they do | What we take |
|---|---|---|
| **Linear** (2025 redesign) | Theme generated in **LCH** from 3 inputs (base, accent, contrast). They cut the blue tint in the chrome for a "more neutral and timeless appearance", and the redesign's stated goal is to "reduce visual noise… increase the hierarchy and density". Inter Display for headings, Inter for everything else. | A perceptual (OKLCH) neutral ramp with almost no chroma. One accent. Inter. |
| **Figma UI3** | Design principle "Minimize UI": focus "less on our UI and more on your work". Panels can be resized and collapsed. | The lights and the 3D stage are the content. The chrome recedes. |
| **Radix Colors** | 12-step scales with a job per step: 1–2 backgrounds, 3–5 component states, 6–8 borders, 9–10 solids, 11–12 text. Text steps are tuned to APCA Lc 60 / Lc 90. | The neutral scale below uses the same step roles. |
| **Tailwind v4** | The whole default palette is defined in OKLCH. Zinc is the cool neutral (hue ≈ 286, chroma ≤ 0.017). | Neutral hue 285 at chroma 0.008: a hint of cool, never visibly blue. |
| **Ableton Live 12** | Built-in dark, light and high-contrast themes with a cool/neutral/warm "Tone" option. Dense, flat, with colour reserved for clips and tracks. Keyboard-first (+/− zoom, Z/X zoom to selection). | Colour lives in content (cues, LEDs), not chrome. Ableton/Resolve-style timeline keys. |
| **grandMA3** | A `default` theme tuned "for optimal readability… under most lighting conditions" plus a `defaultDAYLIGHT` theme. Selection shown with a colour shift (white→yellow) on grey. | It is used in dark rooms, so near-black chrome with a high-contrast option. Selection is a clear colour shift. |
| **Resolume Arena** | A dark look "to minimize eye fatigue in dark environments". Single-window layout with panels you can hide. | A single window. The performance mode hides everything. |
| **Vercel Geist** | Monochrome UI. Accent and colour only for meaning. Geist Mono for technical strings. | Geist Mono for hashes, UIDs, timecode and logs. |
| **High-performance HMI (ISA-101 practice)** | A grey "normal" state. Colour is reserved for abnormal conditions so alarms stand out and operators don't tune them out. | Healthy gifts render neutral. Colour appears only when something needs attention (§4.1). |
| **NN/g** | Don't confirm routine actions ("if you cry wolf too many times, people will stop paying attention"). Prefer undo. Empty states should show system status, teach, and give a direct path. | The tiered safety model in §6.3 and the empty states in §6.4. |

Verified egui facts that shaped the design:

* egui 0.34 replaced `ab_glyph` with **skrifa + vello_cpu**. Text is hinted and sharp, and `FontTweak::coords` supports font
  variations. egui 0.35 added harfrust kerning and ligatures. **There is no runtime OpenType feature API**, so you
  cannot switch on `tnum` per label. We bake tabular figures into a font file instead (§7.2).
* egui 0.37: `default_fonts` is no longer a default egui feature. eframe still enables it, but we ship our own fonts anyway.
  `SidePanel`/`TopBottomPanel` became a single `egui::Panel` with `show(ui, …)`, and `App::update` became `App::ui`.
* `Shadow { offset: [i8; 2], blur: u8, spread: u8, color }`, `CornerRadius::same(u8)`, `Margin::same(i8)`.
  You can't blur or use backdrop filters, so translucent "glass" panels are just alpha fills.
* **Inter 4.1 has 745 glyphs in the Private Use Area (U+E000…)** that collide with Phosphor's codepoints (U+E0xx).
  In the other direction, the stock `Phosphor.ttf` also maps ASCII letters (used for its ligatures). So the icon font is
  **subset to its PUA codepoints** at build time, and only then placed first in the text fallback chains (§7.3).
* `egui-phosphor 0.14` and `egui_dock 0.21.1` both depend on **egui 0.36**, so they are *not* usable with 0.37 (checked
  on crates.io, 2026-11-01). We embed the Phosphor TTF directly and use a fixed panel layout instead of docking.

---

## 1. Design principles

1. **The light is the hero.** The chrome is neutral, near-black and colourless so LED colours, cue palettes and the 3D
   stage are the only saturated things on screen. The accent is used sparingly: primary action, selection, LIVE.
2. **Quiet when healthy, loud only when it matters.** A healthy fleet looks calm and grey. Colour, icons and
   motion are reserved for states that need a human, and they always carry a plain-language reason.
3. **Glanceable first, detailed on demand.** Tiles answer "is everything OK?" in under a second. The inspector, tooltips
   and the event log hold the numbers.
4. **Never ambiguous about the real world.** It must always be obvious whether you are driving the *real gifts* (LIVE)
   or a *preview*, which gifts an action will touch, and whether a firmware update succeeded.
5. **Keyboard-first, mouse-friendly.** Every action has a command palette entry, frequent ones have a shortcut,
   and the timeline follows Ableton/Resolve conventions so users' existing muscle memory works.
6. **Dense but breathable.** 4-pt spacing grid, 13 px body text, 28 px controls. Pro density like Linear or Ableton,
   with consistent alignment and generous gutters between groups.
7. **Motion explains, never decorates.** Short (90–260 ms) eased transitions show cause and effect. Nothing
   loops except the LIVE dot, spinners and the lights themselves.

---

## 2. Design tokens

### 2.1 Colour

All colours were designed in **OKLCH** and converted to sRGB (gamut-clamped by reducing chroma). The neutrals share
hue 285 and chroma 0.008. Contrast is the WCAG 2.x ratio, computed from the final hex values.

#### Neutral base (surfaces, borders, text)

| Token | Hex | OKLCH | Usage |
|---|---|---|---|
| `STAGE_BG` | `#050507` | 11.5% 0.006 285 | 3D viewport clear colour, performance mode. The darkest value, so the lights pop. |
| `BG_SUNKEN` | `#09090C` | 14.0% 0.008 285 | Wells: timeline lane bodies, text inputs, log, sparkline wells |
| `BG` | `#0E0E12` | 16.5% 0.008 285 | App background (`panel_fill`), the central area |
| `SURFACE_1` | `#141418` | 19.5% 0.008 285 | Rail, status bar, inspector, toolbars (level 1) |
| `SURFACE_2` | `#1B1B1F` | 22.2% 0.008 285 | Cards and gift tiles (level 2) |
| `SURFACE_3` | `#222226` | 25.2% 0.008 285 | Popovers, menus, dialogs, idle buttons (level 3) |
| `HOVER` | `#29292D` | 28.2% 0.008 285 | Hover fill on any surface |
| `ACTIVE` | `#303036` | 31.2% 0.010 285 | Pressed fill / selected list row (neutral selection) |
| `BORDER_SUBTLE` | `#242428` | 26.2% 0.008 285 | Separators, card outlines on BG |
| `BORDER` | `#323237` | 32.0% 0.008 285 | Default control outline, lane separators |
| `BORDER_STRONG` | `#4C4C51` | 42.0% 0.008 285 | Hovered outline, beat grid bar lines |
| `BORDER_INPUT` | `#636368` | 50.0% 0.008 285 | Boundaries that must meet 3:1 (inputs, slider rail, checkbox) |
| `TEXT_DISABLED` | `#68686F` | 52.0% 0.010 285 | Disabled text, stale (offline) values |
| `TEXT_FAINT` | `#888890` | 63.0% 0.012 285 | Timestamps, axis labels, units, placeholders |
| `TEXT_MUTED` | `#A7A7AF` | 73.0% 0.012 285 | Secondary labels, metadata |
| `TEXT_SECONDARY` | `#C3C3C9` | 82.0% 0.008 285 | Values in dense tables, icons |
| `TEXT` | `#F0F0F3` | 95.5% 0.004 285 | Primary text, names, headings (never pure white) |

#### Brand accent: "Lumi Orchid"

A violet-orchid that reads both festive (gift wrap, neon) and technical. Its hue (305) is far from every semantic
hue (green 155, amber 80, red 22, blue 240), so it can never be mistaken for a status.

| Token | Hex | OKLCH | Usage |
|---|---|---|---|
| `ACCENT` | `#BB78FE` | 70% 0.196 305 | Primary button fill (with **dark** text), LIVE pill, playhead, snap line, focus |
| `ACCENT_HOVER` | `#C896FF` | 76% 0.154 305 | Hover on accent fills |
| `ACCENT_PRESSED` | `#A964EB` | 64% 0.200 305 | Pressed accent |
| `ACCENT_TEXT` | `#D3ADFE` | 81% 0.118 305 | Accent-coloured text and links, selected-tab label |
| `ACCENT_SOFT` | `#37214D` | 30% 0.080 305 | Selected chip/tab background, selected cue outline glow |
| `SELECTION_BG` | `#4B2A6A` | 36% 0.110 305 | Text selection, selected rows when focus is in the list |
| `FOCUS` | `#CC9FFF` | 78% 0.140 305 | 2 px keyboard focus ring |
| `CANDLE` | `#FCC771` | 86% 0.120 78 | **Brand gradient only** (logo, splash, empty-state art): `ACCENT → CANDLE`. Never used for UI state. |

#### Semantic (tuned for dark backgrounds)

Each has a solid (icons, text, small marks) and a soft fill (badge or row background). Text on a soft fill uses the solid.

| Token | Hex | Soft token | Soft hex | Meaning |
|---|---|---|---|---|
| `OK` | `#4ED589` | `OK_SOFT` | `#0E311D` | Done, verified, connected (used sparingly: success moments, not steady states) |
| `WARN` | `#FABB41` | `WARN_SOFT` | `#3A2805` | Attention: degraded link, low battery, reconnecting |
| `ERROR` | `#FF6367` | `ERROR_SOFT` | `#4A191A` | Action required: failed OTA, bridge lost during LIVE, critical battery |
| `INFO` | `#62BBF5` | `INFO_SOFT` | `#0F2D40` | In progress / neutral news: updating, scanning, analysis |

#### Data visualisation

| Token | Hex | Usage |
|---|---|---|
| `VIZ_BASS` | `#F67D51` | Bass energy lane |
| `VIZ_MID` | `#2FD7B5` | Mid energy lane |
| `VIZ_TREBLE` | `#73B6FA` | Treble energy lane |
| `VIZ_LEVEL` | `#D6D6E5` | Waveform, overall level, neutral sparklines (healthy) |
| `VIZ_BEAT` | `#E3CA69` | Downbeat ticks in the ruler |
| `VIZ_SECTION` | `#DC95D5` | Section boundary markers. The drop marker uses `ACCENT` |
| `GRID` | `#2D2D33` | 3D floor grid lines, beat lines (non-bar) |
| `FLOOR` | `#0D0D10` | 3D floor albedo |

Sparklines are `VIZ_LEVEL` while healthy and switch to `WARN`/`ERROR` only when the metric is in that band.
The LED colours themselves come straight from `lumigift_fx::PALETTES` and are never themed.

#### WCAG contrast (computed)

Text and marks on each surface. AA needs **4.5** for body text and **3.0** for large text and UI parts.

| Foreground \ on | BG_SUNKEN | BG | SURFACE_1 | SURFACE_2 | SURFACE_3 | HOVER |
|---|---|---|---|---|---|---|
| `TEXT` | 17.48 | 16.94 | 16.15 | 15.09 | 13.94 | 12.74 |
| `TEXT_SECONDARY` | 11.34 | 10.98 | 10.47 | 9.79 | 9.04 | 8.26 |
| `TEXT_MUTED` | 8.32 | 8.06 | 7.69 | 7.19 | 6.63 | 6.06 |
| `TEXT_FAINT` | 5.65 | 5.48 | 5.22 | 4.88 | 4.51 | 4.12 ⚠ |
| `TEXT_DISABLED` | 3.60 | 3.48 | 3.32 | 3.11 | 2.87 | 2.62 (exempt: disabled) |
| `ACCENT` | 6.85 | 6.63 | 6.33 | 5.91 | 5.46 | 4.99 |
| `ACCENT_TEXT` | 10.59 | 10.25 | 9.78 | 9.14 | 8.44 | 7.71 |
| `OK` | 10.61 | 10.28 | 9.80 | 9.16 | 8.46 | 7.73 |
| `WARN` | 11.60 | 11.24 | 10.72 | 10.01 | 9.25 | 8.45 |
| `ERROR` | 6.85 | 6.63 | 6.33 | 5.91 | 5.46 | 4.99 |
| `INFO` | 9.42 | 9.12 | 8.70 | 8.13 | 7.51 | 6.86 |
| `VIZ_BASS / MID / TREBLE` | 7.55 / 10.90 / 9.28 | 7.31 / 10.56 / 8.99 | 6.97 / 10.07 / 8.57 | 6.52 / 9.41 / 8.01 | 6.02 / 8.69 / 7.40 | 5.50 / 7.94 / 6.76 |
| `FOCUS` (ring) | 9.44 | 9.15 | 8.72 | 8.15 | 7.53 | 6.88 |
| `BORDER_INPUT` (UI part) | 3.33 | 3.22 | 3.08 | 2.87 | 2.65 | 2.43 |

Other pairs: `BG` text on `ACCENT` **6.63** (so primary buttons use dark text, because `TEXT` on `ACCENT` is only 2.55 ✗).
`OK` on `OK_SOFT` 7.59 · `WARN` on `WARN_SOFT` 8.25 · `ERROR` on `ERROR_SOFT` 5.00 · `INFO` on `INFO_SOFT` 6.77 ·
`ACCENT_TEXT` on `ACCENT_SOFT` 7.52 · `TEXT` on `SELECTION_BG` 10.04 · `TEXT` on `ERROR_SOFT` 12.78 · `TEXT` on `ACTIVE` 11.53.

Rules that follow from the table:
* `TEXT_FAINT` is allowed on BG_SUNKEN…SURFACE_3 only, never on `HOVER` (4.12). For hovered rows, promote faint text to `TEXT_MUTED`.
* `BORDER_INPUT` is only used where the boundary identifies a control (inputs, slider rails, checkboxes) on BG or SURFACE_1.
  On SURFACE_2/3 those controls are identified by their `BG_SUNKEN` fill plus a label, and the border is decorative.
* `BORDER`/`BORDER_SUBTLE` are decorative (≤ 1.6:1) and never the only cue for a control.

### 2.2 Spacing (4-pt grid)

| Token | px | Typical use |
|---|---|---|
| `S0` | 2 | Icon-to-badge nudge, meter bar gap |
| `S1` | 4 | Inside chips, between stacked micro labels |
| `S2` | 6 | `item_spacing.y`, keycap padding |
| `S3` | 8 | `item_spacing.x`, icon-to-label, inside compact rows |
| `S4` | 12 | Tile inner padding, popover padding |
| `S5` | 16 | Card/tile gap, panel padding, dialog body |
| `S6` | 20 | Section gap in the inspector |
| `S7` | 24 | Page gutters (content area padding) |
| `S8` | 32 | Empty-state stack spacing |
| `S9` | 48 | Hero spacing (empty states, performance HUD margins) |

Control heights: **28 px** default (`interact_size.y`), **24 px** compact (table rows, lane headers), **36 px** primary
CTAs in dialogs and empty states. Hit targets are never below 24×24.

### 2.3 Corner radii

| Token | px | Use |
|---|---|---|
| `R_XS` | 3 | Keycaps, tiny badges, cue blocks in very dense zoom |
| `R_SM` | 6 | Buttons, inputs, chips, cue blocks, menu items |
| `R_MD` | 8 | Gift tiles, cards, OTA rows, inspector sections |
| `R_LG` | 12 | Popovers, dialogs, command palette, floating stage toolbars |
| `R_PILL` | 255 | Status pills, LIVE toggle, segmented control outer |

### 2.4 Elevation

There are no drop shadows on in-flow content. Level comes from fill and a 1 px border. Floating layers add a shadow.

| Level | Fill | Border | Shadow (`offset, blur, spread, color`) | Examples |
|---|---|---|---|---|
| 0 | `BG` | none | none | Central area |
| 1 | `SURFACE_1` | 1 px `BORDER_SUBTLE` (edge facing content) | none | Rail, inspector, status bar, toolbar |
| 2 | `SURFACE_2` | 1 px `BORDER_SUBTLE` | none | Tiles, cards, OTA rows |
| 3 | `SURFACE_3` | 1 px `BORDER` | `[0, 8], 24, 0, #000000 @ 55%` | Menus, popovers, tooltips, toasts |
| 4 | `SURFACE_3` | 1 px `BORDER` | `[0, 16], 48, 0, #000000 @ 65%` + scrim `#000000 @ 60%` | Modals, command palette |

**Top highlight:** level 2–4 frames also draw a 1 px line just inside the top edge, inset by the radius, in `#FFFFFF`
at 4% alpha (`Color32::from_rgba_premultiplied(10,10,10,10)`). It is a subtle "lit from above" cue that makes
flat surfaces look machined. Paint it after the frame with `painter.hline`.

### 2.5 Typography

Fonts: **Inter 4.1** (UI), **Inter Tnum** (Inter with `tnum` frozen in, for all live numbers), **Geist Mono 1.9**
(hashes, UIDs, hex, timecode, log). See §7.2 for how to build and register them.

| Style (`TextStyle`) | Family | Size / line | Weight | Use |
|---|---|---|---|---|
| `display` | Inter | 28 / 34 | 600 | Empty-state headline, performance HUD title |
| `Heading` | Inter | 18 / 24 | 600 | Page title in the page header |
| `title` | Inter | 15 / 20 | 600 | Dialog title, inspector gift name |
| `Body` | Inter | 13 / 18 | 400 | Default text |
| `Button` | Inter | 13 / 18 | 500 | Buttons, tabs, menu items |
| `label` | Inter | 12 / 16 | 500 | Field labels, tile name (when compact), table headers |
| `Small` | Inter | 11.5 / 14 | 400 | Metadata, tooltips' secondary line, tile meta row |
| `overline` | Inter | 11 / 14 | 600 | Section headers, UPPERCASE, `extra_letter_spacing(0.6)`, `TEXT_FAINT` |
| `metric` | Inter Tnum | 20 / 24 | 500 | Big readouts (inspector, OTA overall %) |
| `value` | Inter Tnum | 13 / 18 | 500 | Telemetry values in tiles and tables |
| `value_small` | Inter Tnum | 11.5 / 14 | 400 | Meta row numbers |
| `timecode` | Geist Mono | 22 / 26 | 500 | Transport position `01:23.456` |
| `Monospace` | Geist Mono | 12 / 16 | 400 | Git hash, UID, log lines, CRC |

Rules: no italics; at most two weights per component; units stay in `TEXT_FAINT` at the same size as the
value (`1.38` `V`); sentence case everywhere except overlines.

### 2.6 Motion

| Token | Duration | Easing (`egui::emath::easing`) | Use |
|---|---|---|---|
| `MOTION_FAST` | 90 ms | `cubic_out` | Hover/press colour, icon tint |
| `MOTION_BASE` | 160 ms | `cubic_out` | Toggles, chips, tooltips, palette open, toasts in |
| `MOTION_SLOW` | 260 ms | `cubic_in_out` | Panel slide, inspector open, page cross-fade, state colour changes |
| `MOTION_DELIBERATE` | 400 ms | `cubic_out` | OTA success check stroke, LIVE engage glow, camera preset moves (600 ms) |
| `LOOP_LIVE` | 1600 ms period | sine | LIVE dot breathing (alpha 60→100%) |

`style.animation_time = 0.16`. All animations use `ctx.animate_value_with_time(id, target, secs)` or
`animate_bool_with_time_and_easing`. The setting **Reduce motion** sets all durations to 0 (except progress bars)
and disables camera auto-orbit and beat push.

### 2.7 Ready-to-paste `theme` module

```rust
//! host/lumigift-studio/src/theme.rs
use egui::{Color32, CornerRadius, FontFamily, FontId, Margin, Shadow, Stroke, TextStyle, Vec2};

pub mod theme {
    use egui::Color32;
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
    pub const ON_ACCENT: Color32 = BG; // text/icons on ACCENT fills
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
    pub const FLOOR: Color32 = c(0x0D0D10);
    // Overlays (premultiplied)
    pub const SCRIM: Color32 = Color32::from_black_alpha(153); // 60%
    pub const TOP_HIGHLIGHT: Color32 = Color32::from_rgba_premultiplied(10, 10, 10, 10); // white @ 4%

    // Spacing
    pub const S0: f32 = 2.0; pub const S1: f32 = 4.0; pub const S2: f32 = 6.0; pub const S3: f32 = 8.0;
    pub const S4: f32 = 12.0; pub const S5: f32 = 16.0; pub const S6: f32 = 20.0; pub const S7: f32 = 24.0;
    pub const S8: f32 = 32.0; pub const S9: f32 = 48.0;
    pub const CONTROL_H: f32 = 28.0; pub const CONTROL_H_COMPACT: f32 = 24.0; pub const CTA_H: f32 = 36.0;
    // Radii
    pub const R_XS: u8 = 3; pub const R_SM: u8 = 6; pub const R_MD: u8 = 8; pub const R_LG: u8 = 12; pub const R_PILL: u8 = 255;
    // Motion (seconds)
    pub const MOTION_FAST: f32 = 0.09; pub const MOTION_BASE: f32 = 0.16;
    pub const MOTION_SLOW: f32 = 0.26; pub const MOTION_DELIBERATE: f32 = 0.40;
    // Layout
    pub const RAIL_W: f32 = 64.0; pub const RAIL_W_COMPACT: f32 = 52.0;
    pub const PAGE_HEADER_H: f32 = 48.0; pub const STATUS_BAR_H: f32 = 28.0;
    pub const INSPECTOR_W: f32 = 360.0; pub const TILE_W_MIN: f32 = 248.0;
}

pub fn shadow_popover() -> Shadow {
    Shadow { offset: [0, 8], blur: 24, spread: 0, color: Color32::from_black_alpha(140) }
}
pub fn shadow_modal() -> Shadow {
    Shadow { offset: [0, 16], blur: 48, spread: 0, color: Color32::from_black_alpha(166) }
}

/// Card/tile frame (level 2). Use `.fill(theme::HOVER)` etc. for states.
pub fn card() -> egui::Frame {
    egui::Frame::new()
        .fill(theme::SURFACE_2)
        .stroke(Stroke::new(1.0, theme::BORDER_SUBTLE))
        .corner_radius(CornerRadius::same(theme::R_MD))
        .inner_margin(Margin::same(12))
}
pub fn popover() -> egui::Frame {
    egui::Frame::new()
        .fill(theme::SURFACE_3)
        .stroke(Stroke::new(1.0, theme::BORDER))
        .corner_radius(CornerRadius::same(theme::R_LG))
        .inner_margin(Margin::same(12))
        .shadow(shadow_popover())
}

pub fn apply(ctx: &egui::Context) {
    use theme::*;
    ctx.set_theme(egui::ThemePreference::Dark);
    let mut style = (*ctx.style_of(egui::Theme::Dark)).clone();

    // Text styles
    let fam = |n: &str| FontFamily::Name(n.into());
    style.text_styles = [
        (TextStyle::Name("display".into()), FontId::new(28.0, fam("semibold"))),
        (TextStyle::Heading, FontId::new(18.0, fam("semibold"))),
        (TextStyle::Name("title".into()), FontId::new(15.0, fam("semibold"))),
        (TextStyle::Body, FontId::new(13.0, FontFamily::Proportional)),
        (TextStyle::Button, FontId::new(13.0, fam("medium"))),
        (TextStyle::Name("label".into()), FontId::new(12.0, fam("medium"))),
        (TextStyle::Small, FontId::new(11.5, FontFamily::Proportional)),
        (TextStyle::Name("overline".into()), FontId::new(11.0, fam("semibold"))),
        (TextStyle::Name("metric".into()), FontId::new(20.0, fam("tnum-medium"))),
        (TextStyle::Name("value".into()), FontId::new(13.0, fam("tnum-medium"))),
        (TextStyle::Name("value_small".into()), FontId::new(11.5, fam("tnum"))),
        (TextStyle::Name("timecode".into()), FontId::new(22.0, fam("mono-medium"))),
        (TextStyle::Monospace, FontId::new(12.0, FontFamily::Monospace)),
    ]
    .into();

    // Spacing
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
    style.interaction.selectable_labels = false; // labels are not text-selectable (except log/inspector values, opt-in)

    // Visuals
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
    w.noninteractive.bg_stroke = Stroke::new(1.0, BORDER_SUBTLE); // separators
    w.noninteractive.fg_stroke = Stroke::new(1.0, TEXT_SECONDARY); // label text
    w.noninteractive.corner_radius = r;
    w.noninteractive.expansion = 0.0;

    w.inactive.bg_fill = BG_SUNKEN; // checkbox box, slider rail, radio
    w.inactive.weak_bg_fill = SURFACE_3; // button fill
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
```

Call `fonts::install(ctx)` (see §7.2) and then `theme::apply(ctx)` once in the `eframe` creation closure.
Return `theme::STAGE_BG` from `App::clear_color` so window resizes never flash grey.

Custom widgets that egui's `Visuals` can't express (primary button, segmented control, status pill, toggle switch,
meters) are small functions in `widgets.rs` that paint with `ui.painter()` using these tokens (§4).

---

## 3. App layout and navigation

### 3.1 Navigation model

* **One window, six destinations** in a left rail: **Fleet** (home), **Studio**, **Lighting**, **Stage**, **Firmware**, **Settings**.
  Shortcuts `Ctrl+1…6`. The selected rail item gets an `ACCENT_SOFT` pill behind the icon, an `ACCENT_TEXT` icon, and a 3×16 px `ACCENT` bar on the rail's left edge.
* This is Linear's "inverted L": the rail (left) plus a page header (top) frame the content. A **status bar** at the bottom is always
  visible and shows global truth: bridge, online count, LIVE/Preview, audio output, SIM.
* **Global state lives in the status bar, not in pages.** The LIVE toggle appears in the Studio toolbar *and* the status bar,
  bound to the same state.
* The **inspector** (right, 360 px) is contextual. It shows the selected gift (Fleet, Stage), the selected cue(s) (Studio) or
  the image details (Firmware). Toggle with `Ctrl+I`. When nothing is selected it shows a summary.
* **Performance mode** (`F11` or from Stage/Studio) is a separate full-screen presentation of the stage (§5.7).
* Modal dialogs are only used for OTA start, Reboot all, unsaved changes and destructive file actions (§6.3).

egui structure (top-level `App::ui`):

```text
Panel::left("rail").exact_size(RAIL_W)            (SURFACE_1, right border BORDER_SUBTLE)
Panel::bottom("status").exact_size(STATUS_BAR_H)  (SURFACE_1, top border)
Panel::right("inspector").resizable(true).size_range(300.0..=480.0).default_size(360.0)  .show_collapsible(...)
CentralPanel (BG)  ->  page header (48 px) + page body
```

### 3.2 Wireframes at 1600×1000

Legend: `[gem]` = live 2×2 LED swatch, `▂▄▆█` = link meter, `▭▭▭▯` = battery glyph, `~~~` = sparkline, `⋯` = overflow menu.

#### Fleet (home)

```text
┌──────┬───────────────────────────────────────────────────────────────────────────────┬──────────────────────────────┐
│ (gift│ Fleet   10 gifts · 9 online · 1 needs attention   [⌕ Filter  ] [Slot ▾] [▦|☰] [⟳ Rescan] │ Kitchen            #03  ⋯ │
│ logo)│───────────────────────────────────────────────────────────────────────────────│ ▲ Needs attention            │
│      │ ┌─────────────────┐ ┌─────────────────┐ ┌─────────────────┐ ┌─────────────────┐│ ┌────────┐  Low battery      │
│ [■]  │ │[gem] Tree    #01│ │[gem] Desk    #02│ │[gem] Kitchen #03│ │[gem] Hall    #04││ │  gem   │  1.12 V · ~20%   │
│Fleet │ │      Online     │ │      Online     │ │      ▲ Attention│ │      ○ Offline  ││ │  64px  │  Replace the AAA  │
│      │ │▂▄▆█ 98%  ~~~~~~ │ │▂▄▆█ 97%  ~~~~~~ │ │▂▄▆_ 81%  ~~~\_~ │ │---- —     ······ ││ └────────┘                   │
│ [◇]  │ │▭▭▭▯ 1.41 V      │ │▭▭▭▯ 1.39 V      │ │▭▯▯▯ 1.12 V      │ │last seen 2m ago  ││ [◉ Blink] [# Slot] [✎ Name]  │
│Studio│ │3h 12m · 99.6% rx│ │3h 12m · 99.1% rx│ │41m · 88.0% rx   │ │                  ││                              │
│      │ └─────────────────┘ └─────────────────┘ └─────────────────┘ └─────────────────┘│ LINK                         │
│ [◎]  │ ┌─────────────────┐ ┌─────────────────┐ ┌─────────────────┐ ┌─────────────────┐│ Quality 81%  ~~~~~~~~~~~~~~~ │
│Stage │ │ … row 2         │ │                 │ │                 │ │                 ││ Retries 7/min · RX 88.0%     │
│      │ └─────────────────┘ └─────────────────┘ └─────────────────┘ └─────────────────┘│ Sync ±0.31 ms · 21 °C        │
│ [⇪]  │ ┌─────────────────┐ ┌─────────────────┐                                       │ POWER                        │
│Firmw.│ │ … row 3         │ │                 │                                       │ 1.12 V ▭▯▯▯ ~~~~~~~ (30 min) │
│      │ └─────────────────┘ └─────────────────┘                                       │ HEALTH                       │
│      │───────────────────────────────────────────────────────────────────────────────│ Uptime 41m · Resets 2        │
│      │ EVENTS  [All] [Warnings] [Errors]   [Kitchen ×]                     [↓ Latest]│ Last: Brownout, 41 m ago     │
│      │ 12:01:33  ▲  Kitchen   Battery low (1.12 V)                                   │ FIRMWARE                     │
│ [⚙]  │ 11:58:10  ●  Hall      Went offline                                           │ 2026-10-31 14:02  (latest)   │
│Set.  │ 11:20:02  ↻  Kitchen   Rebooted: brownout                                     │ UID  3A9F·11C2·…  hw rev A   │
│      │                                                                               │ [Reboot]  [Reboot to bootl…] │
├──────┴───────────────────────────────────────────────────────────────────────────────┴──────────────────────────────┤
│ ● Bridge · ch 76 · 412 pkt/s · 0.2% fail │ 9/10 online │ ○ Preview │ ♪ Speakers (PipeWire) │              │ 60 fps │
└─────────────────────────────────────────────────────────────────────────────────────────────────────────────────────┘
```

Sizes: content area 1176 px wide (1600 − 64 − 360). Grid padding 24, gap 16, so **4 columns of 270 × 176** tiles.
The event drawer sits at the bottom of the central panel: 200 px open, a 32 px header when collapsed (`Ctrl+J`).
Its open/closed state is persisted.

**Scaling 1 → 30 gifts.** Columns = `floor((w + 16) / (TILE_W_MIN + 16))`, clamped 1…6, and tiles stretch to fill.
* 1–16 gifts: **comfortable tiles** (176 px tall, all rows).
* 17–30 gifts: **compact tiles** (112 px: swatch 32 px, name + slot, status, link %, battery V. Meta row hidden).
  This fits 28 tiles in view, or 35 with the inspector closed.
* **List view** (`☰`, any count): an `egui_extras::TableBuilder` table, 32 px rows, sortable columns:
  Swatch · Name · Slot · State · Link % (+ 80 px sparkline) · RX % · Battery · FW · Uptime · Resets · Sync.
  It is suggested once (a dismissible hint) when > 20 gifts are known.
* Sort: Slot (default), Name, Health (attention first), Battery, Link. A filter text box matches name, slot and UID.
* Tile order is stable. Tiles never jump when values change, only on explicit re-sort. "Health" sorting only re-sorts on demand.

#### Show studio

```text
┌──────┬──────────────────────────────────────────────────────────────────────────────────────────────────────────────┐
│ rail │ Jingle Bell Rock.flac ▾   128.0 BPM · 4/4 · 3:42   [✦ Auto-generate] │ Snap [1 beat ▾] │ Latency 42 ms ━━●━━ │ (●LIVE) │
│      ├──────────────────────────────────────────────────────────────────────────────┬─────────────────────────────┤
│      │                                                                              │ CUE  @ 1:02.250 · bar 33    │
│      │                   3D stage preview (wgpu callback)                           │ Effect  [Chase        ▾]    │
│      │          glowing boxes on dark floor, labels on hover                        │ Palette [■■■■ Neon    ▾]    │
│      │                                                                              │ Bright. ━━━━━━━●━━  80%     │
│      │                                        [⌖ Frame] [⛶ Perform]                 │ Hue     ━━●━━━━━━━  40      │
│      │                                                                              │ Param   ━━━━●━━━━━  128     │
│      ├──────────────────────────────────────────────────────────────────────────────┴─────────────────────────────┤
│      │ ⏮  ▶  ⏭  ⟲   01:02.250   33.1.1   ‖ ─────────────────────── overview minimap [====]──────── │ ⊖ ━●━ ⊕ ⤢ │
│      ├────────────┬───────────────────────────────────────────────────────────────────────────────────────────────┤
│      │            │ 29        30        31        32      ┃ 33        34        35        36                    │
│      │ Sections   │ ▕ Verse 2 ─────────────────────────── ┃▕ Chorus ──────────────── ⚡Drop ─────────────────     │
│      │ Waveform   │ ▁▃▅▇▅▃▁▃▅▇█▇▅▃▁▃▅▇▅▃▁▃▅▇█▇▅▃▁▃▅▇▅▃▁ ┃▃▅▇█▇▅▃▅▇█▇▅▃▅▇█▇█▇█▇█▇█▇█▇▅                      │
│      │ Bass       │ ▁▁▃▃▁▁▃▃▁▁▃▃▁▁▃▃▁▁▃▃▁▁▃▃▁▁▃▃▁▁▃▃▁ ┃▃▅▃▅▃▅▃▅▃▅▃▅▃▅▇▇▇▇▇▇▇▇▇▇                          │
│      │ Mid        │ ▂▂▂▃▃▂▂▂▃▃▂▂▂▃▃▂▂▂▃▃▂▂▂▃▃▂▂▂▃▃▂▂ ┃▃▃▄▄▃▃▄▄▃▃▄▄▅▅▆▆▆▆▆▆▆                            │
│      │ Treble     │ ▁▂▁▂▁▂▁▂▁▂▁▂▁▂▁▂▁▂▁▂▁▂▁▂▁▂▁▂▁▂▁ ┃▂▃▂▃▂▃▂▃▂▃▂▃▃▄▄▄▄▄▄▄▄                            │
│      │ Cues       │ ▐ Pulse · Christmas · 70% ────────── ┃▐▓ Chase · Neon · 80% ▓▓▓▓▓▓▓▐ Strobe · Ice · 100%│
│      │ One-shots  │        ◆ Flash          ◆ Flash      ┃     ◆ Blackout              ◆ Flash               │
│      ├────────────┴───────────────────────────────────────────────────────────────────────────────────────────────┤
│      │ status bar                                                                                                  │
└──────┴─────────────────────────────────────────────────────────────────────────────────────────────────────────────┘
```

Vertical budget: toolbar 48 · upper split 380 (stage | inspector 360) · transport 44 · timeline ≈ 500 · status bar 28.
The split between the stage and the timeline is a draggable horizontal divider (stage height 200–600 px, persisted).
Lane heights: ruler 32 (the section band is drawn inside the ruler's lower 10 px when the Sections lane is hidden),
Sections 24, Waveform 96, Bass/Mid/Treble 28 each, Cues 64, One-shots 28. The lane header column is 120 px wide.
Lanes can be collapsed to 12 px by clicking their header, and the state is persisted per show.

#### Lighting

```text
┌──────┬──────────────────────────────────────────────────────────────────┬─────────────────────────────┐
│ rail │ Lighting   Generic looks, rendered on the gifts…   (● Driving gifts)│ OUTPUT  layers (target·look)│
│      ├──────────────────────────────────────────────────────────────────┤ QUICK   Master ━━●  Crossfade│
│      │            3D stage preview (stage_preview, click selects)        │         [☾ Fade out][Blackout]│
│      ├──────────────────────────────────────────────────────────────────┤ TARGET  [All|Selected|Group] │
│      │ EFFECTS  Click to apply to All gifts                              │         ▣ Window 4  [+ Group]│
│      │ ┌────────┐┌────────┐┌────────┐┌────────┐┌────────┐┌────────┐      │ LOOK    palette swatches 3×3 │
│      │ │▪▪▪▪▪▪▪▪││ live   ││ preview││        ││        ││        │      │         colours / hue, speed,│
│      │ │○ Solid ││ Breathe││ Fade   ││Rainbow♪││Twinkle♪││Waterfal│      │         size, brightness     │
│      │ └────────┘└────────┘└────────┘└────────┘└────────┘└────────┘      │         [Apply to All gifts] │
│      │ …                                                                 │ FAVOURITES  ★ rows           │
└──────┴──────────────────────────────────────────────────────────────────┴─────────────────────────────┘
```

Effect cards (`SURFACE_2`, `R_MD`, ≥ 172 × 112): a `BG_SUNKEN` well with a live preview of 8 mini gifts (2×2 LEDs
like the gems, soft glow), then the effect icon + name (`label`) and a `music-notes` glyph if the look also reacts to
music. The look currently on the target gets a 1.5 px `ACCENT` outline and a soft accent halo. With *Reduce motion*
the previews freeze. The ownership pill in the header mirrors the LIVE pill (accent when the page drives the gifts).
Flashing looks (Blink, Police, Timed strobe) respect *Limit flashing*: speed capped at ~3 Hz, Police forced smooth.
Page shortcuts: `Enter` apply, `F` fade out/in, `B` blackout/restore.

#### Stage (standalone)

```text
┌──────┬────────────────────────────────────────────────────────────────────────────────┬────────────────────────────┐
│ rail │ Stage  Living room ▾      [View] [Arrange]       Cam: [Front][Top][¾]  [⛶ Perform] │ inspector (selected gift)  │
│      ├────────────────────────────────────────────────────────────────────────────────┤                            │
│      │ ┌─────────────┐                                                                 │                            │
│      │ │ ARRANGE     │            full-bleed 3D viewport (STAGE_BG)                    │                            │
│      │ │ ▸ Select  V │                                                                 │                            │
│      │ │ ✥ Move    G │         ▢ #01   ▢ #02   ▢ #03   ▢ #04   ▢ #05                    │                            │
│      │ │ ⟳ Rotate  R │                                                                 │                            │
│      │ │ ─────────── │              ▢ #06   ▢ #07   ▢ #08   ▢ #09   ▢ #10              │                            │
│      │ │ Row  Grid   │                                                                 │                            │
│      │ │ Arc  Circle │                                                                 │                            │
│      │ │ Snap 0.25 ▾ │                                                                 │                            │
│      │ │ Number L→R  │      ┌──────────────────────────────────────────────┐          │                            │
│      │ └─────────────┘      │ ▶ 01:02.25 ━━━━━━●━━━━━━━  Jingle Bell Rock   │ (mini   │                            │
│      │                      └──────────────────────────────────────────────┘ transport)                            │
├──────┴────────────────────────────────────────────────────────────────────────────────┴────────────────────────────┤
```

Floating panels: `SURFACE_2` at 92% alpha, `R_LG`, level-3 shadow, 12 px from the viewport edges. Without a show
loaded, the mini transport is hidden and the boxes show live hardware state.

#### Firmware update

```text
┌──────┬──────────────────────────────────────────────────────────────────────────────┬─────────────────────────────┐
│ rail │ Firmware update                                                               │ IMAGE                       │
│      ├──────────────────────────────────────────────────────────────────────────────┤ lumigift-app.lgfw           │
│      │ ┌──────────────────────────────────────────────────────────────────────────┐ │ Version  2026-10-31 14:02   │
│      │ │ [⇪]  lumigift-app.lgfw              2026-10-31 14:02 · a1b2c3d4 · 21.4 KiB│ │ Git      a1b2c3d4e5f6a7b8   │
│      │ │      ✓ Header valid · hw rev A · CRC 0x5A17C0DE       [Choose another…]   │ │ Size     21.4 KiB (914 ch.) │
│      │ └──────────────────────────────────────────────────────────────────────────┘ │ HW rev   A                  │
│      │ TARGETS   [Select: All · Outdated · None]           8 of 10 selected         │ App CRC  0x5A17C0DE         │
│      │ ┌──────────────────────────────────────────────────────────────────────────┐ │                             │
│      │ │ ☑ [gem] Tree     #01  2026-10-12 → 2026-10-31   ▭▭▭▯ 1.41 V  ▂▄▆█ 98%    │ │ Compared to fleet           │
│      │ │ ☑ [gem] Desk     #02  2026-10-12 → 2026-10-31   ▭▭▭▯ 1.39 V  ▂▄▆█ 97%    │ │ Newer than 8 gifts          │
│      │ │ ☐ [gem] Kitchen  #03  ▲ Battery low: excluded   ▭▯▯▯ 1.12 V              │ │ Same as 1 · Offline 1       │
│      │ │ ☐ [gem] Hall     #04  Offline                                            │ │                             │
│      │ │ ☑ …                                                                      │ │ Safe to interrupt: a gift   │
│      │ └──────────────────────────────────────────────────────────────────────────┘ │ that loses power stays in   │
│      │                                                                               │ recovery mode; re-run.      │
│      │                                   ~25 s · 8 gifts      [ Update 8 gifts ]     │                             │
├──────┴──────────────────────────────────────────────────────────────────────────────┴─────────────────────────────┤
```

While running, the same page switches to the run view (§4.3): an overall progress card on top and per-gift rows below.
The target checkboxes are locked and the CTA becomes `Cancel` (secondary).

#### Settings

```text
┌──────┬───────────────────┬─────────────────────────────────────────────────────────────────────────────────────────┐
│ rail │ Radio            ▸│ RADIO                                                                                   │
│      │ Power & brightness│ Channel  [76 ▾]  2476 MHz        [Scan channels]                                        │
│      │ Audio             │ ┌───────────────────────────────────────────────────────────────────────────────────┐   │
│      │ Simulator         │ │  ▁ ▁▂▃▅▇▇▅▃▂▁ ▁ ▁▂▃▅▇▇▅▃▂▁▁ ▁▁▂▃▅▇▇▅▃▂▁   ▁       ▁  ┃       ▁               │   │
│      │ Appearance        │ │ ░Wi-Fi 1░░░░░░░   ░░Wi-Fi 6░░░░░░   ░░Wi-Fi 11░░░░    │37   │38        ┃76  │39    │   │
│      │ Shortcuts         │ │ 0        20        40        60        80       100      125   (2400–2525 MHz)   │   │
│      │ About             │ └───────────────────────────────────────────────────────────────────────────────────┘   │
│      │                   │ ★ Quietest: 76, 81, 90                                                                  │
└──────┴───────────────────┴─────────────────────────────────────────────────────────────────────────────────────────┘
```

Settings use a 220 px section list and a content column of at most 720 px, left-aligned. There is no inspector.

### 3.3 Compact layout (< 1280 px wide or < 760 px tall; minimum window 1024 × 640)

* The rail shrinks to **52 px, icons only** (labels become tooltips).
* The **inspector becomes an overlay sheet**: it slides in from the right over the content, with a level-3 shadow and a scrim click to close.
  The app does not reserve space for it.
* Fleet: 3 columns of comfortable tiles at 1024, or compact tiles automatically when height < 760.
* Studio: the upper split becomes a **tab strip** `[Stage | Cue]` (stage height 220). The lane header column
  shrinks to 32 px (icon only). The toolbar wraps the latency slider into a `⋯` overflow popover.
* Firmware: the image card and target list stack vertically, and image details move into an expandable section.
* Status bar: hides the audio output and fps segments.
* Breakpoints are evaluated every frame from `ui.available_rect_before_wrap()`. Switching layouts never loses state.

### 3.4 Command palette (`Ctrl+K`)

* Opens as a level-4 overlay, **560 × max 440 px**, centred horizontally, 18% from the top. The scrim is `SCRIM` at 40% (lighter than modals).
  It enters with a 120 ms fade plus an 8 px slide down.
* Anatomy: a 44 px search field (Phosphor magnifier, `Body` text, placeholder "Type a command or search gifts…"), then
  grouped results, 32 px rows: icon (16, `TEXT_MUTED`) · label (`TEXT`) · context (`TEXT_FAINT`, e.g. "Kitchen · #03")
  · right-aligned keycaps. The highlighted row gets `ACTIVE` fill plus a 2 px `ACCENT` left bar.
* Groups, in order when the query is empty: **Recent** (last 5 executed), **Suggestions** (contextual: "Blink selected gift",
  "Insert cue at playhead"), then everything grouped as **Navigation, Gifts, Show, Transport, Firmware, View, Settings**.
* Matching: fuzzy subsequence over label + aliases + context (e.g. "blink kit" → "Blink Kitchen"). Results are ranked by score
  then recency. Each gift generates its own commands: Blink, Select, Rename, Assign slot, Reboot.
* Prefixes: `>` commands only, `@` gifts only, `#` jump to slot, `:` go to time (`:1:02.5` or `:33` = bar 33).
* Keyboard: `↑/↓` move, `Enter` run, `Ctrl+Enter` run and keep open, `Esc` close, `Tab` cycles groups.
* Commands that need confirmation open their normal confirmation after the palette closes. The palette never bypasses safety.
* Empty result: "No matches for 'xyz'" plus a "Search event log for 'xyz'" row.

### 3.5 Keyboard shortcuts

`Ctrl` is `Cmd` on macOS. Shortcuts are disabled while a `TextEdit` has focus, except for `Esc` and `Ctrl+…`
(check with `KeyboardShortcut::conflicts_with_text_editing`). Keycaps render as `R_XS` boxes: `SURFACE_3` fill,
1 px `BORDER`, `Small` text in `TEXT_MUTED`.

| Scope | Keys | Action |
|---|---|---|
| Global | `Ctrl+K` | Command palette |
| | `Ctrl+1…6` | Fleet / Studio / Lighting / Stage / Firmware / Settings |
| | `Ctrl+I` | Toggle inspector |
| | `Ctrl+J` | Toggle event log drawer |
| | `Ctrl+L` | Toggle LIVE (on → see §6.3) |
| | `Ctrl+Shift+B` | **Blackout** all gifts (instant, also works from performance mode) |
| | `F11` | Performance mode (`Esc` exits) |
| | `Ctrl+=` / `Ctrl+-` / `Ctrl+0` | UI zoom in / out / reset (`ctx.set_zoom_factor`, 0.8–1.5) |
| | `?` | Shortcut cheat sheet overlay |
| Fleet | `←↑→↓` | Move selection across tiles |
| | `Enter` | Open selected in inspector |
| | `B` | Blink / identify selected gift(s) |
| | `F2` | Rename |
| | `S` | Assign slot |
| | `Ctrl+R` | Rescan (discovery) |
| | `Ctrl+A` | Select all gifts |
| | `/` | Focus filter |
| Studio: transport | `Space` | Play / pause |
| | `Home` / `End` | Go to start / end |
| | `L` | Toggle loop |
| | `I` / `O` | Set loop in / out at playhead (or to the selection when cues are selected) |
| | `←` / `→` | Nudge playhead (or selected cues) by one snap unit |
| | `Shift+←/→` | Nudge by one bar |
| | `Ctrl+←/→` | Previous / next section marker |
| | `Enter` | Return playhead to where playback started |
| Studio: view | `Ctrl+Wheel`, `Alt+Wheel`, pinch | Zoom horizontally around the mouse pointer |
| | `Wheel` | Scroll lanes vertically (when they overflow), else horizontal scroll |
| | `Shift+Wheel` | Scroll horizontally |
| | `+` / `-` | Zoom in / out around the playhead |
| | `Z` / `Shift+Z` | Zoom to selection / zoom to fit song |
| | `X` | Back to previous zoom (Ableton) |
| | `F` | Toggle follow playhead |
| Studio: edit | `C` | Insert cue at playhead (copies previous cue's settings) |
| | `M` | Add one-shot marker at playhead |
| | `N` | Toggle snap (Resolve) · hold `Alt` while dragging to bypass snap temporarily |
| | `1…8` | Set effect of selected cue(s) (Solid, Pulse, Spectrum, Chase, Strobe, Rainbow, Sparkle, Breathe) |
| | `Shift+1…8` | Set palette (Christmas, Fire, Ocean, Neon, Aurora, Sunset, Candy, Ice) |
| | `[` / `]` | Brightness −/+ 10% |
| | `Delete` / `Backspace` | Delete selection |
| | `Ctrl+D` | Duplicate selection to after itself |
| | `Ctrl+C` / `Ctrl+X` / `Ctrl+V` | Copy / cut / paste at playhead |
| | `Ctrl+Z` / `Ctrl+Shift+Z` | Undo / redo (unbounded within the session) |
| | `Ctrl+S` / `Ctrl+Shift+S` / `Ctrl+O` | Save / Save as / Open `.lgshow` |
| | `Ctrl+Shift+O` | Open audio file |
| | `Ctrl+G` | Auto-generate show (asks before replacing existing cues; undoable) |
| Stage | Mouse: LMB drag / RMB or MMB drag / wheel | Orbit / pan / dolly |
| | `F` | Frame all · double-click a box focuses it |
| | `1` / `2` / `3` | Front / Top / Three-quarter camera |
| | `A` | Toggle Arrange mode · in Arrange: `G` move, `R` rotate 15°, `Q/E` rotate ∓15° |
| | `O` | Toggle auto-orbit |
| | `H` | Toggle labels |

---

## 4. Component specs

All components use `theme::*` tokens. "Hover" means `response.hovered()` and "active" means `is_pointer_button_down_on()`.
Disabled components are drawn with `disabled_alpha` 0.45 and their hover state is suppressed. Every disabled control has a tooltip
explaining **why** it is disabled ("Connect the bridge to blink gifts").

### 4.0 Base controls

| Control | Default | Hover | Active | Disabled | Notes |
|---|---|---|---|---|---|
| **Primary button** | `ACCENT` fill, `ON_ACCENT` text (500), `R_SM`, h 28 (36 in dialogs) | `ACCENT_HOVER` | `ACCENT_PRESSED` | `SURFACE_3` fill, `TEXT_DISABLED` | At most **one** per view. Paint manually: `ui.add(Button::new(..).fill(..))` with a RichText colour |
| **Secondary button** | `SURFACE_3` fill, `TEXT_SECONDARY` | `HOVER`, `TEXT` | `ACTIVE` | alpha 0.45 | Default egui button with theme visuals |
| **Ghost / icon button** | transparent, icon `TEXT_MUTED`, 28×28, `R_SM` | `HOVER` fill, icon `TEXT` | `ACTIVE` | alpha 0.45 | Tooltip with label + keycap mandatory |
| **Danger button** | `SURFACE_3` fill, `ERROR` text | `ERROR_SOFT` fill | `ERROR_SOFT` + 1 px `ERROR` | alpha 0.45 | Becomes solid `ERROR` fill with `BG` text only inside confirmation dialogs |
| **Segmented control** | `BG_SUNKEN` track, `R_PILL`, 2 px inset. Selected segment `SURFACE_3` with 1 px `BORDER`, `TEXT`; others `TEXT_MUTED` | segment `HOVER` | — | — | Selected thumb slides with `MOTION_BASE` |
| **Toggle switch** | 32×18 track `BG_SUNKEN` + 1 px `BORDER_INPUT`, thumb 14 px `TEXT_MUTED` | track `HOVER` | on: track `ACCENT`, thumb `TEXT` | alpha 0.45 | Thumb animates with `animate_bool_with_time_and_easing` |
| **Text input / DragValue** | `BG_SUNKEN`, 1 px `BORDER`, `R_SM`, h 28 | 1 px `BORDER_STRONG` | focused: 1 px `ACCENT` + 2 px `FOCUS` ring at 35% alpha | `TEXT_DISABLED` | Errors: 1 px `ERROR` + helper text below in `ERROR` |
| **Slider** | rail 4 px `BG_SUNKEN` + 1 px `BORDER_INPUT`, trailing fill `TEXT_MUTED`, handle 12 px `TEXT` | handle 14 px | handle `ACCENT`, trailing fill `ACCENT` | — | Value label right-aligned in `value` style |
| **Status pill** | h 20, `R_PILL`, padding 8, soft fill + solid icon/text (`Small` 500) | — | — | — | Neutral pill: `SURFACE_3` + `TEXT_MUTED` |
| **Tooltip** | level 3, max width 320, `Small` text, title line `label` style | — | — | — | Delay 350 ms. Shows shortcut keycaps on the right |
| **Focus ring** (all) | 2 px `FOCUS`, 2 px outside the widget, matching radius | | | | Only after keyboard navigation (egui's `has_focus` plus last input was a key) |

### 4.1 Gift health model (drives tiles, list, 3D and log)

Derived on the host from `Status` replies (several per second) and the link scheduler's counters. Every metric has
an **enter** debounce of 3 s and an **exit** debounce of 10 s (hysteresis), so values hovering on a threshold
don't flicker. Offline detection follows the architecture (polls failing for 1 s).

| Metric | Source | Normal (neutral) | Warn | Critical |
|---|---|---|---|---|
| Link quality % | successful polls ÷ attempted, 10 s window | ≥ 90 | 70–89 | < 70 |
| Poll retries | retries per minute | ≤ 5 | 6–30 | > 30 |
| Broadcast RX % | Δ`bcast_rx` ÷ (Δ`bcast_rx` + Δ`bcast_lost`), 10 s window, u16 wrap-safe | ≥ 95 | 85–94 | < 85 |
| Battery | `vbat_mv`, 30 s median (LED load sags the cell) | ≥ 1.15 V | < 1.15 V or `LOW_BATTERY` flag | < 1.05 V |
| Clock sync | `sync_err_us`, and the `TIME_SYNCED` flag | < 500 µs | 500–2000 µs or not synced > 5 s | — (never critical) |
| Reboots | Δ`resets` and `reset_reason` | none in 10 min | 1–2 in 10 min, or reason Watchdog/Brownout | ≥ 3 in 10 min |
| Bootloader | `IN_BOOTLOADER` outside an OTA session | — | — | "In recovery: update firmware" |
| Firmware | `fw_version` vs the newest known (fleet or loaded image) | equal | older ("Update available"; info, not warn) | — |
| Temperature | `temp_c` | shown in inspector only | > 50 °C | > 65 °C |

**Battery level** (single AAA, alkaline curve by default; a NiMH curve is selectable in Settings › Power):
alkaline `1.45 V=100%, 1.35=75, 1.25=50, 1.15=25, 1.05=10, ≤0.95=0` (linear between points);
NiMH `1.30=100, 1.25=75, 1.20=50, 1.15=25, 1.10=10, ≤1.00=0`. Always show volts. Show the % only with a `~` prefix,
because it is an estimate.

**Overall state** (one per gift, shown by the status pill and the tile border):

| State | When | Pill | Tile treatment |
|---|---|---|---|
| **Online** | healthy | none (just "Online" in `TEXT_MUTED`, no dot colour) | default |
| **Attention** | any Warn | `WARN_SOFT` + ▲ `warning` icon + reason | 1 px `WARN` border at 50% alpha, reason line at the bottom |
| **Critical** | any Critical | `ERROR_SOFT` + `warning-circle` + reason | 1 px `ERROR` border, reason line, sorted first under "Health" |
| **Reconnecting** | polls failing 1–5 s | neutral pill, `circle-notch` spinner | values freeze at 60% opacity |
| **Offline** | > 5 s without a reply | neutral pill "Offline 2m" (`TEXT_FAINT`, ○) | `SURFACE_1` fill, 1 px **dashed** `BORDER`, swatch hatched, values `TEXT_DISABLED` "last seen …" |
| **Updating** | in an OTA session | `INFO_SOFT` + `download-simple` + phase | progress line along the tile bottom (2 px `INFO`) |
| **Simulated** | simulator gift | additional `SIM` chip (`SURFACE_3`, `TEXT_FAINT`, `Small`) | normal |

Offline is **not red**. A gift that is switched off is normal at a party, so it stays neutral. Red is only for
things the user must act on. The page header summarises: "10 gifts · 9 online · 1 needs attention", where the
last part is a link (`WARN` text) that filters the grid to Attention + Critical.

### 4.2 Fleet components

**Gift tile (comfortable, 270 × 176, `card()` frame, `R_MD`, padding 12)**

```text
┌────────────────────────────────────────┐
│ ┌──────┐  Kitchen               #03    │  name: `title` 15/600 TEXT, truncates with "…"; slot chip right
│ │ gem  │  ▲ Low battery                 │  status line: pill or "Online" (TEXT_MUTED, Small)
│ │ 44px │                                │
│ └──────┘                                │
│ ▂▄▆_  81%   ~~~~~~~~~~~~~~~~~~~~~~      │  link meter + value + 60 s sparkline (h 20, fills the rest)
│ ▭▯▯▯  1.12 V  ~20%            v·10-31   │  battery glyph + V + ~% · fw date (TEXT_FAINT, Small), "⇪" if update available
│ 41m · RX 88.0% · ±0.31 ms · 7 retries   │  meta row: value_small, TEXT_FAINT, separators " · "
└────────────────────────────────────────┘
```

* **Slot chip**: `#03` in `value_small`, `SURFACE_3` fill, `R_XS`, `TEXT_MUTED`. Unassigned (`UNASSIGNED_SLOT`) shows a
  dashed outline chip "No slot" in `WARN`. A gift without a slot doesn't take part in the show, and that is the one config error worth flagging.
* **States.** Hover: fill `HOVER`, the quick-action row fades in over the meta row (`MOTION_FAST`): `[◉ Blink] [# Slot] [⋯]`
  as 24 px ghost buttons. Selected: 1.5 px `ACCENT` border (inside) plus `ACCENT_SOFT` glow (a 4 px spread shadow in `ACCENT`
  at 25%). Multi-select uses `Ctrl/Shift+click`, and the inspector then shows "3 gifts selected" with batch actions.
  Focus: focus ring. Pressed: `ACTIVE`. Drag a tile onto another tile to swap slots (shows a drop indicator).
* **Context menu** (right-click or `⋯`): Blink · Rename… · Assign slot… · Show in Stage · Copy UID · separator ·
  Reboot · Reboot to bootloader… · separator · Forget gift (removes from registry until rediscovered).
* **Compact tile (270 × 112)**: gem 32 px, name `label` style, slot chip, pill, link meter + %, battery glyph + V. Nothing else.

**LED swatch ("gem")**: the signature element. 44 × 44, `R_MD`, `BG_SUNKEN` fill, 1 px `BORDER_SUBTLE`.
* It contains a 2 × 2 grid of LED dots, 14 px circles with a 4 px gap, in the gift's **current LED colours**. The colours come from
  the host running the gift's `lumigift_fx::Renderer` with the gift's slot on the same `MusicFrame` stream it sends
  (the gift doesn't report its colours. The renderer is deterministic, so the mirror is exact when `LEDS_ACTIVE` is set and
  `bcast_lost` is low). For `Blink` and `Solid`/`Leds` broadcasts, use the commanded colours.
* Show the **pre-gamma** colour (the value before `lumigift_fx::gamma`), because that is perceptually what the LED looks like.
* **Glow**: behind each dot, paint a radial gradient mesh (centre = colour at alpha `0.55 × luma`, edge transparent, radius 14 px)
  so bright LEDs bloom softly into the well. Then paint the dot with a 1 px inner ring of `#FFFFFF` at 8% so dark LEDs remain
  visible as "glass beads".
* LEDs off: dots are `SURFACE_3` with no glow. Offline: dots are hatched (45° lines, `BORDER`, 3 px pitch, clipped to circles).
* The swatch is repainted every frame only while the gift's colours are changing (playback or blink). Otherwise at 4 Hz.

**Link-quality meter** (16 × 14): 4 bars, each 3 px wide with a 1 px (S0/2) gap, heights 5/8/11/14 px, bottom-aligned, `R_XS`=1.
Filled bars: ≥ 90 → 4, ≥ 75 → 3, ≥ 50 → 2, > 0 → 1, offline → 0 (show an `—` value). Filled colour is `TEXT_SECONDARY`, or the
semantic colour when in the Warn/Critical band. Unfilled bars are `BORDER_STRONG`. Paint with `rect_filled`.

**Sparkline** (60 s in tiles, 5 min in the inspector, 1 sample/s, y fixed 0–100% so gifts are comparable):
* Line: 1.5 px, `VIZ_LEVEL` at 70% alpha (healthy), or `WARN`/`ERROR` at 100% when the current value is in that band.
* Area: a mesh under the line, vertical gradient from line colour at 16% alpha to 0% at the baseline (build a triangle strip with
  `Mesh::colored_vertex`, two vertices per sample).
* Threshold: a dashed 1 px `BORDER_STRONG` line at the warn threshold (90%) in the inspector variant only.
* Last point: a 3 px dot in the line colour. Gaps (offline) break the line.
* Smooth scroll: offset x by the fractional time since the last sample so the line glides instead of stepping.
* Hover (inspector only): a vertical crosshair plus a tooltip `12:01:31 · 83%`.

**Battery glyph** (22 × 11): body is a 20 × 11 rounded rect (`R_XS`=2) with a 1 px `TEXT_MUTED` outline, plus a nub (2 × 5) on the right.
The fill is inset 2 px, width = level. Fill colour: `TEXT_SECONDARY` at ≥ 25%, `WARN` below 25%, `ERROR` below 10%.
Critical adds the `battery-warning` Phosphor glyph instead of the painted glyph. No pulsing.

**Inspector (gift)**: header with a 64 px gem, name (`title`, click to rename inline), slot chip, state pill,
plus a one-sentence plain-language state ("Low battery: replace the AAA cell soon"). Then a row of action buttons (Blink,
Slot, Rename). Then sections with `overline` headers: **Link** (quality sparkline 5 min, retries/min, RX %, sync, temperature),
**Power** (V, ~%, 30 min chart), **Health** (uptime formatted `41m`, `3h 12m`, `2d 4h`; resets since power-on; last reset reason
in words: "Brownout (battery dipped)", "Watchdog (firmware hang)", "Power on", "Software reboot", "Reset pin", "Low-power"),
**Firmware** (`fw_version` formatted as `YYYY-MM-DD HH:MM` UTC if ≥ 1 600 000 000, else `v{n}`; boot version; hw rev; UID in
Geist Mono grouped `3A9F 11C2 …` with a copy button), and a **Danger zone** at the bottom (Reboot, Reboot to bootloader).
Values are selectable text in the inspector (opt-in `selectable(true)`).

**Rename**: inline `TextEdit` in place of the name. `Enter` saves, `Esc` cancels, max 24 chars. Names are stored host-side,
keyed by UID (`Identity.uid`), in the app config. The default is "Gift " + the last 4 hex digits of the UID.

**Assign slot**: a popover with a grid of slots 1…`MAX_SLOTS` (shown 1-based, sent 0-based), 8 per row, 32 × 28 cells.
Free slots: `SURFACE_3`. Occupied: shows a mini gem of the occupant plus its name in a tooltip. Current: `ACCENT_SOFT` + `ACCENT` outline.
Picking an occupied slot shows an inline "Swap with Desk?" `[Swap] [Cancel]`. Also offers "Number all left→right (from Stage layout)".

**Blink / identify**: sends `Unicast::Blink` (default: white `#FFF0DC`, 6 blinks, 250 ms period, configurable). While
`BLINKING` is set, the tile gem, the inspector gem and the 3D box pulse in the same colour and period, and a ring ripples once
from the gem (300 ms, `ACCENT` 1.5 px, radius 22→40, alpha 1→0).

**Event log** (drawer): rows 24 px, `Monospace` time `12:01:33` (`TEXT_FAINT`) · 16 px severity icon (info `info` `TEXT_MUTED`,
warn `warning` `WARN`, error `warning-circle` `ERROR`, reboot `arrow-clockwise`, OTA `download-simple`) · gift chip (mini gem 10 px +
name, clicking selects the gift) · message (`Body`, `TEXT_SECONDARY`). Consecutive identical events within 60 s **coalesce**:
"Link dropped ×3 · last 12:01:33". It auto-scrolls while pinned to the bottom. When scrolled up, a floating `[↓ Latest · 4 new]`
pill appears. Filters are severity chips plus a gift chip. Ring buffer of 5 000 events, exportable as `.log` from the `⋯` menu.
Events logged: discovered, address conflict resolved, online, offline, state changes (enter/exit Attention/Critical), reboot +
reason, slot/name/config changes, OTA phases, bridge connect/disconnect, LIVE on/off.

**Discovery / rescan**: `[⟳ Rescan]` runs the 1…254 sweep. A 2 px `INFO` progress line runs under the page header with the
label "Scanning addresses 112 / 254" in the header. The button becomes `[Stop]`. New gifts fade and scale in (0.96→1, 200 ms) with a
`New` chip (`INFO_SOFT`) for 10 s and a log entry. Address conflicts resolved via `SetNodeAddr` produce an info event
"Two gifts shared address 0x23; moved one to 0x41".

**Bridge status** (status bar, left segment, and popover on click): a dot plus text "Bridge · ch 76 · 412 pkt/s · 0.2% fail" in `Small`.
Dot colours: connected = `TEXT_MUTED` (neutral; healthy is quiet), degraded (fail > 5%) = `WARN`, disconnected = `ERROR` with
text "Bridge disconnected: reconnecting…". The popover (level 3, 300 px) shows device/USB path, bridge firmware, channel,
packets/s and failures/min sparklines, TX queue depth and `[Reconnect]`.

### 4.3 Firmware components

**Image card** (`card()`, full width, 88 px): a drop zone when empty ("Drop a firmware image here or `[Choose file…]`",
dashed 1 px `BORDER_STRONG`, `R_MD`, which turns `ACCENT` dashed while a file is dragged over). Loaded: a 32 px `download-simple` icon
in `ACCENT_TEXT`, file name (`title`), then a line `2026-10-31 14:02 · a1b2c3d4 · 21.4 KiB` (version formatted as in §4.2,
the first 8 hex of `git_hash` in Geist Mono, `app_len + 256` as size) and a validation line: `✓ Header valid · hw rev A · CRC 0x…`
in `TEXT_MUTED` with the check in `OK`. Invalid images (bad magic, header CRC, wrong `hw_rev`, too large for the 20 KiB region)
render an `ERROR_SOFT` banner inside the card with the exact reason, and the CTA is disabled.

**Target list**: rows 40 px. Checkbox · gem 20 px · name · slot · `current → new` version (an arrow in `TEXT_FAINT`; "same" in
`TEXT_FAINT`; older-than-current shows `▲ Downgrade` in `WARN`) · battery glyph + V · link meter. Default selection = all online
gifts whose firmware differs. **Auto-excluded** (unchecked, with the reason inline, can be re-checked with a warning tooltip):
battery < 1.15 V, link < 70%, offline (not selectable). Header row: `Select: All · Outdated · None` text buttons and
"8 of 10 selected".

**CTA**: primary `Update 8 gifts`, with the estimate "~25 s" to its left in `TEXT_MUTED`. Opens the confirmation dialog (§6.3).

**Run view.** Overall card (top):

```text
┌───────────────────────────────────────────────────────────────────────────────────────────────┐
│ Updating 8 gifts                                                        62%      ~9 s left    │
│ ████████████████████████████████████████████████████▌░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░  │
│ Reboot ✓ ── Erase ✓ ── Transfer ● ── Repair ○ ── Verify ○ ── Boot ○      Don't switch gifts off │
└───────────────────────────────────────────────────────────────────────────────────────────────┘
```

* Bar: 8 px, `R_PILL`, track `BG_SUNKEN`, fill `INFO`. It becomes `OK` when everything succeeds, or stays `INFO` with error segments
  if some gifts fail. The value animates (`animate_value_with_time`, 0.25 s), and the % is `metric` style.
* The phase stepper uses the global phase (the slowest gift's phase). Done = `check` in `OK`, current = an 8 px `INFO` dot
  breathing, pending = an 8 px ring `BORDER_STRONG`.
* **ETA**: `remaining_chunks / measured_chunk_rate + Σ remaining phase constants` (reboot 1.0 s, erase 0.6 s, verify 0.4 s,
  boot 1.5 s, plus ~0.5 s per repair round). Show "~N s left" rounded to 5 s above 30 s. Smooth it with an exponential moving
  average (τ = 2 s), and never let it count up by more than 2 s between updates.

**Per-gift OTA row** (`card()`, 48 px):

```text
│ [gem] Tree  #01   ● ● ● ◐ ○ ○ ○   Transferring  612 / 914 chunks   ███████████░░░░░  67% │
```

| Phase | Source | Row label | Row progress |
|---|---|---|---|
| Rebooting | `Reboot{into_bootloader}` sent, waiting for `IN_BOOTLOADER` | "Rebooting into bootloader" | indeterminate stripe |
| Erasing | `OtaState::Erasing` | "Erasing" | indeterminate stripe |
| Transferring | `OtaState::Receiving`, `received` / total | "Transferring 612 / 914 chunks" | `received / total` |
| Repairing | `missing_count > 0` after the full pass | "Repairing 14 missed chunks (round 2)" | `1 − missing / missing_at_start` |
| Verifying | `OtaCommit` sent | "Verifying" | indeterminate |
| Booting | `Verified`, waiting for `Status` without `IN_BOOTLOADER` and `fw_version == new` | "Starting new firmware" | indeterminate |
| Done | — | "Updated to 2026-10-31 14:02" | full, `OK` |
| Failed | `OtaState::Failed`, timeout, or boot didn't report the new version | plain reason (below) | stops, `ERROR` |

The 7-dot stepper (6 px dots, 6 px apart) uses the same done/current/pending styling as the overall stepper.

**Failure states (impossible to misread):** the row background becomes `ERROR_SOFT` with a 1 px `ERROR` border, a 16 px
`x-circle` in `ERROR`, the label in `TEXT` with **what happened, what it means and what to do**, e.g.:
* "Verification failed: the image arrived corrupted. The gift is safe in recovery mode. `[Retry]`"
* "No response for 10 s while transferring. Check the gift is on and in range. `[Retry]`"
* "Booted the old firmware (new image crashed 3×). `[View log]`"

Failed rows **stay** until dismissed. They never auto-fade. When the run ends the overall card shows a summary:
`✓ 7 updated · ✕ 1 failed` with `[Retry failed (1)]` primary and `[Done]` secondary. Success: the overall bar turns
`OK`, a check icon draws in (400 ms), and the toast "Firmware updated on 7 gifts" appears. Rows that succeed tint `OK_SOFT` for 1.2 s, then return to neutral.

**Cancel** during a run: a confirmation popover: "Stop the update? Gifts already erased stay in recovery mode until you run
the update again." `[Stop update]` (danger) `[Keep going]`.

### 4.4 Studio components

**Toolbar** (48 px, `SURFACE_1`): file chip (song name, `▾` opens recent shows + Open/Save/Save as/Export), tempo/meter/
length readout (`value_small`, `TEXT_MUTED`), `[✦ Auto-generate]` secondary button with the `magic-wand` icon, Snap dropdown
(Off, 1/16, 1/8, 1/4 beat, 1/2 beat, **1 beat** (default), 1 bar, Section), the latency slider, the LIVE toggle (far right).
Unsaved changes: a 6 px `TEXT_MUTED` dot after the file name, and the window title gets `•`.

**LIVE toggle**: a pill, 28 px tall, `R_PILL`.
* Preview (off): `SURFACE_3` fill, `TEXT_MUTED` text "Preview", a ring dot `○`. Tooltip: "Lights only in the 3D preview. Gifts not driven."
* LIVE (on): `ACCENT` fill, `ON_ACCENT` text "LIVE", a solid dot breathing (`LOOP_LIVE`). On engage, a single outer glow ring expands
  (4 → 14 px spread, alpha 0.5 → 0, 400 ms). The status bar segment mirrors it ("● LIVE · 10 gifts").
* Disabled when no bridge and the simulator is off (tooltip explains). In simulator mode it reads "LIVE (SIM)".

**Latency offset slider**: range −50…+250 ms, step 1 ms, default from settings (the measured radio + render latency). 120 px
slider + `value` "42 ms". Double-click resets to default. Tooltip: "Shifts the lights relative to the audio. Increase if the gifts
light up late." A `[Calibrate…]` link opens a tap-along calibration (tap `Space` on a click track. The median offset is applied).

**Transport bar** (44 px, `SURFACE_1`, top and bottom borders `BORDER_SUBTLE`):
`⏮` (skip-back), `▶/❚❚` (a 36 px circular button with `TEXT` fill and a `BG` icon. It is deliberately neutral, not accent, so the LIVE pill stays the only accent-filled control in the Studio),
`⏭`, `⟲` loop toggle (on: `ACCENT_TEXT` icon + `ACCENT_SOFT` fill), timecode `01:02.250` (`timecode` style, `TEXT`) and
musical position `33.1.1` (bar.beat.sixteenth, `value`, `TEXT_MUTED`). Click the timecode to type a time. In the middle: the
**overview minimap** (full song waveform 24 px high in `TEXT_FAINT`, sections as coloured underlines, the visible window as a
`BORDER_STRONG` 1 px rectangle with `#FFFFFF` 4% fill that can be dragged and its edges resized to zoom). On the right: zoom
`⊖ ━●━ ⊕` and `⤢` (zoom to fit).

**Timeline geometry and interaction**
* Time ↔ x: `x = lane_left + (t − scroll_t) × px_per_sec`. Zoom range is 4 px/s (whole song) to 2000 px/s. Zoom anchors on the
  pointer for wheel/pinch and on the playhead for keys. Zoom smoothly by animating `px_per_sec` towards the target (80 ms).
* Horizontal wheel scroll and drag on empty ruler space scrubs the playhead. Middle-drag anywhere pans.
* **Ruler** (32 px, `SURFACE_1`): bar numbers (`value_small`, `TEXT_MUTED`) at bar lines. Tick density adapts: show beats when ≥ 12 px
  apart, sixteenths when ≥ 10 px apart, and thin to every 2/4/8/16 bars when bars are < 40 px apart. Downbeat ticks `VIZ_BEAT`
  6 px, beat ticks `BORDER_STRONG` 4 px. The loop region is an `ACCENT_SOFT` band across the ruler with 2 px `ACCENT` brackets
  (drag edges; drag the band to move). Click sets the playhead. Shift+drag in the ruler creates a loop region.
* **Grid in lanes**: bar lines 1 px `BORDER` (alpha 100%), beat lines 1 px `GRID`, sub-beat lines `GRID` at 50%, drawn
  only at the current density. Section boundaries: 1 px `VIZ_SECTION` at 60% through all lanes.
* **Playhead**: 1 px `ACCENT` line through the ruler and all lanes, with an 8 × 8 rounded triangle head in the ruler. During playback with
  follow on, the view pages when the playhead reaches 85% of the width ("page" mode), or "continuous" mode keeps it at 33% (setting).
  The playhead position comes from `Player::position()` each frame. Never ease it.
* **Sections lane** (24 px): blocks spanning each section, `SURFACE_3` with a 2 px left edge in `VIZ_SECTION`, label `Small`
  (`TEXT_SECONDARY`): "Intro", "Verse 1", "Chorus"… Double-click renames, drag edges to adjust. The **drop indicator**: a `lightning`
  glyph in `ACCENT` at the drop time in the sections lane plus a 2 px `ACCENT` line at 40% alpha through the energy lanes.
* **Waveform lane** (96 px): a mirrored min/max envelope per pixel column in `VIZ_LEVEL` at 55%, RMS core at 85%. Precompute
  mip levels (256, 1024, 4096 samples per bucket) at load and pick by zoom. Draw as one `Mesh` of vertical quads (not thousands
  of line shapes). Part of the waveform outside the loop region dims to 35% when loop is on.
* **Energy lanes** (28 px each): filled area chart of the normalised band energy (0–255 → lane height), `VIZ_BASS/MID/TREBLE`
  fill at 35% alpha with a 1 px line at 90%. Beats render as thin ticks at the top of the Bass lane (`VIZ_BEAT` at 50%).
* **Lane header** (120 × lane h, `SURFACE_1`): icon + name (`label`, `TEXT_MUTED`), with a collapse chevron on hover. The Cues lane header has `+`
  (insert cue at playhead) and the One-shots header has `+` too.

**Cue block** (Cues lane, 64 px; a cue lasts until the next cue's start):

```text
▐▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▌  ← 4-stop palette gradient strip, 6 px, full brightness
│ [≋] Chase                                                 │  ← effect icon + effect name (label, TEXT)
│ Neon · 80%                                                │  ← palette name · brightness (Small, TEXT_SECONDARY)
└──────────────────────────────────────────────────────────▔┘  ← bottom 3 px brightness bar: width = brightness, TEXT at 50%
```

* The body is the palette gradient (a mesh with 4 stops from `lumigift_fx::PALETTES`) darkened to 28% brightness, multiplied over
  `SURFACE_2`, with a left-to-right scrim (`#000000` 40% → 0% across 120 px) so the text always reads (≥ 7:1 on the scrim).
  The colour comes from the content, so each cue is recognisable at a glance and the chrome stays neutral.
* `R_SM` corners, 1 px gap between adjacent cues. The start edge has a 2 px bright line in the palette's first stop colour.
* Effect icons: Solid `circle`, Pulse `heartbeat`, Spectrum `equalizer`, Chase `arrows-left-right`, Strobe `lightning`,
  Rainbow `rainbow`, Sparkle `sparkle`, Breathe `wind`; generic looks: Fade `circle-half`, Waterfall `waves`, Knight Rider
  `arrows-horizontal`, Blink `lightbulb-filament`, Twinkle `star-four`, Comet `shooting-star`, Theater Chase `dots-three-outline`,
  Fire `flame`, Police `siren`, Timed Strobe `flashlight`, Timed Pulse `pulse`, Colour Wipe `paint-roller` (Phosphor names; the
  Lighting rail item is `lightbulb`).
* **Narrow cues**: < 80 px hides the second line, < 36 px hides the label and shows only the icon, < 14 px shows only the gradient.
* **States.** Hover: brightness 36%, a 1 px `BORDER_STRONG` outline, resize cursor within 6 px of the start edge.
  Selected: 1.5 px `TEXT` outline plus 2 px `ACCENT` outer glow, and the start edge handle becomes a 4 × 20 px `ACCENT` grip.
  Dragging: the original stays as a 40% ghost, the dragged copy moves with the pointer, and an `ACCENT` 1 px **snap line** appears through all
  lanes with a tooltip `Bar 33 · beat 1 · 1:02.250`. Invalid (dropping onto another cue's exact start): the outline turns `ERROR` and the drop is
  refused. Disabled (playback in LIVE with "lock while live" on): no hover affordances.
* **Interactions**: click selects. `Ctrl+click` toggles, `Shift+click` selects a range, and a rubber band from empty lane space selects
  several. Drag the body to move (snaps to the grid. `Alt` bypasses snap). Drag the start edge to move just the boundary between two cues. Double-click
  opens the inspector focused on Effect. Double-click on empty lane space inserts a cue there (snapped). Right-click: Effect ▸,
  Palette ▸ (with gradient swatches), Brightness ▸, Duplicate, Delete, "Apply to section".
* **Snapping** (when on): candidates are grid lines at the current snap unit, section starts, the drop, the playhead and loop edges. The snap radius is 8 px.
  Show only the winning candidate's line.

**One-shot marker** (One-shots lane, 28 px): a 12 px diamond (rotated square, `R_XS`=1) at the time. Fill by type: Flash `TEXT`,
Blackout `BG` with a `TEXT` 1.5 px outline, Strobe hit `ACCENT_TEXT`, Hue jump `VIZ_SECTION`. Label to the right when there is room (`Small`).
States as for cues (hover outline, selected glow, drag + snap).

**Cue inspector**: Effect (a dropdown with icons, or a 4×2 icon grid toggle), Palette (dropdown where each item shows a 64 × 10 gradient
swatch plus name), Brightness slider (0–100%, shows the power-limit cap as a `WARN` tick at the effective max), Hue (0–255, with a rainbow
track), Param 1/2 (labelled per effect, e.g. Chase: "Speed", "Tail"), Start time (DragValue, snaps), and "Preview this cue" (loops the
cue's span). Multi-select edits apply to all and show "—" for mixed values.

**Auto-generate**: the `magic-wand` button opens a popover: Style `[Calm | Balanced | Energetic]`, Palettes (multi-select chips),
"Keep my edited cues" checkbox, `[Generate]`. Generation replaces only unlocked cues, is a single undo step, and newly
generated cues flash a 1 px `ACCENT_TEXT` outline that fades over 600 ms, so the user sees what changed.

**Audio loading / analysis**: a drop zone in the timeline area: "Drop an audio file (MP3, FLAC, WAV, OGG) or `[Open…]`", plus
recent shows as rows. While analysing: a stepped progress card *Decoding → Bands → Beats & tempo → Sections & drops* with a
determinate bar for decoding and per-step checks. The waveform fades in when decoding finishes (users can scrub before
beat analysis completes. Snap is disabled with a tooltip until the beat grid exists).

---

## 5. 3D stage

### 5.1 Goal and look

The real boxes in `images/lumigift_green_dark.jpg` look like frosted white plastic lit from inside. The lower body glows
brightly with two soft hot-spots from the LEDs, there is a darker band right under the lid, the lid itself is dimmer and more
opaque, and the box throws a large coloured **light pool** onto the floor. The stage reproduces exactly that in a dark room:
near-black background, glowing boxes, soft pools, subtle reflections and bloom. Everything is drawn opaque. Translucency is
faked with emission, so there is no transparency sorting.

### 5.2 Geometry (units: body width = 1.0; the physical box is roughly a 50 mm cube)

From `images/lumigift.jpg`: a near-cube body, a slightly overhanging flat lid, and a small hanging eyelet loop on top. (The
present.svg/ribbon motif is a brand illustration. An optional "Ribbon" toggle adds the bands, default off to match the real boxes.)

| Part | Shape | Dimensions | Position (y up, origin at floor centre of box) |
|---|---|---|---|
| Body | rounded box | 1.00 × 0.86 × 1.00, edge radius 0.05 | y 0.00 → 0.86 |
| Lid | rounded box | 1.06 × 0.13 × 1.06, edge radius 0.035 | y 0.84 → 0.97 (overlaps body by 0.02) |
| Eyelet | torus in the XY plane | major R 0.075, minor r 0.026 | centre (0, 1.065, 0) |
| Eyelet base | cylinder | r 0.05, h 0.03 | y 0.97 → 1.00 |
| Lid slot (detail) | flat dark quad | 0.04 × 0.015 | on lid top at (0, 0.971, 0.14), colour `#000` |
| Ribbon (optional) | two thin boxes crossing | 0.12 wide, 0.005 proud | wraps body + lid |
| LEDs (virtual) | 4 point emitters | — | (±0.22, 0.70, ±0.22) in LED order 0:(−,−) 1:(+,−) 2:(+,+) 3:(−,+) |

Generate the rounded boxes in Rust at startup: subdivide a cube to 8×8 per face, then for each vertex take
`p_inner = clamp(p, -(half - r), half - r)`, `n = normalize(p - p_inner)`, `p = p_inner + n * r`. That is ~1.5 k triangles per box.
Store `local_pos`, `normal` and `part_id` (0 body, 1 lid, 2 eyelet, 3 slot) per vertex in one mesh. Draw all gifts **instanced**
(one draw call for boxes, one for reflections).

Instance data (per gift, `bytemuck` Pod, 96 B): `model: mat4x3` (position + yaw), `led: [vec4; 4]` (linear HDR rgb +
unused), `flags: u32` (online, selected, hovered, blinking), `intensity: f32`.

### 5.3 Materials (WGSL, ~120 lines for the box shader)

LED input: the pre-gamma LED colour (0–255) → `srgb_to_linear(c/255) * LED_INTENSITY` with `LED_INTENSITY = 6.0`, which
puts saturated LEDs into the HDR range so they bloom.

```wgsl
const LED_POS = array<vec3f,4>(vec3f(-0.22,0.70,-0.22), vec3f(0.22,0.70,-0.22), vec3f(0.22,0.70,0.22), vec3f(-0.22,0.70,0.22));
const ALBEDO = vec3f(0.78, 0.78, 0.80);       // frosted white PLA
const KEY_DIR = normalize(vec3f(-0.45, 0.80, 0.40)); // towards the light (upper-left-front)
const KEY = vec3f(0.050, 0.052, 0.060);       // very dim cool key: a dark room
const RIM = vec3f(0.060, 0.060, 0.075);       // back rim so dark/offline boxes keep a silhouette
const AMBIENT = vec3f(0.010, 0.010, 0.013);

fn body_emission(p: vec3f, n: vec3f, v: vec3f, led: array<vec3f,4>) -> vec3f {
    var direct = vec3f(0.0);
    var mean = vec3f(0.0);
    for (var i = 0u; i < 4u; i++) {
        let d = p - LED_POS[i];
        direct += led[i] / (1.0 + 22.0 * dot(d, d));   // soft hot-spots on the nearest faces
        mean += led[i] * 0.25;
    }
    let scatter = mean * 0.45;                          // light diffused through the whole shell
    let h = p.y / 0.86;                                 // PCB shadows the top band (photo)
    let profile = mix(1.0, 0.30, smoothstep(0.66, 0.84, h)) * mix(0.85, 1.0, smoothstep(0.0, 0.25, h));
    let fres = pow(1.0 - saturate(dot(n, v)), 3.0);    // edges glow a bit more (thicker plastic path)
    return (direct * 0.9 + scatter) * profile * (1.0 + 0.5 * fres);
}
// Lid: emission = mean * 0.22 + fresnel rim * mean * 0.15; albedo slightly brighter (0.85). Eyelet: mean * 0.10.
// Final: col = ALBEDO * (KEY * max(dot(n,KEY_DIR),0) + AMBIENT) + RIM * pow(1 - saturate(dot(n, v)), 4) + emission
```

* **Offline**: emission 0, albedo × 0.55, and the label shows "Offline". **Selected**: an extra rim in `ACCENT` (linear) × 0.6.
  **Hovered**: rim × 1.5. **Blinking**: emission follows the blink colour.
* **Ribbon** (optional): `part_id` 4, albedo 0.9, emission = mean × 0.35 (denser plastic).

### 5.4 Floor, pools, reflections, fog

* **Floor**: a 40 × 40 quad at y = 0, albedo `FLOOR` (linear), lit by the same key/ambient.
* **Light pools** (in the floor shader, looping over ≤ 32 gifts from a uniform array of `vec4(pos.xz, 0, 0)` + mean colour):
  `pool += mean_i * 0.55 / (1.0 + 6.0 * d2) * smoothstep(3.2, 0.0, sqrt(d2))`, where `d2` is the squared distance from the box
  centre with the footprint subtracted (`max(abs(dxz) - 0.5, 0)`). Add a **contact shadow** close to the box:
  `* (1.0 - 0.5 * exp(-dist_to_footprint * 14.0))`.
* **Grid**: anti-aliased lines every 0.5 units (`fwidth`-based), colour `GRID` × 0.6, fading out with distance
  (`1 - smoothstep(6, 18, dist_to_camera)`). Toggle with the `G` key in Arrange mode, and shown at 2× strength while arranging.
* **Reflection**: draw the instanced boxes a second time mirrored in y (`scale(1,-1,1)`, flipped winding), with emission × 0.35 and
  a fade of `exp(-3.5 * depth_below_floor)`, **before** the floor. Then draw the floor alpha-blended at alpha 0.88, so reflections
  show through as a soft, slightly diffuse gloss. (Optional: blur the reflection by drawing it into the bloom mip 1. Skip in v1.)
* **Fog**: exponential distance fog towards `STAGE_BG` (linear) in all scene shaders: `mix(col, FOG, 1 - exp(-0.035 * dist))`.
  It is subtle and gives depth with 30 boxes.

### 5.5 Post-processing (render graph)

All offscreen, at the viewport size × `pixels_per_point`. Recreate on resize.

1. **Scene pass**: `Rgba16Float` colour + `Depth32Float`, **4× MSAA** resolved to a single-sample HDR texture. Clear to
   `STAGE_BG` (linear). Order: reflections → floor (blend) → boxes.
2. **Bloom** (the Call of Duty: Advanced Warfare / learnopengl "physically based bloom" method): 6 downsample mips
   (`Rg11b10Ufloat` or `Rgba16Float`), 13-tap downsample with a **Karis average on the first downsample** (prevents fireflies from
   tiny bright texels), then a 3×3 tent upsample with radius 0.005 (uv) and additive blend back up the chain. No threshold.
3. **Composite** → `Rgba8Unorm` (or the egui target format) texture:
   * `hdr = mix(scene, bloom, 0.08)` (`BLOOM_STRENGTH` 0.08 in the studio, 0.10 in performance mode).
   * Tone map (hue-preserving, with white-hot cores like the photo):
     ```wgsl
     fn tonemap(c: vec3f) -> vec3f {
         let m = max(max(c.r, c.g), c.b);
         let mapped = c / (1.0 + m);
         let w = smoothstep(0.8, 4.0, m) * 0.55;
         return mix(mapped, vec3f(m / (1.0 + m)), w);
     }
     ```
   * Vignette: `col *= mix(1.0, 0.72, smoothstep(0.45, 1.05, length(uv - 0.5) * 1.35))`.
   * Encode to sRGB if the target isn't `*Srgb`, then add **±0.5/255 blue-noise or hash dither**. This is required: dark gradients
     band badly in 8 bits without it.

**egui integration**: implement `egui_wgpu::CallbackTrait`. In `prepare(device, queue, screen_descriptor, egui_encoder,
resources)`, update the instance and uniform buffers and record passes 1–3 into the provided encoder (pipelines, textures and bind
groups live in `CallbackResources`, created once from `frame.wgpu_render_state()`, using `target_format` for the final pipeline).
In `paint(info, render_pass, resources)`, set the viewport from `info.viewport_in_pixels()` and draw a full-screen triangle
sampling the composite texture. Add it with
`ui.painter().add(egui_wgpu::Callback::new_paint_callback(rect, StageCallback { … }))`. Reference: egui's `custom3d_wgpu` demo.
Labels, hover outlines in screen space and HUDs are drawn **with the egui painter on top**, by projecting 3D anchor points
(the box top + 0.25) with the same view-projection matrix. This keeps text crisp and uses the app fonts.

**Performance budget**: ≤ 2 ms GPU at 1176 × 380 and ≤ 6 ms at 4K on an integrated GPU. In performance mode above
2560 px wide, start bloom at half resolution. Request repaints only while playing, while live LED data is changing, during camera
motion, or while blinking. Otherwise repaint at 4 Hz.

### 5.6 Camera, lighting, layout

* **Camera**: perspective, `fov_y = 32°`, near 0.05, far 100. Orbit target = layout bounding-box centre at y 0.45. Default yaw 25°,
  pitch 22°, distance = `1.15 × bounding_radius / sin(fov_y / 2)`. Pitch is clamped 6°…85°, distance 1.5…40.
  All camera motion is **damped** (exponential smoothing, 80 ms half-life). Presets animate over 600 ms with `cubic_in_out`.
* **Controls**: LMB drag orbits (0.3°/px). RMB/MMB drag or Shift+LMB pans in the view plane. Wheel/pinch dollies
  (`dist *= exp(-delta * 0.0015)`) towards the pointer. Double-click a box to focus it (target → box, distance → 4). `F` frames all,
  `1` front (yaw 0, pitch 12), `2` top (orthographic, pitch 90, used by Arrange), `3` three-quarter (default).
* **Hover/pick**: a CPU ray–AABB test against the boxes (≤ 30, trivial). Hover shows the label and the hovered rim. Click selects (syncs with
  the Fleet selection and the inspector).
* **Lighting**: a very dim cool key from the upper-left-front, a back rim, and near-zero ambient (§5.3). The boxes and their pools
  light the scene. There are no shadow maps (contact shadows are faked in the floor).
* **Labels** (egui overlay): slot chip `#03` + name in `Small`, on a `SURFACE_3` 85% pill, `R_PILL`, offset above the box. Shown
  on hover/selection always. Shown for all boxes when `H` is on (default on in Stage, off in performance mode). Offline label: `TEXT_FAINT`
  "Offline".
* **Arrange mode** (`A` or the toolbar toggle): the camera animates to Top (orthographic). The grid goes to 2× strength. Boxes are draggable
  on the floor plane (ray–plane intersection), snapping to 0.25 (Alt bypasses). Multi-select drag works, `R`/`Q`/`E` rotate 15°.
  Presets: Row, Grid, Arc, Circle (they animate the boxes to their new positions over 260 ms). "Number slots left→right"
  assigns `SetSlot` by x position (confirm popover, since it changes the show's spatial order). Layouts are saved as named stages
  in the app config (`stages/<name>.json`, `{uid: {x, z, yaw}}`) and selected in the page header. A show stores the name of the
  stage it was made for but plays on any stage. Unknown gifts are auto-placed on the right in a row.
* **Data source**: with a show playing (preview or LIVE), boxes show the `lumigift_fx` render for their slot at the
  current show time. With nothing playing, boxes show the **live mirror** of each real gift (§4.2 gem), greyed if offline.
  Simulator gifts look the same, with "SIM" in their label.

### 5.7 Performance mode

* `F11`, `⛶ Perform` in Stage/Studio, or the command palette. Uses `ViewportCommand::Fullscreen(true)`. Optional: "Show on display ▾"
  opens the stage in a second native viewport (`ctx.show_viewport_immediate`) on another monitor, keeping the editor on the laptop.
* Everything is hidden except the stage. The cursor auto-hides after 2.5 s without movement.
* **HUD** (bottom-left, 48 px margins, fades in on mouse move, fades out after 2.5 s at `MOTION_SLOW`): song title (`title`), time
  `1:02 / 3:42` (`value`), the next section with a countdown "Chorus in 0:08", the LIVE pill, "10/10 gifts". Top-right: `Esc` keycap
  "Exit". Every text element sits on `#000000` 45% `R_LG` plates so it stays readable over bright bloom.
* Camera: auto-orbit 2°/s (toggle `O`, off with Reduce motion). **Beat push** (off by default): 1.5% dolly-in on downbeats,
  decaying over 400 ms.
* `Space` play/pause, `Ctrl+Shift+B` blackout and `Esc` exit all work. Nothing else is interactive, to avoid accidents mid-show.
* **Photosensitivity**: the preview applies a flash limiter by default (Settings › Appearance › "Limit flashing in preview"): the
  on-screen luminance can't change by more than 3 large flashes per second (WCAG 2.3.1), done by low-passing the per-box emission
  when Strobe is active. It only affects the screen, never the real gifts.

---

## 6. Microinteractions, motion, empty/error states, accessibility

### 6.1 Microinteractions

| Interaction | Behaviour |
|---|---|
| Hover on any control/tile | Fill and outline cross-fade over `MOTION_FAST`. No scale, no movement |
| Toggle / segmented | Thumb slides `MOTION_BASE` `cubic_out` |
| Tile state change | Border and pill colour cross-fade over `MOTION_SLOW`. **Numbers never tween** (tweened telemetry would show values that never happened) |
| Sparklines | Glide continuously (sub-sample x offset). New gifts' sparklines start empty, never with fake data |
| Gift discovered | Tile fades in plus scale 0.96→1 (200 ms). A `New` chip shows for 10 s |
| Gift goes offline | Tile desaturates over `MOTION_SLOW`. A 1 s "Reconnecting…" spinner appears first |
| Blink | Gem/3D box pulse in sync with the real gift, plus one ring ripple from the gem |
| Inspector open/close | Slides over `MOTION_SLOW` (egui `Panel::show_collapsible` animation) |
| Page switch | 120 ms cross-fade of the central panel (`ui.set_opacity` while animating) |
| Command palette | Scrim fades in. The panel fades and slides 8 px down (`MOTION_BASE`) |
| Toast | Bottom-right, 12 px from the edges, slides up 8 px + fades (`MOTION_BASE`). Info/success auto-dismiss after 4 s (paused while hovered). Errors persist with `[Dismiss]`. At most 3 stacked |
| Cue drag | 40% ghost, snap line with time tooltip. No sounds anywhere (the app must stay silent over the music) |
| Cue insert | The new block expands from 0 to full width over `MOTION_BASE` |
| Auto-generate | New cues get a 600 ms `ACCENT_TEXT` outline fade |
| LIVE engage | One glow ring (400 ms), then a breathing dot. Disengage: instant, no animation (calm exit) |
| OTA progress | Bars ease over 250 ms. Success check draws in over 400 ms. Success rows tint `OK_SOFT` for 1.2 s |
| Number edits (DragValue) | Drag horizontally, `Shift` = fine (×0.1). Double-click to type, `Esc` reverts |

### 6.2 Repaint policy

egui only repaints on input by default. Explicit policy:
* Playing / LIVE / blinking / camera moving / any animation in flight → `ctx.request_repaint()` (vsync, 60–144 Hz).
* Fleet idle → `request_repaint_after(250 ms)` (telemetry arrives several times per second, and 4 Hz is enough for a calm UI).
* Background threads (link scheduler, OTA engine, analysis) call `ctx.request_repaint()` on meaningful state changes.

### 6.3 Destructive-action safety (tiered)

Following NN/g, confirmations are reserved for consequential actions. Routine actions are immediate and either reversible or
harmless.

| Tier | Actions | Pattern |
|---|---|---|
| **0: immediate** | Blink, select, rename, assign slot (reversible), move/insert/delete cues | Do it instantly. Show cues are undoable (`Ctrl+Z`), and rename/slot show an "Undo" button on the toast for 6 s |
| **1: immediate + toast** | Reboot one gift (back in ~1 s) | Immediate when not LIVE. Toast: "Rebooting Kitchen…" → "Kitchen is back (1.2 s)" |
| **2: inline confirm** | Reboot to bootloader, reboot while LIVE, Forget gift, Number slots L→R, Stop OTA | A popover anchored to the button with one sentence of consequence and a danger button. No modal. `Esc` cancels |
| **3: modal summary** | Start OTA, Reboot all, Auto-generate over existing edited cues, close with unsaved changes | Modal (level 4): title as a question ("Update firmware on 8 gifts?"), a summary list of what will happen, the safety note, then buttons `[Cancel]` (left, secondary) and the specific verb (right, primary or danger), e.g. `[Update 8 gifts]`. Never "OK". Default focus is on Cancel for danger actions and on the verb for OTA |
| **LIVE on** | Switching from Preview to LIVE | No modal. The toggle's visual change is the confirmation. If gifts are mid-OTA, it is disabled with a reason |

**OTA confirmation contents**: the target count and names (collapsible if > 5), from → to version, the estimated time, and the excluded
gifts with reasons. The safety note: "Keep the gifts switched on and within range. If an update is interrupted, the gift stays in
recovery mode and you can simply run the update again. It can't be bricked." Show "Not a newer version" as a `WARN`
line for downgrades or same-version re-flashes.

**Blackout** (`Ctrl+Shift+B`) is a safety action, never a destructive one: it is always immediate, from anywhere.

### 6.4 Empty, loading and error states

All empty states share one layout: centred in the content area, max width 440, a 96 px illustration (the brand `present.svg`
glyph as an outline, filled with the `ACCENT → CANDLE` gradient at 85% and a soft glow), a `display` or `title` headline, one
`Body` sentence in `TEXT_MUTED`, **one primary action**, an optional secondary text button, and an optional "Troubleshooting"
disclosure.

| Situation | Headline | Body | Primary | Secondary / extra |
|---|---|---|---|---|
| No bridge (first run) | "Connect your Lumigift bridge" | "Plug the USB radio bridge into this computer. No hardware? Explore with virtual gifts." | `Start simulator (10 gifts)` | `Retry` · Troubleshooting: USB permissions |
| Bridge found, access denied (Linux) | "The bridge needs USB permission" | "Linux blocked access to the device. Add a udev rule, then replug the bridge." | `Copy udev rule` | Shows the rule in a Geist Mono code block with the VID:PID filled in |
| Bridge, no gifts yet | "Looking for gifts…" | "Switch your gifts on and keep them within a few metres." | (none: the scan is automatic) | A radar animation (3 expanding `ACCENT` rings at 30% around a bridge icon, 2 s period, static with Reduce motion), the scan counter, `Use simulator instead` |
| Bridge lost (was connected) | Banner, not empty state: `ERROR_SOFT` strip under the page header: "Bridge disconnected: reconnecting… Gifts keep their last state and turn off after the idle timeout." | | `Retry now` | Tiles go to Reconnecting/Offline. In LIVE, the Studio toolbar shows the same banner |
| Studio, no audio | "Make a light show" | "Drop an audio file here, or open a saved show." | `Open audio…` | Recent shows list (name, length, modified) |
| Analysing | (progress card, §4.4) | | `Cancel` | |
| Analysis failed | "Couldn't read this file" | Decoder error in plain words ("Unsupported codec: ALAC") | `Choose another file` | `Details` reveals the raw error in mono |
| Stage, no gifts | "Your stage is empty" | "Gifts appear here as soon as they are discovered." | `Start simulator` | |
| Firmware, no image | Drop zone card (§4.3) | | `Choose file…` | |
| Filter with no results | Inline in the grid: "No gifts match 'kit'" | | `Clear filter` | |
| Event log empty | Inline, `TEXT_FAINT`: "No events yet. Discoveries, reboots and warnings will appear here." | | | |

**Simulator mode**: a persistent `SIM` chip in the status bar (`SURFACE_3`, `TEXT_MUTED`) and a dismissible info banner on first
start: "Simulator: these gifts are virtual. Plug in a bridge to switch to real hardware." When a real bridge appears while the
simulator is running, a toast offers `[Switch to hardware]`.

**Errors in general**: say what happened, why if known, and what to do next, in that order and in plain language. Raw error details go
behind a `Details` disclosure in Geist Mono. Errors are never shown only as colour or only as a toast.

### 6.5 Accessibility

* **Contrast**: all text pairs in §2.1 meet WCAG AA (most AAA). Rules for `TEXT_FAINT` and borders are listed there. A
  **High contrast** option (Settings › Appearance) swaps `TEXT_MUTED → TEXT_SECONDARY`, `TEXT_FAINT → TEXT_MUTED` and `BORDER → BORDER_STRONG`,
  and draws 1 px `BORDER_INPUT` outlines on all cards.
* **Never colour alone**: every state has an icon and words (▲ Attention, ○ Offline, ✕ Failed). Battery uses fill length plus
  V. The link meter uses bar count plus %. `OK`/`WARN`/`ERROR` also differ in lightness
  (78/83/70% L) to help red–green colour-blind users, but icons remain mandatory.
* **Keyboard**: everything is reachable with Tab/Shift+Tab in a logical order (rail → header → content → inspector → status bar).
  Arrow keys move within grids, lists, segmented controls and menus. The visible focus ring appears only after keyboard use.
* **Screen readers**: egui 0.37 builds an AccessKit tree. Give every custom-painted widget a role and name
  (`response.widget_info(|| WidgetInfo::labeled(Role::Button, true, "Blink Kitchen"))`), name icon-only buttons, and use
  `Ui::label_inputs_by` for form fields. egui_kittest 0.37 fails tests for unnamed inputs, so run a kittest smoke test per screen.
* **Scaling**: UI zoom 80–150% (`Ctrl+=/-`), persisted. All layout is in points, so it scales cleanly. Minimum effective body size is 13 pt.
* **Motion**: the Reduce motion setting (§2.6). No essential information is conveyed only by animation.
* **Photosensitivity**: the flash limiter for on-screen preview (§5.7) is on by default. The first time Strobe is used in a show, a warning appears in the cue
  inspector.
* **Targets**: ≥ 24 × 24 px for every interactive element, 28 px for primary controls.

---

## 7. Recommended crates, fonts and icons

### 7.1 Crates (checked against crates.io on 2026-11-01)

| Crate | Version | Licence | Use | eframe 0.37 compatible? |
|---|---|---|---|---|
| `eframe` | 0.37.0 | MIT OR Apache-2.0 | App shell. Features: `wgpu`, `persistence`, `accesskit` (default) | ✓ |
| `egui` | 0.37.0 | MIT OR Apache-2.0 | UI | ✓ |
| `egui-wgpu` (via `eframe::egui_wgpu`) | 0.37.0 (wgpu 30) | MIT OR Apache-2.0 | `CallbackTrait` for the 3D stage | ✓ |
| `egui_extras` | 0.37.0 | MIT OR Apache-2.0 | `TableBuilder` (list view, targets, log), image loaders with the `svg` feature (logo, present glyph) | ✓ |
| `egui_tiles` | 0.18.0 | MIT OR Apache-2.0 | *Optional.* Only if user-rearrangeable panes are wanted later. v1 uses fixed `Panel`s | ✓ (egui ^0.37) |
| `egui_plot` | 0.38.0 | MIT OR Apache-2.0 | *Optional* for the inspector's 30-min charts and the channel scan. Custom painting is preferred for sparklines | ✓ (egui ^0.37) |
| `egui-notify` | 0.24.0 | MIT | *Optional* toasts. A ~120-line custom toast (§6.1) matches the design better | ✓ (egui ^0.37) |
| `egui_kittest` | 0.37.0 | MIT OR Apache-2.0 | UI snapshot + accessibility tests | ✓ |
| `rfd` | 0.17.2 | MIT | Native file dialogs (audio, `.lgshow`, firmware) | n/a |
| `glam` | 0.34.1 | MIT OR Apache-2.0 | Camera/matrix math | n/a |
| `bytemuck` | 1.25.2 | MIT/Apache-2.0/Zlib | GPU buffer structs | n/a |
| `fuzzy-matcher` | 0.3.7 | MIT | Command palette scoring (Skim algorithm). Alternative: `nucleo-matcher` 0.3.1 (MPL-2.0) | n/a |
| ~~`egui-phosphor`~~ | 0.14.0 | MIT OR Apache-2.0 | **Do not use yet**: depends on egui ^0.36. Embed the TTF directly (§7.3) | ✗ |
| ~~`egui_dock`~~ | 0.21.1 | MIT | **Do not use**: depends on egui ^0.36. Not needed for this layout | ✗ |

### 7.2 Fonts

| Font | Version | Licence | Files to embed (`assets/fonts/`) | Size |
|---|---|---|---|---|
| **Inter** (rsms) | 4.1 (2024-11-16) | SIL OFL 1.1 | `Inter-Regular.ttf`, `Inter-Medium.ttf`, `Inter-SemiBold.ttf` (from `Inter-4.1.zip` → `extras/ttf/`) | ~410 KB each |
| **Inter Tnum** (derived) | from Inter 4.1 | SIL OFL 1.1 (renamed per the OFL's Reserved Font Name rules: "Inter Tnum") | `InterTnum-Regular.ttf`, `InterTnum-Medium.ttf` | ~410 KB each |
| **Geist Mono** (Vercel) | 1.9.0 (2026-10-30) | SIL OFL 1.1 | `GeistMono-Regular.ttf`, `GeistMono-Medium.ttf` (from `geist-font-v1.9.0.zip` → `GeistMono/ttf/`) | ~150 KB each |

Why: Inter is the de-facto UI face of modern pro tools (Linear, Figma, Raycast) and is hinted for small sizes, which egui
0.34+ now honours. Geist Mono is a modern, slightly technical mono for hashes, timecode and logs. Use static instances, not the
variable `InterVariable.ttf`: egui supports variation coordinates through `FontTweak::coords`, but static files avoid
the known variation glyph-cache issues and are simpler.

**Tabular figures.** egui can't enable OpenType features at runtime, so bake `tnum` into a copy of Inter (a one-time asset step,
verified: all ten digits become 1328 units wide):

```sh
pip install opentype-feature-freezer
pyftfeatfreeze -f tnum -S -U Tnum Inter-Regular.ttf InterTnum-Regular.ttf
pyftfeatfreeze -f tnum -S -U Tnum Inter-Medium.ttf  InterTnum-Medium.ttf
```

Optional size trim: `pyftsubset` each Inter file to Latin + Latin-1 + General Punctuation + arrows (~120 KB each).

**Registration** (`fonts.rs`):

```rust
use egui::{FontData, FontDefinitions, FontFamily, FontTweak};
use std::sync::Arc;

pub fn install(ctx: &egui::Context) {
    let mut f = FontDefinitions::empty();
    let mut add = |name: &str, bytes: &'static [u8], tweak: Option<FontTweak>| {
        let mut d = FontData::from_static(bytes);
        if let Some(t) = tweak { d = d.tweak(t); }
        f.font_data.insert(name.to_owned(), Arc::new(d));
    };
    add("inter-400", include_bytes!("../assets/fonts/Inter-Regular.ttf"), None);
    add("inter-500", include_bytes!("../assets/fonts/Inter-Medium.ttf"), None);
    add("inter-600", include_bytes!("../assets/fonts/Inter-SemiBold.ttf"), None);
    add("tnum-400", include_bytes!("../assets/fonts/InterTnum-Regular.ttf"), None);
    add("tnum-500", include_bytes!("../assets/fonts/InterTnum-Medium.ttf"), None);
    add("mono-400", include_bytes!("../assets/fonts/GeistMono-Regular.ttf"), None);
    add("mono-500", include_bytes!("../assets/fonts/GeistMono-Medium.ttf"), None);
    // Phosphor: slightly larger and nudged down so 16 px icons sit centred on 13 px text.
    let icon_tweak = FontTweak { scale: 1.15, y_offset_factor: 0.10, ..Default::default() };
    add("ph", include_bytes!("../assets/fonts/Phosphor.ttf"), Some(icon_tweak.clone()));
    add("ph-fill", include_bytes!("../assets/fonts/Phosphor-Fill.ttf"), Some(icon_tweak));

    let mut fam = |family: FontFamily, names: &[&str]| {
        f.families.insert(family, names.iter().map(|s| s.to_string()).collect());
    };
    // Icon font goes FIRST: Inter has glyphs in the Private Use Area that collide with Phosphor's codepoints.
    // REQUIRES the PUA-only subset of Phosphor (§7.3): the stock TTF also maps a-z/space/hyphen for
    // ligatures and would hijack normal text.
    fam(FontFamily::Proportional, &["ph", "inter-400"]);
    fam(FontFamily::Name("medium".into()), &["ph", "inter-500"]);
    fam(FontFamily::Name("semibold".into()), &["ph", "inter-600"]);
    fam(FontFamily::Name("tnum".into()), &["tnum-400", "inter-400"]);
    fam(FontFamily::Name("tnum-medium".into()), &["tnum-500", "inter-500"]);
    fam(FontFamily::Monospace, &["mono-400", "inter-400"]);
    fam(FontFamily::Name("mono-medium".into()), &["mono-500", "inter-500"]);
    fam(FontFamily::Name("icons".into()), &["ph"]);
    fam(FontFamily::Name("icons-fill".into()), &["ph-fill"]);
    ctx.set_fonts(f);
}
```

After the first run, check in egui's `FontTweak` debug UI that icon baselines line up with the 13 px body text, and adjust
`y_offset_factor` if not. Keep `scale` so icons render at 16 px next to 13 px text.

### 7.3 Icons: Phosphor (regular + fill), embedded as a font

* **Phosphor Icons**, `@phosphor-icons/web` **2.1.2**, MIT. Files: `src/regular/Phosphor.ttf` and `src/fill/Phosphor-Fill.ttf`
  (unpkg/npm). Why: 1 500+ icons in a consistent 1.5 px-at-16 geometry that suits Inter. The font glyphs are tinted by text colour,
  crisp at any zoom, and cost one glyph per icon (no texture management). Fill is used only for selected/active states (rail
  selected item, play/pause, LIVE dot).
* With the subset font first in the proportional families, icons mix into text, e.g. `ui.button(format!("{}  Blink", icon::HAND_TAP))`.
  Or use a `RichText::new(icon::GIFT).family(FontFamily::Name("icons".into())).size(16.0)` atom for standalone icons.
* **Subsetting is mandatory, not an optimisation.** The stock TTF (1 543 cmap entries) also maps U+0020, U+002D and a–z for its
  ligature mechanism, so Latin text would come out as icon-font glyphs. Subset to the private-use range (or to just the used
  codepoints) and drop layout features:
  `pyftsubset Phosphor.ttf --unicodes=U+E000-F8FF --layout-features='' --output-file=assets/fonts/Phosphor.ttf`
  (do the same for `Phosphor-Fill.ttf`). With an explicit codepoint list (`--unicodes=U+E036,U+E0C0,…`), each file shrinks to roughly 0.5 KB per icon (about 40 KB for the ~80 icons listed below; the PUA-range subset stays ~440 KB).
  Commit the subset files, and keep the command in `assets/fonts/README` so adding an icon is one re-run.
* Lucide (ISC, `lucide-static` 1.66.0) is a fine alternative, but it ships as SVG and needs rasterisation via `egui_extras`' `svg`
  feature. Phosphor-as-font is simpler and sharper in egui.

Icon constants (Phosphor 2.1.2 regular codepoints, verified from the package CSS):

```rust
pub mod icon {
    pub const GIFT: &str = "\u{e276}";            // rail: Fleet, app glyph
    pub const WAVEFORM: &str = "\u{e802}";        // rail: Studio
    pub const CUBE: &str = "\u{e1da}";            // rail: Stage
    pub const DOWNLOAD: &str = "\u{e20c}";        // rail: Firmware, OTA
    pub const GEAR: &str = "\u{e272}";            // rail: Settings
    pub const BROADCAST: &str = "\u{e0f2}";       // LIVE / bridge TX
    pub const PLUGS_CONNECTED: &str = "\u{eb5a}"; // bridge connected
    pub const PLUGS: &str = "\u{eb56}";           // bridge disconnected
    pub const USB: &str = "\u{e956}";
    pub const RADIO: &str = "\u{e77e}";           // channel settings
    pub const SIGNAL_FULL: &str = "\u{e142}";
    pub const SIGNAL_X: &str = "\u{e14e}";
    pub const WIFI_SLASH: &str = "\u{e4f2}";      // offline
    pub const BATTERY_FULL: &str = "\u{e0c0}";
    pub const BATTERY_HIGH: &str = "\u{e0c2}";
    pub const BATTERY_MEDIUM: &str = "\u{e0c6}";
    pub const BATTERY_LOW: &str = "\u{e0c4}";
    pub const BATTERY_EMPTY: &str = "\u{e0be}";
    pub const BATTERY_WARNING: &str = "\u{e0c8}";
    pub const LIGHTNING: &str = "\u{e2de}";       // drop marker, Strobe
    pub const LIGHTNING_SLASH: &str = "\u{e2e0}"; // blackout
    pub const LIGHTBULB: &str = "\u{e2dc}";
    pub const HAND_TAP: &str = "\u{ec90}";        // blink / identify
    pub const TAG: &str = "\u{e478}";             // assign slot
    pub const PENCIL: &str = "\u{e3b4}";          // rename
    pub const REBOOT: &str = "\u{e036}";          // arrow-clockwise
    pub const UNDO: &str = "\u{e038}";            // arrow-counter-clockwise
    pub const POWER: &str = "\u{e3da}";
    pub const CPU: &str = "\u{e610}";             // bootloader
    pub const TERMINAL: &str = "\u{eae8}";        // event log
    pub const PLAY: &str = "\u{e3d0}";
    pub const PAUSE: &str = "\u{e39e}";
    pub const STOP: &str = "\u{e46c}";
    pub const SKIP_BACK: &str = "\u{e5a4}";
    pub const REPEAT: &str = "\u{e3f6}";          // loop
    pub const METRONOME: &str = "\u{ec8e}";       // snap / tempo
    pub const MUSIC_NOTES: &str = "\u{e340}";
    pub const FILE_AUDIO: &str = "\u{ea20}";
    pub const SPEAKER: &str = "\u{e44a}";
    pub const MAGIC_WAND: &str = "\u{e6b6}";      // auto-generate
    pub const SPARKLE: &str = "\u{e6a2}";         // Sparkle effect, New chip
    pub const FLAG: &str = "\u{e244}";            // section marker
    pub const FLAG_BANNER: &str = "\u{e622}";
    pub const SLIDERS: &str = "\u{e434}";
    pub const SEARCH: &str = "\u{e30c}";
    pub const COMMAND: &str = "\u{e1c4}";
    pub const EYE: &str = "\u{e220}";
    pub const CROSSHAIR: &str = "\u{e1d6}";       // frame / focus
    pub const CUBE_FOCUS: &str = "\u{ed0a}";
    pub const FULLSCREEN: &str = "\u{e0a2}";      // arrows-out, performance mode
    pub const MONITOR_PLAY: &str = "\u{e58c}";
    pub const GRID: &str = "\u{e464}";            // squares-four (grid view)
    pub const ROWS: &str = "\u{e5a2}";            // list view
    pub const LIST_NUMBERS: &str = "\u{e2f6}";    // number slots
    pub const FOLDER_OPEN: &str = "\u{e256}";
    pub const SAVE: &str = "\u{e248}";
    pub const TRASH: &str = "\u{e4a6}";
    pub const CLOCK: &str = "\u{e19a}";
    pub const TIMER: &str = "\u{e492}";           // latency
    pub const INFO: &str = "\u{e2ce}";
    pub const WARNING: &str = "\u{e4e0}";
    pub const WARNING_CIRCLE: &str = "\u{e4e2}";
    pub const X_CIRCLE: &str = "\u{e4f8}";
    pub const CHECK_CIRCLE: &str = "\u{e184}";
    pub const SPINNER: &str = "\u{eb44}";         // circle-notch (rotate via painter transform)
    pub const CHECK: &str = "\u{e182}";
    pub const X: &str = "\u{e4f6}";
    // Effects (index = lumigift_fx::effect id)
    pub const EFFECT: [&str; 8] = [
        "\u{e18a}", // Solid    circle
        "\u{e2ac}", // Pulse    heartbeat
        "\u{ebbc}", // Spectrum equalizer
        "\u{e0a0}", // Chase    arrows-left-right
        "\u{e2de}", // Strobe   lightning
        "\u{e598}", // Rainbow  rainbow
        "\u{e6a2}", // Sparkle  sparkle
        "\u{e5d2}", // Breathe  wind
    ];
}
```

To add an icon, look up its `.ph.ph-<name>:before { content: "\eXXX"; }` entry in `src/regular/style.css` (the fill font uses
the same codepoints), then re-run the subset command.

**Brand assets**: `images/present.svg` (gift glyph) for the rail logo, the app icon and empty states. Load it with `egui_extras`
(`svg` feature) via `egui::include_image!`, and tint `TEXT` in the rail (16 px) or apply the `ACCENT → CANDLE` gradient (empty states:
render the SVG to a mask texture and draw a gradient mesh with it). `images/LumiGift.svg` (the playful wordmark) appears only on the
About page and the splash. The UI never sets body text in it.

### 7.4 Settings screen details

* **Radio**: the current channel (0–125, shown with MHz = 2400 + ch). **Channel scan**: `[Scan channels]` makes the bridge sweep
  channels with the nRF24 RPD (received power > −64 dBm) bit, ~200 samples each (a bridge-protocol addition). The chart has 126 bars
  (4 px wide, 1 px gap, height = % of samples with RPD set) in `TEXT_MUTED`, with the current channel bar in `ACCENT` and the top three
  quietest channels marked with a ★ above. Overlays: Wi-Fi 1/6/11 occupied spans as `INFO_SOFT` bands labelled in `Small`, and BLE
  advertising channels 37/38/39 (2402/2426/2480 MHz) as 1 px `VIZ_SECTION` lines. Changing the channel needs every gift to move,
  but the over-the-air protocol has no channel-change command yet (`config_key` covers brightness, idle timeout and power only).
  Until it does, the channel selector is **disabled with that explanation** and the scan is informational.
* **Power & brightness**: max brightness cap (0–100%, `config_key::MAX_BRIGHTNESS`), idle timeout (1–60 s, `IDLE_TIMEOUT_MS`), the battery chemistry curve (Alkaline / NiMH). `[Apply to all gifts]`
  sends `SetConfig` and shows per-gift results (`ConfigResult.ok`) as a small list with checks/failures. Values apply instantly in
  the preview.
* **Audio**: output device (combo from cpal with "System default" first), `[Test tone]`, and the default latency offset with `[Calibrate…]`.
* **Simulator**: an enable toggle, gift count 1–30 (slider), and "Simulate problems" toggles: packet loss (0–50%), a low battery gift,
  a flaky gift (random reboots), random offline. Very useful for demos and for testing every UI state in §4.1.
* **Appearance**: UI zoom, Reduce motion, High contrast, Limit flashing in preview, timeline follow mode (Page / Continuous).
* **Shortcuts**: a read-only table of §3.5 with search.
* **About**: the wordmark, versions, licences for fonts/icons/crates (generated list).

---

## 8. Sources

Design references
* Linear, "How we redesigned the Linear UI (part II)": https://linear.app/now/how-we-redesigned-the-linear-ui
* Figma, "Inside the redesigned Figma, where your work takes center stage": https://www.figma.com/blog/behind-our-redesign-ui3/
* Figma, "Our approach to designing UI3": https://www.figma.com/blog/our-approach-to-designing-ui3/
* Ableton, Live 12 Manual, Keyboard Shortcuts: https://www.ableton.com/live-manual/12/live-keyboard-shortcuts/
* Ableton, Accessibility in Ableton Live FAQ: https://help.ableton.com/hc/en-us/articles/11762577169948
* CDM, "Ableton Live 12: everything new": https://cdm.link/ableton-live-12-everything-new/
* MA Lighting, grandMA3 User Manual, Color Themes: https://help2.malighting.com/Page/grandMA3/ws_colors_color_theme/en/1.4
* Resolume, Interface Layouts: https://www.resolume.com/support/en/layouts
* Vercel, Geist design system: https://vercel.com/geist and Geist font: https://vercel.com/geist/font
* Control.com, "Going Gray: A New HMI Standard": https://control.com/technical-articles/going-gray/
* ISA-101 HMI compliance how-to (Tatsoft): https://docs.tatsoft.com/display/FX/ISA-101+HMI+Compliance+How-to

Colour and accessibility
* Radix Colors, Understanding the scale: https://www.radix-ui.com/colors/docs/palette-composition/understanding-the-scale
* Tailwind CSS v4, Colors (OKLCH palette): https://tailwindcss.com/docs/colors
* Björn Ottosson, "A perceptual color space for image processing" (OKLab, the conversion used here): https://bottosson.github.io/posts/oklab/
* W3C, WCAG 2.2: Contrast (Minimum) 1.4.3: https://www.w3.org/WAI/WCAG22/Understanding/contrast-minimum.html
* W3C, WCAG 2.2: Non-text Contrast 1.4.11: https://www.w3.org/WAI/WCAG22/Understanding/non-text-contrast.html
* W3C, WCAG 2.2: Three Flashes or Below Threshold 2.3.1: https://www.w3.org/WAI/WCAG22/Understanding/three-flashes-or-below-threshold.html

UX patterns
* Nielsen Norman Group, "Confirmation Dialogs Can Prevent User Errors (If Not Overused)": https://www.nngroup.com/articles/confirmation-dialog/
* Nielsen Norman Group, "Designing Empty States in Complex Applications: 3 Guidelines": https://www.nngroup.com/articles/empty-state-interface-design/
* Retool, "Designing the command palette": https://retool.com/blog/designing-the-command-palette
* Blackmagic forum, Resolve timeline zoom (Alt+scroll) and snapping (N): https://forum.blackmagicdesign.com/viewtopic.php?p=428313

3D rendering
* LearnOpenGL, "Physically Based Bloom" (13-tap downsample, Karis average, tent upsample): https://learnopengl.com/Guest-Articles/2022/Phys.-Based-Bloom
* Jorge Jimenez, "Next Generation Post Processing in Call of Duty: Advanced Warfare" (SIGGRAPH 2014): https://www.iryoku.com/next-generation-post-processing-in-call-of-duty-advanced-warfare/
* Barré-Brisebois & Bouchard, "Approximating Translucency for a Fast, Cheap and Convincing Subsurface Scattering Look" (GDC 2011): https://www.ea.com/frostbite/news/approximating-translucency-for-a-fast-cheap-and-convincing-subsurface-scattering-look
* egui-wgpu `CallbackTrait` docs: https://docs.rs/egui-wgpu/latest/egui_wgpu/trait.CallbackTrait.html (see the `custom3d_wgpu` demo in the egui repo)

egui, crates, fonts, icons
* egui CHANGELOG (0.33–0.37: skrifa/vello_cpu text, font variations, `Panel` API, `App::ui`, `default_fonts` change): https://github.com/emilk/egui/blob/main/CHANGELOG.md
* egui 0.37 API docs: `Visuals` https://docs.rs/egui/0.37.0/egui/style/struct.Visuals.html, `Shadow` https://docs.rs/egui/0.37.0/egui/struct.Shadow.html, `Panel` https://docs.rs/egui/0.37.0/egui/containers/panel/struct.Panel.html, `FontTweak` https://docs.rs/epaint/0.37.0/epaint/text/struct.FontTweak.html
* crates.io (versions and dependency requirements, checked 2026-11-01): https://crates.io/crates/eframe, https://crates.io/crates/egui_extras, https://crates.io/crates/egui_plot, https://crates.io/crates/egui_tiles, https://crates.io/crates/egui-notify, https://crates.io/crates/egui-phosphor, https://crates.io/crates/egui_dock, https://crates.io/crates/rfd, https://crates.io/crates/glam, https://crates.io/crates/fuzzy-matcher
* Inter 4.1 (rsms): https://github.com/rsms/inter/releases/tag/v4.1 · https://rsms.me/inter/
* Geist font 1.9.0 (Vercel): https://github.com/vercel/geist-font/releases/tag/v1.9.0
* Phosphor Icons web package 2.1.2: https://github.com/phosphor-icons/web · https://phosphoricons.com
* Lucide (alternative): https://lucide.dev
* OpenType Feature Freezer (`pyftfeatfreeze`): https://github.com/twardoch/fonttools-opentype-feature-freezer
