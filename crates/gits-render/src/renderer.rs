//! GPU resources and the per-frame pass sequence:
//! cached background → scene (tower + sprites) → smear → bloom → tonemap.

use crate::camera::Camera;
use crate::fx;
use crate::gpu_types::{Globals, RingGpu, SegmentGpu, SpriteVertex};
use crate::text::GlyphAtlas;
use crate::world::{ATLAS_W, BOOST_TAU, LUT_BINS, World};
use anyhow::Result;
use wgpu::util::DeviceExt;

const HDR: wgpu::TextureFormat = wgpu::TextureFormat::Rgba16Float;
const BLOOM_LEVELS: usize = 6;
const TOWER_SLICES: u32 = 256;

const COMMON_WGSL: &str = include_str!("../shaders/common.wgsl");
const TOWER_WGSL: &str = include_str!("../shaders/tower.wgsl");
const SPRITE_WGSL: &str = include_str!("../shaders/sprite.wgsl");
const POST_WGSL: &str = include_str!("../shaders/post.wgsl");

const ADDITIVE: wgpu::BlendState = wgpu::BlendState {
    color: wgpu::BlendComponent {
        src_factor: wgpu::BlendFactor::One,
        dst_factor: wgpu::BlendFactor::One,
        operation: wgpu::BlendOperation::Add,
    },
    alpha: wgpu::BlendComponent {
        src_factor: wgpu::BlendFactor::One,
        dst_factor: wgpu::BlendFactor::One,
        operation: wgpu::BlendOperation::Add,
    },
};

struct Target {
    view: wgpu::TextureView,
    _tex: wgpu::Texture,
}

impl Target {
    fn new(device: &wgpu::Device, w: u32, h: u32, format: wgpu::TextureFormat, label: &str) -> Self {
        let tex = device.create_texture(&wgpu::TextureDescriptor {
            label: Some(label),
            size: wgpu::Extent3d { width: w.max(1), height: h.max(1), depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        });
        let view = tex.create_view(&wgpu::TextureViewDescriptor::default());
        Self { view, _tex: tex }
    }
}

struct Targets {
    w: u32,
    h: u32,
    bg: Target,
    scene: Target,
    trail: [Target; 2],
    bloom: Vec<Target>,
    combine_bg: [wgpu::BindGroup; 2],
    down0_bg: [wgpu::BindGroup; 2],
    down_bg: Vec<wgpu::BindGroup>,
    up_bg: Vec<wgpu::BindGroup>,
    final_bg: [wgpu::BindGroup; 2],
}

pub struct Renderer {
    device: wgpu::Device,
    queue: wgpu::Queue,
    pub out_format: wgpu::TextureFormat,
    max_atlas_rows: u32,

    globals_buf: wgpu::Buffer,
    globals_bg: wgpu::BindGroup,

    tower_pipeline: wgpu::RenderPipeline,
    tower_bgl: wgpu::BindGroupLayout,
    tower_bg: Option<wgpu::BindGroup>,
    tower_vb: wgpu::Buffer,
    tower_ib: wgpu::Buffer,
    tower_index_count: u32,
    seg_buf: wgpu::Buffer,
    seg_cap: usize,
    ring_buf: wgpu::Buffer,
    open_buf: wgpu::Buffer,
    ring_cap: usize,
    lut_buf: wgpu::Buffer,
    atlas_tex: wgpu::Texture,
    atlas_view: wgpu::TextureView,
    atlas_rows: u32,

    sprite_pipeline: wgpu::RenderPipeline,
    sprite_bg: wgpu::BindGroup,
    fx_vb: wgpu::Buffer,
    fx_cap: usize,
    fx_count: u32,
    bg_vb: wgpu::Buffer,
    bg_count: u32,

    bg_pipeline: wgpu::RenderPipeline,
    combine_pipeline: wgpu::RenderPipeline,
    down_pipeline: wgpu::RenderPipeline,
    up_pipeline: wgpu::RenderPipeline,
    final_pipeline: wgpu::RenderPipeline,
    post_bgl: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
    dummy: Target,

