//! CPU-side world: the filesystem model + synthetic session, the tower layout,
//! and the animation state that feeds the GPU buffers.

use crate::fx::Fx;
use crate::gpu_types::{RingGpu, SegmentGpu};
use gits_core::config::Config;
use gits_core::layout::{self, Layout, LayoutParams, NONE, SEG_DELETED, SegSource};
use gits_core::model::{EventKind, NEVER};
use gits_core::rng::hash01;
use gits_core::synth::{self, Session, SynthHome};
use std::collections::HashMap;

pub const ATLAS_W: u32 = 1024;
pub const LUT_BINS: usize = 16384;
pub const BOOST_TAU: f32 = 6.0;
/// Max tilt of an unwound ring, in ring pitches (mirrors TILT in tower.wgsl).
pub const TILT: f32 = 0.3;
const RELAYOUT_MIN_INTERVAL: f64 = 0.5;
const OPEN_TAU: f32 = 2.2;
const PRESENCE_TAU: f32 = 1.2;

#[derive(Clone, Copy, Debug)]
struct RingPersist {
    gpu: RingGpu,
    open: f32,
    presence: f32,
}

pub struct World {
    pub cfg: Config,
    pub home: SynthHome,
    pub session: Session,
    pub t: f64,
    pub layout: Layout,
    pub layout_version: u64,
    layout_t: f64,
    needs_relayout: bool,
    last_purge_t: f64,
    max_rings: usize,

    pub segs: Vec<SegmentGpu>,
    pub rings: Vec<RingGpu>,
    pub ring_open: Vec<f32>,
    ring_presence: Vec<f32>,
    pub atlas: Vec<u32>,
    pub lut: Vec<f32>,
    /// Centre height (distance from the tower top) and pitch of each ring.
    pub ring_s: Vec<f32>,
    pub ring_pitch: Vec<f32>,

    pub dir_act: Vec<f32>,
    dir_open_target: Vec<f32>,
    /// Number of rings in each directory's band (caps how tall it unwinds).
    dir_rings: Vec<u16>,

    pub dirty_segs: Vec<u32>,
    pub dirty_rings: Vec<u32>,
    pub lut_dirty: bool,
    pub open_dirty: bool,
    /// True while rings are still easing (keeps the frame rate up).
    pub animating: bool,
    /// Smoothed events per second.
    pub activity: f32,

    pub fx: Fx,
}

impl World {
    pub fn new(cfg: Config, max_rings_limit: usize) -> Self {
        let home = synth::generate_home(cfg.synthetic.seed, cfg.synthetic.scale);
        let session = Session::new(cfg.synthetic.seed);
        let max_rings = cfg.collector.max_rings.min(max_rings_limit).max(64);
        log::info!("synthetic home: {} directories, {} files", home.model.dirs.len(), home.model.live_file_count());
        let mut w = Self {
            cfg,
            home,
            session,
            t: 0.0,
            layout: Layout::default(),
            layout_version: 0,
            layout_t: -1.0,
            needs_relayout: true,
            last_purge_t: 0.0,
            max_rings,
            segs: vec![],
            rings: vec![],
            ring_open: vec![],
            ring_presence: vec![],
            atlas: vec![],
            lut: vec![0.0; LUT_BINS + 1],
            ring_s: vec![],
            ring_pitch: vec![],
            dir_act: vec![],
            dir_open_target: vec![],
            dir_rings: vec![],
            dirty_segs: vec![],
            dirty_rings: vec![],
            lut_dirty: true,
            open_dirty: true,
            animating: false,
            activity: 0.0,
            fx: Fx::default(),
        };
        w.relayout();
        w
    }

    fn layout_params(&self) -> LayoutParams {
        let t = &self.cfg.tower;
        LayoutParams {
            min_arc: t.min_arc,
            max_arc: t.max_arc,
            gap: t.gap.max(2.5 / ATLAS_W as f32),
            dir_marks: t.dir_marks,
            dir_mark_arc: 0.03,
            max_rings: self.max_rings,
        }
    }

