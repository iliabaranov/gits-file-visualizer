// Background gradient, motion smear, bloom and final tonemapping.

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
    let prev = textureSampleLevel(t2, samp, in.uv, 0.0).rgb;
    return vec4<f32>(max(bg + scene, prev * G.post2.x), 1.0);
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

@fragment
fn fs_final(in: FullOut) -> @location(0) vec4<f32> {
    let base = textureSampleLevel(t0, samp, in.uv, 0.0).rgb;
    let bloom = textureSampleLevel(t1, samp, in.uv, 0.0).rgb * G.post2.w;
    let hdr = (base + bloom * G.post.y) * G.post.x;
    // Tonemap on the brightest channel so saturated gold and crimson keep their
    // hue, then let only the very brightest highlights drift toward white.
    let m = max(max(hdr.r, hdr.g), max(hdr.b, 1e-5));
    let mapped = 1.0 - exp(-m);
    var col = hdr * (mapped / m);
    col = mix(col, vec3<f32>(mapped), smoothstep(0.75, 1.0, mapped) * 0.3);

    let n = hash12(in.pos.xy + vec2<f32>(G.post2.y * 17.0, G.post2.y * 29.0)) - 0.5;
    col += n * G.post.z * (0.15 + col * 0.6);

    let v = length((in.uv - 0.5) * vec2<f32>(1.1, 1.0));
    col *= mix(1.0, smoothstep(0.95, 0.2, v), G.post.w);

    col = max(col, vec3<f32>(0.0));
    if (G.post2.z > 0.5) {
        col = pow(col, vec3<f32>(1.0 / 2.2));
    }
    return vec4<f32>(col, 1.0);
}
