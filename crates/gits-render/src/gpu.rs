use anyhow::{Context, Result};

pub struct Gpu {
    pub adapter: wgpu::Adapter,
    pub device: wgpu::Device,
    pub queue: wgpu::Queue,
}

pub fn instance() -> wgpu::Instance {
    wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env())
}

/// Picks an adapter (the integrated GPU when `low_power`, so hybrid laptops
/// never wake the discrete GPU) and opens a device.
pub async fn init(instance: &wgpu::Instance, surface: Option<&wgpu::Surface<'_>>, low_power: bool) -> Result<Gpu> {
    let adapter = instance
        .request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: if low_power { wgpu::PowerPreference::LowPower } else { wgpu::PowerPreference::HighPerformance },
            force_fallback_adapter: false,
            compatible_surface: surface,
            apply_limit_buckets: false,
        })
        .await
        .context("no suitable GPU adapter")?;
    let info = adapter.get_info();
    log::info!("GPU: {} ({:?}, {:?}, driver {})", info.name, info.backend, info.device_type, info.driver_info);

    let mut limits = wgpu::Limits::default();
    limits.max_texture_dimension_2d = adapter.limits().max_texture_dimension_2d.min(16384);
    let (device, queue) = adapter
        .request_device(&wgpu::DeviceDescriptor {
            label: Some("gits"),
            required_limits: limits,
            memory_hints: wgpu::MemoryHints::MemoryUsage,
            ..Default::default()
        })
        .await
        .context("opening GPU device")?;
    Ok(Gpu { adapter, device, queue })
}
