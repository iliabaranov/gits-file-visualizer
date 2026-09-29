//! GPU selection.
//!
//! An always-on wallpaper should use the integrated GPU and leave everything
//! else alone. By default, wgpu initialises every backend and every Vulkan
//! driver: the OpenGL backend drags in Mesa's EGL (falling back to the software
//! rasteriser, with its thread pool and LLVM), Vulkan loads lavapipe (software)
//! and the NVIDIA driver, and opening the NVIDIA device keeps a hybrid laptop's
//! discrete GPU awake. So:
//! - only the Vulkan backend is used (OpenGL is a fallback if Vulkan finds nothing);
//! - before the first Vulkan instance, the driver list is narrowed to drivers
//!   for GPUs that should be used: lavapipe is dropped whenever real GPUs exist,
//!   and when the integrated GPU is wanted NVIDIA's driver is dropped (if an
//!   Intel or AMD GPU is present). `VK_ICD_FILENAMES` / `VK_DRIVER_FILES` set by
//!   the user win.
//! - `[performance] gpu` chooses: "discrete" (high-performance adapter),
//!   "integrated", or "auto" (discrete on mains power, integrated on battery).

use anyhow::{Context, Result};
use std::path::PathBuf;

pub struct Gpu {
    pub adapter: wgpu::Adapter,
    pub device: wgpu::Device,
    pub queue: wgpu::Queue,
}

const VENDOR_INTEL: &str = "0x8086";
const VENDOR_AMD: &str = "0x1002";
const VENDOR_NVIDIA: &str = "0x10de";

fn gpu_vendors() -> Vec<String> {
    let Ok(rd) = std::fs::read_dir("/sys/class/drm") else { return vec![] };
    rd.flatten()
        .filter(|e| e.file_name().to_string_lossy().strip_prefix("card").is_some_and(|n| n.chars().all(|c| c.is_ascii_digit())))
        .filter_map(|e| std::fs::read_to_string(e.path().join("device/vendor")).ok())
        .map(|v| v.trim().to_lowercase())
        .collect()
}

fn icd_dirs() -> Vec<PathBuf> {
    let mut dirs = vec![];
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let config_home = std::env::var_os("XDG_CONFIG_HOME").map(PathBuf::from).or_else(|| home.as_ref().map(|h| h.join(".config")));
    let data_home = std::env::var_os("XDG_DATA_HOME").map(PathBuf::from).or_else(|| home.as_ref().map(|h| h.join(".local/share")));
    dirs.extend(config_home.map(|p| p.join("vulkan/icd.d")));
    let config_dirs = std::env::var("XDG_CONFIG_DIRS").unwrap_or_else(|_| "/etc/xdg".into());
    dirs.extend(config_dirs.split(':').filter(|d| !d.is_empty()).map(|d| PathBuf::from(d).join("vulkan/icd.d")));
    dirs.push(PathBuf::from("/etc/vulkan/icd.d"));
    dirs.extend(data_home.map(|p| p.join("vulkan/icd.d")));
    let data_dirs = std::env::var("XDG_DATA_DIRS").unwrap_or_else(|_| "/usr/local/share:/usr/share".into());
    dirs.extend(data_dirs.split(':').filter(|d| !d.is_empty()).map(|d| PathBuf::from(d).join("vulkan/icd.d")));
    dirs
}

/// Chooses which Vulkan drivers to load (see module docs). Must run before any
/// other thread starts, because it sets an environment variable.
pub fn select_vulkan_drivers(want_discrete: bool) {
    let prefer_igpu = !want_discrete;
    if std::env::var_os("VK_ICD_FILENAMES").is_some() || std::env::var_os("VK_DRIVER_FILES").is_some() {
        log::info!("Vulkan driver list set by the environment; not narrowing it");
        return;
    }
    let vendors = gpu_vendors();
    let has = |v: &str| vendors.iter().any(|x| x == v);
    let real_gpu = has(VENDOR_INTEL) || has(VENDOR_AMD) || has(VENDOR_NVIDIA);
    let drop_nvidia = prefer_igpu && (has(VENDOR_INTEL) || has(VENDOR_AMD));
    if !real_gpu {
        return;
    }
    let mut keep = vec![];
    let mut dropped = vec![];
    let mut seen = std::collections::HashSet::new();
    for dir in icd_dirs() {
        let Ok(rd) = std::fs::read_dir(&dir) else { continue };
        let mut files: Vec<PathBuf> = rd.flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|x| x == "json")).collect();
        files.sort();
        for f in files {
            let name = f.file_name().unwrap_or_default().to_string_lossy().to_lowercase();
            if !seen.insert(name.clone()) {
                continue; // earlier directories take precedence, like the loader
            }
            let wrong_arch = cfg!(target_pointer_width = "64") && (name.contains("i686") || name.contains("i386"));
            let software = name.starts_with("lvp") || name.contains("lavapipe") || name.contains("swiftshader");
            let nvidia = name.starts_with("nvidia");
            if wrong_arch {
                continue;
            }
            if software || (nvidia && drop_nvidia) {
                dropped.push(name);
            } else {
                keep.push(f.to_string_lossy().into_owned());
            }
        }
    }
    if keep.is_empty() {
        return;
    }
    log::info!("Vulkan drivers: skipping {}", if dropped.is_empty() { "none".into() } else { dropped.join(", ") });
    // SAFETY: called at startup before any other thread exists (see doc comment).
    unsafe {
        std::env::set_var("VK_ICD_FILENAMES", keep.join(":"));
    }
}

