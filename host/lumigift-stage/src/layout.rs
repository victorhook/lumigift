//! Automatic floor arrangements.

use serde::{Deserialize, Serialize};

/// Preset floor arrangements for [`crate::Stage::auto_layout`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Arrangement {
    /// One straight line along x.
    Row,
    /// A gentle arc opening towards the default camera (+z), boxes turned to face its centre.
    #[default]
    Arc,
    /// A near-square grid.
    Grid,
    /// A full circle, boxes facing outwards.
    Circle,
    /// A "Christmas tree" triangle: 1, 2, 3, … boxes per row, tip at the back (−z).
    Pyramid,
}

impl Arrangement {
    pub const ALL: [Arrangement; 5] = [
        Arrangement::Row,
        Arrangement::Arc,
        Arrangement::Grid,
        Arrangement::Circle,
        Arrangement::Pyramid,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Arrangement::Row => "Row",
            Arrangement::Arc => "Arc",
            Arrangement::Grid => "Grid",
            Arrangement::Circle => "Circle",
            Arrangement::Pyramid => "Tree",
        }
    }
}

/// Centre-to-centre spacing between neighbouring boxes, metres.
pub const SPACING: f32 = 0.15;

/// Positions (x, z) and yaw for `n` boxes, centred on the origin.
pub fn positions(n: usize, arrangement: Arrangement) -> Vec<([f32; 2], f32)> {
    if n == 0 {
        return Vec::new();
    }
    let s = SPACING;
    let mut out = Vec::with_capacity(n);
    match arrangement {
        Arrangement::Row => {
            for i in 0..n {
                out.push(([(i as f32 - (n - 1) as f32 * 0.5) * s, 0.0], 0.0));
            }
        }
        Arrangement::Arc => {
            if n == 1 {
                out.push(([0.0, 0.0], 0.0));
            } else {
                // Total sweep grows with n but stays ≤ 150°.
                let sweep = (n as f32 * 14f32.to_radians()).min(150f32.to_radians());
                let step = sweep / (n - 1) as f32;
                let r = s / (2.0 * (step * 0.5).sin());
                for i in 0..n {
                    let a = -sweep * 0.5 + step * i as f32;
                    // Centre of the circle is at +z (towards the viewer).
                    let x = r * a.sin();
                    let z = r - r * a.cos();
                    out.push(([x, z], -a));
                }
                // Centre the bounding box on the origin.
                let zmax = out.iter().map(|p| p.0[1]).fold(f32::MIN, f32::max);
                for p in &mut out {
                    p.0[1] -= zmax * 0.5;
                }
            }
        }
        Arrangement::Grid => {
            let cols = (n as f32).sqrt().ceil() as usize;
            let rows = n.div_ceil(cols);
            for i in 0..n {
                let (r, c) = (i / cols, i % cols);
                let in_row = if r == rows - 1 { n - r * cols } else { cols };
                let x = (c as f32 - (in_row - 1) as f32 * 0.5) * s;
                let z = (r as f32 - (rows - 1) as f32 * 0.5) * s;
                out.push(([x, z], 0.0));
            }
        }
        Arrangement::Circle => {
            if n == 1 {
                out.push(([0.0, 0.0], 0.0));
            } else {
                let step = std::f32::consts::TAU / n as f32;
                let r = (s / (2.0 * (step * 0.5).sin())).max(s * 0.75);
                for i in 0..n {
                    let a = step * i as f32;
                    out.push(([r * a.sin(), r * a.cos()], a));
                }
            }
        }
        Arrangement::Pyramid => {
            // Row r holds r + 1 boxes; the last row takes what is left.
            let mut rows: Vec<usize> = Vec::new();
            let mut left = n;
            while left > 0 {
                let k = (rows.len() + 1).min(left);
                rows.push(k);
                left -= k;
            }
            let nrows = rows.len();
            for (r, &k) in rows.iter().enumerate() {
                for c in 0..k {
                    let x = (c as f32 - (k - 1) as f32 * 0.5) * s;
                    let z = (r as f32 - (nrows - 1) as f32 * 0.5) * s * 0.9;
                    out.push(([x, z], 0.0));
                }
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_overlaps() {
        for a in Arrangement::ALL {
            for n in [1usize, 2, 3, 7, 10, 30] {
                let p = positions(n, a);
                assert_eq!(p.len(), n);
                for i in 0..n {
                    for j in i + 1..n {
                        let dx = p[i].0[0] - p[j].0[0];
                        let dz = p[i].0[1] - p[j].0[1];
                        assert!(
                            (dx * dx + dz * dz).sqrt() > 0.09,
                            "{a:?} n={n} {i}-{j} overlap"
                        );
                    }
                }
            }
        }
    }
}
