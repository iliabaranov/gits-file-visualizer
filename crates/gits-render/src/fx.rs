//! Overlay effects drawn as sprites: callouts on touched files (callouts.rs),
//! the wireframe probe, falling "cut string" segments, gold flakes, and the
//! static chamber (teal lattice, floor lights, calligraphy wall).

use crate::callouts::{Callouts, Slots};
use crate::camera::Camera;
use crate::gpu_types::{SpriteVertex, sprite_kind as K};
use crate::text::{Face, GlyphAtlas, ZEAMI_ENGLISH, ZEAMI_VERSE};
use crate::world::{World, smoothstep};
use gits_core::config::Config;
use gits_core::layout::NONE;
use gits_core::model::{EventKind, FileId};
use gits_core::rng::{Rng, hash01};
use glam::{Mat3, Vec3};
use std::f32::consts::{PI, TAU};

const PROBE_HOLD: f64 = 25.0;
const MAX_FLAKES: usize = 1500;
const FALL_LIFE: f32 = 2.4;

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
    pub cards: Callouts,
    pub probe: Probe,
    falls: Vec<Fall>,
    flakes: Vec<Flake>,
    rng: Rng,
}

impl Default for Fx {
    fn default() -> Self {
        Self { cards: Callouts::default(), probe: Probe::default(), falls: vec![], flakes: vec![], rng: Rng::new(2501) }
    }
}

impl Fx {
    pub fn on_event(&mut self, cfg: &Config, kind: EventKind, file: FileId, t: f64) {
        self.cards.on_event(&cfg.labels, kind, file, t);
        match kind {
            EventKind::Read | EventKind::Write => {
                self.probe.target = Some(file);
                self.probe.t_last = t;
            }
            EventKind::Delete if self.probe.target == Some(file) => self.probe.target = None,
            _ => {}
        }
    }

