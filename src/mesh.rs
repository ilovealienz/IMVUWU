//! Static view of any Cal3D mesh (XMF text or binary CMF 919) for the
//! Packages tab: a turnable preview and OBJ export. Unlike the head editor,
//! nothing here assumes a particular topology, and nothing is recentred in
//! exported files.

use crate::model::V;
use crate::package::Package;
use eframe::egui::{Color32, ColorImage};

#[derive(Clone, Debug)]
pub struct Submesh {
    pub material: usize,
    pub positions: Vec<V>,
    pub normals: Vec<V>,
    pub uvs: Vec<[f32; 2]>,
    pub faces: Vec<[usize; 3]>,
}

#[derive(Clone, Debug)]
pub struct StaticMesh {
    pub submeshes: Vec<Submesh>,
}

const MAX_VERTICES: usize = 400_000;

fn floats<const N: usize>(text: Option<std::borrow::Cow<str>>) -> Option<[f32; N]> {
    let values: Vec<f32> = text?
        .split_whitespace()
        .map(|t| t.parse().ok())
        .collect::<Option<_>>()?;
    let array: [f32; N] = values.get(..N)?.try_into().ok()?;
    array.iter().all(|v| v.is_finite()).then_some(array)
}

impl StaticMesh {
    /// Parses XMF text or binary CMF 919 (dispatched by content).
    pub fn parse(bytes: &[u8]) -> Result<Self, String> {
        let text = if bytes.starts_with(b"CMF\0") {
            crate::cmf::decode_preview(bytes)?
        } else {
            String::from_utf8(bytes.to_vec()).map_err(|_| "not XMF text or CMF")?
        };
        let mut body = text.trim_start_matches('\u{feff}').trim();
        if body.starts_with("<?xml") {
            body = body.split_once("?>").map_or("", |(_, r)| r);
        }
        let root = xmltree::Element::parse(format!("<root>{body}</root>").as_bytes())
            .map_err(|e| e.to_string())?;
        let mesh = root.get_child("MESH").ok_or("no MESH element")?;
        let elements = |e: &xmltree::Element, name: &str| -> Vec<xmltree::Element> {
            e.children
                .iter()
                .filter_map(|n| n.as_element())
                .filter(|c| c.name == name)
                .cloned()
                .collect()
        };
        let mut submeshes = Vec::new();
        let mut total = 0;
        for sm in elements(mesh, "SUBMESH") {
            let material = sm
                .attributes
                .get("MATERIAL")
                .and_then(|m| m.parse().ok())
                .unwrap_or(0);
            let vertices = elements(&sm, "VERTEX");
            total += vertices.len();
            if total > MAX_VERTICES {
                return Err("mesh has more than 400,000 vertices".into());
            }
            let mut positions = Vec::with_capacity(vertices.len());
            let mut normals = Vec::with_capacity(vertices.len());
            let mut uvs = Vec::with_capacity(vertices.len());
            for v in &vertices {
                let p = floats::<3>(v.get_child("POS").and_then(|e| e.get_text()))
                    .ok_or("vertex without a valid POS")?;
                let n = floats::<3>(v.get_child("NORM").and_then(|e| e.get_text()))
                    .unwrap_or([0.0, 0.0, 1.0]);
                let t = floats::<2>(v.get_child("TEXCOORD").and_then(|e| e.get_text()))
                    .unwrap_or([0.0, 0.0]);
                positions.push(V(p[0], p[1], p[2]));
                normals.push(V(n[0], n[1], n[2]));
                uvs.push(t);
            }
            let mut faces = Vec::new();
            for f in elements(&sm, "FACE") {
                let ids: Vec<usize> = f
                    .attributes
                    .get("VERTEXID")
                    .ok_or("face without VERTEXID")?
                    .split_whitespace()
                    .map(|t| t.parse().map_err(|_| "bad face index"))
                    .collect::<Result<_, _>>()?;
                let face: [usize; 3] = ids
                    .get(..3)
                    .and_then(|s| s.try_into().ok())
                    .ok_or("face with fewer than 3 vertices")?;
                if face.iter().any(|&i| i >= positions.len()) {
                    return Err("face index outside its submesh".into());
                }
                faces.push(face);
            }
            submeshes.push(Submesh {
                material,
                positions,
                normals,
                uvs,
                faces,
            });
        }
        if submeshes.iter().all(|s| s.positions.is_empty()) {
            return Err("mesh has no vertices (an empty override?)".into());
        }
        Ok(Self { submeshes })
    }

