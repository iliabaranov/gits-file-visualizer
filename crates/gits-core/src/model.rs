//! In-memory filesystem model shared by the synthetic generator (M0) and the
//! real collector (M1). Ids are stable indices: removed entries become
//! tombstones instead of shifting other ids.

pub type DirId = u32;
pub type FileId = u32;

/// Timestamp meaning "never happened".
pub const NEVER: f64 = -1.0e12;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FileKind {
    Source,
    Text,
    Image,
    Video,
    Audio,
    Archive,
    Document,
    Binary,
    Object,
    Config,
    Other,
}

impl FileKind {
    pub fn from_name(name: &str) -> Self {
        let ext = name.rsplit_once('.').map(|(_, e)| e.to_ascii_lowercase()).unwrap_or_default();
        match ext.as_str() {
            "rs" | "c" | "h" | "cpp" | "hpp" | "cc" | "py" | "js" | "ts" | "go" | "java" | "wgsl" | "glsl" | "sh" | "cmake" | "ino" => {
                Self::Source
            }
            "md" | "txt" | "rst" | "csv" | "log" => Self::Text,
            "png" | "jpg" | "jpeg" | "gif" | "webp" | "svg" | "bmp" => Self::Image,
            "mp4" | "mkv" | "webm" | "mov" | "avi" => Self::Video,
            "mp3" | "flac" | "ogg" | "wav" | "m4a" => Self::Audio,
            "zip" | "gz" | "xz" | "zst" | "tar" | "7z" | "deb" | "iso" | "appimage" => Self::Archive,
            "pdf" | "odt" | "docx" | "xlsx" | "ods" | "pptx" => Self::Document,
            "so" | "a" | "bin" | "elf" | "exe" | "rlib" | "rmeta" => Self::Binary,
            "o" | "d" | "obj" | "pack" | "idx" => Self::Object,
            "toml" | "json" | "yaml" | "yml" | "ini" | "conf" | "cfg" | "xml" | "lock" => Self::Config,
            _ => Self::Other,
        }
    }
}

#[derive(Clone, Debug)]
pub struct Dir {
    pub name: String,
    pub parent: Option<DirId>,
    pub depth: u16,
    pub dirs: Vec<DirId>,
    pub files: Vec<FileId>,
}

#[derive(Clone, Debug)]
pub struct File {
    pub name: String,
    pub dir: DirId,
    pub size: u64,
    /// Modification time, unix seconds.
    pub mtime: i64,
    pub kind: FileKind,
    pub hidden: bool,
    pub symlink: bool,
    /// Event times in model seconds ([`NEVER`] if not seen).
    pub t_read: f64,
    pub t_write: f64,
    pub t_create: f64,
    pub t_delete: f64,
    /// Removed from its directory listing (after its deletion ghost expired).
    pub purged: bool,
    /// Arc length in turns, frozen at first layout so positions stay stable.
    pub arc: f32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EventKind {
    Read,
    Write,
    Create,
    Delete,
}

#[derive(Clone, Copy, Debug)]
pub struct Event {
    pub kind: EventKind,
    pub file: FileId,
    pub t: f64,
}

#[derive(Clone, Debug)]
pub struct Model {
    pub dirs: Vec<Dir>,
    pub files: Vec<File>,
    pub root: DirId,
    /// Wall-clock unix time corresponding to model time 0.
    pub epoch_unix: i64,
    /// Bumped on every change that affects layout (create, purge).
    pub structure_gen: u64,
    events: Vec<Event>,
}

impl Model {
    pub fn new(root_name: &str, epoch_unix: i64) -> Self {
        Self {
            dirs: vec![Dir { name: root_name.into(), parent: None, depth: 0, dirs: vec![], files: vec![] }],
            files: vec![],
            root: 0,
            epoch_unix,
            structure_gen: 0,
            events: vec![],
        }
    }

    pub fn add_dir(&mut self, parent: DirId, name: impl Into<String>) -> DirId {
        let id = self.dirs.len() as DirId;
        let depth = self.dirs[parent as usize].depth + 1;
        self.dirs.push(Dir { name: name.into(), parent: Some(parent), depth, dirs: vec![], files: vec![] });
        self.dirs[parent as usize].dirs.push(id);
        self.structure_gen += 1;
        id
    }

