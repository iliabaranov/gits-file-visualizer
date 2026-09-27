//! Synthetic home directory and a scripted "work session" of filesystem events,
//! used for look-dev (M0) and deterministic snapshots. Nothing here touches the
//! real filesystem.

use crate::model::{DirId, FileId, Model};
use crate::rng::Rng;

const DAY: f64 = 86400.0;

const WORDS: &[&str] = &[
    "motor",
    "driver",
    "config",
    "sensor",
    "bridge",
    "state",
    "queue",
    "buffer",
    "parser",
    "render",
    "stop",
    "safety",
    "relay",
    "frame",
    "packet",
    "node",
    "filter",
    "control",
    "power",
    "monitor",
    "camera",
    "lidar",
    "imu",
    "gpio",
    "uart",
    "spi",
    "display",
    "eink",
    "panel",
    "widget",
    "utils",
    "common",
    "types",
    "error",
    "main",
    "server",
    "client",
    "proto",
    "message",
    "timer",
    "clock",
    "task",
    "worker",
    "pool",
    "cache",
    "store",
    "index",
    "layout",
    "shader",
    "mesh",
    "tower",
    "ring",
    "segment",
    "event",
    "watch",
    "scan",
    "tree",
    "path",
    "log",
    "trace",
    "metrics",
    "bench",
    "mock",
    "schema",
    "model",
    "view",
    "api",
    "cli",
    "args",
    "estop",
    "heartbeat",
    "watchdog",
    "can",
    "dbc",
    "firmware",
    "boot",
    "flash",
    "ota",
    "wifi",
    "mqtt",
    "telemetry",
    "battery",
    "charger",
    "pid",
];

const DOC_WORDS: &[&str] = &[
    "Report",
    "Invoice",
    "Memo",
    "Notes",
    "Summary",
    "Proposal",
    "Budget",
    "Manual",
    "Datasheet",
    "Schedule",
    "Review",
    "Plan",
    "Minutes",
    "Contract",
    "Receipt",
    "Statement",
    "Spec",
    "Draft",
];

pub struct Project {
    pub name: String,
    pub root: DirId,
    pub src: Vec<DirId>,
    pub build: Vec<DirId>,
    pub git_objects: Vec<DirId>,
    pub git: DirId,
    pub ext: &'static str,
    pub weight: f64,
}

pub struct Index {
    pub projects: Vec<Project>,
    pub pictures: Vec<DirId>,
    pub downloads: DirId,
    pub documents: Vec<DirId>,
    pub config: Vec<DirId>,
    pub cache: Vec<DirId>,
    pub trash: DirId,
}

pub struct SynthHome {
    pub model: Model,
    pub index: Index,
}

struct Gen {
    m: Model,
    rng: Rng,
    scale: f64,
}

impl Gen {
    fn n(&mut self, base: f64) -> usize {
        (base * self.scale * self.rng.range(0.6, 1.4)).round().max(1.0) as usize
    }

    fn mtime(&mut self, age_days: f64) -> i64 {
        self.m.epoch_unix - (age_days.max(0.0) * DAY) as i64
    }

    fn file(&mut self, dir: DirId, name: String, size: f64, age_days: f64) -> FileId {
        let mt = self.mtime(age_days);
        self.m.add_existing_file(dir, name, size.max(0.0) as u64, mt)
    }

    fn word(&mut self) -> &'static str {
        self.rng.pick(WORDS)
    }

    fn src_name(&mut self, ext: &str) -> String {
        if self.rng.chance(0.4) { format!("{}_{}.{ext}", self.word(), self.word()) } else { format!("{}.{ext}", self.word()) }
    }

    fn hex(&mut self, n: usize) -> String {
        (0..n).map(|_| char::from_digit(self.rng.below(16) as u32, 16).unwrap()).collect()
    }

    fn age(&mut self, median_days: f64, sigma: f64) -> f64 {
        self.rng.lognormal(median_days, sigma)
    }
}

/// Unix seconds → (year, month, day, hour, minute, second), proleptic Gregorian.
pub fn civil(unix: i64) -> (i64, u32, u32, u32, u32, u32) {
    let days = unix.div_euclid(86400);
    let secs = unix.rem_euclid(86400);
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let y = yoe + era * 400 + if m <= 2 { 1 } else { 0 };
    (y, m, d, (secs / 3600) as u32, ((secs / 60) % 60) as u32, (secs % 60) as u32)
}

