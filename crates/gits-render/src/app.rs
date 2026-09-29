//! Window and wallpaper modes.
//!
//! - `window`: one normal window, for look-dev.
//! - `wallpaper`: one borderless window per monitor, launched by the GNOME
//!   extension, which clones each window into that monitor's background. The
//!   extension sends control lines on stdin (see [`Control`]); stdin closing
//!   means the extension is gone, and the renderer exits.

use crate::camera::{Camera, Composition};
use crate::gpu::{self, Gpu};
use crate::renderer::{Renderer, View};
use crate::world::{Notice, World};
use anyhow::{Context, Result};
use gits_core::config::{Config, Performance};
use std::io::BufRead;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};
use winit::application::ApplicationHandler;
use winit::dpi::{LogicalSize, PhysicalSize};
use winit::event::{ElementState, KeyEvent, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop, EventLoopProxy};
use winit::keyboard::{Key, NamedKey};
use winit::window::{Window, WindowId};

/// Warm-up step for the synthetic session (matches headless snapshots).
const SIM_DT: f64 = 1.0 / 24.0;
/// Several windows drawing within this interval share one simulation step.
const MIN_STEP: Duration = Duration::from_millis(3);
/// A redraw is requested this long before it is due; the compositor's frame
/// callback then releases it on the next display refresh (vsync pacing).
const PACING_SLACK: Duration = Duration::from_millis(3);
/// Exit code asking the extension to restart the renderer (e.g. to switch GPU).
pub const EXIT_RESTART: i32 = 75;
/// While nothing can draw (hidden, covered, locked), the simulation still
/// advances this often so collector updates are applied at their real time
/// instead of piling up.
const HIDDEN_STEP: Duration = Duration::from_secs(1);
/// How often wallpaper mode looks for a collector that was not running at start.
const COLLECTOR_PROBE: Duration = Duration::from_secs(10);
/// How often the power source is checked (battery caps the frame rate).
const POWER_CHECK: Duration = Duration::from_secs(30);

/// True when running on battery: a battery exists and no mains/USB supply is online.
pub fn on_battery() -> bool {
    let Ok(rd) = std::fs::read_dir("/sys/class/power_supply") else { return false };
    let mut battery = false;
    for e in rd.flatten() {
        let p = e.path();
        let kind = std::fs::read_to_string(p.join("type")).unwrap_or_default();
        match kind.trim() {
            "Battery" => battery = true,
            "Mains" | "USB" if std::fs::read_to_string(p.join("online")).is_ok_and(|v| v.trim() == "1") => return false,
            _ => {}
        }
    }
    battery
}

pub const APP_ID: &str = "io.github.iliabaranov.GitsFileVisualizer";
/// Window title prefix the GNOME extension uses to map windows to monitors.
pub const WALLPAPER_TITLE: &str = "gits-file-visualizer|";

/// One output: a monitor (wallpaper mode) or the look-dev window.
#[derive(Clone, Debug)]
pub struct OutputSpec {
    /// Monitor index as used by GNOME (-1 for the look-dev window).
    pub index: i32,
    /// Connector name ("DP-1") or "monitor-N"; selects `[monitors."<name>"]`.
    pub name: String,
    pub width: u32,
    pub height: u32,
}

impl std::str::FromStr for OutputSpec {
    type Err = String;

    /// `INDEX:WIDTHxHEIGHT[:NAME]`, e.g. `0:3840x2160:DP-1`.
    fn from_str(s: &str) -> Result<Self, String> {
        let mut parts = s.splitn(3, ':');
        let bad = || format!("bad output '{s}', expected INDEX:WIDTHxHEIGHT[:NAME]");
        let index: i32 = parts.next().and_then(|x| x.parse().ok()).ok_or_else(bad)?;
        let (w, h) = parts.next().and_then(|x| x.split_once('x')).ok_or_else(bad)?;
        let (width, height) = (w.parse().map_err(|_| bad())?, h.parse().map_err(|_| bad())?);
        let name = parts.next().filter(|n| !n.is_empty()).map(str::to_string).unwrap_or_else(|| format!("monitor-{index}"));
        Ok(Self { index, name, width, height })
    }
}

