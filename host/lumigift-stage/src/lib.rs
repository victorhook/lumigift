//! Real-time 3D "stage" of glowing Lumigift boxes for egui (eframe + wgpu).
//!
//! The stage renders each gift as a frosted white box lit from inside by its
//! four LEDs, standing on a dark glossy floor with soft coloured light pools,
//! mirrored reflections, bloom and filmic tonemapping. It is a normal egui
//! widget: [`Stage::show`] fills the available rect and returns hover, click
//! and drag results.
//!
//! ```no_run
//! # use lumigift_stage::*;
//! # fn demo(cc: &eframe::CreationContext<'_>, ui: &mut egui::Ui, gifts: &[StageGift]) {
//! // Once, in the eframe creation closure:
//! let mut stage = Stage::new(cc.wgpu_render_state.as_ref().expect("wgpu backend"));
//! // Every frame:
//! let opts = StageOptions::default();
//! let r = stage.show(ui, gifts, &opts);
//! if let Some(id) = r.clicked { /* select gift `id` */ }
//! # }
//! ```
//!
//! See the crate README for the render pipeline.

mod camera;
mod layout;
mod mesh;
mod render;

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use eframe::egui_wgpu;
use egui::{Color32, Pos2, Rect, Sense, Vec2};
use glam::{Vec3, Vec4Swizzles};

pub use camera::{CameraState, DIST_MAX, DIST_MIN, FOV_Y};
pub use layout::{Arrangement, SPACING};
pub use lumigift_proto::{LEDS_PER_GIFT, Rgb};
pub use render::TargetInfo;

use render::{FrameData, GpuGift, Globals, MAX_POOLS, PostUniform, StageCallback, StageRenderer};

/// Physical body width of a gift box in metres. The box is modelled in units
/// of this size (body 1.00 × 0.86 × 1.00, lid 1.06 wide).
pub const BOX_SIZE: f32 = 0.06;

/// Grid that box drags snap to, metres.
pub const SNAP: f32 = 0.05;

/// LED colour → HDR scale (saturated LEDs land in the bloom range).
const LED_INTENSITY: f32 = 6.0;

/// LED display smoothing: time constant of each of the two low-pass stages.
const LED_SMOOTH_TAU: f32 = 0.1;
/// A channel that moves more than this many levels in one frame is a real
/// change (strobe, blink, onset) and snaps instead of being smoothed.
const LED_SNAP_LEVELS: i16 = 4;

/// Design tokens used by the stage (`docs/UI_DESIGN.md` §2.1).
pub mod theme {
    use egui::Color32;
    pub const STAGE_BG: Color32 = Color32::from_rgb(0x05, 0x05, 0x07);
    pub const ACCENT: Color32 = Color32::from_rgb(0xBB, 0x78, 0xFE);
    pub const SURFACE_3: Color32 = Color32::from_rgb(0x22, 0x22, 0x26);
    pub const BORDER: Color32 = Color32::from_rgb(0x32, 0x32, 0x37);
    pub const TEXT: Color32 = Color32::from_rgb(0xF0, 0xF0, 0xF3);
    pub const TEXT_MUTED: Color32 = Color32::from_rgb(0xA7, 0xA7, 0xAF);
    pub const TEXT_FAINT: Color32 = Color32::from_rgb(0x88, 0x88, 0x90);
    pub const ACCENT_TEXT: Color32 = Color32::from_rgb(0xD3, 0xAD, 0xFE);
}
pub(crate) use theme::STAGE_BG;

/// One gift box on the stage.
#[derive(Clone, Debug, PartialEq)]
pub struct StageGift {
    /// Stable key (e.g. derived from the UID). Returned in [`StageResponse`].
    pub id: u64,
    /// Centre of the box on the floor plane, metres: `[x, z]`.
    pub position: [f32; 2],
    /// Rotation around the vertical axis, radians.
    pub yaw: f32,
    /// Current LED colours as sent to the LEDs (pre-gamma sRGB, 0–255),
    /// in LED order 0:(−x,−z) 1:(+x,−z) 2:(+x,+z) 3:(−x,+z) in box space.
    pub leds: [Rgb; LEDS_PER_GIFT],
    /// Offline boxes render unlit, desaturated and dimmed.
    pub online: bool,
    /// Draws the accent rim and floor ring.
    pub selected: bool,
    /// Shown on hover, on selection, or always when [`StageOptions::labels`] is on.
    pub label: String,
}

