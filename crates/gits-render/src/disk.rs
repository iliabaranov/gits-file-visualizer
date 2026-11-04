//! Disk usage chart, drawn above the system readout: a thin inner gauge shows
//! how full the disk holding the home folder is, and up to three rings show
//! which folders in the home folder use the space (largest labelled).
//!
//! The home folder is measured like `du -x` (allocated blocks, one file system,
//! hard links counted once, symbolic links not followed) on a background thread
//! at idle CPU and I/O priority, and only while the readout is on screen. Unlike
//! the tower it includes caches and build folders, which are often what fills a
//! disk. The result is cached in `~/.cache`, so a restarted renderer shows it at
//! once and re-measures only when it is older than `[system_monitor] disk_every`,
//! or when asked to: `gits-restart` creates `disk-rescan` next to the cache, and
//! the chart keeps showing the previous result while it measures again.

use crate::fx::{line_px, quad_px, text_px};
use crate::gpu_types::SpriteVertex;
use crate::text::GlyphAtlas;
use gits_core::config::Config;
use std::collections::HashSet;
use std::f32::consts::TAU;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// Folder levels below home kept for the chart; deeper sizes add up into their
/// level-3 ancestor.
const LEVELS: usize = 3;
/// Folders smaller than this share of the home folder are not kept.
const MIN_SHARE: f64 = 0.01;
const MAX_CHILDREN: usize = 12;
/// Labelled folders: the largest top-level ones, at least this share of home.
const MAX_LABELS: usize = 5;
const LABEL_SHARE: f64 = 0.03;
const STATFS_EVERY: Duration = Duration::from_secs(60);
const POLL: Duration = Duration::from_secs(5);
const CACHE_MAGIC: &str = "gits-disk-usage 1";

