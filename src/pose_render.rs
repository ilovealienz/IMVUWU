//! Preview geometry from user-supplied IMVU male/female avatar packages, with
//! source inverse-bind transforms, real hand surfaces and supplied materials.
//! The old Flash wire meshes / procedural hands remain a fallback only.
//! Preview geometry and textures never change exported skeletal animation.
use crate::{
    model::V,
    pose::{Bone, Pose, Quat, Skeleton, Transform},
    render::{Camera, Frame},
};
use eframe::egui::{Color32, ColorImage};
use std::{
    collections::BTreeMap,
    io::Cursor,
    sync::{Arc, OnceLock},
};
use xmltree::{Element, XMLNode};

#[derive(Clone)]
pub struct AvatarMesh {
    pub positions: Vec<V>,
    pub weights: Vec<Vec<(usize, f32)>>,
    pub triangles: Vec<[usize; 3]>,
    colors: Vec<[u8; 3]>,
    uvs: Vec<[f32; 2]>,
    triangle_textures: Vec<Option<Arc<image::RgbaImage>>>,
    inverse_bind: Option<Vec<Transform>>,
    /// Derived from the fields above once the mesh is built; see
    /// `blended_triangles` and `gpu_groups`.
    blended: OnceLock<Vec<bool>>,
    gpu_groups: OnceLock<Vec<(crate::gpu::Material, Vec<[usize; 3]>)>>,
}

/// Native IMVU Z is vertical; native negative Y faces the preview camera.
/// This is a preview-only coordinate conversion and never changes XAF data.
pub fn view_point(native: V) -> V {
    V(native.0, native.2 - 560.0, native.1).mul(0.23)
}

