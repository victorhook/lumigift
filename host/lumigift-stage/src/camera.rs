//! Damped orbit camera.

use glam::{Mat4, Vec3};
use serde::{Deserialize, Serialize};

/// Vertical field of view.
pub const FOV_Y: f32 = 32.0 * std::f32::consts::PI / 180.0;
const PITCH_MIN: f32 = 6.0 * std::f32::consts::PI / 180.0;
const PITCH_MAX: f32 = 85.0 * std::f32::consts::PI / 180.0;
/// Distance limits in metres.
pub const DIST_MIN: f32 = 0.10;
pub const DIST_MAX: f32 = 8.0;
/// Exponential smoothing half-life of all camera motion.
const HALF_LIFE: f32 = 0.08;

/// Orbit camera pose. All lengths are in metres, angles in radians.
///
/// This is what the app persists (it is `serde`-serialisable); restore it with
/// [`crate::Stage::set_camera`].
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct CameraState {
    /// Orbit centre (x, y, z), y up.
    pub target: [f32; 3],
    /// Rotation around the vertical axis. 0 looks along −z (from +z towards the origin).
    pub yaw: f32,
    /// Elevation above the floor plane, clamped to 6°…85°.
    pub pitch: f32,
    /// Distance from the target, clamped to [`DIST_MIN`]…[`DIST_MAX`].
    pub distance: f32,
}

impl Default for CameraState {
    fn default() -> Self {
        Self {
            target: [0.0, 0.03, 0.0],
            yaw: 25f32.to_radians(),
            pitch: 22f32.to_radians(),
            distance: 1.2,
        }
    }
}

impl CameraState {
    pub(crate) fn clamped(mut self) -> Self {
        self.pitch = self.pitch.clamp(PITCH_MIN, PITCH_MAX);
        self.distance = self.distance.clamp(DIST_MIN, DIST_MAX);
        if !self.yaw.is_finite() {
            self.yaw = 0.0;
        }
        for t in &mut self.target {
            if !t.is_finite() {
                *t = 0.0;
            }
        }
        self.target[1] = self.target[1].clamp(0.0, 1.0);
        self
    }

    pub fn eye(&self) -> Vec3 {
        let t = Vec3::from(self.target);
        let (sy, cy) = self.yaw.sin_cos();
        let (sp, cp) = self.pitch.sin_cos();
        t + Vec3::new(sy * cp, sp, cy * cp) * self.distance
    }

    pub fn view(&self) -> Mat4 {
        Mat4::look_at_rh(self.eye(), Vec3::from(self.target), Vec3::Y)
    }

    pub fn proj(&self, aspect: f32) -> Mat4 {
        // Near/far scale with distance to keep depth precision.
        let near = (self.distance * 0.02).clamp(0.003, 0.05);
        Mat4::perspective_rh(FOV_Y, aspect.max(1e-3), near, 60.0)
    }

    /// Right and up vectors of the view plane.
    pub fn basis(&self) -> (Vec3, Vec3) {
        let fwd = (Vec3::from(self.target) - self.eye()).normalize();
        let right = fwd.cross(Vec3::Y).normalize();
        let up = right.cross(fwd);
        (right, up)
    }

    fn lerp(&self, goal: &Self, t: f32) -> Self {
        let l = |a: f32, b: f32| a + (b - a) * t;
        Self {
            target: [
                l(self.target[0], goal.target[0]),
                l(self.target[1], goal.target[1]),
                l(self.target[2], goal.target[2]),
            ],
            yaw: l(self.yaw, goal.yaw),
            pitch: l(self.pitch, goal.pitch),
            // Interpolate distance in log space so zooming feels uniform.
            distance: (l(self.distance.ln(), goal.distance.ln())).exp(),
        }
    }

    fn close_to(&self, o: &Self) -> bool {
        let d = Vec3::from(self.target).distance(Vec3::from(o.target));
        d < 1e-4 * self.distance.max(0.1)
            && (self.yaw - o.yaw).abs() < 1e-4
            && (self.pitch - o.pitch).abs() < 1e-4
            && (self.distance / o.distance - 1.0).abs() < 1e-4
    }
}

/// Smoothed camera: `goal` is what input edits, `current` follows it.
#[derive(Clone, Debug, Default)]
pub struct OrbitCamera {
    pub goal: CameraState,
    pub current: CameraState,
}

impl OrbitCamera {
    /// Advances the smoothing. Returns true while still moving.
    pub fn update(&mut self, dt: f32) -> bool {
        self.goal = self.goal.clamped();
        let t = 1.0 - 0.5f32.powf(dt.max(0.0) / HALF_LIFE);
        self.current = self.current.lerp(&self.goal, t);
        if self.current.close_to(&self.goal) {
            self.current = self.goal;
            false
        } else {
            true
        }
    }

    pub fn snap(&mut self) {
        self.goal = self.goal.clamped();
        self.current = self.goal;
    }
}