/// Display options for [`Stage::show`].
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct StageOptions {
    /// Show labels above every box (hovered and selected boxes always get one).
    pub labels: bool,
    /// Arrange mode: drag boxes across the floor. Drags snap to [`SNAP`] (5 cm);
    /// hold **Shift** while dragging to place freely. The grid is drawn at 2×.
    pub edit_layout: bool,
    /// Bloom strength multiplier, 0 = off, 1 = default look, up to ~3.
    pub bloom: f32,
    /// Linear exposure multiplier applied before tonemapping (1 = default).
    pub exposure: f32,
    /// Subtle floor grid (10 cm minor / 50 cm major lines).
    pub show_floor_grid: bool,
    /// Presentation mode: hides labels, hints and highlights, and the camera
    /// orbits slowly by itself (2°/s).
    pub performance: bool,
}

impl Default for StageOptions {
    fn default() -> Self {
        Self {
            labels: false,
            edit_layout: false,
            bloom: 1.0,
            exposure: 1.0,
            show_floor_grid: true,
            performance: false,
        }
    }
}

/// Result of [`Stage::show`].
#[derive(Debug)]
pub struct StageResponse {
    /// Gift under the pointer.
    pub hovered: Option<u64>,
    /// Gift clicked this frame (primary button, no drag).
    pub clicked: Option<u64>,
    /// New floor positions `(id, [x, z])` from dragging in edit mode. The app
    /// should apply them to its layout; the dragged box renders at the drag
    /// position meanwhile.
    pub moved: Vec<(u64, [f32; 2])>,
    /// The widget's egui response (whole stage rect).
    pub response: egui::Response,
}

/// Camera presets for [`Stage::set_preset`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum CameraPreset {
    /// Straight on from +z, low angle.
    Front,
    /// Looking down (pitch 85°), handy for arranging.
    Top,
    /// The default three-quarter view.
    ThreeQuarter,
}

struct BoxDrag {
    id: u64,
    /// Box position minus the floor hit point at drag start.
    offset: glam::Vec2,
    pos: [f32; 2],
}

static NEXT_STAGE_ID: AtomicU64 = AtomicU64::new(1);

/// The 3D stage widget. Holds camera and interaction state; the GPU resources
/// live in egui_wgpu's callback resources.
pub struct Stage {
    id: u64,
    alive: Arc<AtomicBool>,
    camera: camera::OrbitCamera,
    needs_frame: bool,
    /// Last frame_all fit (centre radius, aspect it was computed for); kept
    /// up to date on resize until the user moves the camera.
    fit: Option<(f32, f32)>,
    drag: Option<BoxDrag>,
    last_aspect: f32,
    /// Per-gift LED display smoothing (see [`LedSmooth`]).
    leds: HashMap<u64, LedSmooth>,
}

/// Hides 8-bit quantisation in the 3D view. Dim, slow looks (an idle
/// rainbow peaks around 33/255 after brightness limits) step one level every
/// few hundred ms, which the HDR mapping and bloom make look like stutter.
/// Small changes go through a two-stage low-pass; jumps snap.
struct LedSmooth {
    prev: [Rgb; LEDS_PER_GIFT],
    /// Two cascaded one-pole stages, in LED levels (0–255, fractional).
    s1: [[f32; 3]; LEDS_PER_GIFT],
    s2: [[f32; 3]; LEDS_PER_GIFT],
}

impl LedSmooth {
    fn new(leds: [Rgb; LEDS_PER_GIFT]) -> Self {
        let v = leds.map(|c| [c.r as f32, c.g as f32, c.b as f32]);
        Self { prev: leds, s1: v, s2: v }
    }

    fn update(&mut self, leds: [Rgb; LEDS_PER_GIFT], dt: f32) {
        let a = 1.0 - (-dt / LED_SMOOTH_TAU).exp();
        for (((c, p), s1), s2) in leds.iter().zip(&self.prev).zip(&mut self.s1).zip(&mut self.s2) {
            let x = [c.r as f32, c.g as f32, c.b as f32];
            let jump = [(c.r, p.r), (c.g, p.g), (c.b, p.b)]
                .iter()
                .any(|&(n, o)| (n as i16 - o as i16).abs() > LED_SNAP_LEVELS);
            if jump {
                *s1 = x;
                *s2 = x;
            } else {
                for ((x, s1), s2) in x.iter().zip(s1.iter_mut()).zip(s2.iter_mut()) {
                    *s1 += (x - *s1) * a;
                    *s2 += (*s1 - *s2) * a;
                }
            }
        }
        self.prev = leds;
    }

    /// Still gliding towards the latest colours.
    fn settling(&self) -> bool {
        self.s2.iter().zip(self.prev).any(|(s, c)| {
            (s[0] - c.r as f32).abs() > 0.05
                || (s[1] - c.g as f32).abs() > 0.05
                || (s[2] - c.b as f32).abs() > 0.05
        })
    }
}

