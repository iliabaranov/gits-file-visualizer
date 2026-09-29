//! Headless rendering to PNG, for look-dev and regression snapshots.

use crate::camera::{Camera, Composition};
use crate::gpu;
use crate::renderer::Renderer;
use crate::world::World;
use anyhow::{Context, Result};
use gits_core::config::Config;
use std::path::{Path, PathBuf};

pub struct Options {
    pub out: PathBuf,
    pub width: u32,
    pub height: u32,
    pub at: f64,
    pub frames: u32,
    pub every: f64,
    /// None = as configured for "*" ("auto" picks by aspect ratio).
    pub composition: Option<String>,
}

fn composition_for(cfg: &Config, requested: Option<&str>, width: u32, height: u32) -> Composition {
    let name = requested.map(str::to_string).unwrap_or_else(|| cfg.monitor("*").composition);
    Composition::parse(&name).unwrap_or_else(|| Composition::auto(&[(width, height)])[0])
}

const DT: f64 = 1.0 / 24.0;
/// Frames rendered before each capture so motion smear and bloom settle.
const WARM_SECONDS: f64 = 1.5;

/// Steps a live (collector) world in real time until its first snapshot arrives.
fn wait_for_data(world: &mut World) -> Result<()> {
    let t0 = std::time::Instant::now();
    while !world.has_data() {
        anyhow::ensure!(t0.elapsed().as_secs() < 180, "no snapshot from gits-collector within 3 minutes");
        world.step(0.1);
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    // Let the (possibly large) first layout settle.
    world.step(0.5);
    Ok(())
}

/// With live data `--at` is ignored: captures start once the snapshot arrives
/// and are spaced `every` real seconds apart.
pub fn run(cfg: Config, opts: Options, collector: Option<std::path::PathBuf>) -> Result<()> {
    let (_instance, g) = gpu::init_headless(!crate::want_discrete(&cfg))?;
    let format = wgpu::TextureFormat::Rgba8UnormSrgb;
    let mut renderer = Renderer::new(g.device.clone(), g.queue.clone(), format, crate::renderer::Quality::for_gpu(g.is_integrated()))?;
    let mut view = renderer.new_view();
    let (sc, mp) = cfg.performance.resolution(g.is_integrated());
    renderer.resize(&mut view, opts.width, opts.height, sc, mp);
    let composition = composition_for(&cfg, opts.composition.as_deref(), opts.width, opts.height);
    if cfg.system_monitor.show == "all" || (cfg.system_monitor.show == "main" && composition == crate::camera::Composition::Wide) {
        renderer.sysmon = Some(crate::sysmon::SysMon::start(g.adapter.get_info().vendor));
        view.system_monitor = true;
    }
    let mut world = World::new(cfg, renderer.max_rings(), collector);
    let live = !world.is_synthetic();
    if live {
        wait_for_data(&mut world)?;
    }

    let out_tex = g.device.create_texture(&wgpu::TextureDescriptor {
        label: Some("snapshot"),
        size: wgpu::Extent3d { width: opts.width, height: opts.height, depth_or_array_layers: 1 },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    });
    let out_view = out_tex.create_view(&wgpu::TextureViewDescriptor::default());
    if let Some(dir) = opts.out.parent() {
        std::fs::create_dir_all(dir).ok();
    }

    let aspect = opts.width as f32 / opts.height as f32;
    let start_t = world.t;
    for i in 0..opts.frames.max(1) {
        let capture_t = if live { start_t + WARM_SECONDS + i as f64 * opts.every } else { opts.at + i as f64 * opts.every };
        while world.t < capture_t - WARM_SECONDS {
            world.step(DT);
            if live {
                std::thread::sleep(std::time::Duration::from_secs_f64(DT));
            }
        }
        while world.t < capture_t {
            world.step(DT);
            if live {
                std::thread::sleep(std::time::Duration::from_secs_f64(DT));
            }
            let cam = Camera::new(composition, aspect, &world.cfg, world.y_top(), world.t as f32);
            renderer.render(&mut view, &mut world, &cam, &out_view, DT as f32);
            let _ = g.device.poll(wgpu::PollType::Poll);
        }
        let path = if opts.frames <= 1 { opts.out.clone() } else { numbered(&opts.out, i) };
        save_png(&g.device, &g.queue, &out_tex, opts.width, opts.height, &path)?;
        log::info!(
            "t={:.1}s phase={} activity={:.1}/s rings={} → {}",
            world.t,
            world.phase_name(),
            world.activity,
            world.rings.len(),
            path.display()
        );
    }
    Ok(())
}

/// Renders `frames` frames at 24 fps simulated time, waiting for the GPU after
/// each one, and reports CPU (simulation + encoding) and total frame times.
pub fn bench(
    cfg: Config,
    width: u32,
    height: u32,
    at: f64,
    frames: u32,
    collector: Option<std::path::PathBuf>,
    composition: Option<String>,
) -> Result<()> {
    let (_instance, g) = gpu::init_headless(!crate::want_discrete(&cfg))?;
    let format = wgpu::TextureFormat::Rgba8UnormSrgb;
    let (scale, max_mp) = cfg.performance.resolution(g.is_integrated());
    let mut renderer = Renderer::new(g.device.clone(), g.queue.clone(), format, crate::renderer::Quality::for_gpu(g.is_integrated()))?;
    let mut view = renderer.new_view();
    renderer.resize(&mut view, width, height, scale, max_mp);
    let mut world = World::new(cfg, renderer.max_rings(), collector);
    if !world.is_synthetic() {
        wait_for_data(&mut world)?;
    }
    let out_tex = g.device.create_texture(&wgpu::TextureDescriptor {
        label: Some("bench"),
        size: wgpu::Extent3d { width, height, depth_or_array_layers: 1 },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
        view_formats: &[],
    });
    let out_view = out_tex.create_view(&wgpu::TextureViewDescriptor::default());
    while world.is_synthetic() && world.t < at {
        world.step(DT);
    }
    let aspect = width as f32 / height as f32;
    let composition = composition_for(&world.cfg, composition.as_deref(), width, height);
    let (mut cpu, mut total) = (Vec::new(), Vec::new());
    for i in 0..frames + 10 {
        let t0 = std::time::Instant::now();
        world.step(DT);
        let cam = Camera::new(composition, aspect, &world.cfg, world.y_top(), world.t as f32);
        renderer.render(&mut view, &mut world, &cam, &out_view, DT as f32);
        let t1 = std::time::Instant::now();
        g.device.poll(wgpu::PollType::wait_indefinitely()).context("waiting for GPU")?;
        let t2 = std::time::Instant::now();
        if i >= 10 {
            cpu.push((t1 - t0).as_secs_f64() * 1e3);
            total.push((t2 - t0).as_secs_f64() * 1e3);
        }
    }
    let stats = |v: &mut Vec<f64>| {
        v.sort_by(|a, b| a.total_cmp(b));
        (v.iter().sum::<f64>() / v.len() as f64, v[v.len() * 95 / 100])
    };
    let (cpu_avg, cpu_p95) = stats(&mut cpu);
    let (tot_avg, tot_p95) = stats(&mut total);
    let (iw, ih) = view.internal_size();
    println!("output {width}x{height}, internal {iw}x{ih} (scale {scale}), {} rings, {} segments", world.rings.len(), world.segs.len());
    println!("cpu   avg {cpu_avg:6.2} ms   p95 {cpu_p95:6.2} ms");
    println!("frame avg {tot_avg:6.2} ms   p95 {tot_p95:6.2} ms   (≈ GPU-bound max {:.0} fps)", 1000.0 / tot_avg);
    println!("at 24 fps the GPU is busy ≈{:.0}% of the time; at 8 fps ≈{:.0}%", tot_avg * 24.0 / 10.0, tot_avg * 8.0 / 10.0);
    Ok(())
}

fn numbered(p: &Path, i: u32) -> PathBuf {
    let stem = p.file_stem().and_then(|s| s.to_str()).unwrap_or("snapshot");
    let ext = p.extension().and_then(|s| s.to_str()).unwrap_or("png");
    p.with_file_name(format!("{stem}_{i:03}.{ext}"))
}

pub(crate) fn save_png(device: &wgpu::Device, queue: &wgpu::Queue, tex: &wgpu::Texture, w: u32, h: u32, path: &Path) -> Result<()> {
    let bgra = matches!(tex.format(), wgpu::TextureFormat::Bgra8Unorm | wgpu::TextureFormat::Bgra8UnormSrgb);
    let bpr = (w * 4).div_ceil(256) * 256;
    let buf = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("readback"),
        size: (bpr * h) as u64,
        usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    let mut enc = device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("readback") });
    enc.copy_texture_to_buffer(
        wgpu::TexelCopyTextureInfo { texture: tex, mip_level: 0, origin: wgpu::Origin3d::ZERO, aspect: wgpu::TextureAspect::All },
        wgpu::TexelCopyBufferInfo {
            buffer: &buf,
            layout: wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(bpr), rows_per_image: Some(h) },
        },
        wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
    );
    queue.submit([enc.finish()]);
    let slice = buf.slice(..);
    let (tx, rx) = std::sync::mpsc::channel();
    slice.map_async(wgpu::MapMode::Read, move |r| {
        let _ = tx.send(r);
    });
    device.poll(wgpu::PollType::wait_indefinitely()).context("waiting for GPU")?;
    rx.recv().context("readback channel")?.context("mapping readback buffer")?;
    let mut rgba = Vec::with_capacity((w * h * 4) as usize);
    {
        let data = slice.get_mapped_range().context("mapped range")?;
        for row in 0..h as usize {
            let start = row * bpr as usize;
            rgba.extend_from_slice(&data[start..start + (w * 4) as usize]);
        }
    }
    buf.unmap();
    if bgra {
        rgba.chunks_exact_mut(4).for_each(|p| p.swap(0, 2));
    }

    let file = std::fs::File::create(path).with_context(|| format!("creating {}", path.display()))?;
    let mut encoder = png::Encoder::new(std::io::BufWriter::new(file), w, h);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    let mut writer = encoder.write_header()?;
    writer.write_image_data(&rgba)?;
    Ok(())
}

