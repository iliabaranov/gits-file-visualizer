// Shared declarations, prepended to every shader module.
// Layout must match `Globals` in src/gpu_types.rs.

struct Globals {
    view_proj: mat4x4<f32>,
    cam_pos: vec4<f32>,     // xyz, w = time
    tower: vec4<f32>,       // height, outer radius, inner radius, y_top
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
    times2: vec4<f32>,      // attack, unwound ribbon fill, loop fade, dense taps
    age: vec4<f32>,         // log10 fresh/old/stale days
    screen: vec4<f32>,      // w, h, 1/w, 1/h (internal)
    post: vec4<f32>,        // exposure, bloom, grain, vignette
    post2: vec4<f32>,       // smear decay, seed, manual gamma, 1/bloom levels
    rct: vec4<f32>,         // ring colour texture rows, bins, atlas texels per bin, upscale sharpness
    tower3: vec4<f32>,      // shell_dim, scan interval s (0 = off), scan duration s, scan brightness
    pyr: vec4<f32>,         // shell rotation (turns, fract), padded ring list length (0 = no pyramid), texture A height, B height
    post3: vec4<f32>,       // dither (0/1), output levels per channel (255 = 8 bit), shell radius curve, cache blocks
    qual: vec4<f32>,        // back-face share of the dense samples, _, _, _
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

// Tonemap on the brightest channel so saturated gold and crimson keep their
// hue, then let only the very brightest highlights drift toward white.
fn tonemap(hdr: vec3<f32>) -> vec3<f32> {
    let m = max(max(hdr.r, hdr.g), max(hdr.b, 1e-5));
    let mapped = 1.0 - exp(-m);
    let col = hdr * (mapped / m);
    return mix(col, vec3<f32>(mapped), smoothstep(0.75, 1.0, mapped) * 0.3);
}

// Display encoding (sRGB outputs encode in hardware).
fn encode_out(c: vec3<f32>) -> vec3<f32> {
    let col = max(c, vec3<f32>(0.0));
    if (G.post2.z > 0.5) {
        return pow(col, vec3<f32>(1.0 / 2.2));
    }
    return col;
}

// Replaces NaN and infinite components with 0. One bad pixel would otherwise
// be spread over the screen by the bloom and kept forever by the frame
// feedback (NaN * 0 is NaN).
fn finite(c: vec3<f32>) -> vec3<f32> {
    let bits = bitcast<vec3<u32>>(c) & vec3<u32>(0x7fffffffu);
    return select(c, vec3<f32>(0.0), bits >= vec3<u32>(0x7f800000u));
}

// Levels of the shell image pyramid (world.rs PYRAMID_LEVELS).
const PYRAMID_LEVELS: u32 = 8u;

// First texture row of pyramid level `level` (odd levels in texture A, even in
// B, stacked in order) for a padded ring list of `r0` entries.
fn pyr_y0(level: u32, r0: u32) -> u32 {
    var y = 0u;
    for (var k = 2u - (level & 1u); k < level; k += 2u) {
        y += r0 >> k;
    }
    return y;
}

fn hash12(p: vec2<f32>) -> f32 {
    var p3 = fract(vec3<f32>(p.xyx) * 0.1031);
    p3 += dot(p3, p3.yzx + 33.33);
    return fract((p3.x + p3.y) * p3.z);
}