pub struct Options {
    pub config_path: Option<PathBuf>,
    pub outputs: Vec<OutputSpec>,
    /// Wallpaper mode: borderless windows, stdin control channel.
    pub wallpaper: bool,
    pub warmup: f64,
    pub speed: Option<f64>,
    /// Collector socket, or None for synthetic data.
    pub collector: Option<PathBuf>,
    /// "auto" found no collector and fell back to synthetic data; this socket
    /// is probed periodically in wallpaper mode.
    pub fell_back: Option<PathBuf>,
}

/// Size of a monitor's wallpaper window. On the discrete GPU it is the
/// monitor's size: the scene is drawn smaller and upscaled by the renderer,
/// with text at full resolution. On an integrated GPU the window itself is
/// small and GNOME scales it up, which saves the most memory bandwidth.
pub fn wallpaper_size(width: u32, height: u32, perf: &Performance, integrated: bool) -> (u32, u32) {
    if !integrated {
        return (width, height);
    }
    let (scale, max_megapixels) = perf.resolution(true);
    let area = width as f64 * height as f64;
    let s = (scale as f64).clamp(0.2, 1.0).min((max_megapixels.max(0.1) as f64 * 1.0e6 / area).sqrt());
    (((width as f64 * s).round() as u32).max(16), ((height as f64 * s).round() as u32).max(16))
}

/// `scene_scale` with the monitor's own `render_scale`, if it has one.
pub fn monitor_scale(cfg: &Config, connector: &str, wallpaper: bool, integrated: bool) -> (f32, f32) {
    let (scale, mp) = scene_scale(&cfg.performance, wallpaper, integrated);
    match cfg.monitor(connector).render_scale {
        Some(s) if !(wallpaper && integrated) => (s.clamp(0.2, 1.0), mp),
        _ => (scale, mp),
    }
}

/// Scene resolution (scale, megapixel cap) relative to a view's window.
pub fn scene_scale(perf: &Performance, wallpaper: bool, integrated: bool) -> (f32, f32) {
    // Integrated wallpaper windows are already small (see wallpaper_size).
    if wallpaper && integrated { (1.0, f32::INFINITY) } else { perf.resolution(integrated) }
}

/// The monitor that gets the system readout with `show = "main"`: the largest
/// one showing the wide shot, else the largest.
pub fn main_output(sizes: &[(u32, u32)], compositions: &[Composition]) -> usize {
    let area = |i: usize| sizes[i].0 as u64 * sizes[i].1 as u64;
    let wide = (0..sizes.len()).filter(|&i| compositions.get(i) == Some(&Composition::Wide)).max_by_key(|&i| area(i));
    wide.or_else(|| (0..sizes.len()).max_by_key(|&i| area(i))).unwrap_or(0)
}

/// Control lines from the GNOME extension (wallpaper mode, stdin).
#[derive(Debug)]
pub enum Control {
    /// `visible INDEX 0|1`: whether the monitor's wallpaper can be seen
    /// (older extensions).
    Visible(i32, bool),
    /// `coverage INDEX FRACTION`: share of the monitor's work area covered by
    /// windows (0 in the overview). Paused at `[performance] pause_when_covered`.
    Coverage(i32, f32),
    /// `locked 0|1`: screen locked (nothing drawn).
    Locked(bool),
    /// `sharing 0|1`: a screencast or screen share is running.
    Sharing(bool),
    /// stdin closed or `quit`.
    Quit,
}

fn parse_control(line: &str) -> Option<Control> {
    let mut it = line.split_whitespace();
    let flag = |s: Option<&str>| s.map(|v| v == "1");
    match it.next()? {
        "visible" => Some(Control::Visible(it.next()?.parse().ok()?, flag(it.next())?)),
        "coverage" => Some(Control::Coverage(it.next()?.parse().ok()?, it.next()?.parse::<f32>().ok()?.clamp(0.0, 1.0))),
        "locked" => Some(Control::Locked(flag(it.next())?)),
        "sharing" => Some(Control::Sharing(flag(it.next())?)),
        "quit" => Some(Control::Quit),
        _ => None,
    }
}

