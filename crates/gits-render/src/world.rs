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
use std::sync::Arc;

pub const ATLAS_W: u32 = 1024;
/// How long a ring's row keeps being redrawn every frame after it changed.
const HOT_SECONDS: f32 = 3.0;
/// Each depth layer's cylinder is drawn only over the height bands holding its
/// rings. Bands closer than this are merged (each is padded by half of it).
const BAND_MERGE: f32 = 0.04;
/// At most this many bands per layer (the merge distance grows until it fits).
const MAX_BANDS_PER_LAYER: usize = 128;
pub const LUT_BINS: usize = 16384;
/// The LUT buffer: LUT_BINS + 1 ring coordinates, then per height bin a bit
/// mask of the depth shells that have rings there (as exact small floats).
pub const LUT_LEN: usize = 2 * LUT_BINS + 1;
/// Duration of a write's spin boost. The extra rotation eases in and out
/// (smoothstep), so the ring never jumps in speed.
pub const BOOST_TAU: f32 = 12.0;
/// Max tilt of an unwound ring, in ring pitches (mirrors TILT in tower.wgsl).
pub const TILT: f32 = 0.3;
const RELAYOUT_MIN_INTERVAL: f64 = 0.5;
/// Fixed step of the synthetic work session.
const SESSION_DT: f64 = 1.0 / 24.0;
/// Folder activity (decay, which folders are open) is updated this often.
const ACTIVITY_INTERVAL: f32 = 0.05;
/// Live trees are large (a rebuild takes tens of ms, on a worker thread), so
/// rebuild at most this often. New files appear with the next rebuild.
const RELAYOUT_LIVE_INTERVAL: f64 = 5.0;
/// Directory activity below this does not count as "hot" for layout priority.
const HOT_MIN: f32 = 0.5;
/// Activity at which a collapsed directory is unfolded, and how often that may happen.
const UNFOLD_ACT: f32 = 1.5;
const UNFOLD_INTERVAL: f64 = 5.0;
/// Ceiling for a folder's activity score.
const DIR_ACT_MAX: f32 = 6.0;
/// A folder unwinds when its activity rises above OPEN_ON and winds back below OPEN_OFF.
const OPEN_ON: f32 = 1.5;
const OPEN_OFF: f32 = 0.6;
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
    session_accum: f64,
    session_t: f64,
    /// Time since folder activity was last decayed (it changes slowly).
    activity_dt: f32,
    /// Per ring: fraction of the open pitch it gets (recomputed per layout).
    ring_share: Vec<f32>,
    /// A screencast/screen share is running (labels are obfuscated if configured).
    pub sharing: bool,
    pub notice: Option<Notice>,
    /// Shared with the layout worker while a build runs (nothing mutates it then).
    pub model: Arc<Model>,
    pub source: Source,
    /// A layout being built on a worker thread (live data only).
    pending_layout: Option<std::sync::mpsc::Receiver<LayoutResult>>,
    /// Files before this index already have their arc length.
    arcs_assigned: usize,
    pub t: f64,
    pub layout: Layout,
    pub layout_version: u64,
    layout_t: f64,
    needs_relayout: bool,
    last_purge_t: f64,
    /// Deleted files (deletion time, file) whose ghosts are still shown.
    ghosts: std::collections::VecDeque<(f64, u32)>,
    max_rings: usize,

    pub segs: Vec<SegmentGpu>,
    pub rings: Vec<RingGpu>,
    pub ring_open: Vec<f32>,
    ring_presence: Vec<f32>,
    /// Segment atlas by stable row ("slot"): per row, the 1-based index of the
    /// segment within its ring (0 = gap). A ring keeps its slot across
    /// relayouts, so a rebuild only uploads and recolours rings that changed.
    pub slot_rows: Vec<u8>,
    /// Slot of each ring, and ring of each slot (NONE = free).
    pub ring_slot: Vec<u32>,
    pub slot_ring: Vec<u32>,
    pub n_slots: u32,
    slot_of_key: HashMap<u64, u32>,
    slot_hash: Vec<u64>,
    slot_free: Vec<u32>,
    /// Slots whose atlas row changed since the renderer last uploaded them.
    pub dirty_slots: Vec<u32>,
    /// The renderer must upload every slot (first layout).
    pub slots_full: bool,
    pub lut: Vec<f32>,
    /// Centre height (distance from the tower top) and pitch of each ring.
    pub ring_s: Vec<f32>,
    pub ring_pitch: Vec<f32>,
    /// Per depth layer, the height intervals (distance from the tower top)
    /// holding its rings: `[s0, s1, layer, 0]`.
    pub layer_bands: Vec<[f32; 4]>,
    /// Each depth layer's rings, so the tower shader can visit only the rings of
    /// the layer it is drawing: for layer L and ring index i,
    /// `layer_index[L * (n + 1) + i]` is the position in the list (stored after
    /// the table, from `layers * (n + 1)`) of L's first ring at or after i.
    pub layer_index: Vec<u32>,
    pub layer_index_dirty: bool,

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

    /// Ring colour texture: rebuild every row next frame.
    pub rct_full: bool,
    /// Per ring: redraw its row every frame until this time (see `take_hot_rows`).
    hot_until: Vec<f32>,
    hot: Vec<u32>,

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
            session_accum: 0.0,
            session_t: 0.0,
            activity_dt: 0.0,
            ring_share: vec![],
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
            model: Arc::new(model),
            source,
            pending_layout: None,
            arcs_assigned: 0,
            t: 0.0,
            layout: Layout::default(),
            layout_version: 0,
            layout_t: -1.0,
            needs_relayout: true,
            last_purge_t: 0.0,
            ghosts: Default::default(),
            max_rings,
            segs: vec![],
            rings: vec![],
            ring_open: vec![],
            ring_presence: vec![],
            slot_rows: vec![],
            ring_slot: vec![],
            slot_ring: vec![],
            n_slots: 0,
            slot_of_key: HashMap::new(),
            slot_hash: vec![],
            slot_free: vec![],
            dirty_slots: vec![],
            slots_full: true,
            lut: vec![0.0; LUT_LEN],
            ring_s: vec![],
            ring_pitch: vec![],
            layer_bands: vec![],
            layer_index: vec![],
            layer_index_dirty: false,
            dir_act: vec![],
            dir_open_target: vec![],
            dir_rings: vec![],
            rct_full: true,
            hot_until: vec![],
            hot: vec![],
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
            sectors: t.sectors,
            sector_weight: t.sector_weight,
            min_sector: t.min_sector,
            sector_fill: t.sector_fill,
            max_tree_rings: if t.max_tree_share >= 1.0 {
                0
            } else {
                (t.max_tree_share.max(0.0) * self.max_rings as f32).ceil().max(1.0) as usize
            },
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
            for f in &mut Arc::make_mut(&mut self.model).files {
                f.arc = 0.0;
            }
            self.arcs_assigned = 0;
            self.relayout();
        }
        self.sync_spin();
        self.lut_dirty = true;
        self.rct_full = true;
        relayout
    }

    fn ring_speed(&self, ring: &layout::Ring) -> f32 {
        if self.cfg.tower.sectors {
            // Sectors only line up if the whole tower turns as one.
            return self.cfg.calm.spin_speed / 60.0;
        }
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

    fn building(&self) -> bool {
        self.pending_layout.is_some()
    }

    /// Captures what a layout build needs (assigning arcs to new files first).
    fn layout_job(&mut self) -> LayoutJob {
        let params = self.layout_params();
        self.arcs_assigned = layout::assign_arcs(Arc::make_mut(&mut self.model), &params, self.arcs_assigned);
        // Directories with current activity get expanded first ("unfolding").
        let hot: Vec<f32> = self.dir_act.iter().map(|&a| if a >= HOT_MIN { a } else { 0.0 }).collect();
        LayoutJob {
            now_unix: self.model.epoch_unix as f64 + self.t - self.age_offset,
            model: self.model.clone(),
            params,
            hot,
            prev_layout_t: self.layout_t,
            t: self.t,
        }
    }

    /// Rebuilds the layout now, on this thread.
    pub fn relayout(&mut self) {
        // A build still running on a worker is superseded.
        self.pending_layout = None;
        let job = self.layout_job();
        let res = run_layout_job(job);
        self.install_layout(res);
    }

    /// Starts a rebuild: on a worker thread for live data (large trees), so
    /// frames keep coming while it runs; inline for synthetic data.
    fn start_relayout(&mut self) {
        if self.is_synthetic() || self.layout_version == 0 {
            self.relayout();
            return;
        }
        let job = self.layout_job();
        let (tx, rx) = std::sync::mpsc::channel();
        let spawned = std::thread::Builder::new().name("layout".into()).spawn(move || {
            let _ = tx.send(run_layout_job(job));
        });
        match spawned {
            Ok(_) => {
                self.pending_layout = Some(rx);
                self.needs_relayout = false;
            }
            Err(e) => {
                log::warn!("layout thread: {e}; building inline");
                self.relayout();
            }
        }
    }

    /// Gives every ring a stable slot (atlas row) and copies rows whose content
    /// changed. Returns the number of changed rows.
    fn assign_slots(&mut self, atlas: &[u8], row_hash: &[u64]) -> usize {
        let w = ATLAS_W as usize;
        let n = row_hash.len();
        let mut old = std::mem::take(&mut self.slot_of_key);
        let mut new_keys: HashMap<u64, u32> = HashMap::with_capacity(n);
        self.ring_slot = vec![0; n];
        let mut changed = 0;
        for (i, &hash) in row_hash.iter().enumerate() {
            let key = self.layout.rings[i].key();
            // Duplicate keys (a deleted folder and a new one with the same name)
            // get a fresh slot.
            let reused = if new_keys.contains_key(&key) { None } else { old.remove(&key) };
            let slot = match reused {
                Some(s) => s,
                None => {
                    if let Some(s) = self.slot_free.pop() {
                        self.slot_hash[s as usize] = !hash; // force a copy
                        s
                    } else {
                        self.n_slots += 1;
                        self.slot_rows.resize(self.n_slots as usize * w, 0);
                        self.slot_hash.push(!hash);
                        self.n_slots - 1
                    }
                }
            };
            new_keys.entry(key).or_insert(slot);
            self.ring_slot[i] = slot;
            if self.slot_hash[slot as usize] != hash {
                self.slot_hash[slot as usize] = hash;
                let (d, s) = (slot as usize * w, i * w);
                self.slot_rows[d..d + w].copy_from_slice(&atlas[s..s + w]);
                self.dirty_slots.push(slot);
                changed += 1;
            }
        }
        for (_, s) in old.drain() {
            self.slot_free.push(s);
        }
        self.slot_of_key = new_keys;
        self.slot_ring = vec![NONE; self.n_slots as usize];
        for (i, &s) in self.ring_slot.iter().enumerate() {
            self.slot_ring[s as usize] = i as u32;
        }
        changed
    }

    /// Installs a finished layout, carrying ring animation state over.
    fn install_layout(&mut self, res: LayoutResult) {
        let started = std::time::Instant::now();
        let first = self.layout_version == 0;
        let mut persist: HashMap<u64, RingPersist> = HashMap::with_capacity(self.layout.rings.len());
        for (i, r) in self.layout.rings.iter().enumerate() {
            if i < self.rings.len() {
                persist.insert(r.key(), RingPersist { gpu: self.rings[i], open: self.ring_open[i], presence: self.ring_presence[i] });
            }
        }
        let LayoutResult { layout, segs, atlas, row_hash, first_seg, job_t, t_build, t_segs, t_atlas } = res;
        self.layout = layout;
        self.segs = segs;
        let n_rings = self.layout.rings.len();
        let changed_rows = self.assign_slots(&atlas, &row_hash);

        self.rings = Vec::with_capacity(n_rings);
        self.ring_open = Vec::with_capacity(n_rings);
        self.ring_presence = Vec::with_capacity(n_rings);
        for (i, r) in self.layout.rings.iter().enumerate() {
            let h = r.hash();
            let depth = r.depth as f32;
            if let Some(p) = persist.get(&r.key()) {
                let mut g = p.gpu;
                if self.cfg.tower.sectors {
                    ease_to_sector(&mut g, r.turn0, self.t as f32);
                }
                g.depth = depth;
                g.first_seg = first_seg[i];
                g.row = self.ring_slot[i];
                self.rings.push(g);
                self.ring_open.push(p.open);
                self.ring_presence.push(p.presence);
            } else {
                let dir_hash = hash01(r.dir as u64 * 31 + 7);
                self.rings.push(RingGpu {
                    depth,
                    speed: self.ring_speed(r),
                    phase0: if self.cfg.tower.sectors { r.turn0 } else { hash01(h ^ 0x1234) + dir_hash * 0.3 },
                    boost: 0.0,
                    boost_t0: -1.0e9,
                    hash: hash01(h ^ 0x77),
                    first_seg: first_seg[i],
                    row: self.ring_slot[i],
                });
                self.ring_open.push(0.0);
                self.ring_presence.push(if first { 1.0 } else { 0.0 });
            }
        }

        let n_dirs = self.model.dirs.len();
        self.dir_act.resize(n_dirs, 0.0);
        self.dir_open_target.resize(n_dirs, 0.0);
        self.dir_rings = vec![0; n_dirs];
        for r in &self.layout.rings {
            let c = &mut self.dir_rings[r.dir as usize];
            *c = c.saturating_add(1);
        }

        self.ring_presence_target = vec![1.0; n_rings];
        self.ring_share.clear();
        self.hot_until = vec![f32::MIN; n_rings];
        self.hot.clear();
        // Only changed rows are recoloured (see assign_slots); the first layout
        // colours everything.
        if first {
            self.rct_full = true;
            self.slots_full = true;
        }
        self.mark_animated_rows();
        self.layout_t = job_t;
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
        self.update_layer_index();
        self.update_lut();
        log::debug!(
            "layout v{}: {} rings ({changed_rows} changed), {} segments, {} dirs expanded, {} collapsed; build {t_build:.1} + segs {t_segs:.1} + atlas {t_atlas:.1} ms, install {:.1} ms",
            self.layout_version,
            n_rings,
            self.segs.len(),
            self.layout.expanded,
            self.layout.collapsed,
            started.elapsed().as_secs_f64() * 1e3,
        );
    }

    /// Advances the simulation by `dt` seconds.
    pub fn step(&mut self, dt: f64) {
        let start = std::time::Instant::now();
        let dt = dt.clamp(0.0, 0.5);
        let t = self.t + dt;
        match &mut self.source {
            Source::Synthetic { index, session } => {
                // The session advances in fixed steps, so its story is the same
                // whatever the frame rate; everything else uses `dt` directly.
                self.session_accum += dt;
                while self.session_accum >= SESSION_DT {
                    self.session_accum -= SESSION_DT;
                    self.session_t += SESSION_DT;
                    if self.loop_stage == LoopStage::Running {
                        session.step(Arc::make_mut(&mut self.model), index, self.session_t, SESSION_DT);
                    }
                }
            }
            Source::Collector(_) => self.poll_collector(t),
        }
        self.t = t;
        let tf = t as f32;

        // While a layout is being built the model is shared and unchanged (collector
        // updates wait in their channel), so there is nothing to drain.
        let events = if self.building() { vec![] } else { Arc::make_mut(&mut self.model).drain_events() };
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
            self.fx.on_event(&self.cfg, ev.kind, ev.file, t);
            if ev.kind == EventKind::Create {
                if self.layout.is_expanded(dir as u32) {
                    self.needs_relayout = true;
                } else if let Some(a) = self.layout.agg_for(&self.model, dir as u32)
                    && let Some(g) = self.segs.get_mut(a as usize)
                    && tf - g.t_write >= min_flash
                {
                    // New file inside a collapsed subtree: its aggregate flashes.
                    g.t_write = tf;
                    let ring = g.ring;
                    self.dirty_segs.push(a);
                    self.mark_hot(ring, tf + HOT_SECONDS);
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
                    let spin_up = if self.cfg.tower.sectors { 0.0 } else { self.cfg.events.write.spin_up };
                    if spin_up > 0.0 && (ring as usize) < self.rings.len() {
                        let r = &mut self.rings[ring as usize];
                        // Only one boost at a time: restarting a boost midway
                        // would change the ring's speed abruptly.
                        if tf - r.boost_t0 >= BOOST_TAU {
                            r.phase0 += r.boost;
                            r.boost = spin_up * (0.6 + 0.8 * r.hash);
                            r.boost_t0 = tf;
                        }
                        self.dirty_rings.push(ring);
                    }
                }
                EventKind::Delete => {
                    self.ghosts.push_back((t, ev.file));
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
            self.mark_hot(ring, tf + HOT_SECONDS);
            // Directory tick flashes with any activity inside it.
            if let Some(&first) = self.layout.dir_first_ring.get(dir)
                && first != NONE
                && self.cfg.tower.dir_marks
                && let Some(mark) = self.dir_mark_segment(first)
                && tf - self.segs[mark as usize].t_read >= min_flash
            {
                self.segs[mark as usize].t_read = tf;
                self.dirty_segs.push(mark);
                self.mark_hot(first, tf + HOT_SECONDS);
            }
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
        // Deletion ghosts expire one by one (cheap), in the order they were made.
        let ghost_s = self.cfg.events.delete.ghost_hours as f64 * 3600.0;
        if !self.looping() && !self.building() && self.ghosts.front().is_some_and(|&(td, _)| td < t - ghost_s) {
            let model = Arc::make_mut(&mut self.model);
            while let Some(&(td, f)) = self.ghosts.front() {
                if td >= t - ghost_s {
                    break;
                }
                self.ghosts.pop_front();
                if model.files.get(f as usize).is_some_and(|x| !x.purged && x.t_delete != NEVER) {
                    model.purge_file(f);
                    self.needs_relayout = true;
                }
            }
        }
        // Removed folders are dropped by a full sweep of the tree, which takes a
        // few milliseconds: only while nothing is drawn, or every 30 minutes.
        if !self.looping() && !self.building() && t - self.last_purge_t > if self.hidden { 60.0 } else { 1800.0 } {
            self.last_purge_t = t;
            if Arc::make_mut(&mut self.model).purge_deleted(t - ghost_s) > 0 {
                self.needs_relayout = true;
            }
        }
        if let Some(rx) = &self.pending_layout {
            match rx.try_recv() {
                Ok(res) => {
                    self.pending_layout = None;
                    self.install_layout(res);
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => {}
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    log::warn!("layout worker failed");
                    self.pending_layout = None;
                }
            }
        }
        let min_interval = if self.is_synthetic() { RELAYOUT_MIN_INTERVAL } else { RELAYOUT_LIVE_INTERVAL };
        if self.needs_relayout && !self.hidden && !self.building() && t - self.layout_t >= min_interval {
            self.start_relayout();
        }

        // A busy directory that is collapsed gets unfolded by the next layout.
        if t - self.layout_t >= UNFOLD_INTERVAL
            && touched.iter().any(|&d| !self.layout.is_expanded(d) && self.dir_act.get(d as usize).is_some_and(|&a| a >= UNFOLD_ACT))
        {
            self.needs_relayout = true;
        }

        let p0 = std::time::Instant::now();
        self.update_loop();
        self.activity_dt += dt as f32;
        if self.activity_dt >= ACTIVITY_INTERVAL {
            let adt = std::mem::take(&mut self.activity_dt);
            self.update_activity(adt);
        }
        let p1 = std::time::Instant::now();
        self.update_rings(dt as f32);
        let p2 = std::time::Instant::now();
        if self.lut_dirty {
            self.update_lut();
        }
        let p3 = std::time::Instant::now();
        let fx = std::mem::take(&mut self.fx);
        self.fx = fx.step(self, dt as f32);
        let p4 = std::time::Instant::now();
        if PROFILE.with(|p| p.get()) {
            PROF.with(|pr| {
                let mut pr = pr.borrow_mut();
                pr[0] += (p0 - start).as_secs_f64();
                pr[1] += (p1 - p0).as_secs_f64();
                pr[2] += (p2 - p1).as_secs_f64();
                pr[3] += (p3 - p2).as_secs_f64();
                pr[4] += (p4 - p3).as_secs_f64();
                pr[5] += 1.0;
                if pr[5] >= 300.0 {
                    let n = pr[5];
                    eprintln!(
                        "step profile (ms/step): events+source {:.3} activity {:.3} rings {:.3} lut {:.3} fx {:.3}",
                        pr[0] / n * 1e3,
                        pr[1] / n * 1e3,
                        pr[2] / n * 1e3,
                        pr[3] / n * 1e3,
                        pr[4] / n * 1e3
                    );
                    *pr = [0.0; 6];
                }
            });
        }
    }

    /// Segment a callout for `file` points at: the file's own segment, or while
    /// it has none (created since the last layout) its folder's tick, or the
    /// aggregate of the collapsed folder holding it.
    pub fn anchor_segment(&self, file: u32) -> Option<u32> {
        if let Some(&s) = self.layout.seg_of_file.get(file as usize).filter(|&&s| s != NONE && (s as usize) < self.segs.len()) {
            return Some(s);
        }
        let mut d = self.model.files.get(file as usize)?.dir;
        loop {
            if let Some(&first) = self.layout.dir_first_ring.get(d as usize).filter(|&&r| r != NONE) {
                let ring = self.rings.get(first as usize)?;
                return self.dir_mark_segment(first).or(Some(ring.first_seg).filter(|&s| (s as usize) < self.segs.len()));
            }
            if let Some(&s) = self.layout.agg_seg.get(d as usize).filter(|&&s| s != NONE && (s as usize) < self.segs.len()) {
                return Some(s);
            }
            d = self.model.dirs.get(d as usize)?.parent?;
        }
    }

    fn dir_mark_segment(&self, first_ring: u32) -> Option<u32> {
        // The directory tick is the first segment placed on the band's first ring.
        let s = self.rings.get(first_ring as usize)?.first_seg;
        self.segs.get(s as usize).filter(|g| g.flags & layout::SEG_DIR != 0).map(|_| s)
    }

    /// Keeps ring `ring`'s colour-texture row updated every frame until `until`.
    fn mark_hot(&mut self, ring: u32, until: f32) {
        let Some(h) = self.hot_until.get_mut(ring as usize) else { return };
        if *h < self.t as f32 {
            self.hot.push(ring);
        }
        *h = h.max(until);
    }

    /// Rows with growth/retract animations still to play (after a relayout or restore).
    fn mark_animated_rows(&mut self) {
        let t = self.t as f32;
        let grow = self.cfg.events.create.grow.0;
        for i in 0..self.segs.len() {
            let (s, ring) = (&self.segs[i], self.segs[i].ring);
            let until = (s.t_create + grow).max(s.t_retract + 2.5);
            if until > t {
                self.mark_hot(ring, until + 0.5);
            }
        }
    }

    /// Rows of the ring colour texture that must be redrawn this frame.
    pub fn take_hot_rows(&mut self) -> Vec<u32> {
        let t = self.t as f32;
        let hot_until = &self.hot_until;
        self.hot.retain(|&r| hot_until.get(r as usize).is_some_and(|&u| u >= t));
        self.hot.clone()
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
        if self.building() {
            return; // updates wait in the channel until the layout is installed
        }
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
            self.model = Arc::new(*m);
            self.arcs_assigned = 0;
            self.pending_layout = None;
            self.fx = Fx::default();
            self.dir_act.clear();
            self.needs_relayout = true;
            self.layout_t = -1.0e9;
        }
        let mut applied = 0usize;
        if !ops.is_empty() {
            let model = Arc::make_mut(&mut self.model);
            for op in &ops {
                if model.apply(op, t) {
                    applied += 1;
                }
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
            let cur = &mut Arc::make_mut(&mut self.model).files[f];
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
        self.rct_full = true;
        self.mark_animated_rows();
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
        self.model = Arc::new(model);
        self.arcs_assigned = self.model.files.len();
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
        // Open/closed with hysteresis: a folder opens above OPEN_ON and closes
        // below OPEN_OFF, so a steadily busy folder settles instead of easing
        // forever. Already-open folders are preferred when more than
        // `max_open_dirs` qualify.
        let max_open = self.cfg.calm.max_open_dirs;
        let targets = &self.dir_open_target;
        let mut cand: Vec<(f32, usize)> = self
            .dir_act
            .iter()
            .enumerate()
            .filter(|&(d, &a)| a >= OPEN_ON || (a >= OPEN_OFF && targets.get(d).is_some_and(|&t| t > 0.5)))
            .map(|(d, &a)| (a + if targets.get(d).is_some_and(|&t| t > 0.5) { OPEN_ON } else { 0.0 }, d))
            .collect();
        cand.sort_by(|a, b| b.0.total_cmp(&a.0));
        cand.truncate(max_open);
        self.dir_open_target.iter_mut().for_each(|x| *x = 0.0);
        for (_, d) in cand {
            self.dir_open_target[d] = 1.0;
        }
    }

    fn update_rings(&mut self, dt: f32) {
        let ko = 1.0 - (-dt / OPEN_TAU).exp();
        let kp = 1.0 - (-dt / PRESENCE_TAU).exp();
        let kp_out = 1.0 - (-dt / PRESENCE_OUT_TAU).exp();
        let mut changed = false;
        let mut opened = vec![];
        for (i, r) in self.layout.rings.iter().enumerate() {
            let target = self.dir_open_target.get(r.dir as usize).copied().unwrap_or(0.0);
            let o = &mut self.ring_open[i];
            let d = target - *o;
            if d.abs() > 1e-4 {
                *o += if d.abs() < 2e-3 { d } else { d * ko };
                changed = true;
                opened.push(i as u32);
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
        // Opening changes a ring's colours ("scanned" arcs, wider gaps).
        let until = self.t as f32 + 0.5;
        for r in opened {
            self.mark_hot(r, until);
        }
        if changed {
            self.open_dirty = true;
            self.lut_dirty = true;
        }
    }

    /// Recomputes ring pitches and the height → ring-coordinate lookup table.
    fn update_lut(&mut self) {
        let lut_start = std::time::Instant::now();
        let n = self.layout.rings.len();
        let h = self.cfg.tower.height;
        if self.ring_share.len() != n {
            // A directory unwinds to at most MAX_OPEN_RINGS ring pitches in total,
            // so one huge build directory cannot take over the tower.
            const MAX_OPEN_RINGS: f32 = 5.0;
            self.ring_share = self
                .layout
                .rings
                .iter()
                .map(|r| (MAX_OPEN_RINGS / self.dir_rings.get(r.dir as usize).copied().unwrap_or(1).max(1) as f32).min(1.0))
                .collect();
        }
        let mut u_sum = 0.0f32;
        let mut w_sum = 0.0f32;
        for i in 0..n {
            let (e, u) = (self.ring_presence[i], self.ring_open[i]);
            u_sum += e * u * self.ring_share[i];
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
            let p = e * (u * po * self.ring_share[i] + (1.0 - u) * pw);
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
        // Which shells have rings at each height (for dimming an outer shell's
        // near side where inner shells show through; see tower.wgsl).
        let layers = self.cfg.tower.depth_layers.clamp(1, 24) as usize;
        let mut mask = vec![0u32; LUT_BINS];
        let k = LUT_BINS as f32 / h.max(1e-3);
        for i in 0..n {
            if self.ring_pitch[i] <= 0.0 {
                continue;
            }
            let layer = (self.layout.rings[i].depth as usize).min(layers - 1);
            let b0 = ((ys[i] - BAND_MERGE) * k).max(0.0) as usize;
            let b1 = (((ys[i + 1] + BAND_MERGE) * k).max(0.0) as usize).min(LUT_BINS - 1);
            for m in mask.iter_mut().take(b1 + 1).skip(b0) {
                *m |= 1 << layer;
            }
        }
        for (dst, m) in self.lut[LUT_BINS + 1..].iter_mut().zip(mask) {
            *dst = m as f32;
        }
        let tb = std::time::Instant::now();
        self.update_layer_bands();
        if PROFILE.with(|p| p.get()) {
            PROF.with(|pr| pr.borrow_mut()[5] += 0.0);
            LUTP.with(|l| {
                let mut l = l.borrow_mut();
                l.0 += (tb - lut_start).as_secs_f64();
                l.1 += tb.elapsed().as_secs_f64();
                l.2 += 1;
                if l.2 >= 300 {
                    eprintln!("lut {:.3} ms, bands {:.3} ms", l.0 / l.2 as f64 * 1e3, l.1 / l.2 as f64 * 1e3);
                    *l = (0.0, 0.0, 0);
                }
            });
        }
        self.lut_dirty = true;
    }

    fn update_layer_bands(&mut self) {
        let layers = self.cfg.tower.depth_layers.max(1) as usize;
        let h = self.cfg.tower.height;
        let half = BAND_MERGE * 0.5;
        let mut per: Vec<Vec<[f32; 2]>> = vec![vec![]; layers];
        for (i, r) in self.layout.rings.iter().enumerate() {
            let p = self.ring_pitch.get(i).copied().unwrap_or(0.0);
            if p <= 0.0 {
                continue;
            }
            let layer = (r.depth as usize).min(layers - 1);
            let (s0, s1) = (self.ring_s[i] - p * 0.5 - half, self.ring_s[i] + p * 0.5 + half);
            let bands = &mut per[layer];
            match bands.last_mut() {
                Some(last) if s0 <= last[1] => last[1] = last[1].max(s1),
                _ => bands.push([s0, s1]),
            }
        }
        let mut all = Vec::with_capacity(per.iter().map(|b| b.len().min(MAX_BANDS_PER_LAYER)).sum());
        for (layer, bands) in per.into_iter().enumerate() {
            let bands = if bands.len() > MAX_BANDS_PER_LAYER {
                // Merge across the smallest gaps until it fits.
                let mut gaps: Vec<f32> = bands.windows(2).map(|w| w[1][0] - w[0][1]).collect();
                gaps.sort_by(|a, b| a.total_cmp(b));
                let limit = gaps[bands.len() - MAX_BANDS_PER_LAYER - 1];
                let mut merged: Vec<[f32; 2]> = Vec::with_capacity(MAX_BANDS_PER_LAYER);
                for b in bands {
                    match merged.last_mut() {
                        Some(last) if b[0] - last[1] <= limit => last[1] = last[1].max(b[1]),
                        _ => merged.push(b),
                    }
                }
                merged
            } else {
                bands
            };
            all.extend(bands.into_iter().map(|b| [b[0].clamp(0.0, h), b[1].clamp(0.0, h), layer as f32, 0.0]));
        }
        self.layer_bands = all;
    }

    /// Rebuilds `layer_index` (after a relayout: it depends only on the rings'
    /// order and depths).
    fn update_layer_index(&mut self) {
        let layers = self.cfg.tower.depth_layers.max(1) as usize;
        let idx = layer_index(self.layout.rings.iter().map(|r| (r.depth as usize).min(layers - 1)), self.layout.rings.len(), layers);
        self.layer_index = idx;
        self.layer_index_dirty = true;
    }

    /// Ring rotation (turns) at time `t`; mirrors `ring_phase` in tower.wgsl.
    pub fn ring_phase(&self, ring: usize, t: f32) -> f32 {
        let r = &self.rings[ring];
        let x = ((t - r.boost_t0) / BOOST_TAU).clamp(0.0, 1.0);
        r.phase0 + r.speed * t + r.boost * x * x * (3.0 - 2.0 * x)
    }

    pub fn ring_radius(&self, ring: usize) -> f32 {
        let tw = &self.cfg.tower;
        // Mirrors vs_tower in tower.wgsl.
        let layers = tw.depth_layers.max(1);
        let layer = (self.rings[ring].depth as u32).min(layers - 1);
        tw.inner_radius + (tw.radius - tw.inner_radius) * layer as f32 / (layers - 1).max(1) as f32
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
    /// The privacy text mode in effect: paths | dirs | glyphs | none.
    pub fn label_mode(&self) -> &str {
        if self.sharing && self.cfg.privacy.obfuscate_when_sharing { "glyphs" } else { self.cfg.privacy.text.as_str() }
    }

    pub fn label_for(&self, file: u32) -> Option<String> {
        let m = &self.model;
        let f = m.files.get(file as usize)?;
        match self.label_mode() {
            "none" => None,
            "dirs" => Some(m.dirs[f.dir as usize].name.clone()),
            "glyphs" => Some(format!("{:08X}", gits_core::rng::mix64(file as u64) as u32)),
            _ => Some(f.name.clone()),
        }
    }
}

/// Turns a ring (with sectors) toward its directory's sector start `turn0`:
/// if the sector moved since the ring's last layout, the difference is eased
/// in over BOOST_TAU with the spin-boost mechanism instead of jumping.
fn ease_to_sector(g: &mut RingGpu, turn0: f32, t: f32) {
    let wrap = |x: f32| x - x.round();
    let x = ((t - g.boost_t0) / BOOST_TAU).clamp(0.0, 1.0);
    let now = g.phase0 + g.boost * x * x * (3.0 - 2.0 * x);
    if wrap(turn0 - (g.phase0 + g.boost)).abs() < 1e-4 {
        return; // already there, or on the way
    }
    g.phase0 = now;
    g.boost = wrap(turn0 - now);
    g.boost_t0 = t;
}

/// See `World::layer_index`. `ring_layers` gives each ring's layer, in order.
fn layer_index(ring_layers: impl Iterator<Item = usize> + Clone, n: usize, layers: usize) -> Vec<u32> {
    let mut start = vec![0u32; layers + 1];
    for l in ring_layers.clone() {
        start[l + 1] += 1;
    }
    for l in 0..layers {
        start[l + 1] += start[l];
    }
    let list_at = (layers * (n + 1)) as u32;
    let mut idx = vec![0u32; layers * (n + 1) + n];
    let mut next: Vec<u32> = start[..layers].to_vec();
    for (i, l) in ring_layers.enumerate() {
        for (k, nx) in next.iter().enumerate() {
            idx[k * (n + 1) + i] = list_at + nx;
        }
        idx[(list_at + next[l]) as usize] = i as u32;
        next[l] += 1;
    }
    for (k, nx) in next.iter().enumerate() {
        idx[k * (n + 1) + n] = list_at + nx;
    }
    idx
}

/// Inputs of a layout build (see `World::start_relayout`).
struct LayoutJob {
    model: Arc<Model>,
    params: LayoutParams,
    hot: Vec<f32>,
    now_unix: f64,
    prev_layout_t: f64,
    t: f64,
}

struct LayoutResult {
    layout: Layout,
    segs: Vec<SegmentGpu>,
    /// Ring-ordered atlas rows, and a hash of each row.
    atlas: Vec<u8>,
    row_hash: Vec<u64>,
    first_seg: Vec<u32>,
    job_t: f64,
    t_build: f64,
    t_segs: f64,
    t_atlas: f64,
}

fn tf(t: f64) -> f32 {
    if t <= NEVER * 0.5 { -1.0e9 } else { t as f32 }
}

/// Builds the layout, the GPU segment records and the segment atlas. Pure: runs
/// on a worker thread for live data.
fn run_layout_job(job: LayoutJob) -> LayoutResult {
    let t0 = std::time::Instant::now();
    let m = &*job.model;
    let layout = layout::build(m, &job.params, &job.hot);
    let t1 = std::time::Instant::now();
    let mut segs = Vec::with_capacity(layout.segments.len());
    for (i, s) in layout.segments.iter().enumerate() {
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
                g.age_days = ((job.now_unix - file.mtime as f64) / 86400.0).max(0.0) as f32;
                g.size_f = ((file.size as f64 + 1.0).log2() / 34.0).clamp(0.0, 1.0) as f32;
                g.t_read = tf(file.t_read);
                g.t_write = tf(file.t_write);
                if file.t_create != NEVER {
                    // Files created since the previous layout grow in now.
                    g.t_create = if file.t_create > job.prev_layout_t { job.t as f32 } else { file.t_create as f32 };
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
    let t2 = std::time::Instant::now();
    // Segment atlas: one row per ring, ATLAS_W texels around it, holding the
    // segment's 1-based position within its ring (segments are stored in ring
    // order, so the shader adds the ring's first segment index).
    let n_rings = layout.rings.len();
    let w = ATLAS_W as usize;
    let mut atlas = vec![0u8; w * n_rings.max(1)];
    let mut first_seg = vec![0u32; n_rings];
    let mut first = u32::MAX;
    let mut cur_ring = u32::MAX;
    for (i, s) in layout.segments.iter().enumerate() {
        if s.ring != cur_ring {
            cur_ring = s.ring;
            first = i as u32;
            first_seg[s.ring as usize] = first;
        }
        let local = (i as u32 - first + 1).min(255) as u8;
        let row = s.ring as usize * w;
        let x0 = (s.a0 * w as f32).floor().max(0.0) as usize;
        let x1 = ((s.a1 * w as f32).ceil() as usize).min(w);
        atlas[row + x0..row + x1].fill(local);
    }
    let row_hash: Vec<u64> = atlas
        .chunks_exact(w)
        .take(n_rings)
        .map(|row| {
            row.chunks_exact(8).fold(0x9E37_79B9_7F4A_7C15u64, |h, c| gits_core::rng::mix64(h ^ u64::from_le_bytes(c.try_into().unwrap())))
        })
        .collect();
    let t3 = std::time::Instant::now();
    let ms = |a: std::time::Instant, b: std::time::Instant| (b - a).as_secs_f64() * 1e3;
    LayoutResult { layout, segs, atlas, row_hash, first_seg, job_t: job.t, t_build: ms(t0, t1), t_segs: ms(t1, t2), t_atlas: ms(t2, t3) }
}

thread_local! {
    static PROFILE: std::cell::Cell<bool> = std::cell::Cell::new(std::env::var_os("GITS_PROFILE").is_some());
    static PROF: std::cell::RefCell<[f64; 6]> = const { std::cell::RefCell::new([0.0; 6]) };
    static LUTP: std::cell::RefCell<(f64, f64, u32)> = const { std::cell::RefCell::new((0.0, 0.0, 0)) };
}

pub fn wall_clock() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0)
}

pub fn smoothstep(e0: f32, e1: f32, x: f32) -> f32 {
    let t = ((x - e0) / (e1 - e0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layer_index_lists_each_layers_rings() {
        // Rings 0..6 in layers 0 1 1 0 2 1 (3 layers).
        let ring_layers = [0usize, 1, 1, 0, 2, 1];
        let (n, layers) = (ring_layers.len(), 3);
        let idx = layer_index(ring_layers.iter().copied(), n, layers);
        let list_at = layers * (n + 1);
        // The rings of `layer` from ring index a up to (not including) b.
        let rings = |layer: usize, a: usize, b: usize| -> Vec<u32> {
            let (p0, p1) = (idx[layer * (n + 1) + a] as usize, idx[layer * (n + 1) + b] as usize);
            assert!(p0 >= list_at && p1 <= idx.len());
            idx[p0..p1].to_vec()
        };
        assert_eq!(rings(0, 0, n), [0, 3]);
        assert_eq!(rings(1, 0, n), [1, 2, 5]);
        assert_eq!(rings(2, 0, n), [4]);
        assert_eq!(rings(1, 2, 5), [2]);
        assert_eq!(rings(0, 1, 3), Vec::<u32>::new());
        assert_eq!(rings(2, 4, 5), [4]);
    }
}
