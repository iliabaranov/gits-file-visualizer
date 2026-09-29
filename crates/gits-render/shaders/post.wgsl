// Background gradient, motion smear, bloom, upscaling and final tonemapping.

@group(1) @binding(0) var t0: texture_2d<f32>;
@group(1) @binding(1) var t1: texture_2d<f32>;
@group(1) @binding(2) var t2: texture_2d<f32>;
@group(1) @binding(3) var samp: sampler;

@fragment
fn fs_bg(in: FullOut) -> @location(0) vec4<f32> {
    let aspect = G.screen.x / G.screen.y;
    var col = G.col_bg.rgb;
    let d = length((in.uv - vec2<f32>(0.5, 0.55)) * vec2<f32>(aspect, 1.0));
    col += G.col_fresh.rgb * 0.007 * exp(-d * d * 5.0);
    let floor_haze = exp(-pow((in.uv.y - 0.93) * 6.0, 2.0));
    col += G.col_teal.rgb * 0.007 * floor_haze;
    return vec4<f32>(col, 1.0);
}

// Current frame (background + scene) combined with the decayed previous frame:
// moving highlights leave short trails, static content is unchanged.
@fragment
fn fs_combine(in: FullOut) -> @location(0) vec4<f32> {
    let bg = textureSampleLevel(t0, samp, in.uv, 0.0).rgb;
    let scene = textureSampleLevel(t1, samp, in.uv, 0.0).rgb;
    var col = finite(bg + scene);
    if (G.post2.x > 0.0) {
        col = max(col, finite(textureSampleLevel(t2, samp, in.uv, 0.0).rgb) * G.post2.x);
    }
    return vec4<f32>(min(col, vec3<f32>(6.0e4)), 1.0);
}

fn tap(uv: vec2<f32>) -> vec3<f32> {
    return textureSampleLevel(t0, samp, uv, 0.0).rgb;
}

// 13-tap downsample (Jimenez 2014).
@fragment
fn fs_down(in: FullOut) -> @location(0) vec4<f32> {
    let ts = 1.0 / vec2<f32>(textureDimensions(t0));
    let uv = in.uv;
    let a = tap(uv + ts * vec2<f32>(-2.0, -2.0));
    let b = tap(uv + ts * vec2<f32>(0.0, -2.0));
    let c = tap(uv + ts * vec2<f32>(2.0, -2.0));
    let d = tap(uv + ts * vec2<f32>(-2.0, 0.0));
    let e = tap(uv);
    let f = tap(uv + ts * vec2<f32>(2.0, 0.0));
    let g = tap(uv + ts * vec2<f32>(-2.0, 2.0));
    let h = tap(uv + ts * vec2<f32>(0.0, 2.0));
    let i = tap(uv + ts * vec2<f32>(2.0, 2.0));
    let j = tap(uv + ts * vec2<f32>(-1.0, -1.0));
    let k = tap(uv + ts * vec2<f32>(1.0, -1.0));
    let l = tap(uv + ts * vec2<f32>(-1.0, 1.0));
    let m = tap(uv + ts * vec2<f32>(1.0, 1.0));
    let o = e * 0.125 + (a + c + g + i) * 0.03125 + (b + d + f + h) * 0.0625 + (j + k + l + m) * 0.125;
    return vec4<f32>(o, 1.0);
}

// 3x3 tent upsample, added onto the next larger level.
@fragment
fn fs_up(in: FullOut) -> @location(0) vec4<f32> {
    let ts = 1.0 / vec2<f32>(textureDimensions(t0));
    let uv = in.uv;
    var o = tap(uv) * 4.0;
    o += (tap(uv + vec2<f32>(ts.x, 0.0)) + tap(uv - vec2<f32>(ts.x, 0.0)) + tap(uv + vec2<f32>(0.0, ts.y)) + tap(uv - vec2<f32>(0.0, ts.y))) * 2.0;
    o += tap(uv + ts) + tap(uv - ts) + tap(uv + vec2<f32>(ts.x, -ts.y)) + tap(uv + vec2<f32>(-ts.x, ts.y));
    return vec4<f32>(o / 16.0, 1.0);
}