fn spawn_stdin_reader(proxy: EventLoopProxy<Control>) {
    let spawned = std::thread::Builder::new().name("control".into()).spawn(move || {
        for line in std::io::stdin().lock().lines() {
            let Ok(line) = line else { break };
            match parse_control(&line) {
                Some(c) => {
                    if proxy.send_event(c).is_err() {
                        return;
                    }
                }
                None => log::warn!("unknown control line: {line:?}"),
            }
        }
        let _ = proxy.send_event(Control::Quit);
    });
    if let Err(e) = spawned {
        log::error!("control thread: {e}");
    }
}

struct Output {
    spec: OutputSpec,
    composition: Composition,
    window: Arc<Window>,
    surface: wgpu::Surface<'static>,
    surface_cfg: wgpu::SurfaceConfiguration,
    view: View,
    /// Reported by the extension (covered by windows = not visible).
    visible: bool,
    last_draw: Instant,
    next_frame: Instant,
    frames: u32,
    stats_since: Instant,
    /// Longest gap between presented frames since `stats_since` (judder check).
    worst_gap: Duration,
    /// Save the next presented frame (see `capture_request`).
    capture: bool,
    /// Share of the monitor covered by windows, and whether that is enough
    /// to pause it (`[performance] pause_when_covered`).
    coverage: f32,
    covered: bool,
}

impl Output {
    fn paused(&self, locked: bool) -> bool {
        locked || !self.visible || self.covered
    }

    fn set_coverage(&mut self, coverage: f32, threshold: f32) {
        self.coverage = coverage;
        let covered = coverage >= threshold.clamp(0.05, 1.0) - 1e-4;
        if covered != self.covered {
            log::info!("{}: {} ({:.0}% covered by windows)", self.spec.name, if covered { "paused" } else { "resumed" }, coverage * 100.0);
            self.covered = covered;
            if !covered {
                self.next_frame = Instant::now();
            }
        }
    }
}

struct State {
    /// Rendering on an integrated GPU (lower resolution profile).
    integrated: bool,
    gpu: Gpu,
    renderer: Renderer,
    world: World,
    outputs: Vec<Output>,
    last_step: Instant,
    sim_accum: f64,
}

struct App {
    opts: Options,
    cfg: Config,
    cfg_mtime: Option<SystemTime>,
    last_cfg_check: Instant,
    next_probe: Instant,
    next_power_check: Instant,
    battery: bool,
    locked: bool,
    state: Option<State>,
    error: Option<anyhow::Error>,
    exit_code: i32,
}

/// Returns the process exit code (0, or [`EXIT_RESTART`]).
pub fn run(cfg: Config, opts: Options) -> Result<i32> {
    anyhow::ensure!(!opts.outputs.is_empty(), "no outputs");
    let event_loop = EventLoop::<Control>::with_user_event().build().context("creating event loop")?;
    if opts.wallpaper {
        spawn_stdin_reader(event_loop.create_proxy());
    }
    let cfg_mtime = opts.config_path.as_ref().and_then(|p| std::fs::metadata(p).and_then(|m| m.modified()).ok());
    let mut app = App {
        opts,
        cfg,
        cfg_mtime,
        last_cfg_check: Instant::now(),
        next_probe: Instant::now() + COLLECTOR_PROBE,
        next_power_check: Instant::now(),
        battery: on_battery(),
        locked: false,
        state: None,
        error: None,
        exit_code: 0,
    };
    event_loop.run_app(&mut app)?;
    match app.error {
        Some(e) => Err(e),
        None => Ok(app.exit_code),
    }
}

impl App {
    /// Composition per output: `[monitors."<connector>"]`, then `[monitors."*"]`;
    /// "auto" picks by shape (see `Composition::auto`).
    fn compositions(&self) -> Vec<Composition> {
        let sizes: Vec<(u32, u32)> = self.opts.outputs.iter().map(|o| (o.width, o.height)).collect();
        let auto = Composition::auto(&sizes);
        self.opts.outputs.iter().zip(auto).map(|(o, a)| Composition::parse(&self.cfg.monitor(&o.name).composition).unwrap_or(a)).collect()
    }

    fn wallpaper_size(&self, spec: &OutputSpec, integrated: bool) -> (u32, u32) {
        wallpaper_size(spec.width, spec.height, &self.cfg.performance, integrated)
    }

