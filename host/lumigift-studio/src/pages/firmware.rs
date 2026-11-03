//! Firmware update page (docs/UI_DESIGN.md §3.2 Firmware, §4.3, §6.3).

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use egui::{Align, Align2, CornerRadius, Layout, Rect, Sense, Stroke, StrokeKind, Ui, pos2, vec2};
use lumigift_core::{OtaPhase, OtaReport, OtaTargetPhase, Pending, Snapshot, Uid};
use lumigift_proto::ImageHeader;
use lumigift_proto::image::crc32;
use lumigift_proto::layout::{APP_MAX_SIZE, IMAGE_HEADER_SIZE};

use crate::app::{App, modal_frame};
use crate::health::{self, fmt_fw};
use crate::icons;
use crate::theme::{self, *};
use crate::toasts::Toasts;
use crate::widgets::{self, GemState};

pub struct FwImage {
    pub name: String,
    pub bytes: Vec<u8>,
    pub header: Option<ImageHeader>,
    pub crc_ok: bool,
    pub error: Option<String>,
}

#[derive(Default)]
pub struct Firmware {
    pub image: Option<FwImage>,
    targets: BTreeSet<Uid>,
    touched: bool,
    pending: Option<Pending<OtaReport>>,
    report: Option<Result<OtaReport, String>>,
    confirm: bool,
    cancel_confirm: bool,
    eta: Option<f32>,
    shown_pct: f32,
    finished_at: Option<Instant>,
    retry_targets: Vec<Uid>,
    show_run: bool,
}

impl Firmware {
    pub fn pick(&mut self) {
        if let Some(p) = rfd::FileDialog::new()
            .set_title("Choose a firmware image")
            .add_filter("Lumigift firmware", &["lgfw", "bin"])
            .pick_file()
        {
            self.load(p);
        }
    }

    pub fn load(&mut self, path: PathBuf) {
        let name = path.file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
        let img = match std::fs::read(&path) {
            Ok(bytes) => {
                let header = ImageHeader::decode(&bytes);
                let mut error = None;
                let mut crc_ok = false;
                match &header {
                    None => {
                        error = Some(if bytes.len() < 32 {
                            "The file is too small to be a firmware image.".into()
                        } else if u32::from_le_bytes(bytes[0..4].try_into().unwrap()) != ImageHeader::MAGIC {
                            "Not a Lumigift image (bad magic number).".into()
                        } else {
                            "The image header is corrupted (header CRC mismatch or bad size).".into()
                        })
                    }
                    Some(h) => {
                        let total = h.image_len() as usize;
                        if h.app_len > APP_MAX_SIZE {
                            error = Some(format!("Too large: {} bytes, the app region holds {}.", h.app_len, APP_MAX_SIZE));
                        } else if bytes.len() < total {
                            error = Some(format!("Truncated: the header says {} bytes, the file has {}.", total, bytes.len()));
                        } else {
                            let app = &bytes[IMAGE_HEADER_SIZE as usize..total];
                            crc_ok = crc32(app) == h.app_crc;
                            if !crc_ok {
                                error = Some("App CRC mismatch: the file is corrupted.".into());
                            }
                        }
                    }
                }
                FwImage { name, bytes, header, crc_ok, error }
            }
            Err(e) => FwImage { name, bytes: Vec::new(), header: None, crc_ok: false, error: Some(format!("Couldn't read the file: {e}")) },
        };
        self.image = Some(img);
        self.touched = false;
        self.report = None;
        self.show_run = false;
    }

    /// Newest firmware version known (fleet or loaded image).
    pub fn newest_version(&self, snap: &Snapshot) -> Option<u32> {
        let fleet = snap.gifts.iter().filter(|g| !g.in_bootloader).map(|g| g.fw_version).max();
        let img = self.image.as_ref().and_then(|i| i.header.filter(|_| i.error.is_none()).map(|h| h.fw_version));
        fleet.max(img)
    }

    fn valid(&self) -> Option<&FwImage> {
        self.image.as_ref().filter(|i| i.error.is_none() && i.header.is_some())
    }

    /// Firmware version of the loaded image, if it is valid.
    fn new_version(&self) -> Option<u32> {
        self.valid().and_then(|i| i.header).map(|h| h.fw_version)
    }

    /// Number of 24-byte OTA chunks in the valid image (0 without one).
    fn chunks(&self) -> u32 {
        self.valid().map(|i| (i.bytes.len() as u32).div_ceil(24)).unwrap_or(0)
    }

    pub fn running(&self) -> bool {
        self.pending.is_some()
    }

    pub fn post_frame(&mut self, toasts: &mut Toasts) {
        if let Some(p) = &mut self.pending
            && let Some(r) = p.try_take() {
                self.pending = None;
                self.finished_at = Some(Instant::now());
                match &r {
                    Ok(rep) if rep.failed.is_empty() => toasts.success(format!("Firmware updated on {} gift{}", rep.updated.len(), if rep.updated.len() == 1 { "" } else { "s" })),
                    Ok(rep) => {
                        toasts.push(crate::toasts::ToastKind::Warn, format!("{} updated, {} failed", rep.updated.len(), rep.failed.len()), None);
                    }
                    Err(e) => toasts.error("Firmware update stopped", e.to_string()),
                }
                self.retry_targets = match &r {
                    Ok(rep) => rep.failed.iter().map(|f| f.0).collect(),
                    Err(_) => Vec::new(),
                };
                self.report = Some(r.map_err(|e| e.to_string()));
            }
    }
}

