//! The GPU preview on wgpu (DirectX 12 / Vulkan / Metal / OpenGL, whichever
//! the system has, including Windows' software adapter when no graphics
//! driver is installed).
//!
//! It draws the same [`Scene`]s as the OpenGL renderer in `gpu.rs`, with the
//! same results: CPU-projected triangles, premultiplied-alpha textures with
//! mipmaps, opaque triangles first (depth write on), then blended ones back
//! to front (depth write off). Each preview renders into its own offscreen
//! target (colour with 4x MSAA where supported, plus depth) during egui's
//! prepare step; `blit` then copies it into egui's render pass.
use crate::gpu::Scene;
use eframe::{egui_wgpu, wgpu};
use std::collections::BTreeMap;

const COLOR_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;
const DEPTH_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Depth32Float;

/// Same maths as the OpenGL shaders. OpenGL's clip z
/// `1.002002 d - 20.02002` (NDC -1..1) becomes `1.001001 d - 10.01001`
/// for wgpu's 0..1 depth range: near plane d = 10, far plane d = 10000.
const SCENE_SHADER: &str = r"
struct VertexOut {
    @builtin(position) position: vec4<f32>,
    @location(0) uv: vec2<f32>,
    @location(1) shade: f32,
};

@vertex
fn vs(@location(0) p: vec3<f32>, @location(1) uv: vec2<f32>, @location(2) shade: f32) -> VertexOut {
    var out: VertexOut;
    let d = p.z;
    out.position = vec4<f32>(p.xy * d, 1.001001 * d - 10.01001, d);
    out.uv = uv;
    out.shade = shade;
    return out;
}

@group(0) @binding(0) var surface: texture_2d<f32>;
@group(0) @binding(1) var surface_sampler: sampler;

@fragment
fn fs(input: VertexOut) -> @location(0) vec4<f32> {
    let c = textureSample(surface, surface_sampler, input.uv);
    if (c.a < 0.001) {
        discard;
    }
    return vec4<f32>(c.rgb * input.shade, c.a);
}
";

