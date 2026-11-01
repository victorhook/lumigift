//! Interactive demo of the Lumigift stage.
//!
//! `CARGO_TARGET_DIR=target/agent-stage cargo run -p lumigift-stage --example demo`
//!
//! Environment knobs (for automated checks):
//! * `STAGE_DEMO_GIFTS=30`: number of boxes (default 10).
//! * `STAGE_DEMO_FPS=1`: print the frame rate to stdout once per second.
//! * `STAGE_DEMO_PERF=1`: start in performance mode, `STAGE_DEMO_EDIT=1`: start in edit mode.
//! * `STAGE_DEMO_SELECT=1`: preselect the third box.
//! * `STAGE_DEMO_ARRANGE=grid|row|arc|circle|tree`.
//! * `STAGE_DEMO_OFFLINE=1`: mark one box offline.
//! * `STAGE_DEMO_LABELS=1`: labels on.
//! * `STAGE_DEMO_PATTERN=chase|pulse|sparkle|christmas|off`.
//! * `STAGE_DEMO_EXIT_AFTER=5`: quit after this many seconds.
//! * `STAGE_DEMO_CAM=25,22,0.6`: camera yaw°, pitch°, distance (m).
//! * `STAGE_DEMO_RESIZE=1`: cycle window sizes and hide the stage now and then (robustness).
//! * `STAGE_DEMO_NOVSYNC=1`: uncapped frame rate (for measuring).

use eframe::egui;
use lumigift_stage::{Arrangement, Rgb, Stage, StageGift, StageOptions};

fn main() -> eframe::Result {
    let mut options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1600.0, 1000.0])
            .with_title("Lumigift stage demo"),
        persist_window: false,
        ..Default::default()
    };
    if std::env::var("STAGE_DEMO_NOVSYNC").is_ok() {
        // Uncapped frame rate, for measuring.
        options.wgpu_options.surface.present_mode = eframe::wgpu::PresentMode::AutoNoVsync;
    }
    eframe::run_native(
        "lumigift-stage-demo",
        options,
        Box::new(|cc| Ok(Box::new(Demo::new(cc)))),
    )
}

#[derive(Clone, Copy, PartialEq)]
enum Pattern {
    HueChase,
    BeatPulse,
    Sparkle,
    Christmas,
    Off,
}

impl Pattern {
    const ALL: [Pattern; 5] = [
        Pattern::HueChase,
        Pattern::BeatPulse,
        Pattern::Sparkle,
        Pattern::Christmas,
        Pattern::Off,
    ];
    fn name(self) -> &'static str {
        match self {
            Pattern::HueChase => "Hue chase",
            Pattern::BeatPulse => "Beat pulse",
            Pattern::Sparkle => "Sparkle",
            Pattern::Christmas => "Christmas",
            Pattern::Off => "Off (unlit)",
        }
    }
}

struct Demo {
    stage: Stage,
    gifts: Vec<StageGift>,
    opts: StageOptions,
    arrangement: Arrangement,
    pattern: Pattern,
    bpm: f32,
    brightness: f32,
    start: f64,
    fps_frames: u32,
    fps_t0: f64,
    fps: f32,
    print_fps: bool,
    exit_after: Option<f64>,
    selected: Option<u64>,
    last_event: String,
}

fn env_flag(name: &str) -> bool {
    std::env::var(name).is_ok_and(|v| v != "0")
}

/// Unlit, online, unselected box number `i` (0-based).
fn demo_gift(i: usize, name: &str) -> StageGift {
    StageGift {
        id: 1000 + i as u64,
        position: [0.0, 0.0],
        yaw: 0.0,
        leds: [Rgb::BLACK; 4],
        online: true,
        selected: false,
        label: format!("#{:02}  {name}", i + 1),
    }
}