// Upscales the scene from its internal resolution (the spatial part of
// FSR 1-style upscalers): bicubic Catmull-Rom from 5 bilinear taps, a little
// sharpening against the bilinear value, all clamped to the 2x2 nearest
// texels so bright thin lines neither ring nor grow halos.
fn upscale(uv: vec2<f32>) -> vec3<f32> {
    let size = vec2<f32>(textureDimensions(t0));
    let sp = uv * size;
    let p1 = floor(sp - 0.5) + 0.5;
    let f = sp - p1;
    let w0 = f * (-0.5 + f * (1.0 - 0.5 * f));
    let w1 = 1.0 + f * f * (-2.5 + 1.5 * f);
    let w2 = f * (0.5 + f * (2.0 - 1.5 * f));
    let w3 = f * f * (-0.5 + 0.5 * f);
    let w12 = w1 + w2;
    let q0 = (p1 - 1.0) / size;
    let q3 = (p1 + 2.0) / size;
    let q12 = (p1 + w2 / w12) / size;
    let ka = w12.x * w0.y;
    let kb = w0.x * w12.y;
    let kc = w12.x * w12.y;
    let kd = w3.x * w12.y;
    let ke = w12.x * w3.y;
    var c = tap(vec2<f32>(q12.x, q0.y)) * ka + tap(vec2<f32>(q0.x, q12.y)) * kb + tap(q12) * kc
        + tap(vec2<f32>(q3.x, q12.y)) * kd + tap(vec2<f32>(q12.x, q3.y)) * ke;
    c /= ka + kb + kc + kd + ke;
    // The 2x2 nearest texels, one gather per channel: x = (u0, v1), y = (u1, v1),
    // z = (u1, v0), w = (u0, v0). They give the bilinear value and the clamp.
    let gc = (p1 + 0.5) / size;
    let r = textureGather(0, t0, samp, gc);
    let g = textureGather(1, t0, samp, gc);
    let b = textureGather(2, t0, samp, gc);
    let bilinear = vec3<f32>(
        mix(mix(r.w, r.z, f.x), mix(r.x, r.y, f.x), f.y),
        mix(mix(g.w, g.z, f.x), mix(g.x, g.y, f.x), f.y),
        mix(mix(b.w, b.z, f.x), mix(b.x, b.y, f.x), f.y),
    );
    c += (c - bilinear) * G.rct.w;
    let lo = vec3<f32>(min(min(r.x, r.y), min(r.z, r.w)), min(min(g.x, g.y), min(g.z, g.w)), min(min(b.x, b.y), min(b.z, b.w)));
    let hi = vec3<f32>(max(max(r.x, r.y), max(r.z, r.w)), max(max(g.x, g.y), max(g.z, g.w)), max(max(b.x, b.y), max(b.z, b.w)));
    return clamp(c, lo, hi);
}

fn finish(base: vec3<f32>, in: FullOut) -> vec4<f32> {
    let bloom = textureSampleLevel(t1, samp, in.uv, 0.0).rgb * G.post2.w;
    var col = tonemap((base + bloom * G.post.y) * G.post.x);

    let n = hash12(in.pos.xy + vec2<f32>(G.post2.y * 17.0, G.post2.y * 29.0)) - 0.5;
    col += n * G.post.z * (0.15 + col * 0.6);

    let v = length((in.uv - 0.5) * vec2<f32>(1.1, 1.0));
    col *= mix(1.0, smoothstep(0.95, 0.2, v), G.post.w);
    return vec4<f32>(encode_out(col), 1.0);
}

// Output at the internal resolution.
@fragment
fn fs_final(in: FullOut) -> @location(0) vec4<f32> {
    return finish(textureSampleLevel(t0, samp, in.uv, 0.0).rgb, in);
}

// Output larger than the internal resolution.
@fragment
fn fs_final_up(in: FullOut) -> @location(0) vec4<f32> {
    return finish(upscale(in.uv), in);
}