#[derive(PartialEq)]
enum Exclusion {
    None,
    Offline,
    LowBattery,
    WeakLink,
}

fn exclusion(app: &App, uid: &Uid) -> Exclusion {
    let Some(g) = app.snap.gift(uid) else { return Exclusion::Offline };
    // Asleep gifts can be updated: the update wakes its targets first.
    if !g.online && !g.asleep {
        return Exclusion::Offline;
    }
    let v = app.health.get(uid).and_then(|h| h.vbat_median_mv).unwrap_or(g.vbat_mv as f32);
    if v > 0.0 && v < 1150.0 {
        return Exclusion::LowBattery;
    }
    if g.online && g.poll_success_pct < 70.0 {
        return Exclusion::WeakLink;
    }
    Exclusion::None
}

fn estimate_secs(n: usize, chunks: u32) -> f32 {
    if n == 0 {
        return 0.0;
    }
    let rate = 400.0; // chunks/s, OtaOptions default pacing
    1.0 + 0.6 + chunks as f32 / rate + 0.4 + 1.5 + 0.5
}

pub fn page(app: &mut App, ui: &mut Ui) {
    let running = app.firmware.running() || app.snap.ota.as_ref().is_some_and(|o| o.finished.is_none());
    App::page_header(ui, "Firmware update", |ui| {
        if running {
            widgets::pill(ui, Some(icons::DOWNLOAD), "Updating", INFO, INFO_SOFT);
        }
    });
    egui::ScrollArea::vertical().auto_shrink(false).show(ui, |ui| {
        egui::Frame::new().inner_margin(egui::Margin::same(24)).show(ui, |ui| {
            ui.set_max_width(980.0);
            image_card(app, ui, running);
            ui.add_space(S5);
            if app.firmware.show_run || running {
                run_view(app, ui);
            } else {
                targets(app, ui);
            }
        });
    });
    confirm_modal(app, ui.ctx());
}

fn image_card(app: &mut App, ui: &mut Ui, running: bool) {
    let hovering_file = ui.ctx().input(|i| !i.raw.hovered_files.is_empty());
    let w = ui.available_width();
    match &app.firmware.image {
        None => {
            let (r, resp) = ui.allocate_exact_size(vec2(w, 88.0), Sense::click());
            let p = ui.painter();
            p.rect_filled(r, CornerRadius::same(R_MD), if resp.hovered() { SURFACE_2 } else { SURFACE_1 });
            widgets::dashed_rect(p, r.shrink(0.5), Stroke::new(1.0, if hovering_file { ACCENT } else { BORDER_STRONG }), 5.0, 4.0);
            widgets::paint_icon(p, pos2(r.left() + 40.0, r.center().y), icons::FILE_UP, 28.0, TEXT_MUTED);
            p.text(pos2(r.left() + 72.0, r.center().y - 9.0), Align2::LEFT_CENTER, "Drop a firmware image here", theme::font("title"), TEXT);
            p.text(pos2(r.left() + 72.0, r.center().y + 12.0), Align2::LEFT_CENTER, ".lgfw files built by the firmware workspace", theme::font("small"), TEXT_MUTED);
            let br = Rect::from_min_size(pos2(r.right() - 150.0, r.center().y - 14.0), vec2(130.0, 28.0));
            let mut c = ui.new_child(egui::UiBuilder::new().max_rect(br));
            let b = widgets::primary_button(&mut c, Some(icons::FOLDER_OPEN), "Choose file…", true);
            if b.clicked() || resp.clicked() {
                app.firmware.pick();
            }
        }
        Some(img) => {
            let name = img.name.clone();
            let error = img.error.clone();
            let header = img.header;
            let size = img.bytes.len();
            let crc_ok = img.crc_ok;
            widgets::card_ui(ui, |ui| {
                ui.set_width(w - 24.0);
                ui.horizontal(|ui| {
                    let (ir, _) = ui.allocate_exact_size(vec2(40.0, 40.0), Sense::hover());
                    ui.painter().rect_filled(ir, CornerRadius::same(R_MD), if error.is_some() { ERROR_SOFT } else { ACCENT_SOFT });
                    widgets::paint_icon(ui.painter(), ir.center(), icons::DOWNLOAD, 22.0, if error.is_some() { ERROR } else { ACCENT_TEXT });
                    ui.add_space(4.0);
                    ui.vertical(|ui| {
                        ui.spacing_mut().item_spacing.y = 4.0;
                        ui.horizontal(|ui| {
                            widgets::text(ui, &name, "title", TEXT);
                            if let Some(h) = header {
                                widgets::text(ui, format!("{} · ", fmt_fw(h.fw_version)), "value_small", TEXT_MUTED);
                                widgets::text(ui, hex8(&h.git_hash), "mono", TEXT_MUTED);
                                widgets::text(ui, format!(" · {:.1} KiB", size as f32 / 1024.0), "value_small", TEXT_MUTED);
                            }
                        });
                        if error.is_none()
                            && let Some(h) = header {
                                ui.horizontal(|ui| {
                                    ui.spacing_mut().item_spacing.x = 4.0;
                                    widgets::text(ui, icons::CHECK, "body", OK);
                                    widgets::text(ui, format!("Header valid · hw rev {} · CRC ", health::hw_rev_letter(h.hw_rev as u8)), "small", TEXT_MUTED);
                                    widgets::text(ui, format!("0x{:08X}", h.app_crc), "mono", TEXT_MUTED);
                                    if crc_ok {
                                        widgets::text(ui, "verified", "small", TEXT_FAINT);
                                    }
                                });
                            }
                    });
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if widgets::secondary_button(ui, None, "Choose another…", !running).clicked() && !running {
                            app.firmware.pick();
                        }
                    });
                });
                if let Some(e) = &error {
                    ui.add_space(10.0);
                    egui::Frame::new().fill(ERROR_SOFT).corner_radius(CornerRadius::same(R_SM)).inner_margin(egui::Margin::symmetric(12, 8)).show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        ui.horizontal(|ui| {
                            widgets::text(ui, icons::X_CIRCLE, "body", ERROR);
                            widgets::text(ui, e, "body", TEXT);
                        });
                    });
                }
            });
        }
    }
}