impl Drop for Stage {
    fn drop(&mut self) {
        // Lets the renderer free this stage's offscreen targets.
        self.alive.store(false, Ordering::Relaxed);
    }
}

impl Stage {
    /// Creates a stage, registering the shared GPU pipelines in the
    /// renderer's callback resources (once per app). Assumes egui renders
    /// without MSAA and without a depth buffer (eframe's defaults); use
    /// [`Stage::with_target`] otherwise.
    pub fn new(render_state: &egui_wgpu::RenderState) -> Self {
        Self::with_target(
            render_state,
            TargetInfo {
                msaa_samples: 1,
                depth_format: None,
            },
        )
    }

    /// Like [`Stage::new`], for apps that set `NativeOptions::multisampling`
    /// or `depth_buffer`: pass the same sample count and depth format.
    pub fn with_target(render_state: &egui_wgpu::RenderState, target: TargetInfo) -> Self {
        let mut renderer = render_state.renderer.write();
        if renderer.callback_resources.get::<StageRenderer>().is_none() {
            let r = StageRenderer::new(
                &render_state.device,
                &render_state.adapter,
                render_state.target_format,
                target,
            );
            renderer.callback_resources.insert(r);
        }
        Self::without_gpu()
    }

    /// Stage state without registering GPU resources (paint callbacks are
    /// ignored until some `Stage::new` registered them). Used by tests.
    fn without_gpu() -> Self {
        let mut camera = camera::OrbitCamera::default();
        camera.snap();
        Self {
            id: NEXT_STAGE_ID.fetch_add(1, Ordering::Relaxed),
            alive: Arc::new(AtomicBool::new(true)),
            camera,
            needs_frame: true,
            fit: None,
            drag: None,
            last_aspect: 1.6,
            leds: HashMap::new(),
        }
    }

    /// The camera pose to persist (the goal pose, not the in-flight one).
    pub fn camera(&self) -> CameraState {
        self.camera.goal
    }

    /// Restores a persisted camera pose (jumps there without animating).
    pub fn set_camera(&mut self, state: CameraState) {
        self.camera.goal = state;
        self.camera.snap();
        self.needs_frame = false;
        self.fit = None;
    }

    /// Animates to a preset view angle, keeping the current target and distance.
    pub fn set_preset(&mut self, preset: CameraPreset) {
        let (yaw, pitch) = match preset {
            CameraPreset::Front => (0.0, 12f32.to_radians()),
            CameraPreset::Top => (0.0, 85f32.to_radians()),
            CameraPreset::ThreeQuarter => (25f32.to_radians(), 22f32.to_radians()),
        };
        // Take the short way round.
        let cur = self.camera.goal.yaw;
        let tau = std::f32::consts::TAU;
        let delta = (yaw - cur).rem_euclid(tau);
        let delta = if delta > tau * 0.5 { delta - tau } else { delta };
        self.camera.goal.yaw = cur + delta;
        self.camera.goal.pitch = pitch;
        self.needs_frame = false;
    }

    /// Points the camera so all boxes fit in view (animated).
    pub fn frame_all(&mut self, gifts: &[StageGift]) {
        self.needs_frame = false;
        let Some((lo, hi)) = bounds(gifts) else {
            self.camera.goal.target = [0.0, 0.45 * BOX_SIZE, 0.0];
            self.camera.goal.distance = 0.8;
            return;
        };
        let c = (lo + hi) * 0.5;
        let r = gifts
            .iter()
            .map(|g| glam::Vec2::from(g.position).distance(c))
            .fold(0.0f32, f32::max)
            + BOX_SIZE * 0.75;
        let r = r.max(0.08);
        self.camera.goal.target = [c.x, 0.45 * BOX_SIZE, c.y];
        self.camera.goal.distance = fit_distance(r, self.last_aspect);
        self.fit = Some((r, self.last_aspect));
    }

    /// Computes positions for a preset arrangement, in the order of `gifts`.
    /// Returns `(id, [x, z])`; for orientation, see [`Stage::auto_layout_with_yaw`].
    pub fn auto_layout(gifts: &[StageGift], arrangement: Arrangement) -> Vec<(u64, [f32; 2])> {
        Self::auto_layout_with_yaw(gifts, arrangement)
            .into_iter()
            .map(|(id, p, _)| (id, p))
            .collect()
    }

    /// Like [`Stage::auto_layout`], also returning a suggested yaw per box
    /// (Arc boxes face the arc's centre, Circle boxes face outwards).
    pub fn auto_layout_with_yaw(
        gifts: &[StageGift],
        arrangement: Arrangement,
    ) -> Vec<(u64, [f32; 2], f32)> {
        layout::positions(gifts.len(), arrangement)
            .into_iter()
            .zip(gifts)
            .map(|((p, yaw), g)| (g.id, p, yaw))
            .collect()
    }