    pub fn vertex_count(&self) -> usize {
        self.submeshes.iter().map(|s| s.positions.len()).sum()
    }
    pub fn face_count(&self) -> usize {
        self.submeshes.iter().map(|s| s.faces.len()).sum()
    }
    /// Axis-aligned bounds of every vertex, in native units.
    pub fn bounds(&self) -> (V, V) {
        let mut low = V(f32::MAX, f32::MAX, f32::MAX);
        let mut high = V(f32::MIN, f32::MIN, f32::MIN);
        for p in self.submeshes.iter().flat_map(|s| &s.positions) {
            low = V(low.0.min(p.0), low.1.min(p.1), low.2.min(p.2));
            high = V(high.0.max(p.0), high.1.max(p.1), high.2.max(p.2));
        }
        (low, high)
    }
}

/// Diffuse texture file used by each material ID, found the way IMVU does:
/// index.xml `Material/TextureMapN` with `<Index>` (or N) → `<Asset>` material
/// file → its "Diffuse Color" map → that file in the package. Missing links
/// (inherited from a parent product, for example) are simply absent.
pub fn diffuse_textures(package: &Package) -> std::collections::BTreeMap<usize, String> {
    let mut result = std::collections::BTreeMap::new();
    let Some(index) = package
        .files
        .iter()
        .find(|f| f.name.eq_ignore_ascii_case("index.xml"))
        .and_then(|f| f.bytes.as_ref().ok())
    else {
        return result;
    };
    let Ok(root) = xmltree::Element::parse(index.as_slice()) else {
        return result;
    };
    // Products keep `Material` at the top level; avatars nest one inside
    // each body part. Every `Material` block counts; the first mapping found
    // for a material ID wins.
    fn materials<'a>(e: &'a xmltree::Element, out: &mut Vec<&'a xmltree::Element>) {
        for child in e.children.iter().filter_map(|n| n.as_element()) {
            if child.name == "Material" {
                out.push(child);
            } else {
                materials(child, out);
            }
        }
    }
    let mut blocks = Vec::new();
    materials(&root, &mut blocks);
    for map in blocks
        .iter()
        .flat_map(|m| m.children.iter().filter_map(|n| n.as_element()))
    {
        let Some(slot) = map.name.strip_prefix("TextureMap") else {
            continue;
        };
        let id = map
            .get_child("Index")
            .and_then(|e| e.get_text())
            .and_then(|t| t.trim().parse().ok())
            .or_else(|| slot.parse().ok());
        let asset = map
            .get_child("Asset")
            .and_then(|e| e.get_text())
            .map(|t| t.trim().to_string());
        let (Some(id), Some(asset)) = (id, asset) else {
            continue;
        };
        let Some(material_file) = package.file(&asset).and_then(|f| f.bytes.as_ref().ok()) else {
            continue;
        };
        let Some(Ok(maps)) = crate::package::material_maps(material_file) else {
            continue;
        };
        if let Some(diffuse) = maps
            .iter()
            .find(|m| m.kind.eq_ignore_ascii_case("Diffuse Color") || m.kind.is_empty())
        {
            if let Some(file) = package.file(&diffuse.file) {
                result.entry(id).or_insert_with(|| file.name.clone());
            }
        }
    }
    result
}

/// How exported OBJ coordinates relate to IMVU's native ones.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ObjMode {
    /// Blender-ready: ×0.01 and written Y-up as (x, z, −y), so Blender's
    /// default OBJ import places it exactly like the .blend helper output
    /// (native Z-up × 0.01, nothing recentred).
    Blender,
    /// IMVU's native coordinates, unscaled and Z-up, exactly as in the file.
    Unmodified,
}

impl ObjMode {
    fn position(self, p: V) -> V {
        match self {
            ObjMode::Blender => V(p.0 * 0.01, p.2 * 0.01, -p.1 * 0.01),
            ObjMode::Unmodified => p,
        }
    }
    fn normal(self, n: V) -> V {
        match self {
            ObjMode::Blender => V(n.0, n.2, -n.1),
            ObjMode::Unmodified => n,
        }
    }
}

/// Name of the double-click launcher written next to an exported OBJ.
pub const BLEND_LAUNCHER: &str = "Make blend.cmd";
/// The same launcher for Linux and macOS (`sh "Make blend.sh"`).
pub const BLEND_LAUNCHER_SH: &str = "Make blend.sh";
/// Name of the Blender script the launcher runs.
pub const BLEND_SCRIPT: &str = "make_blend.py";