    /// Applies a new configuration. Returns true if the layout was rebuilt.
    pub fn set_config(&mut self, cfg: Config) -> bool {
        let relayout = cfg.tower != self.cfg.tower || cfg.collector.max_rings != self.cfg.collector.max_rings;
        if cfg.synthetic != self.cfg.synthetic {
            log::warn!("[synthetic] changes take effect after a restart");
        }
        self.cfg = cfg;
        if relayout {
            for f in &mut self.home.model.files {
                f.arc = 0.0;
            }
            self.relayout();
        }
        self.lut_dirty = true;
        relayout
    }

    fn tf(&self, t: f64) -> f32 {
        if t <= NEVER * 0.5 { -1.0e9 } else { t as f32 }
    }

    pub fn relayout(&mut self) {
        let first = self.layout_version == 0;
        let mut persist: HashMap<u64, RingPersist> = HashMap::with_capacity(self.layout.rings.len());
        for (i, r) in self.layout.rings.iter().enumerate() {
            if i < self.rings.len() {
                persist.insert(r.key(), RingPersist { gpu: self.rings[i], open: self.ring_open[i], presence: self.ring_presence[i] });
            }
        }

        let params = self.layout_params();
        self.layout = layout::build(&mut self.home.model, &params);
        let m = &self.home.model;
        let n_rings = self.layout.rings.len();

        // Segments.
        let now_unix = m.epoch_unix as f64 + self.t;
        let prev_layout_t = self.layout_t;
        let mut segs = Vec::with_capacity(self.layout.segments.len());
        for (i, s) in self.layout.segments.iter().enumerate() {
            let mut g = SegmentGpu {
                a0: s.a0,
                a1: s.a1,
                ring: s.ring,
                flags: s.flags,
                hash: hash01(i as u64 * 7919 + s.ring as u64),
                ..Default::default()
            };
            g.t_create = -1.0e9;
            g.t_read = -1.0e9;
            g.t_write = -1.0e9;
            g.t_delete = -1.0e9;
            match s.source {
                SegSource::File(f) => {
                    let file = &m.files[f as usize];
                    g.age_days = ((now_unix - file.mtime as f64) / 86400.0).max(0.0) as f32;
                    g.size_f = ((file.size as f64 + 1.0).log2() / 34.0).clamp(0.0, 1.0) as f32;
                    g.t_read = self.tf(file.t_read);
                    g.t_write = self.tf(file.t_write);
                    if file.t_create != NEVER {
                        // Files created since the previous layout grow in now.
                        g.t_create = if file.t_create > prev_layout_t { self.t as f32 } else { file.t_create as f32 };
                    }
                    if file.t_delete != NEVER {
                        g.flags |= SEG_DELETED;
                        g.t_delete = file.t_delete as f32;
                    }
                    g.hash = hash01(f as u64);
                }
                SegSource::Dir(_) => {
                    g.age_days = 1.0;
                    g.size_f = 0.2;
                }
                SegSource::Agg(_) => {
                    g.age_days = 30.0;
                    g.size_f = 0.8;
                }
            }
            segs.push(g);
        }
        self.segs = segs;

        // Rings, carrying animation state over by key.
        let spin = self.cfg.calm.spin_speed / 60.0;
        self.rings = Vec::with_capacity(n_rings);
        self.ring_open = Vec::with_capacity(n_rings);
        self.ring_presence = Vec::with_capacity(n_rings);
        for r in &self.layout.rings {
            let h = r.hash();
            let depth = r.depth as f32;
            if let Some(p) = persist.get(&r.key()) {
                let mut g = p.gpu;
                g.depth = depth;
                self.rings.push(g);
                self.ring_open.push(p.open);
                self.ring_presence.push(p.presence);
            } else {
                let dir_hash = hash01(r.dir as u64 * 31 + 7);
                let sign = if hash01(h ^ 0xAB) < 0.5 { -1.0 } else { 1.0 };
                self.rings.push(RingGpu {
                    depth,
                    speed: spin * sign * (0.35 + 1.3 * hash01(h)),
                    phase0: hash01(h ^ 0x1234) + dir_hash * 0.3,
                    boost: 0.0,
                    boost_t0: -1.0e9,
                    hash: hash01(h ^ 0x77),
                    _pad: [0.0; 2],
                });
                self.ring_open.push(0.0);
                self.ring_presence.push(if first { 1.0 } else { 0.0 });
            }
        }

        // Segment-id atlas: one row per ring, ATLAS_W texels around it.
        let w = ATLAS_W as usize;
        self.atlas = vec![0u32; w * n_rings.max(1)];
        for (i, s) in self.layout.segments.iter().enumerate() {
            let row = s.ring as usize * w;
            let x0 = (s.a0 * w as f32).floor().max(0.0) as usize;
            let x1 = ((s.a1 * w as f32).ceil() as usize).min(w);
            for x in x0..x1 {
                self.atlas[row + x] = i as u32 + 1;
            }
        }

        let n_dirs = m.dirs.len();
        self.dir_act.resize(n_dirs, 0.0);
        self.dir_open_target.resize(n_dirs, 0.0);
        self.dir_rings = vec![0; n_dirs];
        for r in &self.layout.rings {
            let c = &mut self.dir_rings[r.dir as usize];
            *c = c.saturating_add(1);
        }

        self.layout_t = self.t;
        self.layout_version += 1;
        self.needs_relayout = false;
        self.dirty_segs.clear();
        self.dirty_rings.clear();
        self.lut_dirty = true;
        self.open_dirty = true;
        self.update_lut();
        log::debug!("layout v{}: {} rings, {} segments", self.layout_version, n_rings, self.segs.len());
    }

