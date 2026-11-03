//! Asset checks: the mistakes eyes miss on a contact sheet. Bad names, broken
//! skin weights, loops with a seam, too many triangles, frames over budget.

use std::path::Path;
use std::time::{Duration, Instant};

use glam::Quat;

use crate::behavior::{self, Behavior};
use crate::hit;
use crate::model::{self, Model};
use crate::pet::Pet;
use crate::pose::{self, Pose};
use crate::renderer::{FrameInput, Renderer};
use crate::scene;
use crate::splat::{self, Splat};

/// Triangles above which a pet is too heavy for an all-day desktop pet.
const TRIANGLES_WARN: usize = 60_000;
const TRIANGLES_ERROR: usize = 250_000;
const TEXTURE_WARN: u32 = 4096;
/// GPU time per frame at Retina size; the pet shares the GPU with everything else.
const FRAME_WARN: Duration = Duration::from_millis(4);
const FRAME_ERROR: Duration = Duration::from_millis(12);
/// The click-through test runs ten times a second on the CPU.
const HIT_WARN: Duration = Duration::from_millis(3);
/// Largest vertex jump across a loop's seam, as a fraction of the pet's height.
const SEAM_WARN: f32 = 0.02;
/// Largest jump between the idle start and another clip's start before the
/// crossfade can no longer hide it.
const ENTRY_WARN: f32 = 0.25;
/// Splats the CPU can sort per frame within budget on a laptop.
const SPLATS_WARN: usize = 300_000;
const SPLATS_ERROR: usize = 1_500_000;
/// A splat bound farther than this (fraction of height) from the mesh is
/// probably background or a floater, and won't follow the skeleton well.
const BIND_WARN: f32 = 0.05;

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub enum Level {
    Ok,
    Warn,
    Error,
}

pub struct Finding {
    pub level: Level,
    pub check: &'static str,
    pub message: String,
}

pub struct Findings(pub Vec<Finding>);

impl Findings {
    fn add(&mut self, level: Level, check: &'static str, message: impl Into<String>) {
        self.0.push(Finding {
            level,
            check,
            message: message.into(),
        });
    }

    pub fn worst(&self) -> Level {
        self.0.iter().map(|f| f.level).max().unwrap_or(Level::Ok)
    }

    pub fn to_json(&self) -> String {
        let items: Vec<serde_json::Value> = self
            .0
            .iter()
            .map(|f| {
                serde_json::json!({
                    "level": match f.level { Level::Ok => "ok", Level::Warn => "warn", Level::Error => "error" },
                    "check": f.check,
                    "message": f.message,
                })
            })
            .collect();
        serde_json::to_string_pretty(&serde_json::json!({
            "worst": match self.worst() { Level::Ok => "ok", Level::Warn => "warn", Level::Error => "error" },
            "findings": items,
        }))
        .unwrap_or_default()
    }
}

fn max_jump(a: &[glam::Vec3], b: &[glam::Vec3]) -> f32 {
    a.iter()
        .zip(b)
        .map(|(p, q)| p.distance(*q))
        .fold(0.0, f32::max)
}

