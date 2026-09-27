//! gits-collector: scans the configured folders, watches them for changes and
//! streams the filesystem model to gits-render over a Unix socket.

mod collector;
mod exclude;
mod inotify;
mod server;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use collector::Collector;
use exclude::{Excludes, expand_home};
use gits_core::config::Config;
use gits_core::protocol;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

static STOP: AtomicBool = AtomicBool::new(false);

extern "C" fn on_signal(_: libc::c_int) {
    STOP.store(true, Ordering::SeqCst);
}

#[derive(Parser)]
#[command(name = "gits-collector", version, about = "Filesystem scanner/watcher feeding gits-render")]
struct Cli {
    /// Config file merged over the built-in defaults
    /// [default: ~/.config/gits-file-visualizer/config.toml if it exists].
    #[arg(long, global = true)]
    config: Option<PathBuf>,
    /// Socket path [default: $XDG_RUNTIME_DIR/gits-file-visualizer/collector.sock].
    #[arg(long, global = true)]
    socket: Option<PathBuf>,
    /// Keep normal CPU and I/O priority instead of idle.
    #[arg(long, global = true)]
    no_nice: bool,
    #[command(subcommand)]
    cmd: Option<Cmd>,
}

#[derive(Subcommand)]
enum Cmd {
    /// Scan, watch and serve (default).
    Run,
    /// Scan once, print statistics and the watch budget, then exit.
    Scan,
    /// Connect to the running collector and print changes as they stream in.
    Monitor {
        /// Only print paths containing this text.
        #[arg(long)]
        filter: Option<String>,
        /// Also print reads.
        #[arg(long)]
        reads: bool,
    },
}

fn lower_priority() {
    // SAFETY: plain syscalls on the current process.
    unsafe {
        libc::setpriority(libc::PRIO_PROCESS, 0, 19);
        // IOPRIO_WHO_PROCESS = 1, IOPRIO_CLASS_IDLE = 3.
        libc::syscall(libc::SYS_ioprio_set, 1, 0, 3 << 13);
    }
}

fn watch_budget(configured: usize) -> (usize, usize, usize) {
    let max = inotify::max_user_watches();
    let used = inotify::watches_in_use();
    let free = max.saturating_sub(used);
    let budget = if configured > 0 { configured.min(free.saturating_sub(1024)) } else { (max / 2).min(free.saturating_sub(4096)) };
    (budget, max, used)
}

/// Debug aid (RUST_LOG=gits_collector=debug): the folders with the most activity.
struct Busiest {
    counts: std::collections::HashMap<u32, [u32; 4]>,
    since: Instant,
}

impl Default for Busiest {
    fn default() -> Self {
        Self { counts: Default::default(), since: Instant::now() }
    }
}

impl Busiest {
    fn add(&mut self, col: &Collector, ops: &[protocol::Op]) {
        use protocol::Op;
        for op in ops {
            let (dir, k) = match op {
                Op::Read { file } => (col.model.files[*file as usize].dir, 0),
                Op::Write { file, .. } => (col.model.files[*file as usize].dir, 1),
                Op::Create { dir, .. } => (*dir, 2),
                Op::Delete { file } => (col.model.files[*file as usize].dir, 3),
                Op::MkDir { parent, .. } => (*parent, 2),
                Op::RmDir { dir } => (*dir, 3),
            };
            self.counts.entry(dir).or_default()[k] += 1;
        }
    }

    fn report(&mut self, col: &Collector) {
        let secs = self.since.elapsed().as_secs_f64();
        let mut v: Vec<(u32, [u32; 4])> = self.counts.drain().collect();
        let total: u32 = v.iter().map(|(_, c)| c.iter().sum::<u32>()).sum();
        v.sort_by_key(|(_, c)| std::cmp::Reverse(c.iter().sum::<u32>()));
        log::debug!("{:.1} ops/s over {secs:.0}s; busiest folders (read/write/create/delete):", total as f64 / secs);
        for (d, c) in v.iter().take(8) {
            log::debug!("  {:>5} {:>5} {:>5} {:>5}  {}", c[0], c[1], c[2], c[3], col.model.dir_path(*d));
        }
        self.since = Instant::now();
    }
}