fn process_cpu_seconds() -> f64 {
    let stat = std::fs::read_to_string("/proc/self/stat").unwrap_or_default();
    let after = stat.rsplit_once(')').map(|(_, r)| r).unwrap_or("");
    let f: Vec<&str> = after.split_whitespace().collect();
    let ticks: f64 = f.get(11).and_then(|v| v.parse().ok()).unwrap_or(0.0) + f.get(12).and_then(|v| v.parse().ok()).unwrap_or(0.0);
    ticks / 100.0
}

fn gpu_idle_ms() -> Option<f64> {
    let rd = std::fs::read_dir("/sys/class/drm").ok()?;
    for e in rd.flatten() {
        let p = e.path().join("gt/gt0/rc6_residency_ms");
        if let Ok(v) = std::fs::read_to_string(&p) {
            return v.trim().parse().ok();
        }
    }
    None
}

/// Integrated-GPU busy fraction over `secs` (not in RC6 idle), if measurable.
fn gpu_busy_over(secs: f64) -> Option<f64> {
    let (g0, t0) = (gpu_idle_ms()?, std::time::Instant::now());
    std::thread::sleep(std::time::Duration::from_secs_f64(secs));
    let (g1, dt) = (gpu_idle_ms()?, t0.elapsed().as_secs_f64() * 1e3);
    Some(1.0 - (g1 - g0) / dt)
}