fn hex8(h: &[u8; 8]) -> String {
    h.iter().take(4).map(|b| format!("{b:02x}")).collect()
}

fn targets(app: &mut App, ui: &mut Ui) {
    let new_ver = app.firmware.new_version();
    let gifts = app.snap.gifts.clone();
    // Default selection: online, not excluded, firmware differs.
    if !app.firmware.touched {
        app.firmware.targets = gifts
            .iter()
            .filter(|g| exclusion(app, &g.uid) == Exclusion::None && new_ver.is_none_or(|v| v != g.fw_version || g.in_bootloader))
            .map(|g| g.uid)
            .collect();
    }
    let offline: Vec<Uid> = app.firmware.targets.iter().filter(|u| exclusion(app, u) == Exclusion::Offline).copied().collect();
    for u in offline {
        app.firmware.targets.remove(&u);
    }
    ui.horizontal(|ui| {
        widgets::overline(ui, "Targets");
        ui.add_space(12.0);
        widgets::text(ui, "Select:", "small", TEXT_FAINT);
        if widgets::text_button(ui, "All", ACCENT_TEXT).clicked() {
            app.firmware.targets = gifts.iter().filter(|g| g.online || g.asleep).map(|g| g.uid).collect();
            app.firmware.touched = true;
        }
        widgets::text(ui, "·", "small", TEXT_FAINT);
        if widgets::text_button(ui, "Outdated", ACCENT_TEXT).clicked() {
            app.firmware.targets = gifts.iter().filter(|g| (g.online || g.asleep) && new_ver.is_some_and(|v| g.fw_version < v)).map(|g| g.uid).collect();
            app.firmware.touched = true;
        }
        widgets::text(ui, "·", "small", TEXT_FAINT);
        if widgets::text_button(ui, "None", ACCENT_TEXT).clicked() {
            app.firmware.targets.clear();
            app.firmware.touched = true;
        }
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            widgets::text(ui, format!("{} of {} selected", app.firmware.targets.len(), gifts.len()), "small", TEXT_MUTED);
        });
    });
    ui.add_space(8.0);
    let w = ui.available_width();
    let p0 = ui.cursor().min;
    let list_h = gifts.len().max(1) as f32 * 40.0;
    let frame_r = Rect::from_min_size(p0, vec2(w, list_h + 2.0));
    widgets::paint_card(ui.painter(), frame_r, SURFACE_2, BORDER_SUBTLE);
    ui.allocate_exact_size(vec2(w, list_h + 2.0), Sense::hover());
    for (i, g) in gifts.iter().enumerate() {
        let r = Rect::from_min_size(pos2(p0.x, p0.y + 1.0 + i as f32 * 40.0), vec2(w, 40.0));
        if i > 0 {
            ui.painter().hline((r.left() + 12.0)..=(r.right() - 12.0), r.top(), Stroke::new(1.0, BORDER_SUBTLE));
        }
        let ex = exclusion(app, &g.uid);
        let resp = ui.interact(r, ui.scope_id().with(("tgt", g.uid)), if ex == Exclusion::Offline { Sense::hover() } else { Sense::click() });
        if resp.hovered() && ex != Exclusion::Offline {
            ui.painter().rect_filled(r.shrink(1.0), CornerRadius::same(R_SM), theme::alpha(HOVER, 0.5));
        }
        let checked = app.firmware.targets.contains(&g.uid);
        if resp.clicked() {
            app.firmware.touched = true;
            if checked {
                app.firmware.targets.remove(&g.uid);
            } else {
                app.firmware.targets.insert(g.uid);
            }
        }
        let p = ui.painter();
        let y = r.center().y;
        let mut x = r.left() + 14.0;
        // Checkbox
        let cb = Rect::from_center_size(pos2(x + 8.0, y), vec2(16.0, 16.0));
        if ex == Exclusion::Offline {
            p.rect_stroke(cb, CornerRadius::same(R_XS), Stroke::new(1.0, BORDER), StrokeKind::Inside);
        } else if checked {
            p.rect_filled(cb, CornerRadius::same(R_XS), ACCENT);
            widgets::paint_icon(p, cb.center(), icons::CHECK, 12.0, ON_ACCENT);
        } else {
            p.rect_filled(cb, CornerRadius::same(R_XS), BG_SUNKEN);
            p.rect_stroke(cb, CornerRadius::same(R_XS), Stroke::new(1.0, BORDER_INPUT), StrokeKind::Inside);
        }
        x += 30.0;
        widgets::gem(p, Rect::from_center_size(pos2(x + 10.0, y), vec2(20.0, 20.0)), &app.lights.get(&g.uid), if g.online { GemState::Live } else { GemState::Offline });
        x += 32.0;
        let name = app.gift_name(&g.uid);
        p.text(pos2(x, y), Align2::LEFT_CENTER, &name, theme::font("button"), if g.online { TEXT } else { TEXT_MUTED });
        x += 130.0;
        let slot = g.slot.map(health::fmt_slot).unwrap_or("—".into());
        p.text(pos2(x, y), Align2::LEFT_CENTER, slot, theme::font("value_small"), TEXT_MUTED);
        x += 52.0;
        let mid = match ex {
            Exclusion::Offline => ("Offline".to_string(), TEXT_FAINT, None),
            Exclusion::LowBattery => ("Battery low: excluded".into(), WARN, Some(icons::WARNING)),
            Exclusion::WeakLink => ("Weak link: excluded".into(), WARN, Some(icons::WARNING)),
            Exclusion::None => match new_ver {
                Some(v) if v == g.fw_version && !g.in_bootloader => (format!("{}  same", fmt_fw(g.fw_version)), TEXT_FAINT, None),
                Some(v) if v < g.fw_version => (format!("{} → {}", fmt_fw(g.fw_version), fmt_fw(v)), WARN, Some(icons::WARNING)),
                Some(v) => (format!("{} → {}", if g.in_bootloader { "recovery".into() } else { fmt_fw(g.fw_version) }, fmt_fw(v)), TEXT_SECONDARY, None),
                None => (fmt_fw(g.fw_version), TEXT_SECONDARY, None),
            },
        };
        let mut mx = x;
        if let Some(ic) = mid.2 {
            widgets::paint_icon(p, pos2(mx + 7.0, y), ic, 14.0, mid.1);
            mx += 20.0;
        }
        p.text(pos2(mx, y), Align2::LEFT_CENTER, &mid.0, theme::font("value_small"), mid.1);
        if matches!(ex, Exclusion::None) && new_ver.is_some_and(|v| v < g.fw_version) {
            p.text(pos2(mx + widgets::text_width(ui, &mid.0, theme::font("value_small")) + 8.0, y), Align2::LEFT_CENTER, "Downgrade", theme::font("small_medium"), WARN);
        }
        if g.online {
            let rx = r.right() - 170.0;
            let (v, lvl) = health::battery_state(app.health.get(&g.uid), app.persist.nimh);
            let bc = if lvl.is_some_and(|l| l < 0.25) { WARN } else { TEXT_SECONDARY };
            let p = ui.painter();
            widgets::battery(p, pos2(rx, y - 5.5), lvl, bc);
            p.text(pos2(rx + 28.0, y), Align2::LEFT_CENTER, v.map(|v| format!("{v:.2} V")).unwrap_or("—".into()), theme::font("value"), TEXT_SECONDARY);
            widgets::link_meter(p, pos2(rx + 90.0, y - 7.0), Some(g.poll_success_pct), TEXT_SECONDARY);
            p.text(pos2(rx + 112.0, y), Align2::LEFT_CENTER, format!("{:.0}%", g.poll_success_pct), theme::font("value"), TEXT_SECONDARY);
        }
        if matches!(ex, Exclusion::LowBattery | Exclusion::WeakLink) {
            resp.on_hover_text("Excluded automatically. You can still check it, but the update may fail or leave it in recovery mode.");
        }
    }
    if gifts.is_empty() {
        ui.painter().text(frame_r.center(), Align2::CENTER_CENTER, "No gifts discovered yet", theme::font("body"), TEXT_FAINT);
    }
    ui.add_space(S7);
    let n = app.firmware.targets.len();
    let chunks = app.firmware.chunks();
    ui.horizontal(|ui| {
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            let enabled = app.firmware.valid().is_some() && n > 0 && app.connected() && !app.live;
            let label = format!("Update {n} gift{}", health::plural(n));
            let b = widgets::primary_button_h(ui, Some(icons::DOWNLOAD), &label, enabled, CTA_H);
            let why = if app.firmware.image.is_none() {
                Some("Choose a firmware image first")
            } else if app.firmware.valid().is_none() {
                Some("This image is invalid")
            } else if n == 0 {
                Some("Select at least one gift")
            } else if app.live {
                Some("Switch to Preview before updating firmware")
            } else if !app.connected() {
                Some("Connect the bridge to update gifts")
            } else {
                None
            };
            let b = if let Some(w) = why { b.on_hover_text(w) } else { b };
            if b.clicked() && enabled {
                app.firmware.confirm = true;
            }
            if n > 0 && chunks > 0 {
                widgets::text(ui, format!("~{:.0} s · {n} gifts", estimate_secs(n, chunks).ceil()), "body", TEXT_MUTED);
            }
        });
    });
}