    pub fn spawn_fall(&mut self, w: &World, seg: u32) {
        let Some(s) = w.segs.get(seg as usize) else { return };
        let ring = s.ring as usize;
        if ring >= w.rings.len() {
            return;
        }
        let phase = w.ring_phase(ring, w.gnow());
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
        self.cards.step(&w.cfg.labels, &w.model, t);

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

    /// Per-frame sprites for one monitor: the scene's (`out`, drawn into the HDR
    /// scene) and the overlay's (`hud`: text and leader lines, drawn at the
    /// output resolution `size`). `reserve` is screen space kept free of labels.
    #[allow(clippy::too_many_arguments)]
    pub fn build(
        &self,
        w: &World,
        cam: &Camera,
        atlas: &GlyphAtlas,
        slots: &mut Slots,
        reserve: Option<[f32; 4]>,
        size: (f32, f32),
        dt: f32,
        out: &mut Vec<SpriteVertex>,
        hud: &mut Vec<SpriteVertex>,
    ) {
        let cfg = &w.cfg;
        let t = w.t;
        let cream = cfg.color(&cfg.events.read.color);
        let gold = cfg.color(&cfg.age.fresh);
        let crimson = cfg.color(&cfg.events.delete.color);
        let (width, height) = size;
        let ui = height / 1080.0;
        self.cards.build(w, cam, atlas, slots, reserve, size, dt, out, hud);

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

        if let Some(n) = w.notice.as_ref().filter(|n| t < n.until && !w.locked) {
            let col = if n.error { crimson } else { cream };
            let size = 14.0 * ui;
            let mut text: String = n.text.chars().take(170).collect();
            while text.len() > 8 && atlas.mono.measure(&text, size) > width - 48.0 * ui {
                text.pop();
            }
            let fade = if n.until.is_finite() { (((n.until - t) / 0.8) as f32).clamp(0.0, 1.0) } else { 1.0 };
            text_px(hud, &atlas.mono, &text, 24.0 * ui, height - 24.0 * ui, size, [col[0], col[1], col[2], 1.6 * fade], width, height);
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
    // The room keeps its proportions around wider towers.
    let r = cfg.tower.radius;
    let lattice_r = (r + 1.4).max(4.5);
    let outer_r = (r + 2.4).max(6.0);
    let lights_r = (r + 0.6).max(2.3);
    let floor_ring_r = (r + 1.3).max(3.1);
    let teal = cfg.color("teal");
    let gold = cfg.color(&cfg.age.fresh);
    let bg = &cfg.background;

    if bg.calligraphy && bg.calligraphy_brightness > 0.0 {
        let wall_r = 13.0;
        let size = 0.95;
        let step = size * 1.3;
        let b = bg.calligraphy_brightness * 0.3;
        // (angle, top, brightness): in the wide view the Japanese right of the
        // tower, the English translation left of it, fainter copies further out.
        for (group, (tc, top, fade)) in
            [(-PI * 0.5 + 0.95, 4.8f32, 1.0f32), (-PI * 0.5 - 0.42, 5.6, 1.0), (-PI * 0.5 + 1.38, 7.2, 0.6), (-PI * 0.5 - 1.38, 7.2, 0.6)]
                .iter()
                .enumerate()
        {
            if group == 1 {
                // The English translation, in horizontal lines.
                let size = 0.42;
                for (k, line) in ZEAMI_ENGLISH.iter().enumerate() {
                    let baseline = top - 2.2 - k as f32 * size * 1.5;
                    for (lod, gain) in [(1.2, 1.0), (3.2, 0.7)] {
                        text_on_wall(
                            out,
                            &atlas.cjk,
                            line,
                            *tc - 0.33,
                            wall_r,
                            baseline,
                            size,
                            lod,
                            [gold[0], gold[1], gold[2], b * gain * 1.2],
                        );
                    }
                }
                continue;
            }
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
        let ring_r = lattice_r;
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
            let p = tilt * Vec3::new(outer_r * phi.cos(), 0.0, outer_r * phi.sin()) + Vec3::Y * (floor + 0.7);
            billboard(out, cam, p, 0.025, K::DOT, [teal[0], teal[1], teal[2], lb * 0.8 * hash01(i as u64 + 99)], 0.0);
        }
    }

    if bg.floor_lights {
        let white_teal = [teal[0] * 0.5 + 0.5, teal[1] * 0.5 + 0.5, teal[2] * 0.5 + 0.5];
        for i in 0..7 {
            let phi = TAU * i as f32 / 7.0 + 0.3;
            let p = Vec3::new(lights_r * phi.cos(), floor, lights_r * phi.sin());
            billboard(out, cam, p, 0.24, K::DOT, [teal[0], teal[1], teal[2], 1.6], 0.0);
            billboard(out, cam, p, 0.07, K::DOT, [white_teal[0], white_teal[1], white_teal[2], 5.0], 0.0);
        }
        billboard(out, cam, Vec3::new(0.0, floor, 0.0), 0.5, K::DOT, [teal[0], teal[1], teal[2], 0.35], 0.0);
        let n = 120;
        for k in 0..n {
            let (a0, a1) = (TAU * k as f32 / n as f32, TAU * (k + 1) as f32 / n as f32);
            line_world(
                out,
                Vec3::new(floor_ring_r * a0.cos(), floor, floor_ring_r * a0.sin()),
                Vec3::new(floor_ring_r * a1.cos(), floor, floor_ring_r * a1.sin()),
                0.01,
                [teal[0], teal[1], teal[2], 0.35],
                cam.eye,
            );
        }
    }
}

/// Horizontal text centred at angle `theta` on the inside of the wall, with its
/// baseline at height `baseline` and cap height about `size`.
#[allow(clippy::too_many_arguments)]
fn text_on_wall(
    out: &mut Vec<SpriteVertex>,
    face: &Face,
    text: &str,
    theta: f32,
    wall_r: f32,
    baseline: f32,
    size: f32,
    lod: f32,
    color: [f32; 4],
) {
    let k = size / face.raster_px;
    let width = face.measure(text, size);
    let mut pen = -width * 0.5;
    for ch in text.chars() {
        let Some(g) = face.glyphs.get(&ch) else { continue };
        if g.w > 0.0 {
            let (x0, w, h) = (pen + g.xmin * k, g.w * k, g.h * k);
            let y0 = baseline + g.ymin * k;
            // Flat quad tangent to the wall at the glyph's centre.
            let th = theta + (x0 + w * 0.5) / wall_r;
            let normal = Vec3::new(th.cos(), 0.0, th.sin());
            let tangent = Vec3::new(-th.sin(), 0.0, th.cos());
            let c = normal * wall_r;
            let quad = [
                c - tangent * (w * 0.5) + Vec3::Y * y0,
                c + tangent * (w * 0.5) + Vec3::Y * y0,
                c + tangent * (w * 0.5) + Vec3::Y * (y0 + h),
                c - tangent * (w * 0.5) + Vec3::Y * (y0 + h),
            ];
            let uv = [[g.uv0[0], g.uv1[1]], [g.uv1[0], g.uv1[1]], [g.uv1[0], g.uv0[1]], [g.uv0[0], g.uv0[1]]];
            push_quad(out, quad.map(|p| p.to_array()), uv, 0.0, K::GLYPH, lod, color);
        }
        pen += g.advance * k;
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

pub(crate) fn human_size(b: u64) -> String {
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

pub(crate) fn quad_world(out: &mut Vec<SpriteVertex>, p: [Vec3; 4], kind: f32, color: [f32; 4]) {
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

/// A flat quad with corners `p` in pixels.
pub(crate) fn quad_px(out: &mut Vec<SpriteVertex>, p: [[f32; 2]; 4], color: [f32; 4], w: f32, h: f32) {
    push_quad(out, p.map(|q| px_to_ndc(q[0], q[1], w, h)), [[0.0; 2]; 4], 1.0, K::CARD, 0.0, color);
}

pub(crate) fn line_px(out: &mut Vec<SpriteVertex>, a: [f32; 2], b: [f32; 2], width: f32, color: [f32; 4], w: f32, h: f32) {
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
