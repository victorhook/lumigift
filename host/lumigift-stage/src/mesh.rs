//! Procedural gift-box mesh (body, lid, eyelet, lid slot).
//!
//! Units: body width = 1.0 ("box units"). The vertex shader scales by
//! [`crate::BOX_SIZE`] to get metres. Dimensions follow `docs/UI_DESIGN.md` §5.2.

use bytemuck::{Pod, Zeroable};
use glam::Vec3;

/// Mesh part ids, stored per vertex (the shader picks the material from it).
pub const PART_BODY: f32 = 0.0;
pub const PART_LID: f32 = 1.0;
pub const PART_EYELET: f32 = 2.0;
pub const PART_SLOT: f32 = 3.0;

/// Body: 1.00 × 0.86 × 1.00.
pub const BODY_H: f32 = 0.86;
/// Lid spans y 0.84 → 0.97.
pub const LID_TOP: f32 = 0.97;
/// Top of the eyelet loop (used for picking bounds and label anchors).
pub const TOTAL_H: f32 = 1.10;
/// Half width of the lid (the widest part).
pub const LID_HALF: f32 = 0.53;

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
pub struct Vertex {
    pub pos: [f32; 3],
    pub part: f32,
    pub normal: [f32; 3],
    pub _pad: f32,
}

#[derive(Default)]
pub struct Mesh {
    pub vertices: Vec<Vertex>,
    pub indices: Vec<u32>,
}

impl Mesh {
    fn push(&mut self, p: Vec3, n: Vec3, part: f32) -> u32 {
        let i = self.vertices.len() as u32;
        self.vertices.push(Vertex {
            pos: p.to_array(),
            part,
            normal: n.to_array(),
            _pad: 0.0,
        });
        i
    }

    /// Rounded box centred at `centre` with the given half extents and edge
    /// radius. The grid lines are concentrated in the rounded band so the
    /// edges stay smooth with few triangles.
    fn rounded_box(&mut self, centre: Vec3, half: Vec3, r: f32, part: f32, skip_bottom: bool) {
        let coords = |h: f32| -> Vec<f32> {
            let mut v = vec![-h, -h + r * 0.29, -h + r];
            let inner = 6;
            for i in 1..inner {
                v.push(-h + r + (2.0 * (h - r)) * i as f32 / inner as f32);
            }
            v.extend_from_slice(&[h - r, h - r * 0.29, h]);
            v
        };
        let inner_half = half - Vec3::splat(r);
        // (normal axis, u axis, v axis), sign chosen so triangles wind CCW from outside.
        let faces: [(Vec3, Vec3, Vec3); 6] = [
            (Vec3::X, Vec3::NEG_Z, Vec3::Y),
            (Vec3::NEG_X, Vec3::Z, Vec3::Y),
            (Vec3::Y, Vec3::X, Vec3::NEG_Z),
            (Vec3::NEG_Y, Vec3::X, Vec3::Z),
            (Vec3::Z, Vec3::X, Vec3::Y),
            (Vec3::NEG_Z, Vec3::NEG_X, Vec3::Y),
        ];
        for (n, u, v) in faces {
            if skip_bottom && n == Vec3::NEG_Y {
                continue;
            }
            let hn = (n.abs() * half).element_sum();
            let hu = (u.abs() * half).element_sum();
            let hv = (v.abs() * half).element_sum();
            let cu = coords(hu);
            let cv = coords(hv);
            let base = self.vertices.len() as u32;
            for &b in &cv {
                for &a in &cu {
                    let p = n * hn + u * a + v * b;
                    let pi = p.clamp(-inner_half, inner_half);
                    let d = p - pi;
                    let nn = if d.length_squared() > 1e-12 {
                        d.normalize()
                    } else {
                        n
                    };
                    self.push(centre + pi + nn * r, nn, part);
                }
            }
            let w = cu.len() as u32;
            for j in 0..cv.len() as u32 - 1 {
                for i in 0..w - 1 {
                    let a = base + j * w + i;
                    let b = a + 1;
                    let c = a + w;
                    let d = c + 1;
                    self.indices.extend_from_slice(&[a, b, d, a, d, c]);
                }
            }
        }
    }