fn confirm_modal(app: &mut App, ctx: &egui::Context) {
    if !app.firmware.confirm {
        return;
    }
    let n = app.firmware.targets.len();
    let mut close = false;
    let r = egui::Modal::new(egui::Id::unique("ota-confirm")).backdrop_color(SCRIM).frame(modal_frame()).show(ctx, |ui| {
        ui.set_width(460.0);
        widgets::text(ui, format!("Update firmware on {n} gift{}?", health::plural(n)), "title", TEXT);
        ui.add_space(12.0);
        let new_ver = app.firmware.new_version().unwrap_or(0);
        let names: Vec<String> = app.firmware.targets.iter().map(|u| app.gift_name(u)).collect();
        let shown = if names.len() > 5 { format!("{} and {} more", names[..5].join(", "), names.len() - 5) } else { names.join(", ") };
        widgets::kv_row(ui, "Gifts", 90.0, |ui| { ui.add(egui::Label::new(widgets::rich(shown, "body", TEXT_SECONDARY)).wrap()); });
        widgets::kv_text(ui, "Version", 90.0, format!("→ {}", fmt_fw(new_ver)), "value");
        let chunks = app.firmware.chunks();
        widgets::kv_text(ui, "Time", 90.0, format!("about {:.0} seconds", estimate_secs(n, chunks).ceil()), "body");
        let excluded: Vec<String> = app.snap.gifts.iter().filter(|g| !app.firmware.targets.contains(&g.uid)).map(|g| {
            let why = match exclusion(app, &g.uid) {
                Exclusion::Offline => "offline",
                Exclusion::LowBattery => "battery low",
                Exclusion::WeakLink => "weak link",
                Exclusion::None => "not selected",
            };
            format!("{} ({why})", app.gift_name(&g.uid))
        }).collect();
        if !excluded.is_empty() {
            widgets::kv_row(ui, "Skipped", 90.0, |ui| { ui.add(egui::Label::new(widgets::rich(excluded.join(", "), "small", TEXT_MUTED)).wrap()); });
        }
        let downgrade = app.firmware.targets.iter().any(|u| app.snap.gift(u).is_some_and(|g| g.fw_version >= new_ver && !g.in_bootloader));
        if downgrade {
            ui.add_space(6.0);
            widgets::text(ui, format!("{}  Not a newer version for some gifts", icons::WARNING), "body", WARN);
        }
        ui.add_space(12.0);
        widgets::note(ui, "Keep the gifts switched on and within range. If an update is interrupted, the gift stays in recovery mode and you can simply run the update again. It can't be bricked.", SURFACE_2, TEXT_MUTED, 10.0);
        ui.add_space(20.0);
        ui.horizontal(|ui| {
            if widgets::secondary_button(ui, None, "Cancel", true).clicked() {
                close = true;
            }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if widgets::primary_button(ui, Some(icons::DOWNLOAD), &format!("Update {n} gift{}", health::plural(n)), true).clicked() {
                    start(app, app.firmware.targets.iter().copied().collect());
                    close = true;
                }
            });
        });
    });
    if close || r.should_close() {
        app.firmware.confirm = false;
    }
}