pub fn run(dir: &Path, window_scale: f32) -> Findings {
    let mut f = Findings(Vec::new());
    let pet = match Pet::open(dir) {
        Ok(p) => p,
        Err(e) => {
            f.add(Level::Error, "manifest", e);
            return f;
        }
    };
    f.add(
        Level::Ok,
        "manifest",
        format!("{} parses", dir.join("pet.toml").display()),
    );
    let license = ["LICENSE.md", "LICENSE", "LICENSE.txt"]
        .iter()
        .find_map(|n| std::fs::read_to_string(dir.join(n)).ok());
    if license.as_deref().is_some_and(|t| t.contains("TODO")) {
        f.add(
            Level::Warn,
            "license",
            "LICENSE.md still has TODOs: fill in the model's source and terms",
        );
    } else if license.is_some() {
        f.add(Level::Ok, "license", "license file present");
    } else {
        f.add(
            Level::Warn,
            "license",
            "no LICENSE.md: record where the model came from and its terms",
        );
    }

    let model = match pet.load_model() {
        Ok(m) => m,
        Err(e) => {
            f.add(Level::Error, "model", e);
            return f;
        }
    };
    let tris = model.indices.len() / 3;
    // Rest-pose box: what splat_scale, splat_rotate and splat_offset align to.
    let rest = pose::skinned_positions(&model, &Pose::rest(&model));
    let (lo, hi) = rest.iter().fold(
        (glam::Vec3::splat(f32::MAX), glam::Vec3::splat(f32::MIN)),
        |(l, h), p| (l.min(*p), h.max(*p)),
    );
    let head = pet
        .manifest
        .rig
        .head
        .as_deref()
        .and_then(|n| model.node(n))
        .map(|n| {
            pose::world_matrices(&model, &Pose::rest(&model))[n]
                .w_axis
                .truncate()
        });
    f.add(
        Level::Ok,
        "rest pose",
        format!(
            "box x {:.2}..{:.2}, y {:.2}..{:.2}, z {:.2}..{:.2}{}",
            lo.x,
            hi.x,
            lo.y,
            hi.y,
            lo.z,
            hi.z,
            head.map_or(String::new(), |h| format!(
                "; head at ({:.2}, {:.2}, {:.2})",
                h.x, h.y, h.z
            ))
        ),
    );
    f.add(
        Level::Ok,
        "model",
        format!(
            "{} vertices, {tris} triangles, {} joints, {} morphs, {} clips",
            model.vertices.len(),
            model.palette.len(),
            model.morph_names.len(),
            model.clips.len()
        ),
    );
    if tris > TRIANGLES_ERROR {
        f.add(
            Level::Error,
            "budget",
            format!("{tris} triangles; keep it under {TRIANGLES_ERROR}"),
        );
    } else if tris > TRIANGLES_WARN {
        f.add(
            Level::Warn,
            "budget",
            format!("{tris} triangles; under {TRIANGLES_WARN} keeps power low"),
        );
    }
    if model.dropped_morphs > 0 {
        f.add(
            Level::Warn,
            "morphs",
            format!(
                "{} morph targets beyond {} were dropped",
                model.dropped_morphs,
                model::MAX_MORPHS
            ),
        );
    }
    for (i, t) in model.textures.iter().enumerate() {
        if t.width.max(t.height) > TEXTURE_WARN {
            f.add(
                Level::Warn,
                "textures",
                format!(
                    "texture {i} is {}x{}; {TEXTURE_WARN} is plenty",
                    t.width, t.height
                ),
            );
        }
    }
    check_geometry(&model, &mut f);

    if let Err(e) = Behavior::new(&model, &pet.manifest) {
        f.add(Level::Error, "names", e);
    } else {
        f.add(
            Level::Ok,
            "names",
            "every animation, head and blink name exists",
        );
    }
    let a = &pet.manifest.animations;
    if a.idle.is_none() {
        f.add(
            Level::Warn,
            "animations",
            "no idle clip: the pet will only breathe",
        );
    }
    if pet.manifest.rig.head.is_none() {
        f.add(
            Level::Warn,
            "rig",
            "no rig.head: the pet won't look at the cursor",
        );
    }
    if pet.manifest.rig.blink.is_none() && pet.manifest.eyes.is_none() {
        f.add(
            Level::Warn,
            "rig",
            "no rig.blink: the pet won't blink (needs an eyes-closed morph target)",
        );
    }
    check_motion(&pet, &model, &mut f);
    check_cost(&pet, &model, window_scale, &mut f);
    f
}

fn check_geometry(model: &Model, f: &mut Findings) {
    let bad_pos = model
        .vertices
        .iter()
        .filter(|v| v.position.iter().chain(&v.normal).any(|c| !c.is_finite()))
        .count();
    if bad_pos > 0 {
        f.add(
            Level::Error,
            "geometry",
            format!("{bad_pos} vertices have NaN or infinite positions or normals"),
        );
    }
    let joints = model.palette.len() as u32;
    let mut bad_weights = 0;
    let mut bad_joints = 0;
    for v in &model.vertices {
        let sum: f32 = v.weights.iter().sum();
        if (sum - 1.0).abs() > 0.01 {
            bad_weights += 1;
        }
        if v.joints
            .iter()
            .zip(&v.weights)
            .any(|(j, w)| *w > 0.0 && *j >= joints)
        {
            bad_joints += 1;
        }
    }
    if bad_joints > 0 {
        f.add(
            Level::Error,
            "skin",
            format!("{bad_joints} vertices use a joint the skin doesn't have"),
        );
    }
    if bad_weights > 0 {
        f.add(Level::Warn, "skin", format!("{bad_weights} vertices have skin weights that don't sum to 1; they will shrink or swell"));
    }
    if bad_pos == 0 && bad_joints == 0 && bad_weights == 0 {
        f.add(
            Level::Ok,
            "geometry",
            "finite geometry, skin weights sum to 1",
        );
    }
}