    /// Torus in the XY plane (the hanging eyelet).
    fn torus(&mut self, centre: Vec3, big_r: f32, small_r: f32, part: f32) {
        let (nu, nv) = (24u32, 10u32);
        let base = self.vertices.len() as u32;
        for i in 0..=nu {
            let a = i as f32 / nu as f32 * std::f32::consts::TAU;
            let ring = Vec3::new(a.cos(), a.sin(), 0.0);
            for j in 0..=nv {
                let b = j as f32 / nv as f32 * std::f32::consts::TAU;
                let n = ring * b.cos() + Vec3::Z * b.sin();
                self.push(centre + ring * big_r + n * small_r, n, part);
            }
        }
        let w = nv + 1;
        for i in 0..nu {
            for j in 0..nv {
                let a = base + i * w + j;
                let b = a + w;
                self.indices
                    .extend_from_slice(&[a, b, b + 1, a, b + 1, a + 1]);
            }
        }
    }

    /// Open cylinder along Y plus a top cap.
    fn cylinder(&mut self, centre_bottom: Vec3, r: f32, h: f32, part: f32) {
        let n = 20u32;
        let dir = |i: u32| {
            let a = i as f32 / n as f32 * std::f32::consts::TAU;
            Vec3::new(a.cos(), 0.0, -a.sin())
        };
        let base = self.vertices.len() as u32;
        for i in 0..=n {
            let d = dir(i);
            self.push(centre_bottom + d * r, d, part);
            self.push(centre_bottom + d * r + Vec3::Y * h, d, part);
        }
        for i in 0..n {
            let a = base + i * 2;
            self.indices
                .extend_from_slice(&[a, a + 2, a + 3, a, a + 3, a + 1]);
        }
        let c = self.push(centre_bottom + Vec3::Y * h, Vec3::Y, part);
        let ring = self.vertices.len() as u32;
        for i in 0..=n {
            self.push(centre_bottom + dir(i) * r + Vec3::Y * h, Vec3::Y, part);
        }
        for i in 0..n {
            self.indices.extend_from_slice(&[c, ring + i, ring + i + 1]);
        }
    }

    /// Flat quad facing +Y.
    fn quad_up(&mut self, centre: Vec3, hx: f32, hz: f32, part: f32) {
        let a = self.push(centre + Vec3::new(-hx, 0.0, -hz), Vec3::Y, part);
        let b = self.push(centre + Vec3::new(-hx, 0.0, hz), Vec3::Y, part);
        let c = self.push(centre + Vec3::new(hx, 0.0, hz), Vec3::Y, part);
        let d = self.push(centre + Vec3::new(hx, 0.0, -hz), Vec3::Y, part);
        self.indices.extend_from_slice(&[a, b, c, a, c, d]);
    }
}

/// Builds the full gift mesh (~2.5 k triangles).
pub fn gift_mesh() -> Mesh {
    let mut m = Mesh::default();
    m.rounded_box(
        Vec3::new(0.0, BODY_H * 0.5, 0.0),
        Vec3::new(0.5, BODY_H * 0.5, 0.5),
        0.05,
        PART_BODY,
        false,
    );
    m.rounded_box(
        Vec3::new(0.0, (0.84 + LID_TOP) * 0.5, 0.0),
        Vec3::new(LID_HALF, 0.065, LID_HALF),
        0.035,
        PART_LID,
        false,
    );
    m.cylinder(Vec3::new(0.0, LID_TOP - 0.002, 0.0), 0.05, 0.032, PART_EYELET);
    m.torus(Vec3::new(0.0, 1.065, 0.0), 0.075, 0.026, PART_EYELET);
    m.quad_up(Vec3::new(0.0, LID_TOP + 0.0015, 0.14), 0.02, 0.0075, PART_SLOT);
    m
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mesh_is_sane() {
        let m = gift_mesh();
        assert!(m.indices.len().is_multiple_of(3));
        assert!(m.indices.iter().all(|&i| (i as usize) < m.vertices.len()));
        assert!(m.vertices.len() < 10_000);
        // Outward winding: every triangle's geometric normal agrees with its vertex normal.
        let v = |i: u32| Vec3::from(m.vertices[i as usize].pos);
        let mut outward = 0;
        let mut total = 0;
        for t in m.indices.chunks(3) {
            let (a, b, c) = (v(t[0]), v(t[1]), v(t[2]));
            let n = (b - a).cross(c - a);
            if n.length() < 1e-9 {
                continue;
            }
            let vn = Vec3::from(m.vertices[t[0] as usize].normal);
            total += 1;
            if n.dot(vn) > 0.0 {
                outward += 1;
            }
        }
        assert_eq!(outward, total, "all triangles wind CCW seen from outside");
    }
}
