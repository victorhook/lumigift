//! Stage view adapter: the real 3D renderer from `host/lumigift-stage`.
//! (This module used to hold a 2D placeholder with the same API.)

pub use lumigift_stage::{Arrangement, CameraPreset, CameraState, SPACING, Stage, StageGift, StageOptions};
pub use lumigift_stage::BOX_SIZE;

/// Tight camera fit for small/wide viewports (the studio preview): keeps the
/// default yaw/pitch and finds the closest distance at which every box fits
/// inside 88% of the viewport. `Stage::frame_all` fits a bounding sphere, which
/// leaves wide previews mostly empty.
pub fn fit_tight(stage: &mut Stage, gifts: &[StageGift], aspect: f32) {
    if gifts.is_empty() || !aspect.is_finite() || aspect <= 0.0 {
        return;
    }
    let (mut lo, mut hi) = ([f32::MAX; 2], [f32::MIN; 2]);
    for g in gifts {
        for k in 0..2 {
            lo[k] = lo[k].min(g.position[k]);
            hi[k] = hi[k].max(g.position[k]);
        }
    }
    let h = BOX_SIZE * 0.6;
    let mut cam = CameraState {
        target: [(lo[0] + hi[0]) / 2.0, BOX_SIZE * 0.45, (lo[1] + hi[1]) / 2.0],
        ..CameraState::default()
    };
    let pts: Vec<glam::Vec3> = gifts
        .iter()
        .flat_map(|g| {
            let (x, z) = (g.position[0], g.position[1]);
            [(-1.0, -1.0), (1.0, -1.0), (1.0, 1.0), (-1.0, 1.0)]
                .into_iter()
                .flat_map(move |(dx, dz)| {
                    let (px, pz) = (x + dx * h, z + dz * h);
                    [glam::Vec3::new(px, 0.0, pz), glam::Vec3::new(px, BOX_SIZE * 1.1, pz)]
                })
        })
        .collect();
    let fits = |c: &CameraState| {
        let vp = c.proj(aspect) * c.view();
        pts.iter().all(|p| {
            let q = vp * p.extend(1.0);
            q.w > 0.0 && (q.x / q.w).abs() < 0.88 && (q.y / q.w).abs() < 0.80
        })
    };
    // Bisection on distance.
    let (mut near, mut far) = (lumigift_stage::DIST_MIN, lumigift_stage::DIST_MAX);
    for _ in 0..40 {
        let mid = 0.5 * (near + far);
        cam.distance = mid;
        if fits(&cam) { far = mid } else { near = mid }
    }
    cam.distance = far;
    stage.set_camera(cam);
}