    /// Scene render scale for a view.
    fn render_scale(&self, integrated: bool, connector: &str) -> (f32, f32) {
        monitor_scale(&self.cfg, connector, self.opts.wallpaper, integrated)
    }

    fn target_fps(&self) -> u32 {
        let p = &self.cfg.performance;
        (if self.battery { p.fps.min(p.fps_battery) } else { p.fps }).max(1)
    }

    fn init(&mut self, el: &ActiveEventLoop) -> Result<()> {
        let want_discrete = self.cfg.performance.wants_discrete(self.battery) && gpu::has_discrete_gpu();
        let mut windows = Vec::new();
        for spec in &self.opts.outputs {
            let mut attrs = Window::default_attributes();
            if self.opts.wallpaper {
                let (w, h) = self.wallpaper_size(spec, !want_discrete);
                attrs = attrs
                    .with_title(format!("{WALLPAPER_TITLE}{}", spec.index))
                    .with_decorations(false)
                    .with_resizable(false)
                    .with_inner_size(LogicalSize::new(w, h));
            } else {
                attrs = attrs.with_title("gits-file-visualizer").with_inner_size(PhysicalSize::new(spec.width, spec.height));
            }
            #[cfg(target_os = "linux")]
            {
                use winit::platform::wayland::WindowAttributesExtWayland;
                attrs = attrs.with_name(APP_ID, "gits-file-visualizer");
            }
            let window = el.create_window(attrs)?;
            if self.opts.wallpaper {
                // Clicks go through to whatever is below (the desktop icons, the
                // background menu): an empty input region. The extension keeps
                // these windows minimized, but if one is ever shown it must not
                // behave like a fullscreen app.
                if let Err(e) = window.set_cursor_hittest(false) {
                    log::warn!("{}: could not make the window click-through: {e}", spec.name);
                }
            }
            windows.push((spec.clone(), Arc::new(window)));
        }
        // Vulkan first; OpenGL only if Vulkan finds nothing usable.
        let mut chosen = None;
        for fallback in [false, true] {
            let instance = gpu::instance(fallback);
            let surfaces: Result<Vec<_>, _> = windows.iter().map(|(_, w)| instance.create_surface(w.clone())).collect();
            let Ok(surfaces) = surfaces else { continue };
            match pollster::block_on(gpu::init(&instance, Some(&surfaces[0]), !want_discrete)) {
                Ok(g) => {
                    chosen = Some((g, surfaces));
                    break;
                }
                Err(e) => log::warn!("{}: {e:#}", if fallback { "OpenGL" } else { "Vulkan" }),
            }
        }
        let (g, surfaces) = chosen.context("no usable GPU (Vulkan or OpenGL)")?;
        let integrated = g.is_integrated();
        if integrated == want_discrete {
            log::warn!("wanted the {} GPU, got {}", if want_discrete { "discrete" } else { "integrated" }, g.adapter.get_info().name);
        }
        let windows: Vec<_> = windows.into_iter().zip(surfaces).map(|((spec, w), s)| (spec, w, s)).collect();
        let caps = windows[0].2.get_capabilities(&g.adapter);
        let format =
            caps.formats.iter().copied().find(|f| f.is_srgb()).or(caps.formats.first().copied()).context("surface has no formats")?;
        let mut renderer = Renderer::new(g.device.clone(), g.queue.clone(), format, crate::renderer::Quality::for_gpu(g.is_integrated()))?;
        if self.cfg.system_monitor.show != "off" {
            renderer.sysmon = Some(crate::sysmon::SysMon::start(g.adapter.get_info().vendor));
        }
        renderer.min_scene_megapixels = if integrated { 0.0 } else { self.cfg.performance.min_scene_megapixels };
        let now = Instant::now();
        let mut outputs = Vec::new();
        let compositions = self.compositions();
        for ((spec, window, surface), composition) in windows.into_iter().zip(compositions) {
            let (scale, max_mp) = self.render_scale(integrated, &spec.name);
            log::info!("{}: composition {composition:?}", spec.name);
            if self.opts.wallpaper && integrated == want_discrete {
                // The GPU differs from the one the window was sized for.
                let (w, h) = self.wallpaper_size(&spec, integrated);
                let _ = window.request_inner_size(LogicalSize::new(w, h));
            }
            let size = window.inner_size();
            let mut surface_cfg = surface
                .get_default_config(&g.adapter, size.width.max(1), size.height.max(1))
                .context("surface not supported by adapter")?;
            surface_cfg.format = format;
            // Allows `capture` to read presented frames back.
            if surface.get_capabilities(&g.adapter).usages.contains(wgpu::TextureUsages::COPY_SRC) {
                surface_cfg.usage |= wgpu::TextureUsages::COPY_SRC;
            }
            // Frames are paced by the compositor's frame callbacks (see about_to_wait);
            // mailbox presentation never blocks the other windows.
            let modes = surface.get_capabilities(&g.adapter).present_modes;
            surface_cfg.present_mode =
                if modes.contains(&wgpu::PresentMode::Mailbox) { wgpu::PresentMode::Mailbox } else { wgpu::PresentMode::Fifo };
            surface_cfg.desired_maximum_frame_latency = 2;
            log::debug!("{}: present mode {:?} (available {modes:?})", spec.name, surface_cfg.present_mode);
            surface.configure(&g.device, &surface_cfg);
            let mut view = renderer.new_view();
            renderer.resize(&mut view, surface_cfg.width, surface_cfg.height, scale, max_mp);
            outputs.push(Output {
                composition,
                spec,
                window,
                surface,
                surface_cfg,
                view,
                visible: true,
                last_draw: now,
                next_frame: now,
                frames: 0,
                stats_since: now,
                worst_gap: Duration::ZERO,
                capture: false,
                coverage: 0.0,
                covered: false,
            });
        }

        set_system_monitor(&mut outputs, &self.cfg);
        let mut world = World::new(self.cfg.clone(), renderer.max_rings(), self.opts.collector.clone());
        if self.opts.fell_back.is_some() && !self.opts.wallpaper {
            world.notice = Some(Notice {
                text: "gits-collector is not running: showing synthetic data".into(),
                error: false,
                until: self.opts.warmup + 10.0,
            });
        }
        let steps = if world.is_synthetic() { (self.opts.warmup * 24.0) as usize } else { 0 };
        for _ in 0..steps {
            world.step(SIM_DT);
        }
        log::info!(
            "{} output(s): {}",
            outputs.len(),
            outputs.iter().map(|o| format!("{} {}x{}", o.spec.name, o.spec.width, o.spec.height)).collect::<Vec<_>>().join(", ")
        );
        self.state = Some(State { integrated, gpu: g, renderer, world, outputs, last_step: now, sim_accum: 0.0 });
        Ok(())
    }