    /// Advances the simulation by `dt` seconds.
    pub fn step(&mut self, dt: f64) {
        let dt = dt.clamp(0.0, 0.5);
        let t = self.t + dt;
        self.session.step(&mut self.home, t, dt);
        self.t = t;
        let tf = t as f32;

        let events = self.home.model.drain_events();
        let n_events = events.len();
        let min_flash = 1.0 / self.cfg.calm.max_flash_hz.max(0.1);
        let mut falls = vec![];
        for ev in events {
            let file = &self.home.model.files[ev.file as usize];
            let dir = file.dir as usize;
            let seg = self.layout.seg_of_file.get(ev.file as usize).copied().unwrap_or(NONE);
            let impulse = match ev.kind {
                EventKind::Read => 0.35,
                EventKind::Write => 1.0,
                EventKind::Create => 0.8,
                EventKind::Delete => 1.2,
            };
            if dir < self.dir_act.len() {
                self.dir_act[dir] += impulse;
            }
            if ev.kind == EventKind::Create {
                self.needs_relayout = true;
                continue;
            }
            if seg == NONE || seg as usize >= self.segs.len() {
                continue;
            }
            let s = &mut self.segs[seg as usize];
            let ring = s.ring;
            match ev.kind {
                EventKind::Read => {
                    if tf - s.t_read >= min_flash {
                        s.t_read = tf;
                    }
                }
                EventKind::Write => {
                    if tf - s.t_write >= min_flash {
                        s.t_write = tf;
                    }
                    s.age_days = 0.0;
                    let spin_up = self.cfg.events.write.spin_up;
                    if spin_up > 0.0 && (ring as usize) < self.rings.len() {
                        let r = &mut self.rings[ring as usize];
                        let elapsed = (tf - r.boost_t0).max(0.0);
                        r.phase0 += r.boost * (1.0 - (-elapsed / BOOST_TAU).exp());
                        r.boost = spin_up * (0.6 + 0.8 * r.hash);
                        r.boost_t0 = tf;
                        self.dirty_rings.push(ring);
                    }
                }
                EventKind::Delete => {
                    s.flags |= SEG_DELETED;
                    s.t_delete = tf;
                }
                EventKind::Create => unreachable!(),
            }
            self.dirty_segs.push(seg);
            // Directory tick flashes with any activity inside it.
            if let Some(&first) = self.layout.dir_first_ring.get(dir)
                && first != NONE
                && self.cfg.tower.dir_marks
                && let Some(mark) = self.dir_mark_segment(first)
                && tf - self.segs[mark as usize].t_read >= min_flash
            {
                self.segs[mark as usize].t_read = tf;
                self.dirty_segs.push(mark);
            }
            self.fx.on_event(ev.kind, ev.file, seg, t);
            if ev.kind == EventKind::Delete && self.cfg.events.delete.fall {
                falls.push(seg);
            }
        }
        if !falls.is_empty() {
            let mut fx = std::mem::take(&mut self.fx);
            for seg in falls {
                fx.spawn_fall(self, seg);
            }
            self.fx = fx;
        }

        let a = 1.0 - (-dt as f32 / 5.0).exp();
        self.activity += (n_events as f32 / dt.max(1e-3) as f32 - self.activity) * a;

        // Expire deletion ghosts occasionally.
        if t - self.last_purge_t > 30.0 {
            self.last_purge_t = t;
            let before = t - self.cfg.events.delete.ghost_hours as f64 * 3600.0;
            if self.home.model.purge_deleted(before) > 0 {
                self.needs_relayout = true;
            }
        }
        if self.needs_relayout && t - self.layout_t >= RELAYOUT_MIN_INTERVAL {
            self.relayout();
        }

        self.update_activity(dt as f32);
        self.update_rings(dt as f32);
        if self.lut_dirty {
            self.update_lut();
        }
        let fx = std::mem::take(&mut self.fx);
        self.fx = fx.step(self, dt as f32);
    }

