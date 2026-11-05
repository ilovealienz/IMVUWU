//! Shared GPU viewport. Native mesh/pose IO remains outside this module.
use crate::{
    model::{Document, V},
    render::{Camera, Texture},
};
use eframe::{
    egui,
    glow::{self, HasContext},
};
use std::{
    collections::BTreeMap,
    hash::{Hash, Hasher},
    sync::{Arc, Mutex, OnceLock},
};

pub type Shared = Arc<Mutex<Renderer>>;
#[derive(Clone)]
pub struct Material {
    pub image: Arc<image::RgbaImage>,
    pub key: u64,
    pub blended: bool,
}
impl Material {
    pub fn new(image: Arc<image::RgbaImage>) -> Self {
        let mut h = std::collections::hash_map::DefaultHasher::new();
        image.dimensions().hash(&mut h);
        image.as_raw().hash(&mut h);
        Self {
            key: h.finish(),
            blended: image.pixels().any(|p| p[3] < 255),
            image,
        }
    }
}
pub(crate) struct Triangle {
    vertices: [[f32; 6]; 3],
    material: Material,
    depth: f32,
}
impl Triangle {
    pub(crate) fn material(&self) -> &Material {
        &self.material
    }
    pub(crate) fn vertices(&self) -> [[f32; 6]; 3] {
        self.vertices
    }
}
pub struct Scene {
    id: u64,
    triangles: Vec<Triangle>,
    pub projected: Vec<[f32; 3]>,
    pub size: [usize; 2],
}
impl Scene {
    pub fn new(size: [usize; 2]) -> Self {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
        Self {
            id: NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
            triangles: Vec::new(),
            projected: Vec::new(),
            size,
        }
    }
    pub(crate) fn id(&self) -> u64 {
        self.id
    }
    /// Draw order shared by both renderers: opaque triangles grouped by
    /// material, then blended ones back to front.
    pub(crate) fn draw_order(&self) -> Vec<&Triangle> {
        let mut order: Vec<_> = self.triangles.iter().collect();
        order.sort_by(|a, b| {
            a.material.blended.cmp(&b.material.blended).then_with(|| {
                if a.material.blended {
                    b.depth.total_cmp(&a.depth)
                } else {
                    a.material.key.cmp(&b.material.key)
                }
            })
        });
        order
    }
    pub fn mesh(
        &mut self,
        positions: &[V],
        faces: &[[usize; 3]],
        uvs: &[[f32; 2]],
        material: Material,
        camera: &Camera,
        hidden: &[bool],
    ) {
        let projected: Vec<_> = positions
            .iter()
            .map(|&p| camera.project(p, self.size[0], self.size[1]))
            .collect();
        let mut normals = Vec::new();
        self.faces(
            positions,
            &projected,
            faces,
            uvs,
            &material,
            camera,
            hidden,
            &mut normals,
        );
        self.projected.extend(projected);
    }
    /// Adds `faces` using vertices already projected by the caller, so meshes
    /// split into several materials are projected once rather than per group.
    /// Shading normals come from these faces only (as with `mesh`), so a
    /// vertex on a material seam is lit by its own material's faces.
    /// `normals` is scratch space reused between calls.
    pub fn faces(
        &mut self,
        positions: &[V],
        projected: &[[f32; 3]],
        faces: &[[usize; 3]],
        uvs: &[[f32; 2]],
        material: &Material,
        camera: &Camera,
        hidden: &[bool],
        normals: &mut Vec<V>,
    ) {
        normals.clear();
        normals.resize(positions.len(), V::default());
        for f in faces {
            let n = positions[f[1]]
                .sub(positions[f[0]])
                .cross(positions[f[2]].sub(positions[f[0]]));
            for &i in f {
                normals[i] = normals[i].add(n);
            }
        }
        let light = V(-0.4, 0.55, -0.9).normal();
        let shade = |i: usize| 0.48 + 0.52 * camera.rotate(normals[i].normal()).dot(light).abs();
        let [w, h] = self.size.map(|s| s as f32);
        self.triangles.reserve(faces.len());
        for f in faces {
            if f.iter().any(|&i| hidden.get(i).copied().unwrap_or(false)) {
                continue;
            }
            let vertices = f.map(|i| {
                let p = projected[i];
                [
                    p[0] / w * 2.0 - 1.0,
                    1.0 - p[1] / h * 2.0,
                    p[2],
                    uvs[i][0],
                    uvs[i][1],
                    shade(i),
                ]
            });
            // The vertex shader performs near clipping, not whole-triangle removal.
            let depth = vertices.iter().map(|v| v[2]).sum::<f32>();
            self.triangles.push(Triangle {
                vertices,
                material: material.clone(),
                depth,
            });
        }
    }
    pub fn head(
        doc: &Document,
        textures: &[Texture],
        positions: &[V],
        camera: &Camera,
        size: [usize; 2],
        hidden_parts: &[bool],
        hidden_vertices: &[bool],
        textured: bool,
    ) -> Self {
        let mut scene = Self::new(size);
        for (index, p) in doc.parts.iter().enumerate() {
            if hidden_parts.get(index).copied().unwrap_or(false) {
                continue;
            }
            static UNTEXTURED: OnceLock<Material> = OnceLock::new();
            let material = textured
                .then(|| textures.get(crate::render::texture_slot(p.material, index)))
                .flatten()
                .map(crate::render::Texture::material)
                .unwrap_or_else(|| {
                    UNTEXTURED.get_or_init(|| {
                        Material::new(Arc::new(image::RgbaImage::from_pixel(
                            1,
                            1,
                            image::Rgba([193, 170, 150, 255]),
                        )))
                    })
                });
            scene.mesh(
                &positions[p.offset..p.offset + p.base.len()],
                &p.faces,
                &p.uv,
                material.clone(),
                camera,
                hidden_vertices
                    .get(p.offset..p.offset + p.base.len())
                    .unwrap_or(&[]),
            );
        }
        scene
    }
    pub fn floor(&mut self, camera: &Camera) {
        static FLOOR: OnceLock<Material> = OnceLock::new();
        let material = FLOOR
            .get_or_init(|| {
                let logo =
                    image::load_from_memory(include_bytes!("../assets/poser/silver-spooner.png"))
                        .expect("Floor decal")
                        .to_rgba8();
                let mut image = image::RgbaImage::new(1024, 1024);
                for (y, row) in image.rows_mut().enumerate() {
                    for (x, p) in row.enumerate() {
                        let gx = x as f32 / 1023.0 * 900.0 - 450.0;
                        let gy = y as f32 / 1023.0 * 900.0 - 450.0;
                        let border = gx.abs() > 444.0 || gy.abs() > 444.0;
                        let grid = (gx + 450.0).rem_euclid(100.0) < 2.5
                            || (gy + 450.0).rem_euclid(100.0) < 2.5;
                        let mut c = if border {
                            crate::theme::FLOOR_BORDER
                        } else if grid {
                            crate::theme::FLOOR_GRID
                        } else {
                            crate::theme::FLOOR_FILL
                        }
                        .map(f32::from);
                        let (u, v) = (gx / 420.0 + 0.5, (-147.5 - gy) / 105.0);
                        if (0.0..=1.0).contains(&u) && (0.0..=1.0).contains(&v) {
                            let a = logo.get_pixel(
                                (u * (logo.width() - 1) as f32) as u32,
                                (v * (logo.height() - 1) as f32) as u32,
                            )[3] as f32
                                / 255.0
                                * 0.55;
                            for i in 0..3 {
                                c[i] = c[i] * (1.0 - a) + crate::theme::FLOOR_DECAL[i] as f32 * a;
                            }
                        }
                        *p = image::Rgba([c[0] as u8, c[1] as u8, c[2] as u8, 255]);
                    }
                }
                Material::new(Arc::new(image))
            })
            .clone();
        let p = [
            V(-450.0, -450.0, 0.0),
            V(450.0, -450.0, 0.0),
            V(450.0, 450.0, 0.0),
            V(-450.0, 450.0, 0.0),
        ]
        .map(crate::pose_render::view_point);
        let start = self.projected.len();
        self.mesh(
            &p,
            &[[0, 1, 2], [0, 2, 3]],
            &[[0.0, 0.0], [1.0, 0.0], [1.0, 1.0], [0.0, 1.0]],
            material,
            camera,
            &[],
        );
        self.projected.truncate(start);
        for t in self.triangles.iter_mut().rev().take(2) {
            for v in &mut t.vertices {
                v[5] = 1.0;
            }
        }
    }
}
struct Draw {
    first: i32,
    count: i32,
    key: u64,
    blended: bool,
}
/// The GPU preview renderer: wgpu (the main one) or OpenGL (fallback).
pub enum Renderer {
    Glow(GlowRenderer),
    Wgpu(crate::gpu_wgpu::WgpuRenderer),
}
impl Renderer {
    pub fn create_glow(gl: Arc<glow::Context>) -> Result<Shared, String> {
        GlowRenderer::create(gl).map(|r| Arc::new(Mutex::new(Renderer::Glow(r))))
    }
    pub fn create_wgpu(state: &eframe::egui_wgpu::RenderState) -> Shared {
        Arc::new(Mutex::new(Renderer::Wgpu(
            crate::gpu_wgpu::WgpuRenderer::new(
                &state.adapter,
                state.device.clone(),
                state.queue.clone(),
                Some(state.target_format),
            ),
        )))
    }
    pub fn label(&self) -> &str {
        match self {
            Renderer::Glow(r) => &r.label,
            Renderer::Wgpu(r) => &r.label,
        }
    }
    /// A driver error that should switch the app to the software preview.
    pub fn failure(&self) -> Option<String> {
        match self {
            Renderer::Glow(r) => r.failure.clone(),
            Renderer::Wgpu(r) => r.failure(),
        }
    }
    pub fn destroy(&mut self) {
        if let Renderer::Glow(r) = self {
            r.destroy();
        }
    }
}
pub struct GlowRenderer {
    gl: Arc<glow::Context>,
    program: Option<glow::Program>,
    vao: glow::VertexArray,
    vbo: glow::Buffer,
    textures: BTreeMap<u64, glow::Texture>,
    label: String,
    last_upload: u64,
    last_scene: u64,
    draws: Vec<Draw>,
    pub failure: Option<String>,
}
/// Positions arrive already projected by the CPU camera: xy in clip space
/// before perspective divide, z as view depth.
/// The shaders are written in the GLSL subset shared by desktop GLSL
/// 1.40/1.50 and GLSL ES 3.00; [`shader_header`] picks the `#version` line.
const VERTEX_SHADER: &str = r"
in vec3 position;
in vec2 uv;
in float shade;
out vec2 texcoord;
out float lighting;
void main() {
    float d = position.z;
    gl_Position = vec4(position.xy * d, 1.002002 * d - 20.02002, d);
    texcoord = uv;
    lighting = shade;
}
";
const FRAGMENT_SHADER: &str = r"
uniform sampler2D surface;
in vec2 texcoord;
in float lighting;
out vec4 color;
void main() {
    vec4 c = texture(surface, texcoord);
    if (c.a < 0.001) discard;
    color = vec4(c.rgb * lighting, c.a);
}
";
/// `#version` for the context: GLSL ES 3.00 on OpenGL ES (some Linux/
/// Wayland drivers), 1.50 on desktop OpenGL 3.2+ (macOS only offers core
/// profiles, which reject 1.40), and 1.40 on older OpenGL 3.1 drivers.
fn shader_header(version: &glow::Version) -> &'static str {
    if version.is_embedded {
        "#version 300 es
precision highp float;
"
    } else if (version.major, version.minor) >= (3, 2) {
        "#version 150
"
    } else {
        "#version 140
"
    }
}