    fn check_config(&mut self) {
        if self.last_cfg_check.elapsed() < Duration::from_secs(1) {
            return;
        }
        self.last_cfg_check = Instant::now();
        // Diagnostics: `touch $XDG_RUNTIME_DIR/gits-file-visualizer/capture` saves
        // the next frame each monitor presents next to it (capture-<connector>.png).
        let trigger = capture_dir().join("capture");
        if std::fs::remove_file(&trigger).is_ok()
            && let Some(s) = &mut self.state
        {
            log::info!("capture requested");
            for o in &mut s.outputs {
                o.capture = true;
                o.next_frame = Instant::now();
            }
        }
        let Some(path) = self.opts.config_path.clone() else { return };
        let mtime = std::fs::metadata(&path).and_then(|m| m.modified()).ok();
        if mtime == self.cfg_mtime {
            return;
        }
        self.cfg_mtime = mtime;
        match Config::load(Some(&path)) {
            Ok(cfg) => {
                log::info!("reloaded {}", path.display());
                self.cfg = cfg.clone();
                let Some(s) = &mut self.state else { return };
                s.world.set_config(cfg);
                s.world.notice =
                    Some(Notice { text: format!("config reloaded: {}", path.display()), error: false, until: s.world.t + 3.0 });
                s.renderer.min_scene_megapixels = if s.integrated { 0.0 } else { self.cfg.performance.min_scene_megapixels };
                set_system_monitor(&mut s.outputs, &self.cfg);
                for o in &mut s.outputs {
                    o.set_coverage(o.coverage, self.cfg.performance.pause_when_covered);
                }
                if self.cfg.system_monitor.show != "off" && s.renderer.sysmon.is_none() {
                    s.renderer.sysmon = Some(crate::sysmon::SysMon::start(s.gpu.adapter.get_info().vendor));
                }
                for o in &mut s.outputs {
                    let (scale, max_mp) = monitor_scale(&self.cfg, &o.spec.name, self.opts.wallpaper, s.integrated);
                    o.view.invalidate_background();
                    s.renderer.resize(&mut o.view, o.surface_cfg.width, o.surface_cfg.height, scale, max_mp);
                }
            }
            Err(e) => {
                log::error!("config not reloaded: {e:#}");
                if let Some(s) = &mut self.state {
                    let msg = format!("config error (keeping previous settings): {e:#}");
                    let text = msg.split_whitespace().collect::<Vec<_>>().join(" ");
                    s.world.notice = Some(Notice { text, error: true, until: f64::INFINITY });
                }
            }
        }
    }