// Preview-only ground at native Z=0. Its subtle grid shares avatar depth.
// Fills only `rows` of the frame; `pixels`/`depth` hold just those rows.
fn ground(
    pixels: &mut [Color32],
    depth: &mut [f32],
    camera: &Camera,
    size: [usize; 2],
    rows: std::ops::Range<usize>,
) {
    static LOGO: std::sync::OnceLock<image::RgbaImage> = std::sync::OnceLock::new();
    let logo = LOGO.get_or_init(|| {
        image::load_from_memory(include_bytes!("../assets/poser/silver-spooner.png"))
            .expect("Bundled watermark PNG")
            .to_rgba8()
    });
    let [w, h] = size;
    let origin = camera.unrotate(V(0.0, 0.0, -360.0)).add(V(0.0, -5.0, 0.0));
    let scale = h as f32 * 1.2 * camera.zoom;
    let floor_y = view_point(V::default()).1;
    let right = camera.unrotate(V(1.0, 0.0, 0.0));
    let up = camera.unrotate(V(0.0, 1.0, 0.0));
    let forward = camera.unrotate(V(0.0, 0.0, 1.0));
    for y in rows.clone() {
        for x in 0..w {
            let ray = forward
                .add(right.mul((x as f32 + 0.5 - w as f32 * 0.5 - camera.pan[0]) / scale))
                .add(up.mul(-(y as f32 + 0.5 - h as f32 * 0.5 - camera.pan[1]) / scale));
            if ray.1.abs() < 0.000001 {
                continue;
            }
            let distance = (floor_y - origin.1) / ray.1;
            if distance < 10.0 {
                continue;
            }
            let hit = origin.add(ray.mul(distance));
            let (gx, gy) = (hit.0 / 0.23, hit.2 / 0.23);
            if gx.abs() > 450.0 || gy.abs() > 450.0 {
                continue;
            }
            let border = gx.abs() > 444.0 || gy.abs() > 444.0;
            let grid = (gx + 450.0).rem_euclid(100.0) < 2.5 || (gy + 450.0).rem_euclid(100.0) < 2.5;
            let [r, g, b] = if border {
                crate::theme::FLOOR_BORDER
            } else if grid {
                crate::theme::FLOOR_GRID
            } else {
                crate::theme::FLOOR_FILL
            };
            let mut color = Color32::from_rgb(r, g, b);
            let (u, v) = (gx / 420.0 + 0.5, (-147.5 - gy) / 105.0);
            if (0.0..=1.0).contains(&u) && (0.0..=1.0).contains(&v) {
                let (px, py) = (
                    u * (logo.width() - 1) as f32,
                    v * (logo.height() - 1) as f32,
                );
                let (x0, y0) = (px.floor() as u32, py.floor() as u32);
                let (fx, fy) = (px - x0 as f32, py - y0 as f32);
                let a = |x: u32, y: u32| {
                    logo.get_pixel(x.min(logo.width() - 1), y.min(logo.height() - 1))
                        .0[3] as f32
                        / 255.0
                };
                let alpha = ((a(x0, y0) * (1.0 - fx) + a(x0 + 1, y0) * fx) * (1.0 - fy)
                    + (a(x0, y0 + 1) * (1.0 - fx) + a(x0 + 1, y0 + 1) * fx) * fy)
                    * 0.55;
                let (c, d) = (color.to_array(), crate::theme::FLOOR_DECAL.map(f32::from));
                color = Color32::from_rgb(
                    (c[0] as f32 * (1.0 - alpha) + d[0] * alpha) as u8,
                    (c[1] as f32 * (1.0 - alpha) + d[1] * alpha) as u8,
                    (c[2] as f32 * (1.0 - alpha) + d[2] * alpha) as u8,
                );
            }
            let index = (y - rows.start) * w + x;
            pixels[index] = color;
            depth[index] = distance;
        }
    }
}
fn elements<'a>(e: &'a Element, n: &'a str) -> impl Iterator<Item = &'a Element> {
    e.children
        .iter()
        .filter_map(XMLNode::as_element)
        .filter(move |e| e.name == n)
}
fn attr(e: &Element, n: &str) -> Result<usize, String> {
    e.attributes
        .get(n)
        .ok_or_else(|| format!("Missing {n}"))?
        .parse()
        .map_err(|_| format!("Invalid {n}"))
}
fn vector(s: &str) -> Result<V, String> {
    let v = s
        .split_whitespace()
        .map(str::parse::<f32>)
        .collect::<Result<Vec<_>, _>>()
        .map_err(|_| "Invalid preview coordinate")?;
    if v.len() != 3 || !v.iter().all(|n| n.is_finite()) {
        return Err("Invalid preview position".into());
    }
    Ok(V(v[0], v[1], v[2]))
}
fn asset<'a>(files: &'a [(&str, &[u8])], name: &str) -> Option<&'a [u8]> {
    files
        .iter()
        .find(|(n, _)| n.eq_ignore_ascii_case(name))
        .map(|(_, b)| *b)
}
fn image_asset<'a>(files: &'a [(&str, &[u8])], name: &str) -> Option<&'a [u8]> {
    asset(files, name).or_else(|| {
        let stem = std::path::Path::new(name).file_stem()?.to_str()?;
        files
            .iter()
            .find(|(n, _)| {
                std::path::Path::new(n)
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .is_some_and(|s| s.eq_ignore_ascii_case(stem))
                    && n.to_ascii_lowercase().ends_with(".jpg")
            })
            .map(|(_, b)| *b)
    })
}
struct Binary<'a> {
    bytes: &'a [u8],
    at: usize,
}
impl Binary<'_> {
    fn take(&mut self, n: usize) -> Result<&[u8], String> {
        let end = self.at.checked_add(n).ok_or("Avatar asset size overflow")?;
        let b = self
            .bytes
            .get(self.at..end)
            .ok_or("Truncated avatar asset")?;
        self.at = end;
        Ok(b)
    }
    fn int(&mut self) -> Result<i32, String> {
        Ok(i32::from_le_bytes(self.take(4)?.try_into().unwrap()))
    }
    fn count(&mut self, max: usize) -> Result<usize, String> {
        let n = self.int()?;
        if n < 0 || n as usize > max {
            Err("Avatar asset count out of range".into())
        } else {
            Ok(n as usize)
        }
    }
    fn float(&mut self) -> Result<f32, String> {
        let n = f32::from_le_bytes(self.take(4)?.try_into().unwrap());
        if n.is_finite() {
            Ok(n)
        } else {
            Err("Non-finite avatar asset value".into())
        }
    }
    fn vector(&mut self) -> Result<V, String> {
        Ok(V(self.float()?, self.float()?, self.float()?))
    }
    fn quat(&mut self) -> Result<Quat, String> {
        let q = Quat(self.float()?, self.float()?, self.float()?, -self.float()?);
        if !q.valid() {
            Err("Invalid avatar quaternion".into())
        } else {
            Ok(q.normalized())
        }
    }
    fn string(&mut self) -> Result<String, String> {
        let n = self.count(4096)?;
        String::from_utf8(self.take(n)?.to_vec())
            .map(|n| n.trim_end_matches('\0').to_string())
            .map_err(|_| "Invalid avatar asset string".into())
    }
}
/// CSF 919 field order follows IMVU Cal3D loader.cpp loadBinaryCoreSkeleton /
/// loadCoreBones. Both local rest transforms and inverse binds are kept.
fn source_skeleton(bytes: &[u8]) -> Result<(Skeleton, Vec<Transform>), String> {
    if bytes.len() > 2 * 1024 * 1024 {
        return Err("Avatar skeleton too large".into());
    }
    let mut r = Binary { bytes, at: 0 };
    if r.take(4)? != b"CSF\0" || r.int()? != 919 {
        return Err("Expected CSF 919 skeleton".into());
    }
    let count = r.count(512)?;
    if count == 0 {
        return Err("Avatar skeleton is empty".into());
    }
    let _ambient = r.vector()?;
    let mut bones = Vec::new();
    let mut inverse = Vec::new();
    for id in 0..count {
        let name = r.string()?;
        let translation = r.vector()?;
        let rotation = r.quat()?;
        let position = r.vector()?;
        let bind_rotation = r.quat()?;
        let parent = r.int()?;
        if parent < -1 || parent >= count as i32 || parent == id as i32 {
            return Err("Invalid avatar parent bone".into());
        }
        let _light_type = r.int()?;
        let _light_color = r.vector()?;
        let children = r.count(count)?;
        for _ in 0..children {
            if r.count(count - 1)? == id {
                return Err("Avatar bone cannot be its own child".into());
            }
        }
        bones.push(Bone {
            id,
            name,
            parent: if parent < 0 {
                None
            } else {
                Some(parent as usize)
            },
            translation,
            rotation,
        });
        inverse.push(Transform {
            position,
            rotation: bind_rotation,
        });
    }
    if r.at != bytes.len() {
        return Err("Unexpected avatar skeleton trailing data".into());
    }
    for i in 0..count {
        let mut at = Some(i);
        let mut steps = 0;
        while let Some(id) = at {
            if steps > count {
                return Err("Avatar skeleton parent cycle".into());
            }
            steps += 1;
            at = bones[id].parent;
        }
    }
    Ok((Skeleton { bones }, inverse))
}
fn material_texture(
    files: &[(&str, &[u8])],
    name: &str,
) -> Result<Option<Arc<image::RgbaImage>>, String> {
    let Some(bytes) = asset(files, name) else {
        return Ok(None);
    };
    let mut r = Binary { bytes, at: 0 };
    if r.take(4)? != b"CRF\0" || r.int()? != 919 {
        return Err("Expected CRF 919 avatar material".into());
    }
    r.take(12)?;
    let _shininess = r.float()?;
    let maps = r.count(16)?;
    let mut diffuse = None;
    let mut opacity = None;
    for _ in 0..maps {
        let filename = r.string()?;
        let kind = r.string()?;
        if kind == "Diffuse Color" {
            diffuse = Some(filename);
        } else if kind == "Opacity" {
            opacity = Some(filename);
        }
    }
    if r.at != bytes.len() {
        return Err("Unexpected avatar material trailing data".into());
    }
    let Some(diffuse) = diffuse.and_then(|n| image_asset(files, &n)) else {
        return Ok(None);
    };
    let mut image = image::load_from_memory(diffuse)
        .map_err(|e| e.to_string())?
        .into_rgba8();
    if let Some(mask) = opacity.as_ref().and_then(|n| image_asset(files, n)) {
        let alpha = image::load_from_memory(mask)
            .or_else(|_| image::load_from_memory_with_format(mask, image::ImageFormat::Tga))
            .map_err(|e| e.to_string())?
            .into_rgb8();
        let (w, h) = image.dimensions();
        let (aw, ah) = alpha.dimensions();
        for (x, y, p) in image.enumerate_pixels_mut() {
            let a = alpha.get_pixel((x * aw / w).min(aw - 1), (y * ah / h).min(ah - 1));
            p[3] = ((a[0] as u16 + a[1] as u16 + a[2] as u16) / 3) as u8;
        }
    }
    Ok(Some(Arc::new(image)))
}
/// Head material files of each gender's IMVU base avatar, in head texture
/// slot order: head skin, eyes, eyelashes, eyebrows.
fn head_material_names(female: bool) -> [&'static str; 4] {
    if female {
        [
            "female01.anime01.head01.xrf",
            "female01.anime01.eyes01.xrf",
            "female04.anime01.eyelashes02_alpha.xrf",
            "female04.anime01.eyebrows02_alpha.xrf",
        ]
    } else {
        [
            "male01.anime01.head01.xrf",
            "male01.anime01.eyes01.xrf",
            "Male03_Anime01_eyelashes02_alpha.xrf",
            "Male03_Anime01_eyebrows02_alpha.xrf",
        ]
    }
}

