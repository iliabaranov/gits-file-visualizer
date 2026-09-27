//! Overlay effects drawn as sprites: cards + labels on touched files, the
//! wireframe probe, falling "cut string" segments, gold flakes, and the static
//! chamber (teal lattice, floor lights, calligraphy wall).

use crate::camera::Camera;
use crate::gpu_types::{SpriteVertex, sprite_kind as K};
use crate::text::{Face, GlyphAtlas, ZEAMI_VERSE};
use crate::world::{World, smoothstep};
use gits_core::config::Config;
use gits_core::layout::NONE;
use gits_core::model::{EventKind, FileId};
use gits_core::rng::{Rng, hash01};
use glam::{Mat3, Vec3};
use std::f32::consts::{PI, TAU};

const MAX_CARDS: usize = 6;
const CARD_HOLD: f64 = 7.0;
const CARD_FADE: f64 = 1.6;
const CARD_MIN_INTERVAL: f64 = 0.8;
const PROBE_HOLD: f64 = 25.0;
const MAX_FLAKES: usize = 1500;
const FALL_LIFE: f32 = 2.4;

#[derive(Clone, Debug)]
pub struct Card {
    pub file: FileId,
    pub kind: EventKind,
    pub t0: f64,
    pub t_last: f64,
}

#[derive(Clone, Debug, Default)]
pub struct Probe {
    pub pos: Vec3,
    pub target: Option<FileId>,
    pub t_last: f64,
    pub alpha: f32,
    pub placed: bool,
}

#[derive(Clone, Debug)]
struct Fall {
    y: f32,
    radius: f32,
    theta0: f32,
    theta1: f32,
    t0: f64,
    flaked: bool,
}

#[derive(Clone, Debug)]
struct Flake {
    pos: Vec3,
    vel: Vec3,
    spin: f32,
    spin_rate: f32,
    t0: f64,
    life: f32,
    size: f32,
    phase: f32,
}

pub struct Fx {
    pub cards: Vec<Card>,
    pub probe: Probe,
    falls: Vec<Fall>,
    flakes: Vec<Flake>,
    last_card_t: f64,
    rng: Rng,
}

impl Default for Fx {
    fn default() -> Self {
        Self { cards: vec![], probe: Probe::default(), falls: vec![], flakes: vec![], last_card_t: -1e9, rng: Rng::new(2501) }
    }
}

impl Fx {
    pub fn on_event(&mut self, kind: EventKind, file: FileId, _seg: u32, t: f64) {
        match kind {
            EventKind::Read | EventKind::Write => {
                self.probe.target = Some(file);
                self.probe.t_last = t;
                if let Some(c) = self.cards.iter_mut().find(|c| c.file == file) {
                    c.t_last = t;
                    c.kind = kind;
                } else if t - self.last_card_t >= CARD_MIN_INTERVAL {
                    self.last_card_t = t;
                    self.cards.push(Card { file, kind, t0: t, t_last: t });
                    if self.cards.len() > MAX_CARDS {
                        let oldest = (0..self.cards.len()).min_by(|&a, &b| self.cards[a].t_last.total_cmp(&self.cards[b].t_last)).unwrap();
                        self.cards.remove(oldest);
                    }
                }
            }
            EventKind::Delete => {
                self.cards.retain(|c| c.file != file);
                if self.probe.target == Some(file) {
                    self.probe.target = None;
                }
            }
            EventKind::Create => {}
        }
    }

    pub fn spawn_fall(&mut self, w: &World, seg: u32) {
        let Some(s) = w.segs.get(seg as usize) else { return };
        let ring = s.ring as usize;
        if ring >= w.rings.len() {
            return;
        }
        let phase = w.ring_phase(ring, w.t as f32);
        self.falls.push(Fall {
            y: w.y_top() - w.ring_s[ring],
            radius: w.ring_radius(ring),
            theta0: TAU * (s.a0 + phase - 0.5),
            theta1: TAU * (s.a1 + phase - 0.5),
            t0: w.t,
            flaked: false,
        });
    }

    fn seg_of(w: &World, file: FileId) -> Option<u32> {
        w.layout.seg_of_file.get(file as usize).copied().filter(|&s| s != NONE)
    }