    /// Wallpaper mode started on synthetic data because no collector was
    /// running (e.g. at login): switch over once it appears.
    fn probe_collector(&mut self) {
        if !self.opts.wallpaper || Instant::now() < self.next_probe {
            return;
        }
        self.next_probe = Instant::now() + COLLECTOR_PROBE;
        let Some(socket) = self.opts.fell_back.clone() else { return };
        let Some(s) = &mut self.state else { return };
        if s.world.is_synthetic() && crate::source::collector_available(&socket) {
            log::info!("gits-collector appeared at {}; switching to live data", socket.display());
            s.world.switch_to_collector(socket);
            self.opts.fell_back = None;
        }
    }

    /// Advances the simulation to the current time. Returns the real time elapsed.
    fn advance(&mut self) -> f64 {
        let Some(s) = &mut self.state else { return 0.0 };
        // Live data runs in real time; the synthetic session can be sped up.
        let speed = if s.world.is_synthetic() { self.opts.speed.unwrap_or(self.cfg.synthetic.speed as f64) } else { 1.0 };
        let now = Instant::now();
        let real_dt = now.duration_since(s.last_step).as_secs_f64();
        s.last_step = now;
        // Everything on screen advances by the real time elapsed, every frame
        // (the synthetic session keeps its own fixed steps inside World::step).
        // Long gaps (hidden, suspend) are applied in chunks of at most 0.5 s.
        s.sim_accum += real_dt * speed;
        while s.sim_accum > 1e-6 {
            let dt = s.sim_accum.min(0.5);
            s.world.step(dt);
            s.sim_accum -= dt;
        }
        real_dt
    }

    fn frame(&mut self, id: WindowId) {
        let locked = self.locked;
        let Some(s) = &mut self.state else { return };
        let Some(i) = s.outputs.iter().position(|o| o.window.id() == id) else { return };
        if s.outputs[i].paused(locked) {
            return;
        }
        s.world.hidden = false;
        let real_dt = if s.last_step.elapsed() >= MIN_STEP { self.advance() } else { 0.0 };
        let Some(s) = &mut self.state else { return };
        let o = &mut s.outputs[i];
        let frame_dt = o.last_draw.elapsed();

        let tex = match o.surface.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(t) | wgpu::CurrentSurfaceTexture::Suboptimal(t) => t,
            wgpu::CurrentSurfaceTexture::Outdated | wgpu::CurrentSurfaceTexture::Lost => {
                o.surface.configure(&s.gpu.device, &o.surface_cfg);
                return;
            }
            _ => return,
        };
        let out = tex.texture.create_view(&wgpu::TextureViewDescriptor::default());
        let aspect = o.surface_cfg.width as f32 / o.surface_cfg.height.max(1) as f32;
        let cam = Camera::new(o.composition, aspect, &s.world.cfg, s.world.y_top(), s.world.t as f32);
        let _ = real_dt;
        s.renderer.render(&mut o.view, &mut s.world, &cam, &out, frame_dt.as_secs_f32());
        if std::mem::take(&mut o.capture) {
            let path = capture_dir().join(format!("capture-{}.png", o.spec.name));
            let (w, h) = (o.surface_cfg.width, o.surface_cfg.height);
            match crate::snapshot::save_png(&s.gpu.device, &s.gpu.queue, &tex.texture, w, h, &path) {
                Ok(()) => log::info!("{}: saved {w}x{h} frame to {}", o.spec.name, path.display()),
                Err(e) => log::warn!("{}: capture failed: {e:#}", o.spec.name),
            }
        }
        o.window.pre_present_notify();
        s.gpu.queue.present(tex);
        o.last_draw = Instant::now();
        // Next redraw request goes out as soon as it is due (see about_to_wait).
        o.next_frame = o.last_draw;
        if frame_dt < HIDDEN_STEP {
            o.worst_gap = o.worst_gap.max(frame_dt);
        }
        o.frames += 1;
        let elapsed = o.stats_since.elapsed().as_secs_f64();
        if elapsed >= 5.0 {
            log::debug!(
                "{}: {:.1} fps (longest gap {:.1} ms), sim t={:.0}s activity={:.1}/s rings={}",
                o.spec.name,
                o.frames as f64 / elapsed,
                o.worst_gap.as_secs_f64() * 1e3,
                s.world.t,
                s.world.activity,
                s.world.rings.len(),
            );
            o.frames = 0;
            o.worst_gap = Duration::ZERO;
            o.stats_since = Instant::now();
        }
    }
}