/// Plain, non-IMVU stand-ins used until the avatars are built: a skin tone,
/// pale eyes, and fully transparent lashes and brows.
fn placeholder_head_textures() -> Vec<crate::render::Texture> {
    [
        [193, 160, 132, 255],
        [228, 232, 236, 255],
        [0, 0, 0, 0],
        [0, 0, 0, 0],
    ]
    .into_iter()
    .map(|c| crate::render::Texture::new(image::RgbaImage::from_pixel(1, 1, image::Rgba(c))))
    .collect()
}

/// Shared defaults for HeadMaker, FaceMaker and the poser heads: the head
/// materials of the built (or bundled) IMVU avatar, else placeholders.
pub fn head_textures(female: bool) -> Vec<crate::render::Texture> {
    let Some(package) = crate::avatars::load(female) else {
        return placeholder_head_textures();
    };
    let files = package.slices();
    let mut textures = placeholder_head_textures();
    for (slot, name) in head_material_names(female).into_iter().enumerate() {
        if let Ok(Some(image)) = material_texture(&files, name) {
            textures[slot] = crate::render::Texture::shared(image);
        }
    }
    textures
}

/// Checks a candidate IMVU base avatar package: it must load as a poser
/// avatar (skeleton with the poser's bone IDs and hierarchy) and contain
/// this gender's head materials with their textures.
pub fn validate_avatar_package(files: &[(&str, &[u8])], female: bool) -> Result<(), String> {
    AvatarMesh::package(files)?;
    for name in head_material_names(female) {
        match material_texture(files, name) {
            Ok(Some(_)) => {}
            Ok(None) => return Err(format!("{name} is missing or has no texture")),
            Err(e) => return Err(format!("{name}: {e}")),
        }
    }
    Ok(())
}
impl AvatarMesh {
    pub fn bundled() -> Self {
        let mut mesh = Self::empty();
        for (source, color) in [
            (
                include_str!("../assets/poser/xmf_torso.xmf"),
                [81, 127, 157],
            ),
            (include_str!("../assets/poser/xmf_pelvis.xmf"), [57, 73, 96]),
            (
                include_str!("../assets/poser/xmf_leftLeg.xmf"),
                [57, 73, 96],
            ),
            (
                include_str!("../assets/poser/xmf_rightLeg.xmf"),
                [57, 73, 96],
            ),
            (
                include_str!("../assets/poser/xmf_leftFoot.xmf"),
                [45, 53, 66],
            ),
            (
                include_str!("../assets/poser/xmf_rightFoot.xmf"),
                [45, 53, 66],
            ),
            (
                include_str!("../assets/poser/xmf_neck.xmf"),
                [195, 167, 143],
            ),
            (
                include_str!("../assets/poser/xmf_head.xmf"),
                [195, 167, 143],
            ),
        ] {
            mesh.append_xmf(source, color)
                .expect("Bundled avatar preview mesh");
        }
        mesh.add_hand_proxies(&Skeleton::bundled());
        mesh
    }
    fn empty() -> Self {
        Self {
            positions: Vec::new(),
            weights: Vec::new(),
            triangles: Vec::new(),
            colors: Vec::new(),
            uvs: Vec::new(),
            triangle_textures: Vec::new(),
            inverse_bind: None,
            blended: OnceLock::new(),
            gpu_groups: OnceLock::new(),
        }
    }
    /// Per triangle: does its texture contain any non-opaque texel?
    /// Computed on first use, after the mesh is fully built.
    fn blended_triangles(&self) -> &[bool] {
        self.blended.get_or_init(|| {
            let mut by_texture = BTreeMap::new();
            self.triangle_textures
                .iter()
                .map(|t| {
                    t.as_ref().is_some_and(|t| {
                        *by_texture
                            .entry(Arc::as_ptr(t) as usize)
                            .or_insert_with(|| t.pixels().any(|p| p[3] < 255))
                    })
                })
                .collect()
        })
    }
    /// Triangles grouped by GPU material, in first-seen material key order.
    /// Building a material hashes its whole texture, so this is cached.
    fn gpu_groups(&self) -> &[(crate::gpu::Material, Vec<[usize; 3]>)] {
        self.gpu_groups.get_or_init(|| {
            let mut groups: BTreeMap<u64, (crate::gpu::Material, Vec<[usize; 3]>)> =
                BTreeMap::new();
            let mut cache = BTreeMap::new();
            for (i, face) in self.triangles.iter().enumerate() {
                let texture = self.triangle_textures[i].as_ref();
                let color = self.colors[i];
                let key = texture.map(|t| Arc::as_ptr(t) as usize).unwrap_or(
                    usize::MAX
                        - color[0] as usize * 65536
                        - color[1] as usize * 256
                        - color[2] as usize,
                );
                let material = cache
                    .entry(key)
                    .or_insert_with(|| {
                        crate::gpu::Material::new(texture.cloned().unwrap_or_else(|| {
                            Arc::new(image::RgbaImage::from_pixel(
                                1,
                                1,
                                image::Rgba([color[0], color[1], color[2], 255]),
                            ))
                        }))
                    })
                    .clone();
                groups
                    .entry(material.key)
                    .or_insert_with(|| (material, Vec::new()))
                    .1
                    .push(*face);
            }
            groups.into_values().collect()
        })
    }
    /// The built (or bundled) IMVU female avatar, else the plain preview mesh.
    pub fn female() -> Self {
        Self::avatar(true).unwrap_or_else(Self::bundled)
    }
    /// The built (or bundled) IMVU male avatar, else the plain preview mesh.
    pub fn male() -> Self {
        Self::avatar(false).unwrap_or_else(Self::bundled)
    }
    /// The IMVU base avatar for a gender, if built or bundled and loadable.
    pub fn avatar(female: bool) -> Option<Self> {
        let package = crate::avatars::load(female)?;
        Self::package(&package.slices()).ok()
    }
    fn package(files: &[(&str, &[u8])]) -> Result<Self, String> {
        let mut mesh = Self::empty();
        let index = asset(files, "index.xml").ok_or("Avatar package has no index.xml")?;
        let root = Element::parse(Cursor::new(index)).map_err(|e| e.to_string())?;
        let skeleton_name = root
            .get_child("Skeleton")
            .and_then(|s| s.get_child("Asset"))
            .and_then(Element::get_text)
            .ok_or("Avatar package has no skeleton")?;
        let (source, inverse) =
            source_skeleton(asset(files, &skeleton_name).ok_or("Missing avatar skeleton asset")?)?;
        let canonical = Skeleton::bundled();
        if source.bones.len() != canonical.bones.len()
            || source
                .bones
                .iter()
                .zip(&canonical.bones)
                .any(|(a, b)| a.id != b.id || a.parent != b.parent)
        {
            return Err("Avatar skeleton bone IDs/hierarchy do not match poser skeleton".into());
        }
        mesh.inverse_bind = Some(inverse);
        let parts = root
            .get_child("BodyPatternDef")
            .ok_or("Avatar package has no body parts")?;
        let mut texture_cache: BTreeMap<String, Arc<image::RgbaImage>> = BTreeMap::new();
        for part in parts.children.iter().filter_map(XMLNode::as_element) {
            let Some(mesh_name) = part
                .get_child("Mesh")
                .and_then(|m| m.get_child("Asset"))
                .and_then(Element::get_text)
            else {
                continue;
            };
            let bytes = asset(files, &mesh_name)
                .ok_or_else(|| format!("Missing avatar mesh {mesh_name}"))?;
            let source = if bytes.starts_with(b"CMF\0") {
                crate::cmf::decode_preview(bytes).map_err(|e| format!("{mesh_name}: {e}"))?
            } else {
                String::from_utf8(bytes.to_vec()).map_err(|_| "Invalid avatar mesh XML")?
            };
            let mut materials = BTreeMap::new();
            if let Some(material) = part.get_child("Material") {
                for map in material.children.iter().filter_map(XMLNode::as_element) {
                    let Some(name) = map.get_child("Asset").and_then(Element::get_text) else {
                        continue;
                    };
                    let Some(id) = map
                        .get_child("Index")
                        .and_then(Element::get_text)
                        .and_then(|n| n.parse::<usize>().ok())
                    else {
                        continue;
                    };
                    let texture = if let Some(texture) = texture_cache.get(name.as_ref()) {
                        Some(texture.clone())
                    } else {
                        let texture = material_texture(files, &name)?;
                        if let Some(texture) = &texture {
                            texture_cache.insert(name.to_string(), texture.clone());
                        }
                        texture
                    };
                    materials.insert(id, texture);
                }
            }
            mesh.append_xmf_materials(&source, [185, 164, 146], &materials)?;
        }
        if mesh.positions.is_empty() {
            return Err("Avatar package has no mesh vertices".into());
        }
        Ok(mesh)
    }
    fn append_xmf(&mut self, source: &str, color: [u8; 3]) -> Result<(), String> {
        self.append_xmf_materials(source, color, &BTreeMap::new())
    }
    fn append_xmf_materials(
        &mut self,
        source: &str,
        color: [u8; 3],
        materials: &BTreeMap<usize, Option<Arc<image::RgbaImage>>>,
    ) -> Result<(), String> {
        let root = Element::parse(Cursor::new(format!("<root>{source}</root>")))
            .map_err(|e| e.to_string())?;
        let model = root.get_child("MESH").ok_or("No preview mesh")?;
        for part in elements(model, "SUBMESH") {
            let count = attr(part, "NUMVERTICES")?;
            if count > 100_000 || self.positions.len() + count > 200_000 {
                return Err("Preview mesh too large".into());
            }
            let offset = self.positions.len();
            let mut positions = vec![V::default(); count];
            let mut weights = vec![Vec::new(); count];
            let mut seen = vec![false; count];
            let mut uvs = vec![[0.0; 2]; count];
            for vertex in elements(part, "VERTEX") {
                let id = attr(vertex, "ID")?;
                if id >= count || seen[id] {
                    return Err("Invalid preview vertex ID".into());
                }
                seen[id] = true;
                positions[id] = vector(
                    &vertex
                        .get_child("POS")
                        .and_then(Element::get_text)
                        .ok_or("Missing preview position")?,
                )?;
                if let Some(text) = vertex.get_child("TEXCOORD").and_then(Element::get_text) {
                    let v = text
                        .split_whitespace()
                        .map(str::parse::<f32>)
                        .collect::<Result<Vec<_>, _>>()
                        .map_err(|_| "Invalid avatar UV")?;
                    if v.len() != 2 || !v.iter().all(|n| n.is_finite()) {
                        return Err("Invalid avatar UV".into());
                    }
                    uvs[id] = [v[0], v[1]];
                }
                for influence in elements(vertex, "INFLUENCE") {
                    let bone = attr(influence, "ID")?;
                    let value = influence
                        .get_text()
                        .ok_or("Missing influence")?
                        .parse::<f32>()
                        .map_err(|_| "Invalid preview influence")?;
                    if !value.is_finite() || value < 0.0 || bone >= 87 {
                        return Err("Invalid preview bone weight".into());
                    }
                    weights[id].push((bone, value));
                }
                if weights[id].is_empty() {
                    weights[id].push((0, 1.0));
                }
            }
            if seen.iter().any(|s| !*s) {
                return Err("Missing preview vertices".into());
            }
            self.positions.extend(positions);
            self.weights.extend(weights);
            self.uvs.extend(uvs);
            let texture = materials.get(&attr(part, "MATERIAL")?).cloned().flatten();
            for face in elements(part, "FACE") {
                let ids = face
                    .attributes
                    .get("VERTEXID")
                    .ok_or("Missing preview face")?
                    .split_whitespace()
                    .map(str::parse::<usize>)
                    .collect::<Result<Vec<_>, _>>()
                    .map_err(|_| "Invalid preview face")?;
                if ids.len() != 3 || ids.iter().any(|i| *i >= count) {
                    return Err("Invalid preview face index".into());
                }
                self.triangles
                    .push([offset + ids[0], offset + ids[1], offset + ids[2]]);
                self.colors.push(color);
                self.triangle_textures.push(texture.clone());
            }
        }
        Ok(())
    }
    fn add_hand_proxies(&mut self, skeleton: &Skeleton) {
        let rest = skeleton.world(&skeleton.rest_pose());
        for bone in &skeleton.bones {
            let Some(parent) = bone.parent else { continue };
            if !((29..=54).contains(&bone.id) || (60..=85).contains(&bone.id)) {
                continue;
            }
            let a = rest[parent].position;
            let b = rest[bone.id].position;
            let direction = b.sub(a);
            if direction.len() < 0.1 {
                continue;
            }
            let direction = direction.normal();
            let reference = if direction.2.abs() < 0.9 {
                V(0.0, 0.0, 1.0)
            } else {
                V(0.0, 1.0, 0.0)
            };
            let u = direction.cross(reference).normal();
            let v = direction.cross(u).normal();
            let radius = if matches!(parent, 28 | 29 | 59 | 60) {
                9.0
            } else {
                4.2
            };
            let offset = self.positions.len();
            let sides = 6;
            for end in [a, b] {
                for i in 0..sides {
                    let angle = i as f32 * std::f32::consts::TAU / sides as f32;
                    self.positions.push(
                        end.add(u.mul(angle.cos() * radius))
                            .add(v.mul(angle.sin() * radius)),
                    );
                    self.weights.push(vec![(parent, 1.0)]);
                    self.uvs.push([0.0; 2]);
                }
            }
            for i in 0..sides {
                let next = (i + 1) % sides;
                for face in [
                    [offset + i, offset + next, offset + sides + i],
                    [offset + next, offset + sides + next, offset + sides + i],
                ] {
                    self.triangles.push(face);
                    self.colors.push([195, 167, 143]);
                    self.triangle_textures.push(None);
                }
            }
            for i in 1..sides - 1 {
                self.triangles.push([offset, offset + i + 1, offset + i]);
                self.colors.push([195, 167, 143]);
                self.triangle_textures.push(None);
                self.triangles
                    .push([offset + sides, offset + sides + i, offset + sides + i + 1]);
                self.colors.push([195, 167, 143]);
                self.triangle_textures.push(None);
            }
        }
    }
    pub fn skinned(&self, skeleton: &Skeleton, pose: &Pose) -> Vec<V> {
        let current = skeleton.world(pose);
        // Supplied packages carry their own inverse binds; the bundled preview
        // mesh falls back to the skeleton's rest pose, computed only if needed.
        let mut rest = None;
        let transforms: Vec<_> = current
            .iter()
            .enumerate()
            .map(|(i, &posed)| {
                let inverse = self
                    .inverse_bind
                    .as_ref()
                    .and_then(|b| b.get(i))
                    .copied()
                    .unwrap_or_else(|| {
                        rest.get_or_insert_with(|| skeleton.world(&skeleton.rest_pose()))[i]
                            .inverse()
                    });
                posed.mul(inverse)
            })
            .collect();
        self.positions
            .iter()
            .zip(&self.weights)
            .map(|(&position, weights)| {
                let mut result = V::default();
                let mut total = 0.0;
                for &(bone, weight) in weights {
                    if let Some(t) = transforms.get(bone) {
                        result = result.add(t.transform_point(position).mul(weight));
                        total += weight;
                    }
                }
                if total > 0.00001 {
                    result.mul(1.0 / total)
                } else {
                    position
                }
            })
            .collect()
    }
    pub fn gpu_scene(
        skeleton: &Skeleton,
        actors: &[(&AvatarMesh, &Pose)],
        camera: &Camera,
        size: [usize; 2],
    ) -> crate::gpu::Scene {
        let mut scene = crate::gpu::Scene::new(size);
        let mut projected = Vec::new();
        let mut normals = Vec::new();
        scene.floor(camera);
        for (mesh, pose) in actors {
            let positions: Vec<_> = mesh
                .skinned(skeleton, pose)
                .into_iter()
                .map(view_point)
                .collect();
            let start = projected.len();
            projected.extend(
                positions
                    .iter()
                    .map(|&p| camera.project(p, size[0], size[1])),
            );
            for (material, faces) in mesh.gpu_groups() {
                scene.faces(
                    &positions,
                    &projected[start..],
                    faces,
                    &mesh.uvs,
                    material,
                    camera,
                    &[],
                    &mut normals,
                );
            }
        }
        scene.projected = projected;
        scene
    }
    pub fn render(
        &self,
        skeleton: &Skeleton,
        actors: &[Pose],
        camera: &Camera,
        size: [usize; 2],
    ) -> Frame {
        let actors: Vec<_> = actors.iter().map(|pose| (self, pose)).collect();
        Self::render_actors(skeleton, &actors, camera, size)
    }
    pub fn render_actors(
        skeleton: &Skeleton,
        actors: &[(&AvatarMesh, &Pose)],
        camera: &Camera,
        size: [usize; 2],
    ) -> Frame {
        let [w, h] = size;
        let mut image = ColorImage::new(size, crate::theme::BG_DARK);
        let mut depth = vec![f32::MAX; w * h];
        let light = V(-0.4, 0.55, -0.9).normal();
        // Per-actor geometry is prepared once; rasterisation is then split
        // into row bands that each replay the same draw order.
        struct Prepared<'a> {
            mesh: &'a AvatarMesh,
            actor: usize,
            projected: Vec<[f32; 3]>,
            shades: Vec<f32>,
            blended: Vec<bool>,
            order: Vec<usize>,
        }
        let prepared: Vec<Prepared> = actors
            .iter()
            .enumerate()
            .map(|(actor, (mesh, pose))| {
                let positions: Vec<_> = mesh
                    .skinned(skeleton, pose)
                    .into_iter()
                    .map(view_point)
                    .collect();
                let projected: Vec<_> =
                    positions.iter().map(|&p| camera.project(p, w, h)).collect();
                let mut normals = vec![V::default(); positions.len()];
                for face in &mesh.triangles {
                    let normal = positions[face[1]]
                        .sub(positions[face[0]])
                        .cross(positions[face[2]].sub(positions[face[0]]));
                    for &i in face {
                        normals[i] = normals[i].add(normal);
                    }
                }
                let shades: Vec<_> = normals
                    .into_iter()
                    .map(|n| 0.48 + 0.52 * camera.rotate(n.normal()).dot(light).abs())
                    .collect();
                // Finish opaque surfaces before compositing hair/lashes. A translucent
                // strand must blend over skin, never reserve depth over the background.
                let blended = mesh.blended_triangles().to_vec();
                let depth_sums: Vec<f32> = mesh
                    .triangles
                    .iter()
                    .map(|t| t.iter().map(|&i| projected[i][2]).sum::<f32>())
                    .collect();
                let mut order: Vec<_> = (0..mesh.triangles.len()).collect();
                order.sort_by(|&a, &b| {
                    blended[a]
                        .cmp(&blended[b])
                        .then_with(|| depth_sums[b].total_cmp(&depth_sums[a]))
                });
                Prepared {
                    mesh,
                    actor,
                    projected,
                    shades,
                    blended,
                    order,
                }
            })
            .collect();
        crate::render::for_each_band(
            size,
            &mut image.pixels,
            &mut depth,
            |rows, pixels, depth| {
                for (y, row) in rows.clone().zip(pixels.chunks_mut(w)) {
                    row.fill(crate::theme::stage_row(y, h));
                }
                ground(pixels, depth, camera, size, rows.clone());
                for actor in &prepared {
                    actor.rasterise(pixels, depth, w, &rows);
                }
            },
        );
        let projected = prepared.into_iter().flat_map(|a| a.projected).collect();
        impl Prepared<'_> {
            fn rasterise(
                &self,
                pixels: &mut [Color32],
                depth: &mut [f32],
                w: usize,
                rows: &std::ops::Range<usize>,
            ) {
                let mesh = self.mesh;
                for &index in &self.order {
                    let face = &mesh.triangles[index];
                    let base_color = &mesh.colors[index];
                    let texture = mesh.triangle_textures.get(index).and_then(Option::as_ref);
                    let p = face.map(|i| self.projected[i]);
                    if p.iter()
                        .any(|p| !p.iter().all(|v| v.is_finite()) || p[2] < 10.0)
                    {
                        continue;
                    }
                    let area = edge(p[0], p[1], p[2][0], p[2][1]);
                    if area.abs() < 0.01 {
                        continue;
                    }
                    let Some((xs, ys)) = crate::render::triangle_bounds(&p, w, rows) else {
                        continue;
                    };
                    let color = if self.actor % 2 == 1 && base_color == &[81, 127, 157] {
                        [153, 95, 104]
                    } else {
                        *base_color
                    };
                    let shades = face.map(|i| self.shades[i]);
                    let writes_depth = !self.blended[index];
                    for y in ys {
                        for x in xs.clone() {
                            let a = edge(p[1], p[2], x as f32 + 0.5, y as f32 + 0.5) / area;
                            let b = edge(p[2], p[0], x as f32 + 0.5, y as f32 + 0.5) / area;
                            let c = 1.0 - a - b;
                            if a < -0.00001 || b < -0.00001 || c < -0.00001 {
                                continue;
                            }
                            let iz = a / p[0][2] + b / p[1][2] + c / p[2][2];
                            let z = 1.0 / iz;
                            let index = (y - rows.start) * w + x;
                            if z >= depth[index] {
                                continue;
                            }
                            let brightness = (a / p[0][2] * shades[0]
                                + b / p[1][2] * shades[1]
                                + c / p[2][2] * shades[2])
                                / iz;
                            let rgba = if let Some(texture) = texture {
                                let perspective =
                                    [a / p[0][2] / iz, b / p[1][2] / iz, c / p[2][2] / iz];
                                let u = (0..3)
                                    .map(|i| mesh.uvs[face[i]][0] * perspective[i])
                                    .sum::<f32>();
                                let v = (0..3)
                                    .map(|i| mesh.uvs[face[i]][1] * perspective[i])
                                    .sum::<f32>();
                                crate::render::sample_texture(texture, [u, v])
                            } else {
                                [color[0], color[1], color[2], 255]
                            };
                            if rgba[3] == 0 {
                                continue;
                            }
                            let alpha = rgba[3] as f32 / 255.0;
                            let old = pixels[index];
                            let mix = |i: usize| {
                                (rgba[i] as f32 * brightness * alpha
                                    + old[i] as f32 * (1.0 - alpha))
                                    as u8
                            };
                            pixels[index] = Color32::from_rgb(mix(0), mix(1), mix(2));
                            if writes_depth {
                                depth[index] = z;
                            }
                        }
                    }
                }
            }
        }
        Frame {
            image,
            depth,
            projected,
            width: w,
            height: h,
        }
    }
}
fn edge(a: [f32; 3], b: [f32; 3], x: f32, y: f32) -> f32 {
    (x - a[0]) * (b[1] - a[1]) - (y - a[1]) * (b[0] - a[0])
}

