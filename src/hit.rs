//! Exact cursor hit testing against the posed mesh, so clicks fall through
//! everywhere the pet isn't drawn, including between its legs.

use glam::Vec3;

/// Möller–Trumbore, two-sided.
fn ray_triangle(o: Vec3, d: Vec3, a: Vec3, b: Vec3, c: Vec3) -> bool {
    let e1 = b - a;
    let e2 = c - a;
    let p = d.cross(e2);
    let det = e1.dot(p);
    if det.abs() < 1e-9 {
        return false;
    }
    let inv = 1.0 / det;
    let s = o - a;
    let u = s.dot(p) * inv;
    if !(0.0..=1.0).contains(&u) {
        return false;
    }
    let q = s.cross(e1);
    let v = d.dot(q) * inv;
    v >= 0.0 && u + v <= 1.0 && e2.dot(q) * inv >= 0.0
}

/// Whether a ray hits any triangle of the mesh. `slack` grows each triangle
/// about its centroid, covering outlines, fur and a finger's worth of error.
pub fn ray_mesh(o: Vec3, d: Vec3, positions: &[Vec3], indices: &[u32], slack: f32) -> bool {
    indices.as_chunks::<3>().0.iter().any(|t| {
        let [a, b, c] = [0, 1, 2].map(|i| positions[t[i] as usize]);
        let m = (a + b + c) / 3.0;
        let grow = |p: Vec3| p + (p - m).normalize_or_zero() * slack;
        ray_triangle(o, d, grow(a), grow(b), grow(c))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hits_inside_and_misses_outside() {
        let tri = [
            Vec3::new(-1.0, -1.0, 0.0),
            Vec3::new(1.0, -1.0, 0.0),
            Vec3::new(0.0, 1.0, 0.0),
        ];
        let d = Vec3::NEG_Z;
        assert!(ray_mesh(Vec3::new(0.0, 0.0, 5.0), d, &tri, &[0, 1, 2], 0.0));
        assert!(!ray_mesh(
            Vec3::new(2.0, 0.0, 5.0),
            d,
            &tri,
            &[0, 1, 2],
            0.0
        ));
        assert!(!ray_mesh(
            Vec3::new(0.0, 0.0, -5.0),
            d,
            &tri,
            &[0, 1, 2],
            0.0
        ));
    }
}