/// Renders the wallpaper workload headlessly (one view per output at the
/// wallpaper window size, real-time pacing with the configured frame-rate
/// policy) and reports its CPU and GPU cost against a baseline measured first.
pub fn loadtest(cfg: Config, outputs: Vec<(u32, u32)>, seconds: f64, collector: Option<std::path::PathBuf>) -> Result<()> {
    use crate::camera::Composition;
    let baseline = gpu_busy_over(5.0);
    let (_instance, g) = gpu::init_headless(!crate::want_discrete(&cfg))?;
    let format = wgpu::TextureFormat::Rgba8UnormSrgb;
    let mut renderer = Renderer::new(g.device.clone(), g.queue.clone(), format, crate::renderer::Quality::for_gpu(g.is_integrated()))?;
    let comps = Composition::auto(&outputs);
    let main = crate::app::main_output(&outputs, &comps);
    if cfg.system_monitor.show != "off" {
        renderer.sysmon = Some(crate::sysmon::SysMon::start(g.adapter.get_info().vendor));
    }
    struct Out {
        view: crate::renderer::View,
        tex: wgpu::TextureView,
        size: (u32, u32),
        comp: Composition,
        next: std::time::Instant,
        frames: u32,
    }
    let mut outs = vec![];
    for (i, (&(w, h), comp)) in outputs.iter().zip(comps).enumerate() {
        let size = crate::app::wallpaper_size(w, h, &cfg.performance, g.is_integrated());
        let (sc, mp) = crate::app::scene_scale(&cfg.performance, true, g.is_integrated());
        let mut view = renderer.new_view();
        renderer.resize(&mut view, size.0, size.1, sc, mp);
        view.system_monitor = match cfg.system_monitor.show.as_str() {
            "all" => true,
            "off" => false,
            _ => i == main,
        };
        let tex = g
            .device
            .create_texture(&wgpu::TextureDescriptor {
                label: Some("loadtest"),
                size: wgpu::Extent3d { width: size.0, height: size.1, depth_or_array_layers: 1 },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format,
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
                view_formats: &[],
            })
            .create_view(&wgpu::TextureViewDescriptor::default());
        let (iw, ih) = view.internal_size();
        println!("output {w}x{h}: window {}x{}, scene {iw}x{ih} ({comp:?})", size.0, size.1);
        outs.push(Out { view, tex, size, comp, next: std::time::Instant::now(), frames: 0 });
    }
    let mut world = World::new(cfg, renderer.max_rings(), collector);
    if !world.is_synthetic() {
        wait_for_data(&mut world)?;
    }
    let perf = world.cfg.performance.clone();
    let battery = crate::app::on_battery();
    let fps = if battery { perf.fps.min(perf.fps_battery) } else { perf.fps }.max(1);
    let interval = std::time::Duration::from_secs_f64(1.0 / fps as f64);
    println!("GPU: {} ({}), target {fps} fps", g.adapter.get_info().name, if g.is_integrated() { "integrated" } else { "discrete" });
    let mut gaps: Vec<f64> = vec![];
    let mut last_frame = std::time::Instant::now();
    let (mut t_step, mut t_render) = (0.0f64, 0.0f64);
    let (cpu0, gpu0, t0) = (process_cpu_seconds(), gpu_idle_ms(), std::time::Instant::now());
    let mut last = t0;
    let mut accum = 0.0;
    while t0.elapsed().as_secs_f64() < seconds {
        let now = std::time::Instant::now();
        let real_dt = now.duration_since(last).as_secs_f64();
        last = now;
        accum += real_dt;
        let mut wake = now + interval;
        if outs.iter().any(|o| now >= o.next) {
            let ts = std::time::Instant::now();
            world.step(accum.min(0.5));
            t_step += ts.elapsed().as_secs_f64();
            accum = 0.0;
            if t0.elapsed().as_secs_f64() > 5.0 {
                // Steady state only (the first seconds include loading).
                let g = now.duration_since(last_frame).as_secs_f64() * 1e3;
                gaps.push(g);
                if g > 20.0 {
                    log::debug!("slow frame: {g:.1} ms at t={:.1}s (layout v{})", t0.elapsed().as_secs_f64(), world.layout_version);
                }
            }
            last_frame = now;
        }
        for o in &mut outs {
            if now >= o.next {
                let cam = Camera::new(o.comp, o.size.0 as f32 / o.size.1 as f32, &world.cfg, world.y_top(), world.t as f32);
                let tr = std::time::Instant::now();
                renderer.render(&mut o.view, &mut world, &cam, &o.tex, interval.as_secs_f32());
                t_render += tr.elapsed().as_secs_f64();
                o.frames += 1;
                o.next = now + interval;
            }
            wake = wake.min(o.next);
        }
        let _ = g.device.poll(wgpu::PollType::Poll);
        std::thread::sleep(wake.saturating_duration_since(std::time::Instant::now()));
    }
    let _ = g.device.poll(wgpu::PollType::wait_indefinitely());
    let secs = t0.elapsed().as_secs_f64();
    let cpu = (process_cpu_seconds() - cpu0) / secs * 100.0;
    let gpu = match (gpu0, gpu_idle_ms()) {
        (Some(a), Some(b)) => Some((1.0 - (b - a) / (secs * 1e3)) * 100.0),
        _ => None,
    };
    gaps.sort_by(|a, b| a.total_cmp(b));
    let p99 = gaps.get(gaps.len() * 99 / 100).copied().unwrap_or(0.0);
    let max_gap = gaps.last().copied().unwrap_or(0.0);
    println!("{secs:.0} s{}; frame interval p99 {p99:.1} ms, max {max_gap:.1} ms", if battery { " (battery)" } else { "" });
    for o in &outs {
        println!("  {}x{} {:?}: {:.1} fps", o.size.0, o.size.1, o.comp, o.frames as f64 / secs);
    }
    println!(
        "renderer CPU: {cpu:.1}% of one core (world step {:.1}%, render+encode {:.1}%)",
        t_step / secs * 100.0,
        t_render / secs * 100.0
    );
    match (baseline, gpu) {
        (Some(b), Some(g)) => {
            println!("iGPU busy: {g:.1}% (baseline without the renderer {:.1}%, so +{:.1} points)", b * 100.0, g - b * 100.0)
        }
        _ => println!("iGPU busy: not measurable on this GPU"),
    }
    Ok(())
}