#[cfg(all(test, feature = "bundled-imvu-avatars"))]
mod tests {
    use super::*;
    use crate::pose::Quat;
    #[test]
    fn supplied_skeletons_keep_ids_but_need_source_inverse_bind() {
        let (source, bind) = source_skeleton(
            asset(
                crate::avatars::bundled_static(true),
                "Female04_Anime01_SkeletonMASTER.xsf",
            )
            .unwrap(),
        )
        .unwrap();
        let canonical = Skeleton::bundled();
        assert_eq!(source.bones.len(), 87);
        assert_eq!(bind.len(), 87);
        assert!(source
            .bones
            .iter()
            .zip(&canonical.bones)
            .all(|(a, b)| a.id == b.id && a.parent == b.parent));
        assert!(
            source.bones[25]
                .rotation
                .dot(canonical.bones[25].rotation)
                .abs()
                < 0.99
        );
        for (world, inverse) in source.world(&source.rest_pose()).iter().zip(bind) {
            let identity = world.mul(inverse);
            assert!(identity.position.len() < 0.01, "{:?}", identity.position);
            assert!(identity.rotation.3.abs() > 0.99999);
        }
        let male = asset(
            crate::avatars::bundled_static(false),
            "Female04_Anime01_SkeletonMASTER.xsf",
        )
        .unwrap();
        assert_eq!(
            male,
            asset(
                crate::avatars::bundled_static(true),
                "Female04_Anime01_SkeletonMASTER.xsf"
            )
            .unwrap()
        );
    }
    #[test]
    fn supplied_real_meshes_match_source_bind_and_have_weighted_fingers() {
        let (source, _) = source_skeleton(
            asset(
                crate::avatars::bundled_static(true),
                "Female04_Anime01_SkeletonMASTER.xsf",
            )
            .unwrap(),
        )
        .unwrap();
        for mesh in [AvatarMesh::female(), AvatarMesh::male()] {
            assert!(mesh.positions.len() > 2500);
            assert!(
                mesh.triangle_textures
                    .iter()
                    .filter(|t| t.is_some())
                    .count()
                    > mesh.triangles.len() / 2
            );
            let skinned = mesh.skinned(&source, &source.rest_pose());
            for (a, b) in skinned.iter().zip(&mesh.positions) {
                assert!(a.sub(*b).len() < 0.02, "{a:?} != {b:?}");
            }
            let canonical = Skeleton::bundled();
            let rest = canonical.rest_pose();
            let base = mesh.skinned(&canonical, &rest);
            let mut posed = rest;
            posed.local[32].rotation = posed.local[32]
                .rotation
                .mul(Quat::from_euler_degrees(V(0.0, 0.0, 60.0)));
            let moved = mesh.skinned(&canonical, &posed);
            assert!(mesh
                .weights
                .iter()
                .enumerate()
                .filter(|(_, w)| w.iter().any(|(b, _)| *b == 33))
                .any(|(i, _)| moved[i].sub(base[i]).len() > 1.0));
            assert!(mesh
                .weights
                .iter()
                .enumerate()
                .filter(|(_, w)| w.iter().all(|(b, _)| *b == 10 || *b == 11))
                .all(|(i, _)| moved[i].sub(base[i]).len() < 0.001));
        }
    }
    #[test]
    fn supplied_mixed_gender_couple_renders_finite_geometry() {
        let s = Skeleton::bundled();
        let female = AvatarMesh::female();
        let male = AvatarMesh::male();
        let mut a = s.rest_pose();
        let mut b = a.clone();
        a.local[1].position.0 = -160.0;
        b.local[1].position.0 = 160.0;
        let frame = AvatarMesh::render_actors(
            &s,
            &[(&female, &a), (&male, &b)],
            &Camera {
                zoom: 0.8,
                ..Default::default()
            },
            [320, 320],
        );
        assert_eq!(
            frame.projected.len(),
            female.positions.len() + male.positions.len()
        );
        assert!(frame
            .projected
            .iter()
            .all(|p| p.iter().all(|n| n.is_finite())));
        assert!(frame.depth.iter().filter(|n| **n < 400.0).count() > 500);
    }
    #[test]
    fn malformed_binary_skeleton_and_material_are_rejected() {
        let source = asset(
            crate::avatars::bundled_static(true),
            "Female04_Anime01_SkeletonMASTER.xsf",
        )
        .unwrap();
        assert!(source_skeleton(&source[..source.len() - 1]).is_err());
        let mut bad = source.to_vec();
        bad[8..12].copy_from_slice(&9999_i32.to_le_bytes());
        assert!(source_skeleton(&bad).is_err());
        let source = asset(
            crate::avatars::bundled_static(true),
            "female01.anime01.head01.xrf",
        )
        .unwrap();
        let files: [(&str, &[u8]); 1] = [("bad.xrf", &source[..source.len() - 1])];
        assert!(material_texture(&files, "bad.xrf").is_err());
    }
    #[test]
    fn rest_skinning_reproduces_original_vertices() {
        let s = Skeleton::bundled();
        let m = AvatarMesh::bundled();
        let p = m.skinned(&s, &s.rest_pose());
        assert!(m.positions.len() > 1200);
        for (a, b) in p.iter().zip(&m.positions) {
            assert!(a.sub(*b).len() < 0.002, "{a:?} != {b:?}");
        }
    }
    #[test]
    fn elbow_rotation_moves_only_descendant_weighted_vertices() {
        let s = Skeleton::bundled();
        let m = AvatarMesh::bundled();
        let mut p = s.rest_pose();
        p.local[27].rotation = p.local[27]
            .rotation
            .mul(Quat::from_euler_degrees(V(70.0, 0.0, 0.0)));
        let out = m.skinned(&s, &p);
        let mut moved = 0;
        for (i, v) in out.iter().enumerate() {
            if m.weights[i].iter().any(|(b, _)| *b == 28) && v.sub(m.positions[i]).len() > 0.01 {
                moved += 1;
            }
            if m.weights[i].iter().all(|(b, _)| *b == 9 || *b == 10) {
                assert!(v.sub(m.positions[i]).len() < 0.002);
            }
        }
        assert!(moved > 5);
    }
    #[test]
    fn couple_render_has_finite_visible_geometry() {
        let s = Skeleton::bundled();
        let m = AvatarMesh::bundled();
        let mut a = s.rest_pose();
        let mut b = a.clone();
        a.local[0].position.0 = -150.0;
        b.local[0].position.0 = 150.0;
        let f = m.render(&s, &[a, b], &Camera::default(), [320, 320]);
        assert_eq!(f.projected.len(), m.positions.len() * 2);
        assert!(f.projected.iter().all(|p| p.iter().all(|v| v.is_finite())));
        assert!(f.depth.iter().filter(|d| **d < f32::MAX).count() > 1000);
    }
}