impl GlowRenderer {
    pub fn create(gl: Arc<glow::Context>) -> Result<Self, String> {
        unsafe {
            let program = gl.create_program()?;
            let mut shaders = Vec::new();
            for (kind, source) in [
                (glow::VERTEX_SHADER, VERTEX_SHADER),
                (glow::FRAGMENT_SHADER, FRAGMENT_SHADER),
            ] {
                let shader = gl.create_shader(kind)?;
                let source = format!("{}{source}", shader_header(gl.version()));
                gl.shader_source(shader, &source);
                gl.compile_shader(shader);
                if !gl.get_shader_compile_status(shader) {
                    let error = gl.get_shader_info_log(shader);
                    gl.delete_shader(shader);
                    for s in shaders {
                        gl.delete_shader(s);
                    }
                    gl.delete_program(program);
                    return Err(error);
                }
                gl.attach_shader(program, shader);
                shaders.push(shader);
            }
            gl.bind_attrib_location(program, 0, "position");
            gl.bind_attrib_location(program, 1, "uv");
            gl.bind_attrib_location(program, 2, "shade");
            gl.link_program(program);
            for s in shaders {
                gl.detach_shader(program, s);
                gl.delete_shader(s);
            }
            if !gl.get_program_link_status(program) {
                let e = gl.get_program_info_log(program);
                gl.delete_program(program);
                return Err(e);
            }
            let vao = match gl.create_vertex_array() {
                Ok(v) => v,
                Err(e) => {
                    gl.delete_program(program);
                    return Err(e);
                }
            };
            let vbo = match gl.create_buffer() {
                Ok(v) => v,
                Err(e) => {
                    gl.delete_vertex_array(vao);
                    gl.delete_program(program);
                    return Err(e);
                }
            };
            let label = format!("GPU: {}", gl.get_parameter_string(glow::RENDERER));
            Ok(Self {
                gl,
                program: Some(program),
                vao,
                vbo,
                textures: BTreeMap::new(),
                label,
                last_upload: 0,
                last_scene: 0,
                draws: Vec::new(),
                failure: None,
            })
        }
    }
    pub fn destroy(&mut self) {
        if let Some(program) = self.program.take() {
            unsafe {
                for (_, t) in std::mem::take(&mut self.textures) {
                    self.gl.delete_texture(t);
                }
                self.gl.delete_buffer(self.vbo);
                self.gl.delete_vertex_array(self.vao);
                self.gl.delete_program(program);
            }
        }
    }
    fn draw(&mut self, scene: &Scene) {
        unsafe {
            let Some(program) = self.program else { return };
            let gl = &self.gl;
            gl.active_texture(glow::TEXTURE0);
            gl.bind_vertex_array(Some(self.vao));
            gl.bind_buffer(glow::ARRAY_BUFFER, Some(self.vbo));
            if self.last_scene != scene.id {
                let order = scene.draw_order();
                let mut floats = Vec::with_capacity(order.len() * 18);
                let mut draws: Vec<Draw> = Vec::new();
                for t in order {
                    if !self.textures.contains_key(&t.material.key) {
                        let texture = match gl.create_texture() {
                            Ok(t) => t,
                            Err(e) => {
                                self.failure = Some(e);
                                return;
                            }
                        };
                        gl.bind_texture(glow::TEXTURE_2D, Some(texture));
                        let mut bytes = t.material.image.as_raw().clone();
                        for p in bytes.as_chunks_mut::<4>().0 {
                            for i in 0..3 {
                                p[i] = (p[i] as u16 * p[3] as u16 / 255) as u8;
                            }
                        }
                        gl.pixel_store_i32(glow::UNPACK_ALIGNMENT, 1);
                        gl.tex_image_2d(
                            glow::TEXTURE_2D,
                            0,
                            glow::RGBA8 as i32,
                            t.material.image.width() as i32,
                            t.material.image.height() as i32,
                            0,
                            glow::RGBA,
                            glow::UNSIGNED_BYTE,
                            glow::PixelUnpackData::Slice(Some(&bytes)),
                        );
                        gl.generate_mipmap(glow::TEXTURE_2D);
                        for (p, v) in [
                            (glow::TEXTURE_WRAP_S, glow::REPEAT),
                            (glow::TEXTURE_WRAP_T, glow::REPEAT),
                            (glow::TEXTURE_MIN_FILTER, glow::LINEAR_MIPMAP_LINEAR),
                            (glow::TEXTURE_MAG_FILTER, glow::LINEAR),
                        ] {
                            gl.tex_parameter_i32(glow::TEXTURE_2D, p, v as i32);
                        }
                        self.textures.insert(t.material.key, texture);
                    }
                    let first = (floats.len() / 6) as i32;
                    for v in t.vertices {
                        floats.extend(v);
                    }
                    if let Some(last) = draws
                        .last_mut()
                        .filter(|d| d.key == t.material.key && d.blended == t.material.blended)
                    {
                        last.count += 3;
                    } else {
                        draws.push(Draw {
                            first,
                            count: 3,
                            key: t.material.key,
                            blended: t.material.blended,
                        });
                    }
                }
                gl.use_program(Some(program));
                gl.bind_vertex_array(Some(self.vao));
                gl.bind_buffer(glow::ARRAY_BUFFER, Some(self.vbo));
                let bytes =
                    std::slice::from_raw_parts(floats.as_ptr() as *const u8, floats.len() * 4);
                let mut hash = std::collections::hash_map::DefaultHasher::new();
                bytes.hash(&mut hash);
                let key = hash.finish();
                if key != self.last_upload {
                    gl.buffer_data_u8_slice(glow::ARRAY_BUFFER, bytes, glow::DYNAMIC_DRAW);
                    self.last_upload = key;
                }
                self.draws = draws;
                self.last_scene = scene.id;
            }
            gl.use_program(Some(program));
            for (i, n, offset) in [(0, 3, 0), (1, 2, 12), (2, 1, 20)] {
                gl.enable_vertex_attrib_array(i);
                gl.vertex_attrib_pointer_f32(i, n, glow::FLOAT, false, 24, offset);
            }
            gl.disable(glow::CULL_FACE);
            gl.enable(glow::DEPTH_TEST);
            gl.depth_func(glow::LESS);
            gl.depth_mask(true);
            gl.clear_depth_f32(1.0);
            let [r, g, b] = crate::theme::STAGE_TOP.map(|c| c as f32 / 255.0);
            gl.clear_color(r, g, b, 1.0);
            gl.clear(glow::COLOR_BUFFER_BIT | glow::DEPTH_BUFFER_BIT);
            gl.active_texture(glow::TEXTURE0);
            gl.uniform_1_i32(gl.get_uniform_location(program, "surface").as_ref(), 0);
            for d in &self.draws {
                if d.blended {
                    gl.enable(glow::BLEND);
                    gl.blend_func(glow::ONE, glow::ONE_MINUS_SRC_ALPHA);
                    gl.depth_mask(false);
                } else {
                    gl.disable(glow::BLEND);
                    gl.depth_mask(true);
                }
                gl.bind_texture(glow::TEXTURE_2D, self.textures.get(&d.key).copied());
                gl.draw_arrays(glow::TRIANGLES, d.first, d.count);
            }
            gl.depth_mask(true);
            gl.disable(glow::DEPTH_TEST);
            gl.bind_vertex_array(None);
            gl.bind_buffer(glow::ARRAY_BUFFER, None);
            gl.use_program(None);
            let error = gl.get_error();
            if error != glow::NO_ERROR {
                self.failure = Some(format!("OpenGL error 0x{error:x}"));
            }
        }
    }
}
pub fn paint(ui: &egui::Ui, rect: egui::Rect, renderer: Shared, scene: Arc<Scene>) {
    let ctx = ui.ctx().clone();
    let wgpu = renderer
        .lock()
        .is_ok_and(|r| matches!(*r, Renderer::Wgpu(_)));
    if wgpu {
        let ppp = ctx.pixels_per_point();
        let size = [
            (rect.width() * ppp).round().max(1.0) as u32,
            (rect.height() * ppp).round().max(1.0) as u32,
        ];
        ui.painter()
            .add(eframe::egui_wgpu::Callback::new_paint_callback(
                rect,
                crate::gpu_wgpu::Callback {
                    renderer,
                    scene,
                    size,
                },
            ));
        return;
    }
    ui.painter().add(egui::PaintCallback {
        rect,
        callback: Arc::new(eframe::egui_glow::CallbackFn::new(move |info, _painter| {
            let viewport = info.viewport_in_pixels();
            if let Ok(mut guard) = renderer.lock() {
                let Renderer::Glow(renderer) = &mut *guard else {
                    return;
                };
                unsafe {
                    renderer.gl.scissor(
                        viewport.left_px,
                        viewport.from_bottom_px,
                        viewport.width_px,
                        viewport.height_px,
                    );
                }
                renderer.draw(&scene);
                if renderer.failure.is_some() {
                    ctx.request_repaint();
                }
            }
        })),
    });
}