// Layout at 1080p (scaled with the screen), sharing the readout's column.
const PANEL_H: f32 = 196.0;
const GAP: f32 = 10.0;
const GAUGE: (f32, f32) = (26.0, 30.0);
/// Inner and outer radius of the folder rings, level 1 to 3.
const RINGS: [(f32, f32); LEVELS] = [(33.0, 55.0), (57.0, 67.0), (69.0, 76.0)];
const LABEL_PITCH: f32 = 21.0;

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Node {
    pub name: String,
    /// Bytes on disk, this folder and everything below it.
    pub size: u64,
    /// Largest first; small ones are left out.
    pub children: Vec<Node>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct FsStats {
    pub total: u64,
    pub used: u64,
    pub avail: u64,
}

#[derive(Default)]
struct Shared {
    home: Option<Arc<Node>>,
    fs: Option<FsStats>,
}

pub struct DiskUsage {
    shared: Arc<Mutex<Shared>>,
    active: Arc<AtomicBool>,
    enabled: Arc<AtomicBool>,
    every_s: Arc<AtomicU64>,
}

impl DiskUsage {
    /// `enabled`: `[system_monitor] disk` (kept up to date by `build`).
    pub fn start(enabled: bool) -> Self {
        let shared = Arc::new(Mutex::new(Shared::default()));
        let active = Arc::new(AtomicBool::new(true));
        let enabled = Arc::new(AtomicBool::new(enabled));
        let every_s = Arc::new(AtomicU64::new(1800));
        let (sh, a, en, ev) = (Arc::downgrade(&shared), active.clone(), enabled.clone(), every_s.clone());
        let spawned = std::thread::Builder::new().name("disk".into()).spawn(move || {
            lower_priority();
            let Some(home) = std::env::var_os("HOME").map(PathBuf::from) else { return };
            let mut last_fs: Option<Instant> = None;
            let mut measured_at: Option<i64> = None;
            let mut cache_tried = false;
            loop {
                let Some(shared) = sh.upgrade() else { return };
                if a.load(Ordering::Relaxed) && en.load(Ordering::Relaxed) {
                    if last_fs.is_none_or(|t| t.elapsed() >= STATFS_EVERY) {
                        last_fs = Some(Instant::now());
                        let fs = fs_stats(&home);
                        lock(&shared).fs = fs;
                    }
                    if !cache_tried {
                        cache_tried = true;
                        if let Some((at, node)) = read_cache() {
                            measured_at = Some(at);
                            lock(&shared).home = Some(Arc::new(node));
                        }
                    }
                    let every = ev.load(Ordering::Relaxed).max(60) as i64;
                    let requested = rescan_path().is_some_and(|p| p.exists());
                    if requested || measured_at.is_none_or(|at| unix_now() - at >= every) {
                        if requested {
                            log::info!("disk: re-measuring on request");
                            if let Some(p) = rescan_path() {
                                let _ = std::fs::remove_file(p);
                            }
                        }
                        drop(shared);
                        let started = Instant::now();
                        let node = measure(&home);
                        log::info!(
                            "disk: measured the home folder in {:.1}s: {:.1} GB",
                            started.elapsed().as_secs_f32(),
                            node.size as f64 / 1e9
                        );
                        measured_at = Some(unix_now());
                        write_cache(&node, unix_now());
                        let Some(shared) = sh.upgrade() else { return };
                        lock(&shared).home = Some(Arc::new(node));
                        continue;
                    }
                }
                drop(shared);
                std::thread::sleep(POLL);
            }
        });
        if let Err(e) = spawned {
            log::warn!("disk usage thread: {e}");
        }
        Self { shared, active, enabled, every_s }
    }

    /// Waits until there is something to draw (snapshots), at most `timeout`.
    pub fn wait_ready(&self, timeout: Duration) -> bool {
        let until = Instant::now() + timeout;
        while Instant::now() < until {
            if lock(&self.shared).home.is_some() {
                return true;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        false
    }

    /// Measuring and polling happen only while the chart is on screen.
    pub fn set_active(&self, on: bool) {
        self.active.store(on, Ordering::Relaxed);
    }

    /// Overlay sprites for the panel whose bottom edge is at `bottom` (pixels).
    #[allow(clippy::too_many_arguments)]
    pub fn build(&self, cfg: &Config, atlas: &GlyphAtlas, w: f32, h: f32, bottom: f32, names: bool, out: &mut Vec<SpriteVertex>) {
        self.enabled.store(cfg.system_monitor.disk, Ordering::Relaxed);
        self.every_s.store(cfg.system_monitor.disk_every.0.max(60.0) as u64, Ordering::Relaxed);
        if !cfg.system_monitor.disk {
            return;
        }
        let (home, fs) = {
            let s = lock(&self.shared);
            (s.home.clone(), s.fs)
        };
        draw(home.as_deref(), fs, cfg, atlas, w, h, bottom, names, out);
    }
}

/// Height the panel adds above the readout (pixels), for keeping callouts clear.
pub fn panel_height(w: f32, h: f32) -> f32 {
    (PANEL_H + GAP) * w.min(h) / 1080.0
}

fn lock(s: &Mutex<Shared>) -> std::sync::MutexGuard<'_, Shared> {
    s.lock().unwrap_or_else(|e| e.into_inner())
}

fn unix_now() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs() as i64)
}

/// Idle CPU and I/O priority for this thread only (Linux applies both per thread).
fn lower_priority() {
    // SAFETY: plain syscalls on the calling thread's own id.
    unsafe {
        let tid = libc::syscall(libc::SYS_gettid) as libc::id_t;
        libc::setpriority(libc::PRIO_PROCESS, tid, 19);
        // IOPRIO_WHO_PROCESS = 1, IOPRIO_CLASS_IDLE = 3.
        libc::syscall(libc::SYS_ioprio_set, 1, tid, 3 << 13);
    }
}