impl Demo {
    fn new(cc: &eframe::CreationContext<'_>) -> Self {
        let rs = cc
            .wgpu_render_state
            .as_ref()
            .expect("the demo needs eframe's wgpu backend");
        let stage = Stage::new(rs);
        let n: usize = std::env::var("STAGE_DEMO_GIFTS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(10)
            .clamp(1, 64);
        let names = [
            "Tree", "Desk", "Kitchen", "Hall", "Window", "Shelf", "Sofa", "Door", "Piano", "Mantel",
        ];
        let gifts: Vec<StageGift> = (0..n)
            .map(|i| StageGift {
                online: !(env_flag("STAGE_DEMO_OFFLINE") && i == n / 2 + 1),
                selected: env_flag("STAGE_DEMO_SELECT") && i == 2,
                ..demo_gift(i, names[i % names.len()])
            })
            .collect();
        let arrangement = match std::env::var("STAGE_DEMO_ARRANGE").as_deref() {
            Ok("row") => Arrangement::Row,
            Ok("grid") => Arrangement::Grid,
            Ok("circle") => Arrangement::Circle,
            Ok("tree") => Arrangement::Pyramid,
            _ => Arrangement::Arc,
        };
        let opts = StageOptions {
            performance: env_flag("STAGE_DEMO_PERF"),
            edit_layout: env_flag("STAGE_DEMO_EDIT"),
            labels: env_flag("STAGE_DEMO_LABELS"),
            ..Default::default()
        };
        let mut demo = Self {
            stage,
            gifts,
            opts,
            arrangement,
            pattern: match std::env::var("STAGE_DEMO_PATTERN").as_deref() {
                Ok("pulse") => Pattern::BeatPulse,
                Ok("sparkle") => Pattern::Sparkle,
                Ok("christmas") => Pattern::Christmas,
                Ok("off") => Pattern::Off,
                _ => Pattern::HueChase,
            },
            bpm: 120.0,
            brightness: 1.0,
            start: 0.0,
            fps_frames: 0,
            fps_t0: 0.0,
            fps: 0.0,
            print_fps: env_flag("STAGE_DEMO_FPS"),
            exit_after: std::env::var("STAGE_DEMO_EXIT_AFTER")
                .ok()
                .and_then(|v| v.parse().ok()),
            selected: None,
            last_event: String::new(),
        };
        demo.apply_arrangement();
        if let Ok(c) = std::env::var("STAGE_DEMO_CAM") {
            // "yaw_deg,pitch_deg,distance_m"
            let v: Vec<f32> = c.split(',').filter_map(|x| x.parse().ok()).collect();
            if v.len() == 3 {
                let mut cam = demo.stage.camera();
                cam.yaw = v[0].to_radians();
                cam.pitch = v[1].to_radians();
                cam.distance = v[2];
                demo.stage.set_camera(cam);
            }
        }
        if env_flag("STAGE_DEMO_SELECT") {
            demo.selected = Some(1002);
        }
        demo
    }

    fn apply_arrangement(&mut self) {
        for (id, pos, yaw) in Stage::auto_layout_with_yaw(&self.gifts, self.arrangement) {
            if let Some(g) = self.gifts.iter_mut().find(|g| g.id == id) {
                g.position = pos;
                g.yaw = yaw;
            }
        }
        self.stage.frame_all(&self.gifts);
    }

    fn animate(&mut self, t: f32) {
        let n = self.gifts.len() as f32;
        let beat = t * self.bpm / 60.0;
        let phase = beat.fract();
        let bar = (beat / 4.0).floor();
        for (i, g) in self.gifts.iter_mut().enumerate() {
            let fi = i as f32;
            let leds: [[f32; 3]; 4] = match self.pattern {
                Pattern::HueChase => std::array::from_fn(|k| {
                    let h = fi / n - t * 0.12 + k as f32 * 0.035;
                    hsv(h, 1.0, 1.0)
                }),
                Pattern::BeatPulse => {
                    // Kick on every beat, accent on the downbeat, a hue per bar.
                    let env = (-phase * 5.0).exp();
                    let down = if (beat.floor() as i32) % 4 == 0 { 1.0 } else { 0.55 };
                    let h = bar * 0.17 + fi / n * 0.25;
                    let wave = 0.5 + 0.5 * ((fi / n) * std::f32::consts::TAU - beat * std::f32::consts::PI).sin();
                    std::array::from_fn(|k| {
                        let v = (0.12 + 0.88 * env * down) * (0.6 + 0.4 * wave);
                        hsv(h + k as f32 * 0.02, 0.95, v)
                    })
                }
                Pattern::Sparkle => std::array::from_fn(|k| {
                    let seed = (i * 4 + k) as f32;
                    let r = hash((seed * 17.0 + (t * 6.0).floor()) * 0.731);
                    let v = if r > 0.82 { 1.0 } else { 0.06 };
                    let h = 0.58 + 0.1 * hash(seed * 3.1);
                    hsv(h, 0.35 + 0.5 * hash(seed), v)
                }),
                Pattern::Christmas => std::array::from_fn(|k| {
                    let red = ((i + k / 2) + (beat * 0.5) as usize).is_multiple_of(2);
                    let tw = 0.75 + 0.25 * (t * 3.0 + fi * 1.7 + k as f32).sin();
                    if red {
                        [1.0 * tw, 0.05 * tw, 0.02 * tw]
                    } else {
                        [0.05 * tw, 0.85 * tw, 0.15 * tw]
                    }
                }),
                Pattern::Off => [[0.0; 3]; 4],
            };
            for (dst, c) in g.leds.iter_mut().zip(leds) {
                let b = self.brightness;
                *dst = Rgb::new(
                    (c[0] * b * 255.0).clamp(0.0, 255.0) as u8,
                    (c[1] * b * 255.0).clamp(0.0, 255.0) as u8,
                    (c[2] * b * 255.0).clamp(0.0, 255.0) as u8,
                );
            }
        }
    }

    fn side_panel(&mut self, ui: &mut egui::Ui) {
        ui.heading("Lumigift stage");
        ui.label(format!("{:.0} fps · {} boxes", self.fps, self.gifts.len()));
        ui.separator();

        ui.label("Pattern");
        for p in Pattern::ALL {
            ui.radio_value(&mut self.pattern, p, p.name());
        }
        ui.add(egui::Slider::new(&mut self.bpm, 60.0..=180.0).text("BPM"));
        ui.add(egui::Slider::new(&mut self.brightness, 0.0..=1.0).text("Brightness"));
        ui.separator();

        ui.label("Arrangement");
        ui.horizontal_wrapped(|ui| {
            for a in Arrangement::ALL {
                if ui
                    .selectable_label(self.arrangement == a, a.name())
                    .clicked()
                {
                    self.arrangement = a;
                    self.apply_arrangement();
                }
            }
        });
        let mut n = self.gifts.len();
        if ui
            .add(egui::Slider::new(&mut n, 1..=30).text("Boxes"))
            .changed()
        {
            self.resize(n);
        }
        ui.separator();

        ui.label("Stage options");
        ui.checkbox(&mut self.opts.labels, "Labels");
        ui.checkbox(&mut self.opts.edit_layout, "Edit layout (drag boxes)");
        ui.checkbox(&mut self.opts.show_floor_grid, "Floor grid");
        ui.checkbox(&mut self.opts.performance, "Performance (auto-orbit)");
        ui.add(egui::Slider::new(&mut self.opts.bloom, 0.0..=3.0).text("Bloom"));
        ui.add(egui::Slider::new(&mut self.opts.exposure, 0.1..=4.0).logarithmic(true).text("Exposure"));
        ui.separator();

        ui.horizontal(|ui| {
            if ui.button("Frame all").clicked() {
                self.stage.frame_all(&self.gifts);
            }
            if ui.button("Front").clicked() {
                self.stage.set_preset(lumigift_stage::CameraPreset::Front);
            }
            if ui.button("Top").clicked() {
                self.stage.set_preset(lumigift_stage::CameraPreset::Top);
            }
            if ui.button("¾").clicked() {
                self.stage.set_preset(lumigift_stage::CameraPreset::ThreeQuarter);
            }
        });
        if let Some(id) = self.selected
            && let Some(g) = self.gifts.iter_mut().find(|g| g.id == id)
        {
            ui.separator();
            ui.label(format!("Selected: {}", g.label));
            ui.checkbox(&mut g.online, "Online");
            ui.add(egui::Slider::new(&mut g.yaw, -std::f32::consts::PI..=std::f32::consts::PI).text("Yaw"));
        }
        ui.separator();
        ui.weak("LMB orbit · RMB/MMB or Shift+LMB pan · wheel zoom · double-click frame/focus");
        if !self.last_event.is_empty() {
            ui.weak(&self.last_event);
        }
        let cam = self.stage.camera();
        ui.weak(format!(
            "cam yaw {:.0}° pitch {:.0}° dist {:.2} m",
            cam.yaw.to_degrees(),
            cam.pitch.to_degrees(),
            cam.distance
        ));
    }

    fn resize(&mut self, n: usize) {
        while self.gifts.len() < n {
            self.gifts.push(demo_gift(self.gifts.len(), "Box"));
        }
        self.gifts.truncate(n);
        self.apply_arrangement();
    }
}

impl eframe::App for Demo {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let now = ui.input(|i| i.time);
        if self.start == 0.0 {
            self.start = now;
            self.fps_t0 = now;
        }
        self.fps_frames += 1;
        if now - self.fps_t0 >= 1.0 {
            self.fps = self.fps_frames as f32 / (now - self.fps_t0) as f32;
            if self.print_fps {
                println!("fps {:.1}", self.fps);
            }
            self.fps_frames = 0;
            self.fps_t0 = now;
        }
        if let Some(s) = self.exit_after
            && now - self.start > s
        {
            ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
        }
        if env_flag("STAGE_DEMO_RESIZE") {
            // Robustness check: cycle window sizes (incl. tiny) and hide the stage now and then.
            let sizes = [[1600.0, 1000.0], [420.0, 120.0], [300.0, 40.0], [1000.0, 700.0], [700.0, 1000.0]];
            let k = ((now - self.start) * 3.0) as usize;
            if self.fps_frames == 1 || k != ((now - self.start - 0.02) * 3.0).max(0.0) as usize {
                let sz = sizes[k % sizes.len()];
                ui.ctx()
                    .send_viewport_cmd(egui::ViewportCommand::InnerSize(egui::vec2(sz[0], sz[1])));
            }
            if k % 7 == 3 {
                ui.ctx().request_repaint();
                return;
            }
        }
        self.animate((now - self.start) as f32);
        for g in &mut self.gifts {
            g.selected = Some(g.id) == self.selected;
        }

