//! Settings (docs/UI_DESIGN.md §3.2 Settings, §7.4).

use egui::{Align, Align2, CornerRadius, Layout, Rect, Sense, Stroke, Ui, pos2, vec2};
use lumigift_core::{CmdReport, Pending, Target, config_key};

use crate::app::App;
use crate::icons;
use crate::theme::{self, *};
use crate::widgets;

pub const SECTIONS: [&str; 7] = ["Radio", "Power & brightness", "Audio", "Simulator", "Appearance", "Shortcuts", "About"];

pub struct SettingsPage {
    pub section: usize,
    channel: u8,
    channel_init: bool,
    max_brightness: u16,
    power_ma: u16,
    idle_s: u16,
    apply: Vec<(String, Pending<CmdReport>)>,
    results: Vec<(String, CmdReport)>,
    loss_pct: f32,
    flaky: bool,
    shortcut_query: String,
    devices: Option<Vec<lumigift_audio::OutputDevice>>,
    sim_count: usize,
}

impl Default for SettingsPage {
    fn default() -> Self {
        Self {
            section: 0,
            channel: 76,
            channel_init: false,
            max_brightness: 100,
            power_ma: 250,
            idle_s: 5,
            apply: Vec::new(),
            results: Vec::new(),
            loss_pct: 2.0,
            flaky: false,
            shortcut_query: String::new(),
            devices: None,
            sim_count: 10,
        }
    }
}

pub fn page(app: &mut App, ui: &mut Ui) {
    // Resolve pending config results.
    let mut i = 0;
    while i < app.settings.apply.len() {
        if let Some(r) = app.settings.apply[i].1.try_take() {
            let (name, _) = app.settings.apply.remove(i);
            match r {
                Ok(rep) => app.settings.results.push((name, rep)),
                Err(e) => app.toasts.error(format!("{name} failed"), e.to_string()),
            }
        } else {
            i += 1;
        }
    }
    App::page_header(ui, "Settings", |_| {});
    let rect = ui.available_rect_before_wrap();
    // Section list (220 px).
    let list = Rect::from_min_size(rect.min, vec2(220.0, rect.height()));
    ui.painter().rect_filled(list, CornerRadius::ZERO, SURFACE_1);
    ui.painter().vline(list.right() - 0.5, list.y_range(), Stroke::new(1.0, BORDER_SUBTLE));
    let mut y = list.top() + 12.0;
    for (i, s) in SECTIONS.iter().enumerate() {
        let r = Rect::from_min_size(pos2(list.left() + 8.0, y), vec2(204.0, 30.0));
        y += 32.0;
        let resp = ui.interact(r, ui.scope_id().with(("sec", i)), Sense::click());
        let sel = app.settings.section == i;
        let p = ui.painter();
        if sel {
            p.rect_filled(r, CornerRadius::same(R_SM), ACTIVE);
        } else if resp.hovered() {
            p.rect_filled(r, CornerRadius::same(R_SM), HOVER);
        }
        p.text(pos2(r.left() + 12.0, r.center().y), Align2::LEFT_CENTER, *s, theme::font("button"), if sel { TEXT } else { TEXT_MUTED });
        if sel {
            widgets::paint_icon(p, pos2(r.right() - 14.0, r.center().y), icons::CARET_RIGHT, 12.0, TEXT_FAINT);
        }
        if resp.clicked() {
            app.settings.section = i;
        }
    }
    let content = Rect::from_min_max(pos2(list.right() + S7, rect.top()), rect.max);
    let mut cui = ui.new_child(egui::UiBuilder::new().max_rect(content).layout(Layout::top_down(Align::Min)));
    egui::ScrollArea::vertical().auto_shrink(false).show(&mut cui, |ui| {
        ui.set_max_width(720.0);
        ui.add_space(S7);
        match app.settings.section {
            0 => radio(app, ui),
            1 => power(app, ui),
            2 => audio(app, ui),
            3 => simulator(app, ui),
            4 => appearance(app, ui),
            5 => shortcuts(app, ui),
            _ => about(ui),
        }
        ui.add_space(S8);
    });
}

