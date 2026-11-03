//! glTF loading: geometry, materials, skeleton, morph targets and animation clips.

use std::collections::HashMap;
use std::path::Path;

use bytemuck::{Pod, Zeroable};
use glam::{Mat4, Quat, Vec3};
use gltf::animation::Interpolation;
use gltf::animation::util::ReadOutputs;

/// Morph targets a model may have in total; the vertex shader blends them all.
pub const MAX_MORPHS: usize = 32;

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
pub struct Vertex {
    pub position: [f32; 3],
    pub normal: [f32; 3],
    /// The normal averaged over every vertex at the same position. Outlines and
    /// fur extrude along it, so hard edges don't open up cracks.
    pub smooth_normal: [f32; 3],
    pub uv: [f32; 2],
    pub joints: [u32; 4],
    pub weights: [f32; 4],
}

pub struct Texture {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

pub struct Material {
    pub base_color: [f32; 4],
    pub texture: Option<usize>,
    /// Procedural eye: iris size, pupil size, pupil aspect. Base color is the iris.
    pub eye: Option<[f32; 4]>,
}

pub struct Primitive {
    pub first_index: u32,
    pub index_count: u32,
    pub material: usize,
}

#[derive(Clone, Copy)]
pub struct Trs {
    pub t: Vec3,
    pub r: Quat,
    pub s: Vec3,
}

impl Trs {
    pub fn matrix(&self) -> Mat4 {
        Mat4::from_scale_rotation_translation(self.s, self.r, self.t)
    }
}

pub struct Node {
    pub name: String,
    pub parent: Option<usize>,
    pub rest: Trs,
}

/// One entry of the joint palette the vertex shader indexes.
pub struct PaletteEntry {
    pub node: usize,
    pub inverse_bind: Mat4,
}

pub enum Values {
    Translation(Vec<Vec3>),
    Rotation(Vec<Quat>),
    Scale(Vec<Vec3>),
    /// `width` weights per key, mapped to global morph indices by `Model::node_morphs`.
    Weights {
        width: usize,
        values: Vec<f32>,
    },
}

pub struct Track {
    pub node: usize,
    pub times: Vec<f32>,
    pub step: bool,
    pub values: Values,
}

pub struct Clip {
    pub name: String,
    pub duration: f32,
    pub tracks: Vec<Track>,
}

pub struct Model {
    pub vertices: Vec<Vertex>,
    pub indices: Vec<u32>,
    pub primitives: Vec<Primitive>,
    pub materials: Vec<Material>,
    pub textures: Vec<Texture>,
    pub nodes: Vec<Node>,
    /// Scene nodes, every parent before its children.
    pub order: Vec<usize>,
    pub palette: Vec<PaletteEntry>,
    pub morph_names: Vec<String>,
    /// Position deltas laid out `[target][vertex]`.
    pub morph_deltas: Vec<[f32; 4]>,
    /// For each node with a mesh, the global morph index of each of its targets.
    pub node_morphs: Vec<Vec<Option<usize>>>,
    pub clips: Vec<Clip>,
    /// Morph targets beyond `MAX_MORPHS` that were left out.
    pub dropped_morphs: usize,
}

impl Model {
    pub fn clip(&self, name: &str) -> Option<usize> {
        self.clips.iter().position(|c| c.name == name)
    }

    pub fn node(&self, name: &str) -> Option<usize> {
        self.nodes.iter().position(|n| n.name == name)
    }

