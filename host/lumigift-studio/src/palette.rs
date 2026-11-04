//! Command palette (Ctrl+K), docs/UI_DESIGN.md §3.4.

use std::time::Instant;

use egui::{Align2, Color32, CornerRadius, Id, Key, Modifiers, Order, Rect, Sense, Stroke, StrokeKind, pos2, vec2};
use lumigift_core::Uid;

use crate::app::{App, Page};
use crate::icons;
use crate::stage::Arrangement;
use crate::theme::{self, *};
use crate::widgets;

#[derive(Clone, Debug, PartialEq)]
pub enum PaletteAction {
    Nav(Page),
    ToggleInspector,
    ToggleEventLog,
    ToggleLive,
    Blackout,
    Perform,
    Cheatsheet,
    Blink(Uid),
    BlinkSelected,
    Select(Uid),
    Rename(Uid),
    AssignSlot(Uid),
    Reboot(Uid),
    RebootAll,
    Rescan,
    SleepAll,
    WakeAll,
    AutoAssign,
    StartSim,
    OpenAudio,
    OpenShow,
    SaveShow,
    SaveShowAs,
    AutoGenerate,
    PlayPause,
    GoToStart,
    Seek(f64),
    SeekBar(u32),
    InsertCue,
    ChooseFirmware,
    ZoomIn,
    ZoomOut,
    ZoomReset,
    FrameAll,
    Arrange(Arrangement),
    ReduceMotion,
    SettingsSection(usize),
    SearchLog(String),
    LightingEffect(u8),
    LightingPreset(usize),
    LightingFadeOut,
    LightingOff,
    LightingRelease,
}

#[derive(Clone)]
pub struct Command {
    pub group: &'static str,
    pub icon: &'static str,
    pub label: String,
    pub context: String,
    pub keys: Option<&'static str>,
    pub action: PaletteAction,
    pub gift: bool,
}

fn cmd(group: &'static str, icon: &'static str, label: &str, keys: Option<&'static str>, action: PaletteAction) -> Command {
    Command { group, icon, label: label.into(), context: String::new(), keys, action, gift: false }
}

