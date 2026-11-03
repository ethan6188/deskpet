//! Placeholder pet geometry. Replaced by glTF assets once the asset pipeline lands.

use bytemuck::{Pod, Zeroable};
use glam::Vec3;

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
pub struct Vertex {
    pub position: [f32; 3],
    pub normal: [f32; 3],
}

/// An analytic shape used for cursor hit testing; far cheaper than testing triangles.
#[derive(Clone, Copy)]
pub struct Ellipsoid {
    pub radii: Vec3,
    pub center: Vec3,
}

impl Ellipsoid {
    /// Whether a ray (in the ellipsoid's space) hits it.
    pub fn hit(&self, origin: Vec3, dir: Vec3) -> bool {
        // Scale space so the ellipsoid becomes a unit sphere, then solve the quadratic.
        let o = (origin - self.center) / self.radii;
        let d = dir / self.radii;
        let a = d.dot(d);
        let b = o.dot(d);
        let c = o.dot(o) - 1.0;
        let disc = b * b - a * c;
        disc >= 0.0 && (-b + disc.sqrt()) >= 0.0
    }
}

pub struct Mesh {
    pub vertices: Vec<Vertex>,
    pub indices: Vec<u32>,
    pub hit_shapes: Vec<Ellipsoid>,
}

impl Mesh {
    /// A UV sphere, squashed and offset so it can stand in for body and head.
    pub fn ellipsoid(radii: Vec3, offset: Vec3, rings: u32, segments: u32) -> Self {
        let mut vertices = Vec::with_capacity(((rings + 1) * (segments + 1)) as usize);
        for r in 0..=rings {
            let theta = std::f32::consts::PI * r as f32 / rings as f32;
            for s in 0..=segments {
                let phi = std::f32::consts::TAU * s as f32 / segments as f32;
                let unit = Vec3::new(
                    theta.sin() * phi.cos(),
                    theta.cos(),
                    theta.sin() * phi.sin(),
                );
                // The normal of a scaled sphere is the unit normal divided by the radii.
                let normal = (unit / radii).normalize();
                vertices.push(Vertex {
                    position: (unit * radii + offset).to_array(),
                    normal: normal.to_array(),
                });
            }
        }

        let mut indices = Vec::with_capacity((rings * segments * 6) as usize);
        let row = segments + 1;
        for r in 0..rings {
            for s in 0..segments {
                let a = r * row + s;
                let b = a + row;
                indices.extend_from_slice(&[a, a + 1, b, a + 1, b + 1, b]);
            }
        }
        Self {
            vertices,
            indices,
            hit_shapes: vec![Ellipsoid {
                radii,
                center: offset,
            }],
        }
    }

    /// Axis-aligned bounds of all vertices.
    pub fn bounds(&self) -> (Vec3, Vec3) {
        self.vertices.iter().fold(
            (Vec3::splat(f32::MAX), Vec3::splat(f32::MIN)),
            |(lo, hi), v| {
                let p = Vec3::from_array(v.position);
                (lo.min(p), hi.max(p))
            },
        )
    }

    pub fn append(&mut self, other: Mesh) {
        let base = self.vertices.len() as u32;
        self.hit_shapes.extend(other.hit_shapes);
        self.vertices.extend(other.vertices);
        self.indices
            .extend(other.indices.into_iter().map(|i| i + base));
    }

    /// Body, head and two ears: enough shape to judge lighting, scale and animation.
    pub fn placeholder_pet() -> Self {
        let mut pet = Self::ellipsoid(
            Vec3::new(0.55, 0.42, 0.45),
            Vec3::new(0.0, -0.25, 0.0),
            48,
            64,
        );
        pet.append(Self::ellipsoid(
            Vec3::splat(0.36),
            Vec3::new(0.0, 0.38, 0.12),
            48,
            64,
        ));
        for x in [-0.2, 0.2] {
            pet.append(Self::ellipsoid(
                Vec3::new(0.09, 0.16, 0.06),
                Vec3::new(x, 0.72, 0.1),
                16,
                24,
            ));
        }
        pet
    }
}
