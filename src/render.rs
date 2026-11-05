use crate::model::{Document, V};
use eframe::egui::{Color32, ColorImage};

/// A preview texture. Pixels never change after loading, so the alpha scan
/// and the GPU material (which hashes every byte) are computed once and reused
/// by every redraw.
pub struct Texture {
    pub pixels: std::sync::Arc<image::RgbaImage>,
    blended: std::sync::OnceLock<bool>,
    material: std::sync::OnceLock<crate::gpu::Material>,
}
impl Texture {
    pub fn new(pixels: image::RgbaImage) -> Self {
        Self::shared(std::sync::Arc::new(pixels))
    }
    pub fn shared(pixels: std::sync::Arc<image::RgbaImage>) -> Self {
        Self {
            pixels,
            blended: std::sync::OnceLock::new(),
            material: std::sync::OnceLock::new(),
        }
    }
    pub fn neutral() -> Self {
        Self::new(image::RgbaImage::from_pixel(
            1,
            1,
            image::Rgba([185, 185, 185, 255]),
        ))
    }
    /// True when any texel is not fully opaque.
    pub fn blended(&self) -> bool {
        *self
            .blended
            .get_or_init(|| self.pixels.pixels().any(|p| p[3] < 255))
    }
    pub fn material(&self) -> &crate::gpu::Material {
        self.material
            .get_or_init(|| crate::gpu::Material::new(self.pixels.clone()))
    }
    pub fn sample(&self, uv: [f32; 2]) -> [u8; 4] {
        sample_texture(&self.pixels, uv)
    }
}
pub fn texture_slot(material: usize, part: usize) -> usize {
    match material {
        2 => 0,
        3 => 1,
        5 => 2,
        6 | 8 => 3,
        _ => 4 + part,
    }
}
pub fn textures(female: bool) -> Vec<Texture> {
    crate::pose_render::head_textures(female)
}
/// Cal3D UVs tile beyond [0,1] (especially lashes and hair). Filter in
/// premultiplied alpha so transparent texels do not leave dark fringes.
pub fn sample_texture(image: &image::RgbaImage, uv: [f32; 2]) -> [u8; 4] {
    let (w, h) = image.dimensions();
    let x = uv[0].rem_euclid(1.0) * w as f32 - 0.5;
    let y = uv[1].rem_euclid(1.0) * h as f32 - 0.5;
    let ix = x.floor() as i64;
    let iy = y.floor() as i64;
    let fx = x - x.floor();
    let fy = y - y.floor();
    let mut rgb = [0.0; 3];
    let mut alpha = 0.0;
    for (dx, dy, weight) in [
        (0, 0, (1.0 - fx) * (1.0 - fy)),
        (1, 0, fx * (1.0 - fy)),
        (0, 1, (1.0 - fx) * fy),
        (1, 1, fx * fy),
    ] {
        let p = image.get_pixel(
            (ix + dx).rem_euclid(w as i64) as u32,
            (iy + dy).rem_euclid(h as i64) as u32,
        );
        let a = p[3] as f32 * weight;
        alpha += a;
        for c in 0..3 {
            rgb[c] += p[c] as f32 * a;
        }
    }
    if alpha < 0.00001 {
        return [0; 4];
    }
    [
        (rgb[0] / alpha).round() as u8,
        (rgb[1] / alpha).round() as u8,
        (rgb[2] / alpha).round() as u8,
        alpha.round() as u8,
    ]
}
#[derive(Clone)]
pub struct Camera {
    pub yaw: f32,
    pub pitch: f32,
    pub zoom: f32,
    pub pan: [f32; 2],
}
impl Default for Camera {
    fn default() -> Self {
        Self {
            yaw: 0.0,
            pitch: 0.0,
            zoom: 1.0,
            pan: [0.0, 0.0],
        }
    }
}
impl Camera {
    pub fn unrotate(&self, p: V) -> V {
        let (sy, cy) = self.yaw.sin_cos();
        let (sp, cp) = self.pitch.sin_cos();
        V(
            cy * p.0 + sy * sp * p.1 - sy * cp * p.2,
            cp * p.1 + sp * p.2,
            sy * p.0 - cy * sp * p.1 + cy * cp * p.2,
        )
    }
    pub fn rotate(&self, p: V) -> V {
        let (sy, cy) = self.yaw.sin_cos();
        let (sp, cp) = self.pitch.sin_cos();
        let x = cy * p.0 + sy * p.2;
        let z = -sy * p.0 + cy * p.2;
        V(x, cp * p.1 - sp * z, sp * p.1 + cp * z)
    }
    pub fn project(&self, p: V, w: usize, h: usize) -> [f32; 3] {
        let p = self.rotate(p.sub(V(0.0, -5.0, 0.0)));
        let depth = 360.0 + p.2;
        let scale = h as f32 * 1.2 * self.zoom / depth.max(10.0);
        [
            w as f32 * 0.5 + p.0 * scale + self.pan[0],
            h as f32 * 0.5 - p.1 * scale + self.pan[1],
            depth,
        ]
    }
    pub fn screen_delta(&self, dx: f32, dy: f32, h: f32, depth: f32) -> V {
        let scale = depth / (h * 1.2 * self.zoom);
        let (sy, cy) = self.yaw.sin_cos();
        let (sp, cp) = self.pitch.sin_cos();
        let x = dx * scale;
        let y = -dy * scale;
        V(cy * x + sy * sp * y, cp * y, sy * x - cy * sp * y)
    }
}
pub struct Frame {
    pub image: ColorImage,
    pub depth: Vec<f32>,
    pub projected: Vec<[f32; 3]>,
    pub width: usize,
    pub height: usize,
}
impl Frame {
    pub fn visible(&self, i: usize) -> bool {
        let [x, y, z] = self.projected[i];
        if x < 0.0 || y < 0.0 || x >= self.width as f32 || y >= self.height as f32 {
            return false;
        }
        let mut nearest = f32::MAX;
        for dy in -2..=2 {
            for dx in -2..=2 {
                let xx = (x as i32 + dx).clamp(0, self.width as i32 - 1) as usize;
                let yy = (y as i32 + dy).clamp(0, self.height as i32 - 1) as usize;
                nearest = nearest.min(self.depth[yy * self.width + xx]);
            }
        }
        z <= nearest + 2.0
    }
}
fn edge(a: [f32; 3], b: [f32; 3], x: f32, y: f32) -> f32 {
    (x - a[0]) * (b[1] - a[1]) - (y - a[1]) * (b[0] - a[0])
}

