//! Hidden screenshot automation (`LUMIGIFT_SCREENSHOT=<dir>` or `--screenshot <dir>`):
//! cycles through the pages, saves one PNG each via `ViewportCommand::Screenshot`
//! and closes the app. Used to review the UI without a human at the keyboard.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::app::{App, Page};

#[derive(Clone, Copy, PartialEq)]
enum Phase {
    Setup,
    Waiting,
    Capturing,
}

pub struct Screenshotter {
    dir: PathBuf,
    step: usize,
    phase: Phase,
    since: Instant,
    done: Arc<Mutex<bool>>,
    started: Instant,
    prepped: bool,
    /// `STEPS`, or the comma-separated subset in `LUMIGIFT_SCREENSHOT_STEPS`.
    steps: Vec<&'static str>,
}

const STEPS: &[&str] = &["fleet", "fleet-list", "studio", "lighting", "lighting-groups", "stage", "stage-arrange", "firmware", "firmware-run", "settings", "palette", "fleet-compact", "perform"];

impl Screenshotter {
    pub fn new(dir: PathBuf) -> Self {
        let _ = std::fs::create_dir_all(&dir);
        let only = std::env::var("LUMIGIFT_SCREENSHOT_STEPS").unwrap_or_default();
        let steps = STEPS.iter().copied().filter(|s| only.is_empty() || only.split(',').any(|o| o.trim() == *s)).collect();
        Self { dir, step: 0, phase: Phase::Setup, since: Instant::now(), done: Arc::new(Mutex::new(false)), started: Instant::now(), prepped: false, steps }
    }

    pub fn step(&mut self, app: &mut App, ctx: &egui::Context) {
        ctx.request_repaint();
        let Some(name) = self.steps.get(self.step).copied() else {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            return;
        };
        if !self.prepped && self.started.elapsed() > Duration::from_secs(3) && app.connected() && app.studio.has_track() {
            // Silent playback + LIVE so the simulated gifts light up.
            self.prepped = true;
            app.auto_assign();
            if let Some(p) = &app.studio.player {
                p.set_volume(0.0);
            }
            app.studio.toggle_play();
            app.studio.seek(26.0);
            app.toggle_live();
        }
        match self.phase {
            Phase::Setup => {
                // Give the simulator time to discover gifts and fill sparklines.
                if self.started.elapsed() < Duration::from_secs(10) {
                    return;
                }
                setup(app, ctx, name);
                self.phase = Phase::Waiting;
                self.since = Instant::now();
            }
            Phase::Waiting => {
                let ready = match name {
                    "studio" => app.studio.has_track() && !app.studio.is_loading(),
                    _ => true,
                };
                let wait = match name { "studio" => 2.5, "firmware-run" => 3.0, "lighting" | "lighting-groups" => 2.0, _ => 1.2 };
                if ready && self.since.elapsed() > Duration::from_secs_f32(wait) {
                    if name == "studio" && app.studio.sel.is_empty() {
                        // Select a cue so the inspector shows content.
                        let n = app.studio.show().map(|s| s.cues.len()).unwrap_or(0);
                        if n > 2 {
                            app.studio.sel = [2].into();
                        }
                    }
                    let path = self.dir.join(format!("{:02}-{name}.png", self.step + 1));
                    let done = self.done.clone();
                    *done.lock().unwrap() = false;
                    ctx.request_screenshot(move |img| {
                        save_png(&path, &img);
                        *done.lock().unwrap() = true;
                    });
                    self.phase = Phase::Capturing;
                    self.since = Instant::now();
                } else if !ready && self.since.elapsed() > Duration::from_secs(30) {
                    self.step += 1;
                    self.phase = Phase::Setup;
                }
            }
            Phase::Capturing => {
                if *self.done.lock().unwrap() || self.since.elapsed() > Duration::from_secs(5) {
                    self.step += 1;
                    self.phase = Phase::Setup;
                }
            }
        }
    }
}

fn setup(app: &mut App, ctx: &egui::Context, name: &str) {
    match name {
        "fleet" => {
            app.set_page(Page::Fleet);
            if let Some(g) = app.snap.gifts.get(2) {
                app.select(g.uid);
            }
        }
        "fleet-list" => app.persist.fleet_list_view = true,
        "studio" => {
            app.persist.fleet_list_view = false;
            app.set_page(Page::Studio);
        }
        "lighting" => {
            use crate::pages::lighting;
            app.selection.clear();
            if app.snap.gifts.iter().any(|g| g.slot.is_none()) {
                app.auto_assign();
            }
            app.set_page(Page::Lighting);
            app.persist.lighting.look = lighting::Look { palette: 0, ..lighting::Look::default() };
            lighting::apply_effect(app, lumigift_fx::effect::TWINKLE);
        }
        "lighting-groups" => {
            use crate::pages::lighting;
            // A group of the first four gifts (by slot) runs Knight Rider in red.
            let mut gifts: Vec<_> = app.snap.gifts.iter().filter(|g| g.slot.is_some()).map(|g| (g.slot, g.uid)).collect();
            gifts.sort();
            app.selection = gifts.iter().take(4).map(|g| g.1).collect();
            app.persist.lighting.groups = vec![lighting::Group { name: "Window".into(), uids: app.selection.iter().map(|u| u.to_hex()).collect() }];
            app.lighting.target = lighting::TargetSel::Group("Window".into());
            app.persist.lighting.look = lighting::Look { palette: lumigift_proto::PALETTE_CUSTOM, colors: [[255, 24, 12], [90, 0, 0]], ..lighting::Look::default() };
            lighting::apply_effect(app, lumigift_fx::effect::SCANNER);
            app.selection.clear();
        }
        "stage" => {
            crate::pages::lighting::release(app);
            app.selection.clear();
            app.set_page(Page::Stage);
        }
        "stage-arrange" => {
            app.stage_page.arrange = true;
            app.stage.set_preset(crate::stage::CameraPreset::Top);
        }
        "firmware" => {
            app.stage_page.arrange = false;
            app.set_page(Page::Firmware);
        }
        "firmware-run" => {
            app.live = false;
            crate::pages::firmware::start_all(app);
        }
        "settings" => {
            app.set_page(Page::Settings);
            app.settings.section = 3;
        }
        "palette" => {
            app.set_page(Page::Fleet);
            app.palette.open();
        }
        "perform" => {
            app.set_page(Page::Studio);
            app.set_performance(ctx, true);
        }
        "fleet-compact" => {
            app.palette.open();
            ctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(egui::vec2(1100.0, 720.0)));
        }
        _ => {}
    }
}

fn save_png(path: &std::path::Path, img: &egui::ColorImage) {
    let Ok(f) = std::fs::File::create(path) else { return };
    let w = std::io::BufWriter::new(f);
    let mut enc = png::Encoder::new(w, img.size[0] as u32, img.size[1] as u32);
    enc.set_color(png::ColorType::Rgba);
    enc.set_depth(png::BitDepth::Eight);
    if let Ok(mut wr) = enc.write_header() {
        let bytes: Vec<u8> = img.pixels.iter().flat_map(|c| c.to_array()).collect();
        let _ = wr.write_image_data(&bytes);
    }
}