#[cfg(all(test, feature = "bundled-imvu-avatars"))]
mod eyebrow_regressions {
    use super::*;
    #[test]
    fn supplied_brows_use_real_opacity_so_dark_hairs_stay_opaque() {
        for (files, name) in [
            (
                crate::avatars::bundled_static(false),
                "Male03_Anime01_eyebrows02_alpha.xrf",
            ),
            (
                crate::avatars::bundled_static(true),
                "female04.anime01.eyebrows02_alpha.xrf",
            ),
        ] {
            let image = material_texture(files, name).unwrap().unwrap();
            let dark = image
                .pixels()
                .filter(|p| p[3] > 220 && p[0].max(p[1]).max(p[2]) < 100)
                .count();
            assert!(dark > 100, "missing opaque dark brow hairs: {dark}");
            assert!(
                image.pixels().all(|p| p[3] == 255),
                "supplied all-white mask must keep the shaped brow mesh opaque"
            );
        }
    }
}

#[cfg(all(test, feature = "bundled-imvu-avatars"))]
mod shared_head_material_tests {
    use super::*;
    #[test]
    fn editor_defaults_match_all_four_poser_head_materials() {
        for female in [true, false] {
            let files = if female {
                crate::avatars::bundled_static(true)
            } else {
                crate::avatars::bundled_static(false)
            };
            let index = Element::parse(Cursor::new(asset(files, "index.xml").unwrap())).unwrap();
            let part = index
                .get_child("BodyPatternDef")
                .unwrap()
                .get_child("BodyPart0")
                .unwrap();
            let textures = crate::render::textures(female);
            let mut checked = 0;
            for map in part
                .get_child("Material")
                .unwrap()
                .children
                .iter()
                .filter_map(XMLNode::as_element)
            {
                let id = map
                    .get_child("Index")
                    .unwrap()
                    .get_text()
                    .unwrap()
                    .parse::<usize>()
                    .unwrap();
                let name = map.get_child("Asset").unwrap().get_text().unwrap();
                let poser = material_texture(files, &name).unwrap().unwrap();
                assert_eq!(
                    textures[crate::render::texture_slot(id, 0)].pixels.as_ref(),
                    poser.as_ref()
                );
                checked += 1;
            }
            assert_eq!(checked, 4);
        }
    }
}

