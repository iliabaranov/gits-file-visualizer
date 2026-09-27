// The tower: a few concentric cylinders (one per depth layer) whose fragment
// shader looks up which ring and which file segment it is showing. Millions of
// segments cost a handful of draw calls; everything animates from `time`.

struct Segment {
    a0: f32, a1: f32, ring: u32, flags: u32,
    age_days: f32, size_f: f32, hash: f32, t_create: f32,
    t_read: f32, t_write: f32, t_delete: f32, pad: f32,
};

struct Ring {
    depth: f32, speed: f32, phase0: f32, boost: f32,
    boost_t0: f32, hash: f32, pad0: f32, pad1: f32,
};

@group(1) @binding(0) var<storage, read> segs: array<Segment>;
@group(1) @binding(1) var<storage, read> rings: array<Ring>;
@group(1) @binding(2) var<storage, read> ring_open: array<f32>;
@group(1) @binding(3) var<storage, read> lut: array<f32>;
@group(1) @binding(4) var atlas: texture_2d<u32>;

const SEG_DIR: u32 = 1u;
const SEG_AGG: u32 = 2u;
const SEG_SYMLINK: u32 = 4u;
const SEG_HIDDEN: u32 = 8u;
const SEG_DELETED: u32 = 16u;
// Max tilt of an unwound ring, in ring pitches (mirrored in world.rs).
const TILT: f32 = 0.3;

struct TowerOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) world: vec3<f32>,
    @location(1) @interpolate(flat) layer: u32,
};

@vertex
fn vs_tower(@location(0) p: vec3<f32>, @builtin(instance_index) layer: u32) -> TowerOut {
    let r = G.tower.y * (1.0 - G.tower.z * f32(layer));
    let world = vec3<f32>(p.x * r, G.tower.w - p.y * G.tower.x, p.z * r);
    var o: TowerOut;
    o.clip = G.view_proj * vec4<f32>(world, 1.0);
    o.world = world;
    o.layer = layer;
    return o;
}

// Soft-attack, exponential-release envelope; `fade` is the time to ~95% faded.
fn env(dt: f32, fade: f32) -> f32 {
    if (dt < 0.0) {
        return 0.0;
    }
    return smoothstep(0.0, max(G.times2.x, 1e-3), dt) * exp(-3.0 * dt / max(fade, 1e-3));
}

fn age_color(age_days: f32) -> vec3<f32> {
    let x = log2(max(age_days, 0.001)) * 0.30103;
    let a = smoothstep(G.age.x, G.age.y, x);
    let b = smoothstep(G.age.y, G.age.z, x);
    return mix(mix(G.col_fresh.rgb, G.col_old.rgb, a), G.col_stale.rgb, b);
}

fn ring_phase(r: Ring, t: f32) -> f32 {
    let bt = max(t - r.boost_t0, 0.0);
    return r.phase0 + r.speed * t + r.boost * (1.0 - exp(-bt / G.lut.w));
}

// Colour of a segment at position `u` (0..1) along its arc; alpha = visibility.
fn seg_color(s: Segment, u: f32, open: f32, t: f32) -> vec4<f32> {
    var vis = 1.0;
    let idle = mix(G.idle.x, G.idle.y, open) * (0.55 + 0.45 * s.hash);
    var col: vec3<f32>;
    if ((s.flags & SEG_DIR) != 0u) {
        col = G.col_fresh.rgb * G.idle.z;
    } else {
        col = age_color(s.age_days) * idle;
    }
    // Unwound rings are "being scanned": brighter, with some arcs gone cream-white.
    let scanned = open * step(0.7, fract(s.hash * 7.31));
    col = mix(col * (1.0 + 0.5 * open), G.col_read.rgb * idle * 1.15, scanned);
    if ((s.flags & SEG_HIDDEN) != 0u) {
        col *= 0.65;
    }

    // Creation: grow along the arc with a bright leading tip.
    let tc = t - s.t_create;
    if (tc >= 0.0 && tc < G.times.z) {
        let g = tc / G.times.z;
        let gs = g * g * (3.0 - 2.0 * g);
        if (u > gs) {
            vis = 0.0;
        }
        col += G.col_create.rgb * G.col_create.w * exp(-abs(u - gs) * 30.0) * (1.0 - g) * 1.5;
    }

    // Write: turns the segment crimson, fading back to its idle colour.
    let kw = env(t - s.t_write, G.times.y);
    col = mix(col, G.col_write.rgb * G.col_write.w * max(idle, 0.45), clamp(kw, 0.0, 1.0));
    col += G.col_write.rgb * G.col_write.w * env(t - s.t_write, 1.5) * 0.6;

    // Read: cream flash plus a long, faint afterglow.
    let kr = env(t - s.t_read, 1.6) * 0.8 + env(t - s.t_read, G.times.x) * 0.25;
    col += G.col_read.rgb * G.col_read.w * kr;

    // Delete: brief flash, then a faint ghost in the gap.
    if ((s.flags & SEG_DELETED) != 0u) {
        let td = max(t - s.t_delete, 0.0);
        let flash = exp(-td / 0.35) * G.col_delete.w;
        let ghost = G.idle.w * exp(-3.0 * td / max(G.times.w, 1.0));
        col = G.col_delete.rgb * (flash + ghost);
        vis = 1.0;
    }
    return vec4<f32>(col, vis);
}

