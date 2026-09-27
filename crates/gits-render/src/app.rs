//! Windowed mode (M0). In M2 the GNOME extension will take this window and
//! place it in the background layer; it is identified by its app id.

use crate::camera::Camera;
use crate::gpu::{self, Gpu};
use crate::renderer::Renderer;
use crate::world::{Notice, World};
use anyhow::{Context, Result};
use gits_core::config::Config;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};
use winit::application::ApplicationHandler;
use winit::dpi::PhysicalSize;
use winit::event::{ElementState, KeyEvent, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::keyboard::{Key, NamedKey};
use winit::window::{Window, WindowId};

/// Fixed simulation step, identical to headless snapshots so runs are reproducible.
const SIM_DT: f64 = 1.0 / 24.0;

pub const APP_ID: &str = "io.github.iliabaranov.GitsFileVisualizer";

pub struct Options {
    pub config_path: Option<PathBuf>,
    pub width: u32,
    pub height: u32,
    pub warmup: f64,
    pub speed: Option<f64>,
}

struct State {
    window: Arc<Window>,
    surface: wgpu::Surface<'static>,
    surface_cfg: wgpu::SurfaceConfiguration,
    gpu: Gpu,
    renderer: Renderer,
    world: World,
    last_frame: Instant,
    next_frame: Instant,
    frames: u32,
    stats_since: Instant,
    sim_accum: f64,
}

struct App {
    opts: Options,
    cfg: Config,
    cfg_mtime: Option<SystemTime>,
    last_cfg_check: Instant,
    state: Option<State>,
    error: Option<anyhow::Error>,
}

pub fn run(cfg: Config, opts: Options) -> Result<()> {
    let event_loop = EventLoop::new().context("creating event loop")?;
    let cfg_mtime = opts.config_path.as_ref().and_then(|p| std::fs::metadata(p).and_then(|m| m.modified()).ok());
    let mut app = App { opts, cfg, cfg_mtime, last_cfg_check: Instant::now(), state: None, error: None };
    event_loop.run_app(&mut app)?;
    match app.error {
        Some(e) => Err(e),
        None => Ok(()),
    }
}

impl App {
    fn init(&mut self, el: &ActiveEventLoop) -> Result<()> {
        let mut attrs = Window::default_attributes()
            .with_title("gits-file-visualizer")
            .with_inner_size(PhysicalSize::new(self.opts.width, self.opts.height));
        #[cfg(target_os = "linux")]
        {
            use winit::platform::wayland::WindowAttributesExtWayland;
            attrs = attrs.with_name(APP_ID, "gits-file-visualizer");
        }
        let window = Arc::new(el.create_window(attrs)?);
        let instance = gpu::instance();
        let surface = instance.create_surface(window.clone())?;
        let g = pollster::block_on(gpu::init(&instance, Some(&surface), self.cfg.performance.prefer_igpu))?;
        let caps = surface.get_capabilities(&g.adapter);
        let format =
            caps.formats.iter().copied().find(|f| f.is_srgb()).or(caps.formats.first().copied()).context("surface has no formats")?;
        let size = window.inner_size();
        let mut surface_cfg =
            surface.get_default_config(&g.adapter, size.width.max(1), size.height.max(1)).context("surface not supported by adapter")?;
        surface_cfg.format = format;
        surface_cfg.present_mode = wgpu::PresentMode::Fifo;
        surface_cfg.desired_maximum_frame_latency = 2;
        surface.configure(&g.device, &surface_cfg);

        let mut renderer = Renderer::new(g.device.clone(), g.queue.clone(), format)?;
        renderer.resize(
            surface_cfg.width,
            surface_cfg.height,
            self.cfg.performance.render_scale,
            self.cfg.performance.max_internal_megapixels,
        );
        let mut world = World::new(self.cfg.clone(), renderer.max_rings());
        let steps = (self.opts.warmup * 24.0) as usize;
        for _ in 0..steps {
            world.step(1.0 / 24.0);
        }
        let now = Instant::now();
        self.state = Some(State {
            window,
            surface,
            surface_cfg,
            gpu: g,
            renderer,
            world,
            last_frame: now,
            next_frame: now,
            frames: 0,
            stats_since: now,
            sim_accum: 0.0,
        });
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
                if let Some(s) = &mut self.state {
                    s.world.set_config(cfg);
                    s.world.notice =
                        Some(Notice { text: format!("config reloaded: {}", path.display()), error: false, until: s.world.t + 3.0 });
                    s.renderer.invalidate_background();
                    s.renderer.resize(
                        s.surface_cfg.width,
                        s.surface_cfg.height,
                        self.cfg.performance.render_scale,
                        self.cfg.performance.max_internal_megapixels,
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

    fn frame(&mut self) {
        let speed = self.opts.speed.unwrap_or(self.cfg.synthetic.speed as f64);
        let Some(s) = &mut self.state else { return };
        let now = Instant::now();
        let real_dt = now.duration_since(s.last_frame).as_secs_f64();
        s.last_frame = now;
        s.sim_accum += real_dt * speed;
        let mut steps = 0;
        while s.sim_accum >= SIM_DT && steps < 48 {
            s.world.step(SIM_DT);
            s.sim_accum -= SIM_DT;
            steps += 1;
        }
        if steps == 48 {
            // Fell far behind (e.g. after suspend): skip ahead instead of catching up.
            s.sim_accum = 0.0;
        }

        let tex = match s.surface.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(t) | wgpu::CurrentSurfaceTexture::Suboptimal(t) => t,
            wgpu::CurrentSurfaceTexture::Outdated | wgpu::CurrentSurfaceTexture::Lost => {
                s.surface.configure(&s.gpu.device, &s.surface_cfg);
                return;
            }
            _ => return,
        };
        let view = tex.texture.create_view(&wgpu::TextureViewDescriptor::default());
        let aspect = s.surface_cfg.width as f32 / s.surface_cfg.height.max(1) as f32;
        let cam = Camera::wide(aspect, s.world.y_top(), s.world.cfg.camera.drift, s.world.t as f32);
        s.renderer.render(&mut s.world, &cam, &view, real_dt as f32);
        s.window.pre_present_notify();
        s.gpu.queue.present(tex);
        s.frames += 1;
        let elapsed = s.stats_since.elapsed().as_secs_f64();
        if elapsed >= 5.0 {
            log::debug!(
                "{:.1} fps, sim t={:.0}s phase={} activity={:.1}/s rings={}",
                s.frames as f64 / elapsed,
                s.world.t,
                s.world.phase_name(),
                s.world.activity,
                s.world.rings.len()
            );
            s.frames = 0;
            s.stats_since = Instant::now();
        }
    }
}

impl ApplicationHandler for App {
    fn resumed(&mut self, el: &ActiveEventLoop) {
        if self.state.is_none()
            && let Err(e) = self.init(el)
        {
            self.error = Some(e);
            el.exit();
        }
    }

    fn window_event(&mut self, el: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        match event {
            WindowEvent::CloseRequested => el.exit(),
            WindowEvent::KeyboardInput { event: KeyEvent { logical_key, state: ElementState::Pressed, .. }, .. } => {
                if matches!(logical_key, Key::Named(NamedKey::Escape)) || logical_key == Key::Character("q".into()) {
                    el.exit();
                }
            }
            WindowEvent::Resized(size) => {
                if let Some(s) = &mut self.state
                    && size.width > 0
                    && size.height > 0
                {
                    s.surface_cfg.width = size.width;
                    s.surface_cfg.height = size.height;
                    s.surface.configure(&s.gpu.device, &s.surface_cfg);
                    s.renderer.resize(
                        size.width,
                        size.height,
                        self.cfg.performance.render_scale,
                        self.cfg.performance.max_internal_megapixels,
                    );
                    s.window.request_redraw();
                }
            }
            WindowEvent::RedrawRequested => self.frame(),
            _ => {}
        }
    }

    fn about_to_wait(&mut self, el: &ActiveEventLoop) {
        self.check_config();
        let perf = self.cfg.performance.clone();
        let Some(s) = &mut self.state else { return };
        let busy = s.world.activity > 0.5 || s.world.animating || s.world.fx.alive();
        let fps = if busy { perf.fps_active } else { perf.fps_calm }.max(1);
        let now = Instant::now();
        if now >= s.next_frame {
            s.window.request_redraw();
            s.next_frame = now + Duration::from_secs_f64(1.0 / fps as f64);
        }
        el.set_control_flow(ControlFlow::WaitUntil(s.next_frame));
    }
}
