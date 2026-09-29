//! gits-render: the tower renderer. M0 runs on synthetic data, either in a
//! window or headlessly to PNG.

mod app;
mod callouts;
mod camera;
mod fx;
mod gpu;
mod gpu_types;
mod renderer;
mod snapshot;
mod source;
mod sysmon;
mod text;
mod world;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use gits_core::config::Config;
use std::path::PathBuf;

#[derive(Parser)]
#[command(name = "gits-render", version, about = "Ghost in the Shell: Innocence-style filesystem tower")]
struct Cli {
    /// Config file merged over the built-in defaults
    /// [default: ~/.config/gits-file-visualizer/config.toml if it exists].
    #[arg(long, global = true)]
    config: Option<PathBuf>,
    /// Override [synthetic].seed.
    #[arg(long, global = true)]
    seed: Option<u64>,
    /// Override [synthetic].scale.
    #[arg(long, global = true)]
    scale: Option<f32>,
    /// Data source: auto | collector | synthetic [window default: [data].source;
    /// snapshot/bench/stats default: synthetic, for reproducible output].
    #[arg(long, global = true)]
    source: Option<String>,
    /// Collector socket [default: $XDG_RUNTIME_DIR/gits-file-visualizer/collector.sock].
    #[arg(long, global = true)]
    socket: Option<PathBuf>,
    #[command(subcommand)]
    cmd: Option<Cmd>,
}

#[derive(Subcommand)]
enum Cmd {
    /// Open a window (default).
    Window {
        #[arg(long, default_value_t = 1600)]
        width: u32,
        #[arg(long, default_value_t = 900)]
        height: u32,
        /// Seconds of synthetic activity to simulate before the first frame.
        #[arg(long, default_value_t = 0.0)]
        warmup: f64,
        /// Simulated seconds per real second (overrides [synthetic].speed).
        #[arg(long)]
        speed: Option<f64>,
    },
    /// Wallpaper mode, normally started by the GNOME extension: one borderless
    /// window per monitor, control lines on stdin, exits when stdin closes.
    Wallpaper {
        /// A monitor as INDEX:WIDTHxHEIGHT[:NAME]; repeat for each monitor.
        #[arg(long = "output", required = true)]
        outputs: Vec<app::OutputSpec>,
    },
    /// Render PNG snapshots without a window.
    Snapshot {
        #[arg(long, default_value = "out/snapshot.png")]
        out: PathBuf,
        #[arg(long, default_value_t = 1920)]
        width: u32,
        #[arg(long, default_value_t = 1080)]
        height: u32,
        /// Simulated time (s) of the first capture.
        #[arg(long, default_value_t = 240.0)]
        at: f64,
        /// Number of captures.
        #[arg(long, default_value_t = 1)]
        frames: u32,
        /// Simulated seconds between captures.
        #[arg(long, default_value_t = 2.0)]
        every: f64,
        /// wide | drum | dial | auto [default: [monitors."*"].composition].
        #[arg(long)]
        composition: Option<String>,
    },
    /// Measure frame cost headlessly.
    Bench {
        #[arg(long, default_value_t = 1920)]
        width: u32,
        #[arg(long, default_value_t = 1080)]
        height: u32,
        #[arg(long, default_value_t = 160.0)]
        at: f64,
        #[arg(long, default_value_t = 120)]
        frames: u32,
        /// wide | drum | dial | auto [default: [monitors."*"].composition].
        #[arg(long)]
        composition: Option<String>,
    },
    /// Measure the wallpaper's real CPU/GPU cost headlessly (real-time pacing).
    Loadtest {
        /// Monitor size WIDTHxHEIGHT; repeat for each monitor.
        #[arg(long = "output", default_values_t = ["1920x1200".to_string(), "3840x2160".to_string(), "1440x2560".to_string()])]
        outputs: Vec<String>,
        #[arg(long, default_value_t = 30.0)]
        seconds: f64,
    },
    /// Print statistics about the synthetic dataset and session.
    Stats {
        /// Simulated minutes of session to summarise.
        #[arg(long, default_value_t = 10.0)]
        minutes: f64,
        /// Print every phase change of the work session.
        #[arg(long)]
        timeline: bool,
        /// Break the tower down by depth shell (what each shell holds) and exit.
        #[arg(long)]
        shells: bool,
    },
    /// Print the built-in default configuration.
    DefaultConfig,
    /// Report which GPUs can present to a window here (nothing is shown).
    GpuCheck,
}

