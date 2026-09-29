// Shell image pyramid (used by the tower's dense path when the whole tower
// turns as one, i.e. with sectors). Texel (x, row) of level L is the average
// colour of 2^L consecutive rings of a shell's ring list over one angle bin,
// in the shell's own turning frame, so a pixel covering many packed rings
// reads a few blocks instead of every ring. Odd levels live in texture A,
// even levels in texture B (a pass writes one while reading the other), each
// stacked vertically (see pyr_y0), with one border column on each side so
// bilinear filtering wraps around the circle.
//
// Level 1 is built from the ring colour texture (each ring shifted by its
// angle relative to the shell, a whole number of bins, so it is an exact
// copy); level L from level L - 1. Only rows whose rings changed are redrawn.

@group(1) @binding(0) var ring_tex: texture_2d<f32>;
@group(1) @binding(1) var ring_samp: sampler;
@group(1) @binding(2) var<storage, read> ring_list: array<vec4<f32>>;
@group(1) @binding(3) var src: texture_2d<f32>;
// (first row, end row, level, _) per instance.
@group(1) @binding(4) var<storage, read> pyr_ranges: array<vec4<u32>>;

struct PyrOut {
    @builtin(position) pos: vec4<f32>,
    @location(0) @interpolate(flat) level: u32,
};

@vertex
fn vs_pyr(@builtin(vertex_index) vi: u32, @builtin(instance_index) ii: u32) -> PyrOut {
    let rg = pyr_ranges[ii];
    let level = rg.z;
    let height = select(G.pyr.w, G.pyr.z, (level & 1u) == 1u);
    let y0 = f32(pyr_y0(level, u32(G.pyr.y)));
    let x = select(-1.0, 1.0, (vi & 1u) == 1u);
    let row = y0 + select(f32(rg.x), f32(rg.y), vi >= 2u);
    var o: PyrOut;
    o.pos = vec4<f32>(x, 1.0 - 2.0 * row / height, 0.0, 1.0);
    o.level = level;
    return o;
}

@fragment
fn fs_pyr(in: PyrOut) -> @location(0) vec4<f32> {
    let x = u32(in.pos.x);
    let level = in.level;
    let r0 = u32(G.pyr.y);
    let row = u32(in.pos.y) - pyr_y0(level, r0);
    if (level == 1u) {
        let bins = G.rct.y;
        // Column 0 and bins + 1 repeat the last and first bins.
        let b = (f32(x) - 1.0 + bins) % bins;
        // Shell angle of this bin, plus the shell's rotation = world angle.
        let turn = (b + 0.5) / bins + G.pyr.x;
        var acc = vec3<f32>(0.0);
        for (var k = 0u; k < 2u; k++) {
            let e = ring_list[2u * row + k];
            if (e.z >= 0.0) {
                acc += textureSampleLevel(ring_tex, ring_samp, vec2<f32>(fract(turn - e.y), e.x), 0.0).rgb;
            }
        }
        return vec4<f32>(acc * 0.5, 1.0);
    }
    let y = pyr_y0(level - 1u, r0) + 2u * row;
    let a = textureLoad(src, vec2<u32>(x, y), 0).rgb;
    let c = textureLoad(src, vec2<u32>(x, y + 1u), 0).rgb;
    return vec4<f32>((a + c) * 0.5, 1.0);
}
