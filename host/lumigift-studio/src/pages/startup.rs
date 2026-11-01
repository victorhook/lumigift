//! Empty states shown before a backend exists (docs/UI_DESIGN.md §6.4).

use egui::Ui;

use crate::app::App;
use crate::icons;
use crate::theme::*;
use crate::widgets::{self, EmptyAction, EmptyState};

pub fn no_bridge(app: &mut App, ui: &mut Ui) {
    let n = app.persist.sim_count;
    let label = format!("Start simulator ({n} gifts)");
    let mut troubleshoot = false;
    let act = widgets::empty_state(
        ui,
        &EmptyState {
            headline: "Connect your Lumigift bridge",
            body: "Plug the USB radio bridge into this computer. No hardware? Explore with virtual gifts.",
            primary: Some((&label, Some(icons::GIFT))),
            secondary: Some("Retry"),
            big: true,
        },
        |ui| {
            ui.add_space(S5);
            ui.horizontal(|ui| {
                // Centre the gift-count stepper under the CTA.
                let w = 220.0;
                ui.add_space(((ui.available_width() - w) / 2.0).max(0.0));
                widgets::text(ui, "Virtual gifts", "small", TEXT_FAINT);
                ui.add(egui::Slider::new(&mut app.persist.sim_count, 1..=30).show_value(false));
                widgets::text(ui, format!("{}", app.persist.sim_count), "value", TEXT_SECONDARY);
            });
            ui.add_space(S6);
            ui.horizontal(|ui| {
                ui.add_space(((ui.available_width() - 150.0) / 2.0).max(0.0));
                let r = ui.add(egui::Label::new(widgets::rich(
                    format!("{}  Troubleshooting", icons::CARET_RIGHT),
                    "small_medium",
                    TEXT_MUTED,
                )).sense(egui::Sense::click()));
                if r.clicked() {
                    troubleshoot = true;
                }
            });
            let id = egui::Id::unique("troubleshoot-open");
            let mut open = ui.data_mut(|d| *d.get_temp_mut_or(id, false));
            if troubleshoot {
                open = !open;
                ui.data_mut(|d| d.insert_temp(id, open));
            }
            if open {
                ui.add_space(S3);
                widgets::card_ui(ui, |ui| {
                    ui.set_width(400.0);
                    widgets::text(ui, "Linux: the bridge needs a udev rule so your user can open it.", "small", TEXT_MUTED);
                    ui.add_space(4.0);
                    let rule = "SUBSYSTEM==\"usb\", ATTR{idVendor}==\"1209\", ATTR{idProduct}==\"0001\", MODE=\"0660\", TAG+=\"uaccess\"";
                    egui::Frame::new().fill(BG_SUNKEN).corner_radius(egui::CornerRadius::same(R_SM)).inner_margin(8).show(ui, |ui| {
                        ui.add(egui::Label::new(widgets::rich(rule, "mono", TEXT_SECONDARY)).wrap().selectable(true));
                    });
                    ui.add_space(4.0);
                    if widgets::secondary_button(ui, Some(icons::COPY), "Copy udev rule", true).clicked() {
                        ui.ctx().copy_text(rule.to_owned());
                    }
                });
            }
        },
    );
    match act {
        EmptyAction::Primary => app.start_sim(n),
        EmptyAction::Secondary => {
            if crate::backend::Detector::probe_now() {
                app.start_usb();
            } else {
                app.toasts.info("Still no bridge found. Check the USB cable.");
            }
        }
        EmptyAction::None => {}
    }
}
