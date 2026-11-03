//! Framing: fit the camera, and the window's aspect, tightly around the pet.

use glam::{Mat4, Vec2, Vec3};

/// Extra room around the pet for breathing, squash and stretch.
const MARGIN: f32 = 1.12;
const FOV_Y: f32 = 30.0;

pub struct Camera {
    pub eye: Vec3,
    pub target: Vec3,
    pub aspect: f32,
    near: f32,
    far: f32,
}

impl Camera {
    /// Frames a pet with the given bounds from the front. The pet spins about Y,
    /// so the width covers the largest horizontal radius at any angle.
    pub fn fit(lo: Vec3, hi: Vec3) -> Self {
        let center_y = (lo.y + hi.y) * 0.5;
        let half_h = (hi.y - lo.y) * 0.5 * MARGIN;
        let radius_xz = Vec2::new(lo.x.abs().max(hi.x.abs()), lo.z.abs().max(hi.z.abs())).length();
        let half_w = radius_xz * MARGIN;
        let distance = half_h / (FOV_Y.to_radians() * 0.5).tan() + radius_xz;
        let target = Vec3::new(0.0, center_y, 0.0);
        Self {
            eye: target + Vec3::new(0.0, 0.0, distance),
            target,
            aspect: half_w / half_h,
            near: (distance - radius_xz * 2.0).max(0.05),
            far: distance + radius_xz * 2.0,
        }
    }

    pub fn view_proj(&self) -> Mat4 {
        let proj = glam::camera::rh::proj::directx::perspective(
            FOV_Y.to_radians(),
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