fn main() -> Result<()> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info,wgpu_core=warn,wgpu_hal=warn,naga=warn"))
        .format_timestamp_millis()
        .init();
    let cli = Cli::parse();

    let config_path = cli.config.clone().or_else(|| Config::user_path().filter(|p| p.exists()));
    let mut cfg = Config::load(config_path.as_deref())?;
    if let Some(p) = &config_path {
        log::info!("config: {} (merged over defaults)", p.display());
    }
    // Before any thread exists (it sets an environment variable).
    gpu::select_vulkan_drivers(want_discrete(&cfg));
    if let Some(seed) = cli.seed {
        cfg.synthetic.seed = seed;
    }
    if let Some(scale) = cli.scale {
        cfg.synthetic.scale = scale;
    }

    let socket = cli.socket.clone().unwrap_or_else(gits_core::protocol::default_socket_path);
    let cmd = cli.cmd.unwrap_or(Cmd::Window { width: 1600, height: 900, warmup: 0.0, speed: None });
    let requested = cli.source.clone().unwrap_or_else(|| match cmd {
        Cmd::Window { .. } | Cmd::Wallpaper { .. } => cfg.data.source.clone(),
        _ => "synthetic".into(),
    });
    let (collector, fell_back) = resolve_source(&requested, &socket)?;
    match cmd {
        Cmd::Window { width, height, warmup, speed } => {
            let outputs = vec![app::OutputSpec { index: -1, name: "window".into(), width, height }];
            let fell_back = fell_back.then(|| socket.clone());
            app::run(cfg, app::Options { config_path, outputs, wallpaper: false, warmup, speed, collector, fell_back }).map(|_| ())
        }
        Cmd::Wallpaper { outputs } => {
            let fell_back = fell_back.then(|| socket.clone());
            let code =
                app::run(cfg, app::Options { config_path, outputs, wallpaper: true, warmup: 0.0, speed: None, collector, fell_back })?;
            if code != 0 {
                std::process::exit(code);
            }
            Ok(())
        }
        Cmd::Snapshot { out, width, height, at, frames, every, composition } => {
            snapshot::run(cfg, snapshot::Options { out, width, height, at, frames, every, composition }, collector)
        }
        Cmd::Bench { width, height, at, frames, composition } => snapshot::bench(cfg, width, height, at, frames, collector, composition),
        Cmd::Loadtest { outputs, seconds } => {
            let sizes = outputs
                .iter()
                .map(|o| {
                    o.split_once('x').and_then(|(w, h)| Some((w.parse().ok()?, h.parse().ok()?))).context("bad --output, expected WxH")
                })
                .collect::<Result<Vec<(u32, u32)>>>()?;
            snapshot::loadtest(cfg, sizes, seconds, collector)
        }
        Cmd::Stats { minutes, timeline, shells } => {
            stats(cfg, minutes, timeline, shells, collector);
            Ok(())
        }
        Cmd::GpuCheck => gpu::check(),
        Cmd::DefaultConfig => {
            print!("{}", gits_core::config::DEFAULT_TOML);
            Ok(())
        }
    }
}

