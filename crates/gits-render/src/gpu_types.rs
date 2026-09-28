//! Plain-old-data structs shared with the WGSL shaders. Layouts must match
//! `shaders/common.wgsl` and `shaders/tower.wgsl` exactly.

use bytemuck::{Pod, Zeroable};

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, Pod, Zeroable)]
pub struct SegmentGpu {
    pub a0: f32,
    pub a1: f32,
    pub ring: u32,
    pub flags: u32,
    pub age_days: f32,
    pub size_f: f32,
    pub hash: f32,
    pub t_create: f32,
    pub t_read: f32,
    pub t_write: f32,
    pub t_delete: f32,
    /// Start of the shrink-back animation of a file removed at the end of a loop.
    pub t_retract: f32,
    /// Previous age, blended toward `age_days` from `t_age` (smooth colour changes).
    pub age_from: f32,
    pub t_age: f32,
    pub _pad: [f32; 2],
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, Pod, Zeroable)]
pub struct RingGpu {
    pub depth: f32,
    pub speed: f32,
    pub phase0: f32,
    pub boost: f32,
    pub boost_t0: f32,
    pub hash: f32,
    /// Index of this ring's first segment (segments are stored in ring order).
    pub first_seg: u32,
    /// The ring's row in the segment atlas and ring colour texture ("slot").
    pub row: u32,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, Pod, Zeroable)]
pub struct Globals {
    pub view_proj: [[f32; 4]; 4],
    /// xyz = camera position, w = time (s).
    pub cam_pos: [f32; 4],
    /// height, radius, depth_step, y_top.
    pub tower: [f32; 4],
    /// depth layers, ribbon, fill, back_face.
    pub tower2: [f32; 4],
    /// lut bins, ring count, atlas width, spin-boost time constant.
    pub lut: [f32; 4],
    /// idle_wound, idle_open, dir-mark intensity, ghost intensity.
    pub idle: [f32; 4],
    pub col_fresh: [f32; 4],
    pub col_old: [f32; 4],
    pub col_stale: [f32; 4],
    /// rgb + intensity in w for the event colours.
    pub col_read: [f32; 4],
    pub col_write: [f32; 4],
    pub col_create: [f32; 4],
    pub col_delete: [f32; 4],
    pub col_teal: [f32; 4],
    pub col_bg: [f32; 4],
    /// read fade, write fade, grow, ghost seconds.
    pub times: [f32; 4],
    /// attack, unwound ribbon fill, loop fade (0..1 multiplier on event highlights), dense taps.
    pub times2: [f32; 4],
    /// log10 fresh/old/stale days.
    pub age: [f32; 4],
    /// internal width, height, 1/width, 1/height.
    pub screen: [f32; 4],
    /// exposure, bloom, grain, vignette.
    pub post: [f32; 4],
    /// smear decay per frame, frame seed, needs manual gamma, 1/bloom levels.
    pub post2: [f32; 4],
    /// Ring colour texture rows, bins, atlas texels per bin, _.
    pub rct: [f32; 4],
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, Pod, Zeroable)]
pub struct SpriteVertex {
    pub pos: [f32; 3],
    /// 0 = world space, 1 = normalised device coordinates.
    pub space: f32,
    pub uv: [f32; 2],
    pub kind: f32,
    pub lod: f32,
    pub color: [f32; 4],
}

pub mod sprite_kind {
    pub const LINE: f32 = 0.0;
    pub const GLYPH: f32 = 1.0;
    pub const DOT: f32 = 2.0;
    pub const FLAKE: f32 = 3.0;
    pub const CARD: f32 = 4.0;
}
