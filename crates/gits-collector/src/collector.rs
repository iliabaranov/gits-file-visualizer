//! Filesystem state: scanning, inotify watches within a budget, a crawl for the
//! unwatched part, and translation of raw events into protocol operations.
//!
//! Invariant: every change to `model` goes through [`Collector::emit`], which
//! applies it and queues the same operation for clients, so ids on both sides
//! stay identical. The only exceptions are id-neutral purges (removing already
//! deleted entries from listings).

use crate::exclude::Excludes;
use crate::inotify::{Event, Inotify};
use gits_core::model::{DirId, FileId, Model};
use gits_core::protocol::Op;
use std::collections::{HashMap, HashSet, VecDeque};
use std::ffi::OsString;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::time::Instant;

/// Directories larger than this get a name → file index for fast lookups.
const BIG_DIR: usize = 256;
/// Dirs re-checked per crawl slice (one slice per main-loop tick).
const CRAWL_SLICE: usize = 150;
/// A file re-flashes as read at most this often.
const READ_INTERVAL: f64 = 0.5;

pub struct Stats {
    pub dirs: usize,
    pub files: usize,
    pub watched: usize,
    pub budget: usize,
}

pub struct Collector {
    pub model: Model,
    excludes: Excludes,
    root_dev: u64,
    inotify: Inotify,
    budget: usize,
    start: Instant,

    // Per directory id.
    paths: Vec<PathBuf>,
    dir_mtime: Vec<i64>,
    wd_of: Vec<i32>,
    last_activity: Vec<f64>,
    wd_dir: HashMap<i32, DirId>,
    n_watched: usize,
    big: HashMap<DirId, HashMap<String, FileId>>,

    // Current batch.
    ops: Vec<Op>,
    pending_creates: HashMap<(DirId, String), OsString>,
    pending_reads: HashSet<FileId>,
    pending_writes: HashSet<FileId>,
    last_read: HashMap<FileId, f64>,

    // Crawl of unwatched directories.
    crawl_queue: VecDeque<DirId>,
    next_crawl: f64,
    crawl_interval: f64,
    overflow: bool,
    warned_limit: bool,
}