fn monitor(socket: &std::path::Path, filter: Option<&str>, reads: bool) -> Result<()> {
    use protocol::{Msg, Op};
    let stream = std::os::unix::net::UnixStream::connect(socket).with_context(|| format!("connecting to {}", socket.display()))?;
    let mut r = std::io::BufReader::new(stream);
    let mut model: Option<gits_core::model::Model> = None;
    let started = Instant::now();
    loop {
        match protocol::read_msg(&mut r)? {
            Msg::Hello { version } => eprintln!("connected (protocol v{version})"),
            Msg::Status(s) => eprintln!("status: {s}"),
            Msg::Snapshot(bytes) => {
                let m = protocol::decode_snapshot(&bytes, 0)?;
                eprintln!("snapshot: {} directories, {} files", m.live_dir_count(), m.live_file_count());
                model = Some(m);
            }
            Msg::Ops(ops) => {
                let Some(m) = model.as_mut() else { continue };
                let t = started.elapsed().as_secs_f64();
                for op in ops {
                    // Resolve paths before applying (deleted entries keep their names).
                    let text = match &op {
                        Op::MkDir { parent, name } => format!("mkdir   {}/{name}", m.dir_path(*parent)),
                        Op::RmDir { dir } => format!("rmdir   {}", m.dir_path(*dir)),
                        Op::Create { dir, name, size, .. } => format!("create  {}/{name} ({size} B)", m.dir_path(*dir)),
                        Op::Delete { file } => format!("delete  {}", m.file_path(*file)),
                        Op::Write { file, size, .. } => format!("write   {} ({size} B)", m.file_path(*file)),
                        Op::Read { file } => format!("read    {}", m.file_path(*file)),
                    };
                    m.apply(&op, t);
                    if matches!(op, Op::Read { .. }) && !reads {
                        continue;
                    }
                    if filter.is_none_or(|f| text.contains(f)) {
                        println!("{t:8.2}  {text}");
                    }
                }
            }
        }
    }
}

fn main() -> Result<()> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).format_timestamp_millis().init();
    let cli = Cli::parse();
    if let Some(Cmd::Monitor { filter, reads }) = &cli.cmd {
        let socket = cli.socket.clone().unwrap_or_else(protocol::default_socket_path);
        return monitor(&socket, filter.as_deref(), *reads);
    }
    let config_path = cli.config.clone().or_else(|| Config::user_path().filter(|p| p.exists()));
    let cfg = Config::load(config_path.as_deref())?;
    let c = &cfg.collector;
    let home = PathBuf::from(std::env::var_os("HOME").context("HOME is not set")?);
    if c.roots.len() > 1 {
        log::warn!("only the first of [collector].roots is used for now ({})", c.roots[0]);
    }
    let root_str = c.roots.first().map(String::as_str).unwrap_or("~");
    let root = expand_home(root_str, &home);
    let root_name = if root == home { "~".to_string() } else { root.display().to_string() };
    let excludes = Excludes::new(&c.excludes, &home);
    if !cli.no_nice {
        lower_priority();
    }

    let (budget, max, used) = watch_budget(c.max_watches);
    log::info!("inotify: limit {max} watches per user, {used} in use by other apps → budget {budget}");
    let mut col = Collector::new(&root, &root_name, excludes, budget, c.crawl_interval.0 as f64)
        .with_context(|| format!("root {}", root.display()))?;

    let socket = cli.socket.clone().unwrap_or_else(protocol::default_socket_path);
    let mut server = match cli.cmd {
        Some(Cmd::Scan) => None,
        _ => Some(server::Server::start(&socket).with_context(|| format!("listening on {}", socket.display()))?),
    };
    if let Some(s) = &server {
        s.set_status(format!("scanning {}", root.display()));
        log::info!("listening on {}", socket.display());
    }

    let t0 = Instant::now();
    col.initial_scan();
    let st = col.stats();
    log::info!(
        "scanned {} in {:.1}s: {} directories, {} files; {} watched live (budget {})",
        root.display(),
        t0.elapsed().as_secs_f64(),
        st.dirs,
        st.files,
        st.watched,
        st.budget
    );
    let Some(server) = server.as_mut() else {
        println!(
            "directories: {}\nfiles:       {}\nwatched:     {} of budget {} (limit {max}, {used} used by others)",
            st.dirs, st.files, st.watched, st.budget
        );
        println!("snapshot:    {:.1} MB", protocol::encode_snapshot(&col.model).len() as f64 / 1e6);
        return Ok(());
    };
    server.set_status("ready");

    // SAFETY: installing a handler that only stores to an atomic.
    unsafe {
        libc::signal(libc::SIGTERM, on_signal as *const () as libc::sighandler_t);
        libc::signal(libc::SIGINT, on_signal as *const () as libc::sighandler_t);
    }

    let batch = Duration::from_millis(100);
    let mut last_flush = Instant::now();
    let mut last_stats = Instant::now();
    let mut events = Vec::new();
    let mut busiest = Busiest::default();
    while !STOP.load(Ordering::SeqCst) {
        if col.inotify().wait(100)? {
            col.inotify().read(&mut events)?;
            for ev in events.drain(..) {
                col.handle(ev);
            }
        }
        if last_flush.elapsed() >= batch {
            let ops = col.flush();
            if log::log_enabled!(log::Level::Debug) {
                busiest.add(&col, &ops);
            }
            if !ops.is_empty() {
                server.broadcast(protocol::ops_frame(&ops));
            }
            server.accept_new(|| protocol::snapshot_frame(&col.model));
            last_flush = Instant::now();
            col.crawl_tick();
        }
        if busiest.since.elapsed() >= Duration::from_secs(30) {
            busiest.report(&col);
        }
        if last_stats.elapsed() >= Duration::from_secs(600) {
            let st = col.stats();
            log::info!("{} directories, {} files, {} watched, {} clients", st.dirs, st.files, st.watched, server.client_count());
            last_stats = Instant::now();
        }
    }
    log::info!("stopping");
    Ok(())
}