/// Copies the offscreen image into egui's pass (whose viewport egui has set
/// to the preview rectangle) with one full-screen triangle.
const BLIT_SHADER: &str = r"
struct VertexOut {
    @builtin(position) position: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

@vertex
fn vs(@builtin(vertex_index) i: u32) -> VertexOut {
    let xy = vec2<f32>(f32((i << 1u) & 2u), f32(i & 2u));
    var out: VertexOut;
    out.position = vec4<f32>(xy * 2.0 - 1.0, 0.0, 1.0);
    out.uv = vec2<f32>(xy.x, 1.0 - xy.y);
    return out;
}

@group(0) @binding(0) var image: texture_2d<f32>;
@group(0) @binding(1) var image_sampler: sampler;

@fragment
fn fs(input: VertexOut) -> @location(0) vec4<f32> {
    return textureSample(image, image_sampler, input.uv);
}
";

struct Draw {
    first: u32,
    count: u32,
    key: u64,
    blended: bool,
}

/// The offscreen colour (+ resolve) and depth for one preview size.
struct Target {
    size: [u32; 2],
    multisampled: Option<wgpu::TextureView>,
    resolved: wgpu::Texture,
    resolved_view: wgpu::TextureView,
    depth: wgpu::TextureView,
    blit: Option<wgpu::BindGroup>,
}

pub struct WgpuRenderer {
    device: wgpu::Device,
    queue: wgpu::Queue,
    samples: u32,
    opaque: wgpu::RenderPipeline,
    blended: wgpu::RenderPipeline,
    texture_layout: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
    blit: Option<(wgpu::RenderPipeline, wgpu::BindGroupLayout, wgpu::Sampler)>,
    textures: BTreeMap<u64, wgpu::BindGroup>,
    vertices: Option<wgpu::Buffer>,
    draws: Vec<Draw>,
    last_scene: u64,
    target: Option<Target>,
    pub label: String,
    /// Set by wgpu's error handler; the app then switches to software.
    pub errors: std::sync::Arc<std::sync::Mutex<Option<String>>>,
}

impl WgpuRenderer {
    /// `blit_format` is egui's framebuffer format (None for headless checks).
    pub fn new(
        adapter: &wgpu::Adapter,
        device: wgpu::Device,
        queue: wgpu::Queue,
        blit_format: Option<wgpu::TextureFormat>,
    ) -> Self {
        let info = adapter.get_info();
        let samples = if adapter
            .get_texture_format_features(COLOR_FORMAT)
            .flags
            .sample_count_supported(4)
        {
            4
        } else {
            1
        };
        let errors = std::sync::Arc::new(std::sync::Mutex::new(None));
        let sink = errors.clone();
        device.on_uncaptured_error(Box::new(move |e| {
            if let Ok(mut slot) = sink.lock() {
                slot.get_or_insert_with(|| e.to_string());
            }
        }));
        let texture_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("imvuwu texture"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("imvuwu scene"),
            source: wgpu::ShaderSource::Wgsl(SCENE_SHADER.into()),
        });
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("imvuwu scene"),
            bind_group_layouts: &[&texture_layout],
            push_constant_ranges: &[],
        });
        let pipeline = |blend: bool| {
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some(if blend {
                    "imvuwu blended"
                } else {
                    "imvuwu opaque"
                }),
                layout: Some(&layout),
                vertex: wgpu::VertexState {
                    module: &shader,
                    entry_point: Some("vs"),
                    compilation_options: Default::default(),
                    buffers: &[wgpu::VertexBufferLayout {
                        array_stride: 24,
                        step_mode: wgpu::VertexStepMode::Vertex,
                        attributes: &wgpu::vertex_attr_array![
                            0 => Float32x3, 1 => Float32x2, 2 => Float32
                        ],
                    }],
                },
                primitive: wgpu::PrimitiveState {
                    topology: wgpu::PrimitiveTopology::TriangleList,
                    cull_mode: None,
                    ..Default::default()
                },
                depth_stencil: Some(wgpu::DepthStencilState {
                    format: DEPTH_FORMAT,
                    depth_write_enabled: !blend,
                    depth_compare: wgpu::CompareFunction::Less,
                    stencil: Default::default(),
                    bias: Default::default(),
                }),
                multisample: wgpu::MultisampleState {
                    count: samples,
                    ..Default::default()
                },
                fragment: Some(wgpu::FragmentState {
                    module: &shader,
                    entry_point: Some("fs"),
                    compilation_options: Default::default(),
                    targets: &[Some(wgpu::ColorTargetState {
                        format: COLOR_FORMAT,
                        blend: blend.then_some(wgpu::BlendState::PREMULTIPLIED_ALPHA_BLENDING),
                        write_mask: wgpu::ColorWrites::ALL,
                    })],
                }),
                multiview: None,
                cache: None,
            })
        };
        let (opaque, blended) = (pipeline(false), pipeline(true));
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("imvuwu texture"),
            address_mode_u: wgpu::AddressMode::Repeat,
            address_mode_v: wgpu::AddressMode::Repeat,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            mipmap_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });
        let blit = blit_format.map(|format| {
            let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("imvuwu blit"),
                entries: &[
                    wgpu::BindGroupLayoutEntry {
                        binding: 0,
                        visibility: wgpu::ShaderStages::FRAGMENT,
                        ty: wgpu::BindingType::Texture {
                            sample_type: wgpu::TextureSampleType::Float { filterable: true },
                            view_dimension: wgpu::TextureViewDimension::D2,
                            multisampled: false,
                        },
                        count: None,
                    },
                    wgpu::BindGroupLayoutEntry {
                        binding: 1,
                        visibility: wgpu::ShaderStages::FRAGMENT,
                        ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                        count: None,
                    },
                ],
            });
            let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
                label: Some("imvuwu blit"),
                source: wgpu::ShaderSource::Wgsl(BLIT_SHADER.into()),
            });
            let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("imvuwu blit"),
                bind_group_layouts: &[&layout],
                push_constant_ranges: &[],
            });
            let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some("imvuwu blit"),
                layout: Some(&pipeline_layout),
                vertex: wgpu::VertexState {
                    module: &shader,
                    entry_point: Some("vs"),
                    compilation_options: Default::default(),
                    buffers: &[],
                },
                primitive: Default::default(),
                depth_stencil: None,
                multisample: Default::default(),
                fragment: Some(wgpu::FragmentState {
                    module: &shader,
                    entry_point: Some("fs"),
                    compilation_options: Default::default(),
                    targets: &[Some(wgpu::ColorTargetState {
                        format,
                        blend: None,
                        write_mask: wgpu::ColorWrites::ALL,
                    })],
                }),
                multiview: None,
                cache: None,
            });
            let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
                label: Some("imvuwu blit"),
                ..Default::default()
            });
            (pipeline, layout, sampler)
        });
        Self {
            device,
            queue,
            samples,
            opaque,
            blended,
            texture_layout,
            sampler,
            blit,
            textures: BTreeMap::new(),
            vertices: None,
            draws: Vec::new(),
            last_scene: 0,
            target: None,
            label: format!("GPU: {} ({:?})", info.name, info.backend),
            errors,
        }
    }

    pub fn failure(&self) -> Option<String> {
        self.errors.lock().ok().and_then(|e| e.clone())
    }

    fn ensure_target(&mut self, size: [u32; 2]) {
        if self.target.as_ref().is_some_and(|t| t.size == size) {
            return;
        }
        let extent = wgpu::Extent3d {
            width: size[0],
            height: size[1],
            depth_or_array_layers: 1,
        };
        let texture = |label, format, samples, usage| {
            self.device.create_texture(&wgpu::TextureDescriptor {
                label: Some(label),
                size: extent,
                mip_level_count: 1,
                sample_count: samples,
                dimension: wgpu::TextureDimension::D2,
                format,
                usage,
                view_formats: &[],
            })
        };
        let resolved = texture(
            "imvuwu preview",
            COLOR_FORMAT,
            1,
            wgpu::TextureUsages::RENDER_ATTACHMENT
                | wgpu::TextureUsages::TEXTURE_BINDING
                | wgpu::TextureUsages::COPY_SRC,
        );
        let resolved_view = resolved.create_view(&Default::default());
        let multisampled = (self.samples > 1).then(|| {
            texture(
                "imvuwu preview msaa",
                COLOR_FORMAT,
                self.samples,
                wgpu::TextureUsages::RENDER_ATTACHMENT,
            )
            .create_view(&Default::default())
        });
        let depth = texture(
            "imvuwu depth",
            DEPTH_FORMAT,
            self.samples,
            wgpu::TextureUsages::RENDER_ATTACHMENT,
        )
        .create_view(&Default::default());
        let blit = self.blit.as_ref().map(|(_, layout, sampler)| {
            self.device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("imvuwu blit"),
                layout,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: wgpu::BindingResource::TextureView(&resolved_view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: wgpu::BindingResource::Sampler(sampler),
                    },
                ],
            })
        });
        self.target = Some(Target {
            size,
            multisampled,
            resolved,
            resolved_view,
            depth,
            blit,
        });
    }

    /// Uploads a material image with premultiplied colour and a full mip
    /// chain (2x2 box filter, like glGenerateMipmap).
    fn upload_texture(&mut self, material: &crate::gpu::Material) {
        if self.textures.contains_key(&material.key) {
            return;
        }
        let (w, h) = material.image.dimensions();
        let mut level: Vec<u8> = material.image.as_raw().clone();
        for p in level.as_chunks_mut::<4>().0 {
            for i in 0..3 {
                p[i] = (p[i] as u16 * p[3] as u16 / 255) as u8;
            }
        }
        let mip_count = 32 - w.max(h).max(1).leading_zeros();
        let texture = self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("imvuwu material"),
            size: wgpu::Extent3d {
                width: w,
                height: h,
                depth_or_array_layers: 1,
            },
            mip_level_count: mip_count,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: COLOR_FORMAT,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        let (mut lw, mut lh) = (w, h);
        for mip in 0..mip_count {
            self.queue.write_texture(
                wgpu::TexelCopyTextureInfo {
                    texture: &texture,
                    mip_level: mip,
                    origin: wgpu::Origin3d::ZERO,
                    aspect: wgpu::TextureAspect::All,
                },
                &level,
                wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(4 * lw),
                    rows_per_image: Some(lh),
                },
                wgpu::Extent3d {
                    width: lw,
                    height: lh,
                    depth_or_array_layers: 1,
                },
            );
            let (nw, nh) = ((lw / 2).max(1), (lh / 2).max(1));
            let mut next = vec![0u8; (nw * nh * 4) as usize];
            for y in 0..nh {
                for x in 0..nw {
                    for c in 0..4 {
                        let mut sum = 0u32;
                        let mut n = 0u32;
                        for (dx, dy) in [(0, 0), (1, 0), (0, 1), (1, 1)] {
                            let (sx, sy) = ((x * 2 + dx).min(lw - 1), (y * 2 + dy).min(lh - 1));
                            sum += level[((sy * lw + sx) * 4 + c) as usize] as u32;
                            n += 1;
                        }
                        next[((y * nw + x) * 4 + c) as usize] = ((sum + n / 2) / n) as u8;
                    }
                }
            }
            level = next;
            (lw, lh) = (nw, nh);
        }
        let view = texture.create_view(&Default::default());
        let bind = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("imvuwu material"),
            layout: &self.texture_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&self.sampler),
                },
            ],
        });
        self.textures.insert(material.key, bind);
    }

    /// Builds the vertex buffer and draw list when the scene changes (same
    /// order as the OpenGL renderer).
    fn upload_scene(&mut self, scene: &Scene) {
        if self.last_scene == scene.id() {
            return;
        }
        let order = scene.draw_order();
        let mut floats: Vec<f32> = Vec::with_capacity(order.len() * 18);
        let mut draws: Vec<Draw> = Vec::new();
        for t in order {
            let material = t.material();
            self.upload_texture(material);
            let first = (floats.len() / 6) as u32;
            for v in t.vertices() {
                floats.extend(v);
            }
            match draws
                .last_mut()
                .filter(|d| d.key == material.key && d.blended == material.blended)
            {
                Some(last) => last.count += 3,
                None => draws.push(Draw {
                    first,
                    count: 3,
                    key: material.key,
                    blended: material.blended,
                }),
            }
        }
        let bytes: Vec<u8> = floats.iter().flat_map(|f| f.to_le_bytes()).collect();
        let size = (bytes.len() as u64).max(24);
        if self.vertices.as_ref().is_none_or(|b| b.size() < size) {
            self.vertices = Some(self.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("imvuwu vertices"),
                size: size.next_power_of_two(),
                usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            }));
        }
        if let Some(buffer) = &self.vertices {
            if !bytes.is_empty() {
                self.queue.write_buffer(buffer, 0, &bytes);
            }
        }
        self.draws = draws;
        self.last_scene = scene.id();
    }

    /// Records the scene into the offscreen target at `size` pixels.
    pub fn render(&mut self, scene: &Scene, size: [u32; 2], encoder: &mut wgpu::CommandEncoder) {
        let size = [size[0].clamp(1, 8192), size[1].clamp(1, 8192)];
        self.ensure_target(size);
        self.upload_scene(scene);
        let Some(target) = &self.target else { return };
        let [r, g, b] = crate::theme::STAGE_TOP.map(|c| c as f64 / 255.0);
        let (view, resolve) = match &target.multisampled {
            Some(ms) => (ms, Some(&target.resolved_view)),
            None => (&target.resolved_view, None),
        };
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("imvuwu preview"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view,
                resolve_target: resolve,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color { r, g, b, a: 1.0 }),
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                view: &target.depth,
                depth_ops: Some(wgpu::Operations {
                    load: wgpu::LoadOp::Clear(1.0),
                    store: wgpu::StoreOp::Discard,
                }),
                stencil_ops: None,
            }),
            timestamp_writes: None,
            occlusion_query_set: None,
        });
        let Some(vertices) = &self.vertices else {
            return;
        };
        pass.set_vertex_buffer(0, vertices.slice(..));
        let mut blended = None;
        for d in &self.draws {
            if blended != Some(d.blended) {
                pass.set_pipeline(if d.blended {
                    &self.blended
                } else {
                    &self.opaque
                });
                blended = Some(d.blended);
            }
            if let Some(bind) = self.textures.get(&d.key) {
                pass.set_bind_group(0, bind, &[]);
                pass.draw(d.first..d.first + d.count, 0..1);
            }
        }
    }

    /// Draws the last rendered preview into egui's pass.
    pub fn blit(&self, pass: &mut wgpu::RenderPass<'static>) {
        let (
            Some((pipeline, _, _)),
            Some(Target {
                blit: Some(bind), ..
            }),
        ) = (&self.blit, &self.target)
        else {
            return;
        };
        pass.set_pipeline(pipeline);
        pass.set_bind_group(0, bind, &[]);
        pass.draw(0..3, 0..1);
    }

    /// Renders `scene` at `size` and reads it back (diagnostics only).
    pub fn render_image(
        &mut self,
        scene: &Scene,
        size: [u32; 2],
    ) -> Result<image::RgbaImage, String> {
        let mut encoder = self.device.create_command_encoder(&Default::default());
        self.render(scene, size, &mut encoder);
        let target = self.target.as_ref().ok_or("No render target")?;
        let row = (size[0] * 4).div_ceil(256) * 256;
        let buffer = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("imvuwu readback"),
            size: (row * size[1]) as u64,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        encoder.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo {
                texture: &target.resolved,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyBufferInfo {
                buffer: &buffer,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(row),
                    rows_per_image: Some(size[1]),
                },
            },
            wgpu::Extent3d {
                width: size[0],
                height: size[1],
                depth_or_array_layers: 1,
            },
        );
        self.queue.submit([encoder.finish()]);
        let slice = buffer.slice(..);
        let (sender, receiver) = std::sync::mpsc::channel();
        slice.map_async(wgpu::MapMode::Read, move |r| {
            let _ = sender.send(r);
        });
        let _ = self.device.poll(wgpu::Maintain::Wait);
        receiver
            .recv()
            .map_err(|e| e.to_string())?
            .map_err(|e| e.to_string())?;
        let data = slice.get_mapped_range();
        let mut pixels = Vec::with_capacity((size[0] * size[1] * 4) as usize);
        for y in 0..size[1] {
            let start = (y * row) as usize;
            pixels.extend_from_slice(&data[start..start + (size[0] * 4) as usize]);
        }
        drop(data);
        buffer.unmap();
        if let Some(error) = self.failure() {
            return Err(error);
        }
        image::RgbaImage::from_raw(size[0], size[1], pixels).ok_or_else(|| "Readback size".into())
    }
}