    fn dir_mark_segment(&self, first_ring: u32) -> Option<u32> {
        // The directory tick is the first segment placed on the band's first ring.
        let row = first_ring as usize * ATLAS_W as usize;
        self.atlas
            .get(row..row + ATLAS_W as usize)?
            .iter()
            .find(|&&id| id != 0)
            .map(|&id| id - 1)
            .filter(|&s| self.segs[s as usize].flags & layout::SEG_DIR != 0)
    }

    fn update_activity(&mut self, dt: f32) {
        let decay = (-dt / self.cfg.calm.activity_decay.0.max(1.0)).exp();
        for a in &mut self.dir_act {
            *a *= decay;
        }
        let max_open = self.cfg.calm.max_open_dirs;
        let mut top: Vec<(f32, usize)> = self.dir_act.iter().enumerate().filter(|(_, a)| **a > 0.9).map(|(i, a)| (*a, i)).collect();
        top.sort_by(|a, b| b.0.total_cmp(&a.0));
        top.truncate(max_open);
        self.dir_open_target.iter_mut().for_each(|x| *x = 0.0);
        for (a, d) in top {
            self.dir_open_target[d] = smoothstep(0.9, 3.0, a);
        }
    }

    fn update_rings(&mut self, dt: f32) {
        let ko = 1.0 - (-dt / OPEN_TAU).exp();
        let kp = 1.0 - (-dt / PRESENCE_TAU).exp();
        let mut changed = false;
        for (i, r) in self.layout.rings.iter().enumerate() {
            let target = self.dir_open_target.get(r.dir as usize).copied().unwrap_or(0.0);
            let o = &mut self.ring_open[i];
            let d = target - *o;
            if d.abs() > 1e-4 {
                *o += if d.abs() < 2e-3 { d } else { d * ko };
                changed = true;
            }
            let p = &mut self.ring_presence[i];
            if *p < 1.0 {
                *p = if 1.0 - *p < 2e-3 { 1.0 } else { *p + (1.0 - *p) * kp };
                changed = true;
            }
        }
        self.animating = changed;
        if changed {
            self.open_dirty = true;
            self.lut_dirty = true;
        }
    }

