//! gits-render: the tower renderer. M0 runs on synthetic data, either in a
//! window or headlessly to PNG.

mod app;
mod camera;
mod fx;
mod gpu;
mod gpu_types;
mod renderer;
mod snapshot;
mod source;
mod text;
mod world;

use anyhow::Result;
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
    },
    /// Print statistics about the synthetic dataset and session.
    Stats {
        /// Simulated minutes of session to summarise.
        #[arg(long, default_value_t = 10.0)]
        minutes: f64,
        /// Print every phase change of the work session.
        #[arg(long)]
        timeline: bool,
    },
    /// Print the built-in default configuration.
    DefaultConfig,
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
            app::run(cfg, app::Options { config_path, outputs, wallpaper: false, warmup, speed, collector, fell_back })
        }
        Cmd::Wallpaper { outputs } => {
            let fell_back = fell_back.then(|| socket.clone());
            app::run(cfg, app::Options { config_path, outputs, wallpaper: true, warmup: 0.0, speed: None, collector, fell_back })
        }
        Cmd::Snapshot { out, width, height, at, frames, every } => {
            snapshot::run(cfg, snapshot::Options { out, width, height, at, frames, every }, collector)
        }
        Cmd::Bench { width, height, at, frames } => snapshot::bench(cfg, width, height, at, frames, collector),
        Cmd::Stats { minutes, timeline } => {
            stats(cfg, minutes, timeline, collector);
            Ok(())
        }
        Cmd::DefaultConfig => {
            print!("{}", gits_core::config::DEFAULT_TOML);
            Ok(())
        }
    }
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

fn stats(cfg: Config, minutes: f64, timeline: bool, collector: Option<PathBuf>) {
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