/// 2026-09-27 00:00 UTC — fixed so snapshots are reproducible.
pub const SYNTH_EPOCH_UNIX: i64 = 1_790_467_200;

pub fn generate_home(seed: u64, scale: f32) -> SynthHome {
    // scale 1.0 ≈ 1.7k directories / 39k files, a mid-sized developer home.
    let mut g = Gen { m: Model::new("~", SYNTH_EPOCH_UNIX), rng: Rng::new(seed), scale: (scale.max(0.02) * 3.0) as f64 };
    let home = g.m.root;

    // Dot directories.
    let cache = g.m.add_dir(home, ".cache");
    let mut cache_dirs = vec![];
    let mesa = g.m.add_dir(cache, "mesa_shader_cache");
    for _ in 0..g.n(48.0) {
        let name = g.hex(2);
        let d = g.m.add_dir(mesa, name);
        for _ in 0..g.n(7.0) {
            let (name, size, age) = (g.hex(38), g.rng.lognormal(30_000.0, 1.0), g.age(20.0, 1.2));
            g.file(d, name, size, age);
        }
        cache_dirs.push(d);
    }
    let thumbs = g.m.add_dir(cache, "thumbnails");
    for sub in ["normal", "large"] {
        let d = g.m.add_dir(thumbs, sub);
        let count = g.n(if sub == "normal" { 320.0 } else { 120.0 });
        for _ in 0..count {
            let (name, size, age) = (format!("{}.png", g.hex(32)), g.rng.lognormal(18_000.0, 0.6), g.age(60.0, 1.3));
            g.file(d, name, size, age);
        }
        cache_dirs.push(d);
    }
    for sub in ["pip", "fontconfig", "tracker3", "gstreamer-1.0", "ubuntu-report"] {
        let d = g.m.add_dir(cache, sub);
        for _ in 0..g.n(10.0) {
            let (name, size, age) = (g.hex(16), g.rng.lognormal(80_000.0, 1.5), g.age(40.0, 1.5));
            g.file(d, name, size, age);
        }
        cache_dirs.push(d);
    }

    let cargo = g.m.add_dir(home, ".cargo");
    let registry = g.m.add_dir(cargo, "registry");
    let reg_src = g.m.add_dir(registry, "src");
    let reg_idx = g.m.add_dir(reg_src, "index.crates.io-1949cf8c6b5b557f");
    for _ in 0..g.n(22.0) {
        let name = format!("{}-{}.{}.{}", g.word(), g.rng.int(0, 3), g.rng.int(0, 40), g.rng.int(0, 12));
        let age = g.age(120.0, 0.8);
        let c = g.m.add_dir(reg_idx, name);
        g.file(c, "Cargo.toml".into(), 2_000.0, age);
        g.file(c, "README.md".into(), 6_000.0, age);
        let src = g.m.add_dir(c, "src");
        for _ in 0..g.n(12.0) {
            let (name, size) = (g.src_name("rs"), g.rng.lognormal(14_000.0, 1.0));
            g.file(src, name, size, age);
        }
        if g.rng.chance(0.4) {
            let w = g.word();
            let sub = g.m.add_dir(src, w);
            for _ in 0..g.n(8.0) {
                let (name, size) = (g.src_name("rs"), g.rng.lognormal(10_000.0, 1.0));
                g.file(sub, name, size, age);
            }
        }
    }

    let config = g.m.add_dir(home, ".config");
    let mut config_dirs = vec![];
    for app in [
        "BambuStudio",
        "Code",
        "FreeCAD",
        "JetBrains",
        "Nextcloud",
        "Slack",
        "atuin",
        "autostart",
        "blender",
        "dconf",
        "discord",
        "evolution",
        "fontconfig",
        "gh",
        "git",
        "gnome-session",
        "google-chrome",
        "gtk-3.0",
        "gtk-4.0",
        "htop",
        "ibus",
        "inkscape",
        "kicad",
        "libreoffice",
        "nautilus",
        "obs-studio",
        "opencode",
        "pulse",
        "systemd",
        "vlc",
    ] {
        let d = g.m.add_dir(config, app);
        let heavy = matches!(app, "Code" | "google-chrome" | "libreoffice" | "JetBrains");
        let count = g.n(if heavy { 24.0 } else { 4.0 });
        for _ in 0..count {
            let ext = *g.rng.pick(&["json", "ini", "conf", "xml", "db", "toml"]);
            let (name, size, age) = (format!("{}.{ext}", g.word()), g.rng.lognormal(3_000.0, 1.4), g.age(90.0, 1.4));
            g.file(d, name, size, age);
        }
        if heavy {
            for sub in ["User", "Default", "Cache"] {
                let s = g.m.add_dir(d, sub);
                for _ in 0..g.n(30.0) {
                    let (name, size, age) = (g.hex(12), g.rng.lognormal(20_000.0, 1.6), g.age(10.0, 1.5));
                    g.file(s, name, size, age);
                }
                config_dirs.push(s);
            }
        }
        config_dirs.push(d);
    }

    let local = g.m.add_dir(home, ".local");
    let share = g.m.add_dir(local, "share");
    let apps = g.m.add_dir(share, "applications");
    for _ in 0..g.n(18.0) {
        let (name, age) = (format!("{}.desktop", g.word()), g.age(200.0, 1.0));
        g.file(apps, name, 400.0, age);
    }
    let trash_root = g.m.add_dir(share, "Trash");
    let trash = g.m.add_dir(trash_root, "files");
    for _ in 0..g.n(25.0) {
        let (name, size, age) = (g.src_name("txt"), g.rng.lognormal(200_000.0, 2.0), g.age(30.0, 1.0));
        g.file(trash, name, size, age);
    }
    let ext_dir = g.m.add_dir(share, "gnome-shell");
    let exts = g.m.add_dir(ext_dir, "extensions");
    for name in ["hanabi@jeffshee", "appindicator@ubuntu", "ding@rastersoft.com"] {
        let d = g.m.add_dir(exts, name);
        for f in ["extension.js", "metadata.json", "prefs.js", "stylesheet.css"] {
            let age = g.age(150.0, 0.7);
            g.file(d, f.into(), 8_000.0, age);
        }
    }

    // Visible directories.
    let desktop = g.m.add_dir(home, "Desktop");
    for _ in 0..g.n(5.0) {
        let (name, age) = (format!("{}.desktop", g.word()), g.age(300.0, 0.5));
        g.file(desktop, name, 500.0, age);
    }

    let documents = g.m.add_dir(home, "Documents");
    let mut doc_dirs = vec![];
    for sub in ["Invoices", "Manuals", "Notes", "Polymath", "Reports", "Taxes 2024", "Taxes 2025"] {
        let d = g.m.add_dir(documents, sub);
        for _ in 0..g.n(14.0) {
            let ext = *g.rng.pick(&["pdf", "pdf", "odt", "docx", "xlsx", "txt"]);
            let name = format!("{} {} {}.{ext}", g.rng.pick(DOC_WORDS), g.rng.pick(DOC_WORDS), g.rng.int(1, 2099));
            let (size, age) = (g.rng.lognormal(400_000.0, 1.3), g.age(240.0, 1.1));
            g.file(d, name, size, age);
        }
        doc_dirs.push(d);
    }

    let downloads = g.m.add_dir(home, "Downloads");
    for _ in 0..g.n(200.0) {
        let (ext, med) = *g.rng.pick(&[
            ("pdf", 900_000.0),
            ("zip", 40_000_000.0),
            ("deb", 60_000_000.0),
            ("AppImage", 200_000_000.0),
            ("png", 500_000.0),
            ("stl", 3_000_000.0),
            ("step", 8_000_000.0),
            ("csv", 200_000.0),
            ("tar.gz", 30_000_000.0),
        ]);
        let name = format!("{}-{}_{}.{ext}", g.word(), g.word(), g.rng.int(1, 999));
        let (size, age) = (g.rng.lognormal(med, 1.0), g.age(90.0, 1.4));
        g.file(downloads, name, size, age);
    }

    let music = g.m.add_dir(home, "Music");
    for _ in 0..g.n(5.0) {
        let artist = format!("{} {}", g.rng.pick(DOC_WORDS), g.rng.pick(DOC_WORDS));
        let a = g.m.add_dir(music, artist);
        for al in 0..g.rng.int(1, 3) {
            let album = g.m.add_dir(a, format!("Album {}", al + 1));
            let age = g.age(900.0, 0.4);
            for tr in 0..g.rng.int(7, 13) {
                let (name, size) = (format!("{:02} {}.flac", tr + 1, g.word()), g.rng.lognormal(30_000_000.0, 0.3));
                g.file(album, name, size, age);
            }
        }
    }

    let pictures = g.m.add_dir(home, "Pictures");
    let mut picture_dirs = vec![];
    let shots = g.m.add_dir(pictures, "Screenshots");
    for _ in 0..g.n(140.0) {
        let age = g.age(120.0, 1.2);
        let (y, mo, d, h, mi, s) = civil(g.mtime(age));
        let name = format!("Screenshot from {y}-{mo:02}-{d:02} {h:02}-{mi:02}-{s:02}.png");
        let size = g.rng.lognormal(450_000.0, 0.6);
        g.file(shots, name, size, age);
    }
    picture_dirs.push(shots);
    for year in 2022..=2026 {
        let yd = g.m.add_dir(pictures, year.to_string());
        let year_age = ((2026 - year) as f64 * 365.0 + 120.0).max(10.0);
        for month in 1..=12 {
            if !g.rng.chance(0.5) {
                continue;
            }
            let md = g.m.add_dir(yd, format!("{year}-{month:02}"));
            for _ in 0..g.n(22.0) {
                let age = year_age + g.rng.range(-60.0, 60.0);
                let (y, mo, d, h, mi, s) = civil(g.mtime(age));
                let name = format!("IMG_{y}{mo:02}{d:02}_{h:02}{mi:02}{s:02}.jpg");
                let size = g.rng.lognormal(3_500_000.0, 0.4);
                g.file(md, name, size, age);
            }
            picture_dirs.push(md);
        }
    }

    let videos = g.m.add_dir(home, "Videos");
    for _ in 0..g.n(12.0) {
        let (name, size, age) = (format!("{}_{}.mp4", g.word(), g.rng.int(1, 99)), g.rng.lognormal(400_000_000.0, 1.0), g.age(300.0, 1.0));
        g.file(videos, name, size, age);
    }

    // Projects, modelled on the kinds of trees found in a robotics developer's home.
    let mut projects = vec![];
    for (name, ext, weight, age_med) in [
        ("eink", "c", 1.0, 200.0),
        ("file_cycler", "py", 0.8, 240.0),
        ("gits-file-visualizer", "rs", 4.0, 0.5),
        ("microlink-polymath", "cpp", 1.0, 180.0),
        ("pico", "c", 0.8, 100.0),
        ("protective-stop", "cpp", 3.0, 4.0),
        ("ros2_ws", "py", 1.5, 60.0),
        ("turing-smart-screen-python", "py", 0.6, 300.0),
    ] {
        projects.push(gen_project(&mut g, home, name, ext, weight, age_med));
    }

    g.m.sort_all();
    for p in &mut projects {
        p.src.sort();
    }

    SynthHome {
        model: g.m,
        index: Index { projects, pictures: picture_dirs, downloads, documents: doc_dirs, config: config_dirs, cache: cache_dirs, trash },
    }
}