fn fs_stats(path: &Path) -> Option<FsStats> {
    let c = std::ffi::CString::new(path.as_os_str().as_bytes()).ok()?;
    // SAFETY: statvfs fills the zeroed struct; the path is a valid C string.
    let mut s: libc::statvfs = unsafe { std::mem::zeroed() };
    if unsafe { libc::statvfs(c.as_ptr(), &mut s) } != 0 {
        return None;
    }
    let f = s.f_frsize as u64;
    let total = s.f_blocks as u64 * f;
    Some(FsStats { total, used: total.saturating_sub(s.f_bfree as u64 * f), avail: s.f_bavail as u64 * f })
}

// ---------------------------------------------------------------------------
// Measuring
// ---------------------------------------------------------------------------

fn measure(home: &Path) -> Node {
    let Ok(md) = std::fs::symlink_metadata(home) else { return Node { name: "~".into(), ..Default::default() } };
    let mut seen = HashSet::new();
    let mut root = Node { name: "~".into(), size: 0, children: vec![] };
    root.size = md.blocks() * 512 + measure_dir(home, md.dev(), 1, &mut root.children, &mut seen);
    let total = root.size;
    prune(&mut root, total);
    root
}

/// Bytes below `dir`; folders at `level` <= LEVELS become nodes in `kids`.
fn measure_dir(dir: &Path, dev: u64, level: usize, kids: &mut Vec<Node>, seen: &mut HashSet<(u64, u64)>) -> u64 {
    let Ok(rd) = std::fs::read_dir(dir) else { return 0 };
    let mut total = 0;
    for e in rd.flatten() {
        // Not following symbolic links.
        let Ok(md) = e.metadata() else { continue };
        if md.dev() != dev {
            continue; // another file system mounted here
        }
        if md.is_dir() {
            let own = md.blocks() * 512;
            if level <= LEVELS {
                let mut child = Node { name: e.file_name().to_string_lossy().into_owned(), size: 0, children: vec![] };
                child.size = own + measure_dir(&e.path(), dev, level + 1, &mut child.children, seen);
                total += child.size;
                kids.push(child);
            } else {
                total += own + measure_tree(&e.path(), dev, seen);
            }
        } else {
            total += file_bytes(&md, seen);
        }
    }
    total
}

/// Bytes below `dir`, without keeping nodes (iterative: trees can be deep).
fn measure_tree(dir: &Path, dev: u64, seen: &mut HashSet<(u64, u64)>) -> u64 {
    let mut total = 0;
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&d) else { continue };
        for e in rd.flatten() {
            let Ok(md) = e.metadata() else { continue };
            if md.dev() != dev {
                continue;
            }
            if md.is_dir() {
                total += md.blocks() * 512;
                stack.push(e.path());
            } else {
                total += file_bytes(&md, seen);
            }
        }
    }
    total
}

fn file_bytes(md: &std::fs::Metadata, seen: &mut HashSet<(u64, u64)>) -> u64 {
    if md.nlink() > 1 && !seen.insert((md.dev(), md.ino())) {
        return 0; // a hard link to a file already counted
    }
    md.blocks() * 512
}

/// Sorts children largest first and drops those too small to see.
fn prune(node: &mut Node, home: u64) {
    let min = (home as f64 * MIN_SHARE) as u64;
    node.children.retain(|c| c.size >= min.max(1));
    node.children.sort_by(|a, b| b.size.cmp(&a.size).then_with(|| a.name.cmp(&b.name)));
    node.children.truncate(MAX_CHILDREN);
    for c in &mut node.children {
        prune(c, home);
    }
}

// ---------------------------------------------------------------------------
// Cache: one line per node in pre-order, "<level>\t<bytes>\t<name>".
// ---------------------------------------------------------------------------

fn cache_path() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".cache")))?;
    Some(base.join("gits-file-visualizer").join("disk-usage.txt"))
}

/// Created by `gits-restart` to ask for a new measurement now.
fn rescan_path() -> Option<PathBuf> {
    cache_path().map(|p| p.with_file_name("disk-rescan"))
}

