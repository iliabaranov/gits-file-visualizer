//! Window and wallpaper modes.
//!
//! - `window`: one normal window, for look-dev.
//! - `wallpaper`: one borderless window per monitor, launched by the GNOME
//!   extension, which clones each window into that monitor's background. The
//!   extension sends control lines on stdin (see [`Control`]); stdin closing
//!   means the extension is gone, and the renderer exits.

use crate::camera::Camera;
use crate::gpu::{self, Gpu};
use crate::renderer::{Renderer, View};
use crate::world::{Notice, World};
use anyhow::{Context, Result};
use gits_core::config::Config;
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

/// Fixed simulation step, identical to headless snapshots so runs are reproducible.
const SIM_DT: f64 = 1.0 / 24.0;
/// While nothing can draw (hidden, covered, locked), the simulation still
/// advances this often so collector updates are applied at their real time
/// instead of piling up.
const HIDDEN_STEP: Duration = Duration::from_secs(1);
/// How often wallpaper mode looks for a collector that was not running at start.
const COLLECTOR_PROBE: Duration = Duration::from_secs(10);

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

/// Control lines from the GNOME extension (wallpaper mode, stdin).
#[derive(Debug)]
pub enum Control {
    /// `visible INDEX 0|1`: whether the monitor's wallpaper can be seen.
    Visible(i32, bool),
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
}

impl Output {
    fn paused(&self, locked: bool) -> bool {
        locked || !self.visible
    }
}

struct State {
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
    locked: bool,
    state: Option<State>,
    error: Option<anyhow::Error>,
}

pub fn run(cfg: Config, opts: Options) -> Result<()> {
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
        locked: false,
        state: None,
        error: None,
    };
    event_loop.run_app(&mut app)?;
    match app.error {
        Some(e) => Err(e),
        None => Ok(()),
    }
}

impl App {
    fn composition_warned(&self, spec: &OutputSpec) {
        let c = self.cfg.monitor(&spec.name).composition;
        if c != "wide" {
            log::warn!("{}: composition '{c}' is not implemented yet; using 'wide'", spec.name);
        }
    }

    fn init(&mut self, el: &ActiveEventLoop) -> Result<()> {
        let instance = gpu::instance();
        let mut windows = Vec::new();
        for spec in &self.opts.outputs {
            let mut attrs = Window::default_attributes();
            if self.opts.wallpaper {
                attrs = attrs
                    .with_title(format!("{WALLPAPER_TITLE}{}", spec.index))
                    .with_decorations(false)
                    .with_inner_size(LogicalSize::new(spec.width, spec.height));
            } else {
                attrs = attrs.with_title("gits-file-visualizer").with_inner_size(PhysicalSize::new(spec.width, spec.height));
            }
            #[cfg(target_os = "linux")]
            {
                use winit::platform::wayland::WindowAttributesExtWayland;
                attrs = attrs.with_name(APP_ID, "gits-file-visualizer");
            }
            let window = Arc::new(el.create_window(attrs)?);
            let surface = instance.create_surface(window.clone())?;
            windows.push((spec.clone(), window, surface));
        }
        let g = pollster::block_on(gpu::init(&instance, Some(&windows[0].2), self.cfg.performance.prefer_igpu))?;
        let caps = windows[0].2.get_capabilities(&g.adapter);
        let format =
            caps.formats.iter().copied().find(|f| f.is_srgb()).or(caps.formats.first().copied()).context("surface has no formats")?;
        let renderer = Renderer::new(g.device.clone(), g.queue.clone(), format)?;
        let now = Instant::now();
        let mut outputs = Vec::new();
        for (spec, window, surface) in windows {
            self.composition_warned(&spec);
            let size = window.inner_size();
            let mut surface_cfg = surface
                .get_default_config(&g.adapter, size.width.max(1), size.height.max(1))
                .context("surface not supported by adapter")?;
            surface_cfg.format = format;
            surface_cfg.present_mode = wgpu::PresentMode::Fifo;
            surface_cfg.desired_maximum_frame_latency = 2;
            surface.configure(&g.device, &surface_cfg);
            let mut view = renderer.new_view();
            renderer.resize(
                &mut view,
                surface_cfg.width,
                surface_cfg.height,
                self.cfg.performance.render_scale,
                self.cfg.performance.max_internal_megapixels,
            );
            outputs.push(Output {
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
            });
        }

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
        self.state = Some(State { gpu: g, renderer, world, outputs, last_step: now, sim_accum: 0.0 });
        Ok(())
    }