fn heading(ui: &mut Ui, title: &str, desc: &str) {
    widgets::overline(ui, title);
    ui.add_space(4.0);
    widgets::text(ui, desc, "small", TEXT_MUTED);
    ui.add_space(S5);
}

fn row(ui: &mut Ui, label: &str, add: impl FnOnce(&mut Ui)) {
    ui.horizontal(|ui| {
        widgets::column_label(ui, label, 180.0, CONTROL_H, TEXT_SECONDARY);
        add(ui);
    });
    ui.add_space(6.0);
}

fn radio(app: &mut App, ui: &mut Ui) {
    heading(ui, "Radio", "The 2.4 GHz channel shared by the bridge and every gift.");
    if !app.settings.channel_init && app.backend.is_some() {
        app.settings.channel = app.snap.bridge.radio.channel;
        app.settings.channel_init = true;
    }
    let cur = app.snap.bridge.radio.channel;
    row(ui, "Channel", |ui| {
        ui.add(egui::DragValue::new(&mut app.settings.channel).range(0..=125).speed(0.2));
        widgets::text(ui, format!("{} MHz", 2400 + app.settings.channel as u32), "value", TEXT_MUTED);
        let changed = app.settings.channel != cur;
        let ok = app.connected() && changed;
        let b = widgets::secondary_button(ui, Some(icons::RADIO), "Move fleet to channel", ok);
        let b = if !app.connected() { b.on_hover_text("Connect the bridge to change the channel") } else if !changed { b.on_hover_text("Already on this channel") } else { b };
        if b.clicked() && ok
            && let Some(l) = app.link().cloned() {
                let p = l.set_channel(app.settings.channel);
                app.settings.apply.push((format!("Channel {}", app.settings.channel), p));
            }
    });
    row(ui, "Current", |ui| {
        widgets::text(ui, format!("Channel {cur} · {} MHz", 2400 + cur as u32), "value", TEXT_SECONDARY);
    });
    ui.add_space(4.0);
    widgets::note(ui, "Every online gift is told to switch first (they move 1 s after replying), then the bridge follows. Offline gifts stay on the old channel until you switch back. Channels above 100 are usually clear of Wi-Fi.", SURFACE_2, TEXT_MUTED, 12.0);
    results(app, ui);
}

fn results(app: &mut App, ui: &mut Ui) {
    if app.settings.results.is_empty() && app.settings.apply.is_empty() {
        return;
    }
    ui.add_space(S5);
    widgets::overline(ui, "Results");
    ui.add_space(6.0);
    for (name, _) in &app.settings.apply {
        ui.horizontal(|ui| {
            widgets::spinner(ui, 14.0, INFO);
            widgets::text(ui, name, "body", TEXT_SECONDARY);
        });
    }
    let res = app.settings.results.clone();
    for (name, rep) in res.iter().rev().take(6) {
        ui.horizontal(|ui| {
            if rep.all_ok() {
                widgets::text(ui, icons::CHECK, "body", OK);
            } else {
                widgets::text(ui, icons::X_CIRCLE, "body", ERROR);
            }
            widgets::text(ui, name, "body", TEXT_SECONDARY);
            widgets::text(ui, format!("{} ok", rep.ok.len()), "value_small", TEXT_MUTED);
            for (u, e) in &rep.failed {
                widgets::text(ui, format!("· {}: {e}", app.gift_name(u)), "small", ERROR);
            }
        });
    }
}