    /// Shows the stage in all of `ui`'s available space.
    pub fn show(&mut self, ui: &mut egui::Ui, gifts: &[StageGift], opts: &StageOptions) -> StageResponse {
        let size = ui.available_size().max(Vec2::ZERO);
        let (rect, response) = ui.allocate_exact_size(size, Sense::click_and_drag());
        let mut out = StageResponse {
            hovered: None,
            clicked: None,
            moved: Vec::new(),
            response,
        };
        if rect.width() < 1.0 || rect.height() < 1.0 || !ui.is_rect_visible(rect) {
            return out;
        }
        let response = out.response.clone();
        self.last_aspect = rect.width() / rect.height();
        if let Some((r, aspect)) = &mut self.fit
            && (*aspect / self.last_aspect - 1.0).abs() > 0.01
        {
            *aspect = self.last_aspect;
            self.camera.goal.distance = fit_distance(*r, self.last_aspect);
        }
        if self.needs_frame && !gifts.is_empty() {
            self.frame_all(gifts);
            self.camera.snap();
        }

        let (dt, time, shift) = ui.input(|i| (i.stable_dt.min(0.1), i.time, i.modifiers.shift));

        // Drop a drag whose gift disappeared.
        if let Some(d) = &self.drag
            && !gifts.iter().any(|g| g.id == d.id)
        {
            self.drag = None;
        }

        // --- Picking --------------------------------------------------------
        let cam_now = self.camera.current;
        let inv_vp = self.view_proj().inverse();
        let ray_at = |p: Pos2| -> Ray {
            let ndc_x = (p.x - rect.min.x) / rect.width() * 2.0 - 1.0;
            let ndc_y = 1.0 - (p.y - rect.min.y) / rect.height() * 2.0;
            let a = inv_vp * glam::Vec4::new(ndc_x, ndc_y, 0.0, 1.0);
            let b = inv_vp * glam::Vec4::new(ndc_x, ndc_y, 1.0, 1.0);
            let a = a.xyz() / a.w;
            let b = b.xyz() / b.w;
            (a, (b - a).normalize())
        };
        let pointer = response.hover_pos().or(if response.dragged() {
            response.interact_pointer_pos()
        } else {
            None
        });
        let hovered = response.hover_pos().and_then(|p| self.pick(gifts, ray_at(p)));
        out.hovered = hovered;

        // --- Input ----------------------------------------------------------
        let mut cam_input = false;
        if response.drag_started_by(egui::PointerButton::Primary)
            && opts.edit_layout
            && !opts.performance
            && let Some(p) = response.interact_pointer_pos()
        {
            let ray = ray_at(p);
            if let Some(id) = self.pick(gifts, ray)
                && let Some(hit) = ray_floor(ray)
                && let Some(g) = gifts.iter().find(|g| g.id == id)
            {
                self.drag = Some(BoxDrag {
                    id,
                    offset: glam::Vec2::from(g.position) - hit,
                    pos: g.position,
                });
            }
        }

        if let Some(drag) = &mut self.drag {
            if response.dragged_by(egui::PointerButton::Primary) {
                if let Some(p) = pointer
                    && let Some(hit) = ray_floor(ray_at(p))
                {
                    let mut np = hit + drag.offset;
                    if !shift {
                        np = (np / SNAP).round() * SNAP;
                    }
                    let np = [np.x, np.y];
                    if np != drag.pos {
                        drag.pos = np;
                        out.moved.push((drag.id, np));
                    }
                }
            } else {
                self.drag = None;
            }
        } else {
            let pan = response.dragged_by(egui::PointerButton::Secondary)
                || response.dragged_by(egui::PointerButton::Middle)
                || (response.dragged_by(egui::PointerButton::Primary) && shift);
            let delta = response.drag_delta();
            if pan && delta != Vec2::ZERO {
                let (right, up) = cam_now.basis();
                let m_per_pt = 2.0 * cam_now.distance * (FOV_Y * 0.5).tan() / rect.height();
                let t = Vec3::from(self.camera.goal.target)
                    - right * delta.x * m_per_pt
                    + up * delta.y * m_per_pt;
                self.camera.goal.target = t.to_array();
                cam_input = true;
            } else if response.dragged_by(egui::PointerButton::Primary) && delta != Vec2::ZERO {
                let k = 0.3f32.to_radians();
                self.camera.goal.yaw -= delta.x * k;
                self.camera.goal.pitch += delta.y * k;
                cam_input = true;
            }
        }

        if response.hovered() {
            let (scroll, zoom) = ui.input(|i| (i.smooth_scroll_delta.y, i.zoom_delta()));
            let factor = (-scroll * 0.0015).exp() / zoom.max(1e-3);
            if (factor - 1.0).abs() > 1e-5 {
                let old = self.camera.goal.distance;
                let new = (old * factor).clamp(DIST_MIN, DIST_MAX);
                // Dolly towards the floor point under the pointer.
                if new < old
                    && let Some(p) = response.hover_pos()
                    && let Some(hit) = ray_floor(ray_at(p))
                {
                    let t = Vec3::from(self.camera.goal.target);
                    let goal = Vec3::new(hit.x, t.y, hit.y);
                    let k = (1.0 - new / old) * 0.6;
                    self.camera.goal.target = t.lerp(goal, k).to_array();
                }
                self.camera.goal.distance = new;
                cam_input = true;
                ui.input_mut(|i| i.smooth_scroll_delta.y = 0.0);
            }
        }

        if response.double_clicked() {
            if let Some(id) = hovered
                && let Some(g) = gifts.iter().find(|g| g.id == id)
            {
                self.camera.goal.target = [g.position[0], 0.45 * BOX_SIZE, g.position[1]];
                self.camera.goal.distance = 0.35;
                self.fit = None;
            } else {
                self.frame_all(gifts);
            }
            cam_input = true;
        } else if response.clicked() {
            out.clicked = hovered;
        }

        if opts.performance && !cam_input {
            self.camera.goal.yaw += 2f32.to_radians() * dt;
        }
        let moving = self.camera.update(dt);
        self.leds.retain(|id, _| gifts.iter().any(|g| g.id == *id));
        for g in gifts {
            self.leds
                .entry(g.id)
                .and_modify(|s| s.update(g.leds, dt))
                .or_insert_with(|| LedSmooth::new(g.leds));
        }
        let settling = self.leds.values().any(LedSmooth::settling);
        if moving || settling || opts.performance || self.drag.is_some() {
            ui.ctx().request_repaint();
        }
        if cam_input && !response.double_clicked() || self.drag.is_some() {
            self.needs_frame = false;
            self.fit = None;
        }

        ui.painter().add(egui_wgpu::Callback::new_paint_callback(
            rect,
            StageCallback(self.frame_data(rect, gifts, opts, hovered, time)),
        ));
        self.draw_overlay(ui, rect, gifts, opts, hovered);
        out
    }