/// Where diagnostics captures go: next to the collector socket.
fn capture_dir() -> PathBuf {
    gits_core::protocol::default_socket_path().parent().map(PathBuf::from).unwrap_or_else(std::env::temp_dir)
}

/// Which views draw the system readout (`[system_monitor] show`).
fn set_system_monitor(outputs: &mut [Output], cfg: &Config) {
    let sizes: Vec<(u32, u32)> = outputs.iter().map(|o| (o.spec.width, o.spec.height)).collect();
    let comps: Vec<Composition> = outputs.iter().map(|o| o.composition).collect();
    let main = main_output(&sizes, &comps);
    for (i, o) in outputs.iter_mut().enumerate() {
        o.view.system_monitor = match cfg.system_monitor.show.as_str() {
            "all" => true,
            "off" => false,
            _ => i == main,
        };
    }
}

impl ApplicationHandler<Control> for App {
    fn resumed(&mut self, el: &ActiveEventLoop) {
        if self.state.is_none()
            && let Err(e) = self.init(el)
        {
            self.error = Some(e);
            el.exit();
        }
    }

    fn user_event(&mut self, el: &ActiveEventLoop, c: Control) {
        log::debug!("control: {c:?}");
        match c {
            Control::Quit => el.exit(),
            Control::Locked(l) => self.locked = l,
            Control::Sharing(on) => {
                if let Some(s) = &mut self.state {
                    s.world.sharing = on;
                }
            }
            Control::Visible(index, v) => {
                if let Some(s) = &mut self.state
                    && let Some(o) = s.outputs.iter_mut().find(|o| o.spec.index == index)
                {
                    o.visible = v;
                    if v {
                        o.next_frame = Instant::now();
                    }
                }
            }
            Control::Coverage(index, f) => {
                let threshold = self.cfg.performance.pause_when_covered;
                if let Some(s) = &mut self.state
                    && let Some(o) = s.outputs.iter_mut().find(|o| o.spec.index == index)
                {
                    o.set_coverage(f, threshold);
                }
            }
        }
    }

    fn window_event(&mut self, el: &ActiveEventLoop, id: WindowId, event: WindowEvent) {
        match event {
            WindowEvent::CloseRequested => el.exit(),
            WindowEvent::KeyboardInput { event: KeyEvent { logical_key, state: ElementState::Pressed, .. }, .. } => {
                let quit = matches!(logical_key, Key::Named(NamedKey::Escape)) || logical_key == Key::Character("q".into());
                if quit && !self.opts.wallpaper {
                    el.exit();
                }
            }
            WindowEvent::Resized(size) => {
                let wallpaper = self.opts.wallpaper;
                let cfg = self.cfg.clone();
                if let Some(s) = &mut self.state
                    && let Some(o) = s.outputs.iter_mut().find(|o| o.window.id() == id)
                    && size.width > 0
                    && size.height > 0
                {
                    log::info!("{}: window resized to {}x{}", o.spec.name, size.width, size.height);
                    o.surface_cfg.width = size.width;
                    o.surface_cfg.height = size.height;
                    o.surface.configure(&s.gpu.device, &o.surface_cfg);
                    let (scale, max_mp) = monitor_scale(&cfg, &o.spec.name, wallpaper, s.integrated);
                    s.renderer.resize(&mut o.view, size.width, size.height, scale, max_mp);
                    o.view.invalidate_background();
                    o.window.request_redraw();
                }
            }
            WindowEvent::RedrawRequested => self.frame(id),
            WindowEvent::Focused(true) if self.opts.wallpaper => {
                // A wallpaper window only gets focus if it is on screen as a
                // normal window: ask to be minimized again (the wallpaper is
                // shown through the extension's copies).
                if let Some(s) = &self.state
                    && let Some(o) = s.outputs.iter().find(|o| o.window.id() == id)
                {
                    log::info!("{}: wallpaper window got focus; minimizing it", o.spec.name);
                    o.window.set_minimized(true);
                }
            }
            _ => {}
        }
    }