/// Software-preview helper: splits a `width * height` frame into horizontal
/// bands and runs `work(rows, pixels, depth)` for each band on its own thread.
/// The slices hold only that band's rows, so pixel `(x, y)` lives at
/// `(y - rows.start) * width + x`. Each band repeats the same per-pixel maths
/// in the same order, so the result matches a single-threaded pass exactly.
pub fn for_each_band<F>(size: [usize; 2], pixels: &mut [Color32], depth: &mut [f32], work: F)
where
    F: Fn(std::ops::Range<usize>, &mut [Color32], &mut [f32]) + Sync,
{
    let [w, h] = size;
    if w == 0 || h == 0 {
        return;
    }
    // Bands of at least 32 rows keep per-thread setup negligible.
    let threads = std::thread::available_parallelism()
        .map_or(1, |n| n.get())
        .min(h.div_ceil(32))
        .max(1);
    if threads == 1 {
        work(0..h, pixels, depth);
        return;
    }
    let rows = h.div_ceil(threads);
    std::thread::scope(|scope| {
        let bands = pixels.chunks_mut(rows * w).zip(depth.chunks_mut(rows * w));
        for (band, (pixels, depth)) in bands.enumerate() {
            let work = &work;
            let start = band * rows;
            scope.spawn(move || work(start..start + pixels.len() / w, pixels, depth));
        }
    });
}