    fn check_config(&mut self) {
        if self.last_cfg_check.elapsed() < Duration::from_secs(1) {
            return;
        }
        self.last_cfg_check = Instant::now();
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
                let perf = &self.cfg.performance;
                for o in &mut s.outputs {
                    o.view.invalidate_background();
                    s.renderer.resize(
                        &mut o.view,
                        o.surface_cfg.width,
                        o.surface_cfg.height,
                        perf.render_scale,
                        perf.max_internal_megapixels,
                    );
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
        s.sim_accum += real_dt * speed;
        let mut steps = 0;
        while s.sim_accum >= SIM_DT && steps < 48 {
            s.world.step(SIM_DT);
            s.sim_accum -= SIM_DT;
            steps += 1;
        }
        if steps == 48 && s.sim_accum >= SIM_DT {
            // Fell far behind (e.g. after suspend): jump ahead in one step.
            let rest = s.sim_accum - s.sim_accum % SIM_DT;
            s.world.step(rest);
            s.sim_accum -= rest;
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
        let real_dt = self.advance();
        let Some(s) = &mut self.state else { return };
        let o = &mut s.outputs[i];

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
        let cam = Camera::wide(aspect, s.world.y_top(), s.world.cfg.camera.drift, s.world.t as f32);
        s.renderer.render(&mut o.view, &mut s.world, &cam, &out, real_dt as f32);
        o.window.pre_present_notify();
        s.gpu.queue.present(tex);
        o.last_draw = Instant::now();
        o.frames += 1;
        let elapsed = o.stats_since.elapsed().as_secs_f64();
        if elapsed >= 5.0 {
            log::debug!(
                "{}: {:.1} fps, sim t={:.0}s phase={} activity={:.1}/s rings={}",
                o.spec.name,
                o.frames as f64 / elapsed,
                s.world.t,
                s.world.phase_name(),
                s.world.activity,
                s.world.rings.len()
            );
            o.frames = 0;
            o.stats_since = Instant::now();
        }
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
                let perf = self.cfg.performance.clone();
                if let Some(s) = &mut self.state
                    && let Some(o) = s.outputs.iter_mut().find(|o| o.window.id() == id)
                    && size.width > 0
                    && size.height > 0
                {
                    log::debug!("{}: resized to {}x{}", o.spec.name, size.width, size.height);
                    o.surface_cfg.width = size.width;
                    o.surface_cfg.height = size.height;
                    o.surface.configure(&s.gpu.device, &o.surface_cfg);
                    s.renderer.resize(&mut o.view, size.width, size.height, perf.render_scale, perf.max_internal_megapixels);
                    o.view.invalidate_background();
                    o.window.request_redraw();
                }
            }
            WindowEvent::RedrawRequested => self.frame(id),
            _ => {}
        }
    }

    fn about_to_wait(&mut self, el: &ActiveEventLoop) {
        self.check_config();
        self.probe_collector();
        let perf = self.cfg.performance.clone();
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
        let busy = s.world.activity > 0.5 || s.world.animating || s.world.fx.alive();
        let fps = if busy { perf.fps_active } else { perf.fps_calm }.max(1);
        let mut wake = now + HIDDEN_STEP;
        for o in &mut s.outputs {
            if o.paused(locked) {
                continue;
            }
            if now >= o.next_frame {
                // Ignored by the compositor while the window cannot be seen.
                o.window.request_redraw();
                let hidden = now.duration_since(o.last_draw) > HIDDEN_STEP;
                o.next_frame = now + if hidden { HIDDEN_STEP } else { Duration::from_secs_f64(1.0 / fps as f64) };
            }
            wake = wake.min(o.next_frame);
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
        assert!(matches!(parse_control("locked 1"), Some(Control::Locked(true))));
        assert!(matches!(parse_control("sharing 0"), Some(Control::Sharing(false))));
        assert!(parse_control("bogus").is_none());
    }
}