    /// View-projection matrix of the current (smoothed) camera pose.
    fn view_proj(&self) -> glam::Mat4 {
        let cam = self.camera.current;
        cam.proj(self.last_aspect) * cam.view()
    }

    /// Where `g` is drawn: at the drag position while it is being dragged.
    fn gift_pos(&self, g: &StageGift) -> [f32; 2] {
        match &self.drag {
            Some(d) if d.id == g.id => d.pos,
            _ => g.position,
        }
    }

    /// Nearest gift hit by `ray`.
    fn pick(&self, gifts: &[StageGift], (o, d): Ray) -> Option<u64> {
        let mut best: Option<(f32, u64)> = None;
        for g in gifts {
            if let Some(t) = ray_box(o, d, self.gift_pos(g), g.yaw)
                && best.is_none_or(|(bt, _)| t < bt)
            {
                best = Some((t, g.id));
            }
        }
        best.map(|b| b.1)
    }

    /// Everything the GPU needs for this frame.
    fn frame_data(
        &self,
        rect: Rect,
        gifts: &[StageGift],
        opts: &StageOptions,
        hovered: Option<u64>,
        time: f64,
    ) -> FrameData {
        let cam = self.camera.current;
        let eye = cam.eye();
        let chrome = !opts.performance;
        let gpu_gifts: Vec<GpuGift> = gifts
            .iter()
            .map(|g| {
                let p = self.gift_pos(g);
                let mut leds = [[0.0f32; 4]; 4];
                if g.online {
                    let smooth = self.leds.get(&g.id).map(|s| s.s2);
                    for (i, dst) in leds.iter_mut().enumerate() {
                        let c = g.leds[i];
                        let v = smooth.map_or([c.r as f32, c.g as f32, c.b as f32], |s| s[i]);
                        let l = v.map(|x| srgb_to_linear(x / 255.0) * LED_INTENSITY);
                        *dst = [l[0], l[1], l[2], 0.0];
                    }
                }
                GpuGift {
                    pos_yaw: [p[0], p[1], g.yaw, 1.0],
                    leds,
                    flags: [
                        g.online as u32 as f32,
                        (g.selected && chrome) as u32 as f32,
                        (hovered == Some(g.id) && chrome) as u32 as f32,
                        self.drag.as_ref().is_some_and(|d| d.id == g.id) as u32 as f32,
                    ],
                }
            })
            .collect();
        let grid = match (opts.show_floor_grid, opts.edit_layout, opts.performance) {
            (false, _, _) => 0.0,
            (true, _, true) => 0.5,
            (true, true, false) => 2.0,
            (true, false, false) => 1.0,
        };
        let [ar, ag, ab] = srgb_to_linear_color(theme::ACCENT);
        let [fr, fg, fb] = srgb_to_linear_color(STAGE_BG);
        let mut globals = Box::new(Globals {
            view_proj: self.view_proj().to_cols_array_2d(),
            eye: [eye.x, eye.y, eye.z, (time % 10_000.0) as f32],
            target: [
                cam.target[0],
                cam.target[1],
                cam.target[2],
                gpu_gifts.len().min(MAX_POOLS) as f32,
            ],
            grid: [grid, (layout_radius(gifts) * 2.5).max(0.8), 1.0, 1.0],
            // w: fog start distance (metres).
            accent: [ar, ag, ab, cam.distance * 0.7],
            fog: [fr, fg, fb, 0.35],
            gifts: [GpuGift::default(); MAX_POOLS],
        });
        for (dst, src) in globals.gifts.iter_mut().zip(&gpu_gifts) {
            *dst = *src;
        }
        let post = PostUniform {
            uv_rect: [0.0, 0.0, 1.0, 1.0],
            params: [
                opts.exposure.max(0.0),
                opts.bloom.max(0.0) * 0.9,
                0.35,
                (time % 1000.0) as f32,
            ],
            params2: [0.0, 0.30, 0.008, 1.0],
        };
        FrameData {
            stage_id: self.id,
            alive: self.alive.clone(),
            rect,
            globals,
            gifts: gpu_gifts,
            post,
        }
    }