fn check_motion(pet: &Pet, model: &Model, f: &mut Findings) {
    let height = scene::model_height(model);
    let at = |clip: usize, t: f32| {
        let mut p = Pose::rest(model);
        pose::sample(model, &model.clips[clip], t, &mut p);
        pose::skinned_positions(model, &p)
    };
    let a = &pet.manifest.animations;
    if let Some(idle) = a.idle.as_deref().and_then(|n| model.clip(n)) {
        let d = model.clips[idle].duration;
        let seam = max_jump(&at(idle, 0.0), &at(idle, d)) / height;
        if seam > SEAM_WARN {
            f.add(
                Level::Warn,
                "loop",
                format!(
                    "idle \"{}\" has a seam: the last frame is {:.1}% of the pet's height from the first",
                    model.clips[idle].name,
                    seam * 100.0
                ),
            );
        } else {
            f.add(
                Level::Ok,
                "loop",
                format!("idle loops cleanly (seam {:.2}%)", seam * 100.0),
            );
        }
        let start = at(idle, 0.0);
        let others = a.fidget.iter().chain(a.react.iter());
        for name in others {
            let Some(c) = model.clip(name) else { continue };
            let jump = max_jump(&start, &at(c, 0.0)) / height;
            if jump > ENTRY_WARN {
                f.add(
                    Level::Warn,
                    "transitions",
                    format!(
                        "\"{name}\" starts {:.0}% of the pet's height away from idle; the {:.2}s crossfade will look like sliding",
                        jump * 100.0,
                        behavior::CROSSFADE
                    ),
                );
            }
        }
    }
    let empty: Vec<&str> = model
        .clips
        .iter()
        .filter(|c| c.duration <= 0.0 || c.tracks.is_empty())
        .map(|c| c.name.as_str())
        .collect();
    if !empty.is_empty() {
        f.add(
            Level::Warn,
            "animations",
            format!("clips with no motion: {}", empty.join(", ")),
        );
    }
}

fn check_cost(pet: &Pet, model: &Model, window_scale: f32, f: &mut Findings) {
    let facing = Quat::from_rotation_y(pet.manifest.facing.to_radians());
    let height = scene::model_height(model);
    let rest = Pose::rest(model);

    // Click-through: skin on the CPU and cast one ray, as the app does every 100 ms.
    let started = Instant::now();
    let positions = pose::skinned_positions(model, &rest);
    let _ = hit::ray_mesh(
        glam::Vec3::new(0.0, height * 0.5, height * 10.0),
        glam::Vec3::NEG_Z,
        &positions,
        &model.indices,
        height * 0.01,
    );
    let hit_time = started.elapsed();
    if hit_time > HIT_WARN {
        f.add(
            Level::Warn,
            "budget",
            format!("click-through test takes {hit_time:.1?}; the app runs it 10 times a second"),
        );
    }

    for (name, style) in &pet.manifest.styles {
        let splats = match splat::for_style(&pet.dir, model, style) {
            Ok(s) => s,
            Err(e) => {
                f.add(Level::Error, "splats", format!("style \"{name}\": {e}"));
                continue;
            }
        };
        if !splats.is_empty() {
            check_splats(name, &splats, height, edge_p95(model), f);
            check_splat_rig(pet, model, name, &splats, f);
        }
        let camera = scene::framing(model, style, facing, scene::splat_reach(&splats));
        let h = (pet.manifest.height * window_scale).round() as u32;
        let w = (h as f32 * camera.aspect).round() as u32;
        let mut r = match pollster::block_on(Renderer::offscreen(
            w,
            h,
            model,
            style.clone(),
            &splats,
            camera,
            height,
        )) {
            Ok(r) => r,
            Err(e) => {
                f.add(Level::Error, "render", e);
                return;
            }
        };
        let joints = pose::joint_matrices(model, &rest);
        let input = FrameInput {
            model: scene::placement(0.0, facing, 0.0),
            joints: &joints,
            morphs: &rest.morphs,
            time: 0.0,
            gaze: glam::Vec2::ZERO,
        };
        let mut times = Vec::new();
        let mut coverage = 0.0;
        for i in 0..8 {
            match r.capture(&input) {
                Ok((px, t)) => {
                    // The first frame pays for pipeline warm-up.
                    if i > 0 {
                        times.push(t);
                    }
                    coverage = px.as_chunks::<4>().0.iter().filter(|p| p[3] > 0).count() as f32
                        / (w * h) as f32;
                }
                Err(e) => {
                    f.add(Level::Error, "render", e);
                    return;
                }
            }
        }
        times.sort();
        let median = times[times.len() / 2];
        let level = if median > FRAME_ERROR {
            Level::Error
        } else if median > FRAME_WARN {
            Level::Warn
        } else {
            Level::Ok
        };
        f.add(
            level,
            "budget",
            format!(
                "style \"{name}\": {median:.2?} per frame at {w}x{h} (includes readback), pet covers {:.0}% of the window",
                coverage * 100.0
            ),
        );
        if coverage < 0.02 {
            f.add(
                Level::Error,
                "render",
                format!("style \"{name}\" draws almost nothing: check facing, scale and materials"),
            );
        } else if coverage < 0.15 {
            f.add(Level::Warn, "framing", format!("style \"{name}\": the pet fills only {:.0}% of its window; a clip may wander far", coverage * 100.0));
        }
    }
}