pub fn build_commands(app: &App) -> Vec<Command> {
    use PaletteAction as A;
    let mut v = Vec::new();
    for (i, p) in Page::ALL.iter().enumerate() {
        let keys: &'static str = ["Ctrl+1", "Ctrl+2", "Ctrl+3", "Ctrl+4", "Ctrl+5", "Ctrl+6"][i];
        v.push(cmd("Navigation", p.icon(), &format!("Go to {}", p.name()), Some(keys), A::Nav(*p)));
    }
    if !app.selection.is_empty() {
        let n = app.selection.len();
        let label = if n == 1 { format!("Blink {}", app.gift_name(app.selection.first().unwrap())) } else { format!("Blink {n} selected gifts") };
        v.push(Command { group: "Suggestions", ..cmd("Suggestions", icons::HAND_TAP, &label, Some("B"), A::BlinkSelected) });
    }
    if app.studio.has_track() {
        v.push(cmd("Suggestions", icons::PLUS, "Insert cue at playhead", Some("C"), A::InsertCue));
    }
    for g in &app.snap.gifts {
        let name = app.gift_name(&g.uid);
        let ctx = format!("{} · {}", name, g.slot.map(crate::health::fmt_slot).unwrap_or("no slot".into()));
        let mk = |icon, verb: &str, a: PaletteAction| Command {
            group: "Gifts",
            icon,
            label: format!("{verb} {name}"),
            context: ctx.clone(),
            keys: None,
            action: a,
            gift: true,
        };
        v.push(mk(icons::HAND_TAP, "Blink", A::Blink(g.uid)));
        v.push(mk(icons::CURSOR, "Select", A::Select(g.uid)));
        v.push(mk(icons::PENCIL, "Rename", A::Rename(g.uid)));
        v.push(mk(icons::TAG, "Assign slot for", A::AssignSlot(g.uid)));
        v.push(mk(icons::REBOOT, "Reboot", A::Reboot(g.uid)));
    }
    v.push(cmd("Gifts", icons::REBOOT, "Reboot all gifts…", None, A::RebootAll));
    v.push(cmd("Gifts", icons::SEARCH, "Rescan for gifts", Some("Ctrl+R"), A::Rescan));
    v.push(cmd("Gifts", icons::LIST_NUMBERS, "Auto-assign slots", None, A::AutoAssign));
    v.push(cmd("Gifts", icons::MOON, "Sleep all gifts", None, A::SleepAll));
    v.push(cmd("Gifts", icons::SUN, "Wake all gifts", None, A::WakeAll));
    v.push(cmd("Show", icons::FILE_AUDIO, "Open audio file…", Some("Ctrl+Shift+O"), A::OpenAudio));
    v.push(cmd("Show", icons::FOLDER_OPEN, "Open show…", Some("Ctrl+O"), A::OpenShow));
    v.push(cmd("Show", icons::SAVE, "Save show", Some("Ctrl+S"), A::SaveShow));
    v.push(cmd("Show", icons::SAVE, "Save show as…", Some("Ctrl+Shift+S"), A::SaveShowAs));
    v.push(cmd("Show", icons::MAGIC_WAND, "Auto-generate show", Some("Ctrl+G"), A::AutoGenerate));
    v.push(cmd("Show", icons::BROADCAST, if app.live { "Switch to Preview" } else { "Go LIVE" }, Some("Ctrl+L"), A::ToggleLive));
    v.push(cmd("Show", icons::LIGHTNING_SLASH, "Blackout all gifts", Some("Ctrl+Shift+B"), A::Blackout));
    for &e in crate::pages::lighting::LIBRARY.iter() {
        let name = lumigift_fx::EFFECTS[e as usize].name;
        v.push(cmd("Lighting", icons::EFFECT[e as usize], &format!("Lighting: {name}"), None, A::LightingEffect(e)));
    }
    for (i, p) in app.persist.lighting.presets.iter().enumerate() {
        v.push(cmd("Lighting", icons::STAR, &format!("Lighting: {}", p.name), None, A::LightingPreset(i)));
    }
    v.push(cmd("Lighting", icons::MOON, if app.lighting.fade_out { "Lighting: Fade in" } else { "Lighting: Fade out" }, None, A::LightingFadeOut));
    v.push(cmd("Lighting", icons::POWER, "Lighting: Lights off", None, A::LightingOff));
    if app.lighting.armed {
        v.push(cmd("Lighting", icons::X, "Lighting: Release the gifts", None, A::LightingRelease));
    }
    v.push(cmd("Transport", icons::PLAY, "Play / pause", Some("Space"), A::PlayPause));
    v.push(cmd("Transport", icons::SKIP_BACK, "Go to start", Some("Home"), A::GoToStart));
    v.push(cmd("Firmware", icons::DOWNLOAD, "Choose firmware image…", None, A::ChooseFirmware));
    v.push(cmd("View", icons::SIDEBAR, "Toggle inspector", Some("Ctrl+I"), A::ToggleInspector));
    v.push(cmd("View", icons::TERMINAL, "Toggle event log", Some("Ctrl+J"), A::ToggleEventLog));
    v.push(cmd("View", icons::FULLSCREEN, "Performance mode", Some("F11"), A::Perform));
    v.push(cmd("View", icons::CROSSHAIR, "Frame all gifts", Some("F"), A::FrameAll));
    for (n, a) in [("Row", Arrangement::Row), ("Arc", Arrangement::Arc), ("Grid", Arrangement::Grid), ("Circle", Arrangement::Circle), ("Pyramid", Arrangement::Pyramid)] {
        v.push(cmd("View", icons::MOVE, &format!("Arrange stage: {n}"), None, A::Arrange(a)));
    }
    v.push(cmd("View", icons::ZOOM_IN, "Zoom in", Some("Ctrl+="), A::ZoomIn));
    v.push(cmd("View", icons::ZOOM_OUT, "Zoom out", Some("Ctrl+-"), A::ZoomOut));
    v.push(cmd("View", icons::FIT, "Reset zoom", Some("Ctrl+0"), A::ZoomReset));
    v.push(cmd("View", icons::KEYBOARD, "Keyboard shortcuts", Some("?"), A::Cheatsheet));
    v.push(cmd("Settings", icons::GIFT, "Start simulator", None, A::StartSim));
    v.push(cmd("Settings", icons::EYE, "Toggle reduce motion", None, A::ReduceMotion));
    for (i, s) in crate::pages::settings::SECTIONS.iter().enumerate() {
        v.push(cmd("Settings", icons::GEAR, &format!("Settings: {s}"), None, A::SettingsSection(i)));
    }
    v
}

