//! CPU-side world: the filesystem model (synthetic or from the collector), the tower layout,
//! and the animation state that feeds the GPU buffers.

use crate::fx::Fx;
use crate::gpu_types::{RingGpu, SegmentGpu};
use crate::source::{CollectorClient, Incoming, Source};
use gits_core::config::Config;
use gits_core::layout::{self, Layout, LayoutParams, NONE, SEG_DELETED, SegSource};
use gits_core::model::{EventKind, Model, NEVER};
use gits_core::rng::{Rng, hash01};
use gits_core::synth::{self, Session};
use std::collections::HashMap;
use std::path::PathBuf;

pub const ATLAS_W: u32 = 1024;
pub const LUT_BINS: usize = 16384;
pub const BOOST_TAU: f32 = 6.0;
/// Max tilt of an unwound ring, in ring pitches (mirrors TILT in tower.wgsl).
pub const TILT: f32 = 0.3;
const RELAYOUT_MIN_INTERVAL: f64 = 0.5;
/// Live trees are large (a rebuild takes tens of ms), so rebuild at most this often.
const RELAYOUT_LIVE_INTERVAL: f64 = 5.0;
/// Directory activity below this does not count as "hot" for layout priority.
const HOT_MIN: f32 = 0.5;
/// Activity at which a collapsed directory is unfolded, and how often that may happen.
const UNFOLD_ACT: f32 = 1.5;
const UNFOLD_INTERVAL: f64 = 5.0;
/// Ceiling for a folder's activity score.
const DIR_ACT_MAX: f32 = 6.0;
const OPEN_TAU: f32 = 2.2;
const PRESENCE_TAU: f32 = 1.2;
/// Rings leaving the tower ease out slowly so the drum barely drifts.
const PRESENCE_OUT_TAU: f32 = 5.0;

// Loop wind-down schedule, in simulated seconds after the wind-down starts.
// Events stop, highlights fade out and busy directories wind back; then files
// created during the loop shrink away and deleted ones grow back; then rings
// that only held created files ease out; finally the model and session reset.
const WIND_FADE: f64 = 60.0;
const RESTORE_AT: f64 = 70.0;
const RESTORE_STAGGER: f64 = 15.0;
const RETIRE_AT: f64 = 90.0;
const RESET_AT: f64 = 118.0;
/// Shortest loop accepted (the wind-down alone takes RESET_AT seconds).
const MIN_LOOP: f64 = 240.0;
/// Directory activity decays this fast while winding down.
const WIND_ACTIVITY_TAU: f32 = 12.0;

#[derive(Clone, Copy, Debug, PartialEq)]
enum LoopStage {
    Running,
    /// Wind-down started at this time.
    WindDown(f64),
}

#[derive(Clone, Copy, Debug)]
struct RingPersist {
    gpu: RingGpu,
    open: f32,
    presence: f32,
}

/// Short on-screen status line (config reloads and errors).
#[derive(Clone, Debug)]
pub struct Notice {
    pub text: String,
    pub error: bool,
    /// Simulation time until which it is shown.
    pub until: f64,
}

pub struct World {
    pub cfg: Config,
    /// Nothing is being drawn: skip work that only matters on screen (layout
    /// rebuilds are deferred until the next visible frame).
    pub hidden: bool,
    /// A screencast/screen share is running (labels are obfuscated if configured).
    pub sharing: bool,
    pub notice: Option<Notice>,
    pub model: Model,
    pub source: Source,
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
    /// Per ring row: 1-based index of the segment within its ring (0 = gap).
    pub atlas: Vec<u8>,
    pub lut: Vec<f32>,
    /// Centre height (distance from the tower top) and pitch of each ring.
    pub ring_s: Vec<f32>,
    pub ring_pitch: Vec<f32>,

    pub dir_act: Vec<f32>,
    dir_open_target: Vec<f32>,
    /// Number of rings in each directory's band (caps how tall it unwinds).
    dir_rings: Vec<u16>,

    /// Presence each ring eases toward (0 for rings about to be removed).
    ring_presence_target: Vec<f32>,

