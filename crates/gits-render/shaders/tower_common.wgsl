// Shared by the tower pass (tower.wgsl) and the ring colour pass
// (ringcolor.wgsl): segment/ring data and the colour of a segment.

struct Segment {
    a0: f32, a1: f32, ring: u32, flags: u32,
    age_days: f32, size_f: f32, hash: f32, t_create: f32,
    t_read: f32, t_write: f32, t_delete: f32, t_retract: f32,
    age_from: f32, t_age: f32, pad0: f32, pad1: f32,
};

struct Ring {
    depth: f32, speed: f32, phase0: f32, boost: f32,
    boost_t0: f32, hash: f32, first_seg: u32, row: u32,
};

@group(1) @binding(0) var<storage, read> segs: array<Segment>;
@group(1) @binding(1) var<storage, read> rings: array<Ring>;
@group(1) @binding(2) var<storage, read> ring_open: array<f32>;
@group(1) @binding(3) var<storage, read> lut: array<f32>;
@group(1) @binding(4) var atlas: texture_2d<u32>;
// Per depth layer, the height intervals holding its rings: (s0, s1, layer, _).
@group(1) @binding(5) var<storage, read> bands: array<vec4<f32>>;
// Ring index of each atlas row ("slot"); 0xffffffff = free. Rings keep their
// row across relayouts, so only changed rings need new atlas and colour rows.
@group(1) @binding(6) var<storage, read> slot_ring: array<u32>;
// Each depth layer's rings (see World::layer_index): for layer L and ring
// index i, layer_index[L * (n + 1) + i] is where L's first ring at or after i
// sits in the list of ring indices that follows the table.
@group(1) @binding(7) var<storage, read> layer_index: array<u32>;
// The rings of layer_index's list, in its order, for the current frame:
// (colour texture v, fract(ring phase), ring index, 0).
@group(1) @binding(8) var<storage, read> ring_list: array<vec4<f32>>;

const SEG_DIR: u32 = 1u;
const SEG_AGG: u32 = 2u;
const SEG_SYMLINK: u32 = 4u;
const SEG_HIDDEN: u32 = 8u;
const SEG_DELETED: u32 = 16u;
// Max tilt of an unwound ring, in ring pitches (mirrored in world.rs).
const TILT: f32 = 0.3;
// Seconds for a segment to shrink back along its arc when removed at loop end.
const RETRACT: f32 = 2.0;
// Seconds over which a changed age (colour) blends to its new value.
const AGE_BLEND: f32 = 20.0;

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

fn seg_age(s: Segment, t: f32) -> f32 {
    if (s.t_age < -1.0e8) {
        return s.age_days;
    }
    let k = clamp((t - s.t_age) / AGE_BLEND, 0.0, 1.0);
    let ks = k * k * (3.0 - 2.0 * k);
    return exp2(mix(log2(max(s.age_from, 1e-3)), log2(max(s.age_days, 1e-3)), ks));
}

// Dashed arcs for aggregates (collapsed folders) and symlinks.
fn dash(s: Segment, u: f32) -> f32 {
    if ((s.flags & (SEG_AGG | SEG_SYMLINK)) == 0u) {
        return 1.0;
    }
    let dashes = max((s.a1 - s.a0) * 90.0, 2.0);
    return step(0.45, fract(u * dashes));
}

fn ring_phase(r: Ring, t: f32) -> f32 {
    // Write boost: extra rotation that eases in and out over G.lut.w seconds.
    let x = clamp((t - r.boost_t0) / G.lut.w, 0.0, 1.0);
    return r.phase0 + r.speed * t + r.boost * x * x * (3.0 - 2.0 * x);
}

// Colour of a segment at position `u` (0..1) along its arc; alpha = visibility.
fn seg_color(s: Segment, u: f32, open: f32, t: f32) -> vec4<f32> {
    var vis = 1.0;
    // Fades every event highlight out while a loop winds down.
    let lf = G.times2.z;
    let idle = mix(G.idle.x, G.idle.y, open) * (0.55 + 0.45 * s.hash);
    var col: vec3<f32>;
    if ((s.flags & SEG_DIR) != 0u) {
        col = G.col_fresh.rgb * G.idle.z;
    } else {
        col = age_color(seg_age(s, t)) * idle;
    }
    // Unwound rings are "being scanned": brighter, with some arcs gone cream-white.
    let scanned = open * step(0.7, fract(s.hash * 7.31));
    col = mix(col * (1.0 + 0.5 * open), G.col_read.rgb * idle * 1.15, scanned);
    if ((s.flags & SEG_HIDDEN) != 0u) {
        col *= 0.65;
    }

    // Creation: grow along the arc with a bright leading tip.
    let tc = t - s.t_create;
    if (tc < 0.0 && s.t_create > -1.0e8) {
        // Growth scheduled for later (restored at loop end, staggered): not there yet.
        vis = 0.0;
    }
    if (tc >= 0.0 && tc < G.times.z) {
        let g = tc / G.times.z;
        let gs = g * g * (3.0 - 2.0 * g);
        if (u > gs) {
            vis = 0.0;
        }
        col += G.col_create.rgb * G.col_create.w * exp(-abs(u - gs) * 30.0) * (1.0 - g) * 1.5 * lf;
    }

    // Write: turns the segment crimson, fading back to its idle colour.
    let kw = env(t - s.t_write, G.times.y) * lf;
    col = mix(col, G.col_write.rgb * G.col_write.w * max(idle, 0.45), clamp(kw, 0.0, 1.0));
    col += G.col_write.rgb * G.col_write.w * env(t - s.t_write, 1.5) * 0.6 * lf;

    // Read: cream flash plus a long, faint afterglow.
    let kr = (env(t - s.t_read, 1.6) * 0.8 + env(t - s.t_read, G.times.x) * 0.25) * lf;
    col += G.col_read.rgb * G.col_read.w * kr;

    // Delete: brief flash, then a faint ghost in the gap.
    if ((s.flags & SEG_DELETED) != 0u) {
        let td = max(t - s.t_delete, 0.0);
        let flash = exp(-td / 0.35) * G.col_delete.w;
        let ghost = G.idle.w * exp(-3.0 * td / max(G.times.w, 1.0));
        col = G.col_delete.rgb * (flash + ghost) * lf;
        vis = 1.0;
    }

    // Removed at the end of a loop: shrink back toward the start of the arc.
    let tr = t - s.t_retract;
    if (s.t_retract > -1.0e8 && tr >= 0.0) {
        let g = clamp(tr / RETRACT, 0.0, 1.0);
        if (u > 1.0 - g * g * (3.0 - 2.0 * g)) {
            vis = 0.0;
        }
    }
    return vec4<f32>(col, vis);
}

