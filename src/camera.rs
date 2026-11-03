//! Framing: fit the camera, and the window's aspect, tightly around the pet.

use glam::{Mat4, Vec2, Vec3};

/// Extra room around the pet for animation, fur and outlines.
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
    /// Frames everything in `lo..hi` (already in camera-facing world space)
    /// from the front.
    pub fn fit(lo: Vec3, hi: Vec3) -> Self {
        let center = (lo + hi) * 0.5;
        let half = (hi - lo) * 0.5 * MARGIN;
        let fov_y = FOV_Y.to_radians();
        let distance = half.y / (fov_y * 0.5).tan() + half.z;
        let target = Vec3::new(center.x, center.y, center.z);
        Self {
            eye: target + Vec3::new(0.0, 0.0, distance),
            target,
            aspect: (half.x / half.y).max(0.2),
            fov_y,
            near: (distance - half.z * 2.0).max(distance * 0.05),
            far: distance + half.z * 2.0,
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
