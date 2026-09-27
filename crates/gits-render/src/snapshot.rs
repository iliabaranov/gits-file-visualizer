//! Headless rendering to PNG, for look-dev and regression snapshots.

use crate::camera::Camera;
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
}

const DT: f64 = 1.0 / 24.0;
/// Frames rendered before each capture so motion smear and bloom settle.
const WARM_SECONDS: f64 = 1.5;

pub fn run(cfg: Config, opts: Options) -> Result<()> {
    let instance = gpu::instance();
    let g = pollster::block_on(gpu::init(&instance, None, cfg.performance.prefer_igpu))?;
    let format = wgpu::TextureFormat::Rgba8UnormSrgb;
    let mut renderer = Renderer::new(g.device.clone(), g.queue.clone(), format)?;
    renderer.resize(opts.width, opts.height, cfg.performance.render_scale, cfg.performance.max_internal_megapixels);
    let mut world = World::new(cfg, renderer.max_rings());

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
    let view = out_tex.create_view(&wgpu::TextureViewDescriptor::default());
    if let Some(dir) = opts.out.parent() {
        std::fs::create_dir_all(dir).ok();
    }

    let aspect = opts.width as f32 / opts.height as f32;
    for i in 0..opts.frames.max(1) {
        let capture_t = opts.at + i as f64 * opts.every;
        while world.t < capture_t - WARM_SECONDS {
            world.step(DT);
        }
        while world.t < capture_t {
            world.step(DT);
            let cam = Camera::wide(aspect, world.y_top(), world.cfg.camera.drift, world.t as f32);
            renderer.render(&mut world, &cam, &view, DT as f32);
            let _ = g.device.poll(wgpu::PollType::Poll);
        }
        let path = if opts.frames <= 1 { opts.out.clone() } else { numbered(&opts.out, i) };
        save_png(&g.device, &g.queue, &out_tex, opts.width, opts.height, &path)?;
        log::info!(
            "t={:.1}s phase={} activity={:.1}/s rings={} → {}",
            world.t,
            world.session.phase_name(),
            world.activity,
            world.rings.len(),
            path.display()
        );
    }
    Ok(())
}

/// Renders `frames` frames at 24 fps simulated time, waiting for the GPU after
/// each one, and reports CPU (simulation + encoding) and total frame times.
pub fn bench(cfg: Config, width: u32, height: u32, at: f64, frames: u32) -> Result<()> {
    let instance = gpu::instance();
    let g = pollster::block_on(gpu::init(&instance, None, cfg.performance.prefer_igpu))?;
    let format = wgpu::TextureFormat::Rgba8UnormSrgb;
    let scale = cfg.performance.render_scale;
    let max_mp = cfg.performance.max_internal_megapixels;
    let mut renderer = Renderer::new(g.device.clone(), g.queue.clone(), format)?;
    renderer.resize(width, height, scale, max_mp);
    let mut world = World::new(cfg, renderer.max_rings());
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
    let view = out_tex.create_view(&wgpu::TextureViewDescriptor::default());
    while world.t < at {
        world.step(DT);
    }
    let aspect = width as f32 / height as f32;
    let (mut cpu, mut total) = (Vec::new(), Vec::new());
    for i in 0..frames + 10 {
        let t0 = std::time::Instant::now();
        world.step(DT);
        let cam = Camera::wide(aspect, world.y_top(), world.cfg.camera.drift, world.t as f32);
        renderer.render(&mut world, &cam, &view, DT as f32);
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
    let (iw, ih) = renderer.internal_size();
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

fn save_png(device: &wgpu::Device, queue: &wgpu::Queue, tex: &wgpu::Texture, w: u32, h: u32, path: &Path) -> Result<()> {
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

    let file = std::fs::File::create(path).with_context(|| format!("creating {}", path.display()))?;
    let mut encoder = png::Encoder::new(std::io::BufWriter::new(file), w, h);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    let mut writer = encoder.write_header()?;
    writer.write_image_data(&rgba)?;
    Ok(())
}