@fragment
fn fs_tower(in: TowerOut, @builtin(front_facing) front: bool) -> @location(0) vec4<f32> {
    let h = G.tower.x;
    let bins = G.lut.x;
    let n_rings = u32(G.lut.y);
    let atlas_w = G.lut.z;
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

    // Where several rings share a pixel, average a few of them (the "drum").
    // The far side is dimmer and seen through the near side: one sample is enough.
    var count = 1;
    if (dr_px > 1.0 && front) {
        count = i32(clamp(ceil(dr_px), 2.0, 3.0));
    }
    var acc = vec3<f32>(0.0);
    for (var k = 0; k < count; k++) {
        var rr = r;
        if (count > 1) {
            rr = r + ((f32(k) + 0.5) / f32(count) - 0.5) * dr_px;
        }
        if (rr < 0.0) {
            continue;
        }
        let ri = u32(rr);
        if (ri >= n_rings) {
            continue;
        }
        let ring = rings[ri];
        let layer = min(u32(ring.depth), u32(G.tower2.x) - 1u);
        if (layer != in.layer) {
            continue;
        }
        let turn = fract(turn0 - ring_phase(ring, t));
        let x = min(u32(turn * atlas_w), u32(atlas_w) - 1u);
        let id = textureLoad(atlas, vec2<u32>(x, ri), 0).r;
        if (id == 0u) {
            continue;
        }
        let sg = segs[id - 1u];
        let open = ring_open[ri];
        // Unwound segments shrink toward their centre, opening wider gaps.
        let shrink = 0.5 * (sg.a1 - sg.a0) * 0.12 * open;
        let e = min(turn - sg.a0, sg.a1 - turn) - shrink;
        let hcov = clamp(e / dturn_px + 0.5, 0.0, 1.0);
        if (hcov <= 0.0) {
            continue;
        }
        let u = clamp((turn - sg.a0) / max(sg.a1 - sg.a0, 1e-5), 0.0, 1.0);
        let fill = mix(G.tower2.z, G.times2.y, open);
        let hw = 0.5 * min(pitch * fill, G.tower2.y * (0.45 + 0.55 * sg.size_f));
        var vcov: f32;
        if (count > 1) {
            vcov = clamp(2.0 * hw / pitch, 0.0, 1.0);
        } else {
            // Unwound rings tilt slightly (random axis per ring), like a loose coil.
            let tilt = TILT * open * sin(turn0 * TAU - ring.hash * TAU);
            let d = abs(fract(rr) - 0.5 - tilt) * pitch;
            vcov = clamp((hw - d) / ds_px + 0.5, 0.0, 1.0);
            // soft halo around unwound ribbons: bright core, glowing edges
            vcov += 0.22 * open * exp(-pow(d / max(hw * 2.5, ds_px), 2.0));
        }
        var c = seg_color(sg, u, open, t);
        if ((sg.flags & (SEG_AGG | SEG_SYMLINK)) != 0u) {
            let dashes = max((sg.a1 - sg.a0) * 90.0, 2.0);
            c.a *= step(0.45, fract(u * dashes));
        }
        acc += c.rgb * c.a * hcov * vcov;
    }
    acc /= f32(count);
    if (!front) {
        acc *= G.tower2.w;
    }
    let ends = smoothstep(0.0, 0.03 * h, s) * smoothstep(0.0, 0.03 * h, h - s);
    return vec4<f32>(acc * ends, 1.0);
}
