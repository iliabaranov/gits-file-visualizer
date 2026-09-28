// The tower: a few concentric cylinders (one per depth layer) whose fragment
// shader works out which ring it is showing.
// - Sparse rings (unwound, several pixels apart) are drawn exactly: the ring's
//   file segment under the pixel is looked up and coloured per pixel.
// - Dense rings (the packed drum, many rings per pixel) average a few samples
//   of the ring colour texture (ringcolor.wgsl), which holds each ring's
//   colours per angle bin. Cheap, and stable while rings spin.

@group(2) @binding(0) var ring_tex: texture_2d<f32>;
@group(2) @binding(1) var ring_samp: sampler;

// Rings closer than this many per pixel start using the dense path.
const DENSE_FROM: f32 = 0.7;
const DENSE_TO: f32 = 1.4;
// Vertical taps in the dense path: one per ring covered by the pixel, up to
// G.times2.w (16 on discrete GPUs, 4 on integrated), i.e. a box filter over the
// rings in the pixel's footprint. Fewer taps would pick a random subset of the
// rings, which crawls like sensor noise as the rings turn.

struct TowerOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) world: vec3<f32>,
    @location(1) @interpolate(flat) layer: u32,
};

// One instance per band: each depth layer's cylinder covers only the heights
// where that layer has rings, so most pixels shade one or two layers, not all.
@vertex
fn vs_tower(@location(0) p: vec3<f32>, @builtin(instance_index) ii: u32) -> TowerOut {
    let band = bands[ii];
    let layer = u32(band.z);
    let s = mix(band.x, band.y, p.y);
    let r = G.tower.y * (1.0 - G.tower.z * f32(layer));
    let world = vec3<f32>(p.x * r, G.tower.w - s, p.z * r);
    var o: TowerOut;
    o.clip = G.view_proj * vec4<f32>(world, 1.0);
    o.world = world;
    o.layer = layer;
    return o;
}

// Exact colour of the ring at fractional ring coordinate `rr` (sparse path).
fn exact(in: TowerOut, rr: f32, turn0: f32, pitch: f32, ds_px: f32, dturn_px: f32, t: f32) -> vec3<f32> {
    let n_rings = u32(G.lut.y);
    let atlas_w = G.lut.z;
    if (rr < 0.0) {
        return vec3<f32>(0.0);
    }
    let ri = u32(rr);
    if (ri >= n_rings) {
        return vec3<f32>(0.0);
    }
    let ring = rings[ri];
    if (min(u32(ring.depth), u32(G.tower2.x) - 1u) != in.layer) {
        return vec3<f32>(0.0);
    }
    let turn = fract(turn0 - ring_phase(ring, t));
    let x = min(u32(turn * atlas_w), u32(atlas_w) - 1u);
    let local = textureLoad(atlas, vec2<u32>(x, ring.row), 0).r;
    if (local == 0u) {
        return vec3<f32>(0.0);
    }
    let sg = segs[ring.first_seg + local - 1u];
    let open = ring_open[ri];
    // Unwound segments shrink toward their centre, opening wider gaps.
    let shrink = 0.5 * (sg.a1 - sg.a0) * 0.12 * open;
    let e = min(turn - sg.a0, sg.a1 - turn) - shrink;
    let hcov = clamp(e / dturn_px + 0.5, 0.0, 1.0);
    if (hcov <= 0.0) {
        return vec3<f32>(0.0);
    }
    let u = clamp((turn - sg.a0) / max(sg.a1 - sg.a0, 1e-5), 0.0, 1.0);
    let fill = mix(G.tower2.z, G.times2.y, open);
    let hw = 0.5 * min(pitch * fill, G.tower2.y * (0.45 + 0.55 * sg.size_f));
    // Unwound rings tilt slightly (random axis per ring), like a loose coil.
    let tilt = TILT * open * sin(turn0 * TAU - ring.hash * TAU);
    let d = abs(fract(rr) - 0.5 - tilt) * pitch;
    var vcov = clamp((hw - d) / ds_px + 0.5, 0.0, 1.0);
    // Soft halo around unwound ribbons: bright core, glowing edges.
    vcov += 0.22 * open * exp(-pow(d / max(hw * 2.5, ds_px), 2.0));
    var c = seg_color(sg, u, open, t);
    c.a *= dash(sg, u);
    return c.rgb * c.a * hcov * vcov;
}

// Average colour of this layer's rings covered by the pixel (dense path).
fn dense(in: TowerOut, r: f32, dr_px: f32, turn0: f32, max_taps: f32, t: f32) -> vec3<f32> {
    let n_rings = u32(G.lut.y);
    let rows = G.rct.x;
    let n = i32(clamp(ceil(dr_px), 1.0, max_taps));
    var acc = vec3<f32>(0.0);
    for (var k = 0; k < n; k++) {
        let rr = r + ((f32(k) + 0.5) / f32(n) - 0.5) * dr_px;
        if (rr < 0.0) {
            continue;
        }
        let ri = u32(rr);
        if (ri >= n_rings) {
            continue;
        }
        let ring = rings[ri];
        if (min(u32(ring.depth), u32(G.tower2.x) - 1u) != in.layer) {
            continue;
        }
        let u = fract(turn0 - ring_phase(ring, t));
        acc += textureSampleLevel(ring_tex, ring_samp, vec2<f32>(u, (f32(ring.row) + 0.5) / rows), 0.0).rgb;
    }
    // Packed rings cover `fill` of their pitch.
    return acc * (G.tower2.z / f32(n));
}

@fragment
fn fs_tower(in: TowerOut, @builtin(front_facing) front: bool) -> @location(0) vec4<f32> {
    let h = G.tower.x;
    let bins = G.lut.x;
    let t = G.cam_pos.w;

    // Derivatives first (uniform control flow).
    let s = G.tower.w - in.world.y;
    let ds_px = max(fwidth(s), 1e-5);
    let radius = max(length(in.world.xz), 1e-3);
    let darc_px = max(length(fwidth(in.world.xz)), 1e-5);

    let fb = clamp(s / h * bins, 0.0, bins - 1.001);
    let b = u32(fb);
    let r0 = lut[b];
    let r1 = lut[b + 1u];
    let r = mix(r0, r1, fract(fb));
    let drds = max((r1 - r0) * bins / h, 1e-5);
    let pitch = 1.0 / drds;
    let dr_px = ds_px * drds;
    let turn0 = atan2(in.world.z, in.world.x) / TAU + 0.5;
    let dturn_px = darc_px / (TAU * radius);

    // Blend from the exact path to the dense path as rings get closer than a pixel.
    let w = smoothstep(DENSE_FROM, DENSE_TO, dr_px);
    var acc = vec3<f32>(0.0);
    if (w < 1.0) {
        acc += exact(in, r, turn0, pitch, ds_px, dturn_px, t) * (1.0 - w);
    }
    if (w > 0.0) {
        let taps = select(max(G.times2.w * 0.5, 1.0), G.times2.w, front || G.tower2.w > 0.99);
        acc += dense(in, r, dr_px, turn0, taps, t) * w;
    }
    if (!front) {
        acc *= G.tower2.w;
    }
    let ends = smoothstep(0.0, 0.03 * h, s) * smoothstep(0.0, 0.03 * h, h - s);
    return vec4<f32>(acc * ends, 1.0);
}