    pub fn step(mut self, w: &World, dt: f32) -> Self {
        let t = w.t;
        self.cards.retain(|c| t - c.t_last < CARD_HOLD + CARD_FADE && w.home.model.is_live(c.file));

        // Probe glides toward the most recently touched file.
        let target = self.probe.target.and_then(|f| Self::seg_of(w, f)).and_then(|s| w.segment_point(s, 0.5));
        let want_alpha = if target.is_some() && t - self.probe.t_last < PROBE_HOLD && w.cfg.events.read.probe { 1.0 } else { 0.0 };
        if let Some((p, r)) = target {
            let out = Vec3::new(p.x, 0.0, p.z) / r.max(1e-3);
            let goal = p + out * 0.16;
            if !self.probe.placed {
                self.probe.pos = goal + Vec3::new(0.0, 0.6, 0.0);
                self.probe.placed = true;
            }
            let k = 1.0 - (-dt / 2.0).exp();
            self.probe.pos += (goal - self.probe.pos) * k;
        }
        self.probe.alpha += (want_alpha - self.probe.alpha) * (1.0 - (-dt / 1.2).exp());

        // Falling segments break into flakes.
        let mut new_flakes = vec![];
        for f in &mut self.falls {
            let age = (t - f.t0) as f32;
            if age > 0.9 && !f.flaked {
                f.flaked = true;
                let arc_len = (f.theta1 - f.theta0).abs() * f.radius;
                let n = ((arc_len / 0.05) as usize).clamp(4, 24);
                for i in 0..n {
                    let u = (i as f32 + self.rng.f32()) / n as f32;
                    let th = f.theta0 + (f.theta1 - f.theta0) * u;
                    let r = f.radius + 0.3 * age;
                    let y = f.y - 0.45 * age * age;
                    let pos = Vec3::new(r * th.cos(), y, r * th.sin());
                    let out = Vec3::new(th.cos(), 0.0, th.sin());
                    new_flakes.push(Flake {
                        pos,
                        vel: out * (0.05 + 0.1 * self.rng.f32()) + Vec3::new(0.0, -0.12 - 0.1 * self.rng.f32(), 0.0),
                        spin: self.rng.f32() * TAU,
                        spin_rate: 1.5 + 3.0 * self.rng.f32(),
                        t0: t,
                        life: 5.0 + 4.0 * self.rng.f32(),
                        size: 0.022 + 0.02 * self.rng.f32(),
                        phase: self.rng.f32() * TAU,
                    });
                }
            }
        }
        self.falls.retain(|f| ((t - f.t0) as f32) < FALL_LIFE);
        self.flakes.extend(new_flakes);
        if self.flakes.len() > MAX_FLAKES {
            let excess = self.flakes.len() - MAX_FLAKES;
            self.flakes.drain(..excess);
        }
        let drag = (-dt * 0.4).exp();
        for fl in &mut self.flakes {
            let age = (t - fl.t0) as f32;
            fl.vel *= drag;
            fl.vel.y -= 0.015 * dt;
            let flutter = Vec3::new((age * 1.7 + fl.phase).sin(), 0.0, (age * 1.3 + fl.phase * 1.7).cos()) * 0.05;
            fl.pos += (fl.vel + flutter) * dt;
            fl.spin += fl.spin_rate * dt;
        }
        self.flakes.retain(|fl| ((t - fl.t0) as f32) < fl.life);
        self
    }

    pub fn alive(&self) -> bool {
        !self.falls.is_empty() || !self.flakes.is_empty() || !self.cards.is_empty() || self.probe.alpha > 0.02
    }