/// Pixel bounds of a projected triangle, clamped to `width` columns and the
/// band's `rows`. Returns `None` when nothing is left to rasterise.
pub fn triangle_bounds(
    p: &[[f32; 3]; 3],
    width: usize,
    rows: &std::ops::Range<usize>,
) -> Option<(std::ops::Range<usize>, std::ops::Range<usize>)> {
    let (mut min_x, mut max_x, mut min_y, mut max_y) = (
        f32::INFINITY,
        f32::NEG_INFINITY,
        f32::INFINITY,
        f32::NEG_INFINITY,
    );
    for v in p {
        min_x = min_x.min(v[0]);
        max_x = max_x.max(v[0]);
        min_y = min_y.min(v[1]);
        max_y = max_y.max(v[1]);
    }
    let x0 = min_x.floor().max(0.0) as usize;
    let x1 = max_x.ceil().clamp(0.0, width as f32) as usize;
    let y0 = (min_y.floor().max(0.0) as usize).max(rows.start);
    let y1 = (max_y.ceil().max(0.0) as usize).min(rows.end);
    (x0 < x1 && y0 < y1).then_some((x0..x1, y0..y1))
}

pub fn render(
    doc: &Document,
    tex: &[Texture],
    camera: &Camera,
    size: [usize; 2],
    hidden_parts: &[bool],
    hidden_vertices: &[bool],
    textured: bool,
) -> Frame {
    render_positions(
        doc,
        tex,
        camera,
        size,
        hidden_parts,
        hidden_vertices,
        textured,
        doc.positions(),
    )
}
pub fn render_positions(
    doc: &Document,
    tex: &[Texture],
    camera: &Camera,
    size: [usize; 2],
    hidden_parts: &[bool],
    hidden_vertices: &[bool],
    textured: bool,
    positions: &[V],
) -> Frame {
    let [w, h] = size;
    let mut image = ColorImage::new(size, crate::theme::BG_DARK);
    let ps = positions;
    let projected: Vec<_> = ps.iter().map(|&p| camera.project(p, w, h)).collect();
    let mut depth = vec![f32::MAX; w * h];
    let light = V(-0.35, 0.5, -1.0).normal();
    let mut triangles = Vec::new();
    for (part_id, part) in doc.parts.iter().enumerate() {
        if hidden_parts.get(part_id).copied().unwrap_or(false) {
            continue;
        }
        let mut normals = vec![V::default(); part.base.len()];
        for f in &part.faces {
            let p = [
                ps[part.offset + f[0]],
                ps[part.offset + f[1]],
                ps[part.offset + f[2]],
            ];
            let n = p[1].sub(p[0]).cross(p[2].sub(p[0]));
            for &i in f {
                normals[i] = normals[i].add(n);
            }
        }
        let shades: Vec<_> = normals
            .iter()
            .map(|&n| 0.52 + 0.48 * camera.rotate(n.normal()).dot(light).abs())
            .collect();
        for f in &part.faces {
            let ids = [part.offset + f[0], part.offset + f[1], part.offset + f[2]];
            if ids
                .iter()
                .any(|&i| hidden_vertices.get(i).copied().unwrap_or(false))
            {
                continue;
            }
            let p = [projected[ids[0]], projected[ids[1]], projected[ids[2]]];
            if p.iter().any(|p| p[2] < 10.0) {
                continue;
            }
            triangles.push((
                p,
                [part.uv[f[0]], part.uv[f[1]], part.uv[f[2]]],
                [shades[f[0]], shades[f[1]], shades[f[2]]],
                part_id,
            ));
        }
    }
    let blended: Vec<bool> = tex.iter().map(Texture::blended).collect();
    let transparent = |part: usize| {
        blended
            .get(texture_slot(doc.parts[part].material, part))
            .copied()
            .unwrap_or(false)
    };
    // Opaque surfaces first; translucent lashes blend over completed skin.
    // Sort keys are computed once rather than inside every comparison.
    let depth_sums: Vec<f32> = triangles
        .iter()
        .map(|t| t.0.iter().map(|p| p[2]).sum::<f32>())
        .collect();
    let mut order: Vec<usize> = (0..triangles.len()).collect();
    order.sort_by(|&a, &b| {
        transparent(triangles[a].3)
            .cmp(&transparent(triangles[b].3))
            .then_with(|| depth_sums[b].total_cmp(&depth_sums[a]))
    });
    for_each_band(
        size,
        &mut image.pixels,
        &mut depth,
        |rows, pixels, depth| {
            for (y, row) in rows.clone().zip(pixels.chunks_mut(w)) {
                row.fill(crate::theme::stage_row(y, h));
            }
            for &t in &order {
                let (p, uv, shade, part_id) = triangles[t];
                let area = edge(p[0], p[1], p[2][0], p[2][1]);
                if area.abs() < 0.01 {
                    continue;
                }
                let Some((xs, ys)) = triangle_bounds(&p, w, &rows) else {
                    continue;
                };
                let slot = texture_slot(doc.parts[part_id].material, part_id);
                let writes_depth = !transparent(part_id);
                for y in ys {
                    for x in xs.clone() {
                        let a = edge(p[1], p[2], x as f32 + 0.5, y as f32 + 0.5) / area;
                        let b = edge(p[2], p[0], x as f32 + 0.5, y as f32 + 0.5) / area;
                        let c = 1.0 - a - b;
                        if a < -0.0001 || b < -0.0001 || c < -0.0001 {
                            continue;
                        }
                        let iz = a / p[0][2] + b / p[1][2] + c / p[2][2];
                        let z = 1.0 / iz;
                        let index = (y - rows.start) * w + x;
                        if z >= depth[index] {
                            continue;
                        }
                        let weights = [a / p[0][2] / iz, b / p[1][2] / iz, c / p[2][2] / iz];
                        let mut rgba = if textured {
                            let u = (0..3).map(|i| uv[i][0] * weights[i]).sum();
                            let v = (0..3).map(|i| uv[i][1] * weights[i]).sum();
                            tex.get(slot)
                                .map(|t| t.sample([u, v]))
                                .unwrap_or([185, 185, 185, 255])
                        } else {
                            match part_id {
                                0 => [193, 170, 150, 255],
                                1 => [210, 216, 221, 255],
                                _ => [65, 56, 49, 255],
                            }
                        };
                        if rgba[3] == 0 {
                            continue;
                        }
                        let brightness = (0..3).map(|i| shade[i] * weights[i]).sum::<f32>();
                        let old = pixels[index];
                        let alpha = rgba[3] as f32 / 255.0;
                        for i in 0..3 {
                            rgba[i] = (rgba[i] as f32 * brightness * alpha
                                + old[i] as f32 * (1.0 - alpha))
                                as u8;
                        }
                        pixels[index] = Color32::from_rgb(rgba[0], rgba[1], rgba[2]);
                        if writes_depth {
                            depth[index] = z;
                        }
                    }
                }
            }
        },
    );
    Frame {
        image,
        depth,
        projected,
        width: w,
        height: h,
    }
}
pub fn render_check(path: &str) -> Result<(), String> {
    std::fs::create_dir_all(path).map_err(|e| e.to_string())?;
    for female in [true, false] {
        let d = Document::template(female);
        let tex = textures(female);
        for (view, yaw) in [("front", 0.0), ("three-quarter", 0.55), ("profile", 1.57)] {
            let cam = Camera {
                yaw,
                ..Default::default()
            };
            let frame = render(
                &d,
                &tex,
                &cam,
                [640, 720],
                &[false; 4],
                &vec![false; 956],
                true,
            );
            let bytes: Vec<u8> = frame
                .image
                .pixels
                .iter()
                .flat_map(|p| p.to_array())
                .collect();
            let file = format!(
                "{path}/{}-{view}.png",
                if female { "female" } else { "male" }
            );
            image::save_buffer(&file, &bytes, 640, 720, image::ColorType::Rgba8)
                .map_err(|e| e.to_string())?;
            println!("{file}");
        }
    }
    Ok(())
}

