//! Gaussian splats bound to the pet's skeleton. Splats give photographic
//! appearance (fur, fuzz, soft edges); the glTF still supplies the skeleton,
//! the clips and the mesh used for click-through.
//!
//! Splats are loaded in the pet's rest pose, then each one is bound to the
//! nearest rest-pose vertex: it takes that vertex's joints and weights and is
//! moved into bind space, so the GPU skins splats exactly like vertices.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::path::Path;

use bytemuck::{Pod, Zeroable};
use glam::{Mat3, Mat4, Quat, Vec3};

use crate::model::Model;
use crate::pet::{Style, StyleKind};
use crate::pose::{self, Pose};

/// Zeroth-order spherical harmonic constant: color = 0.5 + C0 * f_dc.
const SH_C0: f32 = 0.282_094_8;

#[derive(Clone, Copy, Debug)]
pub struct Splat {
    pub center: Vec3,
    /// Standard deviations along the splat's local axes.
    pub scale: Vec3,
    pub rotation: Quat,
    pub opacity: f32,
    /// sRGB, 0..1.
    pub color: [f32; 3],
    pub joints: [u32; 4],
    pub weights: [f32; 4],
    /// Distance to the vertex it is bound to, in model units.
    pub bind_distance: f32,
    /// How far it reaches outside the mesh's rest-pose box, in model units.
    pub overhang: f32,
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
pub struct GpuSplat {
    center_opacity: [f32; 4],
    scale: [f32; 4],
    rotation: [f32; 4],
    color: [f32; 4],
    joints: [u32; 4],
    weights: [f32; 4],
}

fn srgb_to_linear(c: f32) -> f32 {
    if c <= 0.04045 {
        c / 12.92
    } else {
        ((c + 0.055) / 1.055).powf(2.4)
    }
}

fn linear_to_srgb(c: f32) -> f32 {
    if c <= 0.003_130_8 {
        c * 12.92
    } else {
        1.055 * c.powf(1.0 / 2.4) - 0.055
    }
}

impl Splat {
    pub fn gpu(&self) -> GpuSplat {
        let c = self.color.map(|c| srgb_to_linear(c.clamp(0.0, 1.0)));
        GpuSplat {
            center_opacity: self.center.extend(self.opacity).to_array(),
            scale: self.scale.extend(0.0).to_array(),
            rotation: self.rotation.to_array(),
            color: [c[0], c[1], c[2], 1.0],
            joints: self.joints,
            weights: self.weights,
        }
    }
}

/// Where splats sit relative to the model: a capture rarely shares its origin,
/// scale or up axis.
#[derive(Clone, Copy, Debug)]
pub struct Align {
    pub scale: f32,
    pub rotation: Quat,
    pub offset: Vec3,
}

impl Align {
    fn apply(&self, s: &mut Splat) {
        s.center = self.rotation * (s.center * self.scale) + self.offset;
        s.scale *= self.scale;
        s.rotation = (self.rotation * s.rotation).normalize();
    }
}

#[derive(Clone, Copy)]
enum Ty {
    U8,
    F32,
    F64,
}

/// Reads the binary PLY that 3D Gaussian splatting tools write (the INRIA
/// layout: x y z, f_dc_*, opacity, scale_*, rot_*). Higher-order SH bands are
/// skipped, so color does not change with view angle.
pub fn load_ply(path: &Path) -> Result<Vec<Splat>, String> {
    let err = |e: String| format!("{}: {e}", path.display());
    let file = std::fs::File::open(path).map_err(|e| err(e.to_string()))?;
    let mut r = BufReader::new(file);
    let mut props: Vec<(String, Ty)> = Vec::new();
    let mut count = 0usize;
    let mut in_vertex = false;
    let mut line = String::new();
    loop {
        line.clear();
        if r.read_line(&mut line).map_err(|e| err(e.to_string()))? == 0 {
            return Err(err("no end_header".into()));
        }
        let words: Vec<&str> = line.split_whitespace().collect();
        match words.as_slice() {
            ["end_header"] => break,
            ["format", f, ..] if *f != "binary_little_endian" => {
                return Err(err(format!(
                    "PLY format {f}; only binary_little_endian is supported"
                )));
            }
            ["element", "vertex", n] => {
                count = n.parse().map_err(|_| err("bad vertex count".into()))?;
                in_vertex = true;
            }
            ["element", ..] => in_vertex = false,
            ["property", "list", ..] if in_vertex => {
                return Err(err("list properties on vertices are not supported".into()));
            }
            ["property", ty, name] if in_vertex => {
                let ty = match *ty {
                    "float" | "float32" => Ty::F32,
                    "double" | "float64" => Ty::F64,
                    "uchar" | "uint8" => Ty::U8,
                    other => return Err(err(format!("property type {other} is not supported"))),
                };
                props.push((name.to_string(), ty));
            }
            _ => {}
        }
    }
    let stride: usize = props
        .iter()
        .map(|(_, t)| match t {
            Ty::U8 => 1,
            Ty::F32 => 4,
            Ty::F64 => 8,
        })
        .sum();
    let index: HashMap<&str, usize> = props
        .iter()
        .enumerate()
        .map(|(i, p)| (p.0.as_str(), i))
        .collect();
    let offsets: Vec<usize> = props
        .iter()
        .scan(0, |o, (_, t)| {
            let here = *o;
            *o += match t {
                Ty::U8 => 1,
                Ty::F32 => 4,
                Ty::F64 => 8,
            };
            Some(here)
        })
        .collect();
    for need in ["x", "y", "z"] {
        if !index.contains_key(need) {
            return Err(err(format!("no \"{need}\" property")));
        }
    }
    let mut data = vec![0u8; stride * count];
    r.read_exact(&mut data)
        .map_err(|e| err(format!("truncated: {e}")))?;

    let mut out = Vec::with_capacity(count);
    for row in data.chunks_exact(stride) {
        let get = |name: &str| -> Option<f32> {
            let i = *index.get(name)?;
            let b = &row[offsets[i]..];
            Some(match props[i].1 {
                Ty::U8 => b[0] as f32 / 255.0,
                Ty::F32 => f32::from_le_bytes(b[..4].try_into().unwrap()),
                Ty::F64 => f64::from_le_bytes(b[..8].try_into().unwrap()) as f32,
            })
        };
        let center = Vec3::new(get("x").unwrap(), get("y").unwrap(), get("z").unwrap());
        let color = match (get("f_dc_0"), get("f_dc_1"), get("f_dc_2")) {
            (Some(r), Some(g), Some(b)) => [r, g, b].map(|c| 0.5 + SH_C0 * c),
            _ => [
                get("red").unwrap_or(0.8),
                get("green").unwrap_or(0.8),
                get("blue").unwrap_or(0.8),
            ],
        };
        // Stored as logit and log; plain point clouds get small opaque dots.
        let opacity = get("opacity").map_or(1.0, |o| 1.0 / (1.0 + (-o).exp()));
        let scale = match (get("scale_0"), get("scale_1"), get("scale_2")) {
            (Some(a), Some(b), Some(c)) => Vec3::new(a.exp(), b.exp(), c.exp()),
            _ => Vec3::splat(0.0),
        };
        // INRIA order is w, x, y, z.
        let rotation = match (get("rot_0"), get("rot_1"), get("rot_2"), get("rot_3")) {
            (Some(w), Some(x), Some(y), Some(z)) => Quat::from_xyzw(x, y, z, w).normalize(),
            _ => Quat::IDENTITY,
        };
        if !center.is_finite() || !scale.is_finite() {
            continue;
        }
        out.push(Splat {
            center,
            scale,
            rotation,
            opacity,
            color,
            joints: [0; 4],
            weights: [1.0, 0.0, 0.0, 0.0],
            bind_distance: 0.0,
            overhang: 0.0,
        });
    }
    // Point clouds without scales: size each dot from the cloud's spacing.
    if out.iter().all(|s| s.scale == Vec3::ZERO) && !out.is_empty() {
        let (lo, hi) = bounds(out.iter().map(|s| s.center));
        let spacing = ((hi - lo).max_element() / (out.len() as f32).cbrt()) * 0.5;
        for s in &mut out {
            s.scale = Vec3::splat(spacing);
        }
    }
    Ok(out)
}

/// Writes splats (their rest-pose placement, not bind space) in the INRIA layout.
pub fn write_ply(path: &Path, splats: &[Splat]) -> Result<(), String> {
    let names = [
        "x", "y", "z", "nx", "ny", "nz", "f_dc_0", "f_dc_1", "f_dc_2", "opacity", "scale_0",
        "scale_1", "scale_2", "rot_0", "rot_1", "rot_2", "rot_3",
    ];
    let mut out = Vec::with_capacity(splats.len() * names.len() * 4 + 512);
    write!(
        out,
        "ply\nformat binary_little_endian 1.0\nelement vertex {}\n",
        splats.len()
    )
    .unwrap();
    for n in names {
        writeln!(out, "property float {n}").unwrap();
    }
    out.extend_from_slice(b"end_header\n");
    for s in splats {
        let dc = s.color.map(|c| (c - 0.5) / SH_C0);
        let o = s.opacity.clamp(1e-4, 1.0 - 1e-4);
        let q = s.rotation;
        let row = [
            s.center.x,
            s.center.y,
            s.center.z,
            0.0,
            0.0,
            0.0,
            dc[0],
            dc[1],
            dc[2],
            (o / (1.0 - o)).ln(),
            s.scale.x.max(1e-8).ln(),
            s.scale.y.max(1e-8).ln(),
            s.scale.z.max(1e-8).ln(),
            q.w,
            q.x,
            q.y,
            q.z,
        ];
        for v in row {
            out.extend_from_slice(&v.to_le_bytes());
        }
    }
    std::fs::File::create(path)
        .and_then(|mut f| f.write_all(&out))
        .map_err(|e| format!("{}: {e}", path.display()))
}

fn bounds(points: impl Iterator<Item = Vec3>) -> (Vec3, Vec3) {
    points.fold(
        (Vec3::splat(f32::MAX), Vec3::splat(f32::MIN)),
        |(l, h), p| (l.min(p), h.max(p)),
    )
}

/// Uniform grid over points, for nearest-neighbor lookups.
struct Grid {
    cell: f32,
    cells: HashMap<[i32; 3], Vec<u32>>,
}

impl Grid {
    fn new(points: &[Vec3], cell: f32) -> Self {
        let mut cells: HashMap<[i32; 3], Vec<u32>> = HashMap::new();
        for (i, p) in points.iter().enumerate() {
            cells.entry(Self::key(*p, cell)).or_default().push(i as u32);
        }
        Self { cell, cells }
    }