fn gen_project(g: &mut Gen, home: DirId, name: &str, ext: &'static str, weight: f64, age_med: f64) -> Project {
    let root = g.m.add_dir(home, name);
    let build_file = match ext {
        "rs" => "Cargo.toml",
        "py" => "setup.py",
        _ => "CMakeLists.txt",
    };
    for f in ["README.md", "LICENSE", ".gitignore", build_file] {
        let age = g.age(age_med * 2.0, 1.0);
        let size = g.rng.lognormal(3_000.0, 0.8);
        g.file(root, f.into(), size, age);
    }

    let mut src = vec![];
    let src_root = g.m.add_dir(root, if ext == "rs" { "crates" } else { "src" });
    src.push(src_root);
    for _ in 0..g.n(6.0) {
        let module = g.word();
        let d = g.m.add_dir(src_root, module);
        for _ in 0..g.n(8.0) {
            let (fname, size, age) = (g.src_name(ext), g.rng.lognormal(9_000.0, 1.0), g.age(age_med * 3.0, 1.2));
            g.file(d, fname, size, age);
        }
        if ext == "c" || ext == "cpp" {
            for _ in 0..g.n(4.0) {
                let hext = if ext == "c" { "h" } else { "hpp" };
                let (fname, size, age) = (g.src_name(hext), g.rng.lognormal(4_000.0, 0.8), g.age(age_med * 3.0, 1.2));
                g.file(d, fname, size, age);
            }
        }
        if ext == "py" && g.rng.chance(0.7) {
            let pc = g.m.add_dir(d, "__pycache__");
            for _ in 0..g.n(8.0) {
                let fname = format!("{}.cpython-310.pyc", g.word());
                let age = g.age(age_med, 1.0);
                let size = g.rng.lognormal(12_000.0, 0.8);
                g.file(pc, fname, size, age);
            }
        }
        src.push(d);
    }
    let tests = g.m.add_dir(root, "tests");
    for _ in 0..g.n(6.0) {
        let (fname, size, age) = (format!("test_{}", g.src_name(ext)), g.rng.lognormal(6_000.0, 0.8), g.age(age_med * 3.0, 1.0));
        g.file(tests, fname, size, age);
    }
    src.push(tests);

    let mut build = vec![];
    if ext == "rs" {
        let target = g.m.add_dir(root, "target");
        let debug = g.m.add_dir(target, "debug");
        let deps = g.m.add_dir(debug, "deps");
        for _ in 0..g.n(90.0) {
            let (w, h) = (g.word(), g.hex(16));
            let fext = *g.rng.pick(&["rlib", "rmeta", "d"]);
            let (size, age) = (g.rng.lognormal(600_000.0, 1.5), g.age(age_med, 1.0));
            g.file(deps, format!("lib{w}-{h}.{fext}"), size, age);
        }
        build.push(deps);
        let inc = g.m.add_dir(debug, "incremental");
        for _ in 0..g.n(5.0) {
            let dname = format!("{}-{}", g.word(), g.hex(13));
            let d = g.m.add_dir(inc, dname);
            for _ in 0..g.n(18.0) {
                let (fname, size, age) = (format!("{}.o", g.hex(20)), g.rng.lognormal(90_000.0, 1.2), g.age(age_med, 1.0));
                g.file(d, fname, size, age);
            }
            build.push(d);
        }
    } else if ext == "py" {
        if g.rng.chance(0.6) {
            let venv = g.m.add_dir(root, ".venv");
            let lib = g.m.add_dir(venv, "lib");
            let py = g.m.add_dir(lib, "python3.10");
            let sp = g.m.add_dir(py, "site-packages");
            for _ in 0..g.n(14.0) {
                let pkg = g.word();
                let d = g.m.add_dir(sp, pkg);
                let age = g.age(age_med * 2.0, 0.5);
                for _ in 0..g.n(10.0) {
                    let (fname, size) = (g.src_name("py"), g.rng.lognormal(12_000.0, 1.0));
                    g.file(d, fname, size, age);
                }
            }
        }
        let bd = g.m.add_dir(root, "build");
        for _ in 0..g.n(30.0) {
            let (fname, size, age) = (g.src_name("py"), g.rng.lognormal(9_000.0, 1.0), g.age(age_med, 1.0));
            g.file(bd, fname, size, age);
        }
        build.push(bd);
    } else {
        let bd = g.m.add_dir(root, "build");
        let cmf = g.m.add_dir(bd, "CMakeFiles");
        for _ in 0..g.n(3.0) {
            let tname = format!("{}.dir", g.word());
            let t = g.m.add_dir(cmf, tname);
            for _ in 0..g.n(30.0) {
                let (fname, size, age) = (format!("{}.{ext}.o", g.word()), g.rng.lognormal(40_000.0, 1.0), g.age(age_med, 1.0));
                g.file(t, fname, size, age);
            }
            build.push(t);
        }
        for f in ["CMakeCache.txt", "Makefile", "compile_commands.json"] {
            let age = g.age(age_med, 1.0);
            g.file(bd, f.into(), 40_000.0, age);
        }
        let (bname, age) = (format!("{name}.elf"), g.age(age_med, 1.0));
        g.file(bd, bname, 2_500_000.0, age);
        build.push(bd);
    }

    let git = g.m.add_dir(root, ".git");
    for f in ["HEAD", "config", "description", "index", "ORIG_HEAD", "FETCH_HEAD"] {
        let age = g.age(age_med, 1.0);
        let size = g.rng.lognormal(2_000.0, 1.5);
        g.file(git, f.into(), size, age);
    }
    let hooks = g.m.add_dir(git, "hooks");
    for h in ["pre-commit.sample", "pre-push.sample", "commit-msg.sample", "post-update.sample", "pre-rebase.sample"] {
        g.file(hooks, h.into(), 1_500.0, 900.0);
    }
    let objects = g.m.add_dir(git, "objects");
    let mut git_objects = vec![];
    for _ in 0..g.n(36.0) {
        let dname = g.hex(2);
        let d = g.m.add_dir(objects, dname);
        for _ in 0..g.n(5.0) {
            let (fname, size, age) = (g.hex(38), g.rng.lognormal(3_000.0, 1.3), g.age(age_med * 4.0, 1.5));
            g.file(d, fname, size, age);
        }
        git_objects.push(d);
    }
    let pack = g.m.add_dir(objects, "pack");
    for _ in 0..g.rng.int(1, 3) {
        let h = g.hex(40);
        let age = g.age(age_med * 6.0, 1.0);
        let (pack_size, idx_size) = (g.rng.lognormal(20_000_000.0, 1.0), g.rng.lognormal(400_000.0, 1.0));
        g.file(pack, format!("pack-{h}.pack"), pack_size, age);
        g.file(pack, format!("pack-{h}.idx"), idx_size, age);
    }

    Project { name: name.into(), root, src, build, git_objects, git, ext, weight }
}