#[cfg(test)]
mod texture_sampling_tests {
    use super::*;
    #[test]
    fn tiled_uvs_and_alpha_edges_are_filtered_without_dark_halos() {
        let mut image = image::RgbaImage::new(2, 1);
        image.put_pixel(0, 0, image::Rgba([240, 120, 60, 255]));
        image.put_pixel(1, 0, image::Rgba([0, 0, 0, 0]));
        assert_eq!(sample_texture(&image, [0.25, 0.5]), [240, 120, 60, 255]);
        assert_eq!(sample_texture(&image, [-0.75, 1.5]), [240, 120, 60, 255]);
        assert_eq!(sample_texture(&image, [2.25, -2.5]), [240, 120, 60, 255]);
        assert_eq!(sample_texture(&image, [0.5, 0.5]), [240, 120, 60, 128]);
    }
}

/// Render above physical display resolution and downsample once on redraw.
/// Uniform limits preserve aspect ratio and bound software-renderer memory.
pub struct PreviewSize {
    pub render: [usize; 2],
    pub display: [usize; 2],
}
#[derive(Default)]
pub struct PreviewBudget {
    last_input: Option<f64>,
}
impl PreviewBudget {
    pub fn choose(
        &mut self,
        ctx: &eframe::egui::Context,
        size: eframe::egui::Vec2,
        playing: bool,
    ) -> PreviewSize {
        let (now, busy) = ctx.input(|i| {
            (
                i.time,
                playing || i.pointer.any_down() || i.smooth_scroll_delta.length_sq() > 0.01,
            )
        });
        if busy {
            self.last_input = Some(now);
        }
        let remaining = self
            .last_input
            .map_or(0.0, |last| (0.18 - (now - last)).max(0.0));
        if remaining > 0.0 {
            ctx.request_repaint_after(std::time::Duration::from_secs_f64(remaining));
        }
        preview_size_for_activity(size, ctx.pixels_per_point(), busy || remaining > 0.0)
    }
}
pub fn preview_size_for_activity(
    points: eframe::egui::Vec2,
    pixels_per_point: f32,
    moving: bool,
) -> PreviewSize {
    let width = (points.x * pixels_per_point).max(10.0);
    let height = (points.y * pixels_per_point).max(10.0);
    let display_budget = if moving { 500_000.0 } else { 2_000_000.0 };
    let scale = (display_budget / (width * height)).sqrt().min(1.0);
    let display = [
        (width * scale).round().max(10.0) as usize,
        (height * scale).round().max(10.0) as usize,
    ];
    let aa = if moving { 1.0 } else { 1.5 };
    PreviewSize {
        render: [
            (display[0] as f32 * aa).round() as usize,
            (display[1] as f32 * aa).round() as usize,
        ],
        display,
    }
}
/// Downsamples a supersampled software frame for display.
///
/// This is the same separable triangle filter as
/// `image::imageops::resize(.., FilterType::Triangle)` (image 0.25): a vertical
/// pass kept in f32, then a horizontal pass rounded to the nearest byte, with
/// identical tap ranges, weights and summation order, so the output is
/// byte-identical. It runs on all cores in bands of output rows and skips the
/// intermediate RGBA buffer copies.
pub fn antialias_preview(image: ColorImage, display: [usize; 2]) -> ColorImage {
    let [sw, sh] = image.size;
    let [dw, dh] = display;
    if image.size == display || sw == 0 || sh == 0 || dw == 0 || dh == 0 {
        return if image.size == display {
            image
        } else {
            ColorImage::new(display, Color32::TRANSPARENT)
        };
    }
    let columns = triangle_taps(sw, dw);
    let rows = triangle_taps(sh, dh);
    let source = &image.pixels;
    let mut out = ColorImage::new(display, Color32::TRANSPARENT);
    let threads = std::thread::available_parallelism()
        .map_or(1, |n| n.get())
        .min(dh.div_ceil(16))
        .max(1);
    let band_rows = dh.div_ceil(threads);
    std::thread::scope(|scope| {
        for (band, pixels) in out.pixels.chunks_mut(band_rows * dw).enumerate() {
            let (rows, columns) = (&rows, &columns);
            scope.spawn(move || {
                let mut line = vec![[0.0_f32; 4]; sw];
                for (out_y, out_row) in (band * band_rows..).zip(pixels.chunks_mut(dw)) {
                    // Vertical pass for one output row (kept in f32).
                    let (top, weights) = &rows[out_y];
                    for (x, value) in line.iter_mut().enumerate() {
                        let mut t = [0.0_f32; 4];
                        for (i, w) in weights.iter().enumerate() {
                            let p = source[(top + i) * sw + x].to_array();
                            for c in 0..4 {
                                t[c] += f32::from(p[c]) * w;
                            }
                        }
                        *value = t;
                    }
                    // Horizontal pass, rounded to bytes.
                    for (out_x, pixel) in out_row.iter_mut().enumerate() {
                        let (left, weights) = &columns[out_x];
                        let mut t = [0.0_f32; 4];
                        for (i, w) in weights.iter().enumerate() {
                            let p = line[left + i];
                            for c in 0..4 {
                                t[c] += p[c] * w;
                            }
                        }
                        let [r, g, b, a] = t.map(|v| v.clamp(0.0, 255.0).round() as u8);
                        *pixel = Color32::from_rgba_unmultiplied(r, g, b, a);
                    }
                }
            });
        }
    });
    out
}

