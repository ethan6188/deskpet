//! Framing: fit the camera, and the window's aspect, tightly around the pet.

use glam::{Mat4, Vec2, Vec3};

/// Extra room around the pet for breathing and the hit-test slack.
const MARGIN: f32 = 1.1;
const FOV_Y: f32 = 25.0;

pub struct Camera {
    pub eye: Vec3,
    pub target: Vec3,
    pub aspect: f32,
    fov_y: f32,
    near: f32,
    far: f32,
}

impl Camera {
    /// The tightest front view that keeps every point (in world space) in
    /// frame: the camera backs off just far enough for the tallest point, then
    /// the width follows from the widest point at its own depth.
    pub fn fit_points(points: &[Vec3]) -> Self {
        let fov_y = FOV_Y.to_radians();
        let tan = (fov_y * 0.5).tan();
        let (lo, hi) = points.iter().fold(
            (Vec3::splat(f32::MAX), Vec3::splat(f32::MIN)),
            |(l, h), p| (l.min(*p), h.max(*p)),
        );
        let cy = (lo.y + hi.y) * 0.5;
        let cx = (lo.x + hi.x) * 0.5;
        // Eye at (cx, cy, d): a point at depth z is seen at distance d - z.
        let d = points
            .iter()
            .map(|p| p.z + (p.y - cy).abs() * MARGIN / tan)
            .fold(f32::MIN, f32::max);
        let half_w_ratio = points
            .iter()
            .map(|p| (p.x - cx).abs() * MARGIN / (d - p.z).max(1e-4))
            .fold(0.0, f32::max);
        let target = Vec3::new(cx, cy, 0.0);
        let near = points.iter().map(|p| d - p.z).fold(f32::MAX, f32::min);
        let far = points.iter().map(|p| d - p.z).fold(f32::MIN, f32::max);
        Self {
            eye: Vec3::new(cx, cy, d),
            target,
            aspect: (half_w_ratio / tan).max(0.2),
            fov_y,
            near: (near * 0.9).max(d * 0.01),
            far: far * 1.1,
        }
    }

    pub fn view_proj(&self) -> Mat4 {
        let proj = glam::camera::rh::proj::directx::perspective(
            self.fov_y,
            self.aspect,
            self.near,
            self.far,
        );
        proj * glam::camera::rh::view::look_at_mat4(self.eye, self.target, Vec3::Y)
    }

    /// World-space ray through a point in normalized device coordinates.
    pub fn ray(&self, ndc: Vec2) -> (Vec3, Vec3) {
        let inv = self.view_proj().inverse();
        let near = inv.project_point3(ndc.extend(0.0));
        let far = inv.project_point3(ndc.extend(1.0));
        (near, (far - near).normalize())
    }
}