    /// Labels, the edit-mode hint and the cursor, painted on top of the 3D view.
    fn draw_overlay(
        &self,
        ui: &egui::Ui,
        rect: Rect,
        gifts: &[StageGift],
        opts: &StageOptions,
        hovered: Option<u64>,
    ) {
        let painter = ui.painter_at(rect);
        let view_proj = self.view_proj();
        let project = |w: Vec3| -> Option<Pos2> {
            let c = view_proj * w.extend(1.0);
            if c.w <= 1e-4 {
                return None;
            }
            let n = c.xyz() / c.w;
            Some(Pos2::new(
                rect.min.x + (n.x * 0.5 + 0.5) * rect.width(),
                rect.min.y + (0.5 - n.y * 0.5) * rect.height(),
            ))
        };
        for g in gifts {
            let is_hovered = hovered == Some(g.id);
            let show = !opts.performance && (opts.labels || is_hovered || g.selected);
            if !show || g.label.is_empty() && g.online {
                continue;
            }
            let p = self.gift_pos(g);
            let anchor = Vec3::new(p[0], (mesh::TOTAL_H + 0.25) * BOX_SIZE, p[1]);
            if let Some(sp) = project(anchor) {
                draw_label(&painter, sp, g, is_hovered);
            }
        }
        if opts.edit_layout && !opts.performance {
            let text = "Drag boxes to move · snaps to 5 cm · hold Shift for free placement";
            painter.text(
                rect.left_bottom() + Vec2::new(12.0, -12.0),
                egui::Align2::LEFT_BOTTOM,
                text,
                egui::FontId::proportional(11.5),
                theme::TEXT_FAINT,
            );
        }
        if self.drag.is_some() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::Grabbing);
        } else if hovered.is_some() && opts.edit_layout {
            ui.ctx().set_cursor_icon(egui::CursorIcon::Grab);
        } else if hovered.is_some() && !opts.performance {
            ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
        }
    }
}

fn draw_label(painter: &egui::Painter, anchor: Pos2, g: &StageGift, hovered: bool) {
    let font = egui::FontId::proportional(11.5);
    let (text, color) = if g.online {
        (g.label.clone(), theme::TEXT)
    } else if g.label.is_empty() {
        ("Offline".to_owned(), theme::TEXT_FAINT)
    } else {
        (format!("{} · Offline", g.label), theme::TEXT_FAINT)
    };
    let galley = painter.layout_no_wrap(text, font, color);
    let pad = Vec2::new(8.0, 3.0);
    let size = galley.size() + pad * 2.0;
    let rect = Rect::from_min_size(anchor - Vec2::new(size.x * 0.5, size.y), size);
    let fill = theme::SURFACE_3.gamma_multiply(0.85);
    let stroke = if g.selected {
        egui::Stroke::new(1.0, theme::ACCENT)
    } else if hovered {
        egui::Stroke::new(1.0, theme::BORDER)
    } else {
        egui::Stroke::NONE
    };
    painter.rect(rect, egui::CornerRadius::same(255), fill, stroke, egui::StrokeKind::Inside);
    painter.galley(rect.min + pad, galley, Color32::WHITE);
}