        if !self.opts.performance {
            egui::Panel::right("controls")
                .default_size(280.0)
                .show(ui, |ui| {
                    egui::ScrollArea::vertical().show(ui, |ui| self.side_panel(ui));
                });
        }
        egui::CentralPanel::default()
            .frame(egui::Frame::new().fill(lumigift_stage::theme::STAGE_BG))
            .show(ui, |ui| {
                let r = self.stage.show(ui, &self.gifts, &self.opts);
                if let Some(id) = r.clicked {
                    self.selected = if self.selected == Some(id) { None } else { Some(id) };
                    self.last_event = format!("clicked {id}");
                } else if r.response.clicked() {
                    self.selected = None;
                }
                for (id, pos) in r.moved {
                    if let Some(g) = self.gifts.iter_mut().find(|g| g.id == id) {
                        g.position = pos;
                    }
                    self.last_event = format!("moved {id} to ({:.2}, {:.2}) m", pos[0], pos[1]);
                }
            });
        if ui.input(|i| i.key_pressed(egui::Key::Escape)) && self.opts.performance {
            self.opts.performance = false;
        }
        if ui.input(|i| i.key_pressed(egui::Key::P)) {
            self.opts.performance = !self.opts.performance;
        }
        // The lights animate continuously.
        ui.ctx().request_repaint();
    }

    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        lumigift_stage::theme::STAGE_BG.to_normalized_gamma_f32()
    }
}

fn hsv(h: f32, s: f32, v: f32) -> [f32; 3] {
    let h = h.rem_euclid(1.0) * 6.0;
    let i = h.floor() as i32;
    let f = h - i as f32;
    let p = v * (1.0 - s);
    let q = v * (1.0 - s * f);
    let t = v * (1.0 - s * (1.0 - f));
    match i {
        0 => [v, t, p],
        1 => [q, v, p],
        2 => [p, v, t],
        3 => [p, q, v],
        4 => [t, p, v],
        _ => [v, p, q],
    }
}

fn hash(x: f32) -> f32 {
    ((x * 12.9898).sin() * 43758.547).fract().abs()
}
