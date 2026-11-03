//! What the window app and the agent tools share: how a pet is framed and
//! placed, so a contact sheet shows exactly what the desktop will.

use glam::{Mat4, Quat, Vec3};

use crate::camera::Camera;
use crate::model::Model;
use crate::pet::Style;
use crate::pose::{self, Pose};
use crate::splat::Splat;

/// Samples per clip when measuring how far the pet moves.
const BOUNDS_SAMPLES: usize = 12;

/// Height of the rest pose in model units; style widths are fractions of it.
pub fn model_height(model: &Model) -> f32 {
    let (lo, hi) = pose::skinned_positions(model, &Pose::rest(model))
        .iter()
        .fold((f32::MAX, f32::MIN), |(lo, hi), p| {
            (lo.min(p.y), hi.max(p.y))
        });
    (hi - lo).max(1e-6)
}

/// Spin angles sampled when framing; a right-drag can turn the pet to any of them.
const FRAMING_YAWS: usize = 8;

/// A camera that keeps the pet in frame through every clip, at any spin, as
/// tightly as the projection allows.
/// `extra` pads the bounds, in model units, for anything drawn beyond the mesh.
pub fn framing(model: &Model, style: &Style, facing: Quat, extra: f32) -> Camera {
    let mut points = Vec::new();
    let mut poses = vec![Pose::rest(model)];
    for clip in &model.clips {
        for k in 0..=BOUNDS_SAMPLES {
            let mut pose = Pose::rest(model);
            let t = clip.duration * k as f32 / BOUNDS_SAMPLES as f32;
            pose::sample(model, clip, t, &mut pose);
            poses.push(pose);
        }
    }
    // Corners of each pose's box are enough, and keep this fast for big meshes.
    let height = model_height(model);
    let pad = style.fur.max(style.outline) * height + extra;
    for pose in &poses {
        let (lo, hi) = pose::skinned_positions(model, pose).iter().fold(
            (Vec3::splat(f32::MAX), Vec3::splat(f32::MIN)),
            |(l, h), p| (l.min(*p), h.max(*p)),
        );
        let (lo, hi) = (lo - pad, hi + pad);
        for k in 0..FRAMING_YAWS {
            let place = placement(
                std::f32::consts::TAU * k as f32 / FRAMING_YAWS as f32,
                facing,
                0.0,
            );
            for c in 0..8 {
                let corner = Vec3::new(
                    if c & 1 == 0 { lo.x } else { hi.x },
                    if c & 2 == 0 { lo.y } else { hi.y },
                    if c & 4 == 0 { lo.z } else { hi.z },
                );
                points.push(place.transform_point3(corner));
            }
        }
    }
    Camera::fit_points(&points)
}

/// Spin, facing and breathing. `breath_phase` is in radians.
pub fn placement(yaw: f32, facing: Quat, breath_phase: f32) -> Mat4 {
    // A slow vertical squash-and-stretch about the feet that keeps volume.
    let b = 1.0 + 0.015 * breath_phase.sin();
    Mat4::from_quat(Quat::from_rotation_y(yaw) * facing)
        * Mat4::from_scale(Vec3::new(1.0 / b.sqrt(), b, 1.0 / b.sqrt()))
}

/// How far splats reach beyond the mesh's box, at 2 sigma.
pub fn splat_reach(splats: &[Splat]) -> f32 {
    let mut reach: Vec<f32> = splats.iter().map(|s| s.overhang).collect();
    if reach.is_empty() {
        return 0.0;
    }
    // Ignore the farthest 0.5%: stray floaters shouldn't blow up the window.
    let k = (reach.len() as f32 * 0.995) as usize;
    let k = k.min(reach.len() - 1);
    *reach.select_nth_unstable_by(k, |a, b| a.total_cmp(b)).1
}