/// Camera distance that fits a circle of radius `r` (metres) in view.
fn fit_distance(r: f32, aspect: f32) -> f32 {
    let half_v = FOV_Y * 0.5;
    let half_h = (half_v.tan() * aspect).atan();
    r / half_v.min(half_h).sin()
}

/// Floor bounding box `(min, max)` of all gifts, `None` without gifts.
fn bounds(gifts: &[StageGift]) -> Option<(glam::Vec2, glam::Vec2)> {
    let mut points = gifts.iter().map(|g| glam::Vec2::from(g.position));
    let first = points.next()?;
    Some(points.fold((first, first), |(lo, hi), p| (lo.min(p), hi.max(p))))
}

/// Radius of the layout around its bounding-box centre, metres.
fn layout_radius(gifts: &[StageGift]) -> f32 {
    bounds(gifts).map_or(0.3, |(lo, hi)| (hi - lo).length() * 0.5 + BOX_SIZE)
}

/// A picking ray: (origin, normalised direction).
type Ray = (Vec3, Vec3);

/// Ray vs. the y = 0 plane, returns the hit (x, z).
fn ray_floor((o, d): Ray) -> Option<glam::Vec2> {
    if d.y.abs() < 1e-6 {
        return None;
    }
    let t = -o.y / d.y;
    (t > 0.0).then(|| {
        let p = o + d * t;
        glam::Vec2::new(p.x, p.z)
    })
}

/// Ray vs. a gift's oriented bounding box; returns the hit distance.
fn ray_box(o: Vec3, d: Vec3, pos: [f32; 2], yaw: f32) -> Option<f32> {
    // Into box space (box units): inverse of world = R(yaw) * p * BOX + pos.
    let (s, c) = yaw.sin_cos();
    let inv_rot = |v: Vec3| Vec3::new(c * v.x - s * v.z, v.y, s * v.x + c * v.z);
    let lo_ = inv_rot(o - Vec3::new(pos[0], 0.0, pos[1])) / BOX_SIZE;
    let ld = inv_rot(d) / BOX_SIZE;
    let bmin = Vec3::new(-mesh::LID_HALF, 0.0, -mesh::LID_HALF);
    let bmax = Vec3::new(mesh::LID_HALF, mesh::TOTAL_H, mesh::LID_HALF);
    let inv = ld.recip();
    let t1 = (bmin - lo_) * inv;
    let t2 = (bmax - lo_) * inv;
    let tmin = t1.min(t2).max_element();
    let tmax = t1.max(t2).min_element();
    (tmax >= tmin.max(0.0)).then_some(tmin.max(0.0))
}

fn srgb_to_linear(c: f32) -> f32 {
    if c <= 0.04045 {
        c / 12.92
    } else {
        ((c + 0.055) / 1.055).powf(2.4)
    }
}

/// LED byte colour (pre-gamma sRGB) to linear light.
fn srgb_to_linear_rgb(c: Rgb) -> [f32; 3] {
    [
        srgb_to_linear(c.r as f32 / 255.0),
        srgb_to_linear(c.g as f32 / 255.0),
        srgb_to_linear(c.b as f32 / 255.0),
    ]
}

pub(crate) fn srgb_to_linear_color(c: Color32) -> [f32; 3] {
    srgb_to_linear_rgb(Rgb::new(c.r(), c.g(), c.b()))
}