/// Start updating every currently selected target (used by the screenshot script).
pub fn start_all(app: &mut App) {
    let t: Vec<Uid> = app.firmware.targets.iter().copied().collect();
    start(app, t);
}

fn start(app: &mut App, targets: Vec<Uid>) {
    let Some(link) = app.link().cloned() else { return };
    let Some(img) = app.firmware.valid() else { return };
    let bytes = img.bytes.clone();
    app.firmware.pending = Some(link.start_ota(bytes, targets));
    app.firmware.report = None;
    app.firmware.show_run = true;
    app.firmware.eta = None;
    app.firmware.shown_pct = 0.0;
}

const PHASES: [&str; 6] = ["Reboot", "Erase", "Transfer", "Repair", "Verify", "Boot"];

fn phase_index(p: OtaTargetPhase, missing: u16) -> usize {
    match p {
        OtaTargetPhase::Waiting | OtaTargetPhase::EnteringBootloader => 0,
        OtaTargetPhase::Erasing => 1,
        OtaTargetPhase::Receiving if missing > 0 => 3,
        OtaTargetPhase::Receiving => 2,
        OtaTargetPhase::Committing => 4,
        OtaTargetPhase::Verified | OtaTargetPhase::Booting => 5,
        OtaTargetPhase::Done => 6,
        OtaTargetPhase::Failed | OtaTargetPhase::Cancelled => 7,
    }
}