fn check_splats(name: &str, splats: &[Splat], height: f32, edge: f32, f: &mut Findings) {
    let n = splats.len();
    let level = if n > SPLATS_ERROR {
        Level::Error
    } else if n > SPLATS_WARN {
        Level::Warn
    } else {
        Level::Ok
    };
    let gpu: Vec<_> = splats.iter().map(Splat::gpu).collect();
    let joints = splats.iter().flat_map(|s| s.joints).max().unwrap_or(0) as usize + 1;
    let identity = vec![glam::Mat4::IDENTITY; joints];
    let (mut order, mut keys) = (Vec::new(), Vec::new());
    let started = Instant::now();
    for _ in 0..5 {
        splat::sort_order(&gpu, &identity, glam::Mat4::IDENTITY, &mut order, &mut keys);
    }
    let sort = started.elapsed() / 5;
    f.add(
        level,
        "splats",
        format!("style \"{name}\": {n} splats, CPU depth sort {sort:.2?} per frame"),
    );
    // Splats bind to the nearest vertex, so on a coarse mesh even surface
    // splats sit up to about an edge length away.
    let limit = (BIND_WARN * height).max(edge);
    let far = splats.iter().filter(|s| s.bind_distance > limit).count();
    let share = far as f32 / n as f32;
    if share > 0.01 {
        f.add(
            Level::Warn,
            "splats",
            format!(
                "style \"{name}\": {:.1}% of splats sit more than {:.0}% of the height from the mesh; check splat_scale, splat_rotate and splat_offset, or crop the background",
                share * 100.0,
                BIND_WARN * 100.0
            ),
        );
    } else {
        f.add(
            Level::Ok,
            "splats",
            format!("style \"{name}\": splats line up with the mesh"),
        );
    }
}

/// 95th-percentile triangle edge length in the rest pose: a splat on a big
/// triangle can sit that far from every vertex.
fn edge_p95(model: &Model) -> f32 {
    let p = pose::skinned_positions(model, &Pose::rest(model));
    let mut e: Vec<f32> = model
        .indices
        .as_chunks::<3>()
        .0
        .iter()
        .flat_map(|t| [(t[0], t[1]), (t[1], t[2]), (t[2], t[0])])
        .map(|(a, b)| p[a as usize].distance(p[b as usize]))
        .collect();
    if e.is_empty() {
        return 0.0;
    }
    let k = e.len() * 95 / 100;
    *e.select_nth_unstable_by(k, |a, b| a.total_cmp(b)).1
}

/// Share of splats that follow the head. Near zero means the splats are
/// misaligned: the head will turn and the photo won't.
fn check_splat_rig(pet: &Pet, model: &Model, name: &str, splats: &[Splat], f: &mut Findings) {
    let Some(head) = pet.manifest.rig.head.as_deref().and_then(|n| model.node(n)) else {
        return;
    };
    let under_head = |mut n: usize| loop {
        if n == head {
            return true;
        }
        match model.nodes[n].parent {
            Some(p) => n = p,
            None => return false,
        }
    };
    let on_head = splats
        .iter()
        .filter(|s| {
            let k = (0..4)
                .max_by(|a, b| s.weights[*a].total_cmp(&s.weights[*b]))
                .unwrap_or(0);
            under_head(model.palette[s.joints[k] as usize].node)
        })
        .count();
    let share = on_head as f32 / splats.len().max(1) as f32;
    let mesh = model
        .vertices
        .iter()
        .filter(|v| {
            let k = (0..4)
                .max_by(|a, b| v.weights[*a].total_cmp(&v.weights[*b]))
                .unwrap_or(0);
            under_head(model.palette[v.joints[k] as usize].node)
        })
        .count() as f32
        / model.vertices.len().max(1) as f32;
    let level = if share < mesh * 0.3 {
        Level::Warn
    } else {
        Level::Ok
    };
    f.add(
        level,
        "splats",
        format!(
            "style \"{name}\": {:.1}% of splats follow the head (mesh: {:.1}%){}",
            share * 100.0,
            mesh * 100.0,
            if level == Level::Warn {
                "; the head is probably misaligned"
            } else {
                ""
            }
        ),
    );
}