impl Collector {
    pub fn new(root: &Path, root_name: &str, excludes: Excludes, budget: usize, crawl_interval: f64) -> std::io::Result<Self> {
        let md = std::fs::metadata(root)?;
        let epoch = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0);
        Ok(Self {
            model: Model::new(root_name, epoch),
            excludes,
            root_dev: md.dev(),
            inotify: Inotify::new()?,
            budget,
            start: Instant::now(),
            paths: vec![root.to_path_buf()],
            dir_mtime: vec![md.mtime()],
            wd_of: vec![-1],
            last_activity: vec![0.0],
            wd_dir: HashMap::new(),
            n_watched: 0,
            big: HashMap::new(),
            ops: vec![],
            pending_creates: HashMap::new(),
            pending_reads: HashSet::new(),
            pending_writes: HashSet::new(),
            last_read: HashMap::new(),
            crawl_queue: VecDeque::new(),
            next_crawl: crawl_interval,
            crawl_interval: crawl_interval.max(10.0),
            overflow: false,
            warned_limit: false,
        })
    }

    pub fn now(&self) -> f64 {
        self.start.elapsed().as_secs_f64()
    }

    pub fn inotify(&mut self) -> &mut Inotify {
        &mut self.inotify
    }

    pub fn stats(&self) -> Stats {
        Stats { dirs: self.model.live_dir_count(), files: self.model.live_file_count(), watched: self.n_watched, budget: self.budget }
    }

    // --- model mutation -----------------------------------------------------

    fn emit(&mut self, op: Op) {
        let t = self.now();
        if !self.model.apply(&op, t) {
            log::warn!("dropping inconsistent op {op:?}");
            return;
        }
        match &op {
            Op::MkDir { .. } => {}
            Op::Create { dir, name, .. } => {
                let f = self.model.files.len() as FileId - 1;
                if let Some(idx) = self.big.get_mut(dir) {
                    idx.insert(name.clone(), f);
                } else if self.model.dirs[*dir as usize].files.len() > BIG_DIR {
                    self.index_dir(*dir);
                }
            }
            Op::Delete { file } => {
                let f = &self.model.files[*file as usize];
                let (d, name) = (f.dir, f.name.clone());
                if let Some(idx) = self.big.get_mut(&d) {
                    idx.remove(&name);
                }
                self.model.purge_file(*file);
                self.pending_reads.remove(file);
                self.pending_writes.remove(file);
            }
            Op::RmDir { dir } => {
                self.unwatch_subtree(*dir);
                self.model.purge_dir(*dir);
            }
            Op::Read { .. } | Op::Write { .. } => {}
        }
        self.ops.push(op);
    }

    fn mkdir(&mut self, parent: DirId, name: String, path: PathBuf, mtime: i64, emit: bool) -> DirId {
        if emit {
            self.emit(Op::MkDir { parent, name });
        } else {
            self.model.add_dir(parent, name);
        }
        let id = self.model.dirs.len() as DirId - 1;
        self.paths.push(path);
        self.dir_mtime.push(mtime);
        self.wd_of.push(-1);
        self.last_activity.push(if emit { self.now() } else { 0.0 });
        id
    }

    fn index_dir(&mut self, d: DirId) {
        let idx = self.model.dirs[d as usize].files.iter().map(|&f| (self.model.files[f as usize].name.clone(), f)).collect();
        self.big.insert(d, idx);
    }

    fn lookup_file(&self, d: DirId, name: &str) -> Option<FileId> {
        match self.big.get(&d) {
            Some(idx) => idx.get(name).copied().filter(|&f| self.model.is_live(f)),
            None => self.model.find_file(d, name),
        }
    }

    fn file_path(&self, f: FileId) -> PathBuf {
        let file = &self.model.files[f as usize];
        self.paths[file.dir as usize].join(&file.name)
    }

    // --- scanning -----------------------------------------------------------

    /// Scans the tree below `start`. With `emit`, new entries are sent to clients
    /// as creations; otherwise they are added silently (initial scan).
    pub fn scan(&mut self, start: DirId, emit: bool) {
        let mut stack = vec![start];
        while let Some(d) = stack.pop() {
            let path = self.paths[d as usize].clone();
            let Ok(rd) = std::fs::read_dir(&path) else { continue };
            if let Ok(md) = std::fs::symlink_metadata(&path) {
                self.dir_mtime[d as usize] = md.mtime();
            }
            for entry in rd.flatten() {
                let os_name = entry.file_name();
                let name = os_name.to_string_lossy().into_owned();
                let p = path.join(&os_name);
                if self.excludes.is_excluded(&p) {
                    continue;
                }
                let Ok(ft) = entry.file_type() else { continue };
                if ft.is_dir() {
                    let Ok(md) = std::fs::symlink_metadata(&p) else { continue };
                    if md.dev() != self.root_dev || (emit && self.model.find_dir(d, &name).is_some()) {
                        continue;
                    }
                    let child = self.mkdir(d, name, p, md.mtime(), emit);
                    stack.push(child);
                } else if ft.is_file() || ft.is_symlink() {
                    if emit && self.lookup_file(d, &name).is_some() {
                        continue;
                    }
                    let Ok(md) = entry.metadata() else { continue };
                    if emit {
                        self.emit(Op::Create { dir: d, name, size: md.size(), mtime: md.mtime(), symlink: ft.is_symlink() });
                    } else {
                        let f = self.model.add_existing_file(d, name, md.size(), md.mtime());
                        self.model.files[f as usize].symlink = ft.is_symlink();
                    }
                }
            }
        }
    }

    /// Initial scan of the whole tree, then chooses which directories to watch.
    pub fn initial_scan(&mut self) {
        self.scan(self.model.root, false);
        self.model.sort_all();
        for d in 0..self.model.dirs.len() {
            if self.model.dirs[d].files.len() > BIG_DIR {
                self.index_dir(d as DirId);
            }
        }
        self.model.drain_events();
        self.choose_watches();
    }

    // --- watches ------------------------------------------------------------

    fn choose_watches(&mut self) {
        let mut order: Vec<DirId> = (0..self.model.dirs.len() as DirId).filter(|&d| !self.model.dirs[d as usize].removed).collect();
        // Top levels first, then the most recently modified directories.
        order.sort_by_key(|&d| (self.model.dirs[d as usize].depth > 1, std::cmp::Reverse(self.dir_mtime[d as usize])));
        for &d in &order {
            if self.n_watched >= self.budget {
                break;
            }
            self.watch(d);
        }
        self.crawl_queue = order.into_iter().filter(|&d| self.wd_of[d as usize] < 0).collect();
        log::info!("watching {} directories live (budget {}); {} checked by crawl", self.n_watched, self.budget, self.crawl_queue.len());
    }

    fn watch(&mut self, d: DirId) -> bool {
        if self.wd_of[d as usize] >= 0 {
            return true;
        }
        if self.n_watched >= self.budget {
            return false;
        }
        match self.inotify.add(&self.paths[d as usize]) {
            Ok(wd) => {
                if let Some(old) = self.wd_dir.insert(wd, d)
                    && old != d
                {
                    self.wd_of[old as usize] = -1;
                    self.n_watched -= 1;
                }
                self.wd_of[d as usize] = wd;
                self.n_watched += 1;
                true
            }
            Err(e) => {
                if e.raw_os_error() == Some(libc::ENOSPC) {
                    if !self.warned_limit {
                        log::warn!("system inotify watch limit reached at {} watches; budget lowered", self.n_watched);
                        self.warned_limit = true;
                    }
                    self.budget = self.n_watched;
                }
                false
            }
        }
    }

    fn unwatch(&mut self, d: DirId) {
        let wd = self.wd_of[d as usize];
        if wd >= 0 {
            self.inotify.remove(wd);
            self.wd_dir.remove(&wd);
            self.wd_of[d as usize] = -1;
            self.n_watched -= 1;
        }
    }

    fn unwatch_subtree(&mut self, d: DirId) {
        let mut stack = vec![d];
        while let Some(d) = stack.pop() {
            self.unwatch(d);
            stack.extend_from_slice(&self.model.dirs[d as usize].dirs);
        }
    }

    /// Watches `d`, evicting the least recently active watched directory if needed.
    fn promote(&mut self, d: DirId) {
        if self.watch(d) {
            return;
        }
        let now = self.now();
        let victim = self
            .wd_dir
            .values()
            .copied()
            .filter(|&v| self.model.dirs[v as usize].depth > 1 && v != d)
            .take(256)
            .min_by(|&a, &b| self.last_activity[a as usize].total_cmp(&self.last_activity[b as usize]));
        if let Some(v) = victim
            && now - self.last_activity[v as usize] > 60.0
        {
            self.unwatch(v);
            self.crawl_queue.push_back(v);
            self.watch(d);
        }
    }

    // --- events -------------------------------------------------------------

    pub fn handle(&mut self, ev: Event) {
        if ev.mask & libc::IN_Q_OVERFLOW != 0 {
            log::warn!("inotify queue overflow; rescanning watched directories");
            self.overflow = true;
            return;
        }
        let Some(&d) = self.wd_dir.get(&ev.wd) else { return };
        if ev.mask & libc::IN_IGNORED != 0 {
            self.wd_dir.remove(&ev.wd);
            if self.wd_of[d as usize] == ev.wd {
                self.wd_of[d as usize] = -1;
                self.n_watched -= 1;
            }
            return;
        }
        if self.model.dirs[d as usize].removed {
            return;
        }
        if ev.mask & libc::IN_MOVE_SELF != 0 && d != self.model.root {
            // Moved away under an unwatched parent: our path is stale. The parent's
            // listing will be reconciled by the crawl.
            self.unwatch(d);
            if let Some(p) = self.model.dirs[d as usize].parent {
                self.crawl_queue.push_front(p);
            }
            return;
        }
        let Some(os_name) = ev.name else { return };
        let name = os_name.to_string_lossy().into_owned();
        let path = self.paths[d as usize].join(&os_name);
        if self.excludes.is_excluded(&path) {
            return;
        }
        self.last_activity[d as usize] = self.now();
        let m = ev.mask;
        if m & libc::IN_ISDIR != 0 {
            if m & (libc::IN_CREATE | libc::IN_MOVED_TO) != 0 && self.model.find_dir(d, &name).is_none() {
                let Ok(md) = std::fs::symlink_metadata(&path) else { return };
                if md.dev() != self.root_dev {
                    return;
                }
                let child = self.mkdir(d, name, path, md.mtime(), true);
                // Watch before scanning so nothing created meanwhile is missed.
                self.promote(child);
                self.scan(child, true);
            } else if m & (libc::IN_DELETE | libc::IN_MOVED_FROM) != 0
                && let Some(child) = self.model.find_dir(d, &name)
            {
                self.emit(Op::RmDir { dir: child });
            }
            return;
        }
        let key = (d, name);
        if m & (libc::IN_CREATE | libc::IN_MOVED_TO) != 0 {
            match self.lookup_file(d, &key.1) {
                Some(f) => {
                    self.pending_writes.insert(f);
                }
                None => {
                    self.pending_creates.insert(key, os_name);
                }
            }
        } else if m & (libc::IN_DELETE | libc::IN_MOVED_FROM) != 0 {
            if self.pending_creates.remove(&key).is_none()
                && let Some(f) = self.lookup_file(d, &key.1)
            {
                self.emit(Op::Delete { file: f });
            }
        } else if m & (libc::IN_MODIFY | libc::IN_CLOSE_WRITE) != 0 {
            if !self.pending_creates.contains_key(&key) {
                match self.lookup_file(d, &key.1) {
                    Some(f) => {
                        self.pending_writes.insert(f);
                    }
                    None => {
                        self.pending_creates.insert(key, os_name);
                    }
                }
            }
        } else if m & (libc::IN_ACCESS | libc::IN_OPEN) != 0
            && !self.pending_creates.contains_key(&key)
            && let Some(f) = self.lookup_file(d, &key.1)
        {
            self.pending_reads.insert(f);
        }
    }

    /// Turns the pending batch into operations. Returns them (already applied).
    pub fn flush(&mut self) -> Vec<Op> {
        let mut creates: Vec<((DirId, String), OsString)> = self.pending_creates.drain().collect();
        creates.sort_by(|a, b| a.0.cmp(&b.0));
        for ((d, name), os_name) in creates {
            if self.model.dirs[d as usize].removed || self.lookup_file(d, &name).is_some() {
                continue;
            }
            let path = self.paths[d as usize].join(&os_name);
            let Ok(md) = std::fs::symlink_metadata(&path) else { continue };
            if md.is_dir() {
                continue;
            }
            self.emit(Op::Create { dir: d, name, size: md.size(), mtime: md.mtime(), symlink: md.file_type().is_symlink() });
        }
        let writes: Vec<FileId> = self.pending_writes.drain().collect();
        for &f in &writes {
            if !self.model.is_live(f) {
                continue;
            }
            if let Ok(md) = std::fs::symlink_metadata(self.file_path(f)) {
                self.emit(Op::Write { file: f, size: md.size(), mtime: md.mtime() });
            }
        }
        let now = self.now();
        let reads: Vec<FileId> = self.pending_reads.drain().collect();
        for f in reads {
            if !self.model.is_live(f) || writes.contains(&f) {
                continue;
            }
            let last = self.last_read.get(&f).copied().unwrap_or(-1e9);
            if now - last >= READ_INTERVAL {
                self.last_read.insert(f, now);
                self.emit(Op::Read { file: f });
            }
        }
        if self.last_read.len() > 100_000 {
            self.last_read.retain(|_, t| now - *t < READ_INTERVAL);
        }
        self.model.drain_events();
        std::mem::take(&mut self.ops)
    }

    // --- crawl --------------------------------------------------------------

    /// Re-reads a directory listing and emits whatever changed.
    fn reconcile(&mut self, d: DirId) {
        let path = self.paths[d as usize].clone();
        let Ok(rd) = std::fs::read_dir(&path) else {
            if d != self.model.root {
                self.emit(Op::RmDir { dir: d });
            }
            return;
        };
        if let Ok(md) = std::fs::symlink_metadata(&path) {
            self.dir_mtime[d as usize] = md.mtime();
        }
        let mut seen_files = HashSet::new();
        let mut seen_dirs = HashSet::new();
        for entry in rd.flatten() {
            let os_name = entry.file_name();
            let name = os_name.to_string_lossy().into_owned();
            let p = path.join(&os_name);
            if self.excludes.is_excluded(&p) {
                continue;
            }
            let Ok(ft) = entry.file_type() else { continue };
            if ft.is_dir() {
                seen_dirs.insert(name.clone());
                if self.model.find_dir(d, &name).is_none()
                    && let Ok(md) = std::fs::symlink_metadata(&p)
                    && md.dev() == self.root_dev
                {
                    let child = self.mkdir(d, name, p, md.mtime(), true);
                    self.scan(child, true);
                }
            } else if ft.is_file() || ft.is_symlink() {
                seen_files.insert(name.clone());
                if self.lookup_file(d, &name).is_none()
                    && let Ok(md) = entry.metadata()
                {
                    self.emit(Op::Create { dir: d, name, size: md.size(), mtime: md.mtime(), symlink: ft.is_symlink() });
                }
            }
        }
        let gone_files: Vec<FileId> = self.model.dirs[d as usize]
            .files
            .iter()
            .copied()
            .filter(|&f| !seen_files.contains(&self.model.files[f as usize].name))
            .collect();
        for f in gone_files {
            self.emit(Op::Delete { file: f });
        }
        let gone_dirs: Vec<DirId> = self.model.dirs[d as usize]
            .dirs
            .iter()
            .copied()
            .filter(|&c| !self.model.dirs[c as usize].removed && !seen_dirs.contains(&self.model.dirs[c as usize].name))
            .collect();
        for c in gone_dirs {
            self.emit(Op::RmDir { dir: c });
        }
    }

    /// One time slice of the background crawl over unwatched directories.
    pub fn crawl_tick(&mut self) {
        if self.overflow {
            self.overflow = false;
            let watched: Vec<DirId> = self.wd_dir.values().copied().collect();
            for d in watched {
                self.crawl_queue.push_front(d);
            }
        }
        let now = self.now();
        if self.crawl_queue.is_empty() {
            if now < self.next_crawl {
                return;
            }
            self.crawl_queue = (0..self.model.dirs.len() as DirId)
                .filter(|&d| !self.model.dirs[d as usize].removed && self.wd_of[d as usize] < 0)
                .collect();
            self.next_crawl = now + self.crawl_interval;
        }
        for _ in 0..CRAWL_SLICE {
            let Some(d) = self.crawl_queue.pop_front() else { break };
            if self.model.dirs[d as usize].removed {
                continue;
            }
            let Ok(md) = std::fs::symlink_metadata(&self.paths[d as usize]) else {
                if d != self.model.root {
                    self.emit(Op::RmDir { dir: d });
                }
                continue;
            };
            if md.mtime() != self.dir_mtime[d as usize] || self.wd_of[d as usize] >= 0 {
                let before = self.ops.len();
                self.reconcile(d);
                if self.ops.len() > before {
                    self.last_activity[d as usize] = now;
                    self.promote(d);
                }
            }
        }
    }
}
