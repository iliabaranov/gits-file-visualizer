// The tower: concentric cylinders, one per folder depth (the root's files
// nearest the axis, deeper folders further out), whose fragment shader works
// out which ring it is showing.
// - Sparse rings (unwound, several pixels apart) are drawn exactly: the ring's
//   file segment under the pixel is looked up and coloured per pixel.
// - Dense rings (the packed drum, many rings per pixel) average a few samples
//   of the ring colour texture (ringcolor.wgsl), which holds each ring's
//   colours per angle bin. Cheap, and stable while rings spin.

@group(2) @binding(0) var ring_tex: texture_2d<f32>;
@group(2) @binding(1) var ring_samp: sampler;
// Shell image pyramid (pyramid.wgsl): odd levels in A, even levels in B.
@group(2) @binding(2) var pyr_a: texture_2d<f32>;
@group(2) @binding(3) var pyr_b: texture_2d<f32>;
@group(2) @binding(4) var pyr_samp: sampler;

// Rings closer than this many per pixel start using the dense path.
const DENSE_FROM: f32 = 0.7;
const DENSE_TO: f32 = 1.4;
// Vertical taps in the dense path: one per ring covered by the pixel, up to
// G.times2.w (16 on discrete GPUs, 4 on integrated), i.e. a box filter over the
// rings in the pixel's footprint. Fewer taps would pick a random subset of the
// rings, which crawls like sensor noise as the rings turn.

// Position in layer_index's ring list of layer `layer`'s first ring at or
// after fractional ring coordinate `rr`.
fn layer_pos(layer: u32, rr: f32) -> u32 {
    let n = u32(G.lut.y);
    let i = u32(clamp(ceil(rr), 0.0, f32(n)));
    return layer_index[layer * (n + 1u) + i];
}

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
    let r = mix(G.tower.z, G.tower.y, f32(layer) / max(G.tower2.x - 1.0, 1.0));
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

// Box filter over the pixel's footprint [lo, hi] (ring coordinates): the sum
// of this layer's rings' colours weighted by how much of the footprint each
// covers (dense path). Only this layer's rings are visited (from its list in
// layer_index), every `stride`-th one when there are more than `max_taps`;
// the stride is counted from the start of the list, so as rings move the
// chosen ones stay the same and only the ends of the footprint change.
// Taps read ring_list sequentially (one load per tap, phase precomputed).
fn dense(lo: f32, hi: f32, p0: u32, p1: u32, turn0: f32, max_taps: f32) -> vec3<f32> {
    let list_at = u32(G.tower2.x) * (u32(G.lut.y) + 1u);
    let dr = hi - lo;
    let stride = u32(max(ceil(dr / max_taps), 1.0));
    var acc = vec3<f32>(0.0);
    var p = (p0 + stride - 1u) / stride * stride;
    for (var k = 0u; k <= u32(max_taps) && p < p1; k++) {
        let e = ring_list[p - list_at];
        let cover = clamp(min(hi, e.z + 1.0) - max(lo, e.z), 0.0, 1.0);
        acc += textureSampleLevel(ring_tex, ring_samp, vec2<f32>(fract(turn0 - e.y), e.x), 0.0).rgb * cover;
        p += stride;
    }
    // Packed rings cover `fill` of their pitch.
    return acc * (G.tower2.z * f32(stride) / max(dr, 1e-3));
}

// Colour of the ring at list position `p` (absolute index into layer_index)
// at world angle `turn0`, and in w its ring index.
fn ring_at(p: u32, list_at: u32, turn0: f32) -> vec4<f32> {
    let e = ring_list[p - list_at];
    return vec4<f32>(textureSampleLevel(ring_tex, ring_samp, vec2<f32>(fract(turn0 - e.y), e.x), 0.0).rgb, e.z);
}

// Average colour of `2^level` rings starting at list block `row`, at shell angle `us`.
fn pyr_at(level: u32, row: u32, us: f32) -> vec3<f32> {
    let bins = G.rct.y;
    let x = (1.0 + us * bins) / (bins + 2.0);
    let y = f32(pyr_y0(level, u32(G.pyr.y)) + row) + 0.5;
    if ((level & 1u) == 1u) {
        return textureSampleLevel(pyr_a, pyr_samp, vec2<f32>(x, y / G.pyr.z), 0.0).rgb;
    }
    return textureSampleLevel(pyr_b, pyr_samp, vec2<f32>(x, y / G.pyr.w), 0.0).rgb;
}