fn write_cache(node: &Node, at: i64) {
    let Some(path) = cache_path() else { return };
    let mut text = format!("{CACHE_MAGIC} {at}\n");
    serialize(node, 0, &mut text);
    let tmp = path.with_extension("tmp");
    let res = std::fs::create_dir_all(path.parent().unwrap_or(Path::new(".")))
        .and_then(|_| std::fs::write(&tmp, text))
        .and_then(|_| std::fs::rename(&tmp, &path));
    if let Err(e) = res {
        log::warn!("disk: could not write {}: {e}", path.display());
    }
}

fn serialize(node: &Node, level: usize, out: &mut String) {
    let name: String = node.name.chars().map(|c| if c == '\t' || c == '\n' || c == '\r' { ' ' } else { c }).collect();
    out.push_str(&format!("{level}\t{}\t{name}\n", node.size));
    for c in &node.children {
        serialize(c, level + 1, out);
    }
}

fn read_cache() -> Option<(i64, Node)> {
    parse_cache(&std::fs::read_to_string(cache_path()?).ok()?)
}

fn parse_cache(text: &str) -> Option<(i64, Node)> {
    let mut lines = text.lines();
    let at: i64 = lines.next()?.strip_prefix(CACHE_MAGIC)?.trim().parse().ok()?;
    // Path of open nodes by level; each finished node is attached to its parent.
    let mut stack: Vec<Node> = vec![];
    for line in lines {
        let mut it = line.splitn(3, '\t');
        let level: usize = it.next()?.parse().ok()?;
        let size: u64 = it.next()?.parse().ok()?;
        let name = it.next()?.to_string();
        if level == 0 && !stack.is_empty() || level > stack.len() || level > LEVELS {
            return None;
        }
        while stack.len() > level {
            let done = stack.pop()?;
            stack.last_mut()?.children.push(done);
        }
        stack.push(Node { name, size, children: vec![] });
    }
    while stack.len() > 1 {
        let done = stack.pop()?;
        stack.last_mut()?.children.push(done);
    }
    stack.pop().map(|n| (at, n))
}

// ---------------------------------------------------------------------------
// Drawing
// ---------------------------------------------------------------------------

/// "755 GB", "1.06 TB", "512 MB" (decimal units, as disks are sold and `df -H`).
pub fn size_text(b: u64) -> String {
    let b = b as f64;
    if b >= 1e12 {
        format!("{:.2} TB", b / 1e12)
    } else if b >= 1e10 {
        format!("{:.0} GB", b / 1e9)
    } else if b >= 1e9 {
        format!("{:.1} GB", b / 1e9)
    } else {
        format!("{:.0} MB", b / 1e6)
    }
}

fn short_name(s: &str) -> String {
    if s.chars().count() <= 11 { s.to_string() } else { format!("{}..", s.chars().take(9).collect::<String>()) }
}

struct Ring<'a> {
    out: &'a mut Vec<SpriteVertex>,
    c: [f32; 2],
    ui: f32,
    w: f32,
    h: f32,
}

impl Ring<'_> {
    /// Point at radius `r`, angle `a` (0 = up, clockwise).
    fn at(&self, r: f32, a: f32) -> [f32; 2] {
        [self.c[0] + r * a.sin(), self.c[1] - r * a.cos()]
    }

    /// A ring segment filled with `fill`, its outer edge drawn in `edge`.
    fn band(&mut self, r0: f32, r1: f32, a0: f32, a1: f32, fill: [f32; 4], edge: [f32; 4]) {
        // A hair of space between neighbours, constant in pixels.
        let pad = 0.7 * self.ui / r1;
        let (a0, a1) = (a0 + pad, a1 - pad);
        if (a1 - a0) * r1 < 0.6 * self.ui {
            return;
        }
        let steps = ((a1 - a0) / (TAU / 90.0)).ceil().max(1.0) as usize;
        for i in 0..steps {
            let (t0, t1) = (a0 + (a1 - a0) * i as f32 / steps as f32, a0 + (a1 - a0) * (i + 1) as f32 / steps as f32);
            let p = [self.at(r0, t0), self.at(r1, t0), self.at(r1, t1), self.at(r0, t1)];
            quad_px(self.out, p, fill, self.w, self.h);
            if edge[3] > 0.0 {
                line_px(self.out, self.at(r1, t0), self.at(r1, t1), 1.0 * self.ui, edge, self.w, self.h);
            }
        }
    }
}