#[cfg(all(test, feature = "bundled-imvu-avatars"))]
mod hair_alpha_regression {
    use super::*;
    #[test]
    fn translucent_hair_blends_without_reserving_opaque_depth() {
        let mut mesh = AvatarMesh::empty();
        mesh.positions = vec![
            V(-200.0, -50.0, 360.0),
            V(200.0, -50.0, 360.0),
            V(0.0, -50.0, 760.0),
        ];
        mesh.weights = vec![vec![]; 3];
        mesh.uvs = vec![[0.5, 0.5]; 3];
        mesh.triangles = vec![[0, 1, 2]];
        mesh.colors = vec![[0; 3]];
        mesh.triangle_textures = vec![Some(Arc::new(image::RgbaImage::from_pixel(
            1,
            1,
            image::Rgba([220, 100, 40, 192]),
        )))];
        let skeleton = Skeleton::bundled();
        let pose = skeleton.rest_pose();
        let frame = mesh.render(&skeleton, &[pose], &Camera::default(), [160, 160]);
        assert_eq!(
            frame.depth[80 * 160 + 80],
            f32::MAX,
            "translucent strands must not occlude skin in the depth buffer"
        );
        assert!(
            frame.image.pixels[80 * 160 + 80].r() > 80,
            "strand should still blend visibly"
        );
    }
}