    /// Recomputes ring pitches and the height → ring-coordinate lookup table.
    fn update_lut(&mut self) {
        let n = self.layout.rings.len();
        let h = self.cfg.tower.height;
        // A directory unwinds to at most MAX_OPEN_RINGS ring pitches in total, so
        // one huge build directory cannot take over the tower.
        const MAX_OPEN_RINGS: f32 = 5.0;
        let open_share = |dir: u32| -> f32 {
            let c = self.dir_rings.get(dir as usize).copied().unwrap_or(1).max(1) as f32;
            (MAX_OPEN_RINGS / c).min(1.0)
        };
        let mut u_sum = 0.0f32;
        let mut w_sum = 0.0f32;
        for i in 0..n {
            let (e, u) = (self.ring_presence[i], self.ring_open[i]);
            u_sum += e * u * open_share(self.layout.rings[i].dir);
            w_sum += e * (1.0 - u);
        }
        let mut po = self.cfg.tower.open_pitch;
        if po * u_sum > 0.35 * h {
            po = 0.35 * h / u_sum.max(1e-6);
        }
        let pw = ((h - po * u_sum) / w_sum.max(1e-6)).max(0.0);

        self.ring_s.resize(n, 0.0);
        self.ring_pitch.resize(n, 0.0);
        let mut ys = Vec::with_capacity(n + 1);
        let mut y = 0.0f32;
        ys.push(0.0);
        for i in 0..n {
            let (e, u) = (self.ring_presence[i], self.ring_open[i]);
            let p = e * (u * po * open_share(self.layout.rings[i].dir) + (1.0 - u) * pw);
            self.ring_pitch[i] = p;
            self.ring_s[i] = y + p * 0.5;
            y += p;
            ys.push(y);
        }
        let mut ri = 0usize;
        for b in 0..=LUT_BINS {
            let s = b as f32 / LUT_BINS as f32 * h;
            while ri < n && ys[ri + 1] <= s {
                ri += 1;
            }
            self.lut[b] = if ri >= n {
                n as f32
            } else {
                let p = self.ring_pitch[ri];
                if p <= 1e-9 { ri as f32 } else { ri as f32 + ((s - ys[ri]) / p).clamp(0.0, 0.9999) }
            };
        }
        self.lut_dirty = true;
    }

    /// Ring rotation (turns) at time `t`; mirrors `ring_phase` in tower.wgsl.
    pub fn ring_phase(&self, ring: usize, t: f32) -> f32 {
        let r = &self.rings[ring];
        r.phase0 + r.speed * t + r.boost * (1.0 - (-(t - r.boost_t0).max(0.0) / BOOST_TAU).exp())
    }

    pub fn ring_radius(&self, ring: usize) -> f32 {
        let tw = &self.cfg.tower;
        let layer = (self.rings[ring].depth as u32).min(tw.depth_layers.max(1) - 1);
        tw.radius * (1.0 - tw.depth_step * layer as f32)
    }

    pub fn y_top(&self) -> f32 {
        self.cfg.tower.height - 3.2
    }

    /// World position of a point `u` (0..1) along a segment, and the ring radius.
    pub fn segment_point(&self, seg: u32, u: f32) -> Option<(glam::Vec3, f32)> {
        let s = self.segs.get(seg as usize)?;
        let ring = s.ring as usize;
        if ring >= self.rings.len() {
            return None;
        }
        let r = self.ring_radius(ring);
        let turn = s.a0 + (s.a1 - s.a0) * u;
        let theta = std::f32::consts::TAU * (turn + self.ring_phase(ring, self.t as f32) - 0.5);
        let turn0 = theta / std::f32::consts::TAU + 0.5;
        let tilt = TILT * self.ring_open[ring] * (turn0 * std::f32::consts::TAU - self.rings[ring].hash * std::f32::consts::TAU).sin();
        let y = self.y_top() - self.ring_s[ring] - tilt * self.ring_pitch[ring];
        Some((glam::Vec3::new(r * theta.cos(), y, r * theta.sin()), r))
    }

    /// Short label for a file according to the privacy setting.
    pub fn label_for(&self, file: u32) -> Option<String> {
        let m = &self.home.model;
        let f = m.files.get(file as usize)?;
        match self.cfg.privacy.text.as_str() {
            "none" => None,
            "dirs" => Some(m.dirs[f.dir as usize].name.clone()),
            "glyphs" => Some(format!("{:08X}", gits_core::rng::mix64(file as u64) as u32)),
            _ => Some(f.name.clone()),
        }
    }
}

pub fn smoothstep(e0: f32, e1: f32, x: f32) -> f32 {
    let t = ((x - e0) / (e1 - e0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}
