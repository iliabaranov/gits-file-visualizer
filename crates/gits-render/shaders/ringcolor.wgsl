// Ring colour pass: renders the ring colour texture used by the tower's dense
// path. Texel (x, y) = average colour of the ring in slot y over angle bin x (in
// the ring's own rotating frame), from the atlas texels inside the bin. Each instance
// redraws a range of rows; only rows that changed (or are due for a periodic
// refresh) are drawn each frame.

@group(2) @binding(0) var<storage, read> ranges: array<vec2<u32>>;

struct RowOut {
    @builtin(position) pos: vec4<f32>,
};

@vertex
fn vs_rows(@builtin(vertex_index) vi: u32, @builtin(instance_index) ii: u32) -> RowOut {
    let rg = ranges[ii];
    let rows = G.rct.x;
    let x = select(-1.0, 1.0, (vi & 1u) == 1u);
    let row = select(f32(rg.x), f32(rg.y), vi >= 2u);
    var o: RowOut;
    o.pos = vec4<f32>(x, 1.0 - 2.0 * row / rows, 0.0, 1.0);
    return o;
}

@fragment
fn fs_rows(in: RowOut) -> @location(0) vec4<f32> {
    let t = G.cam_pos.w;
    let slot = u32(in.pos.y);
    let bin = u32(in.pos.x);
    let per_bin = u32(G.rct.z);
    let atlas_w = G.lut.z;
    let ri = slot_ring[slot];
    if (ri >= u32(G.lut.y)) {
        return vec4<f32>(0.0, 0.0, 0.0, 1.0);
    }
    let ring = rings[ri];
    let open = ring_open[ri];
    var acc = vec3<f32>(0.0);
    for (var j = 0u; j < per_bin; j++) {
        let x = bin * per_bin + j;
        let local = textureLoad(atlas, vec2<u32>(x, slot), 0).r;
        if (local == 0u) {
            continue;
        }
        let sg = segs[ring.first_seg + local - 1u];
        let turn = (f32(x) + 0.5) / atlas_w;
        let shrink = 0.5 * (sg.a1 - sg.a0) * 0.12 * open;
        if (min(turn - sg.a0, sg.a1 - turn) - shrink <= 0.0) {
            continue;
        }
        let u = clamp((turn - sg.a0) / max(sg.a1 - sg.a0, 1e-5), 0.0, 1.0);
        var c = seg_color(sg, u, open, t);
        c.a *= dash(sg, u);
        acc += c.rgb * c.a;
    }
    return vec4<f32>(acc / f32(per_bin), 1.0);
}