    fn about_to_wait(&mut self, el: &ActiveEventLoop) {
        self.check_config();
        self.probe_collector();
        if Instant::now() >= self.next_power_check {
            self.next_power_check = Instant::now() + POWER_CHECK;
            let b = on_battery();
            if b != self.battery {
                log::info!("power: {}", if b { "battery (frame rate capped)" } else { "mains" });
                self.battery = b;
                // gpu = "auto": switch between discrete and integrated via a restart.
                if let Some(s) = &self.state
                    && self.opts.wallpaper
                    && gpu::has_discrete_gpu()
                    && self.cfg.performance.wants_discrete(b) == s.integrated
                {
                    log::info!("power source changed: restarting to switch GPU");
                    self.exit_code = EXIT_RESTART;
                    el.exit();
                    return;
                }
            }
        }
        let interval = Duration::from_secs_f64(1.0 / self.target_fps() as f64);
        let locked = self.locked;
        let Some(s) = &mut self.state else { return };
        let now = Instant::now();
        // Nothing drawn for a while (paused, or the compositor withholds frames).
        let all_hidden = s.outputs.iter().all(|o| o.paused(locked) || now.duration_since(o.last_draw) > HIDDEN_STEP);
        s.world.hidden = all_hidden;
        if all_hidden && now.duration_since(s.last_step) >= HIDDEN_STEP {
            self.advance();
        }
        let Some(s) = &mut self.state else { return };
        if let Some(sm) = &s.renderer.sysmon {
            sm.set_active(s.outputs.iter().any(|o| o.view.system_monitor && !o.paused(locked)));
        }
        let mut wake = now + HIDDEN_STEP;
        for o in &mut s.outputs {
            if o.paused(locked) {
                continue;
            }
            // Ask for the next frame shortly before it is due. Because each frame
            // calls pre_present_notify, winit delivers the redraw on the
            // compositor's next frame callback, i.e. in step with the display.
            // A window the compositor is not showing gets no callbacks and is
            // retried once a second.
            let request_at = (o.last_draw + interval).checked_sub(PACING_SLACK).unwrap_or(o.last_draw);
            if now >= request_at {
                if now >= o.next_frame {
                    o.window.request_redraw();
                    o.next_frame = now + HIDDEN_STEP;
                }
                wake = wake.min(o.next_frame);
            } else {
                wake = wake.min(request_at);
            }
        }
        el.set_control_flow(ControlFlow::WaitUntil(wake));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn output_spec_parses() {
        let o: OutputSpec = "2:1440x2560:DP-3".parse().unwrap();
        assert_eq!((o.index, o.width, o.height, o.name.as_str()), (2, 1440, 2560, "DP-3"));
        let o: OutputSpec = "0:800x600".parse().unwrap();
        assert_eq!(o.name, "monitor-0");
        assert!("x:1x1".parse::<OutputSpec>().is_err());
    }

    #[test]
    fn control_lines_parse() {
        assert!(matches!(parse_control("visible 1 0"), Some(Control::Visible(1, false))));
        assert!(matches!(parse_control("coverage 2 0.87"), Some(Control::Coverage(2, f)) if (f - 0.87).abs() < 1e-6));
        assert!(matches!(parse_control("locked 1"), Some(Control::Locked(true))));
        assert!(matches!(parse_control("sharing 0"), Some(Control::Sharing(false))));
        assert!(parse_control("bogus").is_none());
    }
}