    /// Per-frame sprites (world + screen space).
    pub fn build(&self, w: &World, cam: &Camera, atlas: &GlyphAtlas, width: f32, height: f32, out: &mut Vec<SpriteVertex>) {
        let cfg = &w.cfg;
        let t = w.t;
        let cream = cfg.color(&cfg.events.read.color);
        let gold = cfg.color(&cfg.age.fresh);
        let crimson = cfg.color(&cfg.events.delete.color);
        let write_col = cfg.color(&cfg.events.write.color);
        let ui = height / 1080.0;

        // Cards and labels.
        for c in &self.cards {
            let Some(seg) = Self::seg_of(w, c.file) else { continue };
            let Some((p, r)) = w.segment_point(seg, 0.5) else { continue };
            let age = t - c.t0;
            let since = t - c.t_last;
            let alpha = (smoothstep(0.0, 0.35, age as f32)
                * (1.0 - smoothstep(CARD_HOLD as f32, (CARD_HOLD + CARD_FADE) as f32, since as f32)))
            .max(0.0);
            if alpha <= 0.0 {
                continue;
            }
            let normal = Vec3::new(p.x, 0.0, p.z) / r.max(1e-3);
            let facing = normal.dot((cam.eye - p).normalize());
            let vis = 0.2 + 0.8 * smoothstep(-0.2, 0.35, facing);
            let tangent = Vec3::new(-normal.z, 0.0, normal.x);
            let center = p + normal * 0.03;
            let (hw, hh) = (0.06, 0.34);
            let col = [cream[0], cream[1], cream[2], 1.5 * alpha * vis];
            quad_world(
                out,
                [
                    center - tangent * hw - Vec3::Y * hh,
                    center + tangent * hw - Vec3::Y * hh,
                    center + tangent * hw + Vec3::Y * hh,
                    center - tangent * hw + Vec3::Y * hh,
                ],
                K::CARD,
                col,
            );

            if facing < 0.05 {
                continue;
            }
            let Some(label) = w.label_for(c.file) else { continue };
            let Some((sx, sy)) = cam.project(center + Vec3::Y * hh * 0.6, width, height) else { continue };
            let size = 15.0 * ui;
            let right = sx >= width * 0.5;
            let text_w = atlas.mono.measure(&label, size);
            let lead = 48.0 * ui;
            let lx = if right { sx + lead } else { sx - lead - text_w };
            let ly = sy - 10.0 * ui;
            let la = alpha * vis;
            line_px(
                out,
                [sx, sy],
                [if right { lx - 6.0 * ui } else { lx + text_w + 6.0 * ui }, ly - size * 0.3],
                1.2 * ui,
                [gold[0], gold[1], gold[2], 0.9 * la],
                width,
                height,
            );
            text_px(out, &atlas.mono, &label, lx, ly, size, [gold[0], gold[1], gold[2], 1.6 * la], width, height);
            let size_bytes = w.home.model.files[c.file as usize].size;
            let (tag, tag_col) = match c.kind {
                EventKind::Write => ("WRITE", write_col),
                _ => ("READ ", cream),
            };
            let sub = format!("{tag} · {}", human_size(size_bytes));
            let sub_size = 11.0 * ui;
            let sub_x = if right { lx } else { lx + text_w - atlas.mono.measure(&sub, sub_size) };
            text_px(out, &atlas.mono, &sub, sub_x, ly + 15.0 * ui, sub_size, [tag_col[0], tag_col[1], tag_col[2], 1.1 * la], width, height);
        }

        // Probe: three nested wire circles and a bright core.
        if self.probe.alpha > 0.01 {
            let a = self.probe.alpha;
            let tf = t as f32;
            let p = self.probe.pos;
            let col = [cream[0], cream[1], cream[2], 1.4 * a];
            for (i, rot) in [
                Mat3::from_rotation_y(tf * 0.7),
                Mat3::from_rotation_x(tf * 0.9) * Mat3::from_rotation_y(0.6),
                Mat3::from_rotation_z(tf * 0.5 + 1.0) * Mat3::from_rotation_x(PI * 0.5),
            ]
            .iter()
            .enumerate()
            {
                let r = 0.1 + 0.015 * i as f32;
                let n = 40;
                for k in 0..n {
                    let (a0, a1) = (TAU * k as f32 / n as f32, TAU * (k + 1) as f32 / n as f32);
                    let q0 = p + *rot * Vec3::new(r * a0.cos(), 0.0, r * a0.sin());
                    let q1 = p + *rot * Vec3::new(r * a1.cos(), 0.0, r * a1.sin());
                    line_world(out, q0, q1, 0.005, col, cam.eye);
                }
            }
            billboard(out, cam, p, 0.05, K::DOT, [cream[0], cream[1], cream[2], 2.5 * a], 0.0);
        }

        // Falling segments.
        let del_int = cfg.events.delete.intensity;
        for f in &self.falls {
            let age = (t - f.t0) as f32;
            let fade = (-age * 1.1).exp() * (1.0 - smoothstep(0.9, FALL_LIFE, age));
            let col = [crimson[0], crimson[1], crimson[2], del_int * 1.5 * fade];
            let r = f.radius + 0.3 * age;
            let y = f.y - 0.45 * age * age;
            let n = 16;
            for k in 0..n {
                let th0 = f.theta0 + (f.theta1 - f.theta0) * k as f32 / n as f32 + 0.1 * age;
                let th1 = f.theta0 + (f.theta1 - f.theta0) * (k + 1) as f32 / n as f32 + 0.1 * age;
                let q0 = Vec3::new(r * th0.cos(), y, r * th0.sin());
                let q1 = Vec3::new(r * th1.cos(), y, r * th1.sin());
                line_world(out, q0, q1, 0.04, col, cam.eye);
            }
        }

        // Gold flakes.
        for fl in &self.flakes {
            let age = (t - fl.t0) as f32;
            let fade = smoothstep(0.0, 0.25, age) * (1.0 - smoothstep(fl.life * 0.65, fl.life, age));
            let glint = 0.25 + 0.75 * (fl.spin * 0.5 + fl.phase).sin().abs().powi(3);
            let col = [gold[0], gold[1], gold[2], 2.2 * glint * fade];
            billboard(out, cam, fl.pos, fl.size, K::FLAKE, col, fl.spin);
        }
    }
}