/// Vulkan-only instance, or OpenGL as a fallback (`WGPU_BACKEND` overrides).
pub fn instance(fallback: bool) -> wgpu::Instance {
    let mut desc = wgpu::InstanceDescriptor::new_without_display_handle();
    desc.backends = if fallback { wgpu::Backends::GL } else { wgpu::Backends::VULKAN };
    wgpu::Instance::new(desc.with_env())
}

async fn adapter(instance: &wgpu::Instance, surface: Option<&wgpu::Surface<'_>>, low_power: bool) -> Option<wgpu::Adapter> {
    instance
        .request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: if low_power { wgpu::PowerPreference::LowPower } else { wgpu::PowerPreference::HighPerformance },
            force_fallback_adapter: false,
            compatible_surface: surface,
            apply_limit_buckets: false,
        })
        .await
        .ok()
}

/// True if the machine has a discrete GPU the Vulkan loader could use.
pub fn has_discrete_gpu() -> bool {
    let v = gpu_vendors();
    v.iter().any(|x| x == VENDOR_NVIDIA) || v.iter().filter(|x| *x == VENDOR_AMD).count() > 1
}

impl Gpu {
    pub fn is_integrated(&self) -> bool {
        !matches!(self.adapter.get_info().device_type, wgpu::DeviceType::DiscreteGpu)
    }
}

/// Picks an adapter (the integrated GPU when `low_power`) and opens a device.
/// If the preferred kind cannot present to `surface`, the other kind is used.
pub async fn init(instance: &wgpu::Instance, surface: Option<&wgpu::Surface<'_>>, low_power: bool) -> Result<Gpu> {
    for a in instance.enumerate_adapters(wgpu::Backends::all()).await {
        let i = a.get_info();
        let ok =
            surface.map(|s| if a.is_surface_supported(s) { "can present" } else { "cannot present to this window" }).unwrap_or("headless");
        log::info!("adapter: {} ({:?}, {:?}): {ok}", i.name, i.backend, i.device_type);
    }
    let adapter = match adapter(instance, surface, low_power).await {
        Some(a) => a,
        None => adapter(instance, surface, !low_power).await.context("no suitable GPU adapter")?,
    };
    let info = adapter.get_info();
    log::info!("GPU: {} ({:?}, {:?}, driver {})", info.name, info.backend, info.device_type, info.driver_info);

    let mut limits = wgpu::Limits::default();
    limits.max_texture_dimension_2d = adapter.limits().max_texture_dimension_2d.min(16384);
    // Smaller HDR render targets where supported (half the memory and bandwidth).
    let mut required_features = adapter.features() & wgpu::Features::RG11B10UFLOAT_RENDERABLE;
    // `bench` with GITS_GPUPROF: per-pass GPU times and tower draw statistics.
    if std::env::var_os("GITS_GPUPROF").is_some() {
        required_features |= adapter.features() & (wgpu::Features::TIMESTAMP_QUERY | wgpu::Features::PIPELINE_STATISTICS_QUERY);
    }
    let (device, queue) = adapter
        .request_device(&wgpu::DeviceDescriptor {
            label: Some("gits"),
            required_features,
            required_limits: limits,
            memory_hints: wgpu::MemoryHints::MemoryUsage,
            ..Default::default()
        })
        .await
        .context("opening GPU device")?;
    Ok(Gpu { adapter, device, queue })
}

/// Headless: Vulkan first, then OpenGL.
pub fn init_headless(low_power: bool) -> Result<(wgpu::Instance, Gpu)> {
    for fallback in [false, true] {
        let inst = instance(fallback);
        if let Ok(g) = pollster::block_on(init(&inst, None, low_power)) {
            return Ok((inst, g));
        }
        log::warn!("no {} adapter", if fallback { "OpenGL" } else { "Vulkan" });
    }
    anyhow::bail!("no usable GPU adapter (Vulkan or OpenGL)")
}

/// `gits-render gpu-check`: which GPUs can present to a window of this display
/// server (the window is never shown).
pub fn check() -> Result<()> {
    use winit::application::ApplicationHandler;
    use winit::event::WindowEvent;
    use winit::event_loop::{ActiveEventLoop, EventLoop};
    use winit::window::{Window, WindowId};
    struct Check(Option<Result<()>>);
    impl ApplicationHandler for Check {
        fn resumed(&mut self, el: &ActiveEventLoop) {
            let run = || -> Result<()> {
                let window = std::sync::Arc::new(el.create_window(Window::default_attributes().with_visible(false))?);
                let display = if std::env::var_os("WAYLAND_DISPLAY").is_some() { "Wayland" } else { "X11" };
                for fallback in [false, true] {
                    let inst = instance(fallback);
                    let surface = inst.create_surface(window.clone())?;
                    for a in pollster::block_on(inst.enumerate_adapters(wgpu::Backends::all())) {
                        let i = a.get_info();
                        let ok = if a.is_surface_supported(&surface) { "can present" } else { "cannot present" };
                        println!("{display}: {} ({:?}, {:?}): {ok}", i.name, i.backend, i.device_type);
                    }
                }
                Ok(())
            };
            self.0 = Some(run());
            el.exit();
        }
        fn window_event(&mut self, _: &ActiveEventLoop, _: WindowId, _: WindowEvent) {}
    }
    let el = EventLoop::new()?;
    let mut c = Check(None);
    el.run_app(&mut c)?;
    c.0.unwrap_or(Ok(()))
}