    targets: Option<Targets>,
    trail_idx: usize,
    bg_dirty: bool,
    layout_version: u64,
    frame: u64,
    scratch: Vec<SpriteVertex>,
    pub atlas: GlyphAtlas,
}

fn storage_entry(binding: u32) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::FRAGMENT,
        ty: wgpu::BindingType::Buffer {
            ty: wgpu::BufferBindingType::Storage { read_only: true },
            has_dynamic_offset: false,
            min_binding_size: None,
        },
        count: None,
    }
}

fn texture_entry(binding: u32, sample_type: wgpu::TextureSampleType) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::FRAGMENT,
        ty: wgpu::BindingType::Texture { sample_type, view_dimension: wgpu::TextureViewDimension::D2, multisampled: false },
        count: None,
    }
}

fn sampler_entry(binding: u32) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::FRAGMENT,
        ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
        count: None,
    }
}

fn storage_buffer(device: &wgpu::Device, label: &str, bytes: u64) -> wgpu::Buffer {
    device.create_buffer(&wgpu::BufferDescriptor {
        label: Some(label),
        size: bytes.max(16),
        usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    })
}

fn atlas_texture(device: &wgpu::Device, rows: u32) -> (wgpu::Texture, wgpu::TextureView) {
    let tex = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("segment atlas"),
        size: wgpu::Extent3d { width: ATLAS_W, height: rows.max(1), depth_or_array_layers: 1 },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::R8Uint,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });
    let view = tex.create_view(&wgpu::TextureViewDescriptor::default());
    (tex, view)
}

fn vertex_buffer(device: &wgpu::Device, label: &str, count: usize) -> wgpu::Buffer {
    device.create_buffer(&wgpu::BufferDescriptor {
        label: Some(label),
        size: (count.max(64) * std::mem::size_of::<SpriteVertex>()) as u64,
        usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    })
}