fn power(app: &mut App, ui: &mut Ui) {
    heading(ui, "Power & brightness", "Limits stored on every gift. The power limiter keeps the AAA boost converter happy.");
    let s = &mut app.settings;
    row(ui, "Max brightness", |ui| {
        ui.add(egui::Slider::new(&mut s.max_brightness, 0..=100).show_value(false));
        widgets::text(ui, format!("{}%", s.max_brightness), "value", TEXT_SECONDARY);
    });
    row(ui, "LED power budget", |ui| {
        ui.add(egui::Slider::new(&mut s.power_ma, 50..=400).show_value(false).step_by(10.0));
        widgets::text(ui, format!("{} mA", s.power_ma), "value", TEXT_SECONDARY);
    });
    row(ui, "Idle timeout", |ui| {
        ui.add(egui::Slider::new(&mut s.idle_s, 1..=60).show_value(false));
        widgets::text(ui, format!("{} s", s.idle_s), "value", TEXT_SECONDARY);
    });
    row(ui, "Battery chemistry", |ui| {
        let mut i = app.persist.nimh as usize;
        if widgets::segmented(ui, "chem", &mut i, &["Alkaline", "NiMH"]) {
            app.persist.nimh = i == 1;
        }
    });
    ui.add_space(S3);
    let ok = app.connected();
    let b = widgets::primary_button(ui, Some(icons::CHECK), "Apply to all gifts", ok);
    let b = if !ok { b.on_hover_text("Connect the bridge to apply settings") } else { b };
    if b.clicked() && ok
        && let Some(l) = app.link().cloned() {
            let s = &app.settings;
            let mb = (s.max_brightness as u32 * 255 / 100) as u16;
            let items = [
                ("Max brightness".to_string(), config_key::MAX_BRIGHTNESS, mb),
                ("Power budget".to_string(), config_key::POWER_BUDGET_MA, s.power_ma),
                ("Idle timeout".to_string(), config_key::IDLE_TIMEOUT_MS, s.idle_s * 1000),
            ];
            for (n, k, v) in items {
                let p = l.set_config(Target::All, k, v);
                app.settings.apply.push((n, p));
            }
        }
    results(app, ui);
}

fn audio(app: &mut App, ui: &mut Ui) {
    heading(ui, "Audio", "Where the show's music plays, and how far the lights are shifted against it.");
    if app.settings.devices.is_none() {
        app.settings.devices = Some(lumigift_audio::Player::output_devices().unwrap_or_default());
    }
    let devs = app.settings.devices.clone().unwrap_or_default();
    row(ui, "Output device", |ui| {
        let cur = app.persist.audio_device.clone();
        let label = cur
            .as_ref()
            .and_then(|id| devs.iter().find(|d| &d.id == id).map(|d| d.name.clone()))
            .unwrap_or_else(|| "System default".into());
        egui::ComboBox::from_id_salt("audio-dev").width(300.0).selected_text(label).show_ui(ui, |ui| {
            if ui.selectable_label(cur.is_none(), "System default").clicked() {
                app.persist.audio_device = None;
                app.studio.set_device(None, &mut app.toasts);
            }
            for d in &devs {
                if ui.selectable_label(cur.as_deref() == Some(&d.id), &d.name).clicked() {
                    app.persist.audio_device = Some(d.id.clone());
                    app.studio.set_device(Some(d.id.clone()), &mut app.toasts);
                }
            }
        });
        if widgets::ghost_button(ui, icons::REBOOT, "Refresh devices", None).clicked() {
            app.settings.devices = None;
        }
    });
    row(ui, "Default latency", |ui| {
        ui.add(egui::Slider::new(&mut app.persist.default_latency_ms, -50.0..=250.0).show_value(false).step_by(1.0));
        widgets::text(ui, format!("{:.0} ms", app.persist.default_latency_ms), "value", TEXT_SECONDARY);
        if widgets::text_button(ui, "Use now", ACCENT_TEXT).clicked() {
            app.persist.latency_ms = app.persist.default_latency_ms;
        }
    });
    widgets::text(ui, "Radio + render latency. Increase if the gifts light up late. Double-click the Studio slider to reset to this value.", "small", TEXT_FAINT);
}

