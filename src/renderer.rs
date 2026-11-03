//! wgpu renderer: skinned, morphing, multi-style pet drawn with 4x MSAA into a
//! transparent, Retina-resolution window surface, or into an offscreen texture
//! that the agent tools read back.

use std::sync::Arc;

use bytemuck::{Pod, Zeroable};
use glam::Mat4;
use wgpu::util::DeviceExt;
use winit::window::Window;

use crate::camera::Camera;
use crate::model::{Model, Texture, Vertex};
use crate::pet::{Style, StyleKind};
use crate::splat::{self, GpuSplat, Splat};

const MSAA_SAMPLES: u32 = 4;
const DEPTH_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Depth32Float;
/// Fur shells; more look denser but cost one extra draw of the mesh each.
const FUR_SHELLS: u32 = 16;

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct FrameUniforms {
    view_proj: [[f32; 4]; 4],
    model: [[f32; 4]; 4],
    eye: [f32; 4],
    light: [f32; 4],
    tint: [f32; 4],
    outline: [f32; 4],
    glow: [f32; 4],
    params: [f32; 4],
    counts: [u32; 4],
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct MaterialUniforms {
    base_color: [f32; 4],
    flags: [u32; 4],
    eye: [f32; 4],
}

/// Everything that changes from one frame to the next.
pub struct FrameInput<'a> {
    pub model: Mat4,
    pub joints: &'a [Mat4],
    pub morphs: &'a [f32],
    pub time: f32,
    /// Where the eyes look, -1..1 with y up.
    pub gaze: glam::Vec2,
}

struct DrawBatch {
    first_index: u32,
    index_count: u32,
    material: usize,
}

enum Target {
    Window {
        surface: wgpu::Surface<'static>,
        config: wgpu::SurfaceConfiguration,
    },
    Offscreen {
        texture: wgpu::Texture,
    },
}

/// Offscreen color format; sRGB so the pixels match what a window shows.
const OFFSCREEN_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8UnormSrgb;

pub struct Renderer {
    target: Target,
    device: wgpu::Device,
    queue: wgpu::Queue,
    width: u32,
    height_px: u32,
    format: wgpu::TextureFormat,
    main: wgpu::RenderPipeline,
    outline: Option<wgpu::RenderPipeline>,
    fur: Option<wgpu::RenderPipeline>,
    vertex_buffer: wgpu::Buffer,
    index_buffer: wgpu::Buffer,
    batches: Vec<DrawBatch>,
    frame_buffer: wgpu::Buffer,
    joint_buffer: wgpu::Buffer,
    morph_weight_buffer: wgpu::Buffer,
    frame_group: wgpu::BindGroup,
    material_groups: Vec<wgpu::BindGroup>,
    msaa_view: wgpu::TextureView,
    depth_view: wgpu::TextureView,
    /// 4 for meshes; 1 for splats, which are already smooth Gaussians and would
    /// only pay four times the blending for nothing.
    samples: u32,
    pub camera: Camera,
    style: Style,
    /// Pet height in model units; style widths are fractions of it.
    height: f32,
    vertex_count: u32,
    morph_count: u32,
    splats: Option<SplatPass>,
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct SplatUniforms {
    view: [[f32; 4]; 4],
    focal_viewport: [f32; 4],
    clip: [f32; 4],
}

/// The Gaussian splat path: drawn instead of the mesh for `kind = "splat"`.
struct SplatPass {
    pipeline: wgpu::RenderPipeline,
    uniforms: wgpu::Buffer,
    order_buffer: wgpu::Buffer,
    group: wgpu::BindGroup,
    /// CPU copy for sorting.
    splats: Vec<GpuSplat>,
    order: Vec<u32>,
    keys: Vec<u32>,
    /// What the current order was sorted for; unchanged inputs skip the sort.
    sorted_for: Option<(Vec<Mat4>, Mat4)>,
}

/// Box-filtered mip chain, so a detailed texture stays sharp but never shimmers
/// when the pet is drawn small.
fn mip_chain(tex: &Texture) -> Vec<(u32, u32, Vec<u8>)> {
    let mut levels = vec![(tex.width, tex.height, tex.rgba.clone())];
    while let Some((w, h, px)) = levels.last().filter(|(w, h, _)| *w > 1 || *h > 1) {
        let (nw, nh) = ((w / 2).max(1), (h / 2).max(1));
        let mut out = vec![0u8; (nw * nh * 4) as usize];
        for y in 0..nh {
            for x in 0..nw {
                for c in 0..4 {
                    let mut sum = 0u32;
                    for (dx, dy) in [(0, 0), (1, 0), (0, 1), (1, 1)] {
                        let sx = (x * 2 + dx).min(w - 1);
                        let sy = (y * 2 + dy).min(h - 1);
                        sum += px[((sy * w + sx) * 4 + c) as usize] as u32;
                    }
                    out[((y * nw + x) * 4 + c) as usize] = (sum / 4) as u8;
                }
            }
        }
        levels.push((nw, nh, out));
    }
    levels
}

fn upload_texture(device: &wgpu::Device, queue: &wgpu::Queue, tex: &Texture) -> wgpu::TextureView {
    let levels = mip_chain(tex);
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("base color"),
        size: wgpu::Extent3d {
            width: tex.width,
            height: tex.height,
            depth_or_array_layers: 1,
        },
        mip_level_count: levels.len() as u32,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8UnormSrgb,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });
    for (level, (w, h, px)) in levels.iter().enumerate() {
        queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &texture,
                mip_level: level as u32,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            px,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(w * 4),
                rows_per_image: Some(*h),
            },
            wgpu::Extent3d {
                width: *w,
                height: *h,
                depth_or_array_layers: 1,
            },
        );
    }
    texture.create_view(&Default::default())
}