// Most blocks read per pixel and shell by dense_cached (enough for footprints
// of ~2000 rings; beyond, the rest is extrapolated).
const PYR_MAX_STEPS: u32 = 24u;

// The same box filter as `dense`, exact and cheaper when the whole tower
// turns as one: the two partly covered rings at the ends come from the ring
// colour texture, the fully covered ones in between from the shell image
// pyramid, as aligned blocks of 1, 2, 4 … 256 rings.
fn dense_cached(layer: u32, lo: f32, hi: f32, turn0: f32) -> vec3<f32> {
    let n = u32(G.lut.y);
    let list_at = u32(G.tower2.x) * (n + 1u);
    let end = layer_index[layer * (n + 1u) + n];
    let fl = floor(lo);
    let fh = floor(hi);
    var acc = vec3<f32>(0.0);
    let pa = layer_pos(layer, fl);
    if (pa < end) {
        let c = ring_at(pa, list_at, turn0);
        if (c.w == fl) {
            acc += c.rgb * (min(hi, fl + 1.0) - lo);
        }
    }
    if (fh > fl) {
        let pb = layer_pos(layer, fh);
        if (pb < end) {
            let c = ring_at(pb, list_at, turn0);
            if (c.w == fh) {
                acc += c.rgb * (hi - fh);
            }
        }
    }
    let q0 = layer_pos(layer, fl + 1.0) - list_at;
    let q1 = layer_pos(layer, fh) - list_at;
    let us = fract(turn0 - G.pyr.x);
    var a = q0;
    for (var k = 0u; k < PYR_MAX_STEPS && a < q1; k++) {
        var level = select(firstTrailingBit(a), PYRAMID_LEVELS, a == 0u);
        level = min(min(level, PYRAMID_LEVELS), firstLeadingBit(q1 - a));
        if (level == 0u) {
            acc += ring_at(a + list_at, list_at, turn0).rgb;
        } else {
            acc += pyr_at(level, a >> level, us) * f32(1u << level);
        }
        a += 1u << level;
    }
    if (a < q1) {
        acc *= f32(q1 - q0 + 2u) / f32(a - q0 + 2u);
    }
    // Packed rings cover `fill` of their pitch.
    return acc * (G.tower2.z / max(hi - lo, 1e-3));
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
    // Layers interleave finely wherever folders of different depths alternate,
    // so a layer's bands cover heights where it has no rings: skip those.
    let half = 0.5 * max(dr_px, 1.0);
    let p0 = layer_pos(in.layer, r - half - 1.0);
    let p1 = layer_pos(in.layer, r + half);
    if (p1 == p0) {
        discard;
    }

    // Blend from the exact path to the dense path as rings get closer than a pixel.
    let w = smoothstep(DENSE_FROM, DENSE_TO, dr_px);
    var acc = vec3<f32>(0.0);
    if (w < 1.0) {
        acc += exact(in, r, turn0, pitch, ds_px, dturn_px, t) * (1.0 - w);
    }
    if (w > 0.0) {
        if (G.pyr.y > 0.0) {
            acc += dense_cached(in.layer, r - 0.5 * dr_px, r + 0.5 * dr_px, turn0) * w;
        } else {
            let taps = select(max(G.times2.w * 0.5, 1.0), G.times2.w, front || G.tower2.w > 0.99);
            acc += dense(r - 0.5 * dr_px, r + 0.5 * dr_px, p0, p1, turn0, taps) * w;
        }
    }
    if (!front) {
        acc *= G.tower2.w;
    } else if (G.tower3.x < 0.999 && G.tower2.w < 0.99) {
        // An outer shell's near side is dimmed where inner shells are behind
        // it, so the nested shells read as nested (not in the dial, which
        // looks at the shells from inside).
        let mask = u32(lut[u32(bins) + 1u + b]);
        if ((mask & ((1u << in.layer) - 1u)) != 0u) {
            acc *= G.tower3.x;
        }
    }
    // Optional scan: a soft band of light travelling up the tower now and then.
    if (G.tower3.y > 0.0) {
        let p = (t % G.tower3.y) / G.tower3.z;
        if (p < 1.0) {
            let band = exp(-pow((s - h * (1.0 - p)) / (0.03 * h), 2.0));
            acc *= 1.0 + G.tower3.w * band * smoothstep(0.0, 0.06, p) * smoothstep(1.0, 0.94, p);
        }
    }
    let ends = smoothstep(0.0, 0.03 * h, s) * smoothstep(0.0, 0.03 * h, h - s);
    return vec4<f32>(acc * ends, 1.0);
}
