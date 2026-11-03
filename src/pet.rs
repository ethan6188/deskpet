//! Pet packages: a directory with `pet.toml`, a glTF model and its license.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::Deserialize;

#[derive(Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum StyleKind {
    /// Physically based shading with soft fur-like diffuse and shell fur.
    Real,
    /// Banded cel shading with ink outlines.
    Anime,
    /// 3D Gaussian splats bound to the model's skeleton.
    Splat,
}

#[derive(Deserialize, Clone, Debug)]
#[serde(deny_unknown_fields)]
pub struct Style {
    pub kind: StyleKind,
    /// Multiplies the base color texture.
    #[serde(default = "white")]
    pub tint: [f32; 3],
    /// Ink outline width as a fraction of the pet's height (anime).
    #[serde(default = "default_outline")]
    pub outline: f32,
    #[serde(default = "black")]
    pub outline_color: [f32; 3],
    /// Fur length as a fraction of the pet's height; 0 turns fur off (real).
    #[serde(default)]
    pub fur: f32,
    /// Rim light color.
    #[serde(default = "default_glow")]
    pub glow: [f32; 3],
    /// Number of cel bands (anime).
    #[serde(default = "default_bands")]
    pub bands: u32,
    /// Splat file (binary PLY), relative to the pet directory (splat).
    pub splats: Option<PathBuf>,
    /// Scale, rotation (degrees about X, Y, Z) and offset that put the splats
    /// over the model's rest pose (splat).
    #[serde(default = "one")]
    pub splat_scale: f32,
    #[serde(default)]
    pub splat_rotate: [f32; 3],
    #[serde(default)]
    pub splat_offset: [f32; 3],
}

fn one() -> f32 {
    1.0
}

fn white() -> [f32; 3] {
    [1.0; 3]
}
fn black() -> [f32; 3] {
    [0.08, 0.05, 0.05]
}
fn default_outline() -> f32 {
    0.006
}
fn default_glow() -> [f32; 3] {
    [0.3, 0.9, 1.0]
}
fn default_bands() -> u32 {
    3
}

/// Which clip plays in which state. Names are the glTF animation names.
#[derive(Deserialize, Clone, Debug, Default)]
#[serde(deny_unknown_fields)]
pub struct Animations {
    pub idle: Option<String>,
    /// Played now and then while idle, then back to `idle`.
    #[serde(default)]
    pub fidget: Vec<String>,
    /// Played when the pet is clicked.
    pub react: Option<String>,
}

/// Procedural motion layered on top of the clips.
#[derive(Deserialize, Clone, Debug, Default)]
#[serde(deny_unknown_fields)]
pub struct Rig {
    /// Node that turns to look at the cursor.
    pub head: Option<String>,
    /// How far the head may turn, in degrees.
    #[serde(default = "default_look")]
    pub look_limit: f32,
    /// Morph target that closes the eyes; drives blinking.
    pub blink: Option<String>,
}

fn default_look() -> f32 {
    35.0
}

#[derive(Deserialize, Clone, Debug)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    pub name: String,
    pub model: PathBuf,
    /// Rotation about Y, in degrees, that turns the model to face the camera.
    #[serde(default)]
    pub facing: f32,
    /// Window height in points.
    #[serde(default = "default_height")]
    pub height: f32,
    #[serde(default)]
    pub animations: Animations,
    #[serde(default)]
    pub rig: Rig,
    /// Procedural eyes on `rig.head`, for models whose eyes are missing or flat.
    pub eyes: Option<Eyes>,
    /// Name of the style used when none is asked for.
    pub style: String,
    /// Every style this pet supports, by name.
    pub styles: BTreeMap<String, Style>,
}

/// Two procedural eyes, mirrored across x = 0, in the rest pose's model space.
#[derive(Deserialize, Clone, Debug)]
#[serde(deny_unknown_fields)]
pub struct Eyes {
    /// Center of the eye on the +x side.
    pub center: [f32; 3],
    /// Direction that eye looks out of the head.
    #[serde(default = "forward")]
    pub forward: [f32; 3],
    pub radius: f32,
    /// Height as a fraction of width: below 1 for almond eyes.
    #[serde(default = "eye_height")]
    pub height: f32,
    #[serde(default = "amber")]
    pub iris: [f32; 3],
    /// Iris radius as a fraction of the eye.
    #[serde(default = "iris_size")]
    pub iris_size: f32,
    /// Pupil radius as a fraction of the iris.
    #[serde(default = "pupil")]
    pub pupil: f32,
    /// Pupil width over height: 1 is round, small values are slits.
    #[serde(default = "one")]
    pub pupil_aspect: f32,
}

fn forward() -> [f32; 3] {
    [0.0, 0.0, 1.0]
}
fn eye_height() -> f32 {
    0.8
}
fn amber() -> [f32; 3] {
    [0.85, 0.52, 0.12]
}
fn iris_size() -> f32 {
    0.82
}
fn pupil() -> f32 {
    0.42
}

fn default_height() -> f32 {
    280.0
}

pub struct Pet {
    pub dir: PathBuf,
    pub manifest: Manifest,
}

impl Pet {
    pub fn open(dir: &Path) -> Result<Self, String> {
        let file = dir.join("pet.toml");
        let text =
            std::fs::read_to_string(&file).map_err(|e| format!("{}: {e}", file.display()))?;
        let manifest: Manifest =
            toml::from_str(&text).map_err(|e| format!("{}: {e}", file.display()))?;
        if !manifest.styles.contains_key(&manifest.style) {
            return Err(format!(
                "{}: style \"{}\" is not one of [styles]",
                file.display(),
                manifest.style
            ));
        }
        Ok(Self {
            dir: dir.to_path_buf(),
            manifest,
        })
    }

    /// The named style, or the default one.
    pub fn style(&self, name: Option<&str>) -> Result<Style, String> {
        let name = name.unwrap_or(&self.manifest.style);
        self.manifest.styles.get(name).cloned().ok_or_else(|| {
            let known: Vec<_> = self.manifest.styles.keys().map(String::as_str).collect();
            format!("no style \"{name}\"; this pet has {}", known.join(", "))
        })
    }

    /// The model with everything the manifest adds to it, such as eyes.
    pub fn load_model(&self) -> Result<crate::model::Model, String> {
        let mut model = crate::model::load(&self.model_path())?;
        if let Some(eyes) = &self.manifest.eyes {
            let head = self
                .manifest
                .rig
                .head
                .as_deref()
                .ok_or("[eyes] needs rig.head")?;
            let node = model
                .node(head)
                .ok_or_else(|| format!("rig.head \"{head}\" is not a node"))?;
            crate::eyes::add(&mut model, eyes, node);
        }
        Ok(model)
    }

    pub fn model_path(&self) -> PathBuf {
        self.dir.join(&self.manifest.model)
    }
}