fn stepper(p: &egui::Painter, ui: &Ui, origin: egui::Pos2, cur: usize, labels: bool) -> f32 {
    let mut x = origin.x;
    let t = ui.input(|i| i.time) as f32;
    for (i, name) in PHASES.iter().enumerate() {
        let c = pos2(x + 4.0, origin.y);
        if i < cur {
            widgets::paint_icon(p, c, icons::CHECK, 13.0, OK);
        } else if i == cur {
            let a = 0.6 + 0.4 * (t * 4.0).sin().abs();
            p.circle_filled(c, 4.0, theme::alpha(INFO, a));
        } else {
            p.circle_stroke(c, 3.5, Stroke::new(1.0, BORDER_STRONG));
        }
        x += 12.0;
        if labels {
            let r = p.text(pos2(x, origin.y), Align2::LEFT_CENTER, *name, theme::font("small"), if i == cur { TEXT } else if i < cur { TEXT_MUTED } else { TEXT_FAINT });
            x = r.right() + 8.0;
            if i + 1 < PHASES.len() {
                p.hline(x..=(x + 14.0), origin.y, Stroke::new(1.0, BORDER));
                x += 22.0;
            }
        }
    }
    x
}

fn run_view(app: &mut App, ui: &mut Ui) {
    let Some(ota) = app.snap.ota.clone() else {
        widgets::text(ui, "Starting…", "body", TEXT_MUTED);
        return;
    };
    let finished = ota.finished.is_some() && app.firmware.pending.is_none();
    let n = ota.targets.len();
    let total_pct: f32 = ota.targets.iter().map(|t| if matches!(t.phase, OtaTargetPhase::Done) { 100.0 } else { t.percent }).sum::<f32>() / n.max(1) as f32;
    let dt = ui.input(|i| i.stable_dt);
    app.firmware.shown_pct += (total_pct - app.firmware.shown_pct) * (1.0 - (-dt / 0.25).exp());
    let failed = ota.targets.iter().filter(|t| t.phase == OtaTargetPhase::Failed).count();
    let done = ota.targets.iter().filter(|t| t.phase == OtaTargetPhase::Done).count();
    // ETA: remaining chunks / rate + phase constants, smoothed (τ = 2 s), never counting up by > 2 s.
    let elapsed = ota.started.elapsed().as_secs_f32();
    let rate = if elapsed > 0.5 { ota.chunks_sent as f32 / elapsed } else { 400.0 };
    let remaining_chunks = (ota.chunks as f32 * (1.0 - total_pct / 100.0)).max(0.0);
    let consts = match ota.phase {
        OtaPhase::Validating | OtaPhase::EnteringBootloader => 1.0 + 0.6 + 0.4 + 1.5,
        OtaPhase::Beginning => 0.6 + 0.4 + 1.5,
        OtaPhase::Streaming | OtaPhase::Repairing => 0.4 + 1.5 + 0.5,
        OtaPhase::Committing => 1.5,
        _ => 0.5,
    };
    let raw = remaining_chunks / rate.max(50.0) + consts;
    let eta = match app.firmware.eta {
        None => raw,
        Some(prev) => {
            let k = 1.0 - (-dt / 2.0).exp();
            let next = prev + (raw - prev) * k;
            next.min(prev + 2.0 * dt.max(0.0) + 0.0001).max(0.0)
        }
    };
    app.firmware.eta = Some(eta);

    // Overall card.
    let all_ok = finished && failed == 0;
    widgets::card_ui(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.horizontal(|ui| {
            let title = if finished {
                if failed == 0 { format!("Updated {done} gift{}", health::plural(done)) } else { format!("Update finished with {failed} failure{}", health::plural(failed)) }
            } else if ota.phase == OtaPhase::Cancelled {
                "Update cancelled".into()
            } else {
                format!("Updating {n} gift{}", health::plural(n))
            };
            if all_ok {
                // Check draws in over 400 ms.
                let a = app.firmware.finished_at.map(|t| (t.elapsed().as_secs_f32() / MOTION_DELIBERATE).min(1.0)).unwrap_or(1.0);
                widgets::text(ui, icons::CHECK_CIRCLE, "title", theme::alpha(OK, a));
            }
            widgets::text(ui, title, "title", TEXT);
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if !finished {
                    let s = if eta > 30.0 { (eta / 5.0).round() * 5.0 } else { eta.ceil() };
                    widgets::text(ui, format!("~{s:.0} s left"), "body", TEXT_MUTED);
                    ui.add_space(12.0);
                }
                widgets::text(ui, format!("{:.0}%", app.firmware.shown_pct), "metric", TEXT);
            });
        });
        ui.add_space(10.0);
        let (r, _) = ui.allocate_exact_size(vec2(ui.available_width(), 8.0), Sense::hover());
        widgets::progress_bar(ui.painter(), r, app.firmware.shown_pct / 100.0, if all_ok { OK } else { INFO });
        if failed > 0 {
            // Error segments proportional to failures.
            let seg = r.width() / n.max(1) as f32;
            for i in 0..failed {
                let x = r.right() - seg * (i + 1) as f32;
                ui.painter().rect_filled(Rect::from_min_size(pos2(x + 1.0, r.top()), vec2(seg - 2.0, r.height())), CornerRadius::same(R_PILL), ERROR);
            }
        }
        ui.add_space(12.0);
        let (sr, _) = ui.allocate_exact_size(vec2(ui.available_width(), 18.0), Sense::hover());
        let slowest = ota.targets.iter().filter(|t| !matches!(t.phase, OtaTargetPhase::Failed | OtaTargetPhase::Cancelled)).map(|t| phase_index(t.phase, t.missing)).min().unwrap_or(6);
        stepper(ui.painter(), ui, sr.left_center(), slowest, true);
        if !finished {
            ui.painter().text(sr.right_center(), Align2::RIGHT_CENTER, "Don't switch gifts off", theme::font("small"), TEXT_FAINT);
        }
        if finished {
            ui.add_space(12.0);
            ui.horizontal(|ui| {
                widgets::text(ui, format!("{} {done} updated", icons::CHECK), "body", OK);
                if failed > 0 {
                    widgets::text(ui, format!("· {} {failed} failed", icons::X), "body", ERROR);
                }
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if widgets::secondary_button(ui, None, "Done", true).clicked() {
                        app.firmware.show_run = false;
                        app.firmware.touched = false;
                    }
                    if failed > 0 && widgets::primary_button(ui, Some(icons::REBOOT), &format!("Retry failed ({failed})"), true).clicked() {
                        let t = app.firmware.retry_targets.clone();
                        start(app, t);
                    }
                });
            });
        } else {
            ui.add_space(10.0);
            ui.horizontal(|ui| {
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    let b = widgets::secondary_button(ui, Some(icons::STOP), "Cancel", true);
                    if b.clicked() {
                        app.firmware.cancel_confirm = true;
                    }
                    if app.firmware.cancel_confirm {
                        egui::Popup::new(egui::Id::unique("ota-cancel"), ui.ctx().clone(), b.rect, ui.layer_id())
                            .open(true)
                            .frame(theme::popover())
                            .width(300.0)
                            .show(|ui| {
                                ui.add(egui::Label::new(widgets::rich("Stop the update? Gifts already erased stay in recovery mode until you run the update again.", "body", TEXT_SECONDARY)).wrap());
                                ui.add_space(10.0);
                                ui.horizontal(|ui| {
                                    if widgets::danger_button(ui, None, "Stop update", true).clicked() {
                                        if let Some(l) = app.link() {
                                            l.cancel_ota();
                                        }
                                        app.firmware.cancel_confirm = false;
                                    }
                                    if widgets::secondary_button(ui, None, "Keep going", true).clicked() {
                                        app.firmware.cancel_confirm = false;
                                    }
                                });
                            });
                    }
                });
            });
        }
    });
    ui.add_space(S5);
    // Per-gift rows.
    for t in &ota.targets {
        let failed = matches!(t.phase, OtaTargetPhase::Failed);
        let done = matches!(t.phase, OtaTargetPhase::Done);
        let tint_ok = done && app.firmware.finished_at.is_none_or(|f| f.elapsed() < Duration::from_millis(1200));
        let w = ui.available_width();
        let h = if failed { 64.0 } else { 48.0 };
        let (r, _) = ui.allocate_exact_size(vec2(w, h), Sense::hover());
        let (fill, stroke) = if failed { (ERROR_SOFT, ERROR) } else if tint_ok { (OK_SOFT, BORDER_SUBTLE) } else { (SURFACE_2, BORDER_SUBTLE) };
        widgets::paint_card(ui.painter(), r, fill, stroke);
        let p = ui.painter();
        let y = r.top() + 24.0;
        let mut x = r.left() + 12.0;
        let online = app.snap.gift(&t.uid).is_some_and(|g| g.online);
        if failed {
            widgets::paint_icon(p, pos2(x + 10.0, y), icons::X_CIRCLE, 18.0, ERROR);
        } else {
            widgets::gem(p, Rect::from_center_size(pos2(x + 12.0, y), vec2(24.0, 24.0)), &app.lights.get(&t.uid), if online { GemState::Live } else { GemState::Offline });
        }
        x += 36.0;
        p.text(pos2(x, y), Align2::LEFT_CENTER, app.gift_name(&t.uid), theme::font("button"), TEXT);
        x += 120.0;
        let slot = app.snap.gift(&t.uid).and_then(|g| g.slot).map(health::fmt_slot).unwrap_or("—".into());
        p.text(pos2(x, y), Align2::LEFT_CENTER, slot, theme::font("value_small"), TEXT_MUTED);
        x += 44.0;
        // 7-dot stepper
        let cur = phase_index(t.phase, t.missing);
        let tt = ui.input(|i| i.time) as f32;
        for i in 0..7 {
            let c = pos2(x + 3.0 + i as f32 * 12.0, y);
            if i < cur.min(7) {
                p.circle_filled(c, 3.0, if failed { theme::alpha(ERROR, 0.6) } else { OK });
            } else if i == cur && !failed {
                p.circle_filled(c, 3.0, theme::alpha(INFO, 0.6 + 0.4 * (tt * 4.0).sin().abs()));
            } else {
                p.circle_stroke(c, 2.5, Stroke::new(1.0, BORDER_STRONG));
            }
        }
        x += 96.0;
        let label = match t.phase {
            OtaTargetPhase::Waiting => "Waiting".to_string(),
            OtaTargetPhase::EnteringBootloader => "Rebooting into bootloader".into(),
            OtaTargetPhase::Erasing => "Erasing".into(),
            OtaTargetPhase::Receiving if t.missing > 0 && ota.round > 0 => format!("Repairing {} missed chunks (round {})", t.missing, ota.round),
            OtaTargetPhase::Receiving => format!("Transferring {} / {} chunks", ((t.percent / 100.0) * ota.chunks as f32) as u32, ota.chunks),
            OtaTargetPhase::Committing => "Verifying".into(),
            OtaTargetPhase::Verified | OtaTargetPhase::Booting => "Starting new firmware".into(),
            OtaTargetPhase::Done => format!("Updated to {}", fmt_fw(ota.fw_version)),
            OtaTargetPhase::Failed => "Failed".into(),
            OtaTargetPhase::Cancelled => "Cancelled".into(),
        };
        p.text(pos2(x, y), Align2::LEFT_CENTER, label, theme::font("body"), if failed { TEXT } else { TEXT_SECONDARY });
        let br = Rect::from_min_size(pos2(r.right() - 220.0, y - 3.0), vec2(160.0, 6.0));
        match t.phase {
            OtaTargetPhase::EnteringBootloader | OtaTargetPhase::Erasing | OtaTargetPhase::Committing | OtaTargetPhase::Verified | OtaTargetPhase::Booting | OtaTargetPhase::Waiting => {
                widgets::indeterminate(ui, br, INFO);
            }
            _ => {
                let c = if failed { ERROR } else if done { OK } else { INFO };
                widgets::progress_bar(ui.painter(), br, if done { 1.0 } else { t.percent / 100.0 }, c);
            }
        }
        ui.painter().text(pos2(r.right() - 14.0, y), Align2::RIGHT_CENTER, format!("{:.0}%", if done { 100.0 } else { t.percent }), theme::font("value"), TEXT_SECONDARY);
        if failed {
            let reason = t.error.clone().unwrap_or_else(|| "Unknown error".into());
            let msg = failure_message(&reason);
            ui.painter().text(pos2(r.left() + 48.0, r.bottom() - 14.0), Align2::LEFT_CENTER, msg, theme::font("small"), TEXT_SECONDARY);
        }
        ui.add_space(8.0);
    }
}