/// Fuzzy subsequence score (higher is better); None if not a match.
pub fn fuzzy(query: &str, hay: &str) -> Option<i32> {
    if query.is_empty() {
        return Some(0);
    }
    let h: Vec<char> = hay.to_lowercase().chars().collect();
    let mut score = 0i32;
    let mut hi = 0usize;
    let mut last_match: Option<usize> = None;
    for word in query.to_lowercase().split_whitespace() {
        for qc in word.chars() {
            let mut found = false;
            while hi < h.len() {
                if h[hi] == qc {
                    let bonus = if hi == 0 || !h[hi - 1].is_alphanumeric() { 8 } else { 0 };
                    let consec = if last_match == Some(hi.wrapping_sub(1)) { 5 } else { 0 };
                    score += 1 + bonus + consec;
                    last_match = Some(hi);
                    hi += 1;
                    found = true;
                    break;
                }
                hi += 1;
            }
            if !found {
                // Allow words in any order: retry this word from the start.
                return fuzzy_any_order(query, &h);
            }
        }
    }
    Some(score - (h.len() as i32) / 8)
}

fn fuzzy_any_order(query: &str, h: &[char]) -> Option<i32> {
    let mut total = 0;
    for w in query.to_lowercase().split_whitespace() {
        let mut hi = 0;
        let mut s = 0;
        for qc in w.chars() {
            let pos = h[hi..].iter().position(|c| *c == qc)?;
            hi += pos + 1;
            s += 1;
        }
        total += s;
    }
    Some(total - 4)
}

#[derive(Default)]
pub struct CommandPalette {
    open: bool,
    query: String,
    selected: usize,
    opened_at: Option<Instant>,
    recent: Vec<String>,
    focus_pending: bool,
}

impl CommandPalette {
    pub fn is_open(&self) -> bool {
        self.open
    }

    pub fn open(&mut self) {
        self.open = !self.open;
        self.query.clear();
        self.selected = 0;
        self.opened_at = Some(Instant::now());
        self.focus_pending = true;
    }

