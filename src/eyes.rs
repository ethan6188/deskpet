//! Procedural eyes. Many models (like the Khronos fox) have painted or no
//! eyes, and a pet without eyes doesn't feel alive. This adds two eyeballs on
//! the head joint: the shader draws iris, pupil and catchlights on them, the
//! iris follows the cursor, and a generated morph target blinks them.

use glam::Vec3;

use crate::model::{Material, Model, PaletteEntry, Primitive, Vertex};
use crate::pet::Eyes;
use crate::pose::{self, Pose};

/// Morph target added for blinking; used when `rig.blink` is not set.
pub const BLINK_MORPH: &str = "deskpet_blink";

const RINGS: u32 = 16;
const SEGMENTS: u32 = 24;
/// Height, as a fraction of the eye, of the line the lids close on.
const LID_LINE: f32 = 0.15;

/// Adds both eyes to `model`, bound to `head`.
pub fn add(model: &mut Model, eyes: &Eyes, head: usize) {
    let rest = Pose::rest(model);
    let world = pose::world_matrices(model, &rest);
    // A palette entry whose rest skin matrix is identity: eye vertices are
    // written in rest-pose model space and follow the head from there.
    let joint = model.palette.len() as u32;
    model.palette.push(PaletteEntry {
        node: head,
        inverse_bind: world[head].inverse(),
    });
    let material = model.materials.len();
    model.materials.push(Material {
        base_color: [eyes.iris[0], eyes.iris[1], eyes.iris[2], 1.0],
        texture: None,
        eye: Some([eyes.iris_size, eyes.pupil, eyes.pupil_aspect, 0.0]),
    });

    let old_count = model.vertices.len();
    let mut blink: Vec<[f32; 4]> = Vec::new();
    let first_index = model.indices.len() as u32;
    for side in [1.0f32, -1.0] {
        let center = Vec3::new(eyes.center[0] * side, eyes.center[1], eyes.center[2]);
        let forward =
            Vec3::new(eyes.forward[0] * side, eyes.forward[1], eyes.forward[2]).normalize();
        // Both eyes share one "right", so the iris shifts the same way in each.
        let right = Vec3::Y.cross(forward).normalize() * if forward.z >= 0.0 { 1.0 } else { -1.0 };
        let right = if right.x < 0.0 { -right } else { right };
        let up = forward.cross(right).normalize();
        let up = if up.y < 0.0 { -up } else { up };
        let radii = Vec3::new(eyes.radius, eyes.radius * eyes.height, eyes.radius * 0.55);
        let base = model.vertices.len() as u32;
        for r in 0..=RINGS {
            let theta = std::f32::consts::PI * r as f32 / RINGS as f32;
            for s in 0..=SEGMENTS {
                let phi = std::f32::consts::TAU * s as f32 / SEGMENTS as f32;
                // Unit sphere with +z looking out of the head.
                let (a, b, c) = (
                    theta.sin() * phi.cos(),
                    theta.sin() * phi.sin(),
                    theta.cos(),
                );
                let local = Vec3::new(a, b, c) * radii;
                let position = center + right * local.x + up * local.y + forward * local.z;
                let n = (right * (a / radii.x) + up * (b / radii.y) + forward * (c / radii.z))
                    .normalize();
                // The shader reads eye-disk coordinates from the UVs; the back is never seen.
                let uv = if c > 0.0 { [a, b] } else { [0.0, -2.0] };
                model.vertices.push(Vertex {
                    position: position.to_array(),
                    normal: n.to_array(),
                    smooth_normal: n.to_array(),
                    uv,
                    joints: [joint; 4],
                    weights: [1.0, 0.0, 0.0, 0.0],
                });
                // Closed: every point squashed onto the lid line, slightly sunk.
                let closed_b = LID_LINE + (b - LID_LINE) * 0.06;
                let d = up * (closed_b - b) * radii.y - forward * local.z * 0.15;
                blink.push([d.x, d.y, d.z, 0.0]);
            }
        }
        let row = SEGMENTS + 1;
        for r in 0..RINGS {
            for s in 0..SEGMENTS {
                let a = base + r * row + s;
                let b = a + row;
                model
                    .indices
                    .extend_from_slice(&[a, b, a + 1, a + 1, b, b + 1]);
            }
        }
    }
    model.primitives.push(Primitive {
        first_index,
        index_count: model.indices.len() as u32 - first_index,
        material,
    });

    // Morph deltas are laid out [target][vertex]: re-lay them for the new count.
    let new_count = model.vertices.len();
    let targets = model.morph_names.len();
    let mut deltas = vec![[0.0; 4]; (targets + 1) * new_count];
    for t in 0..targets {
        for v in 0..old_count {
            deltas[t * new_count + v] = model.morph_deltas[t * old_count + v];
        }
    }
    for (i, d) in blink.into_iter().enumerate() {
        deltas[targets * new_count + old_count + i] = d;
    }
    model.morph_deltas = deltas;
    model.morph_names.push(BLINK_MORPH.into());
}