    /// Adds a file that already existed before observation started (no event).
    pub fn add_existing_file(&mut self, dir: DirId, name: impl Into<String>, size: u64, mtime: i64) -> FileId {
        let name = name.into();
        let id = self.files.len() as FileId;
        self.files.push(File {
            kind: FileKind::from_name(&name),
            hidden: name.starts_with('.'),
            symlink: false,
            name,
            dir,
            size,
            mtime,
            t_read: NEVER,
            t_write: NEVER,
            t_create: NEVER,
            t_delete: NEVER,
            purged: false,
            arc: 0.0,
        });
        self.dirs[dir as usize].files.push(id);
        self.structure_gen += 1;
        id
    }

    /// Sorts every directory's children by name (dotfiles first), like a listing.
    pub fn sort_all(&mut self) {
        let dir_names: Vec<String> = self.dirs.iter().map(|d| d.name.to_lowercase()).collect();
        let file_names: Vec<String> = self.files.iter().map(|f| f.name.to_lowercase()).collect();
        for d in &mut self.dirs {
            d.dirs.sort_by(|a, b| dir_names[*a as usize].cmp(&dir_names[*b as usize]));
            d.files.sort_by(|a, b| file_names[*a as usize].cmp(&file_names[*b as usize]));
        }
    }

    pub fn is_live(&self, f: FileId) -> bool {
        let f = &self.files[f as usize];
        f.t_delete == NEVER && !f.purged
    }

    pub fn read(&mut self, f: FileId, t: f64) {
        if !self.is_live(f) {
            return;
        }
        self.files[f as usize].t_read = t;
        self.events.push(Event { kind: EventKind::Read, file: f, t });
    }

    pub fn write(&mut self, f: FileId, t: f64, new_size: Option<u64>) {
        if !self.is_live(f) {
            return;
        }
        let epoch = self.epoch_unix;
        let file = &mut self.files[f as usize];
        file.t_write = t;
        file.mtime = epoch + t as i64;
        if let Some(s) = new_size {
            file.size = s;
        }
        self.events.push(Event { kind: EventKind::Write, file: f, t });
    }

    pub fn create(&mut self, dir: DirId, name: impl Into<String>, size: u64, t: f64) -> FileId {
        let epoch = self.epoch_unix;
        let id = self.add_existing_file(dir, name, size, epoch + t as i64);
        let file = &mut self.files[id as usize];
        file.t_create = t;
        file.t_write = t;
        self.events.push(Event { kind: EventKind::Create, file: id, t });
        id
    }

    pub fn delete(&mut self, f: FileId, t: f64) {
        if !self.is_live(f) {
            return;
        }
        self.files[f as usize].t_delete = t;
        self.events.push(Event { kind: EventKind::Delete, file: f, t });
    }

    /// Drops deleted files whose ghost has expired from their directory listing.
    pub fn purge_deleted(&mut self, deleted_before: f64) -> usize {
        let mut n = 0;
        for d in 0..self.dirs.len() {
            let files = std::mem::take(&mut self.dirs[d].files);
            let (keep, drop): (Vec<FileId>, Vec<FileId>) = files.into_iter().partition(|&f| {
                let t = self.files[f as usize].t_delete;
                t == NEVER || t >= deleted_before
            });
            for f in &drop {
                self.files[*f as usize].purged = true;
            }
            n += drop.len();
            self.dirs[d].files = keep;
        }
        if n > 0 {
            self.structure_gen += 1;
        }
        n
    }

    pub fn drain_events(&mut self) -> Vec<Event> {
        std::mem::take(&mut self.events)
    }

    pub fn dir_path(&self, mut d: DirId) -> String {
        let mut parts = vec![];
        loop {
            let dir = &self.dirs[d as usize];
            parts.push(dir.name.as_str());
            match dir.parent {
                Some(p) => d = p,
                None => break,
            }
        }
        parts.reverse();
        parts.join("/")
    }

    pub fn file_path(&self, f: FileId) -> String {
        let file = &self.files[f as usize];
        format!("{}/{}", self.dir_path(file.dir), file.name)
    }

    pub fn live_file_count(&self) -> usize {
        self.files.iter().filter(|f| !f.purged).count()
    }

    pub fn max_depth(&self) -> u16 {
        self.dirs.iter().map(|d| d.depth).max().unwrap_or(0)
    }
}