pub fn logical_size(size: egui::Vec2) -> crate::render::PreviewSize {
    let scale = (480.0 / size.x.max(size.y)).min(1.0);
    let render = [
        (size.x * scale).round().max(10.0) as usize,
        (size.y * scale).round().max(10.0) as usize,
    ];
    crate::render::PreviewSize {
        render,
        display: render,
    }
}

/// The scenes both GPU checks render at 960x960: both template heads, both
/// posed avatars, a hair close-up and a couple.
pub fn check_scenes() -> Vec<(&'static str, Scene)> {
    let skeleton = crate::pose::Skeleton::bundled();
    let pose = skeleton.rest_pose();
    let female = crate::pose_render::AvatarMesh::female();
    let male = crate::pose_render::AvatarMesh::male();
    let mut a = pose.clone();
    a.local[1].position.0 = -180.0;
    let mut b = pose.clone();
    b.local[1].position.0 = 180.0;
    let mut scenes = Vec::new();
    for female in [true, false] {
        let doc = Document::template(female);
        let textures = crate::render::textures(female);
        scenes.push((
            if female { "head-female" } else { "head-male" },
            Scene::head(
                &doc,
                &textures,
                doc.positions(),
                &Camera::default(),
                [960, 960],
                &[],
                &[],
                true,
            ),
        ));
    }
    for (name, mesh) in [("poser-female", &female), ("poser-male", &male)] {
        let camera = Camera {
            zoom: 0.8,
            ..Default::default()
        };
        scenes.push((
            name,
            crate::pose_render::AvatarMesh::gpu_scene(
                &skeleton,
                &[(mesh, &pose)],
                &camera,
                [960, 960],
            ),
        ));
    }
    let mut close = Camera {
        zoom: 3.2,
        yaw: 0.35,
        ..Default::default()
    };
    let center = close.project(
        crate::pose_render::view_point(skeleton.world(&pose)[22].position.add(V(0.0, 0.0, 75.0))),
        960,
        960,
    );
    close.pan = [480.0 - center[0], 440.0 - center[1]];
    scenes.push((
        "hair-closeup",
        crate::pose_render::AvatarMesh::gpu_scene(
            &skeleton,
            &[(&female, &pose)],
            &close,
            [960, 960],
        ),
    ));
    scenes.push((
        "couple",
        crate::pose_render::AvatarMesh::gpu_scene(
            &skeleton,
            &[(&female, &a), (&male, &b)],
            &Camera {
                zoom: 0.8,
                ..Default::default()
            },
            [960, 960],
        ),
    ));
    scenes
}