fn uniform_entry(binding: u32) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
        ty: wgpu::BindingType::Buffer {
            ty: wgpu::BufferBindingType::Uniform,
            has_dynamic_offset: false,
            min_binding_size: None,
        },
        count: None,
    }
}

fn storage_entry(binding: u32) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::VERTEX,
        ty: wgpu::BindingType::Buffer {
            ty: wgpu::BufferBindingType::Storage { read_only: true },
            has_dynamic_offset: false,
            min_binding_size: None,
        },
        count: None,
    }
}

impl Renderer {
    /// A renderer drawing into `window`. `camera` frames the pet; `height` is
    /// its height in model units.
    pub async fn new(
        window: Arc<Window>,
        model: &Model,
        style: Style,
        splats: &[Splat],
        camera: Camera,
        height: f32,
    ) -> Self {
        let size = window.inner_size();
        let instance = Self::instance();
        let surface = instance.create_surface(window).expect("create surface");
        let adapter = Self::adapter(&instance, Some(&surface))
            .await
            .expect("no GPU adapter");
        let (device, queue) = adapter
            .request_device(&wgpu::DeviceDescriptor::default())
            .await
            .expect("request device");

        let caps = surface.get_capabilities(&adapter);
        let format = caps
            .formats
            .iter()
            .copied()
            .find(|f| f.is_srgb())
            .unwrap_or(caps.formats[0]);
        let alpha_mode = [
            wgpu::CompositeAlphaMode::PostMultiplied,
            wgpu::CompositeAlphaMode::PreMultiplied,
        ]
        .into_iter()
        .find(|m| caps.alpha_modes.contains(m))
        .unwrap_or(caps.alpha_modes[0]);
        let mut config = surface
            .get_default_config(&adapter, size.width.max(1), size.height.max(1))
            .expect("surface unsupported");
        config.format = format;
        config.alpha_mode = alpha_mode;
        // Fifo is vsync: never renders faster than the display needs.
        config.present_mode = wgpu::PresentMode::Fifo;
        surface.configure(&device, &config);
        let (w, h) = (config.width, config.height);
        Self::build(
            Target::Window { surface, config },
            device,
            queue,
            format,
            (w, h),
            model,
            style,
            splats,
            camera,
            height,
        )
    }

