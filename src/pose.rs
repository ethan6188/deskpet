//! Sampling animation clips into a pose, blending poses, and turning a pose into
//! the joint matrices and morph weights the GPU consumes.

use glam::{Mat4, Quat, Vec3};

use crate::model::{Clip, Model, Track, Trs, Values};

/// Local transform of every node plus every morph weight.
#[derive(Clone)]
pub struct Pose {
    pub local: Vec<Trs>,
    pub morphs: Vec<f32>,
}

impl Pose {
    pub fn rest(model: &Model) -> Self {
        Self {
            local: model.nodes.iter().map(|n| n.rest).collect(),
            morphs: vec![0.0; model.morph_names.len()],
        }
    }

    /// Blends `other` into `self` by `w` in 0..=1.
    pub fn blend(&mut self, other: &Pose, w: f32) {
        for (a, b) in self.local.iter_mut().zip(&other.local) {
            a.t = a.t.lerp(b.t, w);
            a.r = a.r.slerp(b.r, w);
            a.s = a.s.lerp(b.s, w);
        }
        for (a, b) in self.morphs.iter_mut().zip(&other.morphs) {
            *a += (b - *a) * w;
        }
    }
}

/// Index of the key at or before `t`, and how far `t` is towards the next key.
fn key(times: &[f32], t: f32) -> (usize, usize, f32) {
    let last = times.len() - 1;
    if t <= times[0] {
        return (0, 0, 0.0);
    }
    if t >= times[last] {
        return (last, last, 0.0);
    }
    let i = times.partition_point(|&k| k <= t) - 1;
    let span = times[i + 1] - times[i];
    (
        i,
        i + 1,
        if span > 0.0 {
            (t - times[i]) / span
        } else {
            0.0
        },
    )
}

fn sample_track(track: &Track, t: f32, model: &Model, pose: &mut Pose) {
    if track.times.is_empty() {
        return;
    }
    let (a, b, mut f) = key(&track.times, t);
    if track.step {
        f = 0.0;
    }
    let local = &mut pose.local[track.node];
    match &track.values {
        Values::Translation(v) => local.t = v[a].lerp(v[b], f),
        Values::Rotation(v) => local.r = v[a].slerp(v[b], f).normalize(),
        Values::Scale(v) => local.s = v[a].lerp(v[b], f),
        Values::Weights { width, values } => {
            for (k, g) in model.node_morphs[track.node]
                .iter()
                .enumerate()
                .take(*width)
            {
                if let Some(g) = g {
                    let wa = values[a * width + k];
                    let wb = values[b * width + k];
                    pose.morphs[*g] = wa + (wb - wa) * f;
                }
            }
        }
    }
}

/// Samples `clip` at `t` seconds over the rest pose.
pub fn sample(model: &Model, clip: &Clip, t: f32, pose: &mut Pose) {
    for track in &clip.tracks {
        sample_track(track, t, model, pose);
    }
}

/// Adds a rotation, given in world space, on top of a node's animated rotation.
pub fn rotate_world(model: &Model, pose: &mut Pose, node: usize, world_rot: Quat) {
    let parent_world = world_matrices(model, pose)
        .get(model.nodes[node].parent.unwrap_or(usize::MAX))
        .copied()
        .unwrap_or(Mat4::IDENTITY);
    let (_, parent_rot, _) = parent_world.to_scale_rotation_translation();
    let local = &mut pose.local[node];
    local.r = (parent_rot.inverse() * world_rot * parent_rot * local.r).normalize();
}

pub fn world_matrices(model: &Model, pose: &Pose) -> Vec<Mat4> {
    let mut world = vec![Mat4::IDENTITY; model.nodes.len()];
    for &n in &model.order {
        let local = pose.local[n].matrix();
        world[n] = match model.nodes[n].parent {
            Some(p) => world[p] * local,
            None => local,
        };
    }
    world
}

/// Joint matrices for the vertex shader, in model space.
pub fn joint_matrices(model: &Model, pose: &Pose) -> Vec<Mat4> {
    let world = world_matrices(model, pose);
    model
        .palette
        .iter()
        .map(|e| world[e.node] * e.inverse_bind)
        .collect()
}

/// Model-space position of every vertex in `pose`, on the CPU. Used for bounds;
/// the renderer does the same on the GPU.
pub fn skinned_positions(model: &Model, pose: &Pose) -> Vec<Vec3> {
    let joints = joint_matrices(model, pose);
    let n = model.vertices.len();
    model
        .vertices
        .iter()
        .enumerate()
        .map(|(i, v)| {
            let mut p = Vec3::from(v.position);
            for (m, w) in pose.morphs.iter().enumerate() {
                if *w != 0.0 {
                    let d = model.morph_deltas[m * n + i];
                    p += Vec3::new(d[0], d[1], d[2]) * *w;
                }
            }
            let mut out = Vec3::ZERO;
            for k in 0..4 {
                out += joints[v.joints[k] as usize].transform_point3(p) * v.weights[k];
            }
            out
        })
        .collect()
}