    fn results<'a>(&self, cmds: &'a [Command]) -> Vec<(&'static str, &'a Command)> {
        let q = self.query.trim();
        if q.is_empty() {
            let mut out: Vec<(&'static str, &Command)> = Vec::new();
            for r in self.recent.iter().take(5) {
                if let Some(c) = cmds.iter().find(|c| &c.label == r) {
                    out.push(("Recent", c));
                }
            }
            for c in cmds.iter().filter(|c| c.group == "Suggestions") {
                out.push(("Suggestions", c));
            }
            for g in ["Navigation", "Gifts", "Show", "Lighting", "Transport", "Firmware", "View", "Settings"] {
                for c in cmds.iter().filter(|c| c.group == g) {
                    // Keep per-gift commands out of the unfiltered list except "Blink".
                    if c.gift && !matches!(c.action, PaletteAction::Blink(_)) {
                        continue;
                    }
                    out.push((g, c));
                }
            }
            return out;
        }
        let (mode, q) = match q.chars().next() {
            Some('>') => ('>', q[1..].trim()),
            Some('@') => ('@', q[1..].trim()),
            _ => (' ', q),
        };
        let mut scored: Vec<(i32, &Command)> = cmds
            .iter()
            .filter(|c| c.group != "Suggestions")
            .filter(|c| match mode {
                '>' => !c.gift,
                '@' => c.gift,
                _ => true,
            })
            .filter_map(|c| {
                let hay = format!("{} {}", c.label, c.context);
                fuzzy(q, &hay).map(|s| {
                    let rec = if self.recent.contains(&c.label) { 3 } else { 0 };
                    (s + rec, c)
                })
            })
            .collect();
        scored.sort_by_key(|s| std::cmp::Reverse(s.0));
        scored.into_iter().take(40).map(|(_, c)| (c.group, c)).collect()
    }

    /// Paint the palette; returns an action to run.
    pub fn show(&mut self, ctx: &egui::Context, cmds: &[Command]) -> Option<PaletteAction> {
        if !self.open {
            return None;
        }
        let mut action = None;
        let screen = ctx.content_rect();
        let age = self.opened_at.map(|t| t.elapsed().as_secs_f32()).unwrap_or(1.0);
        let t = egui::emath::easing::cubic_out((age / MOTION_BASE).min(1.0));
        if t < 1.0 {
            ctx.request_repaint();
        }
        // Scrim
        egui::Area::new(Id::unique("palette-scrim"))
            .order(Order::Foreground)
            .fixed_pos(screen.min)
            .show(ctx, |ui| {
                let (r, resp) = ui.allocate_exact_size(screen.size(), Sense::click());
                ui.painter().rect_filled(r, CornerRadius::ZERO, Color32::from_black_alpha((102.0 * t) as u8));
                if resp.clicked() {
                    self.open = false;
                }
            });
        // Special prefixes producing synthetic results.
        let q = self.query.trim().to_owned();
        let mut synthetic: Vec<Command> = Vec::new();
        if let Some(rest) = q.strip_prefix(':') {
            let rest = rest.trim();
            if let Some((m, s)) = rest.split_once(':') {
                if let (Ok(m), Ok(s)) = (m.parse::<f64>(), s.parse::<f64>()) {
                    synthetic.push(cmd("Transport", icons::CLOCK, &format!("Go to {m}:{s:04.1}"), None, PaletteAction::Seek(m * 60.0 + s)));
                }
            } else if let Ok(b) = rest.parse::<u32>() {
                synthetic.push(cmd("Transport", icons::CLOCK, &format!("Go to bar {b}"), None, PaletteAction::SeekBar(b)));
            }
        }
        if let Some(rest) = q.strip_prefix('#')
            && let Ok(s) = rest.trim().parse::<u8>() {
                for c in cmds.iter().filter(|c| matches!(c.action, PaletteAction::Select(_)) && c.context.contains(&format!("#{s:02}"))) {
                    synthetic.push(c.clone());
                }
            }
        let results: Vec<(&'static str, Command)> = if q.starts_with(':') || q.starts_with('#') {
            synthetic.iter().map(|c| (c.group, c.clone())).collect()
        } else {
            self.results(cmds).into_iter().map(|(g, c)| (g, c.clone())).collect()
        };

        // Keys
        let n = results.len();
        ctx.input_mut(|i| {
            if i.consume_key(Modifiers::NONE, Key::ArrowDown) && n > 0 {
                self.selected = (self.selected + 1) % n;
            }
            if i.consume_key(Modifiers::NONE, Key::ArrowUp) && n > 0 {
                self.selected = (self.selected + n - 1) % n;
            }
            if i.consume_key(Modifiers::NONE, Key::Escape) {
                self.open = false;
            }
        });
        if self.selected >= n {
            self.selected = n.saturating_sub(1);
        }
        let enter = ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Enter));
        let ctrl_enter = ctx.input_mut(|i| i.consume_key(Modifiers::COMMAND, Key::Enter));

        let w = 560.0;
        let max_h = 440.0;
        let pos = pos2(screen.center().x - w / 2.0, screen.top() + screen.height() * 0.18 - 8.0 * (1.0 - t));
        let mut run_idx: Option<usize> = None;
        egui::Area::new(Id::unique("palette"))
            .order(Order::Foreground)
            .fixed_pos(pos)
            .show(ctx, |ui| {
                ui.set_opacity(t);
                let frame = egui::Frame::new()
                    .fill(SURFACE_3)
                    .stroke(Stroke::new(1.0, BORDER))
                    .corner_radius(CornerRadius::same(R_LG))
                    .shadow(theme::shadow_modal());
                frame.show(ui, |ui| {
                    ui.set_width(w);
                    // Search field (44 px)
                    ui.horizontal(|ui| {
                        ui.set_height(44.0);
                        ui.add_space(14.0);
                        widgets::text(ui, icons::SEARCH, "body", TEXT_MUTED);
                        let te = egui::TextEdit::singleline(&mut self.query)
                            .hint_text(widgets::rich("Type a command or search gifts…", "body", TEXT_FAINT))
                            .frame(egui::Frame::NONE)
                            .font(theme::font("body"))
                            .desired_width(w - 120.0);
                        let resp = ui.add(te);
                        if self.focus_pending {
                            resp.request_focus();
                            self.focus_pending = false;
                        }
                        if resp.changed() {
                            self.selected = 0;
                        }
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            ui.add_space(14.0);
                            widgets::keycap(ui, "Esc");
                        });
                    });
                    let sep = ui.allocate_exact_size(vec2(w, 1.0), Sense::hover()).0;
                    ui.painter().hline(sep.x_range(), sep.center().y, Stroke::new(1.0, BORDER));
                    egui::ScrollArea::vertical().max_height(max_h - 46.0).auto_shrink([false, true]).show(ui, |ui| {
                        ui.add_space(6.0);
                        if results.is_empty() {
                            ui.add_space(8.0);
                            ui.horizontal(|ui| {
                                ui.add_space(16.0);
                                widgets::text(ui, format!("No matches for '{}'", self.query.trim()), "body", TEXT_MUTED);
                            });
                            ui.add_space(4.0);
                            let r = row(ui, w, icons::TERMINAL, &format!("Search event log for '{}'", self.query.trim()), "", None, true);
                            if r.clicked() || enter {
                                action = Some(PaletteAction::SearchLog(self.query.trim().to_owned()));
                                self.open = false;
                            }
                            ui.add_space(8.0);
                            return;
                        }
                        let mut last_group = "";
                        for (i, (g, c)) in results.iter().enumerate() {
                            if *g != last_group {
                                if !last_group.is_empty() {
                                    ui.add_space(4.0);
                                }
                                ui.horizontal(|ui| {
                                    ui.add_space(16.0);
                                    widgets::overline(ui, g);
                                });
                                last_group = g;
                            }
                            let r = row(ui, w, c.icon, &c.label, &c.context, c.keys, i == self.selected);
                            if i == self.selected && (enter || ctrl_enter) {
                                r.scroll_to_me(None);
                            }
                            if r.hovered() && ui.input(|i| i.pointer.delta().length() > 0.0) {
                                self.selected = i;
                            }
                            if r.clicked() {
                                run_idx = Some(i);
                            }
                        }
                        ui.add_space(6.0);
                    });
                });
            });
        if (enter || ctrl_enter) && n > 0 && action.is_none() {
            run_idx = Some(self.selected);
        }
        if let Some(i) = run_idx {
            let c = &results[i].1;
            self.recent.retain(|r| r != &c.label);
            self.recent.insert(0, c.label.clone());
            self.recent.truncate(5);
            action = Some(c.action.clone());
            if !ctrl_enter {
                self.open = false;
            }
        }
        action
    }
}