/// Static chamber sprites, rendered once into the cached background.
pub fn build_background(cfg: &Config, atlas: &GlyphAtlas, y_top: f32, cam: &Camera, out: &mut Vec<SpriteVertex>) {
    let floor = y_top - cfg.tower.height - 0.1;
    let teal = cfg.color("teal");
    let gold = cfg.color(&cfg.age.fresh);
    let bg = &cfg.background;

    if bg.calligraphy && bg.calligraphy_brightness > 0.0 {
        let wall_r = 13.0;
        let size = 0.95;
        let step = size * 1.3;
        let b = bg.calligraphy_brightness * 0.3;
        for (group, (tc, top)) in
            [(-PI * 0.5 + 0.34, 7.4f32), (-PI * 0.5 - 0.42, 5.6), (-PI * 0.5 + 0.95, 4.2), (-PI * 0.5 - 1.05, 6.8)].iter().enumerate()
        {
            let fade = if group < 2 { 1.0 } else { 0.6 };
            for (k, line) in ZEAMI_VERSE.iter().enumerate() {
                let theta = tc + (1.5 - k as f32) * step / wall_r;
                for (j, ch) in line.chars().enumerate() {
                    let y = top - j as f32 * size * 1.08;
                    for (lod, gain) in [(1.8, 1.0), (3.8, 0.7)] {
                        glyph_on_wall(out, &atlas.cjk, ch, theta, wall_r, y, size, lod, [gold[0], gold[1], gold[2], b * gain * fade]);
                    }
                }
            }
        }
    }

    if bg.lattice && bg.lattice_brightness > 0.0 {
        let lb = bg.lattice_brightness;
        let tilt = Mat3::from_rotation_x(0.16);
        let center_y = floor + 1.6;
        let ring_r = 4.5;
        let tube = 0.13;
        let n = 220;
        let point = |i: usize, strand: f32| -> Vec3 {
            let phi = TAU * i as f32 / n as f32;
            let radial = Vec3::new(phi.cos(), 0.0, phi.sin());
            let psi = phi * 22.0 + strand * PI;
            let local = radial * (ring_r + tube * psi.cos()) + Vec3::Y * (tube * psi.sin());
            tilt * local + Vec3::Y * center_y
        };
        for i in 0..n {
            for strand in [0.0, 1.0] {
                let h = hash01(i as u64 * 2 + strand as u64);
                let p0 = point(i, strand);
                let p1 = point(i + 1, strand);
                billboard(out, cam, p0, 0.03 + 0.02 * h, K::DOT, [teal[0], teal[1], teal[2], lb * (1.2 + 1.5 * h)], 0.0);
                line_world(out, p0, p1, 0.012, [teal[0], teal[1], teal[2], lb * 0.45], cam.eye);
            }
            if i % 4 == 0 {
                line_world(out, point(i, 0.0), point(i, 1.0), 0.008, [teal[0], teal[1], teal[2], lb * 0.35], cam.eye);
            }
        }
        // Sparse outer ring of points.
        for i in 0..90 {
            let phi = TAU * i as f32 / 90.0 + 0.02;
            let p = tilt * Vec3::new(6.0 * phi.cos(), 0.0, 6.0 * phi.sin()) + Vec3::Y * (floor + 0.7);
            billboard(out, cam, p, 0.025, K::DOT, [teal[0], teal[1], teal[2], lb * 0.8 * hash01(i as u64 + 99)], 0.0);
        }
    }

    if bg.floor_lights {
        let white_teal = [teal[0] * 0.5 + 0.5, teal[1] * 0.5 + 0.5, teal[2] * 0.5 + 0.5];
        for i in 0..7 {
            let phi = TAU * i as f32 / 7.0 + 0.3;
            let p = Vec3::new(2.3 * phi.cos(), floor, 2.3 * phi.sin());
            billboard(out, cam, p, 0.24, K::DOT, [teal[0], teal[1], teal[2], 1.6], 0.0);
            billboard(out, cam, p, 0.07, K::DOT, [white_teal[0], white_teal[1], white_teal[2], 5.0], 0.0);
        }
        billboard(out, cam, Vec3::new(0.0, floor, 0.0), 0.5, K::DOT, [teal[0], teal[1], teal[2], 0.35], 0.0);
        let n = 120;
        for k in 0..n {
            let (a0, a1) = (TAU * k as f32 / n as f32, TAU * (k + 1) as f32 / n as f32);
            line_world(
                out,
                Vec3::new(3.1 * a0.cos(), floor, 3.1 * a0.sin()),
                Vec3::new(3.1 * a1.cos(), floor, 3.1 * a1.sin()),
                0.01,
                [teal[0], teal[1], teal[2], 0.35],
                cam.eye,
            );
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn glyph_on_wall(
    out: &mut Vec<SpriteVertex>,
    face: &Face,
    ch: char,
    theta: f32,
    wall_r: f32,
    y: f32,
    size: f32,
    lod: f32,
    color: [f32; 4],
) {
    let Some(g) = face.glyphs.get(&ch) else { return };
    let k = size / face.raster_px;
    let (w, h) = (g.w * k, g.h * k);
    // Viewed from inside the wall, +tangent is screen-right.
    let normal = Vec3::new(theta.cos(), 0.0, theta.sin());
    let tangent = Vec3::new(-theta.sin(), 0.0, theta.cos());
    let c = normal * wall_r + Vec3::Y * y;
    let quad = [
        c - tangent * (w * 0.5) - Vec3::Y * (h * 0.5),
        c + tangent * (w * 0.5) - Vec3::Y * (h * 0.5),
        c + tangent * (w * 0.5) + Vec3::Y * (h * 0.5),
        c - tangent * (w * 0.5) + Vec3::Y * (h * 0.5),
    ];
    let uv = [[g.uv0[0], g.uv1[1]], [g.uv1[0], g.uv1[1]], [g.uv1[0], g.uv0[1]], [g.uv0[0], g.uv0[1]]];
    push_quad(out, quad.map(|p| p.to_array()), uv, 0.0, K::GLYPH, lod, color);
}

fn human_size(b: u64) -> String {
    let b = b as f64;
    if b < 1024.0 {
        format!("{b:.0} B")
    } else if b < 1024.0 * 1024.0 {
        format!("{:.1} KB", b / 1024.0)
    } else if b < 1024.0 * 1024.0 * 1024.0 {
        format!("{:.1} MB", b / 1048576.0)
    } else {
        format!("{:.2} GB", b / 1073741824.0)
    }
}

// ---------------------------------------------------------------------------
// Sprite geometry helpers
// ---------------------------------------------------------------------------

fn push_quad(out: &mut Vec<SpriteVertex>, p: [[f32; 3]; 4], uv: [[f32; 2]; 4], space: f32, kind: f32, lod: f32, color: [f32; 4]) {
    for i in [0, 1, 2, 0, 2, 3] {
        out.push(SpriteVertex { pos: p[i], space, uv: uv[i], kind, lod, color });
    }
}

const UV_BOX: [[f32; 2]; 4] = [[-1.0, -1.0], [1.0, -1.0], [1.0, 1.0], [-1.0, 1.0]];

fn quad_world(out: &mut Vec<SpriteVertex>, p: [Vec3; 4], kind: f32, color: [f32; 4]) {
    push_quad(out, p.map(|v| v.to_array()), UV_BOX, 0.0, kind, 0.0, color);
}

fn billboard(out: &mut Vec<SpriteVertex>, cam: &Camera, c: Vec3, size: f32, kind: f32, color: [f32; 4], rot: f32) {
    let (right, up) = cam.right_up();
    let (s, co) = rot.sin_cos();
    let r = (right * co + up * s) * size;
    let u = (up * co - right * s) * size;
    quad_world(out, [c - r - u, c + r - u, c + r + u, c - r + u], kind, color);
}

/// A glowing line between two world points, facing the camera.
fn line_world(out: &mut Vec<SpriteVertex>, a: Vec3, b: Vec3, width: f32, color: [f32; 4], eye: Vec3) {
    let dir = b - a;
    let view = (a + b) * 0.5 - eye;
    let side = dir.cross(view).normalize_or_zero() * (width * 0.5);
    let uv = [[0.0, -1.0], [1.0, -1.0], [1.0, 1.0], [0.0, 1.0]];
    push_quad(
        out,
        [(a - side).to_array(), (b - side).to_array(), (b + side).to_array(), (a + side).to_array()],
        uv,
        0.0,
        K::LINE,
        0.0,
        color,
    );
}

fn px_to_ndc(x: f32, y: f32, w: f32, h: f32) -> [f32; 3] {
    [x / w * 2.0 - 1.0, 1.0 - y / h * 2.0, 0.0]
}

fn line_px(out: &mut Vec<SpriteVertex>, a: [f32; 2], b: [f32; 2], width: f32, color: [f32; 4], w: f32, h: f32) {
    let (dx, dy) = (b[0] - a[0], b[1] - a[1]);
    let len = (dx * dx + dy * dy).sqrt().max(1e-3);
    let (nx, ny) = (-dy / len * width * 0.5, dx / len * width * 0.5);
    let p = [
        px_to_ndc(a[0] - nx, a[1] - ny, w, h),
        px_to_ndc(b[0] - nx, b[1] - ny, w, h),
        px_to_ndc(b[0] + nx, b[1] + ny, w, h),
        px_to_ndc(a[0] + nx, a[1] + ny, w, h),
    ];
    push_quad(out, p, [[0.0, -1.0], [1.0, -1.0], [1.0, 1.0], [0.0, 1.0]], 1.0, K::LINE, 0.0, color);
}

/// Draws text with its baseline at `y` (pixels, origin top-left).
#[allow(clippy::too_many_arguments)]
pub fn text_px(out: &mut Vec<SpriteVertex>, face: &Face, s: &str, x: f32, y: f32, size: f32, color: [f32; 4], w: f32, h: f32) {
    let k = size / face.raster_px;
    let lod = (1.0 / k).log2().max(0.0) * 0.85;
    let mut pen = x;
    for ch in s.chars() {
        let Some(g) = face.glyph(ch) else { continue };
        if g.w > 0.0 {
            let x0 = pen + g.xmin * k;
            let y0 = y - (g.ymin + g.h) * k;
            let (x1, y1) = (x0 + g.w * k, y0 + g.h * k);
            let p = [px_to_ndc(x0, y1, w, h), px_to_ndc(x1, y1, w, h), px_to_ndc(x1, y0, w, h), px_to_ndc(x0, y0, w, h)];
            let uv = [[g.uv0[0], g.uv1[1]], [g.uv1[0], g.uv1[1]], [g.uv1[0], g.uv0[1]], [g.uv0[0], g.uv0[1]]];
            push_quad(out, p, uv, 1.0, K::GLYPH, lod, color);
        }
        pen += g.advance * k;
    }
}
