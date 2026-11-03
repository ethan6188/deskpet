// Skinned 3D Gaussian splats. Each instance is one splat drawn as a screen
// quad: the vertex stage skins its center and covariance, projects the
// covariance to 2D (EWA splatting) and sizes the quad to 3 sigma; the fragment
// stage evaluates the Gaussian. Instances arrive sorted back to front.

struct Frame {
    view_proj: mat4x4<f32>,
    model: mat4x4<f32>,
    eye: vec4<f32>,
    light: vec4<f32>,
    tint: vec4<f32>,
    outline: vec4<f32>,
    glow: vec4<f32>,
    params: vec4<f32>,
    counts: vec4<u32>,
};

struct SplatFrame {
    view: mat4x4<f32>,
    // x, y: focal length in pixels; z, w: viewport size in pixels.
    focal_viewport: vec4<f32>,
    // x: near plane distance.
    clip: vec4<f32>,
};

struct Splat {
    center_opacity: vec4<f32>,
    scale: vec4<f32>,
    // x, y, z, w
    rotation: vec4<f32>,
    color: vec4<f32>,
    joints: vec4<u32>,
    weights: vec4<f32>,
};

@group(0) @binding(0) var<uniform> f: Frame;
@group(0) @binding(1) var<storage, read> joints: array<mat4x4<f32>>;

@group(1) @binding(0) var<uniform> sf: SplatFrame;
@group(1) @binding(1) var<storage, read> splats: array<Splat>;
@group(1) @binding(2) var<storage, read> order: array<u32>;

struct VsOut {
    @builtin(position) clip: vec4<f32>,
    // Position in units of standard deviations along the splat's 2D axes.
    @location(0) local: vec2<f32>,
    @location(1) color: vec4<f32>,
};

fn quat_mat(q: vec4<f32>) -> mat3x3<f32> {
    let x = q.x; let y = q.y; let z = q.z; let w = q.w;
    return mat3x3<f32>(
        vec3<f32>(1.0 - 2.0 * (y * y + z * z), 2.0 * (x * y + w * z), 2.0 * (x * z - w * y)),
        vec3<f32>(2.0 * (x * y - w * z), 1.0 - 2.0 * (x * x + z * z), 2.0 * (y * z + w * x)),
        vec3<f32>(2.0 * (x * z + w * y), 2.0 * (y * z - w * x), 1.0 - 2.0 * (x * x + y * y)),
    );
}

fn upper3(m: mat4x4<f32>) -> mat3x3<f32> {
    return mat3x3<f32>(m[0].xyz, m[1].xyz, m[2].xyz);
}

const CORNERS = array<vec2<f32>, 6>(
    vec2<f32>(-1.0, -1.0), vec2<f32>(1.0, -1.0), vec2<f32>(1.0, 1.0),
    vec2<f32>(-1.0, -1.0), vec2<f32>(1.0, 1.0), vec2<f32>(-1.0, 1.0),
);

@vertex
fn vs_splat(@builtin(vertex_index) vi: u32, @builtin(instance_index) ii: u32) -> VsOut {
    var out: VsOut;
    let s = splats[order[ii]];
    let skin = joints[s.joints.x] * s.weights.x + joints[s.joints.y] * s.weights.y
        + joints[s.joints.z] * s.weights.z + joints[s.joints.w] * s.weights.w;
    let model_view = sf.view * f.model * skin;
    let p = model_view * vec4<f32>(s.center_opacity.xyz, 1.0);
    // Behind the camera: emit nothing.
    if (-p.z < sf.clip.x) {
        out.clip = vec4<f32>(0.0, 0.0, 2.0, 1.0);
        return out;
    }

    // 3D covariance in view space: W R S Sᵀ Rᵀ Wᵀ.
    let rs = quat_mat(s.rotation) * mat3x3<f32>(
        vec3<f32>(s.scale.x, 0.0, 0.0), vec3<f32>(0.0, s.scale.y, 0.0), vec3<f32>(0.0, 0.0, s.scale.z));
    let m = upper3(model_view) * rs;
    let cov3 = m * transpose(m);

    // Jacobian of the perspective projection at p, in pixels.
    let fx = sf.focal_viewport.x;
    let fy = sf.focal_viewport.y;
    let z = -p.z;
    let j = mat3x3<f32>(
        vec3<f32>(fx / z, 0.0, 0.0),
        vec3<f32>(0.0, fy / z, 0.0),
        vec3<f32>(fx * p.x / (z * z), fy * p.y / (z * z), 0.0),
    );
    let cov2 = j * cov3 * transpose(j);
    // A low-pass of about a third of a pixel keeps tiny splats from aliasing.
    let a = cov2[0][0] + 0.3;
    let b = cov2[0][1];
    let c = cov2[1][1] + 0.3;

    let mid = 0.5 * (a + c);
    let r = sqrt(max(0.25 * (a - c) * (a - c) + b * b, 0.0));
    let l1 = mid + r;
    let l2 = max(mid - r, 0.0);
    var v1 = vec2<f32>(1.0, 0.0);
    if (abs(b) > 1e-8) {
        v1 = normalize(vec2<f32>(b, l1 - a));
    } else if (c > a) {
        v1 = vec2<f32>(0.0, 1.0);
    }
    let v2 = vec2<f32>(-v1.y, v1.x);
    // Size the quad to where alpha falls below 1/255, not a fixed 3 sigma: faint
    // splats get small quads, which is most of the fill-rate saving.
    let opacity = s.center_opacity.w;
    let k = sqrt(max(2.0 * log(opacity * 255.0), 0.0));
    if (k <= 0.0) {
        out.clip = vec4<f32>(0.0, 0.0, 2.0, 1.0);
        return out;
    }
    // Cap the footprint: a huge splat costs fill rate and is almost always a floater.
    let ax1 = v1 * min(k * sqrt(l1), 512.0);
    let ax2 = v2 * min(k * sqrt(l2), 512.0);

    let corner = CORNERS[vi];
    let offset_px = ax1 * corner.x + ax2 * corner.y;
    let center = f.view_proj * f.model * skin * vec4<f32>(s.center_opacity.xyz, 1.0);
    let ndc_offset = offset_px * 2.0 / sf.focal_viewport.zw;
    out.clip = center + vec4<f32>(ndc_offset * center.w, 0.0, 0.0);
    out.local = corner * k;
    out.color = vec4<f32>(s.color.rgb * f.tint.rgb, s.center_opacity.w);
    return out;
}

@fragment
fn fs_splat(in: VsOut) -> @location(0) vec4<f32> {
    let alpha = in.color.a * exp(-0.5 * dot(in.local, in.local));
    if (alpha < 1.0 / 255.0) {
        discard;
    }
    // Premultiplied, blended back to front.
    return vec4<f32>(in.color.rgb * alpha, alpha);
}