// The callback must be Send + Sync for egui_wgpu.
const _: () = {
    const fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<StageCallback>();
};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn led_smoothing_glides_steps_and_snaps_jumps() {
        let c = |v: u8| [Rgb::new(v, 0, 0); LEDS_PER_GIFT];
        let dt = 1.0 / 60.0;
        // A dim slow ramp: one level every 15 frames.
        let mut s = LedSmooth::new(c(30));
        let mut out = Vec::new();
        for f in 0..240 {
            s.update(c(30 + (f / 15) as u8), dt);
            out.push(s.s2[0][0]);
        }
        // Monotonic, and it moves on most frames instead of in 1-level steps.
        assert!(out.windows(2).all(|w| w[1] >= w[0]));
        let max_step = out.windows(2).map(|w| w[1] - w[0]).fold(0.0, f32::max);
        assert!(max_step < 0.35, "max step {max_step}");
        // A big change (strobe) is shown immediately.
        s.update(c(200), dt);
        assert_eq!(s.s2[0][0], 200.0);
        assert!(!s.settling());
    }

    #[test]
    fn ray_hits_box_from_above() {
        let t = ray_box(Vec3::new(0.0, 1.0, 0.0), Vec3::NEG_Y, [0.0, 0.0], 0.3);
        assert!(t.is_some());
        let miss = ray_box(Vec3::new(0.2, 1.0, 0.0), Vec3::NEG_Y, [0.0, 0.0], 0.3);
        assert!(miss.is_none());
    }

    type FrameResult = (Option<u64>, Option<u64>, Vec<(u64, [f32; 2])>);

    fn frame(
        ctx: &egui::Context,
        stage: &mut Stage,
        gifts: &[StageGift],
        opts: &StageOptions,
        events: Vec<egui::Event>,
        t: f64,
    ) -> FrameResult {
        let input = egui::RawInput {
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(800.0, 600.0))),
            time: Some(t),
            events,
            ..Default::default()
        };
        let mut res = (None, None, Vec::new());
        let mut out = ctx.run_ui(input, |ui| {
            let r = stage.show(ui, gifts, opts);
            res = (r.hovered, r.clicked, r.moved);
        });
        out.textures_delta.clear();
        res
    }

    fn press(pos: Pos2, pressed: bool) -> egui::Event {
        egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::NONE,
        }
    }

    #[test]
    fn hover_click_and_drag() {
        let ctx = egui::Context::default();
        let mut stage = Stage::without_gpu();
        let gifts = vec![StageGift {
            id: 7,
            position: [0.0, 0.0],
            yaw: 0.0,
            leds: [Rgb::new(255, 0, 0); 4],
            online: true,
            selected: false,
            label: "a".into(),
        }];
        let mut opts = StageOptions::default();
        let c = Pos2::new(400.0, 300.0);
        let mut t = 0.0;
        let mut step = |stage: &mut Stage, opts: &StageOptions, ev: Vec<egui::Event>| {
            t += 1.0 / 60.0;
            frame(&ctx, stage, &gifts, opts, ev, t)
        };
        step(&mut stage, &opts, vec![]);
        let (hovered, _, _) = step(&mut stage, &opts, vec![egui::Event::PointerMoved(c)]);
        assert_eq!(hovered, Some(7), "box under the screen centre after framing");

        // Click (no edit mode).
        step(&mut stage, &opts, vec![press(c, true)]);
        let (_, clicked, _) = step(&mut stage, &opts, vec![press(c, false)]);
        assert_eq!(clicked, Some(7));

        // Empty space is not hovered.
        let (hovered, _, _) = step(&mut stage, &opts, vec![egui::Event::PointerMoved(Pos2::new(10.0, 10.0))]);
        assert_eq!(hovered, None);

        // Drag in edit mode: moves snap to 5 cm.
        opts.edit_layout = true;
        step(&mut stage, &opts, vec![egui::Event::PointerMoved(c)]);
        step(&mut stage, &opts, vec![press(c, true)]);
        let mut moved = Vec::new();
        for i in 1..=10 {
            let p = c + Vec2::new(20.0 * i as f32, 0.0);
            let (_, _, m) = step(&mut stage, &opts, vec![egui::Event::PointerMoved(p)]);
            moved.extend(m);
        }
        step(&mut stage, &opts, vec![press(c + Vec2::new(200.0, 0.0), false)]);
        let last = moved.last().expect("drag reported moves");
        assert_eq!(last.0, 7);
        assert!(last.1[0] > 0.04, "moved right: {:?}", last.1);
        for v in last.1 {
            let k = v / SNAP;
            assert!((k - k.round()).abs() < 1e-3, "snapped: {v}");
        }
        // The camera did not orbit during the box drag.
        assert_eq!(stage.camera().yaw, CameraState::default().yaw);
    }

    #[test]
    fn zero_size_is_harmless() {
        let ctx = egui::Context::default();
        let mut stage = Stage::without_gpu();
        let input = egui::RawInput {
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::ZERO)),
            ..Default::default()
        };
        let mut out = ctx.run_ui(input, |ui| {
            let r = stage.show(ui, &[], &StageOptions::default());
            assert!(r.hovered.is_none());
        });
        out.textures_delta.clear();
        stage.frame_all(&[]);
    }

    #[test]
    fn floor_hit() {
        let h = ray_floor((Vec3::new(1.0, 2.0, 0.0), Vec3::new(0.0, -1.0, 0.0))).unwrap();
        assert!((h.x - 1.0).abs() < 1e-6);
    }
}