// ---------------------------------------------------------------------------
// Work session
// ---------------------------------------------------------------------------

#[derive(Clone, Debug)]
enum Phase {
    Calm,
    Edit { p: usize, focus: Vec<FileId> },
    Build { p: usize },
    Commit { p: usize },
    Browse { dir: DirId, cursor: usize },
    Download { file: FileId, size: f64 },
    Cleanup { targets: Vec<FileId> },
}

pub struct Session {
    rng: Rng,
    phase: Phase,
    /// Session-local clock, so a restarted session replays exactly the same story.
    clock: f64,
    phase_start: f64,
    phase_end: f64,
    browse_timer: f64,
}

impl Session {
    pub fn new(seed: u64) -> Self {
        Self {
            rng: Rng::new(seed ^ 0xC0FF_EE00_2501),
            phase: Phase::Calm,
            clock: 0.0,
            phase_start: 0.0,
            phase_end: 20.0,
            browse_timer: 0.0,
        }
    }

    /// Short human-readable description of the current phase (for logs/HUD).
    pub fn phase_name(&self) -> &'static str {
        match self.phase {
            Phase::Calm => "calm",
            Phase::Edit { .. } => "edit",
            Phase::Build { .. } => "build",
            Phase::Commit { .. } => "commit",
            Phase::Browse { .. } => "browse",
            Phase::Download { .. } => "download",
            Phase::Cleanup { .. } => "cleanup",
        }
    }

    /// Advances the session from `t` by `dt` seconds, mutating the model.
    /// `t` is the model time used for event timestamps; phases run on the session clock.
    pub fn step(&mut self, m: &mut Model, ix: &Index, t: f64, dt: f64) {
        self.clock += dt;
        if self.clock >= self.phase_end {
            self.next_phase(m, ix, t);
        }
        let rng = &mut self.rng;

        // Background noise from daemons and apps.
        for _ in 0..rng.poisson(0.2 * dt) {
            if let Some(f) = random_file(rng, m, &ix.config) {
                m.read(f, t);
            }
        }
        for _ in 0..rng.poisson(0.1 * dt) {
            let d = *rng.pick(&ix.cache);
            if rng.chance(0.35) {
                let name = format!("{:016x}", rng.next_u64());
                m.create(d, name, rng.lognormal(20_000.0, 1.0) as u64, t);
            } else if let Some(f) = random_file_in(rng, m, d) {
                m.write(f, t, None);
            }
        }

        match &mut self.phase {
            Phase::Calm => {}
            Phase::Edit { p, focus } => {
                let proj = &ix.projects[*p];
                for _ in 0..rng.poisson(dt / 9.0) {
                    if !focus.is_empty() {
                        let f = *rng.pick(focus);
                        let grow = m.files[f as usize].size + rng.int(-200, 900).max(0) as u64;
                        m.write(f, t, Some(grow));
                    }
                }
                for _ in 0..rng.poisson(0.3 * dt) {
                    if let Some(f) = random_file(rng, m, &proj.src) {
                        m.read(f, t);
                    }
                }
            }
            Phase::Build { p } => {
                let proj = &ix.projects[*p];
                for _ in 0..rng.poisson(14.0 * dt) {
                    if let Some(f) = random_file(rng, m, &proj.src) {
                        m.read(f, t);
                    }
                }
                for _ in 0..rng.poisson(9.0 * dt) {
                    if let Some(f) = random_file(rng, m, &proj.build) {
                        m.write(f, t, None);
                    }
                }
                for _ in 0..rng.poisson(1.0 * dt) {
                    let d = *rng.pick(&proj.build);
                    let name = format!("{}-{:012x}.o", rng.pick(WORDS), rng.next_u64() >> 16);
                    m.create(d, name, rng.lognormal(60_000.0, 1.2) as u64, t);
                }
            }
            Phase::Commit { p } => {
                let proj = &ix.projects[*p];
                for _ in 0..rng.poisson(3.0 * dt) {
                    let d = *rng.pick(&proj.git_objects);
                    let name = format!("{:038x}", rng.next_u64() as u128 * 0x1_0000_0001);
                    m.create(d, name, rng.lognormal(2_500.0, 1.0) as u64, t);
                }
                for _ in 0..rng.poisson(0.8 * dt) {
                    if let Some(f) = random_file_in(rng, m, proj.git) {
                        m.write(f, t, None);
                    }
                }
            }
            Phase::Browse { dir, cursor } => {
                self.browse_timer -= dt;
                if self.browse_timer <= 0.0 {
                    self.browse_timer = rng.range(1.5, 4.0);
                    let files: Vec<FileId> = live_files(m, *dir);
                    if !files.is_empty() {
                        m.read(files[*cursor % files.len()], t);
                        *cursor += 1;
                    }
                }
            }
            Phase::Download { file, size } => {
                if rng.chance((dt / 0.7).min(1.0)) {
                    *size *= rng.range(1.15, 1.6);
                    m.write(*file, t, Some(*size as u64));
                }
            }
            Phase::Cleanup { targets } => {
                let dur = (self.phase_end - self.phase_start).max(0.5);
                let n = rng.poisson(targets.len() as f64 / dur * dt * 1.2) as usize;
                for _ in 0..n.min(targets.len()) {
                    let f = targets.swap_remove(rng.below(targets.len()));
                    m.delete(f, t);
                }
            }
        }
    }

    fn next_phase(&mut self, m: &mut Model, ix: &Index, t: f64) {
        let rng = &mut self.rng;
        let r = rng.f64();
        let pick_project = |rng: &mut Rng| -> usize {
            let total: f64 = ix.projects.iter().map(|p| p.weight).sum();
            let mut x = rng.f64() * total;
            for (i, p) in ix.projects.iter().enumerate() {
                x -= p.weight;
                if x <= 0.0 {
                    return i;
                }
            }
            0
        };
        let current_p = match &self.phase {
            Phase::Edit { p, .. } | Phase::Build { p } | Phase::Commit { p } => Some(*p),
            _ => None,
        };
        let edit = |rng: &mut Rng, m: &Model, p: usize| -> Phase {
            let files: Vec<FileId> = ix.projects[p].src.iter().flat_map(|&d| live_files(m, d)).collect();
            let focus = (0..rng.int(1, 3)).filter_map(|_| files.get(rng.below(files.len())).copied()).collect();
            Phase::Edit { p, focus }
        };

        let (phase, dur) = match (&self.phase, current_p) {
            (Phase::Calm, _) | (Phase::Browse { .. }, _) | (Phase::Download { .. }, _) | (Phase::Cleanup { .. }, _) => {
                if r < 0.45 {
                    let p = pick_project(rng);
                    (edit(rng, m, p), rng.range(90.0, 240.0))
                } else if r < 0.62 {
                    let dir = *rng.pick(&ix.pictures);
                    (Phase::Browse { dir, cursor: 0 }, rng.range(30.0, 90.0))
                } else if r < 0.72 {
                    let ext = *rng.pick(&["zip", "pdf", "AppImage", "step", "deb"]);
                    let name = format!("{}-{}.{ext}", rng.pick(WORDS), rng.int(1, 999));
                    let size = rng.lognormal(200_000.0, 1.0);
                    let file = m.create(ix.downloads, name, size as u64, t);
                    (Phase::Download { file, size }, rng.range(12.0, 40.0))
                } else if r < 0.78 {
                    let p = pick_project(rng);
                    let d = *rng.pick(&ix.projects[p].build);
                    let mut files = live_files(m, d);
                    let keep = (files.len() as f64 * rng.range(0.4, 0.75)) as usize;
                    for _ in 0..keep.min(files.len()) {
                        files.swap_remove(rng.below(files.len()));
                    }
                    (Phase::Cleanup { targets: files }, rng.range(3.0, 6.0))
                } else {
                    (Phase::Calm, rng.range(40.0, 120.0))
                }
            }
            (Phase::Edit { .. }, Some(p)) => {
                if r < 0.5 {
                    (Phase::Build { p }, rng.range(12.0, 30.0))
                } else if r < 0.7 {
                    (Phase::Commit { p }, rng.range(3.0, 6.0))
                } else if r < 0.85 {
                    (edit(rng, m, p), rng.range(60.0, 180.0))
                } else {
                    (Phase::Calm, rng.range(30.0, 90.0))
                }
            }
            (Phase::Build { .. }, Some(p)) => {
                if r < 0.6 {
                    (edit(rng, m, p), rng.range(60.0, 180.0))
                } else if r < 0.8 {
                    (Phase::Commit { p }, rng.range(3.0, 6.0))
                } else {
                    (Phase::Calm, rng.range(30.0, 90.0))
                }
            }
            (Phase::Commit { .. }, Some(p)) => {
                if r < 0.5 {
                    (edit(rng, m, p), rng.range(60.0, 180.0))
                } else {
                    (Phase::Calm, rng.range(40.0, 120.0))
                }
            }
            _ => (Phase::Calm, 60.0),
        };
        self.phase = phase;
        self.phase_start = self.clock;
        self.phase_end = self.clock + dur;
    }
}