/// egui paint callback: renders in `prepare`, copies in `paint`.
pub struct Callback {
    pub renderer: crate::gpu::Shared,
    pub scene: std::sync::Arc<Scene>,
    pub size: [u32; 2],
}

impl egui_wgpu::CallbackTrait for Callback {
    fn prepare(
        &self,
        _device: &wgpu::Device,
        _queue: &wgpu::Queue,
        _screen: &egui_wgpu::ScreenDescriptor,
        encoder: &mut wgpu::CommandEncoder,
        _resources: &mut egui_wgpu::CallbackResources,
    ) -> Vec<wgpu::CommandBuffer> {
        if let Ok(mut renderer) = self.renderer.lock() {
            if let crate::gpu::Renderer::Wgpu(r) = &mut *renderer {
                r.render(&self.scene, self.size, encoder);
            }
        }
        Vec::new()
    }

    fn paint(
        &self,
        _info: eframe::egui::PaintCallbackInfo,
        pass: &mut wgpu::RenderPass<'static>,
        _resources: &egui_wgpu::CallbackResources,
    ) {
        if let Ok(renderer) = self.renderer.lock() {
            if let crate::gpu::Renderer::Wgpu(r) = &*renderer {
                r.blit(pass);
            }
        }
    }
}

/// `--wgpu-render-check <folder> [--software]`: the GPU preview's scenes
/// rendered headless through wgpu (no window), saved as PNGs with timings.
/// `--software` asks for the fallback (CPU) adapter, as on a PC with no
/// graphics driver.
pub fn run_checks(folder: &std::path::Path, software: bool) -> Result<(), String> {
    std::fs::create_dir_all(folder).map_err(|e| e.to_string())?;
    // progress.txt shows how far a run got if a driver call never returns.
    let progress = |text: &str| {
        use std::io::Write;
        if let Ok(mut f) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(folder.join("progress.txt"))
        {
            let _ = writeln!(f, "{text}");
        }
    };
    progress("creating instance");
    let instance = wgpu::Instance::new(&wgpu::InstanceDescriptor::default());
    progress("requesting adapter");
    let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
        power_preference: wgpu::PowerPreference::HighPerformance,
        force_fallback_adapter: software,
        compatible_surface: None,
    }))
    .ok_or("No wgpu adapter")?;
    progress(&format!("adapter: {:?}", adapter.get_info()));
    let (device, queue) = pollster::block_on(adapter.request_device(&Default::default(), None))
        .map_err(|e| e.to_string())?;
    progress("device ready");
    let mut renderer = WgpuRenderer::new(&adapter, device, queue, None);
    progress("renderer ready");
    let mut report = format!("{}\nMSAA samples: {}\n", renderer.label, renderer.samples);
    for (name, scene) in crate::gpu::check_scenes() {
        progress(&format!("rendering {name}"));
        let start = std::time::Instant::now();
        let image = renderer.render_image(&scene, [960, 960])?;
        let ms = start.elapsed().as_secs_f64() * 1000.0;
        let cached = std::time::Instant::now();
        renderer.render_image(&scene, [960, 960])?;
        let cached_ms = cached.elapsed().as_secs_f64() * 1000.0;
        let visible = image.pixels().filter(|p| p[0] > 65 && p[0] > p[2]).count();
        if visible < 1000 {
            return Err(format!(
                "{name}: empty/invalid GPU image ({visible} skin pixels)"
            ));
        }
        image
            .save(folder.join(format!("{name}.png")))
            .map_err(|e| e.to_string())?;
        report.push_str(&format!(
            "{name}: first={ms:.2} ms, cached={cached_ms:.2} ms (incl. readback), {visible} skin pixels\n"
        ));
    }
    std::fs::write(folder.join("report.txt"), report).map_err(|e| e.to_string())
}
