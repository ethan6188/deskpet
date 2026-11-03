struct Uniforms {
    view_proj: mat4x4<f32>,
    model: mat4x4<f32>,
    // xyz: light direction (world space), w: unused
    light_dir: vec4<f32>,
    // rgb: base color, a: unused
    base_color: vec4<f32>,
    // xyz: camera position (world space)
    eye: vec4<f32>,
};

@group(0) @binding(0) var<uniform> u: Uniforms;

struct VsIn {
    @location(0) position: vec3<f32>,
    @location(1) normal: vec3<f32>,
};

struct VsOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) normal: vec3<f32>,
    @location(1) world: vec3<f32>,
};

@vertex
fn vs_main(v: VsIn) -> VsOut {
    var out: VsOut;
    let world = u.model * vec4<f32>(v.position, 1.0);
    out.clip = u.view_proj * world;
    out.world = world.xyz;
    out.normal = normalize((u.model * vec4<f32>(v.normal, 0.0)).xyz);
    return out;
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    let n = normalize(in.normal);
    let l = normalize(u.light_dir.xyz);
    let v = normalize(u.eye.xyz - in.world);
    let h = normalize(l + v);

    // Half-Lambert keeps the shadow side soft, like fur.
    let diffuse = pow(dot(n, l) * 0.5 + 0.5, 2.0);
    let spec = pow(max(dot(n, h), 0.0), 48.0) * 0.25;
    // Rim light separates the pet from any desktop wallpaper.
    let rim = pow(1.0 - max(dot(n, v), 0.0), 3.0) * 0.35;

    let color = u.base_color.rgb * (0.25 + 0.75 * diffuse) + vec3<f32>(spec + rim);
    // Premultiplied alpha: the window is transparent outside the pet.
    return vec4<f32>(color, 1.0);
}
