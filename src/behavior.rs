//! What the pet is doing: which clip plays, crossfades between them, and the
//! procedural layer on top (breathing, blinking, looking at the cursor).

use glam::{Quat, Vec2, Vec3};

use crate::model::Model;
use crate::pet::Manifest;
use crate::pose::{self, Pose};

pub const CROSSFADE: f32 = 0.35;
const BLINK_SECONDS: f32 = 0.16;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum State {
    Idle,
    Fidget,
    React,
}

struct Playing {
    clip: Option<usize>,
    t: f32,
    looped: bool,
}

/// A small xorshift; behavior only needs "unpredictable enough".
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> f32 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        (self.0 >> 40) as f32 / (1u64 << 24) as f32
    }
    fn range(&mut self, lo: f32, hi: f32) -> f32 {
        lo + (hi - lo) * self.next()
    }
}

pub struct Behavior {
    idle: Option<usize>,
    fidgets: Vec<usize>,
    react: Option<usize>,
    head: Option<usize>,
    look_limit: f32,
    blink: Option<usize>,

    pub state: State,
    current: Playing,
    previous: Option<Playing>,
    fade: f32,
    until_fidget: f32,
    until_blink: f32,
    blink_t: f32,
    gaze: Vec2,
    pub breath_phase: f32,
    rng: Rng,
}

impl Behavior {
    pub fn new(model: &Model, manifest: &Manifest) -> Result<Self, String> {
        let clip = |name: &String| {
            model.clip(name).ok_or_else(|| {
                let names: Vec<&str> = model.clips.iter().map(|c| c.name.as_str()).collect();
                format!(
                    "pet.toml names animation \"{name}\", which the model lacks; clips: {}",
                    names.join(", ")
                )
            })
        };
        let a = &manifest.animations;
        let opt = |n: &Option<String>| n.as_ref().map(clip).transpose();
        let head = manifest
            .rig
            .head
            .as_ref()
            .map(|n| {
                model.node(n).ok_or_else(|| {
                    let names: Vec<&str> = model
                        .nodes
                        .iter()
                        .map(|n| n.name.as_str())
                        .filter(|n| !n.is_empty())
                        .collect();
                    format!(
                        "rig.head \"{n}\" is not a node; nodes: {}",
                        names.join(", ")
                    )
                })
            })
            .transpose()?;
        let blink = manifest
            .rig
            .blink
            .as_ref()
            .map(|n| {
                model.morph(n).ok_or_else(|| {
                    format!(
                        "rig.blink \"{n}\" is not a morph target; morphs: {}",
                        model.morph_names.join(", ")
                    )
                })
            })
            .transpose()?
            .or_else(|| model.morph(crate::eyes::BLINK_MORPH));
        let idle = opt(&a.idle)?;
        let seed = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(1, |d| d.as_nanos() as u64)
            | 1;
        let mut rng = Rng(seed);
        Ok(Self {
            idle,
            fidgets: a.fidget.iter().map(clip).collect::<Result<_, _>>()?,
            react: opt(&a.react)?,
            head,
            look_limit: manifest.rig.look_limit.to_radians(),
            blink,
            state: State::Idle,
            current: Playing {
                clip: idle,
                t: 0.0,
                looped: true,
            },
            previous: None,
            fade: 1.0,
            until_fidget: rng.range(6.0, 14.0),
            until_blink: rng.range(2.0, 5.0),
            blink_t: -1.0,
            gaze: Vec2::ZERO,
            breath_phase: 0.0,
            rng,
        })
    }

    fn play(&mut self, state: State, clip: Option<usize>, looped: bool) {
        if self.state == state && self.current.clip == clip {
            return;
        }
        self.state = state;
        let next = Playing {
            clip,
            t: 0.0,
            looped,
        };
        self.previous = Some(std::mem::replace(&mut self.current, next));
        self.fade = 0.0;
    }

    pub fn clicked(&mut self) {
        if self.react.is_some() {
            self.play(State::React, self.react, false);
        }
        // A click always gets a blink, even without a reaction clip.
        self.blink_t = 0.0;
    }

    /// Where the eyes look, -1..1 with y up.
    pub fn gaze(&self) -> Vec2 {
        self.gaze.clamp(Vec2::splat(-1.0), Vec2::splat(1.0))
    }

    /// Whether anything is moving that needs frames, beyond breathing.
    pub fn busy(&self) -> bool {
        self.state != State::Idle || self.fade < 1.0 || self.blink_t >= 0.0
    }

