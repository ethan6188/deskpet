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
}

/// Everything that changes from one frame to the next.
pub struct FrameInput<'a> {
    pub model: Mat4,
    pub joints: &'a [Mat4],
    pub morphs: &'a [f32],
    pub time: f32,
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
    pub camera: Camera,
    style: Style,
    /// Pet height in model units; style widths are fractions of it.
    height: f32,
    vertex_count: u32,
    morph_count: u32,
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
                    flags: [view.is_some() as u32, 0, 0, 0],
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
        let outline = (style.kind != StyleKind::Real && style.outline > 0.0)
            .then(|| pipeline("outline", "vs_outline", "fs_outline", wgpu::Face::Front));
        // Shells are seen from both sides at the silhouette.
        let fur = (style.kind == StyleKind::Real && style.fur > 0.0)
            .then(|| pipeline("fur", "vs_fur", "fs_fur", wgpu::Face::Back));

        let (msaa_view, depth_view) = Self::attachments(&device, format, width, height_px);
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
            camera,
            style,
            height,
            vertex_count: model.vertices.len() as u32,
            morph_count: model.morph_names.len() as u32,
        }
    }

    fn attachments(
        device: &wgpu::Device,
        format: wgpu::TextureFormat,
        width: u32,
        height: u32,
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
                    sample_count: MSAA_SAMPLES,
                    dimension: wgpu::TextureDimension::D2,
                    format,
                    usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
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
            Self::attachments(&self.device, self.format, width, height);
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
            StyleKind::Real => 0,
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
            params: [input.time, s.bands as f32, 0.0, 0.0],
            counts: [self.vertex_count, self.morph_count, shells, kind],
        };
        self.queue
            .write_buffer(&self.frame_buffer, 0, bytemuck::bytes_of(&uniforms));
        let joints: Vec<[[f32; 4]; 4]> =
            input.joints.iter().map(|m| m.to_cols_array_2d()).collect();
        self.queue
            .write_buffer(&self.joint_buffer, 0, bytemuck::cast_slice(&joints));
        if !input.morphs.is_empty() {
            self.queue.write_buffer(
                &self.morph_weight_buffer,
                0,
                bytemuck::cast_slice(input.morphs),
            );
        }

        let mut encoder = self.device.create_command_encoder(&Default::default());
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("pet pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &self.msaa_view,
                    depth_slice: None,
                    resolve_target: Some(view),
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                        store: wgpu::StoreOp::Discard,
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
            pass.set_vertex_buffer(0, self.vertex_buffer.slice(..));
            pass.set_index_buffer(self.index_buffer.slice(..), wgpu::IndexFormat::Uint32);
            let passes = [
                (Some(&self.main), 1),
                (self.outline.as_ref(), 1),
                (self.fur.as_ref(), shells),
            ];
            for (pipeline, instances) in passes {
                let Some(pipeline) = pipeline else { continue };
                pass.set_pipeline(pipeline);
                for b in &self.batches {
                    pass.set_bind_group(1, &self.material_groups[b.material], &[]);
                    pass.draw_indexed(
                        b.first_index..b.first_index + b.index_count,
                        0,
                        0..instances,
                    );
                }
            }
        }
        self.queue.submit([encoder.finish()]);
    }
}