struct Label {
    name: String,
    size: u64,
    angle: f32,
}

#[allow(clippy::too_many_arguments)]
fn draw(
    home: Option<&Node>,
    fs: Option<FsStats>,
    cfg: &Config,
    atlas: &GlyphAtlas,
    w: f32,
    h: f32,
    bottom: f32,
    names: bool,
    out: &mut Vec<SpriteVertex>,
) {
    let ui = w.min(h) / 1080.0;
    let face = &atlas.mono;
    let gold = cfg.color(&cfg.age.fresh);
    let tones = [gold, cfg.color("orange")];
    let alert = cfg.color(&cfg.events.write.color);
    let grey = [0.5, 0.5, 0.5];
    let col = |c: [f32; 3], a: f32| [c[0], c[1], c[2], a];
    let right = w - crate::sysmon::MARGIN_R * ui;
    let left = right - crate::sysmon::WIDTH * ui;
    let top = bottom - PANEL_H * ui;
    let full = fs.is_some_and(|f| f.total > 0 && f.used as f64 / f.total as f64 > 0.9);

    // Header, the totals, a thin rule between them.
    let head_y = top + 10.0 * ui;
    let hs = 9.5 * ui;
    text_px(out, face, "DISK", left, head_y, hs, col(gold, 0.45), w, h);
    let summary = match (fs, home) {
        (Some(f), Some(n)) => format!("~ {}  ·  {} free", size_text(n.size), size_text(f.avail)),
        (Some(f), None) => format!("{} free", size_text(f.avail)),
        _ => String::new(),
    };
    let ss = 9.5 * ui;
    let sw = face.measure(&summary, ss);
    text_px(out, face, &summary, right - sw, head_y, ss, col(if full { alert } else { gold }, 0.6), w, h);
    let rule_x = left + face.measure("DISK", hs) + 8.0 * ui;
    line_px(out, [rule_x, head_y - 3.0 * ui], [right - sw - 8.0 * ui, head_y - 3.0 * ui], 1.0 * ui, col(gold, 0.18), w, h);

    let center = [left + crate::sysmon::WIDTH * 0.5 * ui, top + (22.0 + RINGS[LEVELS - 1].1 + 4.0) * ui];
    let mut ring = Ring { out, c: center, ui, w, h };

    // Faint tracks so the rings read as a structure even where they are empty.
    for &(r0, r1) in std::iter::once(&GAUGE).chain(RINGS.iter()) {
        ring.band(r0 * ui, r1 * ui, 0.0, TAU, col(gold, 0.025), col(gold, 0.06));
    }

    // Gauge: the whole disk. Home, then the rest of what is used; free is the empty track.
    if let Some(f) = fs.filter(|f| f.total > 0) {
        let used_c = if full { alert } else { gold };
        let home_b = home.map_or(0, |n| n.size).min(f.used);
        let a_home = TAU * home_b as f32 / f.total as f32;
        let a_used = TAU * f.used as f32 / f.total as f32;
        ring.band(GAUGE.0 * ui, GAUGE.1 * ui, 0.0, a_home, col(used_c, 0.42), col(used_c, 0.8));
        ring.band(GAUGE.0 * ui, GAUGE.1 * ui, a_home, a_used, col(used_c, 0.14), col(used_c, 0.35));
        ring.band(GAUGE.0 * ui, GAUGE.1 * ui, a_used, TAU, col(grey, 0.04), col(grey, 0.16));
        let pct = format!("{:.0}%", 100.0 * f.used as f64 / f.total as f64);
        let ps = 12.0 * ui;
        text_px(ring.out, face, &pct, center[0] - face.measure(&pct, ps) * 0.5, center[1] + 2.0 * ui, ps, col(used_c, 0.85), w, h);
        let us = 6.5 * ui;
        text_px(ring.out, face, "USED", center[0] - face.measure("USED", us) * 0.5, center[1] + 10.0 * ui, us, col(gold, 0.35), w, h);
    }

    let Some(home) = home.filter(|n| n.size > 0) else {
        let msg = "MEASURING";
        let ms = 8.0 * ui;
        text_px(
            ring.out,
            face,
            msg,
            center[0] - face.measure(msg, ms) * 0.5,
            center[1] + (RINGS[0].1 + 12.0) * ui,
            ms,
            col(gold, 0.35),
            w,
            h,
        );
        return;
    };

    // Folder rings: each folder's span splits among its children by size;
    // what is left over (files, small folders) stays an empty gap.
    let mut labels = vec![];
    let mut a = 0.0;
    for (i, c) in home.children.iter().enumerate() {
        let span = TAU * c.size as f32 / home.size as f32;
        let tone = tones[i % tones.len()];
        draw_node(&mut ring, c, 0, a, a + span, tone);
        if labels.len() < MAX_LABELS && c.size as f64 >= LABEL_SHARE * home.size as f64 {
            labels.push(Label { name: c.name.clone(), size: c.size, angle: a + span * 0.5 });
        }
        a += span;
    }
    draw_labels(&mut ring, labels, face, gold, names, top + 20.0 * ui, bottom);
}