    /// Advances by `dt` seconds. `cursor` is the cursor relative to the pet's
    /// center in window units (-1..1 inside the window), if known. `dozing`
    /// slows breathing and stops fidgets.
    pub fn update(&mut self, model: &Model, dt: f32, cursor: Option<Vec2>, dozing: bool) {
        let duration = |p: &Playing| p.clip.map_or(0.0, |c| model.clips[c].duration);
        self.current.t += dt;
        if let Some(prev) = &mut self.previous {
            prev.t += dt;
        }
        self.fade = (self.fade + dt / CROSSFADE).min(1.0);
        if self.fade >= 1.0 {
            self.previous = None;
        }
        let d = duration(&self.current);
        if self.current.looped {
            if d > 0.0 {
                self.current.t %= d;
            }
        } else if self.current.t >= d - CROSSFADE * 0.5 {
            self.play(State::Idle, self.idle, true);
        }

        if self.state == State::Idle && !dozing && !self.fidgets.is_empty() {
            self.until_fidget -= dt;
            if self.until_fidget <= 0.0 {
                let i = (self.rng.next() * self.fidgets.len() as f32) as usize;
                self.play(
                    State::Fidget,
                    Some(self.fidgets[i.min(self.fidgets.len() - 1)]),
                    false,
                );
                self.until_fidget = self.rng.range(8.0, 20.0);
            }
        }

        if self.blink.is_some() {
            if self.blink_t >= 0.0 {
                self.blink_t += dt;
                if self.blink_t > BLINK_SECONDS {
                    self.blink_t = -1.0;
                }
            } else {
                self.until_blink -= dt;
                if self.until_blink <= 0.0 {
                    self.blink_t = 0.0;
                    // Now and then a double blink.
                    self.until_blink = if self.rng.next() < 0.15 {
                        0.3
                    } else {
                        self.rng.range(2.5, 6.0)
                    };
                }
            }
        }

        // Eyes ease towards the cursor; with no cursor, back to the camera.
        let target = cursor
            .unwrap_or(Vec2::ZERO)
            .clamp(Vec2::splat(-1.5), Vec2::splat(1.5));
        self.gaze += (target - self.gaze) * (1.0 - (-dt * 6.0).exp());

        let rate = if dozing { 1.2 } else { 2.2 };
        self.breath_phase = (self.breath_phase + dt * rate) % std::f32::consts::TAU;
    }

    /// The pose to draw. `facing` is the model's rotation into camera space.
    pub fn pose(&self, model: &Model, facing: Quat) -> Pose {
        let sample = |p: &Playing| {
            let mut pose = Pose::rest(model);
            if let Some(c) = p.clip {
                pose::sample(model, &model.clips[c], p.t, &mut pose);
            }
            pose
        };
        let mut pose = match &self.previous {
            Some(prev) => {
                let mut a = sample(prev);
                // Smoothstep so the blend starts and ends gently.
                let f = self.fade * self.fade * (3.0 - 2.0 * self.fade);
                a.blend(&sample(&self.current), f);
                a
            }
            None => sample(&self.current),
        };

        if let Some(head) = self.head {
            look(model, &mut pose, head, self.look_limit, self.gaze, facing);
        }
        if let (Some(m), Some(w)) = (self.blink, blink_weight(self.blink_t)) {
            pose.morphs[m] = pose.morphs[m].max(w);
        }
        pose
    }
}

/// Turns `head` from facing the camera towards `gaze` (-1..1, y up), at most
/// `limit` radians. `facing` is the model's rotation into camera space.
pub fn look(model: &Model, pose: &mut Pose, head: usize, limit: f32, gaze: Vec2, facing: Quat) {
    let yaw = (gaze.x * limit).clamp(-limit, limit);
    let pitch = (gaze.y * limit * 0.6).clamp(-limit, limit);
    let to_model = facing.inverse();
    let forward = to_model * Vec3::Z;
    let wanted = to_model * Vec3::new(yaw.tan(), pitch.tan(), 1.0).normalize();
    pose::rotate_world(model, pose, head, Quat::from_rotation_arc(forward, wanted));
}

/// Weight of the blink morph `t` seconds into a blink, or None outside one.
pub fn blink_weight(t: f32) -> Option<f32> {
    let t = t / BLINK_SECONDS;
    if !(0.0..=1.0).contains(&t) {
        return None;
    }
    // Close fast, open a little slower.
    let w = if t < 0.4 {
        t / 0.4
    } else {
        1.0 - (t - 0.4) / 0.6
    };
    Some(w.clamp(0.0, 1.0))
}