fn simulator(app: &mut App, ui: &mut Ui) {
    heading(ui, "Simulator", "Virtual gifts with realistic radio behaviour. Useful for demos and for testing every state.");
    let running = app.is_sim();
    if !running && app.settings.sim_count != app.persist.sim_count {
        app.settings.sim_count = app.persist.sim_count;
    }
    row(ui, "Gift count", |ui| {
        ui.add(egui::Slider::new(&mut app.settings.sim_count, 1..=30).show_value(false));
        widgets::text(ui, format!("{}", app.settings.sim_count), "value", TEXT_SECONDARY);
    });
    row(ui, "", |ui| {
        let label = if running { "Restart simulator" } else { "Start simulator" };
        if widgets::primary_button(ui, Some(icons::GIFT), label, true).clicked() {
            let n = app.settings.sim_count;
            app.start_sim(n);
        }
        if running && widgets::secondary_button(ui, Some(icons::PLUGS), "Stop", true).clicked() {
            if let Some(b) = app.backend.take() {
                b.link.shutdown();
            }
            app.live = false;
        }
    });
    ui.add_space(S5);
    widgets::overline(ui, "Simulate problems");
    ui.add_space(8.0);
    let sim = app.backend.as_ref().and_then(|b| b.sim.clone());
    ui.add_enabled_ui(sim.is_some(), |ui| {
        row(ui, "Packet loss", |ui| {
            let r = ui.add(egui::Slider::new(&mut app.settings.loss_pct, 0.0..=50.0).show_value(false));
            widgets::text(ui, format!("{:.0}%", app.settings.loss_pct), "value", TEXT_SECONDARY);
            if r.changed()
                && let Some(s) = &sim {
                    s.set_loss_pct(None, app.settings.loss_pct);
                }
        });
        row(ui, "Flaky gifts", |ui| {
            if widgets::toggle(ui, &mut app.settings.flaky).changed()
                && let Some(s) = &sim {
                    s.set_reboot_rate(if app.settings.flaky { 60.0 } else { 0.0 });
                }
            widgets::text(ui, "random reboots (~1 per minute per gift)", "small", TEXT_FAINT);
        });
        if let Some(s) = &sim {
            ui.add_space(8.0);
            widgets::text(ui, "Power individual gifts on or off (random offline):", "small", TEXT_MUTED);
            ui.add_space(6.0);
            let gifts = s.gifts();
            ui.horizontal_wrapped(|ui| {
                for g in gifts {
                    let name = app.gift_name(&g.uid);
                    let mut on = g.powered;
                    ui.horizontal(|ui| {
                        if widgets::toggle(ui, &mut on).changed() {
                            s.set_powered(g.index, on);
                        }
                        widgets::text(ui, &name, "small", TEXT_SECONDARY);
                        ui.add_space(10.0);
                    });
                }
            });
            ui.add_space(8.0);
            if widgets::secondary_button(ui, Some(icons::REBOOT), "Reboot a random gift", true).clicked() && !s.is_empty() {
                let i = (std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.subsec_nanos()).unwrap_or(0) as usize) % s.len();
                s.reboot(i);
            }
        }
    });
    if sim.is_none() {
        widgets::text(ui, "Start the simulator to inject faults.", "small", TEXT_FAINT);
    }
}

fn appearance(app: &mut App, ui: &mut Ui) {
    heading(ui, "Appearance", "Display preferences for this computer.");
    row(ui, "UI zoom", |ui| {
        let r = ui.add(egui::Slider::new(&mut app.persist.zoom, 0.8..=1.5).show_value(false).step_by(0.05));
        widgets::text(ui, format!("{:.0}%", app.persist.zoom * 100.0), "value", TEXT_SECONDARY);
        if r.drag_stopped() || r.changed() && !r.dragged() {
            ui.ctx().set_zoom_factor(app.persist.zoom);
        }
    });
    row(ui, "Reduce motion", |ui| {
        widgets::toggle(ui, &mut app.persist.reduce_motion);
        widgets::text(ui, "no page fades, radar or camera animation", "small", TEXT_FAINT);
    });
    row(ui, "Limit flashing", |ui| {
        widgets::toggle(ui, &mut app.persist.limit_flashing);
        widgets::text(ui, "in the on-screen preview only (never the gifts)", "small", TEXT_FAINT);
    });
    row(ui, "Timeline follow", |ui| {
        let mut i = app.persist.follow_continuous as usize;
        if widgets::segmented(ui, "follow", &mut i, &["Page", "Continuous"]) {
            app.persist.follow_continuous = i == 1;
        }
    });
}

