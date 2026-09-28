// Shared declarations, prepended to every shader module.
// Layout must match `Globals` in src/gpu_types.rs.

struct Globals {
    view_proj: mat4x4<f32>,
    cam_pos: vec4<f32>,     // xyz, w = time
    tower: vec4<f32>,       // height, radius, depth_step, y_top
    tower2: vec4<f32>,      // depth layers, ribbon, fill, back_face
    lut: vec4<f32>,         // lut bins, ring count, atlas width, boost tau
    idle: vec4<f32>,        // idle_wound, idle_open, dir mark, ghost
    col_fresh: vec4<f32>,
    col_old: vec4<f32>,
    col_stale: vec4<f32>,
    col_read: vec4<f32>,    // rgb + intensity
    col_write: vec4<f32>,
    col_create: vec4<f32>,
    col_delete: vec4<f32>,
    col_teal: vec4<f32>,
    col_bg: vec4<f32>,
    times: vec4<f32>,       // read fade, write fade, grow, ghost seconds
    times2: vec4<f32>,      // attack, unwound ribbon fill, loop fade
    age: vec4<f32>,         // log10 fresh/old/stale days
    screen: vec4<f32>,      // w, h, 1/w, 1/h (internal)
    post: vec4<f32>,        // exposure, bloom, grain, vignette
    post2: vec4<f32>,       // smear decay, seed, manual gamma, 1/bloom levels
    rct: vec4<f32>,         // ring colour texture rows, bins, atlas texels per bin, _
};

@group(0) @binding(0) var<uniform> G: Globals;

const TAU: f32 = 6.28318530718;

struct FullOut {
    @builtin(position) pos: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

@vertex
fn vs_full(@builtin(vertex_index) i: u32) -> FullOut {
    let x = f32((i << 1u) & 2u);
    let y = f32(i & 2u);
    var o: FullOut;
    o.pos = vec4<f32>(x * 2.0 - 1.0, 1.0 - y * 2.0, 0.0, 1.0);
    o.uv = vec2<f32>(x, y);
    return o;
}

fn hash12(p: vec2<f32>) -> f32 {
    var p3 = fract(vec3<f32>(p.xyx) * 0.1031);
    p3 += dot(p3, p3.yzx + 33.33);
    return fract((p3.x + p3.y) * p3.z);
}