fn row(ui: &mut egui::Ui, w: f32, icon: &str, label: &str, context: &str, keys: Option<&str>, sel: bool) -> egui::Response {
    let (r, resp) = ui.allocate_exact_size(vec2(w, 32.0), Sense::click());
    let inner = Rect::from_min_max(pos2(r.left() + 6.0, r.top()), pos2(r.right() - 6.0, r.bottom()));
    let p = ui.painter();
    if sel {
        p.rect_filled(inner, CornerRadius::same(R_SM), ACTIVE);
        p.rect_filled(
            Rect::from_min_size(pos2(inner.left(), inner.top() + 6.0), vec2(2.0, inner.height() - 12.0)),
            CornerRadius::same(1),
            ACCENT,
        );
    } else if resp.hovered() {
        p.rect_filled(inner, CornerRadius::same(R_SM), HOVER);
    }
    widgets::paint_icon(p, pos2(inner.left() + 20.0, r.center().y), icon, 16.0, if sel { TEXT_SECONDARY } else { TEXT_MUTED });
    let lr = p.text(pos2(inner.left() + 40.0, r.center().y), Align2::LEFT_CENTER, label, theme::font("body"), TEXT);
    if !context.is_empty() {
        p.text(pos2(lr.right() + 10.0, r.center().y), Align2::LEFT_CENTER, context, theme::font("small"), if sel || resp.hovered() { TEXT_MUTED } else { TEXT_FAINT });
    }
    if let Some(k) = keys {
        // Right-aligned keycaps, painted manually.
        let font = theme::font("small");
        let mut x = inner.right() - 10.0;
        for part in k.split('+').rev() {
            let kw = widgets::text_width(ui, part, font.clone()).max(8.0) + 10.0;
            let kr = Rect::from_min_size(pos2(x - kw, r.center().y - 9.0), vec2(kw, 18.0));
            p.rect_filled(kr, CornerRadius::same(R_XS), SURFACE_3);
            p.rect_stroke(kr, CornerRadius::same(R_XS), Stroke::new(1.0, BORDER), StrokeKind::Inside);
            p.text(kr.center(), Align2::CENTER_CENTER, part, font.clone(), TEXT_MUTED);
            x -= kw + 3.0;
        }
    }
    resp
}