    fn key(p: Vec3, cell: f32) -> [i32; 3] {
        (p / cell).floor().as_ivec3().to_array()
    }

    /// Nearest point within `rings` cells, else by brute force.
    fn nearest(&self, points: &[Vec3], q: Vec3, rings: i32) -> (u32, f32) {
        let k = Self::key(q, self.cell);
        let mut best = (0u32, f32::MAX);
        for r in 0..=rings {
            for dx in -r..=r {
                for dy in -r..=r {
                    for dz in -r..=r {
                        if dx.abs().max(dy.abs()).max(dz.abs()) != r {
                            continue;
                        }
                        let Some(list) = self.cells.get(&[k[0] + dx, k[1] + dy, k[2] + dz]) else {
                            continue;
                        };
                        for &i in list {
                            let d = points[i as usize].distance_squared(q);
                            if d < best.1 {
                                best = (i, d);
                            }
                        }
                    }
                }
            }
            // A hit in ring r can still be beaten by one in ring r + 1 at most.
            if best.1 < (r as f32 * self.cell).powi(2) {
                break;
            }
        }
        if best.1 == f32::MAX {
            for (i, p) in points.iter().enumerate() {
                let d = p.distance_squared(q);
                if d < best.1 {
                    best = (i as u32, d);
                }
            }
        }
        (best.0, best.1.sqrt())
    }
}

/// Aligns rest-pose splats to the model and binds each to the skeleton.
pub fn bind(model: &Model, splats: &mut [Splat], align: Align) {
    let rest = Pose::rest(model);
    let posed = pose::skinned_positions(model, &rest);
    let joints = pose::joint_matrices(model, &rest);
    let (lo, hi) = bounds(posed.iter().copied());
    let grid = Grid::new(&posed, ((hi - lo).max_element() / 48.0).max(1e-6));
    for s in splats.iter_mut() {
        align.apply(s);
        let r = 2.0 * s.scale.max_element();
        s.overhang = ((lo - s.center).max(s.center - hi).max_element() + r).max(0.0);
        let (v, d) = grid.nearest(&posed, s.center, 3);
        let vert = &model.vertices[v as usize];
        s.joints = vert.joints;
        s.weights = vert.weights;
        s.bind_distance = d;
        // Undo the rest pose so the GPU's skinning puts the splat back where it was.
        let skin: Mat4 = (0..4)
            .map(|k| joints[vert.joints[k] as usize] * vert.weights[k])
            .fold(Mat4::ZERO, |a, m| a + m);
        let inv = skin.inverse();
        if !inv.is_finite() {
            continue;
        }
        s.center = inv.transform_point3(s.center);
        let (scale, rot, _) = inv.to_scale_rotation_translation();
        s.rotation = (rot * s.rotation).normalize();
        s.scale *= scale.abs().max_element();
    }
}

/// Covers the mesh surface (in the rest pose) with about `count` flat splats
/// colored by its textures. Useful to try the splat path, and as a starting
/// point; a real capture looks far better.
pub fn bake(model: &Model, count: usize) -> Vec<Splat> {
    let rest = Pose::rest(model);
    let posed = pose::skinned_positions(model, &rest);
    let tris: Vec<(usize, [u32; 3], f32)> = model
        .primitives
        .iter()
        .flat_map(|p| {
            let idx =
                &model.indices[p.first_index as usize..(p.first_index + p.index_count) as usize];
            idx.as_chunks::<3>()
                .0
                .iter()
                .map(move |t| (p.material, [t[0], t[1], t[2]]))
        })
        .map(|(m, t)| {
            let [a, b, c] = t.map(|i| posed[i as usize]);
            (m, t, (b - a).cross(c - a).length() * 0.5)
        })
        .collect();
    let total: f32 = tris.iter().map(|t| t.2).sum();
    if total <= 0.0 {
        return Vec::new();
    }
    let radius = (total / count as f32).sqrt() * 0.75;
    // Deterministic low-discrepancy sampling: same model, same splats.
    let mut seed = 0x9E37_79B9u32;
    let mut rand = || {
        seed ^= seed << 13;
        seed ^= seed >> 17;
        seed ^= seed << 5;
        seed as f32 / u32::MAX as f32
    };
    let mut out = Vec::with_capacity(count);
    let mut carry = 0.0;
    for (material, t, area) in tris {
        carry += area / total * count as f32;
        let [a, b, c] = t.map(|i| posed[i as usize]);
        let n = (b - a).cross(c - a).normalize_or(Vec3::Y);
        let rotation = Quat::from_rotation_arc(Vec3::Z, n);
        while carry >= 1.0 {
            carry -= 1.0;
            let (mut u, mut v) = (rand(), rand());
            if u + v > 1.0 {
                (u, v) = (1.0 - u, 1.0 - v);
            }
            let w = 1.0 - u - v;
            let uv = [0, 1].map(|k| {
                let [ua, ub, uc] = t.map(|i| model.vertices[i as usize].uv[k]);
                ua * w + ub * u + uc * v
            });
            out.push(Splat {
                center: a * w + b * u + c * v,
                scale: Vec3::new(radius, radius, radius * 0.15),
                rotation,
                opacity: 0.95,
                color: surface_color(model, material, uv),
                joints: [0; 4],
                weights: [1.0, 0.0, 0.0, 0.0],
                bind_distance: 0.0,
                overhang: 0.0,
            });
        }
    }
    out
}

/// Base color factor times texture, as sRGB.
fn surface_color(model: &Model, material: usize, uv: [f32; 2]) -> [f32; 3] {
    let m = &model.materials[material];
    let tex = m.texture.and_then(|i| model.textures.get(i));
    let texel = tex.map_or([1.0; 3], |t| {
        let x = ((uv[0].rem_euclid(1.0) * t.width as f32) as u32).min(t.width - 1);
        let y = ((uv[1].rem_euclid(1.0) * t.height as f32) as u32).min(t.height - 1);
        let i = ((y * t.width + x) * 4) as usize;
        [0, 1, 2].map(|c| srgb_to_linear(t.rgba[i + c] as f32 / 255.0))
    });
    [0, 1, 2].map(|c| linear_to_srgb(texel[c] * m.base_color[c]))
}

/// Back-to-front draw order for the current pose. `view_model` maps model
/// space to view space; only its depth row matters.
pub fn sort_order(
    splats: &[GpuSplat],
    joints: &[Mat4],
    view_model: Mat4,
    order: &mut Vec<u32>,
    keys: &mut Vec<u32>,
) {
    let row = view_model.row(2);
    // Depth of a splat = row · (Σ w_k J_k) c, so fold the row into each joint once.
    let joint_rows: Vec<glam::Vec4> = joints.iter().map(|j| j.transpose() * row).collect();
    keys.clear();
    keys.extend(splats.iter().map(|s| {
        let c = glam::Vec4::new(
            s.center_opacity[0],
            s.center_opacity[1],
            s.center_opacity[2],
            1.0,
        );
        let mut z = 0.0;
        for k in 0..4 {
            z += s.weights[k] * joint_rows[s.joints[k] as usize].dot(c);
        }
        // View space looks down -z: the most negative z is farthest, so it sorts first.
        let bits = z.to_bits();
        if bits & 0x8000_0000 != 0 {
            !bits
        } else {
            bits | 0x8000_0000
        }
    }));
    radix_sort(keys, order);
}

/// LSD radix sort of indices by 32-bit key, 8 bits a pass: linear time, and
/// its four passes stream through memory, which the caches like.
fn radix_sort(keys: &[u32], order: &mut Vec<u32>) {
    let n = keys.len();
    order.clear();
    order.extend(0..n as u32);
    let mut tmp = vec![0u32; n];
    for pass in 0..4 {
        let shift = pass * 8;
        let mut count = [0usize; 257];
        for &i in order.iter() {
            count[((keys[i as usize] >> shift) & 0xFF) as usize + 1] += 1;
        }
        for b in 0..256 {
            count[b + 1] += count[b];
        }
        for &i in order.iter() {
            let b = ((keys[i as usize] >> shift) & 0xFF) as usize;
            tmp[count[b]] = i;
            count[b] += 1;
        }
        std::mem::swap(order, &mut tmp);
    }
}

/// The bound splats a style draws, or none for mesh styles.
pub fn for_style(dir: &Path, model: &Model, style: &Style) -> Result<Vec<Splat>, String> {
    if style.kind != StyleKind::Splat {
        return Ok(Vec::new());
    }
    let file = style
        .splats
        .as_ref()
        .ok_or("a splat style needs `splats = \"file.ply\"`")?;
    let mut splats = load_ply(&dir.join(file))?;
    if splats.is_empty() {
        return Err(format!("{}: no splats", file.display()));
    }
    let align = Align {
        scale: style.splat_scale,
        rotation: euler_degrees(style.splat_rotate),
        offset: Vec3::from(style.splat_offset),
    };
    bind(model, &mut splats, align);
    Ok(splats)
}

/// Rotation from Euler angles in degrees about X, Y and Z.
pub fn euler_degrees(deg: [f32; 3]) -> Quat {
    let r = deg.map(f32::to_radians);
    Quat::from_mat3(&Mat3::from_euler(glam::EulerRot::YXZ, r[1], r[0], r[2]))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn radix_sort_orders_by_key() {
        let keys = vec![5, 1, 0xFFFF_0000, 3, 3, 0];
        let mut order = Vec::new();
        radix_sort(&keys, &mut order);
        let sorted: Vec<u32> = order.iter().map(|&i| keys[i as usize]).collect();
        assert_eq!(sorted, vec![0, 1, 3, 3, 5, 0xFFFF_0000]);
    }

    #[test]
    fn ply_round_trips() {
        let s = Splat {
            center: Vec3::new(1.0, -2.0, 3.0),
            scale: Vec3::new(0.1, 0.2, 0.05),
            rotation: Quat::from_rotation_y(0.7),
            opacity: 0.8,
            color: [0.9, 0.4, 0.1],
            joints: [0; 4],
            weights: [1.0, 0.0, 0.0, 0.0],
            bind_distance: 0.0,
            overhang: 0.0,
        };
        let path = std::env::temp_dir().join("deskpet-roundtrip.ply");
        write_ply(&path, &[s]).unwrap();
        let back = load_ply(&path).unwrap();
        assert_eq!(back.len(), 1);
        let b = back[0];
        assert!(b.center.distance(s.center) < 1e-5);
        assert!(b.scale.distance(s.scale) < 1e-5);
        assert!((b.opacity - s.opacity).abs() < 1e-4);
        assert!(b.rotation.dot(s.rotation).abs() > 0.9999);
        assert!((b.color[1] - 0.4).abs() < 1e-4);
    }

    #[test]
    fn depth_keys_sort_far_first() {
        let mk = |z: f32| {
            Splat {
                center: Vec3::new(0.0, 0.0, z),
                scale: Vec3::ONE,
                rotation: Quat::IDENTITY,
                opacity: 1.0,
                color: [1.0; 3],
                joints: [0; 4],
                weights: [1.0, 0.0, 0.0, 0.0],
                bind_distance: 0.0,
                overhang: 0.0,
            }
            .gpu()
        };
        let splats = [mk(-1.0), mk(-5.0), mk(2.0)];
        let (mut order, mut keys) = (Vec::new(), Vec::new());
        sort_order(
            &splats,
            &[Mat4::IDENTITY],
            Mat4::IDENTITY,
            &mut order,
            &mut keys,
        );
        assert_eq!(order, vec![1, 0, 2]);
    }
}