/// What each depth shell of the tower holds: rings, files, collapsed folders,
/// share of the tower's height, and the folders contributing most.
fn print_shells(w: &world::World) {
    use gits_core::layout::SegSource;
    let m = &w.model;
    let tw = &w.cfg.tower;
    let layers = tw.depth_layers.max(1) as usize;
    let h = tw.height;
    // Files below a folder, including subfolders.
    fn subtree_files(m: &gits_core::model::Model, d: u32) -> usize {
        let dir = &m.dirs[d as usize];
        dir.files.iter().filter(|&&f| m.is_live(f)).count() + dir.dirs.iter().map(|&c| subtree_files(m, c)).sum::<usize>()
    }
    // A folder's path cut to its first `n` components below ~.
    let prefix = |d: u32, n: usize| -> String { m.dir_path(d).split('/').take(n + 1).collect::<Vec<_>>().join("/") };
    #[derive(Default)]
    struct Shell {
        rings: usize,
        dirs: std::collections::BTreeSet<u32>,
        files: usize,
        aggs: usize,
        agg_files: usize,
        height: f32,
        top: f32,
        by_prefix: std::collections::HashMap<String, usize>,
        by_deep: std::collections::HashMap<String, usize>,
    }
    let mut shells: Vec<Shell> = (0..layers).map(|_| Shell { top: f32::MAX, ..Default::default() }).collect();
    for (i, r) in w.layout.rings.iter().enumerate() {
        let s = &mut shells[(r.depth as usize).min(layers - 1)];
        s.rings += 1;
        s.dirs.insert(r.dir);
        s.height += w.ring_pitch.get(i).copied().unwrap_or(0.0);
        s.top = s.top.min(w.ring_s.get(i).copied().unwrap_or(0.0));
        *s.by_prefix.entry(prefix(r.dir, 2)).or_default() += 1;
        *s.by_deep.entry(prefix(r.dir, 5)).or_default() += 1;
    }
    for seg in &w.layout.segments {
        let Some(r) = w.layout.rings.get(seg.ring as usize) else { continue };
        let s = &mut shells[(r.depth as usize).min(layers - 1)];
        match seg.source {
            SegSource::File(_) => s.files += 1,
            SegSource::Agg(d) => {
                s.aggs += 1;
                s.agg_files += subtree_files(m, d);
            }
            SegSource::Dir(_) => {}
        }
    }
    println!();
    println!("shell  depth  radius  folders  rings  files   collapsed (files inside)  height  starts at");
    for (l, s) in shells.iter().enumerate() {
        let radius = tw.inner_radius + (tw.radius - tw.inner_radius) * l as f32 / (layers - 1).max(1) as f32;
        let depth = if l + 1 == layers { format!("{l}+") } else { format!("{l}") };
        println!(
            "{l:>5}  {depth:>5}  {radius:>6.2}  {:>7}  {:>5}  {:>6}  {:>7} ({:>8})          {:>4.0}%  {:>5.0}%",
            s.dirs.len(),
            s.rings,
            s.files,
            s.aggs,
            s.agg_files,
            s.height / h * 100.0,
            if s.top == f32::MAX { 0.0 } else { s.top / h * 100.0 },
        );
    }
    for (l, s) in shells.iter().enumerate().rev().take(2) {
        let mut top: Vec<(&String, &usize)> = s.by_prefix.iter().collect();
        top.sort_by(|a, b| b.1.cmp(a.1));
        println!();
        println!("shell {l}: rings by folder");
        for (p, n) in top.iter().take(12) {
            println!("  {n:>5}  {p}");
        }
    }
    if let Some(s) = shells.last() {
        let mut top: Vec<(&String, &usize)> = s.by_deep.iter().collect();
        top.sort_by(|a, b| b.1.cmp(a.1));
        println!();
        println!("shell {}: rings by folder, 5 levels down", layers - 1);
        for (p, n) in top.iter().take(20) {
            println!("  {n:>5}  {p}");
        }
    }
}

/// Discrete GPU wanted (per `[performance] gpu`) and present.
pub fn want_discrete(cfg: &Config) -> bool {
    cfg.performance.wants_discrete(app::on_battery()) && gpu::has_discrete_gpu()
}

/// Returns the collector socket to use (None = synthetic) and whether "auto"
/// fell back to synthetic data because no collector is running.
fn resolve_source(requested: &str, socket: &std::path::Path) -> Result<(Option<PathBuf>, bool)> {
    match requested {
        "synthetic" => Ok((None, false)),
        "collector" => Ok((Some(socket.to_path_buf()), false)),
        "auto" => {
            if source::collector_available(socket) {
                log::info!("using gits-collector at {}", socket.display());
                Ok((Some(socket.to_path_buf()), false))
            } else {
                log::info!("no gits-collector at {}; showing synthetic data", socket.display());
                Ok((None, true))
            }
        }
        other => anyhow::bail!("unknown source '{other}' (expected auto, collector or synthetic)"),
    }
}

fn stats(cfg: Config, minutes: f64, timeline: bool, shells: bool, collector: Option<PathBuf>) {
    let live = collector.is_some();
    let mut w = world::World::new(cfg, 16384, collector);
    if live {
        let t0 = std::time::Instant::now();
        while !w.has_data() && t0.elapsed().as_secs() < 180 {
            w.step(0.1);
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
    }
    println!("directories: {}", w.model.dirs.len());
    println!("files:       {}", w.model.live_file_count());
    println!("rings:       {}", w.rings.len());
    println!("segments:    {}", w.segs.len());
    if shells {
        w.step(0.5);
        print_shells(&w);
        return;
    }
    let mut phases = std::collections::BTreeMap::<&str, f64>::new();
    let dt = 1.0 / 24.0;
    let steps = (minutes * 60.0 / dt) as usize;
    let mut peak = 0.0f32;
    let mut last = "";
    for _ in 0..steps {
        w.step(dt);
        let phase = w.phase_name();
        if timeline && phase != last {
            println!("  t={:>6.1}s  {phase}", w.t);
            last = phase;
        }
        *phases.entry(phase).or_default() += dt;
        peak = peak.max(w.activity);
    }
    println!(
        "after {minutes} simulated minutes: {} files, {} rings, peak activity {:.1} events/s",
        w.model.live_file_count(),
        w.rings.len(),
        peak
    );
    for (p, secs) in phases {
        println!("  {p:<9} {:>5.0}s", secs);
    }
}
