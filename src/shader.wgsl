// One shader for every style. The vertex stage applies morph targets and
// skinning on the GPU; the fragment stage branches on the style kind, which is
// uniform for the whole draw, so the branch costs nothing.

const KIND_REAL: u32 = 0u;
const KIND_ANIME: u32 = 1u;
const KIND_CYBER: u32 = 2u;

struct Frame {
    view_proj: mat4x4<f32>,
    // Pet placement: spin, breathing and facing.
    model: mat4x4<f32>,
    eye: vec4<f32>,
    // xyz: direction towards the key light.
    light: vec4<f32>,
    // rgb: tint.
    tint: vec4<f32>,
    // rgb: outline color, w: outline width in model units.
    outline: vec4<f32>,
    // rgb: glow / rim color, w: fur length in model units.
    glow: vec4<f32>,
    // x: seconds, y: cel bands.
    params: vec4<f32>,
    // x: vertex count, y: morph count, z: fur shells, w: style kind.
    counts: vec4<u32>,
};

struct Material {
    base_color: vec4<f32>,
    // x: 1 if the base color texture is present.
    flags: vec4<u32>,
};

@group(0) @binding(0) var<uniform> f: Frame;
@group(0) @binding(1) var<storage, read> joints: array<mat4x4<f32>>;
@group(0) @binding(2) var<storage, read> morph_deltas: array<vec4<f32>>;
@group(0) @binding(3) var<storage, read> morph_weights: array<f32>;

@group(1) @binding(0) var<uniform> mat: Material;
@group(1) @binding(1) var base_tex: texture_2d<f32>;
@group(1) @binding(2) var base_samp: sampler;

struct VsIn {
    @builtin(vertex_index) index: u32,
    @location(0) position: vec3<f32>,
    @location(1) normal: vec3<f32>,
    @location(2) smooth_normal: vec3<f32>,
    @location(3) uv: vec2<f32>,
    @location(4) joints: vec4<u32>,
    @location(5) weights: vec4<f32>,
};

struct VsOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) world: vec3<f32>,
    @location(1) normal: vec3<f32>,
    @location(2) uv: vec2<f32>,
    // 0 on the skin, 1 at the fur tips.
    @location(3) shell: f32,
};

struct Posed {
    position: vec3<f32>,
    normal: vec3<f32>,
    smooth_normal: vec3<f32>,
};

fn pose(v: VsIn) -> Posed {
    var p = v.position;
    for (var m = 0u; m < f.counts.y; m++) {
        let w = morph_weights[m];
        if (w != 0.0) {
            p += morph_deltas[m * f.counts.x + v.index].xyz * w;
        }
    }
    let skin = joints[v.joints.x] * v.weights.x + joints[v.joints.y] * v.weights.y
        + joints[v.joints.z] * v.weights.z + joints[v.joints.w] * v.weights.w;
    let m = f.model * skin;
    var out: Posed;
    out.position = (m * vec4<f32>(p, 1.0)).xyz;
    out.normal = normalize((m * vec4<f32>(v.normal, 0.0)).xyz);
    out.smooth_normal = normalize((m * vec4<f32>(v.smooth_normal, 0.0)).xyz);
    return out;
}

fn emit(world: vec3<f32>, normal: vec3<f32>, uv: vec2<f32>, shell: f32) -> VsOut {
    var out: VsOut;
    out.clip = f.view_proj * vec4<f32>(world, 1.0);
    out.world = world;
    out.normal = normal;
    out.uv = uv;
    out.shell = shell;
    return out;
}

@vertex
fn vs_main(v: VsIn) -> VsOut {
    let p = pose(v);
    return emit(p.position, p.normal, v.uv, 0.0);
}

// Inverted hull: front faces culled, pushed out along the smooth normal.
@vertex
fn vs_outline(v: VsIn) -> VsOut {
    let p = pose(v);
    return emit(p.position + p.smooth_normal * f.outline.w, p.normal, v.uv, 0.0);
}

// Shell fur: instance k draws the k-th shell above the skin.
@vertex
fn vs_fur(v: VsIn, @builtin(instance_index) k: u32) -> VsOut {
    let p = pose(v);
    let h = f32(k + 1u) / f32(f.counts.z);
    // Tips sag a little under gravity.
    let dir = normalize(p.smooth_normal - vec3<f32>(0.0, 0.35 * h, 0.0));
    return emit(p.position + dir * f.glow.w * h, p.normal, v.uv, h);
}

fn albedo(uv: vec2<f32>) -> vec3<f32> {
    var c = mat.base_color.rgb;
    if (mat.flags.x == 1u) {
        c *= textureSample(base_tex, base_samp, uv).rgb;
    }
    return c * f.tint.rgb;
}

fn hash(p: vec2<f32>) -> f32 {
    let q = fract(p * vec2<f32>(123.34, 456.21));
    let r = q + dot(q, q + 45.32);
    return fract(r.x * r.y);
}