    /// A renderer drawing `width` x `height` pixels offscreen, for `capture`.
    pub async fn offscreen(
        width: u32,
        height_px: u32,
        model: &Model,
        style: Style,
        splats: &[Splat],
        camera: Camera,
        height: f32,
    ) -> Result<Self, String> {
        let instance = Self::instance();
        let adapter = Self::adapter(&instance, None)
            .await
            .map_err(|e| format!("no GPU adapter: {e}"))?;
        let (device, queue) = adapter
            .request_device(&wgpu::DeviceDescriptor::default())
            .await
            .map_err(|e| format!("request device: {e}"))?;
        let texture = Self::offscreen_texture(&device, width, height_px);
        Ok(Self::build(
            Target::Offscreen { texture },
            device,
            queue,
            OFFSCREEN_FORMAT,
            (width, height_px),
            model,
            style,
            splats,
            camera,
            height,
        ))
    }

    fn instance() -> wgpu::Instance {
        wgpu::Instance::new(wgpu::InstanceDescriptor {
            backends: wgpu::Backends::METAL,
            ..wgpu::InstanceDescriptor::new_without_display_handle()
        })
    }

    async fn adapter(
        instance: &wgpu::Instance,
        surface: Option<&wgpu::Surface<'_>>,
    ) -> Result<wgpu::Adapter, wgpu::RequestAdapterError> {
        // A desktop pet runs all day: prefer the integrated, low-power GPU.
        instance
            .request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::LowPower,
                force_fallback_adapter: false,
                compatible_surface: surface,
                ..Default::default()
            })
            .await
    }

    fn offscreen_texture(device: &wgpu::Device, width: u32, height: u32) -> wgpu::Texture {
        device.create_texture(&wgpu::TextureDescriptor {
            label: Some("offscreen"),
            size: wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: OFFSCREEN_FORMAT,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        })
    }

    #[allow(clippy::too_many_arguments)]
    fn build(
        target: Target,
        device: wgpu::Device,
        queue: wgpu::Queue,
        format: wgpu::TextureFormat,
        (width, height_px): (u32, u32),
        model: &Model,
        style: Style,
        splats: &[Splat],
        camera: Camera,
        height: f32,
    ) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("pet shader"),
            source: wgpu::ShaderSource::Wgsl(include_str!("shader.wgsl").into()),
        });

        let vertex_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("vertices"),
            contents: bytemuck::cast_slice(&model.vertices),
            usage: wgpu::BufferUsages::VERTEX,
        });
        let index_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("indices"),
            contents: bytemuck::cast_slice(&model.indices),
            usage: wgpu::BufferUsages::INDEX,
        });
        let frame_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("frame uniforms"),
            size: std::mem::size_of::<FrameUniforms>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let joint_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("joints"),
            size: (model.palette.len().max(1) * 64) as u64,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let morph_delta_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("morph deltas"),
            contents: bytemuck::cast_slice(&model.morph_deltas),
            usage: wgpu::BufferUsages::STORAGE,
        });
        let morph_weight_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("morph weights"),
            size: (model.morph_names.len().max(1) * 4) as u64,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let frame_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("frame layout"),
            entries: &[
                uniform_entry(0),
                storage_entry(1),
                storage_entry(2),
                storage_entry(3),
            ],
        });
        let frame_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("frame"),
            layout: &frame_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: frame_buffer.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: joint_buffer.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: morph_delta_buffer.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: morph_weight_buffer.as_entire_binding(),
                },
            ],
        });

        let material_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("material layout"),
            entries: &[
                uniform_entry(0),
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("base color"),
            address_mode_u: wgpu::AddressMode::Repeat,
            address_mode_v: wgpu::AddressMode::Repeat,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            mipmap_filter: wgpu::MipmapFilterMode::Linear,
            anisotropy_clamp: 8,
            ..Default::default()
        });
        let white = upload_texture(
            &device,
            &queue,
            &Texture {
                width: 1,
                height: 1,
                rgba: vec![255; 4],
            },
        );
        let textures: Vec<wgpu::TextureView> = model
            .textures
            .iter()
            .map(|t| upload_texture(&device, &queue, t))
            .collect();
        let material_groups = model
            .materials
            .iter()
            .map(|m| {
                let view = m.texture.and_then(|i| textures.get(i));
                let uniforms = MaterialUniforms {
                    base_color: m.base_color,
                    flags: [view.is_some() as u32, m.eye.is_some() as u32, 0, 0],
                    eye: m.eye.unwrap_or_default(),
                };
                let buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                    label: Some("material"),
                    contents: bytemuck::bytes_of(&uniforms),
                    usage: wgpu::BufferUsages::UNIFORM,
                });
                device.create_bind_group(&wgpu::BindGroupDescriptor {
                    label: Some("material"),
                    layout: &material_layout,
                    entries: &[
                        wgpu::BindGroupEntry {
                            binding: 0,
                            resource: buffer.as_entire_binding(),
                        },
                        wgpu::BindGroupEntry {
                            binding: 1,
                            resource: wgpu::BindingResource::TextureView(view.unwrap_or(&white)),
                        },
                        wgpu::BindGroupEntry {
                            binding: 2,
                            resource: wgpu::BindingResource::Sampler(&sampler),
                        },
                    ],
                })
            })
            .collect();

        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("pet layout"),
            bind_group_layouts: &[Some(&frame_layout), Some(&material_layout)],
            immediate_size: 0,
        });
        let attributes = wgpu::vertex_attr_array![
            0 => Float32x3, 1 => Float32x3, 2 => Float32x3, 3 => Float32x2, 4 => Uint32x4, 5 => Float32x4
        ];
        let pipeline = |label, vs, fs, cull| {
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some(label),
                layout: Some(&layout),
                vertex: wgpu::VertexState {
                    module: &shader,
                    entry_point: Some(vs),
                    compilation_options: Default::default(),
                    buffers: &[Some(wgpu::VertexBufferLayout {
                        array_stride: std::mem::size_of::<Vertex>() as u64,
                        step_mode: wgpu::VertexStepMode::Vertex,
                        attributes: &attributes,
                    })],
                },
                primitive: wgpu::PrimitiveState {
                    cull_mode: Some(cull),
                    ..Default::default()
                },
                depth_stencil: Some(wgpu::DepthStencilState {
                    format: DEPTH_FORMAT,
                    depth_write_enabled: Some(true),
                    depth_compare: Some(wgpu::CompareFunction::LessEqual),
                    stencil: Default::default(),
                    bias: Default::default(),
                }),
                multisample: wgpu::MultisampleState {
                    count: MSAA_SAMPLES,
                    ..Default::default()
                },
                fragment: Some(wgpu::FragmentState {
                    module: &shader,
                    entry_point: Some(fs),
                    compilation_options: Default::default(),
                    targets: &[Some(wgpu::ColorTargetState {
                        format,
                        blend: Some(wgpu::BlendState::PREMULTIPLIED_ALPHA_BLENDING),
                        write_mask: wgpu::ColorWrites::ALL,
                    })],
                }),
                multiview_mask: None,
                cache: None,
            })
        };
        let main = pipeline("main", "vs_main", "fs_main", wgpu::Face::Back);
        let splat_pass =
            (!splats.is_empty()).then(|| Self::splat_pass(&device, &frame_layout, format, splats));
        let outline = (style.kind == StyleKind::Anime && style.outline > 0.0)
            .then(|| pipeline("outline", "vs_outline", "fs_outline", wgpu::Face::Front));
        // Shells are seen from both sides at the silhouette.
        let fur = (style.kind == StyleKind::Real && style.fur > 0.0)
            .then(|| pipeline("fur", "vs_fur", "fs_fur", wgpu::Face::Back));

        let samples = if splats.is_empty() { MSAA_SAMPLES } else { 1 };
        let (msaa_view, depth_view) = Self::attachments(&device, format, width, height_px, samples);
        Self {
            target,
            device,
            queue,
            width,
            height_px,
            format,
            main,
            outline,
            fur,
            vertex_buffer,
            index_buffer,
            batches: model
                .primitives
                .iter()
                .map(|p| DrawBatch {
                    first_index: p.first_index,
                    index_count: p.index_count,
                    material: p.material,
                })
                .collect(),
            frame_buffer,
            joint_buffer,
            morph_weight_buffer,
            frame_group,
            material_groups,
            msaa_view,
            depth_view,
            samples,
            camera,
            style,
            height,
            vertex_count: model.vertices.len() as u32,
            morph_count: model.morph_names.len() as u32,
            splats: splat_pass,
        }
    }

    fn attachments(
        device: &wgpu::Device,
        format: wgpu::TextureFormat,
        width: u32,
        height: u32,
        samples: u32,
    ) -> (wgpu::TextureView, wgpu::TextureView) {
        let size = wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        };
        let make = |label, format| {
            device
                .create_texture(&wgpu::TextureDescriptor {
                    label: Some(label),
                    size,
                    mip_level_count: 1,
                    sample_count: samples,
                    dimension: wgpu::TextureDimension::D2,
                    format,
                    // Apple GPUs are tile-based: MSAA samples and depth live in on-chip
                    // tile memory and are never written to RAM, because we resolve and
                    // discard them in the same pass. Memoryless storage makes that explicit
                    // and frees the backing memory entirely.
                    usage: wgpu::TextureUsages::RENDER_ATTACHMENT
                        | wgpu::TextureUsages::TRANSIENT_ATTACHMENT,
                    view_formats: &[],
                })
                .create_view(&Default::default())
        };
        (make("msaa color", format), make("depth", DEPTH_FORMAT))
    }

    pub fn resize(&mut self, width: u32, height: u32) {
        if width == 0 || height == 0 {
            return;
        }
        self.width = width;
        self.height_px = height;
        match &mut self.target {
            Target::Window { surface, config } => {
                config.width = width;
                config.height = height;
                surface.configure(&self.device, config);
            }
            Target::Offscreen { texture } => {
                *texture = Self::offscreen_texture(&self.device, width, height);
            }
        }
        (self.msaa_view, self.depth_view) =
            Self::attachments(&self.device, self.format, width, height, self.samples);
    }

    /// Draws a frame and shows it in the window. Offscreen, it only draws.
    pub fn render(&mut self, input: &FrameInput) {
        let frame = match &self.target {
            Target::Window { surface, config } => match surface.get_current_texture() {
                wgpu::CurrentSurfaceTexture::Success(f)
                | wgpu::CurrentSurfaceTexture::Suboptimal(f) => Some(f),
                wgpu::CurrentSurfaceTexture::Outdated | wgpu::CurrentSurfaceTexture::Lost => {
                    surface.configure(&self.device, config);
                    return;
                }
                _ => return,
            },
            Target::Offscreen { .. } => None,
        };
        let view = match (&frame, &self.target) {
            (Some(f), _) => f.texture.create_view(&Default::default()),
            (None, Target::Offscreen { texture }) => texture.create_view(&Default::default()),
            (None, Target::Window { .. }) => return,
        };
        self.encode(input, &view);
        if let Some(frame) = frame {
            self.queue.present(frame);
        }
    }

    /// Draws a frame offscreen and returns it as tightly packed RGBA8 rows
    /// (sRGB, premultiplied alpha), plus how long the GPU took.
    pub fn capture(
        &mut self,
        input: &FrameInput,
    ) -> Result<(Vec<u8>, std::time::Duration), String> {
        let Target::Offscreen { texture } = &self.target else {
            return Err("capture needs an offscreen renderer".into());
        };
        let texture = texture.clone();
        let view = texture.create_view(&Default::default());
        let started = std::time::Instant::now();
        self.encode(input, &view);
        let (w, h) = (self.width, self.height_px);
        // Rows of a texture-to-buffer copy must be 256-byte aligned.
        let row = (w * 4).div_ceil(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT)
            * wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
        let buffer = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("readback"),
            size: (row * h) as u64,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let mut encoder = self.device.create_command_encoder(&Default::default());
        encoder.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo {
                texture: &texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyBufferInfo {
                buffer: &buffer,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(row),
                    rows_per_image: Some(h),
                },
            },
            wgpu::Extent3d {
                width: w,
                height: h,
                depth_or_array_layers: 1,
            },
        );
        self.queue.submit([encoder.finish()]);
        let slice = buffer.slice(..);
        let (tx, rx) = std::sync::mpsc::channel();
        slice.map_async(wgpu::MapMode::Read, move |r| {
            let _ = tx.send(r);
        });
        self.device
            .poll(wgpu::PollType::Wait {
                submission_index: None,
                timeout: None,
            })
            .map_err(|e| format!("GPU poll: {e}"))?;
        let gpu_time = started.elapsed();
        rx.recv()
            .map_err(|e| e.to_string())?
            .map_err(|e| format!("map readback: {e}"))?;
        let mapped = slice
            .get_mapped_range()
            .map_err(|e| format!("read mapped buffer: {e:?}"))?;
        let mut out = Vec::with_capacity((w * h * 4) as usize);
        for y in 0..h {
            let start = (y * row) as usize;
            out.extend_from_slice(&mapped[start..start + (w * 4) as usize]);
        }
        drop(mapped);
        buffer.unmap();
        Ok((out, gpu_time))
    }

    fn encode(&mut self, input: &FrameInput, view: &wgpu::TextureView) {
        let s = &self.style;
        let kind = match s.kind {
            StyleKind::Real | StyleKind::Splat => 0,
            StyleKind::Anime => 1,
        };
        let shells = if self.fur.is_some() { FUR_SHELLS } else { 0 };
        let uniforms = FrameUniforms {
            view_proj: self.camera.view_proj().to_cols_array_2d(),
            model: input.model.to_cols_array_2d(),
            eye: self.camera.eye.extend(1.0).to_array(),
            light: [0.45, 0.85, 0.75, 0.0],
            tint: [s.tint[0], s.tint[1], s.tint[2], 1.0],
            outline: [
                s.outline_color[0],
                s.outline_color[1],
                s.outline_color[2],
                s.outline * self.height,
            ],
            glow: [s.glow[0], s.glow[1], s.glow[2], s.fur * self.height],
            params: [input.time, s.bands as f32, input.gaze.x, input.gaze.y],
            counts: [self.vertex_count, self.morph_count, shells, kind],
        };
        self.queue
            .write_buffer(&self.frame_buffer, 0, bytemuck::bytes_of(&uniforms));
        // glam's Mat4 is 16 column-major f32s, exactly the WGSL layout: upload in place.
        self.queue
            .write_buffer(&self.joint_buffer, 0, bytemuck::cast_slice(input.joints));
        if !input.morphs.is_empty() {
            self.queue.write_buffer(
                &self.morph_weight_buffer,
                0,
                bytemuck::cast_slice(input.morphs),
            );
        }

        if let Some(sp) = &mut self.splats {
            let view = self.camera.view();
            let view_model = view * input.model;
            // An idle pet only breathes, a ~1.5% scale that can't reorder splats
            // by more than a hair: when the skeleton hasn't moved, keep the order.
            let same = sp.sorted_for.as_ref().is_some_and(|(j, vm)| {
                j.as_slice() == input.joints
                    && vm.abs_diff_eq(view_model, 0.02 * vm.w_axis.length())
            });
            if !same {
                splat::sort_order(
                    &sp.splats,
                    input.joints,
                    view_model,
                    &mut sp.order,
                    &mut sp.keys,
                );
                self.queue
                    .write_buffer(&sp.order_buffer, 0, bytemuck::cast_slice(&sp.order));
                sp.sorted_for = Some((input.joints.to_vec(), view_model));
            }
            let focal = self.camera.focal(self.height_px as f32);
            let u = SplatUniforms {
                view: view.to_cols_array_2d(),
                focal_viewport: [focal, focal, self.width as f32, self.height_px as f32],
                clip: [self.camera.near(), 0.0, 0.0, 0.0],
            };
            self.queue
                .write_buffer(&sp.uniforms, 0, bytemuck::bytes_of(&u));
        }

        let mut encoder = self.device.create_command_encoder(&Default::default());
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("pet pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    // Without MSAA, draw straight into the target.
                    view: if self.samples > 1 {
                        &self.msaa_view
                    } else {
                        view
                    },
                    depth_slice: None,
                    resolve_target: (self.samples > 1).then_some(view),
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                        store: if self.samples > 1 {
                            wgpu::StoreOp::Discard
                        } else {
                            wgpu::StoreOp::Store
                        },
                    },
                })],
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                    view: &self.depth_view,
                    depth_ops: Some(wgpu::Operations {
                        load: wgpu::LoadOp::Clear(1.0),
                        store: wgpu::StoreOp::Discard,
                    }),
                    stencil_ops: None,
                }),
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            pass.set_bind_group(0, &self.frame_group, &[]);
            if let Some(sp) = &self.splats {
                pass.set_pipeline(&sp.pipeline);
                pass.set_bind_group(1, &sp.group, &[]);
                pass.draw(0..6, 0..sp.order.len() as u32);
            } else {
                Self::draw_mesh(&mut pass, self, shells);
            }
        }
        self.queue.submit([encoder.finish()]);
    }

    fn draw_mesh(pass: &mut wgpu::RenderPass<'_>, r: &Renderer, shells: u32) {
        {
            pass.set_vertex_buffer(0, r.vertex_buffer.slice(..));
            pass.set_index_buffer(r.index_buffer.slice(..), wgpu::IndexFormat::Uint32);
            let passes = [
                (Some(&r.main), 1),
                (r.outline.as_ref(), 1),
                (r.fur.as_ref(), shells),
            ];
            for (pipeline, instances) in passes {
                let Some(pipeline) = pipeline else { continue };
                pass.set_pipeline(pipeline);
                for b in &r.batches {
                    pass.set_bind_group(1, &r.material_groups[b.material], &[]);
                    pass.draw_indexed(
                        b.first_index..b.first_index + b.index_count,
                        0,
                        0..instances,
                    );
                }
            }
        }
    }

    fn splat_pass(
        device: &wgpu::Device,
        frame_layout: &wgpu::BindGroupLayout,
        format: wgpu::TextureFormat,
        splats: &[Splat],
    ) -> SplatPass {
        let gpu: Vec<GpuSplat> = splats.iter().map(Splat::gpu).collect();
        let splat_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("splats"),
            contents: bytemuck::cast_slice(&gpu),
            usage: wgpu::BufferUsages::STORAGE,
        });
        let order: Vec<u32> = (0..gpu.len() as u32).collect();
        let order_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("splat order"),
            contents: bytemuck::cast_slice(&order),
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
        });
        let uniforms = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("splat uniforms"),
            size: std::mem::size_of::<SplatUniforms>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("splat layout"),
            entries: &[uniform_entry(0), storage_entry(1), storage_entry(2)],
        });
        let group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("splats"),
            layout: &layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: uniforms.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: splat_buffer.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: order_buffer.as_entire_binding(),
                },
            ],
        });
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("splat shader"),
            source: wgpu::ShaderSource::Wgsl(include_str!("splat.wgsl").into()),
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("splat pipeline layout"),
            bind_group_layouts: &[Some(frame_layout), Some(&layout)],
            immediate_size: 0,
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("splats"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_splat"),
                compilation_options: Default::default(),
                buffers: &[],
            },
            primitive: wgpu::PrimitiveState::default(),
            // Sorted and blended: no depth writes, but the pass has a depth target.
            depth_stencil: Some(wgpu::DepthStencilState {
                format: DEPTH_FORMAT,
                depth_write_enabled: Some(false),
                depth_compare: Some(wgpu::CompareFunction::Always),
                stencil: Default::default(),
                bias: Default::default(),
            }),
            multisample: wgpu::MultisampleState {
                count: 1,
                ..Default::default()
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_splat"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    blend: Some(wgpu::BlendState::PREMULTIPLIED_ALPHA_BLENDING),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            multiview_mask: None,
            cache: None,
        });
        SplatPass {
            pipeline,
            uniforms,
            order_buffer,
            group,
            splats: gpu,
            order,
            keys: Vec::new(),
            sorted_for: None,
        }
    }
}