/// Actual driver/shader/FBO smoke tests, independent of a desktop screenshot.
/// Readback is deliberately limited to this diagnostic, never interactive draw.
pub fn run_checks(gl: Arc<glow::Context>, folder: &std::path::Path) -> Result<(), String> {
    unsafe {
        std::fs::create_dir_all(folder).map_err(|e| e.to_string())?;
        let renderer = Mutex::new(GlowRenderer::create(gl.clone())?);
        let mut report = String::new();
        report.push_str(&format!(
            "{}\nOpenGL {}\nNative samples: {}\n",
            renderer.lock().unwrap().label,
            gl.get_parameter_string(glow::VERSION),
            gl.get_parameter_i32(glow::SAMPLES)
        ));
        let framebuffer = gl.create_framebuffer()?;
        let color = gl.create_texture()?;
        let depth = gl.create_renderbuffer()?;
        gl.bind_framebuffer(glow::FRAMEBUFFER, Some(framebuffer));
        gl.bind_texture(glow::TEXTURE_2D, Some(color));
        gl.tex_image_2d(
            glow::TEXTURE_2D,
            0,
            glow::RGBA8 as i32,
            960,
            960,
            0,
            glow::RGBA,
            glow::UNSIGNED_BYTE,
            glow::PixelUnpackData::Slice(None),
        );
        gl.framebuffer_texture_2d(
            glow::FRAMEBUFFER,
            glow::COLOR_ATTACHMENT0,
            glow::TEXTURE_2D,
            Some(color),
            0,
        );
        gl.bind_renderbuffer(glow::RENDERBUFFER, Some(depth));
        gl.renderbuffer_storage(glow::RENDERBUFFER, glow::DEPTH_COMPONENT24, 960, 960);
        gl.framebuffer_renderbuffer(
            glow::FRAMEBUFFER,
            glow::DEPTH_ATTACHMENT,
            glow::RENDERBUFFER,
            Some(depth),
        );
        if gl.check_framebuffer_status(glow::FRAMEBUFFER) != glow::FRAMEBUFFER_COMPLETE {
            return Err("GPU diagnostic framebuffer incomplete".into());
        }
        gl.viewport(0, 0, 960, 960);
        gl.enable(glow::SCISSOR_TEST);
        gl.scissor(0, 0, 960, 960);
        let scenes = check_scenes();
        for (name, scene) in scenes {
            let start = std::time::Instant::now();
            renderer.lock().unwrap().draw(&scene);
            gl.finish();
            let ms = start.elapsed().as_secs_f64() * 1000.0;
            let cached = std::time::Instant::now();
            renderer.lock().unwrap().draw(&scene);
            gl.finish();
            let cached_ms = cached.elapsed().as_secs_f64() * 1000.0;
            if let Some(error) = renderer.lock().unwrap().failure.clone() {
                return Err(format!("{name}: {error}"));
            }
            let mut bytes = vec![0; 960 * 960 * 4];
            gl.read_pixels(
                0,
                0,
                960,
                960,
                glow::RGBA,
                glow::UNSIGNED_BYTE,
                glow::PixelPackData::Slice(Some(&mut bytes)),
            );
            let image = image::RgbaImage::from_raw(960, 960, bytes).unwrap();
            let visible = image.pixels().filter(|p| p[0] > 65 && p[0] > p[2]).count();
            if visible < 1000 {
                return Err(format!(
                    "{name}: empty/invalid GPU image ({visible} skin pixels)"
                ));
            }
            image::imageops::flip_vertical(&image)
                .save(folder.join(format!("{name}.png")))
                .map_err(|e| e.to_string())?;
            report.push_str(&format!(
                "{name}: first={ms:.2} ms, cached={cached_ms:.2} ms, {visible} skin pixels\n"
            ));
        }
        gl.bind_framebuffer(glow::FRAMEBUFFER, None);
        gl.delete_renderbuffer(depth);
        gl.delete_texture(color);
        gl.delete_framebuffer(framebuffer);
        renderer.lock().unwrap().destroy();
        std::fs::write(folder.join("report.txt"), report).map_err(|e| e.to_string())?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn viewport_projection_is_independent_of_monitor_dpi() {
        let camera = Camera {
            yaw: 0.4,
            pitch: 0.2,
            zoom: 1.2,
            ..Default::default()
        };
        let size = logical_size(egui::vec2(1700.0, 1000.0)).render;
        let positions = [V(-20.0, -10.0, 0.0), V(20.0, -10.0, 0.0), V(0.0, 20.0, 0.0)];
        let material = Material::new(Arc::new(image::RgbaImage::from_pixel(
            1,
            1,
            image::Rgba([255; 4]),
        )));
        let mut scene = Scene::new(size);
        scene.mesh(
            &positions,
            &[[0, 1, 2]],
            &[[0.0; 2]; 3],
            material,
            &camera,
            &[],
        );
        for (i, v) in scene.triangles[0].vertices.iter().enumerate() {
            let p = camera.project(positions[i], size[0], size[1]);
            assert!(((v[0] + 1.0) * 0.5 * size[0] as f32 - p[0]).abs() < 0.001);
            assert!(((1.0 - v[1]) * 0.5 * size[1] as f32 - p[1]).abs() < 0.001);
        }
    }
    #[test]
    fn opacity_classification_and_material_identity_follow_image_contents() {
        let mut image = image::RgbaImage::from_pixel(2, 2, image::Rgba([0, 0, 0, 255]));
        let opaque = Material::new(Arc::new(image.clone()));
        assert!(!opaque.blended);
        image.put_pixel(0, 0, image::Rgba([0, 0, 0, 128]));
        let alpha = Material::new(Arc::new(image));
        assert!(alpha.blended);
        assert_ne!(opaque.key, alpha.key);
    }
    // Needs the real avatars (their hair is blended); the fallback mesh has none.
    #[cfg(feature = "bundled-imvu-avatars")]
    #[test]
    fn supplied_gpu_scenes_keep_original_actor_projection_offsets() {
        let s = crate::pose::Skeleton::bundled();
        let a = s.rest_pose();
        let mut b = a.clone();
        b.local[1].position.0 += 300.0;
        let female = crate::pose_render::AvatarMesh::female();
        let male = crate::pose_render::AvatarMesh::male();
        let camera = Camera::default();
        let scene = crate::pose_render::AvatarMesh::gpu_scene(
            &s,
            &[(&female, &a), (&male, &b)],
            &camera,
            [480, 400],
        );
        assert_eq!(
            scene.projected.len(),
            female.positions.len() + male.positions.len()
        );
        let expected = camera.project(
            crate::pose_render::view_point(male.skinned(&s, &b)[0]),
            480,
            400,
        );
        assert_eq!(scene.projected[female.positions.len()], expected);
        assert!(scene.triangles.iter().any(|t| t.material.blended));
        assert!(scene.triangles.iter().any(|t| !t.material.blended));
    }
    #[test]
    fn head_picking_depth_matches_visible_surface_without_colour_raster() {
        let d = Document::template(true);
        let c = Camera::default();
        let frame = crate::render::picking_frame(&d, d.positions(), &c, [320, 320], &[], &[]);
        assert_eq!(frame.image.size, [1, 1]);
        assert_eq!(frame.projected.len(), d.positions().len());
        assert!(frame.depth[160 * 320 + 160] < f32::MAX);
        assert_eq!(frame.depth[0], f32::MAX);
    }
}

#[cfg(test)]
mod shader_tests {
    use super::*;

    fn version(major: u32, minor: u32, is_embedded: bool) -> glow::Version {
        glow::Version {
            major,
            minor,
            is_embedded,
            revision: None,
            vendor_info: String::new(),
        }
    }

    #[test]
    fn shader_version_matches_the_context() {
        assert_eq!(
            shader_header(&version(4, 6, false)),
            "#version 150
"
        );
        // macOS core profile (4.1) and plain 3.2 both get 1.50.
        assert_eq!(
            shader_header(&version(4, 1, false)),
            "#version 150
"
        );
        assert_eq!(
            shader_header(&version(3, 1, false)),
            "#version 140
"
        );
        assert!(shader_header(&version(3, 0, true)).starts_with(
            "#version 300 es
"
        ));
        // The bodies must not carry their own #version line.
        assert!(!VERTEX_SHADER.contains("#version") && !FRAGMENT_SHADER.contains("#version"));
    }
}
