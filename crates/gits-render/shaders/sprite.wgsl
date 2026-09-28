// Additive sprites: lines, glyphs, dots, flakes and cards, in world space or
// directly in normalised device coordinates. fs_sprite draws into the HDR
// scene; fs_hud draws the full-resolution overlay (labels, system monitor)
// onto the finished frame, tonemapped the same way.

@group(1) @binding(0) var glyphs: texture_2d<f32>;
@group(1) @binding(1) var glyph_sampler: sampler;

struct SpriteIn {
    @location(0) pos: vec3<f32>,
    @location(1) space: f32,
    @location(2) uv: vec2<f32>,
    @location(3) kind: f32,
    @location(4) lod: f32,
    @location(5) color: vec4<f32>,
};

struct SpriteOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) uv: vec2<f32>,
    @location(1) @interpolate(flat) kind_lod: vec2<f32>,
    @location(2) color: vec4<f32>,
};

@vertex
fn vs_sprite(v: SpriteIn) -> SpriteOut {
    var o: SpriteOut;
    if (v.space < 0.5) {
        o.clip = G.view_proj * vec4<f32>(v.pos, 1.0);
    } else {
        o.clip = vec4<f32>(v.pos.xy, 0.0, 1.0);
    }
    o.uv = v.uv;
    o.kind_lod = vec2<f32>(v.kind, v.lod);
    o.color = v.color;
    return o;
}

fn sprite_color(in: SpriteOut) -> vec3<f32> {
    let kind = u32(in.kind_lod.x + 0.5);
    var a = 0.0;
    if (kind == 0u) {
        // line: bright core, soft falloff across its width
        let d = abs(in.uv.y);
        a = pow(max(1.0 - d, 0.0), 1.6);
    } else if (kind == 1u) {
        a = textureSampleLevel(glyphs, glyph_sampler, in.uv, in.kind_lod.y).r;
    } else if (kind == 2u) {
        a = exp(-dot(in.uv, in.uv) * 5.0);
    } else if (kind == 3u) {
        let d = abs(in.uv.x) + abs(in.uv.y) * 1.6;
        a = 1.0 - smoothstep(0.55, 1.0, d);
    } else {
        let m = max(abs(in.uv.x), abs(in.uv.y));
        a = 1.0 - smoothstep(0.75, 1.0, m);
    }
    return in.color.rgb * in.color.a * a;
}

@fragment
fn fs_sprite(in: SpriteOut) -> @location(0) vec4<f32> {
    return vec4<f32>(sprite_color(in), 1.0);
}

@fragment
fn fs_hud(in: SpriteOut) -> @location(0) vec4<f32> {
    return vec4<f32>(encode_out(tonemap(sprite_color(in) * G.post.x)), 1.0);
}