/// For each of `dst` output samples: the first source index and the
/// normalised triangle-filter weights, computed exactly as image 0.25's
/// `vertical_sample` / `horizontal_sample` do.
fn triangle_taps(src: usize, dst: usize) -> Vec<(usize, Vec<f32>)> {
    let ratio = src as f32 / dst as f32;
    let sratio = if ratio < 1.0 { 1.0 } else { ratio };
    let support = sratio; // Triangle filter support is 1.0.
    (0..dst)
        .map(|out| {
            let input = (out as f32 + 0.5) * ratio;
            let left = ((input - support).floor() as i64).clamp(0, src as i64 - 1);
            let right = ((input + support).ceil() as i64).clamp(left + 1, src as i64);
            let centre = input - 0.5;
            let mut weights: Vec<f32> = (left..right)
                .map(|i| {
                    let x = ((i as f32 - centre) / sratio).abs();
                    if x < 1.0 {
                        1.0 - x
                    } else {
                        0.0
                    }
                })
                .collect();
            let mut sum = 0.0;
            for w in &weights {
                sum += w;
            }
            for w in &mut weights {
                *w /= sum;
            }
            (left as usize, weights)
        })
        .collect()
}
#[cfg(test)]
mod preview_resolution_tests {
    use super::*;
    #[test]
    fn adaptive_views_preserve_aspect_and_bound_interactive_work() {
        for (size, dpi) in [
            (eframe::egui::vec2(1600.0, 900.0), 1.0),
            (eframe::egui::vec2(1600.0, 900.0), 2.0),
            (eframe::egui::vec2(600.0, 1600.0), 1.5),
        ] {
            let p = preview_size_for_activity(size, dpi, false);
            let moving = preview_size_for_activity(size, dpi, true);
            assert!((p.render[0] as f32 / p.render[1] as f32 - size.x / size.y).abs() < 0.003);
            assert!(p.render[0] * p.render[1] < 4_520_000);
            assert!(moving.render[0] * moving.render[1] < 502_000);
            assert_eq!(moving.render, moving.display);
            assert!(p.render[0] * p.render[1] > moving.render[0] * moving.render[1]);
        }
    }
    #[test]
    fn supersampled_edges_get_intermediate_coverage() {
        let mut source = ColorImage::new([8, 8], Color32::BLACK);
        for y in 0..8 {
            for x in 0..8 {
                if x >= y {
                    source.pixels[y * 8 + x] = Color32::WHITE;
                }
            }
        }
        let image = antialias_preview(source, [4, 4]);
        assert_eq!(image.size, [4, 4]);
        assert!(image.pixels.iter().any(|p| p.r() > 0 && p.r() < 255));
    }
    #[test]
    fn parallel_antialias_matches_image_crate_triangle_resize_exactly() {
        // Pseudo-random opaque frame plus a few translucent pixels.
        let size = [301, 170];
        let mut state = 0x2545_f491_u32;
        let mut source = ColorImage::new(size, Color32::BLACK);
        for (i, p) in source.pixels.iter_mut().enumerate() {
            state ^= state << 13;
            state ^= state >> 17;
            state ^= state << 5;
            let [r, g, b, _] = state.to_le_bytes();
            let a = if i % 97 == 0 { 128 } else { 255 };
            *p = Color32::from_rgba_unmultiplied(r, g, b, a);
        }
        // The settled 1.5x supersample ratio, an odd ratio and an upscale.
        for display in [[201, 113], [97, 61], [450, 260]] {
            let bytes: Vec<u8> = source.pixels.iter().flat_map(|p| p.to_array()).collect();
            let reference = image::imageops::resize(
                &image::RgbaImage::from_raw(size[0] as u32, size[1] as u32, bytes).unwrap(),
                display[0] as u32,
                display[1] as u32,
                image::imageops::FilterType::Triangle,
            );
            let expected = ColorImage::from_rgba_unmultiplied(display, &reference);
            let actual = antialias_preview(source.clone(), display);
            assert_eq!(actual.size, expected.size);
            assert!(actual.pixels == expected.pixels, "mismatch at {display:?}");
        }
    }
}