    pub fn morph(&self, name: &str) -> Option<usize> {
        self.morph_names.iter().position(|n| n == name)
    }
}

fn to_rgba(img: &gltf::image::Data) -> Result<Texture, String> {
    use gltf::image::Format;
    let px = &img.pixels;
    let rgba = match img.format {
        Format::R8G8B8A8 => px.clone(),
        Format::R8G8B8 => px.chunks(3).flat_map(|c| [c[0], c[1], c[2], 255]).collect(),
        Format::R8G8 => px
            .chunks(2)
            .flat_map(|c| [c[0], c[0], c[0], c[1]])
            .collect(),
        Format::R8 => px.iter().flat_map(|&c| [c, c, c, 255]).collect(),
        other => return Err(format!("unsupported texture format {other:?}")),
    };
    Ok(Texture {
        width: img.width,
        height: img.height,
        rgba,
    })
}

/// Area-weighted vertex normals, for models exported without them.
fn compute_normals(positions: &[[f32; 3]], indices: &[u32]) -> Vec<[f32; 3]> {
    let mut acc = vec![Vec3::ZERO; positions.len()];
    for tri in indices.as_chunks::<3>().0 {
        let [a, b, c] = [0, 1, 2].map(|i| Vec3::from(positions[tri[i] as usize]));
        let n = (b - a).cross(c - a);
        for &i in tri {
            acc[i as usize] += n;
        }
    }
    acc.iter()
        .map(|n| n.normalize_or(Vec3::Y).to_array())
        .collect()
}

fn smooth_normals(positions: &[[f32; 3]], normals: &[[f32; 3]]) -> Vec<[f32; 3]> {
    let key = |p: &[f32; 3]| p.map(|c| (c * 1e4).round() as i64);
    let mut sums: HashMap<[i64; 3], Vec3> = HashMap::new();
    for (p, n) in positions.iter().zip(normals) {
        *sums.entry(key(p)).or_default() += Vec3::from(*n);
    }
    positions
        .iter()
        .map(|p| sums[&key(p)].normalize_or(Vec3::Y).to_array())
        .collect()
}

fn target_names(mesh: &gltf::Mesh) -> Vec<String> {
    mesh.extras()
        .as_ref()
        .and_then(|raw| serde_json::from_str::<serde_json::Value>(raw.get()).ok())
        .and_then(|v| v.get("targetNames").cloned())
        .and_then(|v| serde_json::from_value(v).ok())
        .unwrap_or_default()
}

pub fn load(path: &Path) -> Result<Model, String> {
    let (doc, buffers, images) =
        gltf::import(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let textures = images.iter().map(to_rgba).collect::<Result<Vec<_>, _>>()?;
    let mut materials: Vec<Material> = doc
        .materials()
        .map(|m| {
            let pbr = m.pbr_metallic_roughness();
            Material {
                base_color: pbr.base_color_factor(),
                texture: pbr
                    .base_color_texture()
                    .map(|t| t.texture().source().index()),
                eye: None,
            }
        })
        .collect();
    let default_material = materials.len();
    materials.push(Material {
        base_color: [0.8, 0.8, 0.8, 1.0],
        texture: None,
        eye: None,
    });

    let gnodes: Vec<gltf::Node> = doc.nodes().collect();
    let mut nodes: Vec<Node> = gnodes
        .iter()
        .map(|n| {
            let (t, r, s) = n.transform().decomposed();
            Node {
                name: n.name().unwrap_or("").to_string(),
                parent: None,
                rest: Trs {
                    t: Vec3::from(t),
                    r: Quat::from_array(r),
                    s: Vec3::from(s),
                },
            }
        })
        .collect();
    for n in &gnodes {
        for c in n.children() {
            nodes[c.index()].parent = Some(n.index());
        }
    }
    let scene = doc
        .default_scene()
        .or_else(|| doc.scenes().next())
        .ok_or("no scene")?;
    let mut order: Vec<usize> = scene.nodes().map(|n| n.index()).collect();
    let mut i = 0;
    while i < order.len() {
        order.extend(gnodes[order[i]].children().map(|c| c.index()));
        i += 1;
    }

    let mut model = Model {
        vertices: Vec::new(),
        indices: Vec::new(),
        primitives: Vec::new(),
        materials,
        textures,
        nodes,
        order: order.clone(),
        palette: Vec::new(),
        morph_names: Vec::new(),
        morph_deltas: Vec::new(),
        node_morphs: vec![Vec::new(); gnodes.len()],
        clips: Vec::new(),
        dropped_morphs: 0,
    };
    // Per-vertex morph deltas, keyed by global morph index, before transposing.
    let mut deltas: Vec<Vec<(usize, [f32; 4])>> = Vec::new();

    for &ni in &order {
        let node = &gnodes[ni];
        let Some(mesh) = node.mesh() else { continue };
        // Joint palette for this mesh instance. An unskinned mesh follows its node,
        // through a single palette entry, so all geometry goes down one path.
        let base = model.palette.len() as u32;
        match node.skin() {
            Some(skin) => {
                let ibms: Vec<Mat4> = skin
                    .reader(|b| Some(&buffers[b.index()]))
                    .read_inverse_bind_matrices()
                    .map(|it| it.map(|m| Mat4::from_cols_array_2d(&m)).collect())
                    .unwrap_or_default();
                for (j, joint) in skin.joints().enumerate() {
                    model.palette.push(PaletteEntry {
                        node: joint.index(),
                        inverse_bind: ibms.get(j).copied().unwrap_or(Mat4::IDENTITY),
                    });
                }
            }
            None => model.palette.push(PaletteEntry {
                node: ni,
                inverse_bind: Mat4::IDENTITY,
            }),
        }

        let names = target_names(&mesh);
        let target_count = mesh
            .primitives()
            .map(|p| p.morph_targets().len())
            .max()
            .unwrap_or(0);
        let globals: Vec<Option<usize>> = (0..target_count)
            .map(|t| {
                (model.morph_names.len() < MAX_MORPHS).then(|| {
                    let name = names.get(t).cloned().unwrap_or_else(|| format!("morph{t}"));
                    model.morph_names.push(name);
                    model.morph_names.len() - 1
                })
            })
            .collect();
        model.dropped_morphs += globals.iter().filter(|g| g.is_none()).count();
        model.node_morphs[ni] = globals.clone();

        for prim in mesh.primitives() {
            if prim.mode() != gltf::mesh::Mode::Triangles {
                continue;
            }
            let r = prim.reader(|b| Some(&buffers[b.index()]));
            let Some(positions) = r.read_positions().map(|p| p.collect::<Vec<_>>()) else {
                continue;
            };
            let n = positions.len();
            let local: Vec<u32> = r
                .read_indices()
                .map(|i| i.into_u32().collect())
                .unwrap_or_else(|| (0..n as u32).collect());
            let normals = r
                .read_normals()
                .map(|it| it.collect())
                .unwrap_or_else(|| compute_normals(&positions, &local));
            let smooth = smooth_normals(&positions, &normals);
            let uvs: Vec<[f32; 2]> = r
                .read_tex_coords(0)
                .map(|t| t.into_f32().collect())
                .unwrap_or_else(|| vec![[0.0; 2]; n]);
            let joints: Vec<[u32; 4]> = match r.read_joints(0) {
                Some(j) if node.skin().is_some() => {
                    j.into_u16().map(|j| j.map(|x| base + x as u32)).collect()
                }
                _ => vec![[base; 4]; n],
            };
            let weights: Vec<[f32; 4]> = match r.read_weights(0) {
                Some(w) if node.skin().is_some() => w.into_f32().collect(),
                _ => vec![[1.0, 0.0, 0.0, 0.0]; n],
            };

            let first_vertex = model.vertices.len() as u32;
            for v in 0..n {
                model.vertices.push(Vertex {
                    position: positions[v],
                    normal: normals[v],
                    smooth_normal: smooth[v],
                    uv: uvs[v],
                    joints: joints[v],
                    weights: weights[v],
                });
            }
            deltas.resize(model.vertices.len(), Vec::new());
            for (t, (target_positions, _, _)) in r.read_morph_targets().enumerate() {
                let (Some(g), Some(ps)) = (globals.get(t).copied().flatten(), target_positions)
                else {
                    continue;
                };
                for (v, d) in ps.enumerate() {
                    if d != [0.0; 3] {
                        deltas[first_vertex as usize + v].push((g, [d[0], d[1], d[2], 0.0]));
                    }
                }
            }

            model.primitives.push(Primitive {
                first_index: model.indices.len() as u32,
                index_count: local.len() as u32,
                material: prim.material().index().unwrap_or(default_material),
            });
            model.indices.extend(local.iter().map(|i| i + first_vertex));
        }
    }
    if model.vertices.is_empty() {
        return Err(format!("{}: no triangle geometry", path.display()));
    }
    let vcount = model.vertices.len();
    model.morph_deltas = vec![[0.0; 4]; model.morph_names.len().max(1) * vcount];
    for (v, list) in deltas.iter().enumerate() {
        for &(g, d) in list {
            model.morph_deltas[g * vcount + v] = d;
        }
    }

    for anim in doc.animations() {
        let mut clip = Clip {
            name: anim.name().unwrap_or("").to_string(),
            duration: 0.0,
            tracks: Vec::new(),
        };
        for ch in anim.channels() {
            let r = ch.reader(|b| Some(&buffers[b.index()]));
            let (Some(times), Some(out)) = (r.read_inputs(), r.read_outputs()) else {
                continue;
            };
            let times: Vec<f32> = times.collect();
            let interp = ch.sampler().interpolation();
            // Cubic splines store in-tangent, value, out-tangent; keep the values.
            let pick = |len: usize, i: usize| match interp {
                Interpolation::CubicSpline => i % 3 == 1 || len == times.len(),
                _ => true,
            };
            let values = match out {
                ReadOutputs::Translations(it) => {
                    let all: Vec<Vec3> = it.map(Vec3::from).collect();
                    let len = all.len();
                    Values::Translation(
                        all.into_iter()
                            .enumerate()
                            .filter(|(i, _)| pick(len, *i))
                            .map(|x| x.1)
                            .collect(),
                    )
                }
                ReadOutputs::Rotations(it) => {
                    let all: Vec<Quat> = it.into_f32().map(Quat::from_array).collect();
                    let len = all.len();
                    Values::Rotation(
                        all.into_iter()
                            .enumerate()
                            .filter(|(i, _)| pick(len, *i))
                            .map(|x| x.1)
                            .collect(),
                    )
                }
                ReadOutputs::Scales(it) => {
                    let all: Vec<Vec3> = it.map(Vec3::from).collect();
                    let len = all.len();
                    Values::Scale(
                        all.into_iter()
                            .enumerate()
                            .filter(|(i, _)| pick(len, *i))
                            .map(|x| x.1)
                            .collect(),
                    )
                }
                ReadOutputs::MorphTargetWeights(it) => {
                    let all: Vec<f32> = it.into_f32().collect();
                    let width = all.len()
                        / times.len()
                        / if interp == Interpolation::CubicSpline {
                            3
                        } else {
                            1
                        };
                    let values = match interp {
                        Interpolation::CubicSpline => all
                            .chunks(width)
                            .enumerate()
                            .filter(|(i, _)| i % 3 == 1)
                            .flat_map(|(_, c)| c.to_vec())
                            .collect(),
                        _ => all,
                    };
                    Values::Weights { width, values }
                }
            };
            clip.duration = clip.duration.max(times.last().copied().unwrap_or(0.0));
            clip.tracks.push(Track {
                node: ch.target().node().index(),
                times,
                step: interp == Interpolation::Step,
                values,
            });
        }
        model.clips.push(clip);
    }
    Ok(model)
}