/// The files that turn an exported OBJ into a .blend: a Windows launcher
/// that finds Blender and runs a script which imports the OBJ with the right
/// axes, saves `<stem>.blend` beside it and leaves it open in Blender. A
/// later launch opens the saved .blend instead of importing again.
pub fn blend_launcher(stem: &str, mode: ObjMode) -> [(&'static str, String); 3] {
    // The launcher looks for Blender in BLENDER_EXE, then PATH, then the
    // newest folder under Program Files\Blender Foundation.
    let cmd = [
        "@echo off",
        "setlocal",
        "set \"BLENDER=%BLENDER_EXE%\"",
        "if not defined BLENDER for /f \"delims=\" %%B in ('where blender.exe 2^>nul') do if not defined BLENDER set \"BLENDER=%%B\"",
        "if not defined BLENDER for /f \"delims=\" %%B in ('dir /b /ad /o-n \"%ProgramFiles%\\Blender Foundation\\Blender*\" 2^>nul') do if not defined BLENDER if exist \"%ProgramFiles%\\Blender Foundation\\%%B\\blender.exe\" set \"BLENDER=%ProgramFiles%\\Blender Foundation\\%%B\\blender.exe\"",
        "if not defined BLENDER (",
        "  echo Blender was not found. Install it from blender.org, or set BLENDER_EXE to blender.exe.",
        "  pause",
        "  exit /b 1",
        ")",
        "start \"\" \"%BLENDER%\" --python \"%~dp0make_blend.py\" -- \"%~dp0.\"",
        "",
    ]
    .join("\r\n");
    // Linux/macOS: BLENDER_EXE, then PATH, then the macOS app bundle, then
    // the Flatpak. LF line endings; works from any current directory.
    let sh = [
        "#!/bin/sh",
        "# Made by IMVUWU: opens the exported OBJ in Blender (Linux/macOS).",
        "cd \"$(dirname \"$0\")\" || exit 1",
        "if [ -n \"$BLENDER_EXE\" ]; then set -- \"$BLENDER_EXE\"",
        "elif command -v blender >/dev/null 2>&1; then set -- blender",
        "elif [ -x /Applications/Blender.app/Contents/MacOS/Blender ]; then set -- /Applications/Blender.app/Contents/MacOS/Blender",
        "elif command -v flatpak >/dev/null 2>&1 && flatpak info org.blender.Blender >/dev/null 2>&1; then set -- flatpak run org.blender.Blender",
        "else echo \"Blender was not found. Install it from blender.org, or set BLENDER_EXE.\"; exit 1",
        "fi",
        "\"$@\" --python \"$PWD/make_blend.py\" -- \"$PWD\" &",
        "",
    ]
    .join("\n");
    let (forward, up, clip) = match mode {
        // Written Y-up as (x, z, -y): Blender's default import restores Z-up.
        ObjMode::Blender => ("NEGATIVE_Z", "Y", 1000.0),
        // Native coordinates are already Blender's Z-up; units are 100x larger.
        ObjMode::Unmodified => ("Y", "Z", 100000.0),
    };
    let quoted = serde_json::to_string(stem).unwrap_or_else(|_| "\"mesh\"".into());
    let script = format!(
        r#"# Made by IMVUWU: imports {stem}.obj into Blender and saves {stem}.blend.
# Run by "{BLEND_LAUNCHER}"; also works from Blender's Scripting tab.
import os
import sys

import bpy

STEM = {quoted}
FORWARD, UP = "{forward}", "{up}"

if "--" in sys.argv[1:]:
    folder = sys.argv[sys.argv.index("--") + 1]
else:
    folder = os.path.dirname(bpy.path.abspath(__file__))
folder = os.path.abspath(folder)
blend = os.path.join(folder, STEM + ".blend")

if os.path.exists(blend):
    bpy.ops.wm.open_mainfile(filepath=blend)
else:
    for obj in list(bpy.data.objects):
        bpy.data.objects.remove(obj, do_unlink=True)
    bpy.ops.wm.obj_import(
        filepath=os.path.join(folder, STEM + ".obj"),
        forward_axis=FORWARD,
        up_axis=UP,
    )
    # Bake the importer's axis rotation into the meshes: clean transforms,
    # with vertex data in Blender's Z-up like IMVU's.
    bpy.ops.object.transform_apply(location=False, rotation=True, scale=False)
    bpy.ops.object.select_all(action="DESELECT")
    bpy.ops.wm.save_as_mainfile(filepath=blend)
    print("Saved", blend)


def show_model():
    for window in bpy.context.window_manager.windows:
        for area in window.screen.areas:
            if area.type != "VIEW_3D":
                continue
            space = area.spaces.active
            space.clip_end = {clip}
            space.shading.type = "SOLID"
            space.shading.color_type = "TEXTURE"
            region = next(r for r in area.regions if r.type == "WINDOW")
            with bpy.context.temp_override(window=window, area=area, region=region):
                bpy.ops.view3d.view_all()
    return None


if not bpy.app.background:
    bpy.app.timers.register(show_model, first_interval=0.3)
"#
    );
    [
        (BLEND_LAUNCHER, cmd),
        (BLEND_LAUNCHER_SH, sh),
        (BLEND_SCRIPT, script),
    ]
}

/// OBJ text, plus MTL text when `textures` names a diffuse image for any
/// material. UV V is flipped (OBJ's origin is bottom-left) in both modes.
pub fn to_obj(
    mesh: &StaticMesh,
    name: &str,
    mode: ObjMode,
    textures: &std::collections::BTreeMap<usize, String>,
) -> (String, Option<String>) {
    use std::fmt::Write;
    let mtl_name = format!("{name}.mtl");
    let mut obj = String::new();
    let _ = writeln!(obj, "# IMVU mesh {name}");
    let _ = writeln!(
        obj,
        "# {}",
        match mode {
            ObjMode::Blender =>
                "Blender-ready: x0.01, Y-up (x, z, -y); import with Blender's default OBJ axes",
            ObjMode::Unmodified => "Unmodified: IMVU native units and Z-up axes, as in the file",
        }
    );
    let has_textures = mesh
        .submeshes
        .iter()
        .any(|s| textures.contains_key(&s.material));
    if has_textures {
        let _ = writeln!(obj, "mtllib {mtl_name}");
    }
    let mut offset = 1;
    for (index, sm) in mesh.submeshes.iter().enumerate() {
        let _ = writeln!(obj, "o submesh_{index}_mat{}", sm.material);
        for &p in &sm.positions {
            let q = mode.position(p);
            let _ = writeln!(obj, "v {} {} {}", q.0, q.1, q.2);
        }
        for &n in &sm.normals {
            let q = mode.normal(n);
            let _ = writeln!(obj, "vn {} {} {}", q.0, q.1, q.2);
        }
        for t in &sm.uvs {
            let _ = writeln!(obj, "vt {} {}", t[0], 1.0 - t[1]);
        }
        if textures.contains_key(&sm.material) {
            let _ = writeln!(obj, "usemtl material_{}", sm.material);
        }
        for f in &sm.faces {
            let [a, b, c] = f.map(|i| i + offset);
            let _ = writeln!(obj, "f {a}/{a}/{a} {b}/{b}/{b} {c}/{c}/{c}");
        }
        offset += sm.positions.len();
    }
    let mtl = has_textures.then(|| {
        let mut mtl = String::new();
        for (id, file) in textures {
            let _ = writeln!(mtl, "newmtl material_{id}\nKd 1 1 1\nmap_Kd {file}\n");
        }
        mtl
    });
    (obj, mtl)
}

/// Software preview: the whole mesh fitted to the view, turned by `yaw`
/// (around the vertical axis) and `pitch`, textured where `images` gives an
/// image for a material, flat-shaded otherwise. Native Z is up and −Y faces
/// the viewer at yaw 0.
pub fn render_preview(
    mesh: &StaticMesh,
    images: &std::collections::BTreeMap<usize, image::RgbaImage>,
    yaw: f32,
    pitch: f32,
    size: [usize; 2],
) -> ColorImage {
    let [w, h] = size;
    let mut image = ColorImage::new(size, crate::theme::BG_DARK);
    for y in 0..h {
        image.pixels[y * w..(y + 1) * w].fill(crate::theme::stage_row(y, h));
    }
    let mut depth = vec![f32::MAX; w * h];
    let (low, high) = mesh.bounds();
    let centre = V(
        (low.0 + high.0) / 2.0,
        (low.1 + high.1) / 2.0,
        (low.2 + high.2) / 2.0,
    );
    let radius = high.sub(low).len().max(1e-6) / 2.0;
    let scale = (w.min(h) as f32 * 0.45) / radius;
    let (sy, cy) = yaw.sin_cos();
    let (sp, cp) = pitch.sin_cos();
    // View space: x right, y up, z away from the viewer.
    let view = |p: V| {
        let q = p.sub(centre);
        let x = cy * q.0 - sy * q.1;
        let fwd = sy * q.0 + cy * q.1; // native +Y points away at yaw 0
        let up = q.2;
        V(x, cp * up - sp * fwd, sp * up + cp * fwd)
    };
    let light = V(-0.35, 0.55, -0.75).normal();
    for sm in &mesh.submeshes {
        let texture = images.get(&sm.material);
        let projected: Vec<[f32; 3]> = sm
            .positions
            .iter()
            .map(|&p| {
                let v = view(p);
                [
                    w as f32 / 2.0 + v.0 * scale,
                    h as f32 / 2.0 - v.1 * scale,
                    v.2,
                ]
            })
            .collect();
        for f in &sm.faces {
            let p = f.map(|i| projected[i]);
            let a = view(sm.positions[f[0]]);
            let normal = view(sm.positions[f[1]])
                .sub(a)
                .cross(view(sm.positions[f[2]]).sub(a))
                .normal();
            let shade = 0.45 + 0.55 * normal.dot(light).abs();
            let area = (p[1][0] - p[0][0]) * (p[2][1] - p[0][1])
                - (p[2][0] - p[0][0]) * (p[1][1] - p[0][1]);
            if area.abs() < 1e-6 {
                continue;
            }
            let x0 = p
                .iter()
                .map(|q| q[0])
                .fold(f32::MAX, f32::min)
                .floor()
                .max(0.0) as usize;
            let x1 = (p
                .iter()
                .map(|q| q[0])
                .fold(f32::MIN, f32::max)
                .ceil()
                .max(0.0) as usize)
                .min(w);
            let y0 = p
                .iter()
                .map(|q| q[1])
                .fold(f32::MAX, f32::min)
                .floor()
                .max(0.0) as usize;
            let y1 = (p
                .iter()
                .map(|q| q[1])
                .fold(f32::MIN, f32::max)
                .ceil()
                .max(0.0) as usize)
                .min(h);
            for y in y0..y1 {
                for x in x0..x1 {
                    let (px, py) = (x as f32 + 0.5, y as f32 + 0.5);
                    let e = |a: [f32; 3], b: [f32; 3]| {
                        (b[0] - a[0]) * (py - a[1]) - (b[1] - a[1]) * (px - a[0])
                    };
                    let b0 = e(p[1], p[2]) / area;
                    let b1 = e(p[2], p[0]) / area;
                    let b2 = 1.0 - b0 - b1;
                    if b0 < 0.0 || b1 < 0.0 || b2 < 0.0 {
                        continue;
                    }
                    let z = b0 * p[0][2] + b1 * p[1][2] + b2 * p[2][2];
                    let at = y * w + x;
                    if z >= depth[at] {
                        continue;
                    }
                    let rgba = match texture {
                        Some(t) => {
                            let uv = [0, 1].map(|k| {
                                b0 * sm.uvs[f[0]][k] + b1 * sm.uvs[f[1]][k] + b2 * sm.uvs[f[2]][k]
                            });
                            crate::render::sample_texture(t, uv)
                        }
                        None => [190, 178, 168, 255],
                    };
                    if rgba[3] < 8 {
                        continue;
                    }
                    let alpha = rgba[3] as f32 / 255.0;
                    let old = image.pixels[at];
                    let mix = |c: u8, o: u8| {
                        (c as f32 * shade * alpha + o as f32 * (1.0 - alpha)).clamp(0.0, 255.0)
                            as u8
                    };
                    image.pixels[at] = Color32::from_rgb(
                        mix(rgba[0], old.r()),
                        mix(rgba[1], old.g()),
                        mix(rgba[2], old.b()),
                    );
                    if alpha > 0.5 {
                        depth[at] = z;
                    }
                }
            }
        }
    }
    image
}

#[cfg(test)]
mod tests {
    use super::*;

    const XMF: &str = r#"<HEADER MAGIC="XMF" VERSION="919"/>
<MESH NUMSUBMESH="1">
  <SUBMESH NUMVERTICES="3" NUMFACES="1" NUMLODSTEPS="0" NUMSPRINGS="0" NUMMORPHS="0" NUMTEXCOORDS="1" MATERIAL="2">
    <VERTEX NUMINFLUENCES="0" ID="0"><POS>100 200 1000</POS><NORM>0 -1 0</NORM><TEXCOORD>0 0</TEXCOORD></VERTEX>
    <VERTEX NUMINFLUENCES="0" ID="1"><POS>110 200 1000</POS><NORM>0 -1 0</NORM><TEXCOORD>1 0</TEXCOORD></VERTEX>
    <VERTEX NUMINFLUENCES="0" ID="2"><POS>100 200 1010</POS><NORM>0 -1 0</NORM><TEXCOORD>0 1</TEXCOORD></VERTEX>
    <FACE VERTEXID="0 1 2"/>
  </SUBMESH>
</MESH>"#;

    #[test]
    fn blend_launcher_matches_the_export_mode() {
        let [(cmd_name, cmd), (sh_name, sh), (script_name, script)] =
            blend_launcher("Head \"quoted\" name", ObjMode::Blender);
        assert_eq!(
            (cmd_name, sh_name, script_name),
            (BLEND_LAUNCHER, BLEND_LAUNCHER_SH, BLEND_SCRIPT)
        );
        // The shell launcher has Unix line endings and runs the same script.
        assert!(sh.starts_with("#!/bin/sh\n") && !sh.contains('\r'));
        assert!(sh.contains("--python \"$PWD/make_blend.py\""));
        // Windows batch files need CRLF; the script path is quoted for spaces.
        assert!(cmd.contains("\r\nstart \"\" \"%BLENDER%\" --python \"%~dp0make_blend.py\""));
        assert!(!cmd.replace("\r\n", "").contains('\n'));
        assert!(script.contains(r#"STEM = "Head \"quoted\" name""#));
        assert!(script.contains(r#"FORWARD, UP = "NEGATIVE_Z", "Y""#));
        let [_, _, (_, native)] = blend_launcher("x", ObjMode::Unmodified);
        assert!(native.contains(r#"FORWARD, UP = "Y", "Z""#));
    }

    #[test]
    fn parses_xmf_and_rejects_bad_faces() {
        let mesh = StaticMesh::parse(XMF.as_bytes()).unwrap();
        assert_eq!(mesh.vertex_count(), 3);
        assert_eq!(mesh.face_count(), 1);
        assert_eq!(mesh.submeshes[0].material, 2);
        let bad = XMF.replace(r#"VERTEXID="0 1 2""#, r#"VERTEXID="0 1 9""#);
        assert!(StaticMesh::parse(bad.as_bytes()).is_err());
        assert!(StaticMesh::parse(b"<HEADER/><MESH/>").is_err());
    }

    #[test]
    fn obj_modes_match_the_blender_helper_and_the_raw_file() {
        let mesh = StaticMesh::parse(XMF.as_bytes()).unwrap();
        let none = std::collections::BTreeMap::new();
        let (blender, mtl) = to_obj(&mesh, "x", ObjMode::Blender, &none);
        assert!(mtl.is_none());
        // Native (100, 200, 1000) -> (x, z, -y) * 0.01 = (1, 10, -2).
        assert!(blender.contains("\nv 1 10 -2\n"), "{blender}");
        // Normal (0, -1, 0) -> (0, 0, 1), unscaled.
        assert!(blender.contains("\nvn 0 0 1\n"));
        // UV V flipped.
        assert!(blender.contains("\nvt 0 0\n") && blender.contains("\nvt 0 1\n"));
        let (raw, _) = to_obj(&mesh, "x", ObjMode::Unmodified, &none);
        assert!(raw.contains("\nv 100 200 1000\n"), "{raw}");
        assert!(raw.contains("\nvn 0 -1 0\n"));
        assert!(raw.contains("\nf 1/1/1 2/2/2 3/3/3\n"));
        // With a texture, an MTL names it.
        let tex = std::collections::BTreeMap::from([(2, "skin.jpg".to_string())]);
        let (obj, mtl) = to_obj(&mesh, "x", ObjMode::Blender, &tex);
        assert!(obj.contains("mtllib x.mtl") && obj.contains("usemtl material_2"));
        assert!(mtl.unwrap().contains("map_Kd skin.jpg"));
    }

    #[test]
    fn preview_draws_the_mesh() {
        let mesh = StaticMesh::parse(XMF.as_bytes()).unwrap();
        let image = render_preview(&mesh, &Default::default(), 0.0, 0.0, [64, 64]);
        let background = crate::theme::stage_row(32, 64);
        assert!(image.pixels.iter().any(|&p| p != background && p.r() > 100));
    }
}