/// Coarse depth-only head picking; no texture sampling or colour rasterization.
pub fn picking_frame(
    doc: &Document,
    positions: &[V],
    camera: &Camera,
    size: [usize; 2],
    hidden_parts: &[bool],
    hidden: &[bool],
) -> Frame {
    let [w, h] = size;
    let projected: Vec<_> = positions.iter().map(|&p| camera.project(p, w, h)).collect();
    let mut depth = vec![f32::MAX; w * h];
    for (part_id, part) in doc.parts.iter().enumerate() {
        if hidden_parts.get(part_id).copied().unwrap_or(false) {
            continue;
        }
        for f in &part.faces {
            let ids = f.map(|i| i + part.offset);
            if ids.iter().any(|&i| hidden.get(i).copied().unwrap_or(false)) {
                continue;
            }
            let p = ids.map(|i| projected[i]);
            if p.iter().any(|p| p[2] < 10.0) {
                continue;
            }
            let area = edge(p[0], p[1], p[2][0], p[2][1]);
            if area.abs() < 0.01 {
                continue;
            }
            let x0 = p
                .iter()
                .map(|p| p[0])
                .fold(f32::INFINITY, f32::min)
                .floor()
                .clamp(0.0, w as f32) as usize;
            let x1 = p
                .iter()
                .map(|p| p[0])
                .fold(f32::NEG_INFINITY, f32::max)
                .ceil()
                .clamp(0.0, w as f32) as usize;
            let y0 = p
                .iter()
                .map(|p| p[1])
                .fold(f32::INFINITY, f32::min)
                .floor()
                .clamp(0.0, h as f32) as usize;
            let y1 = p
                .iter()
                .map(|p| p[1])
                .fold(f32::NEG_INFINITY, f32::max)
                .ceil()
                .clamp(0.0, h as f32) as usize;
            for y in y0..y1 {
                for x in x0..x1 {
                    let a = edge(p[1], p[2], x as f32 + 0.5, y as f32 + 0.5) / area;
                    let b = edge(p[2], p[0], x as f32 + 0.5, y as f32 + 0.5) / area;
                    let c = 1.0 - a - b;
                    if a >= 0.0 && b >= 0.0 && c >= 0.0 {
                        let z = 1.0 / (a / p[0][2] + b / p[1][2] + c / p[2][2]);
                        depth[y * w + x] = depth[y * w + x].min(z);
                    }
                }
            }
        }
    }
    Frame {
        image: ColorImage::new([1, 1], Color32::TRANSPARENT),
        depth,
        projected,
        width: w,
        height: h,
    }
}