    /// Starting state of the synthetic home, restored at the end of every loop.
    initial_model: Model,
    initial_files: usize,
    loop_start: f64,
    loop_stage: LoopStage,
    loop_count: u64,
    restored: bool,
    retired: bool,
    /// 0..1 multiplier on event highlights (fades them out during wind-down).
    pub loop_fade: f32,
    /// Subtracted from the clock when computing file ages, so ages repeat per loop.
    age_offset: f64,

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
    /// `collector`: socket of a running gits-collector, or None for synthetic data.
    pub fn new(cfg: Config, max_rings_limit: usize, collector: Option<PathBuf>) -> Self {
        let max_rings = cfg.collector.max_rings.min(max_rings_limit).max(64);
        let (model, source, notice) = match collector {
            None => {
                let home = synth::generate_home(cfg.synthetic.seed, cfg.synthetic.scale);
                log::info!("synthetic home: {} directories, {} files", home.model.dirs.len(), home.model.live_file_count());
                let session = Session::new(cfg.synthetic.seed);
                (home.model, Source::Synthetic { index: home.index, session }, None)
            }
            Some(socket) => {
                log::info!("waiting for the collector at {}", socket.display());
                let notice = Notice { text: "connecting to gits-collector…".into(), error: false, until: f64::INFINITY };
                (Model::new("~", wall_clock()), Source::Collector(CollectorClient::spawn(socket)), Some(notice))
            }
        };
        let initial_model = model.clone();
        let initial_files = initial_model.files.len();
        let mut w = Self {
            hidden: false,
            sharing: false,
            initial_model,
            initial_files,
            loop_start: 0.0,
            loop_stage: LoopStage::Running,
            loop_count: 0,
            restored: false,
            retired: false,
            loop_fade: 1.0,
            age_offset: 0.0,
            ring_presence_target: vec![],
            cfg,
            notice,
            model,
            source,
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
            // The atlas stores a segment's position within its ring in 8 bits.
            min_arc: t.min_arc.max(1.0 / 250.0),
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
        if cfg.synthetic.seed != self.cfg.synthetic.seed || cfg.synthetic.scale != self.cfg.synthetic.scale {
            log::warn!("[synthetic] seed/scale changes take effect after a restart");
        }
        self.cfg = cfg;
        if relayout {
            for f in &mut self.model.files {
                f.arc = 0.0;
            }
            self.relayout();
        }
        self.sync_spin();
        self.lut_dirty = true;
        relayout
    }

    fn ring_speed(&self, ring: &layout::Ring) -> f32 {
        let h = ring.hash();
        let sign = if hash01(h ^ 0xAB) < 0.5 { -1.0 } else { 1.0 };
        self.cfg.calm.spin_speed / 60.0 * sign * (0.35 + 1.3 * hash01(h))
    }

    /// Applies the configured spin speed to existing rings without making them jump.
    fn sync_spin(&mut self) {
        let t = self.t as f32;
        for i in 0..self.rings.len() {
            let new = self.ring_speed(&self.layout.rings[i]);
            let r = &mut self.rings[i];
            if (r.speed - new).abs() > 1e-7 {
                r.phase0 += (r.speed - new) * t;
                r.speed = new;
                self.dirty_rings.push(i as u32);
            }
        }
    }

    fn tf(&self, t: f64) -> f32 {
        if t <= NEVER * 0.5 { -1.0e9 } else { t as f32 }
    }

    pub fn relayout(&mut self) {
        let started = std::time::Instant::now();
        let first = self.layout_version == 0;
        let mut persist: HashMap<u64, RingPersist> = HashMap::with_capacity(self.layout.rings.len());
        for (i, r) in self.layout.rings.iter().enumerate() {
            if i < self.rings.len() {
                persist.insert(r.key(), RingPersist { gpu: self.rings[i], open: self.ring_open[i], presence: self.ring_presence[i] });
            }
        }

        let params = self.layout_params();
        // Directories with current activity get expanded first ("unfolding").
        let hot: Vec<f32> = self.dir_act.iter().map(|&a| if a >= HOT_MIN { a } else { 0.0 }).collect();
        self.layout = layout::build(&mut self.model, &params, &hot);
        let t_build = started.elapsed().as_secs_f64() * 1e3;
        let m = &self.model;
        let n_rings = self.layout.rings.len();

        // Segments.
        let now_unix = m.epoch_unix as f64 + self.t - self.age_offset;
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
            g.t_retract = -1.0e9;
            g.t_age = -1.0e9;
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
            g.age_from = g.age_days;
            segs.push(g);
        }
        self.segs = segs;

        // Rings, carrying animation state over by key.
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
                self.rings.push(RingGpu {
                    depth,
                    speed: self.ring_speed(r),
                    phase0: hash01(h ^ 0x1234) + dir_hash * 0.3,
                    boost: 0.0,
                    boost_t0: -1.0e9,
                    hash: hash01(h ^ 0x77),
                    first_seg: 0,
                    _pad: 0.0,
                });
                self.ring_open.push(0.0);
                self.ring_presence.push(if first { 1.0 } else { 0.0 });
            }
        }

        let t_segs = started.elapsed().as_secs_f64() * 1e3;
        // Segment atlas: one row per ring, ATLAS_W texels around it, holding the
        // segment's 1-based position within its ring (segments are stored in ring
        // order, so the shader adds the ring's first segment index).
        let w = ATLAS_W as usize;
        self.atlas = vec![0u8; w * n_rings.max(1)];
        let mut first = u32::MAX;
        let mut cur_ring = u32::MAX;
        for (i, s) in self.layout.segments.iter().enumerate() {
            if s.ring != cur_ring {
                cur_ring = s.ring;
                first = i as u32;
                self.rings[s.ring as usize].first_seg = first;
            }
            let local = (i as u32 - first + 1).min(255) as u8;
            let row = s.ring as usize * w;
            let x0 = (s.a0 * w as f32).floor().max(0.0) as usize;
            let x1 = ((s.a1 * w as f32).ceil() as usize).min(w);
            self.atlas[row + x0..row + x1].fill(local);
        }

        let t_atlas = started.elapsed().as_secs_f64() * 1e3;
        let n_dirs = m.dirs.len();
        self.dir_act.resize(n_dirs, 0.0);
        self.dir_open_target.resize(n_dirs, 0.0);
        self.dir_rings = vec![0; n_dirs];
        for r in &self.layout.rings {
            let c = &mut self.dir_rings[r.dir as usize];
            *c = c.saturating_add(1);
        }

        self.ring_presence_target = vec![1.0; n_rings];
        self.layout_t = self.t;
        self.layout_version += 1;
        self.needs_relayout = false;
        self.dirty_segs.clear();
        self.dirty_rings.clear();
        self.lut_dirty = true;
        self.open_dirty = true;
        if matches!(self.loop_stage, LoopStage::WindDown(_)) {
            // Rebuilt mid wind-down (e.g. a config change): keep what was already restored.
            if self.restored {
                for (i, seg) in self.layout.segments.iter().enumerate() {
                    if matches!(seg.source, SegSource::File(f) if (f as usize) >= self.initial_files) {
                        self.segs[i].t_retract = self.t as f32 - 10.0;
                    }
                }
            }
            if self.retired {
                self.loop_retire();
            }
        }
        self.update_lut();
        log::debug!(
            "layout v{}: {} rings, {} segments, {} dirs expanded, {} collapsed, {:.1} ms (build {t_build:.1}, segs+rings {:.1}, atlas {:.1})",
            self.layout_version,
            n_rings,
            self.segs.len(),
            self.layout.expanded,
            self.layout.collapsed,
            started.elapsed().as_secs_f64() * 1e3,
            t_segs - t_build,
            t_atlas - t_segs
        );
    }

    /// Advances the simulation by `dt` seconds.
    pub fn step(&mut self, dt: f64) {
        let dt = dt.clamp(0.0, 0.5);
        let t = self.t + dt;
        match &mut self.source {
            Source::Synthetic { index, session } => {
                if self.loop_stage == LoopStage::Running {
                    session.step(&mut self.model, index, t, dt);
                }
            }
            Source::Collector(_) => self.poll_collector(t),
        }
        self.t = t;
        let tf = t as f32;

        let events = self.model.drain_events();
        let n_events = events.len();
        let min_flash = 1.0 / self.cfg.calm.max_flash_hz.max(0.1);
        let mut falls = vec![];
        let mut touched: Vec<u32> = vec![];
        for ev in events {
            let file = &self.model.files[ev.file as usize];
            let dir = file.dir as usize;
            let seg = self.layout.seg_of_file.get(ev.file as usize).copied().unwrap_or(NONE);
            let impulse = match ev.kind {
                EventKind::Read => 0.35,
                EventKind::Write => 1.0,
                EventKind::Create => 0.8,
                EventKind::Delete => 1.2,
            };
            if dir < self.dir_act.len() {
                // Saturates: a folder written to constantly (a log, a soak test)
                // stays open but does not dominate the layout forever.
                let a = &mut self.dir_act[dir];
                *a += impulse * (1.0 - *a / DIR_ACT_MAX).max(0.0);
            }
            if ev.kind == EventKind::Create {
                if self.layout.is_expanded(dir as u32) {
                    self.needs_relayout = true;
                } else if let Some(a) = self.layout.agg_for(&self.model, dir as u32)
                    && let Some(g) = self.segs.get_mut(a as usize)
                    && tf - g.t_write >= min_flash
                {
                    // New file inside a collapsed subtree: its aggregate flashes.
                    g.t_write = tf;
                    self.dirty_segs.push(a);
                }
                touched.push(dir as u32);
                continue;
            }
            touched.push(dir as u32);
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
                    s.age_from = 0.0;
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
                    if matches!(self.layout.segments[seg as usize].source, SegSource::File(_)) {
                        s.flags |= SEG_DELETED;
                        s.t_delete = tf;
                    } else {
                        // A file inside a collapsed subtree: flash the aggregate instead.
                        s.t_write = tf;
                    }
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
            if ev.kind == EventKind::Delete
                && self.cfg.events.delete.fall
                && matches!(self.layout.segments[seg as usize].source, SegSource::File(_))
            {
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

        // Expire deletion ghosts occasionally (a looping session restores them instead).
        if !self.looping() && t - self.last_purge_t > 30.0 {
            self.last_purge_t = t;
            let before = t - self.cfg.events.delete.ghost_hours as f64 * 3600.0;
            if self.model.purge_deleted(before) > 0 {
                self.needs_relayout = true;
            }
        }
        let min_interval = if self.is_synthetic() { RELAYOUT_MIN_INTERVAL } else { RELAYOUT_LIVE_INTERVAL };
        if self.needs_relayout && !self.hidden && t - self.layout_t >= min_interval {
            self.relayout();
        }

        // A busy directory that is collapsed gets unfolded by the next layout.
        if t - self.layout_t >= UNFOLD_INTERVAL
            && touched.iter().any(|&d| !self.layout.is_expanded(d) && self.dir_act.get(d as usize).is_some_and(|&a| a >= UNFOLD_ACT))
        {
            self.needs_relayout = true;
        }

        self.update_loop();
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
        let s = self.rings.get(first_ring as usize)?.first_seg;
        self.segs.get(s as usize).filter(|g| g.flags & layout::SEG_DIR != 0).map(|_| s)
    }

    fn looping(&self) -> bool {
        self.is_synthetic() && self.cfg.synthetic.loop_len.0 > 0.0
    }

    /// Switches from synthetic data to a collector that became available; the
    /// synthetic tower stays on screen until the collector's snapshot arrives.
    pub fn switch_to_collector(&mut self, socket: PathBuf) {
        self.source = Source::Collector(CollectorClient::spawn(socket));
        self.loop_stage = LoopStage::Running;
        self.loop_fade = 1.0;
    }

    pub fn is_synthetic(&self) -> bool {
        matches!(self.source, Source::Synthetic { .. })
    }

    /// True once there is data to show (always for synthetic data).
    pub fn has_data(&self) -> bool {
        match &self.source {
            Source::Synthetic { .. } => true,
            Source::Collector(c) => c.has_snapshot,
        }
    }

    /// Current work-session phase, "wind-down" at the end of a loop, or "live".
    pub fn phase_name(&self) -> &'static str {
        match (&self.source, self.loop_stage) {
            (Source::Collector(_), _) => "live",
            (Source::Synthetic { session, .. }, LoopStage::Running) => session.phase_name(),
            (_, LoopStage::WindDown(_)) => "wind-down",
        }
    }

    /// Applies whatever the collector sent since the last frame.
    fn poll_collector(&mut self, t: f64) {
        let Source::Collector(client) = &mut self.source else { return };
        let mut snapshot = None;
        let mut ops = Vec::new();
        let mut notice = None;
        while let Some(msg) = client.try_recv() {
            match msg {
                Incoming::Snapshot(m) => {
                    // A new snapshot supersedes anything queued before it.
                    ops.clear();
                    snapshot = Some(m);
                    client.has_snapshot = true;
                }
                Incoming::Ops(o) => ops.extend(o),
                Incoming::Status(s) => {
                    if !client.has_snapshot {
                        notice = Some(Notice { text: format!("gits-collector: {s}…"), error: false, until: f64::INFINITY });
                    }
                }
                Incoming::Disconnected(e) => {
                    notice = Some(Notice { text: format!("{e} (retrying)"), error: true, until: f64::INFINITY });
                }
            }
        }
        if let Some(n) = notice {
            self.notice = Some(n);
        }
        if let Some(mut m) = snapshot {
            m.epoch_unix = wall_clock() - t as i64;
            log::info!("collector snapshot: {} directories, {} files", m.live_dir_count(), m.live_file_count());
            self.notice = Some(Notice {
                text: format!("gits-collector: {} folders, {} files", m.live_dir_count(), m.live_file_count()),
                error: false,
                until: t + 6.0,
            });
            self.model = *m;
            self.fx = Fx::default();
            self.dir_act.clear();
            self.needs_relayout = true;
            self.layout_t = -1.0e9;
        }
        let mut applied = 0usize;
        for op in &ops {
            if self.model.apply(op, t) {
                applied += 1;
            }
        }
        if applied < ops.len() {
            log::debug!("ignored {} ops for unknown ids", ops.len() - applied);
        }
        use gits_core::protocol::Op;
        let layout = &self.layout;
        let model = &self.model;
        let expanded_parent = |d: u32| model.dirs.get(d as usize).and_then(|x| x.parent).is_some_and(|p| layout.is_expanded(p));
        if ops.iter().any(|o| match o {
            Op::MkDir { parent, .. } => layout.is_expanded(*parent),
            Op::RmDir { dir } => layout.is_expanded(*dir) || expanded_parent(*dir),
            _ => false,
        }) {
            self.needs_relayout = true;
        }
    }

    fn update_loop(&mut self) {
        match self.loop_stage {
            LoopStage::Running => {
                let len = (self.cfg.synthetic.loop_len.0 as f64).max(MIN_LOOP);
                if self.looping() && self.t - self.loop_start >= len - RESET_AT {
                    log::debug!("loop {}: winding down at t={:.0}s", self.loop_count, self.t);
                    self.loop_stage = LoopStage::WindDown(self.t);
                    self.restored = false;
                    self.retired = false;
                }
                self.loop_fade = 1.0;
            }
            LoopStage::WindDown(since) => {
                let e = self.t - since;
                self.loop_fade = 1.0 - smoothstep(0.0, WIND_FADE as f32, e as f32);
                if e >= RESTORE_AT && !self.restored {
                    self.restored = true;
                    self.loop_restore();
                }
                if e >= RETIRE_AT && !self.retired {
                    self.retired = true;
                    self.loop_retire();
                }
                if e >= RESET_AT {
                    self.loop_reset();
                }
            }
        }
    }

    /// Highlights have faded out: shrink away files created during the loop,
    /// grow deleted files back, and ease changed ages back to their originals.
    fn loop_restore(&mut self) {
        let t = self.t as f32;
        let mut rng = Rng::new(0x100B ^ self.loop_count);
        let epoch = self.initial_model.epoch_unix;
        for f in 0..self.initial_files {
            let orig = &self.initial_model.files[f];
            let cur = &mut self.model.files[f];
            cur.t_read = NEVER;
            cur.t_write = NEVER;
            cur.t_delete = NEVER;
            cur.mtime = orig.mtime;
            cur.size = orig.size;
        }
        for (i, seg) in self.layout.segments.iter().enumerate() {
            let g = &mut self.segs[i];
            g.t_read = -1.0e9;
            g.t_write = -1.0e9;
            match seg.source {
                SegSource::File(f) if (f as usize) < self.initial_files => {
                    if g.flags & SEG_DELETED != 0 {
                        g.flags &= !SEG_DELETED;
                        g.t_delete = -1.0e9;
                        g.t_create = t + rng.range(0.0, RESTORE_STAGGER) as f32;
                    }
                    let orig_age = ((epoch - self.initial_model.files[f as usize].mtime) as f64 / 86400.0).max(0.0) as f32;
                    if (orig_age - g.age_days).abs() > 1e-4 {
                        g.age_from = g.age_days;
                        g.age_days = orig_age;
                        g.t_age = t + rng.range(0.0, RESTORE_STAGGER) as f32;
                    }
                }
                SegSource::File(_) => g.t_retract = t + rng.range(0.0, RESTORE_STAGGER) as f32,
                SegSource::Dir(_) | SegSource::Agg(_) => {}
            }
        }
        self.dirty_segs.extend(0..self.segs.len() as u32);
        self.fx.cards.clear();
        self.fx.probe.target = None;
    }

    /// Rings that only held files created during the loop ease out of the tower.
    fn loop_retire(&mut self) {
        let mut keep = vec![false; self.layout.rings.len()];
        for seg in &self.layout.segments {
            let original = match seg.source {
                SegSource::File(f) => (f as usize) < self.initial_files,
                SegSource::Dir(_) | SegSource::Agg(_) => true,
            };
            if original {
                keep[seg.ring as usize] = true;
            }
        }
        for (i, k) in keep.into_iter().enumerate() {
            if !k {
                self.ring_presence_target[i] = 0.0;
            }
        }
    }

    /// Everything on screen now matches the starting state: swap the model back
    /// and restart the session. Nothing visible changes at this moment.
    fn loop_reset(&mut self) {
        let mut model = self.initial_model.clone();
        for (f, file) in model.files.iter_mut().enumerate() {
            // Keep the arcs currently on screen (they differ if the tower config changed).
            file.arc = self.model.files[f].arc;
        }
        self.model = model;
        if let Source::Synthetic { session, .. } = &mut self.source {
            *session = Session::new(self.cfg.synthetic.seed);
        }
        self.dir_act.iter_mut().for_each(|a| *a = 0.0);
        self.age_offset = self.t;
        self.loop_start = self.t;
        self.loop_count += 1;
        self.loop_stage = LoopStage::Running;
        self.loop_fade = 1.0;
        self.fx.cards.clear();
        self.relayout();
        log::debug!("loop {}: restarted at t={:.0}s", self.loop_count, self.t);
    }

    fn update_activity(&mut self, dt: f32) {
        let tau = if self.loop_stage == LoopStage::Running { self.cfg.calm.activity_decay.0.max(1.0) } else { WIND_ACTIVITY_TAU };
        let decay = (-dt / tau).exp();
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
        let kp_out = 1.0 - (-dt / PRESENCE_OUT_TAU).exp();
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
            let pt = self.ring_presence_target.get(i).copied().unwrap_or(1.0);
            let d = pt - *p;
            if d.abs() > 1e-4 {
                let k = if d < 0.0 { kp_out } else { kp };
                *p = if d.abs() < 2e-3 { pt } else { *p + d * k };
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
        let m = &self.model;
        let f = m.files.get(file as usize)?;
        let mode = if self.sharing && self.cfg.privacy.obfuscate_when_sharing { "glyphs" } else { self.cfg.privacy.text.as_str() };
        match mode {
            "none" => None,
            "dirs" => Some(m.dirs[f.dir as usize].name.clone()),
            "glyphs" => Some(format!("{:08X}", gits_core::rng::mix64(file as u64) as u32)),
            _ => Some(f.name.clone()),
        }
    }
}

pub fn wall_clock() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0)
}

pub fn smoothstep(e0: f32, e1: f32, x: f32) -> f32 {
    let t = ((x - e0) / (e1 - e0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}