fn draw_node(ring: &mut Ring, node: &Node, level: usize, a0: f32, a1: f32, tone: [f32; 3]) {
    let (r0, r1) = RINGS[level];
    let fill = [0.11, 0.075, 0.05][level];
    let edge = [0.85, 0.6, 0.42][level];
    let ui = ring.ui;
    ring.band(r0 * ui, r1 * ui, a0, a1, [tone[0], tone[1], tone[2], fill], [tone[0], tone[1], tone[2], edge]);
    if level + 1 >= LEVELS || node.size == 0 {
        return;
    }
    let mut a = a0;
    for c in &node.children {
        let span = (a1 - a0) * c.size as f32 / node.size as f32;
        draw_node(ring, c, level + 1, a, a + span, tone);
        a += span;
    }
}

/// Labels beside the chart, on the side their folder is on, joined to it by a
/// leader line like the tower's callouts.
fn draw_labels(ring: &mut Ring, labels: Vec<Label>, face: &crate::text::Face, gold: [f32; 3], names: bool, y_min: f32, y_max: f32) {
    let ui = ring.ui;
    let col = |a: f32| [gold[0], gold[1], gold[2], a];
    let r_out = RINGS[LEVELS - 1].1 * ui;
    for side in [1.0f32, -1.0] {
        let mut side_labels: Vec<(f32, &Label)> =
            labels.iter().filter(|l| (l.angle.sin() >= 0.0) == (side > 0.0)).map(|l| (ring.at(r_out + 8.0 * ui, l.angle)[1], l)).collect();
        side_labels.sort_by(|a, b| a.0.total_cmp(&b.0));
        // Keep them a line apart, inside the panel.
        let pitch = LABEL_PITCH * ui;
        let lo = y_min + 6.0 * ui;
        let hi = y_max - 12.0 * ui;
        for i in 0..side_labels.len() {
            let min = if i == 0 { lo } else { side_labels[i - 1].0 + pitch };
            side_labels[i].0 = side_labels[i].0.max(min);
        }
        for i in (0..side_labels.len()).rev() {
            let max = if i + 1 == side_labels.len() { hi } else { side_labels[i + 1].0 - pitch };
            side_labels[i].0 = side_labels[i].0.min(max);
        }
        let x_col = ring.c[0] + side * (r_out + 16.0 * ui);
        for (y, l) in side_labels {
            let anchor = ring.at((RINGS[0].1 + 1.0) * ui, l.angle);
            let exit = ring.at(r_out + 3.0 * ui, l.angle);
            let elbow = [x_col - side * 4.0 * ui, y];
            line_px(ring.out, anchor, exit, 0.8 * ui, col(0.35), ring.w, ring.h);
            line_px(ring.out, exit, elbow, 0.8 * ui, col(0.35), ring.w, ring.h);
            quad_px(
                ring.out,
                [
                    [anchor[0] - 1.2 * ui, anchor[1] - 1.2 * ui],
                    [anchor[0] + 1.2 * ui, anchor[1] - 1.2 * ui],
                    [anchor[0] + 1.2 * ui, anchor[1] + 1.2 * ui],
                    [anchor[0] - 1.2 * ui, anchor[1] + 1.2 * ui],
                ],
                col(0.8),
                ring.w,
                ring.h,
            );
            let (ns, zs) = (9.5 * ui, 8.0 * ui);
            let name = if names { short_name(&l.name) } else { String::new() };
            let size = size_text(l.size);
            let (name_y, size_y) = if names { (y + 1.0 * ui, y + 10.0 * ui) } else { (y, y) };
            let place = |s: &str, sz: f32| if side > 0.0 { x_col } else { x_col - face.measure(s, sz) };
            if names {
                text_px(ring.out, face, &name, place(&name, ns), name_y, ns, col(0.75), ring.w, ring.h);
            }
            text_px(ring.out, face, &size, place(&size, zs), size_y + if names { 0.0 } else { 3.0 * ui }, zs, col(0.45), ring.w, ring.h);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn n(name: &str, size: u64, children: Vec<Node>) -> Node {
        Node { name: name.into(), size, children }
    }

    #[test]
    fn cache_round_trips() {
        let tree = n("~", 1000, vec![n("Downloads", 600, vec![n("iso", 500, vec![n("deep", 400, vec![])])]), n("tab\tname", 300, vec![])]);
        let mut text = format!("{CACHE_MAGIC} 42\n");
        serialize(&tree, 0, &mut text);
        let (at, back) = parse_cache(&text).unwrap();
        assert_eq!(at, 42);
        assert_eq!(back.children[0].children[0].children[0].name, "deep");
        assert_eq!(back.children[1].name, "tab name");
        assert_eq!(back.size, 1000);
        assert!(parse_cache("something else\n0\t1\t~\n").is_none());
        assert!(parse_cache(&format!("{CACHE_MAGIC} 1\n0\t1\t~\n2\t1\tskipped a level\n")).is_none());
    }

    #[test]
    fn prune_sorts_and_drops_slivers() {
        let mut tree = n("~", 10_000, vec![n("small", 10, vec![]), n("b", 3000, vec![]), n("a", 6000, vec![])]);
        prune(&mut tree, 10_000);
        assert_eq!(tree.children.iter().map(|c| c.name.as_str()).collect::<Vec<_>>(), ["a", "b"]);
    }

    #[test]
    fn measures_like_du() {
        let dir = std::env::temp_dir().join(format!("gits-disk-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("big/inner")).unwrap();
        std::fs::write(dir.join("big/inner/f"), vec![1u8; 200_000]).unwrap();
        std::fs::hard_link(dir.join("big/inner/f"), dir.join("big/link")).unwrap();
        std::os::unix::fs::symlink("/usr", dir.join("not-followed")).unwrap();
        let node = measure(&dir);
        let big = node.children.iter().find(|c| c.name == "big").unwrap();
        // The file counts once despite the hard link, and the symlink adds nothing.
        assert!(big.size >= 200_000 && big.size < 400_000, "{}", big.size);
        assert!(node.size < 500_000);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn sizes_read_well() {
        assert_eq!(size_text(754_700_000_000), "755 GB");
        assert_eq!(size_text(1_965_000_000_000), "1.97 TB");
        assert_eq!(size_text(3_400_000_000), "3.4 GB");
        assert_eq!(short_name("RobotVacuumSample"), "RobotVacu..");
    }
}