fn failure_message(raw: &str) -> String {
    let l = raw.to_lowercase();
    if l.contains("verif") || l.contains("crc") {
        "Verification failed: the image arrived corrupted. The gift is safe in recovery mode. Retry.".into()
    } else if l.contains("timed out") || l.contains("timeout") || l.contains("no reply") || l.contains("ack") {
        format!("No response ({raw}). Check the gift is on and in range, then retry.")
    } else if l.contains("boot") {
        format!("Didn't start the new firmware ({raw}). It may have booted the old image.")
    } else {
        format!("{raw}. The gift stays in recovery mode; run the update again.")
    }
}

pub fn inspector(app: &mut App, ui: &mut Ui) {
    widgets::overline(ui, "Image");
    ui.add_space(6.0);
    let Some(img) = &app.firmware.image else {
        widgets::text(ui, "No image loaded", "body", TEXT_MUTED);
        ui.add_space(4.0);
        widgets::text(ui, "Choose a .lgfw file to see its details.", "small", TEXT_FAINT);
        return;
    };
    widgets::text(ui, &img.name, "title", TEXT);
    ui.add_space(8.0);
    let lw = 84.0;
    if let Some(h) = img.header {
        widgets::kv_value(ui, "Version", lw, fmt_fw(h.fw_version), "value");
        widgets::kv_value(ui, "Git", lw, h.git_hash.iter().map(|b| format!("{b:02x}")).collect::<String>(), "mono");
        widgets::kv_value(ui, "Size", lw, format!("{:.1} KiB ({} ch.)", h.image_len() as f32 / 1024.0, h.image_len().div_ceil(24)), "value");
        widgets::kv_value(ui, "HW rev", lw, format!("{}", health::hw_rev_letter(h.hw_rev as u8)), "value");
        widgets::kv_row(ui, "App CRC", lw, |ui| {
            widgets::value_text(ui, format!("0x{:08X}", h.app_crc), "mono", TEXT_SECONDARY);
            if img.crc_ok {
                widgets::text(ui, icons::CHECK, "body", OK);
            }
        });
        widgets::kv_value(ui, "Header", lw, format!("v{} · CRC 0x{:08X}", h.header_version, h.header_crc), "mono");
        ui.add_space(S6);
        widgets::overline(ui, "Compared to fleet");
        ui.add_space(6.0);
        let newer = app.snap.gifts.iter().filter(|g| g.online && g.fw_version < h.fw_version).count();
        let same = app.snap.gifts.iter().filter(|g| g.online && g.fw_version == h.fw_version).count();
        let older = app.snap.gifts.iter().filter(|g| g.online && g.fw_version > h.fw_version).count();
        let off = app.snap.gifts.iter().filter(|g| !g.online).count();
        widgets::text(ui, format!("Newer than {newer} gift{}", health::plural(newer)), "body", TEXT_SECONDARY);
        widgets::text(ui, format!("Same as {same} · Older than {older} · Offline {off}"), "small", TEXT_MUTED);
    } else {
        widgets::text(ui, img.error.clone().unwrap_or_default(), "body", ERROR);
    }
    ui.add_space(S6);
    widgets::note(ui, "Safe to interrupt: a gift that loses power stays in recovery mode; just re-run the update.", SURFACE_2, TEXT_MUTED, 10.0);
}