fn live_files(m: &Model, d: DirId) -> Vec<FileId> {
    m.dirs[d as usize].files.iter().copied().filter(|&f| m.is_live(f)).collect()
}

fn random_file_in(rng: &mut Rng, m: &Model, d: DirId) -> Option<FileId> {
    let files = &m.dirs[d as usize].files;
    for _ in 0..4 {
        let f = *files.get(rng.below(files.len()))?;
        if m.is_live(f) {
            return Some(f);
        }
    }
    None
}

fn random_file(rng: &mut Rng, m: &Model, dirs: &[DirId]) -> Option<FileId> {
    if dirs.is_empty() {
        return None;
    }
    for _ in 0..4 {
        let d = *rng.pick(dirs);
        if let Some(f) = random_file_in(rng, m, d) {
            return Some(f);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn civil_dates() {
        assert_eq!(civil(0), (1970, 1, 1, 0, 0, 0));
        assert_eq!(civil(SYNTH_EPOCH_UNIX), (2026, 9, 27, 0, 0, 0));
    }

    #[test]
    fn home_is_deterministic_and_plausible() {
        let a = generate_home(7, 1.0);
        let b = generate_home(7, 1.0);
        assert_eq!(a.model.files.len(), b.model.files.len());
        assert!(a.model.dirs.len() > 300);
        assert!(a.model.files.len() > 5_000);
    }

    #[test]
    fn restarted_session_replays_identically() {
        let run = |t0: f64| {
            let mut home = generate_home(3, 0.3);
            let mut s = Session::new(3);
            let dt = 1.0 / 24.0;
            let mut log = vec![];
            for i in 1..(24 * 400) {
                s.step(&mut home.model, &home.index, t0 + i as f64 * dt, dt);
                log.extend(home.model.drain_events().into_iter().map(|e| (e.kind, e.file, ((e.t - t0) * 24.0).round() as i64)));
            }
            log
        };
        let a = run(0.0);
        assert!(!a.is_empty());
        assert_eq!(a, run(12_345.6));
    }

    #[test]
    fn session_produces_events() {
        let mut home = generate_home(1, 0.5);
        let mut s = Session::new(1);
        let mut n = 0;
        let dt = 1.0 / 24.0;
        for i in 0..(24 * 600) {
            s.step(&mut home.model, &home.index, i as f64 * dt, dt);
            n += home.model.drain_events().len();
        }
        assert!(n > 100, "only {n} events in 10 minutes");
    }
}