pub const SHORTCUTS: &[(&str, &str, &str)] = &[
    ("Global", "Ctrl+K", "Command palette"),
    ("Global", "Ctrl+1…6", "Fleet / Studio / Lighting / Stage / Firmware / Settings"),
    ("Global", "Ctrl+I", "Toggle inspector"),
    ("Global", "Ctrl+J", "Toggle event log"),
    ("Global", "Ctrl+L", "Toggle LIVE"),
    ("Global", "Ctrl+Shift+B", "Blackout all gifts"),
    ("Global", "F11", "Performance mode (Esc exits)"),
    ("Global", "Ctrl+= / Ctrl+- / Ctrl+0", "UI zoom in / out / reset"),
    ("Global", "?", "Shortcut cheat sheet"),
    ("Fleet", "Arrows", "Move selection across tiles"),
    ("Fleet", "Enter", "Open selected in inspector"),
    ("Fleet", "B", "Blink selected gift(s)"),
    ("Fleet", "Shift+B", "Blink all (or the selected) in sequence, by slot"),
    ("Fleet", "F2", "Rename"),
    ("Fleet", "S", "Assign slot"),
    ("Fleet", "Ctrl+R", "Rescan"),
    ("Fleet", "Ctrl+A", "Select all gifts"),
    ("Fleet", "/", "Focus filter"),
    ("Transport", "Space", "Play / pause"),
    ("Transport", "Home / End", "Go to start / end"),
    ("Transport", "L", "Toggle loop"),
    ("Transport", "I / O", "Loop in / out at playhead"),
    ("Transport", "← / →", "Nudge playhead (or selected cues) one snap unit"),
    ("Transport", "Shift+← / →", "Nudge by one bar"),
    ("Transport", "Ctrl+← / →", "Previous / next section"),
    ("Transport", "Enter", "Return to where playback started"),
    ("Timeline", "Ctrl+Wheel / Alt+Wheel", "Zoom around the pointer"),
    ("Timeline", "Shift+Wheel", "Scroll horizontally"),
    ("Timeline", "+ / -", "Zoom around the playhead"),
    ("Timeline", "Z / Shift+Z", "Zoom to selection / fit song"),
    ("Timeline", "X", "Previous zoom"),
    ("Timeline", "F", "Toggle follow playhead"),
    ("Edit", "C", "Insert cue at playhead"),
    ("Edit", "M", "Add one-shot marker"),
    ("Edit", "N", "Toggle snap (hold Alt to bypass)"),
    ("Edit", "1…8", "Set effect of selected cues"),
    ("Edit", "Shift+1…8", "Set palette"),
    ("Edit", "[ / ]", "Brightness −/+ 10%"),
    ("Edit", "Delete", "Delete selection"),
    ("Edit", "Ctrl+D", "Duplicate selection"),
    ("Edit", "Ctrl+Z / Ctrl+Shift+Z", "Undo / redo"),
    ("Edit", "Ctrl+S / Ctrl+Shift+S / Ctrl+O", "Save / save as / open show"),
    ("Edit", "Ctrl+Shift+O", "Open audio file"),
    ("Edit", "Ctrl+G", "Auto-generate show"),
    ("Stage", "F", "Frame all"),
    ("Stage", "A", "Toggle Arrange mode"),
    ("Stage", "H", "Toggle labels"),
    ("Stage", "B", "Blink selected gift(s)"),
    ("Stage", "Shift+B", "Blink all (or the selected) in sequence, left to right"),
    ("Stage", "S", "Assign slot"),
    ("Stage", "Ctrl+A / Esc", "Select all / clear selection"),
    ("Stage", "1 / 2 / 3", "Camera: front / top / ¾"),
    ("Lighting", "Enter", "Apply the look to the target"),
    ("Lighting", "F", "Fade out / in"),
    ("Lighting", "B", "Lighting blackout / restore"),
];