impl Renderer {
    pub fn new(device: wgpu::Device, queue: wgpu::Queue, out_format: wgpu::TextureFormat) -> Result<Self> {
        let atlas = GlyphAtlas::build()?;
        let max_atlas_rows = device.limits().max_texture_dimension_2d;

        let module = |label: &str, src: &str| {
            device.create_shader_module(wgpu::ShaderModuleDescriptor {
                label: Some(label),
                source: wgpu::ShaderSource::Wgsl(format!("{COMMON_WGSL}\n{src}").into()),
            })
        };
        let tower_mod = module("tower.wgsl", TOWER_WGSL);
        let sprite_mod = module("sprite.wgsl", SPRITE_WGSL);
        let post_mod = module("post.wgsl", POST_WGSL);

        let globals_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("globals"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX | wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Buffer { ty: wgpu::BufferBindingType::Uniform, has_dynamic_offset: false, min_binding_size: None },
                count: None,
            }],
        });
        let globals_buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("globals"),
            size: std::mem::size_of::<Globals>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let globals_bg = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("globals"),
            layout: &globals_bgl,
            entries: &[wgpu::BindGroupEntry { binding: 0, resource: globals_buf.as_entire_binding() }],
        });

        let tower_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("tower"),
            entries: &[
                storage_entry(0),
                storage_entry(1),
                storage_entry(2),
                storage_entry(3),
                texture_entry(4, wgpu::TextureSampleType::Uint),
            ],
        });
        let sprite_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("sprite"),
            entries: &[texture_entry(0, wgpu::TextureSampleType::Float { filterable: true }), sampler_entry(1)],
        });
        let float_tex = wgpu::TextureSampleType::Float { filterable: true };
        let post_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("post"),
            entries: &[texture_entry(0, float_tex), texture_entry(1, float_tex), texture_entry(2, float_tex), sampler_entry(3)],
        });

        let layout = |label: &str, groups: &[&wgpu::BindGroupLayout]| {
            let groups: Vec<Option<&wgpu::BindGroupLayout>> = groups.iter().map(|g| Some(*g)).collect();
            device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some(label),
                bind_group_layouts: &groups,
                immediate_size: 0,
            })
        };
        let tower_layout = layout("tower", &[&globals_bgl, &tower_bgl]);
        let sprite_layout = layout("sprite", &[&globals_bgl, &sprite_bgl]);
        let post_layout = layout("post", &[&globals_bgl, &post_bgl]);
        let bg_layout = layout("background", &[&globals_bgl]);

        let tower_attrs = wgpu::vertex_attr_array![0 => Float32x3];
        let tower_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("tower"),
            layout: Some(&tower_layout),
            vertex: wgpu::VertexState {
                module: &tower_mod,
                entry_point: Some("vs_tower"),
                compilation_options: Default::default(),
                buffers: &[Some(wgpu::VertexBufferLayout {
                    array_stride: 12,
                    step_mode: wgpu::VertexStepMode::Vertex,
                    attributes: &tower_attrs,
                })],
            },
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            fragment: Some(wgpu::FragmentState {
                module: &tower_mod,
                entry_point: Some("fs_tower"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState { format: HDR, blend: Some(ADDITIVE), write_mask: wgpu::ColorWrites::ALL })],
            }),
            multiview_mask: None,
            cache: None,
        });

        let sprite_attrs =
            wgpu::vertex_attr_array![0 => Float32x3, 1 => Float32, 2 => Float32x2, 3 => Float32, 4 => Float32, 5 => Float32x4];
        let sprite_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("sprite"),
            layout: Some(&sprite_layout),
            vertex: wgpu::VertexState {
                module: &sprite_mod,
                entry_point: Some("vs_sprite"),
                compilation_options: Default::default(),
                buffers: &[Some(wgpu::VertexBufferLayout {
                    array_stride: std::mem::size_of::<SpriteVertex>() as u64,
                    step_mode: wgpu::VertexStepMode::Vertex,
                    attributes: &sprite_attrs,
                })],
            },
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            fragment: Some(wgpu::FragmentState {
                module: &sprite_mod,
                entry_point: Some("fs_sprite"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState { format: HDR, blend: Some(ADDITIVE), write_mask: wgpu::ColorWrites::ALL })],
            }),
            multiview_mask: None,
            cache: None,
        });

        let fullscreen =
            |label: &str, layout: &wgpu::PipelineLayout, entry: &str, format: wgpu::TextureFormat, blend: Option<wgpu::BlendState>| {
                device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                    label: Some(label),
                    layout: Some(layout),
                    vertex: wgpu::VertexState {
                        module: &post_mod,
                        entry_point: Some("vs_full"),
                        compilation_options: Default::default(),
                        buffers: &[],
                    },
                    primitive: wgpu::PrimitiveState::default(),
                    depth_stencil: None,
                    multisample: wgpu::MultisampleState::default(),
                    fragment: Some(wgpu::FragmentState {
                        module: &post_mod,
                        entry_point: Some(entry),
                        compilation_options: Default::default(),
                        targets: &[Some(wgpu::ColorTargetState { format, blend, write_mask: wgpu::ColorWrites::ALL })],
                    }),
                    multiview_mask: None,
                    cache: None,
                })
            };
        let bg_pipeline = fullscreen("background", &bg_layout, "fs_bg", HDR, None);
        let combine_pipeline = fullscreen("combine", &post_layout, "fs_combine", HDR, None);
        let down_pipeline = fullscreen("bloom down", &post_layout, "fs_down", HDR, None);
        let up_pipeline = fullscreen("bloom up", &post_layout, "fs_up", HDR, Some(ADDITIVE));
        let final_pipeline = fullscreen("final", &post_layout, "fs_final", out_format, None);

        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("linear clamp"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            address_mode_w: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            mipmap_filter: wgpu::MipmapFilterMode::Linear,
            ..Default::default()
        });

        // Glyph atlas with its mip chain (mips give cheap defocus for calligraphy).
        let glyph_tex = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("glyph atlas"),
            size: wgpu::Extent3d { width: atlas.width, height: atlas.height, depth_or_array_layers: 1 },
            mip_level_count: atlas.mips.len() as u32,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::R8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        for (level, data) in atlas.mips.iter().enumerate() {
            let (w, h) = ((atlas.width >> level).max(1), (atlas.height >> level).max(1));
            queue.write_texture(
                wgpu::TexelCopyTextureInfo {
                    texture: &glyph_tex,
                    mip_level: level as u32,
                    origin: wgpu::Origin3d::ZERO,
                    aspect: wgpu::TextureAspect::All,
                },
                data,
                wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(w), rows_per_image: Some(h) },
                wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
            );
        }
        let glyph_view = glyph_tex.create_view(&wgpu::TextureViewDescriptor::default());
        let sprite_bg = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("sprite"),
            layout: &sprite_bgl,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(&glyph_view) },
                wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::Sampler(&sampler) },
            ],
        });

        // Unit cylinder: y = 0 at the top of the tower, 1 at the bottom.
        let mut verts: Vec<[f32; 3]> = Vec::new();
        for i in 0..=TOWER_SLICES {
            let a = std::f32::consts::TAU * i as f32 / TOWER_SLICES as f32;
            verts.push([a.cos(), 0.0, a.sin()]);
            verts.push([a.cos(), 1.0, a.sin()]);
        }
        let mut idx: Vec<u32> = Vec::new();
        for i in 0..TOWER_SLICES {
            let b = i * 2;
            idx.extend_from_slice(&[b, b + 1, b + 2, b + 1, b + 3, b + 2]);
        }
        let tower_vb = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("tower mesh"),
            contents: bytemuck::cast_slice(&verts),
            usage: wgpu::BufferUsages::VERTEX,
        });
        let tower_ib = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("tower idx"),
            contents: bytemuck::cast_slice(&idx),
            usage: wgpu::BufferUsages::INDEX,
        });

        let seg_cap = 1024;
        let ring_cap = 256;
        let (atlas_tex, atlas_view) = atlas_texture(&device, 256);
        let dummy = Target::new(&device, 1, 1, HDR, "dummy");

        Ok(Self {
            seg_buf: storage_buffer(&device, "segments", (seg_cap * std::mem::size_of::<SegmentGpu>()) as u64),
            ring_buf: storage_buffer(&device, "rings", (ring_cap * std::mem::size_of::<RingGpu>()) as u64),
            open_buf: storage_buffer(&device, "ring open", (ring_cap * 4) as u64),
            lut_buf: storage_buffer(&device, "lut", ((LUT_BINS + 1) * 4) as u64),
            fx_vb: vertex_buffer(&device, "fx sprites", 4096),
            bg_vb: vertex_buffer(&device, "background sprites", 64),
            fx_cap: 4096,
            fx_count: 0,
            bg_count: 0,
            seg_cap,
            ring_cap,
            atlas_tex,
            atlas_view,
            atlas_rows: 256,
            max_atlas_rows,
            tower_bg: None,
            tower_vb,
            tower_ib,
            tower_index_count: idx.len() as u32,
            device,
            queue,
            out_format,
            globals_buf,
            globals_bg,
            tower_pipeline,
            tower_bgl,
            sprite_pipeline,
            sprite_bg,
            bg_pipeline,
            combine_pipeline,
            down_pipeline,
            up_pipeline,
            final_pipeline,
            post_bgl,
            sampler,
            dummy,
            targets: None,
            trail_idx: 0,
            bg_dirty: true,
            layout_version: 0,
            frame: 0,
            scratch: Vec::new(),
            atlas,
        })
    }

    /// Maximum number of rings the GPU can hold (atlas rows).
    pub fn max_rings(&self) -> usize {
        self.max_atlas_rows as usize
    }

    pub fn internal_size(&self) -> (u32, u32) {
        self.targets.as_ref().map(|t| (t.w, t.h)).unwrap_or((0, 0))
    }

    pub fn invalidate_background(&mut self) {
        self.bg_dirty = true;
    }

    fn post_group(&self, label: &str, a: &wgpu::TextureView, b: &wgpu::TextureView, c: &wgpu::TextureView) -> wgpu::BindGroup {
        self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some(label),
            layout: &self.post_bgl,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(a) },
                wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::TextureView(b) },
                wgpu::BindGroupEntry { binding: 2, resource: wgpu::BindingResource::TextureView(c) },
                wgpu::BindGroupEntry { binding: 3, resource: wgpu::BindingResource::Sampler(&self.sampler) },
            ],
        })
    }

    /// (Re)creates the internal render targets for an output of `out_w`×`out_h`.
    pub fn resize(&mut self, out_w: u32, out_h: u32, render_scale: f32, max_megapixels: f32) {
        let mut s = render_scale.clamp(0.2, 1.0);
        let max_px = (max_megapixels.max(0.1) * 1.0e6) as f64;
        let px = out_w as f64 * out_h as f64 * (s * s) as f64;
        if px > max_px {
            s *= (max_px / px).sqrt() as f32;
        }
        let (w, h) = (((out_w as f32 * s) as u32).max(16), ((out_h as f32 * s) as u32).max(16));
        if self.targets.as_ref().is_some_and(|t| t.w == w && t.h == h) {
            return;
        }
        let d = &self.device;
        let bg = Target::new(d, w, h, HDR, "background");
        let scene = Target::new(d, w, h, HDR, "scene");
        let trail = [Target::new(d, w, h, HDR, "trail a"), Target::new(d, w, h, HDR, "trail b")];
        let bloom: Vec<Target> =
            (0..BLOOM_LEVELS).map(|i| Target::new(d, (w >> (i + 1)).max(1), (h >> (i + 1)).max(1), HDR, "bloom")).collect();
        let dm = &self.dummy.view;
        let combine_bg = [
            self.post_group("combine a", &bg.view, &scene.view, &trail[1].view),
            self.post_group("combine b", &bg.view, &scene.view, &trail[0].view),
        ];
        let down0_bg = [self.post_group("down0 a", &trail[0].view, dm, dm), self.post_group("down0 b", &trail[1].view, dm, dm)];
        let down_bg = (1..BLOOM_LEVELS).map(|i| self.post_group("down", &bloom[i - 1].view, dm, dm)).collect();
        let up_bg = (0..BLOOM_LEVELS - 1).map(|i| self.post_group("up", &bloom[i + 1].view, dm, dm)).collect();
        let final_bg = [
            self.post_group("final a", &trail[0].view, &bloom[0].view, dm),
            self.post_group("final b", &trail[1].view, &bloom[0].view, dm),
        ];
        self.targets = Some(Targets { w, h, bg, scene, trail, bloom, combine_bg, down0_bg, down_bg, up_bg, final_bg });
        self.bg_dirty = true;
    }

    fn upload_world(&mut self, w: &mut World) {
        let q = &self.queue;
        if w.layout_version != self.layout_version {
            let n_segs = w.segs.len().max(1);
            let n_rings = w.rings.len().max(1);
            let mut rebind = self.tower_bg.is_none();
            if n_segs > self.seg_cap {
                self.seg_cap = n_segs * 3 / 2 + 256;
                self.seg_buf = storage_buffer(&self.device, "segments", (self.seg_cap * std::mem::size_of::<SegmentGpu>()) as u64);
                rebind = true;
            }
            if n_rings > self.ring_cap {
                self.ring_cap = n_rings * 3 / 2 + 64;
                self.ring_buf = storage_buffer(&self.device, "rings", (self.ring_cap * std::mem::size_of::<RingGpu>()) as u64);
                self.open_buf = storage_buffer(&self.device, "ring open", (self.ring_cap * 4) as u64);
                rebind = true;
            }
            if n_rings as u32 > self.atlas_rows {
                let rows = ((n_rings as u32 * 5 / 4).div_ceil(256) * 256).min(self.max_atlas_rows);
                (self.atlas_tex, self.atlas_view) = atlas_texture(&self.device, rows);
                self.atlas_rows = rows;
                rebind = true;
            }
            if !w.segs.is_empty() {
                q.write_buffer(&self.seg_buf, 0, bytemuck::cast_slice(&w.segs));
            }
            if !w.rings.is_empty() {
                q.write_buffer(&self.ring_buf, 0, bytemuck::cast_slice(&w.rings));
                q.write_buffer(&self.open_buf, 0, bytemuck::cast_slice(&w.ring_open));
                let rows = (w.rings.len() as u32).min(self.atlas_rows);
                q.write_texture(
                    wgpu::TexelCopyTextureInfo {
                        texture: &self.atlas_tex,
                        mip_level: 0,
                        origin: wgpu::Origin3d::ZERO,
                        aspect: wgpu::TextureAspect::All,
                    },
                    &w.atlas[..(rows * ATLAS_W) as usize],
                    wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(ATLAS_W), rows_per_image: Some(rows) },
                    wgpu::Extent3d { width: ATLAS_W, height: rows, depth_or_array_layers: 1 },
                );
            }
            if rebind {
                self.tower_bg = Some(self.device.create_bind_group(&wgpu::BindGroupDescriptor {
                    label: Some("tower"),
                    layout: &self.tower_bgl,
                    entries: &[
                        wgpu::BindGroupEntry { binding: 0, resource: self.seg_buf.as_entire_binding() },
                        wgpu::BindGroupEntry { binding: 1, resource: self.ring_buf.as_entire_binding() },
                        wgpu::BindGroupEntry { binding: 2, resource: self.open_buf.as_entire_binding() },
                        wgpu::BindGroupEntry { binding: 3, resource: self.lut_buf.as_entire_binding() },
                        wgpu::BindGroupEntry { binding: 4, resource: wgpu::BindingResource::TextureView(&self.atlas_view) },
                    ],
                }));
            }
            self.layout_version = w.layout_version;
            w.dirty_segs.clear();
            w.dirty_rings.clear();
            w.open_dirty = false;
            w.lut_dirty = true;
        } else {
            let seg_size = std::mem::size_of::<SegmentGpu>();
            if w.dirty_segs.len() > 512 {
                q.write_buffer(&self.seg_buf, 0, bytemuck::cast_slice(&w.segs));
            } else {
                w.dirty_segs.sort_unstable();
                w.dirty_segs.dedup();
                for &s in &w.dirty_segs {
                    if let Some(seg) = w.segs.get(s as usize) {
                        q.write_buffer(&self.seg_buf, (s as usize * seg_size) as u64, bytemuck::bytes_of(seg));
                    }
                }
            }
            w.dirty_segs.clear();
            let ring_size = std::mem::size_of::<RingGpu>();
            if w.dirty_rings.len() > 256 {
                q.write_buffer(&self.ring_buf, 0, bytemuck::cast_slice(&w.rings));
            } else {
                w.dirty_rings.sort_unstable();
                w.dirty_rings.dedup();
                for &r in &w.dirty_rings {
                    if let Some(ring) = w.rings.get(r as usize) {
                        q.write_buffer(&self.ring_buf, (r as usize * ring_size) as u64, bytemuck::bytes_of(ring));
                    }
                }
            }
            w.dirty_rings.clear();
            if w.open_dirty && !w.ring_open.is_empty() {
                q.write_buffer(&self.open_buf, 0, bytemuck::cast_slice(&w.ring_open));
                w.open_dirty = false;
            }
        }
        if w.lut_dirty {
            q.write_buffer(&self.lut_buf, 0, bytemuck::cast_slice(&w.lut));
            w.lut_dirty = false;
        }
    }

    fn globals(&self, w: &World, cam: &Camera, frame_dt: f32) -> Globals {
        let c = &w.cfg;
        let tw = &c.tower;
        let rgb = |name: &str, a: f32| {
            let v = c.color(name);
            [v[0], v[1], v[2], a]
        };
        let (iw, ih) = self.internal_size();
        let smear = c.palette.smear.0;
        let smear_decay = if smear > 1e-3 { (-frame_dt.max(0.0) / smear).exp() } else { 0.0 };
        Globals {
            view_proj: cam.view_proj().to_cols_array_2d(),
            cam_pos: [cam.eye.x, cam.eye.y, cam.eye.z, w.t as f32],
            tower: [tw.height, tw.radius, tw.depth_step, w.y_top()],
            tower2: [tw.depth_layers.max(1) as f32, tw.ribbon, tw.fill, tw.back_face],
            lut: [LUT_BINS as f32, w.rings.len() as f32, ATLAS_W as f32, BOOST_TAU],
            idle: [tw.idle_wound, tw.idle_open, 0.75, 0.22],
            col_fresh: rgb(&c.age.fresh, 1.0),
            col_old: rgb(&c.age.old, 1.0),
            col_stale: rgb(&c.age.stale, 1.0),
            col_read: rgb(&c.events.read.color, c.events.read.intensity),
            col_write: rgb(&c.events.write.color, c.events.write.intensity),
            col_create: rgb(&c.events.create.color, c.events.create.intensity),
            col_delete: rgb(&c.events.delete.color, c.events.delete.intensity),
            col_teal: rgb("teal", 1.0),
            col_bg: rgb("background", 1.0),
            times: [c.events.read.decay.0, c.events.write.decay.0, c.events.create.grow.0.max(0.01), c.events.delete.ghost_hours * 3600.0],
            times2: [c.calm.attack.0, tw.fill_open, w.loop_fade, 0.0],
            age: [c.age.fresh_days.max(1e-3).log10(), c.age.old_days.max(1e-3).log10(), c.age.stale_days.max(1e-3).log10(), 0.0],
            screen: [iw as f32, ih as f32, 1.0 / iw.max(1) as f32, 1.0 / ih.max(1) as f32],
            post: [c.palette.exposure, c.palette.bloom, c.palette.grain, c.palette.vignette],
            post2: [smear_decay, (self.frame % 997) as f32, if self.out_format.is_srgb() { 0.0 } else { 1.0 }, 1.0 / BLOOM_LEVELS as f32],
        }
    }

    /// Renders one frame of `w` seen through `cam` into `view` (the output size
    /// given to [`Renderer::resize`]).
    pub fn render(&mut self, w: &mut World, cam: &Camera, view: &wgpu::TextureView, frame_dt: f32) {
        if self.targets.is_none() {
            return;
        }
        self.upload_world(w);
        let globals = self.globals(w, cam, frame_dt);
        self.queue.write_buffer(&self.globals_buf, 0, bytemuck::bytes_of(&globals));
        let (iw, ih) = self.internal_size();

        let mut scratch = std::mem::take(&mut self.scratch);
        if self.bg_dirty {
            scratch.clear();
            fx::build_background(&w.cfg, &self.atlas, w.y_top(), cam, &mut scratch);
            self.bg_vb = vertex_buffer(&self.device, "background sprites", scratch.len());
            self.queue.write_buffer(&self.bg_vb, 0, bytemuck::cast_slice(&scratch));
            self.bg_count = scratch.len() as u32;
        }
        scratch.clear();
        w.fx.build(w, cam, &self.atlas, iw as f32, ih as f32, &mut scratch);
        if scratch.len() > self.fx_cap {
            self.fx_cap = scratch.len() * 3 / 2;
            self.fx_vb = vertex_buffer(&self.device, "fx sprites", self.fx_cap);
        }
        if !scratch.is_empty() {
            self.queue.write_buffer(&self.fx_vb, 0, bytemuck::cast_slice(&scratch));
        }
        self.fx_count = scratch.len() as u32;
        self.scratch = scratch;

        let tg = self.targets.as_ref().unwrap();
        let mut enc = self.device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("frame") });

        let pass = |enc: &mut wgpu::CommandEncoder, view: &wgpu::TextureView, clear: bool, label: &str| -> wgpu::RenderPass<'static> {
            enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some(label),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: if clear { wgpu::LoadOp::Clear(wgpu::Color::BLACK) } else { wgpu::LoadOp::Load },
                        store: wgpu::StoreOp::Store,
                    },
                })],
                ..Default::default()
            })
            .forget_lifetime()
        };

        if self.bg_dirty {
            let mut p = pass(&mut enc, &tg.bg.view, true, "background");
            p.set_bind_group(0, &self.globals_bg, &[]);
            p.set_pipeline(&self.bg_pipeline);
            p.draw(0..3, 0..1);
            if self.bg_count > 0 {
                p.set_pipeline(&self.sprite_pipeline);
                p.set_bind_group(1, &self.sprite_bg, &[]);
                p.set_vertex_buffer(0, self.bg_vb.slice(..));
                p.draw(0..self.bg_count, 0..1);
            }
            drop(p);
            self.bg_dirty = false;
        }

        // Profiling aid: GITS_SKIP=tower,sprites,bloom skips passes.
        let skip = std::env::var("GITS_SKIP").unwrap_or_default();
        {
            let mut p = pass(&mut enc, &tg.scene.view, true, "scene");
            p.set_bind_group(0, &self.globals_bg, &[]);
            if let Some(tbg) = self.tower_bg.as_ref().filter(|_| !skip.contains("tower")) {
                p.set_pipeline(&self.tower_pipeline);
                p.set_bind_group(1, tbg, &[]);
                p.set_vertex_buffer(0, self.tower_vb.slice(..));
                p.set_index_buffer(self.tower_ib.slice(..), wgpu::IndexFormat::Uint32);
                p.draw_indexed(0..self.tower_index_count, 0, 0..w.cfg.tower.depth_layers.max(1));
            }
            if self.fx_count > 0 && !skip.contains("sprites") {
                p.set_pipeline(&self.sprite_pipeline);
                p.set_bind_group(1, &self.sprite_bg, &[]);
                p.set_vertex_buffer(0, self.fx_vb.slice(..));
                p.draw(0..self.fx_count, 0..1);
            }
        }

        let cur = self.trail_idx;
        let fullscreen = |enc: &mut wgpu::CommandEncoder,
                          pipeline: &wgpu::RenderPipeline,
                          target: &wgpu::TextureView,
                          group: &wgpu::BindGroup,
                          clear: bool,
                          label: &str| {
            let mut p = pass(enc, target, clear, label);
            p.set_pipeline(pipeline);
            p.set_bind_group(0, &self.globals_bg, &[]);
            p.set_bind_group(1, group, &[]);
            p.draw(0..3, 0..1);
        };
        fullscreen(&mut enc, &self.combine_pipeline, &tg.trail[cur].view, &tg.combine_bg[cur], true, "combine");
        if !skip.contains("bloom") {
            fullscreen(&mut enc, &self.down_pipeline, &tg.bloom[0].view, &tg.down0_bg[cur], true, "bloom down 0");
            for i in 1..BLOOM_LEVELS {
                fullscreen(&mut enc, &self.down_pipeline, &tg.bloom[i].view, &tg.down_bg[i - 1], true, "bloom down");
            }
            for i in (0..BLOOM_LEVELS - 1).rev() {
                fullscreen(&mut enc, &self.up_pipeline, &tg.bloom[i].view, &tg.up_bg[i], false, "bloom up");
            }
        }
        fullscreen(&mut enc, &self.final_pipeline, view, &tg.final_bg[cur], true, "final");

        self.queue.submit([enc.finish()]);
        self.trail_idx = 1 - cur;
        self.frame += 1;
    }
}