// GGX specular with Schlick fresnel; enough for fur, eyes and a wet nose.
fn specular(n: vec3<f32>, l: vec3<f32>, v: vec3<f32>, roughness: f32) -> f32 {
    let h = normalize(l + v);
    let a = roughness * roughness;
    let nh = max(dot(n, h), 0.0);
    let d = a * a / (3.14159 * pow(nh * nh * (a * a - 1.0) + 1.0, 2.0));
    let fr = 0.04 + 0.96 * pow(1.0 - max(dot(h, v), 0.0), 5.0);
    return d * fr * max(dot(n, l), 0.0) * 0.25;
}

fn shade_real(base: vec3<f32>, n: vec3<f32>, l: vec3<f32>, v: vec3<f32>, occlusion: f32) -> vec3<f32> {
    // Wrapped diffuse lets light bleed round the terminator, like scattering in fur.
    let wrap = max((dot(n, l) + 0.4) / 1.4, 0.0);
    // Sky above, warm bounce from the desk below.
    let ambient = mix(vec3<f32>(0.32, 0.28, 0.26), vec3<f32>(0.42, 0.48, 0.58), n.y * 0.5 + 0.5);
    let rim = pow(1.0 - max(dot(n, v), 0.0), 4.0) * 0.4 * f.glow.rgb;
    let key = vec3<f32>(1.0, 0.96, 0.9) * wrap;
    return base * (ambient * occlusion + key) + specular(n, l, v, 0.55) + rim * occlusion;
}

fn shade_anime(base: vec3<f32>, n: vec3<f32>, l: vec3<f32>, v: vec3<f32>) -> vec3<f32> {
    let bands = max(f.params.y, 1.0);
    let ndl = dot(n, l) * 0.5 + 0.5;
    // Hard steps, each with a hair of anti-aliasing.
    let x = ndl * bands;
    let step_ = floor(x) + smoothstep(0.45, 0.55, fract(x));
    let light = mix(0.55, 1.0, clamp(step_ / bands, 0.0, 1.0));
    let shadow_tint = mix(vec3<f32>(0.75, 0.68, 0.9), vec3<f32>(1.0), light);
    let h = normalize(l + v);
    let highlight = smoothstep(0.96, 0.98, dot(n, h)) * 0.35;
    let rim = smoothstep(0.62, 0.68, 1.0 - max(dot(n, v), 0.0)) * smoothstep(0.3, 0.6, ndl) * 0.25;
    return base * light * shadow_tint + vec3<f32>(highlight) + rim * f.glow.rgb;
}

fn shade_cyber(base: vec3<f32>, n: vec3<f32>, v: vec3<f32>, world: vec3<f32>, frag_y: f32) -> vec4<f32> {
    let fresnel = pow(1.0 - max(dot(n, v), 0.0), 2.5);
    let luma = dot(base, vec3<f32>(0.3, 0.59, 0.11));
    let t = f.params.x;
    // Scan lines in screen space, so they stay crisp at any size.
    let scan = 0.8 + 0.2 * sin(frag_y * 1.6);
    // A brighter band sweeping up the body.
    let sweep = smoothstep(0.08, 0.0, abs(fract(world.y * 0.01 - t * 0.25) - 0.5));
    let flicker = 0.94 + 0.06 * sin(t * 37.0) * sin(t * 13.0);
    let c = f.glow.rgb * (0.18 + luma * 0.55 + fresnel * 1.4 + sweep * 0.6) * scan * flicker;
    let alpha = clamp(0.55 + fresnel * 0.45 + sweep * 0.3, 0.0, 1.0);
    // Premultiplied.
    return vec4<f32>(c * alpha, alpha);
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    let n = normalize(in.normal);
    let l = normalize(f.light.xyz);
    let v = normalize(f.eye.xyz - in.world);
    let base = albedo(in.uv);
    switch f.counts.w {
        case KIND_ANIME: {
            return vec4<f32>(shade_anime(base, n, l, v), 1.0);
        }
        case KIND_CYBER: {
            return shade_cyber(base, n, v, in.world, in.clip.y);
        }
        default: {
            // The skin under fur is darker: light rarely reaches it.
            let occlusion = select(1.0, 0.7, f.counts.z > 0u);
            return vec4<f32>(shade_real(base, n, l, v, occlusion), 1.0);
        }
    }
}

@fragment
fn fs_outline(in: VsOut) -> @location(0) vec4<f32> {
    if (f.counts.w == KIND_CYBER) {
        return vec4<f32>(f.glow.rgb * 0.9, 0.9);
    }
    return vec4<f32>(f.outline.rgb, 1.0);
}

@fragment
fn fs_fur(in: VsOut) -> @location(0) vec4<f32> {
    // Strands: a cell grid over the UVs, each cell a hair of random length that tapers.
    let density = 320.0;
    let cell = floor(in.uv * density);
    let local = fract(in.uv * density) - 0.5;
    let len = hash(cell);
    let radius = 0.5 * (1.0 - in.shell / max(len, 0.001));
    if (len < in.shell || length(local) > radius) {
        discard;
    }
    let n = normalize(in.normal);
    let l = normalize(f.light.xyz);
    let v = normalize(f.eye.xyz - in.world);
    // Self-shadowing: deeper shells get less light.
    let occlusion = mix(0.55, 1.0, in.shell);
    return vec4<f32>(shade_real(albedo(in.uv), n, l, v, occlusion) * mix(0.75, 1.05, in.shell), 1.0);
}