/// Shortcut list in two columns grouped by scope.
pub fn shortcut_columns(ui: &mut Ui, query: &str) {
    let q = query.to_lowercase();
    let items: Vec<&(&str, &str, &str)> = SHORTCUTS
        .iter()
        .filter(|(s, k, d)| q.is_empty() || d.to_lowercase().contains(&q) || k.to_lowercase().contains(&q) || s.to_lowercase().contains(&q))
        .collect();
    let half = items.len().div_ceil(2);
    ui.columns(2, |cols| {
        for (ci, col) in cols.iter_mut().enumerate() {
            let slice = if ci == 0 { &items[..half.min(items.len())] } else { &items[half.min(items.len())..] };
            let mut last = "";
            for (scope, keys, desc) in slice {
                if *scope != last {
                    col.add_space(if last.is_empty() { 0.0 } else { 10.0 });
                    widgets::overline(col, scope);
                    col.add_space(4.0);
                    last = scope;
                }
                col.horizontal(|ui| {
                    ui.set_height(24.0);
                    widgets::text(ui, *desc, "body", TEXT_SECONDARY);
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        ui.add_space(12.0);
                        // keys may contain " / " alternatives
                        let alts: Vec<&str> = keys.split(" / ").collect();
                        for (i, a) in alts.iter().rev().enumerate() {
                            if i > 0 {
                                widgets::text(ui, "/", "small", TEXT_FAINT);
                            }
                            ui.horizontal(|ui| {
                                ui.spacing_mut().item_spacing.x = 3.0;
                                let parts: Vec<&str> = if *a == "+ " || a.ends_with("++") { vec![a] } else { a.split('+').filter(|s| !s.is_empty()).collect() };
                                for p in parts.iter() {
                                    widgets::keycap(ui, p.trim());
                                }
                            });
                        }
                    });
                });
            }
        }
    });
}

fn shortcuts(app: &mut App, ui: &mut Ui) {
    heading(ui, "Shortcuts", "Every action is also in the command palette (Ctrl+K).");
    ui.add(egui::TextEdit::singleline(&mut app.settings.shortcut_query).hint_text(widgets::rich(format!("{}  Search shortcuts", icons::SEARCH), "body", TEXT_FAINT)).desired_width(280.0).margin(vec2(8.0, 5.0)));
    ui.add_space(S5);
    let q = app.settings.shortcut_query.clone();
    shortcut_columns(ui, &q);
}

fn about(ui: &mut Ui) {
    ui.horizontal(|ui| {
        let (r, _) = ui.allocate_exact_size(vec2(56.0, 56.0), Sense::hover());
        widgets::brand_glyph(ui.painter(), r.center(), 44.0);
        ui.vertical(|ui| {
            widgets::text(ui, "Lumigift Studio", "display", TEXT);
            widgets::text(ui, format!("Version {}", env!("CARGO_PKG_VERSION")), "value_small", TEXT_MUTED);
        });
    });
    ui.add_space(S6);
    widgets::text(ui, "Music-synced light shows for battery-powered gift boxes.", "body", TEXT_SECONDARY);
    ui.add_space(S6);
    widgets::overline(ui, "Licences");
    ui.add_space(6.0);
    for (n, l) in [
        ("Inter 4.1 (and the derived Inter Tnum)", "SIL Open Font License 1.1"),
        ("Geist Mono 1.9.0", "SIL Open Font License 1.1"),
        ("Phosphor Icons 2.1.2", "MIT"),
        ("egui / eframe 0.37", "MIT OR Apache-2.0"),
        ("symphonia, cpal, realfft", "MPL-2.0 / Apache-2.0 / MIT"),
        ("rfd", "MIT"),
    ] {
        widgets::kv_row(ui, n, 300.0, |ui| {
            widgets::text(ui, l, "small", TEXT_MUTED);
        });
    }
}
