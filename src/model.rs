use crate::protected::{NECK, SAFE};
use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    io::Cursor,
};
use xmltree::{Element, XMLNode};
pub const BASE_MESH: &str = "Base mesh";

#[derive(Clone, Copy, Default, Debug, PartialEq)]
pub struct V(pub f32, pub f32, pub f32);
impl V {
    pub fn add(self, b: Self) -> Self {
        V(self.0 + b.0, self.1 + b.1, self.2 + b.2)
    }
    pub fn sub(self, b: Self) -> Self {
        V(self.0 - b.0, self.1 - b.1, self.2 - b.2)
    }
    pub fn mul(self, t: f32) -> Self {
        V(self.0 * t, self.1 * t, self.2 * t)
    }
    pub fn dot(self, b: Self) -> f32 {
        self.0 * b.0 + self.1 * b.1 + self.2 * b.2
    }
    pub fn cross(self, b: Self) -> Self {
        V(
            self.1 * b.2 - self.2 * b.1,
            self.2 * b.0 - self.0 * b.2,
            self.0 * b.1 - self.1 * b.0,
        )
    }
    pub fn len(self) -> f32 {
        self.dot(self).sqrt()
    }
    pub fn normal(self) -> Self {
        self.mul(1.0 / self.len().max(1e-10))
    }
    pub fn mirror(self) -> Self {
        V(-self.0, self.1, self.2)
    }
    pub fn lerp(self, b: Self, t: f32) -> Self {
        self.add(b.sub(self).mul(t))
    }
}
fn numbers(s: &str) -> Result<Vec<f32>, String> {
    s.split_whitespace()
        .map(|s| s.parse::<f32>().map_err(|_| format!("Invalid number: {s}")))
        .collect::<Result<Vec<_>, _>>()
        .and_then(|v| {
            if v.iter().all(|n| n.is_finite()) {
                Ok(v)
            } else {
                Err("Non-finite coordinate".into())
            }
        })
}
fn vector(s: &str) -> Result<V, String> {
    let v = numbers(s)?;
    if v.len() != 3 {
        return Err("Expected three coordinates".into());
    }
    Ok(V(v[0], v[1], v[2]))
}
fn to_editor(p: V) -> V {
    let x = p.0 - 0.1396;
    V(
        if x.abs() < 0.1 { 0.0 } else { x },
        p.2 - 1060.0,
        p.1 + 25.0,
    )
}
fn to_file(p: V) -> V {
    V(p.0 + 0.1396, p.2 - 25.0, p.1 + 1060.0)
}
fn text(e: &Element, name: &str) -> Result<String, String> {
    e.get_child(name)
        .and_then(|c| c.get_text())
        .map(|s| s.into_owned())
        .ok_or_else(|| format!("Missing {name}"))
}
fn attr(e: &Element, name: &str) -> Result<usize, String> {
    e.attributes
        .get(name)
        .ok_or_else(|| format!("Missing {name}"))?
        .parse()
        .map_err(|_| format!("Invalid {name}"))
}
fn children<'a>(e: &'a Element, name: &'a str) -> impl Iterator<Item = &'a Element> {
    e.children
        .iter()
        .filter_map(XMLNode::as_element)
        .filter(move |e| e.name == name)
}
fn node(name: &str, value: String) -> Element {
    let mut e = Element::new(name);
    e.children.push(XMLNode::Text(value));
    e
}
fn fmt(p: V) -> String {
    format!("{:.6} {:.6} {:.6}", p.0, p.1, p.2)
}

#[derive(Clone)]
pub struct Part {
    pub material: usize,
    pub base: Vec<V>,
    pub uv: Vec<[f32; 2]>,
    pub faces: Vec<[usize; 3]>,
    pub neighbors: Vec<Vec<usize>>,
    pub offset: usize,
}
/// Stable point thinning for the editors' "Fewer points": walks the points
/// in order and keeps one unless an already-kept point lies within `radius`
/// (mesh units). Depends only on the points, so the shown set does not
/// change with zoom or rotation. Uses a grid of `radius`-sized cells.
pub fn thin_points(points: &[V], radius: f32) -> Vec<bool> {
    let mut keep = vec![radius <= 0.0; points.len()];
    if radius <= 0.0 {
        return keep;
    }
    let cell = |p: V| {
        (
            (p.0 / radius).floor() as i32,
            (p.1 / radius).floor() as i32,
            (p.2 / radius).floor() as i32,
        )
    };
    let mut grid: std::collections::HashMap<(i32, i32, i32), Vec<usize>> = Default::default();
    for (i, &p) in points.iter().enumerate() {
        let (x, y, z) = cell(p);
        let near = (-1..=1).any(|dx| {
            (-1..=1).any(|dy| {
                (-1..=1).any(|dz| {
                    grid.get(&(x + dx, y + dy, z + dz)).is_some_and(|kept| {
                        kept.iter().any(|&j| {
                            let q = points[j];
                            let d = V(p.0 - q.0, p.1 - q.1, p.2 - q.2);
                            d.0 * d.0 + d.1 * d.1 + d.2 * d.2 < radius * radius
                        })
                    })
                })
            })
        });
        if !near {
            keep[i] = true;
            grid.entry((x, y, z)).or_default().push(i);
        }
    }
    keep
}

#[derive(Clone)]
pub struct Document {
    root: Element,
    pub parts: Vec<Part>,
    pub morphs: BTreeMap<String, Vec<V>>,
    pub active: String,
    pub edited: BTreeSet<String>,
    pub female: bool,
    pub neck_repaired: bool,
    pub neck_editing: bool,
    pub legacy: bool,
    pub neck_vertices: Vec<usize>,
    pub dirty: bool,
    pub revision: u64,
    pub saved_revision: u64,
    next_revision: u64,
    undo: Vec<History>,
    redo: Vec<History>,
}
#[derive(Clone)]
struct History {
    morph: String,
    positions: Option<Vec<V>>,
    edited: bool,
    revision: u64,
}
impl Document {
    /// The default head for new sessions, Reset head and FaceMaker. The
    /// defaults are the user's own starter heads, supplied on 4 Nov 2026:
    /// `female-default-head.xmf` (22.xmf: the female template with its neck
    /// seam repaired and a reshaped Face.Average) and `male-default-head.xmf`
    /// (MaleHead.xmf: the male template with its neck seam repaired). The
    /// original templates stay the neck-repair references in `neck.rs`.
    pub fn template(female: bool) -> Self {
        Self::parse(
            if female {
                include_str!("../assets/female-default-head.xmf")
            } else {
                include_str!("../assets/male-default-head.xmf")
            },
            female,
        )
        .expect("Bundled mesh is valid")
    }
    pub fn parse(source: &str, female: bool) -> Result<Self, String> {
        Self::parse_internal(source, female, true)
    }
    pub fn load(bytes: &[u8], female: bool) -> Result<Self, String> {
        let source = if bytes.starts_with(b"CMF\0") {
            crate::cmf::decode(bytes)?
        } else {
            String::from_utf8(bytes.to_vec())
                .map_err(|_| "Mesh is neither UTF-8 XMF nor supported binary CMF")?
        };
        Self::parse_internal(&source, female, false)
    }
    fn parse_internal(source: &str, female: bool, repair: bool) -> Result<Self, String> {
        // Cal3D XMF is an XML fragment: HEADER and MESH are sibling roots.
        let mut source = source.trim_start_matches('\u{feff}').trim().to_owned();
        if source.starts_with("<?xml") {
            let i = source.find("?>").ok_or("Unclosed XML declaration")?;
            source = source[i + 2..].to_owned();
        }
        let mut root = Element::parse(Cursor::new(format!("<root>{source}</root>")))
            .map_err(|e| e.to_string())?;
        let neck_repaired = repair && crate::neck::repair(&mut root, female);
        let mesh = root.get_child("MESH").ok_or("No MESH element")?;
        let original_layout = children(mesh, "SUBMESH")
            .map(|s| attr(s, "NUMVERTICES").unwrap_or(0))
            .collect::<Vec<_>>()
            == [755, 114, 52, 35];
        let transform = |p: V| {
            if original_layout {
                to_editor(p)
            } else {
                V(p.0 - 0.1396, p.2 - 1060.0, p.1 + 25.0)
            }
        };
        let mut parts = Vec::new();
        let mut base = Vec::new();
        let mut names = BTreeSet::new();
        for sm in children(mesh, "SUBMESH") {
            let offset = base.len();
            let count = attr(sm, "NUMVERTICES")?;
            if count > 100_000 || offset + count > 200_000 {
                return Err("Mesh exceeds 200,000-vertex limit".into());
            }
            let vs: Vec<_> = children(sm, "VERTEX").collect();
            if vs.len() != count {
                return Err("NUMVERTICES does not match mesh".into());
            }
            let mut positions = vec![V::default(); count];
            let mut uv = vec![[0.0; 2]; count];
            let mut seen = BTreeSet::new();
            for v in vs {
                let id = attr(v, "ID")?;
                if id >= count || !seen.insert(id) {
                    return Err("Invalid or repeated vertex ID".into());
                }
                positions[id] = transform(vector(&text(v, "POS")?)?);
                let t = numbers(&text(v, "TEXCOORD")?)?;
                if t.len() != 2 {
                    return Err("Invalid texture coordinate".into());
                }
                uv[id] = [t[0], t[1]];
            }
            let mut faces = Vec::new();
            let mut neighbors = vec![Vec::new(); count];
            for f in children(sm, "FACE") {
                let ids = f
                    .attributes
                    .get("VERTEXID")
                    .ok_or("Face missing VERTEXID")?
                    .split_whitespace()
                    .map(str::parse::<usize>)
                    .collect::<Result<Vec<_>, _>>()
                    .map_err(|_| "Invalid face")?;
                if ids.len() != 3 || ids.iter().any(|&i| i >= count) {
                    return Err("Face references an invalid vertex".into());
                }
                let tri = [ids[0], ids[1], ids[2]];
                for i in 0..3 {
                    for j in 0..3 {
                        if i != j && !neighbors[tri[i]].contains(&tri[j]) {
                            neighbors[tri[i]].push(tri[j]);
                        }
                    }
                }
                faces.push(tri);
            }
            if faces.len() != attr(sm, "NUMFACES")? {
                return Err("NUMFACES does not match mesh".into());
            }
            for m in children(sm, "MORPH") {
                names.insert(m.attributes.get("NAME").ok_or("Morph has no name")?.clone());
            }
            base.extend_from_slice(&positions);
            parts.push(Part {
                material: attr(sm, "MATERIAL")?,
                base: positions,
                uv,
                faces,
                neighbors,
                offset,
            });
        }
        if parts.is_empty() || parts.len() != attr(mesh, "NUMSUBMESH")? {
            return Err("Invalid submesh count".into());
        }
        let mut morphs = BTreeMap::new();
        for name in names {
            let mut positions = base.clone();
            for (part, sm) in parts.iter().zip(children(mesh, "SUBMESH")) {
                for m in children(sm, "MORPH").filter(|m| m.attributes.get("NAME") == Some(&name)) {
                    let blends: Vec<_> = children(m, "BLENDVERTEX").collect();
                    if blends.len() != attr(m, "NUMBLENDVERTS")? {
                        return Err("NUMBLENDVERTS does not match morph".into());
                    }
                    for v in blends {
                        let id = attr(v, "VERTEXID")?;
                        if id >= part.base.len() {
                            return Err("Invalid blend vertex".into());
                        }
                        positions[part.offset + id] = transform(vector(&text(v, "POSITION")?)?);
                    }
                }
            }
            morphs.insert(name, positions);
        }
        let active = if morphs.contains_key("Face.Average") {
            "Face.Average".to_string()
        } else {
            morphs.insert(BASE_MESH.into(), base.clone());
            BASE_MESH.into()
        };
        if parts.len() > 64 {
            return Err("This viewer supports up to 64 submeshes".into());
        }
        let legacy = parts.iter().map(|p| p.base.len()).collect::<Vec<_>>() == [755, 114, 52, 35];
        let neck_vertices = if legacy {
            crate::neck::BAND.to_vec()
        } else {
            children(mesh, "SUBMESH")
                .next()
                .map(|skin| {
                    children(skin, "VERTEX")
                        .filter(|v| {
                            children(v, "INFLUENCE").any(|i| {
                                matches!(
                                    i.attributes.get("ID").map(String::as_str),
                                    Some("20" | "21")
                                ) && i
                                    .get_text()
                                    .and_then(|s| s.parse::<f32>().ok())
                                    .is_some_and(|w| w >= 0.5)
                            })
                        })
                        .filter_map(|v| attr(v, "ID").ok())
                        .collect()
                })
                .unwrap_or_default()
        };
        Ok(Self {
            root,
            parts,
            morphs,
            active,
            edited: BTreeSet::new(),
            female,
            neck_repaired,
            neck_editing: false,
            legacy,
            neck_vertices,
            dirty: false,
            revision: 0,
            saved_revision: 0,
            next_revision: 1,
            undo: Vec::new(),
            redo: Vec::new(),
        })
    }
    pub fn needs_neck_repair(&self) -> bool {
        crate::neck::detect_legacy(&self.root, self.female)
    }
    pub fn repair_neck(&mut self) -> Result<(), String> {
        let mut root = self.root.clone();
        if !crate::neck::repair(&mut root, self.female) {
            return Err(
                "This mesh does not match the supported neck repair for the selected head".into(),
            );
        }
        let mut xml = Vec::new();
        for e in root.children.iter().filter_map(XMLNode::as_element) {
            e.write_with_config(
                &mut xml,
                xmltree::EmitterConfig::new().write_document_declaration(false),
            )
            .map_err(|e| e.to_string())?;
        }
        let mut fixed = Self::parse_internal(
            &String::from_utf8(xml).map_err(|e| e.to_string())?,
            self.female,
            false,
        )?;
        fixed.neck_repaired = true;
        fixed.revision = 1;
        fixed.next_revision = 2;
        fixed.dirty = true;
        *self = fixed;
        Ok(())
    }
    pub fn positions(&self) -> &[V] {
        &self.morphs[&self.active]
    }
    pub fn base(&self) -> Vec<V> {
        self.parts
            .iter()
            .flat_map(|p| p.base.iter().copied())
            .collect()
    }
    // Test helper for a test that needs the unpublished IMVU fixtures.
    #[cfg(all(test, imvu_fixtures))]
    pub fn scaled_head_positions(&self, factor: V, allow_neck: bool) -> Vec<V> {
        let ps = self.positions();
        let pivot = if !allow_neck && !self.neck_vertices.is_empty() {
            self.neck_vertices
                .iter()
                .map(|&i| ps[i])
                .fold(V::default(), V::add)
                .mul(1.0 / self.neck_vertices.len() as f32)
        } else {
            V::default()
        };
        ps.iter()
            .enumerate()
            .map(|(i, &p)| {
                if !allow_neck && self.neck_vertices.contains(&i) {
                    return p;
                }
                let d = p.sub(pivot);
                pivot.add(V(d.0 * factor.0, d.1 * factor.1, d.2 * factor.2))
            })
            .collect()
    }
    pub fn checkpoint(&mut self) {
        self.undo.push(History {
            morph: self.active.clone(),
            positions: Some(self.positions().to_vec()),
            edited: self.edited.contains(&self.active),
            revision: self.revision,
        });
        if self.undo.len() > 100 {
            self.undo.remove(0);
        }
        self.redo.clear();
    }
    pub fn change(&mut self, mut positions: Vec<V>) {
        // Defense in depth: every editing path, including whole-head scale and
        // presets, retains the body seam, even if a caller supplies new positions.
        if !self.neck_editing && positions.len() == self.positions().len() {
            for &i in &self.neck_vertices {
                positions[i] = self.positions()[i];
            }
        }
        if positions.len() == self.positions().len()
            && positions
                .iter()
                .all(|v| v.0.is_finite() && v.1.is_finite() && v.2.is_finite())
            && positions != self.positions()
        {
            if self.active == BASE_MESH {
                let delta: Vec<_> = positions
                    .iter()
                    .zip(self.positions())
                    .map(|(&a, &b)| a.sub(b))
                    .collect();
                for (name, ps) in &mut self.morphs {
                    if name != BASE_MESH {
                        for (p, &d) in ps.iter_mut().zip(&delta) {
                            *p = p.add(d);
                        }
                    }
                }
            }
            self.morphs.insert(self.active.clone(), positions);
            self.edited.insert(self.active.clone());
            self.revision = self.next_revision;
            self.next_revision += 1;
            self.dirty = self.revision != self.saved_revision;
        }
    }
    pub fn undo(&mut self, redo: bool) {
        let entry = if redo {
            self.redo.pop()
        } else {
            self.undo.pop()
        };
        if let Some(h) = entry {
            let current = History {
                morph: h.morph.clone(),
                positions: self.morphs.get(&h.morph).cloned(),
                edited: self.edited.contains(&h.morph),
                revision: self.revision,
            };
            if redo {
                self.undo.push(current)
            } else {
                self.redo.push(current)
            }
            if h.morph == BASE_MESH {
                let delta: Vec<_> = h
                    .positions
                    .as_ref()
                    .unwrap()
                    .iter()
                    .zip(&self.morphs[BASE_MESH])
                    .map(|(&a, &b)| a.sub(b))
                    .collect();
                for (name, ps) in &mut self.morphs {
                    if name != BASE_MESH {
                        for (p, &d) in ps.iter_mut().zip(&delta) {
                            *p = p.add(d);
                        }
                    }
                }
            }
            if let Some(ps) = h.positions {
                self.active = h.morph.clone();
                self.morphs.insert(h.morph.clone(), ps);
            } else {
                self.morphs.remove(&h.morph);
                self.active = if self.morphs.contains_key("Face.Average") {
                    "Face.Average".into()
                } else {
                    BASE_MESH.into()
                };
            }
            if h.edited {
                self.edited.insert(h.morph);
            } else {
                self.edited.remove(&h.morph);
            }
            self.revision = h.revision;
            self.dirty = self.revision != self.saved_revision;
        }
    }
    pub fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }
    pub fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }
    pub fn create_expression(&mut self, name: &str) -> Result<(), String> {
        if self.morphs.len() >= 256 {
            return Err("This head has reached the 256-target authoring limit".into());
        }
        if name.len() > 128
            || !name
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || "._-".contains(c))
            || ![".Exclusive", ".Clamped", ".Average", ".Additive"]
                .iter()
                .any(|s| name.ends_with(s) && name.len() > s.len())
        {
            return Err(
                "Use a unique name such as custom.Smile.Exclusive (letters, numbers, dots, - or _)"
                    .into(),
            );
        }
        if self.morphs.contains_key(name) {
            return Err("That morph already exists; use its Points button to edit it".into());
        }
        self.undo.push(History {
            morph: name.into(),
            positions: None,
            edited: false,
            revision: self.revision,
        });
        if self.undo.len() > 100 {
            self.undo.remove(0);
        }
        self.redo.clear();
        let neutral = self
            .morphs
            .get(BASE_MESH)
            .cloned()
            .unwrap_or_else(|| self.base());
        self.morphs.insert(name.into(), neutral);
        self.active = name.into();
        self.edited.insert(name.into());
        self.revision = self.next_revision;
        self.next_revision += 1;
        self.dirty = true;
        Ok(())
    }
    pub fn export(&self) -> Result<String, String> {
        let mut root = self.root.clone();
        let base = self.base();
        let base_edited = self.edited.contains(BASE_MESH);
        let new_base = if base_edited {
            self.morphs[BASE_MESH].clone()
        } else {
            base.clone()
        };
        // Cal3D ignores unknown header attributes. Record intentional neck edits
        // so loading this file does not run the legacy seam repair over them.
        let neck = &self.neck_vertices;
        if self.morphs.values().any(|ps| {
            neck.iter()
                .any(|&i| ps.get(i).is_some_and(|p| *p != base[i]))
        }) {
            if let Some(header) = root.get_mut_child("HEADER") {
                header
                    .attributes
                    .insert("HEADMAKER_NECK_EDITS".into(), "1".into());
            }
        }
        let mesh = root.get_mut_child("MESH").ok_or("No MESH")?;
        let mut part_index = 0;
        for n in &mut mesh.children {
            let Some(sm) = n.as_mut_element().filter(|e| e.name == "SUBMESH") else {
                continue;
            };
            let part = &self.parts[part_index];
            part_index += 1;
            let texture_count = attr(sm, "NUMTEXCOORDS")?;
            // A custom expression is geometry in the XMF, referenced by name in XPF.
            let existing: BTreeSet<_> = children(sm, "MORPH")
                .filter_map(|m| m.attributes.get("NAME").cloned())
                .collect();
            let first_new_id = children(sm, "MORPH")
                .filter_map(|m| attr(m, "MORPHID").ok())
                .max()
                .map_or(0, |n| n + 1);
            let new_names = self.morphs.keys().filter(|n| {
                n.as_str() != BASE_MESH && self.edited.contains(*n) && !existing.contains(*n)
            });
            for (id, name) in (first_new_id..).zip(new_names) {
                let mut morph = Element::new("MORPH");
                morph.attributes.insert("NAME".into(), name.clone());
                morph.attributes.insert("NUMBLENDVERTS".into(), "0".into());
                morph.attributes.insert("MORPHID".into(), id.to_string());
                let before_face = sm
                    .children
                    .iter()
                    .position(|n| n.as_element().is_some_and(|e| e.name == "FACE"))
                    .unwrap_or(sm.children.len());
                sm.children.insert(before_face, XMLNode::Element(morph));
            }
            sm.attributes.insert(
                "NUMMORPHS".into(),
                children(sm, "MORPH").count().to_string(),
            );
            if base_edited {
                for v in sm
                    .children
                    .iter_mut()
                    .filter_map(XMLNode::as_mut_element)
                    .filter(|v| v.name == "VERTEX")
                {
                    let id = attr(v, "ID")?;
                    let global = part.offset + id;
                    let delta = new_base[global].sub(base[global]);
                    if delta != V::default() {
                        let original = vector(&text(v, "POS")?)?;
                        if let Some(p) = v.get_mut_child("POS") {
                            p.children = vec![XMLNode::Text(fmt(
                                original.add(V(delta.0, delta.2, delta.1))
                            ))];
                        }
                    }
                }
            }
            // Leave unedited morphs, skinning, normals, materials, UVs and unknown XML intact.
            let normals: Vec<String> = children(sm, "VERTEX")
                .map(|v| text(v, "NORM").unwrap_or("0 0 1".into()))
                .collect();
            for n in &mut sm.children {
                let Some(m) = n.as_mut_element().filter(|e| e.name == "MORPH") else {
                    continue;
                };
                let name = m.attributes.get("NAME").ok_or("Morph has no name")?;
                if !self.edited.contains(name) {
                    if base_edited {
                        for v in m
                            .children
                            .iter_mut()
                            .filter_map(XMLNode::as_mut_element)
                            .filter(|v| v.name == "BLENDVERTEX")
                        {
                            let id = attr(v, "VERTEXID")?;
                            let delta = new_base[part.offset + id].sub(base[part.offset + id]);
                            if delta != V::default() {
                                let original = vector(&text(v, "POSITION")?)?;
                                if let Some(p) = v.get_mut_child("POSITION") {
                                    p.children = vec![XMLNode::Text(fmt(
                                        original.add(V(delta.0, delta.2, delta.1))
                                    ))];
                                }
                            }
                        }
                    }
                    continue;
                }
                let positions = &self.morphs[name];
                m.children
                    .retain(|n| !n.as_element().is_some_and(|e| e.name == "BLENDVERTEX"));
                let mut count = 0;
                for local in 0..part.base.len() {
                    let global = part.offset + local;
                    let p = positions[global];
                    let delta = p.sub(new_base[global]);
                    if delta.len() < 0.00001 {
                        continue;
                    }
                    let mut blend = Element::new("BLENDVERTEX");
                    blend
                        .attributes
                        .insert("VERTEXID".into(), local.to_string());
                    blend
                        .attributes
                        .insert("POSDIFF".into(), format!("{:.6}", delta.len()));
                    blend
                        .children
                        .push(XMLNode::Element(node("POSITION", fmt(to_file(p)))));
                    blend
                        .children
                        .push(XMLNode::Element(node("NORMAL", normals[local].clone())));
                    for _ in 0..texture_count {
                        blend
                            .children
                            .push(XMLNode::Element(node("TEXCOORD", "0 0".into())));
                    }
                    m.children.push(XMLNode::Element(blend));
                    count += 1;
                }
                m.attributes
                    .insert("NUMBLENDVERTS".into(), count.to_string());
            }
        }
        crate::xmf::order_vertex_fields(&mut root);
        crate::xmf::validate(&root)?;
        let config = xmltree::EmitterConfig::new()
            .perform_indent(true)
            .line_separator("\r\n")
            .pad_self_closing(false)
            .write_document_declaration(false);
        let mut bytes = Vec::new();
        for n in &root.children {
            if let Some(e) = n.as_element() {
                e.write_with_config(&mut bytes, config.clone())
                    .map_err(|e| e.to_string())?;
                bytes.extend_from_slice(b"\r\n");
            }
        }
        String::from_utf8(bytes).map_err(|e| e.to_string())
    }
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Falloff {
    Smooth,
    Linear,
    EaseIn,
    EaseOut,
    Flat,
    All,
}
#[derive(Clone)]
pub struct Selection {
    pub seeds: Vec<usize>,
    pub weights: Vec<f32>,
    pub hidden: Vec<bool>,
    pub editable: Vec<bool>,
    pub radius: f32,
    pub symmetric: bool,
    pub critical: bool,
    pub single_part: bool,
    pub edge_limit: bool,
    pub edge_steps: usize,
    pub falloff: Falloff,
    female_neck_band: bool,
    pub allow_neck: bool,
    legacy_points: bool,
    neck_points: Vec<usize>,
}
impl Selection {
    pub fn new(n: usize) -> Self {
        Self {
            seeds: Vec::new(),
            weights: vec![0.0; n],
            hidden: vec![false; n],
            editable: vec![true; n],
            radius: 18.0,
            symmetric: true,
            critical: true,
            single_part: false,
            edge_limit: false,
            edge_steps: 3,
            falloff: Falloff::Smooth,
            female_neck_band: false,
            allow_neck: false,
            legacy_points: true,
            neck_points: crate::neck::VERTICES.to_vec(),
        }
    }
    pub fn locked(&self, i: usize) -> bool {
        if !self.editable.get(i).copied().unwrap_or(false) {
            return true;
        }
        // Recovered Flash IDs belong to DisplayObject3D instances. Twenty-eight
        // objects preceded vertex zero; they are not one-based vertex indexes.
        let neck = self.neck_points.contains(&i)
            || (self.legacy_points
                && (NECK.contains(&(i + 28))
                    || (self.female_neck_band && crate::neck::BAND.contains(&i))));
        if neck {
            !self.allow_neck
        } else {
            self.legacy_points && self.critical && SAFE.contains(&(i + 28))
        }
    }
    pub fn update(&mut self, doc: &Document) {
        self.female_neck_band = doc.legacy;
        self.legacy_points = doc.legacy;
        self.neck_points = doc.neck_vertices.clone();
        let ps = doc.positions();
        self.weights.fill(0.0);
        let Some(&seed) = self.seeds.last() else {
            return;
        };
        let seed_part = doc
            .parts
            .iter()
            .position(|p| seed >= p.offset && seed < p.offset + p.base.len());
        let mut reachable = BTreeSet::new();
        if self.edge_limit {
            let mut queue: VecDeque<_> = self.seeds.iter().map(|&s| (s, 0)).collect();
            while let Some((i, depth)) = queue.pop_front() {
                if !reachable.insert(i) || depth >= self.edge_steps {
                    continue;
                }
                if let Some(p) = doc
                    .parts
                    .iter()
                    .find(|p| i >= p.offset && i < p.offset + p.base.len())
                {
                    for &n in &p.neighbors[i - p.offset] {
                        queue.push_back((p.offset + n, depth + 1));
                    }
                }
            }
        }
        for (part_id, part) in doc.parts.iter().enumerate() {
            for j in 0..part.base.len() {
                let i = part.offset + j;
                if self.locked(i)
                    || self.hidden[i]
                    || (self.single_part && Some(part_id) != seed_part)
                {
                    continue;
                }
                if self.edge_limit && !reachable.contains(&i) {
                    continue;
                }
                if self.radius == 0.0 && !self.symmetric {
                    if self.seeds.contains(&i) {
                        self.weights[i] = 1.0;
                    }
                    continue;
                }
                let mut distance = f32::MAX;
                for &s in &self.seeds {
                    distance = distance.min(ps[i].sub(ps[s]).len());
                    if self.symmetric {
                        distance = distance.min(ps[i].sub(ps[s].mirror()).len());
                    }
                }
                if self.falloff != Falloff::All && distance > self.radius {
                    continue;
                }
                let t = distance / (self.radius + 1.0);
                let w = match self.falloff {
                    Falloff::Smooth => (1.0 + (t * std::f32::consts::PI).cos()) / 2.0,
                    Falloff::Linear => 1.0 - t,
                    Falloff::EaseIn => 1.0 - t * t,
                    Falloff::EaseOut => 1.0 - t.sqrt(),
                    _ => 1.0,
                };
                self.weights[i] = w.clamp(0.0, 1.0);
            }
        }
    }
    pub fn translate(&self, before: &[V], delta: V) -> Vec<V> {
        let seed = self.seeds.last().map(|&i| before[i]).unwrap_or_default();
        before
            .iter()
            .enumerate()
            .map(|(i, &p)| {
                let mut d = delta;
                if self.symmetric {
                    d.0 *= if p.0.abs() < 0.01 {
                        0.0
                    } else if p.0 * seed.0 < 0.0 {
                        -1.0
                    } else {
                        1.0
                    };
                }
                p.add(d.mul(self.weights[i]))
            })
            .collect()
    }
}

pub fn presets() -> Vec<(String, Vec<V>)> {
    let root =
        Element::parse(Cursor::new(include_str!("../assets/ethno2.xml"))).expect("Bundled presets");
    children(&root, "head")
        .map(|e| {
            let ns: Vec<f32> = e
                .get_text()
                .unwrap_or_default()
                .split(',')
                .map(|n| n.trim().parse().expect("Preset number"))
                .collect();
            (
                e.attributes["name"].clone(),
                ns.as_chunks::<3>()
                    .0
                    .iter()
                    .map(|n| V(n[0], n[2], n[1]))
                    .collect(),
            )
        })
        .collect()
}

#[cfg(test)]
mod tests {
    #[test]
    fn thinning_keeps_far_points_and_drops_close_ones() {
        let points = [
            V(0.0, 0.0, 0.0),
            V(0.5, 0.0, 0.0),
            V(3.0, 0.0, 0.0),
            V(3.0, 0.9, 0.0),
        ];
        assert_eq!(thin_points(&points, 1.0), vec![true, false, true, false]);
        assert_eq!(thin_points(&points, 0.0), vec![true; 4]);
        // The default head: Fewer keeps about half, Fewest about a quarter.
        let base = Document::template(true).base();
        let (lo, hi) = base.iter().fold((f32::MAX, f32::MIN), |(lo, hi), p| {
            (lo.min(p.0), hi.max(p.0))
        });
        let kept = |f: f32| {
            thin_points(&base, f * (hi - lo))
                .iter()
                .filter(|k| **k)
                .count()
        };
        assert!(kept(0.04) * 10 < base.len() * 6, "{}", kept(0.04));
        assert!(kept(0.08) * 10 < base.len() * 3, "{}", kept(0.08));
    }
    use super::*;
    // Uses IMVU head meshes from tests/fixtures, which are not published.
    #[cfg(imvu_fixtures)]
    #[test]
    fn expression_export_keeps_imported_bone_bindings_and_head_geometry() {
        let files: [&[u8]; 7] = [
            include_bytes!("../tests/fixtures/Head-Srs.cmf"),
            include_bytes!("../tests/fixtures/anime07.cmf"),
            include_bytes!("../tests/fixtures/anime08.cmf"),
            include_bytes!("../tests/fixtures/anime-plus.cmf"),
            include_bytes!("../tests/fixtures/male-default.cmf"),
            include_bytes!("../assets/NewExportHead6.xmf"),
            include_bytes!("../assets/MaleHeadStart_9b.xmf"),
        ];
        fn bindings(d: &Document) -> Vec<String> {
            children(d.root.get_child("MESH").unwrap(), "SUBMESH")
                .flat_map(|s| children(s, "VERTEX"))
                .flat_map(|v| {
                    children(v, "INFLUENCE")
                        .map(|i| format!("{}:{}", i.attributes["ID"], i.get_text().unwrap()))
                })
                .collect()
        }
        for b in files {
            let mut d = Document::load(b, true).unwrap();
            let original = bindings(&d);
            let base = d.base();
            d.create_expression("custom.Test.Exclusive").unwrap();
            let id = (0..base.len())
                .find(|i| !d.neck_vertices.contains(i))
                .unwrap();
            let mut ps = d.positions().to_vec();
            ps[id].1 += 1.0;
            d.change(ps);
            let r = Document::load(d.export().unwrap().as_bytes(), true).unwrap();
            assert_eq!(original, bindings(&r));
            assert_eq!(base, r.base());
        }
    }
    #[test]
    fn originals_load_and_round_trip() {
        for female in [false, true] {
            let d = Document::template(female);
            assert_eq!(d.positions().len(), 956);
            assert_eq!(d.morphs.len(), 29);
            let copy = Document::parse(&d.export().unwrap(), female).unwrap();
            assert_eq!(d.morphs, copy.morphs);
            assert_eq!(
                d.parts.iter().map(|p| p.faces.len()).sum::<usize>(),
                copy.parts.iter().map(|p| p.faces.len()).sum::<usize>()
            );
        }
    }
    #[test]
    fn edit_export_preserves_other_morphs_and_skinning() {
        let mut d = Document::template(true);
        let before = d.clone();
        d.checkpoint();
        let mut ps = d.positions().to_vec();
        ps[0] = ps[0].add(V(2.0, 3.0, 4.0));
        d.change(ps.clone());
        let xml = d.export().unwrap();
        let loaded = Document::parse(&xml, true).unwrap();
        assert!(loaded.positions()[0].sub(ps[0]).len() < 0.001);
        for (name, p) in &before.morphs {
            if name != "Face.Average" {
                assert_eq!(p, &loaded.morphs[name]);
            }
        }
        assert!(xml.contains("INFLUENCE"));
        assert!(xml.contains("NUMTEXCOORDS=\"2\""));
        d.undo(false);
        assert_eq!(d.positions(), before.positions());
        assert!(!d.dirty);
        d.undo(true);
        assert_eq!(d.positions(), ps);
        assert!(d.dirty);
    }
    #[test]
    fn mirrored_edit_and_protected_vertices() {
        let d = Document::template(false);
        let mut s = Selection::new(956);
        s.seeds = vec![0];
        s.radius = 1000.0;
        s.falloff = Falloff::All;
        s.update(&d);
        let edited = s.translate(d.positions(), V(5.0, 2.0, 0.0));
        for i in 0..956 {
            if s.locked(i) {
                assert_eq!(edited[i], d.positions()[i]);
            } else if d.positions()[i].0 * d.positions()[0].0 < 0.0 {
                assert!((edited[i].0 - d.positions()[i].0 + 5.0).abs() < 0.001);
            }
        }
    }
    #[test]
    fn saved_revision_is_restored_by_redo() {
        let mut d = Document::template(false);
        d.checkpoint();
        let mut ps = d.positions().to_vec();
        ps[0].0 += 1.0;
        d.change(ps);
        d.saved_revision = d.revision;
        d.dirty = false;
        d.undo(false);
        assert!(d.dirty);
        d.undo(true);
        assert!(!d.dirty);
    }
    #[test]
    fn malformed_mesh_is_rejected() {
        assert!(Document::parse("<MESH NUMSUBMESH=\"0\"/>", true).is_err());
    }
    #[test]
    fn presets_have_original_order_and_count() {
        let p = presets();
        assert_eq!(p.len(), 15);
        assert_eq!(p[0].0, "African");
        assert!(p.iter().all(|p| p.1.len() == 955));
        assert_eq!(p[0].1[0], V(0.05, -3.2, -9.67));
    }
    #[test]
    fn female_neck_band_matches_original_reference_without_a_collar() {
        // The original Flash template is repaired at load; the default head
        // (already repaired when it was made) must carry the same neck band.
        let original = Document::parse(include_str!("../assets/NewExportHead6.xmf"), true).unwrap();
        assert!(original.neck_repaired);
        let default = Document::template(true);
        assert!(!default.needs_neck_repair());
        for d in [original, default] {
            let xml = d.export().unwrap();
            let root = Element::parse(Cursor::new(format!("<root>{xml}</root>"))).unwrap();
            let skin = root
                .get_child("MESH")
                .unwrap()
                .get_child("SUBMESH")
                .unwrap();
            let reference = Element::parse(Cursor::new(include_str!(
                "../assets/female-neck-reference.xml"
            )))
            .unwrap();
            for expected in reference.children.iter().filter_map(XMLNode::as_element) {
                let actual = children(skin, "VERTEX")
                    .find(|v| v.attributes["ID"] == expected.attributes["ID"])
                    .unwrap();
                for name in ["POS", "NORM", "TEXCOORD", "INFLUENCE"] {
                    assert_eq!(
                        actual.get_child(name),
                        expected.get_child(name),
                        "Vertex {}: {name}",
                        expected.attributes["ID"]
                    );
                }
            }
            assert_eq!(d.parts.len(), 4);
            assert_eq!(d.positions().len(), 956);
            // This is the measured former gap at one right-side neck vertex.
            let corrected = vector(
                &text(
                    children(skin, "VERTEX")
                        .find(|v| v.attributes["ID"] == "223")
                        .unwrap(),
                    "POS",
                )
                .unwrap(),
            )
            .unwrap();
            assert!((975.691 - corrected.2 - 2.571).abs() < 0.001);
            let mut root = root.clone();
            assert!(
                !crate::neck::repair(&mut root, true),
                "Repair must be idempotent"
            );
        }
    }
    #[test]
    fn actual_neck_vertices_are_locked_in_both_heads_and_all_morphs() {
        for female in [true, false] {
            let mut d = Document::template(female);
            let mut selection = Selection::new(956);
            selection.critical = false;
            for &i in crate::neck::VERTICES {
                assert!(selection.locked(i), "neck {i}");
            }
            // Vertex zero must not be mistaken for one of the runtime-ID locks.
            assert!(!selection.locked(0));
            for name in d.morphs.keys().cloned().collect::<Vec<_>>() {
                d.active = name;
                let mut moved = d.positions().to_vec();
                for &i in crate::neck::VERTICES {
                    moved[i] = moved[i].add(V(10.0, 20.0, 30.0));
                }
                moved[0].0 += 1.0;
                d.change(moved);
                for &i in crate::neck::VERTICES {
                    assert_eq!(d.positions()[i], d.parts[0].base[i]);
                }
            }
            let exported = d.export().unwrap();
            let loaded = Document::parse(&exported, female).unwrap();
            for positions in loaded.morphs.values() {
                for &i in crate::neck::VERTICES {
                    assert_eq!(positions[i], loaded.parts[0].base[i]);
                }
            }
        }
    }
    #[test]
    fn old_bad_neck_morphs_are_removed_but_face_edits_survive() {
        let mut d = Document::template(true);
        let skin = d
            .root
            .get_mut_child("MESH")
            .unwrap()
            .get_mut_child("SUBMESH")
            .unwrap();
        let morph = skin
            .children
            .iter_mut()
            .filter_map(XMLNode::as_mut_element)
            .find(|m| m.name == "MORPH" && m.attributes["NAME"] == "Face.Average")
            .unwrap();
        let mut blend = Element::new("BLENDVERTEX");
        blend.attributes.insert("VERTEXID".into(), "223".into());
        blend.children.push(XMLNode::Element(node(
            "POSITION",
            "22.9548 4.21639 990.0".into(),
        )));
        morph.children.push(XMLNode::Element(blend));
        let count = children(morph, "BLENDVERTEX").count();
        morph
            .attributes
            .insert("NUMBLENDVERTS".into(), count.to_string());
        // Construct the legacy bad input directly; the fixed exporter must now
        // refuse an invalid blend record before writing it.
        let mut source = Vec::new();
        for e in d.root.children.iter().filter_map(XMLNode::as_element) {
            e.write_with_config(
                &mut source,
                xmltree::EmitterConfig::new().write_document_declaration(false),
            )
            .unwrap();
        }
        let source = String::from_utf8(source).unwrap();
        let fixed = Document::parse(&source, true).unwrap();
        assert!(fixed.neck_repaired);
        assert_eq!(fixed.positions()[223], fixed.parts[0].base[223]);
        assert_eq!(fixed.positions()[0], d.positions()[0]);
    }
    #[test]
    fn female_reference_does_not_change_male_seam() {
        let source = Element::parse(Cursor::new(format!(
            "<root>{}</root>",
            include_str!("../assets/MaleHeadStart_9b.xmf")
        )))
        .unwrap();
        let mut root = source.clone();
        assert!(!crate::neck::repair(&mut root, true));
        assert_eq!(root, source);
        assert!(crate::neck::repair(&mut root, false));
        assert!(!crate::neck::repair(&mut root, false));
    }
    #[test]
    fn neck_transition_is_protected_and_face_edits_remain_editable() {
        let mut d = Document::template(true);
        let mut selection = Selection::new(956);
        selection.critical = false;
        selection.update(&d);
        let before = d.positions().to_vec();
        let mut moved = before.clone();
        for &i in crate::neck::BAND {
            assert!(selection.locked(i));
            moved[i] = moved[i].add(V(7.0, 8.0, 9.0));
        }
        moved[100].0 += 2.0;
        d.change(moved);
        for &i in crate::neck::BAND {
            assert_eq!(d.positions()[i], before[i]);
        }
        assert_eq!(d.positions()[100].0, before[100].0 + 2.0);
        let loaded = Document::parse(&d.export().unwrap(), true).unwrap();
        for &i in crate::neck::BAND {
            assert!(loaded.positions()[i].sub(before[i]).len() < 0.001);
        }
    }
    #[test]
    fn animated_neck_band_matches_original_bone_bindings() {
        fn position(v: &Element) -> V {
            vector(&text(v, "POS").unwrap()).unwrap()
        }
        fn skinned(v: &Element, angle: f32) -> V {
            let p = position(v);
            let mut out = V::default();
            for influence in children(v, "INFLUENCE") {
                let bone = attr(influence, "ID").unwrap();
                let w = influence.get_text().unwrap().parse::<f32>().unwrap();
                // Different head/neck transforms exercise the actual mixed band.
                let angle = if bone == 21 { -angle * 0.3 } else { angle };
                let pivot = if bone == 21 {
                    V(0.0, 0.0, 970.0)
                } else {
                    V(0.0, 0.0, 1000.0)
                };
                let q = p.sub(pivot);
                let (s, c) = angle.sin_cos();
                out = out.add(
                    V(q.0, q.1 * c - q.2 * s, q.1 * s + q.2 * c)
                        .add(pivot)
                        .mul(w),
                );
            }
            out
        }
        let doc = Document::template(true);
        let xml = doc.export().unwrap();
        let output = Element::parse(Cursor::new(format!("<root>{xml}</root>"))).unwrap();
        let skin = output
            .get_child("MESH")
            .unwrap()
            .get_child("SUBMESH")
            .unwrap();
        let reference = Element::parse(Cursor::new(include_str!(
            "../assets/female-neck-reference.xml"
        )))
        .unwrap();
        assert_eq!(children(&reference, "VERTEX").count(), 35);
        for original in children(&reference, "VERTEX") {
            let actual = children(skin, "VERTEX")
                .find(|v| v.attributes["ID"] == original.attributes["ID"])
                .unwrap();
            for degrees in [-60.0_f32, -30.0, 0.0, 30.0, 60.0] {
                assert!(
                    skinned(actual, degrees.to_radians())
                        .sub(skinned(original, degrees.to_radians()))
                        .len()
                        < 0.00001
                );
            }
        }
        // This front transition point was still raised in the first edge-only repair.
        let chin = children(skin, "VERTEX")
            .find(|v| v.attributes["ID"] == "44")
            .unwrap();
        assert!((position(chin).2 - 963.623_f32).abs() < 0.00001);
    }
    #[test]
    fn head_and_neck_unlocks_are_independent() {
        let d = Document::template(true);
        let mut s = Selection::new(956);
        s.update(&d);
        assert!(s.locked(126));
        assert!(s.locked(223));
        s.critical = false;
        assert!(!s.locked(126));
        assert!(s.locked(223));
        s.critical = true;
        s.allow_neck = true;
        assert!(s.locked(126));
        assert!(!s.locked(223));
        s.critical = false;
        assert!(!s.locked(126));
        assert!(!s.locked(223));
    }
    #[test]
    fn intentional_neck_edits_survive_save_reload_and_relocking() {
        for female in [true, false] {
            let mut d = Document::template(female);
            let original = d.positions()[223];
            d.neck_editing = true;
            d.checkpoint();
            let mut ps = d.positions().to_vec();
            ps[223].1 += 5.0;
            d.change(ps);
            d.neck_editing = false;
            let mut head = d.positions().to_vec();
            head[100].0 += 1.0;
            head[223].1 += 10.0;
            d.change(head);
            assert_eq!(d.positions()[223].1, original.1 + 5.0);
            let output = d.export().unwrap();
            assert!(output.contains("HEADMAKER_NECK_EDITS=\"1\""));
            let reload = Document::parse(&output, female).unwrap();
            assert!(reload.positions()[223].sub(d.positions()[223]).len() < 0.001);
            assert!(
                !reload.neck_editing,
                "Unlock permission is off again on a new session"
            );
        }
    }
    // Uses IMVU head meshes from tests/fixtures, which are not published.
    #[cfg(imvu_fixtures)]
    #[test]
    fn binary_custom_head_loads_edits_and_exports_without_legacy_indexes() {
        let mut d = Document::load(include_bytes!("../tests/fixtures/Head-Srs.cmf"), true).unwrap();
        assert_eq!(d.positions().len(), 1380);
        assert_eq!(d.morphs.len(), 58);
        assert!(!d.legacy);
        assert!(!d.needs_neck_repair());
        let mut s = Selection::new(d.positions().len());
        s.update(&d);
        assert!(!s.locked(0));
        assert_eq!(s.weights.len(), 1380);
        let mut ps = d.positions().to_vec();
        ps[0].0 += 2.0;
        d.change(ps);
        let exported = d.export().unwrap();
        let loaded = Document::load(exported.as_bytes(), true).unwrap();
        assert_eq!(
            loaded
                .parts
                .iter()
                .map(|p| p.base.len())
                .collect::<Vec<_>>(),
            [1116, 162, 60, 42]
        );
        assert_eq!(loaded.morphs.len(), 58);
        assert!(loaded.positions()[0].sub(d.positions()[0]).len() < 0.001);
    }
    #[test]
    fn legacy_neck_files_are_detected_without_silent_repair() {
        for bytes in [
            include_bytes!("../tests/fixtures/legacy-neck-a.xmf").as_slice(),
            include_bytes!("../tests/fixtures/legacy-neck-b.xmf").as_slice(),
        ] {
            let mut d = Document::load(bytes, true).unwrap();
            assert!(d.needs_neck_repair());
            assert!(!d.neck_repaired);
            assert!(!d.dirty);
            let before = d.positions().to_vec();
            d.repair_neck().unwrap();
            assert!(d.neck_repaired);
            assert!(d.dirty);
            assert!(!d.needs_neck_repair());
            for i in 0..before.len() {
                if !crate::neck::BAND.contains(&i) {
                    assert_eq!(before[i], d.positions()[i]);
                }
            }
        }
    }
    // Uses IMVU head meshes from tests/fixtures, which are not published.
    #[cfg(imvu_fixtures)]
    #[test]
    fn truncated_binary_is_rejected_without_panicking() {
        let bytes = include_bytes!("../tests/fixtures/Head-Srs.cmf");
        for length in [4, 11, 100, bytes.len() - 1] {
            assert!(Document::load(&bytes[..length], true).is_err());
        }
    }
    // Uses IMVU head meshes from tests/fixtures, which are not published.
    #[cfg(imvu_fixtures)]
    #[test]
    fn supplied_five_part_heads_import_edit_render_and_round_trip() {
        for (bytes, total) in [
            (
                include_bytes!("../tests/fixtures/anime07.cmf").as_slice(),
                1693,
            ),
            (
                include_bytes!("../tests/fixtures/anime08.cmf").as_slice(),
                1819,
            ),
            (
                include_bytes!("../tests/fixtures/anime-plus.cmf").as_slice(),
                1693,
            ),
        ] {
            let mut d = Document::load(bytes, true).unwrap();
            assert_eq!(d.parts.len(), 5);
            assert_eq!(d.positions().len(), total);
            assert_eq!(d.morphs.len(), 84);
            assert_eq!(d.active, BASE_MESH);
            assert_eq!(
                d.parts.iter().map(|p| p.material).collect::<Vec<_>>(),
                [2, 3, 5, 8, 11]
            );
            assert!(!d.needs_neck_repair());
            let extra = d.parts[4].offset;
            let mut ps = d.positions().to_vec();
            ps[0].0 += 1.0;
            ps[extra].0 += 2.0;
            d.change(ps);
            let xml = d.export().unwrap();
            let loaded = Document::load(xml.as_bytes(), true).unwrap();
            assert_eq!(loaded.parts.len(), 5);
            assert_eq!(loaded.morphs.len(), 84);
            assert!(loaded.positions()[extra].sub(d.positions()[extra]).len() < 0.001);
            for (a, b) in d.parts.iter().zip(&loaded.parts) {
                assert_eq!(a.faces, b.faces);
                assert_eq!(a.material, b.material);
                assert_eq!(a.uv, b.uv);
            }
            let textures = crate::render::textures(true);
            let f = crate::render::render(
                &loaded,
                &textures,
                &crate::render::Camera::default(),
                [160, 180],
                &[false; 5],
                &vec![false; total],
                true,
            );
            assert!(f.depth.iter().filter(|&&z| z < f32::MAX).count() > 1000);
            let mut hidden = vec![false; 5];
            hidden[4] = true;
            crate::render::render(
                &loaded,
                &textures,
                &crate::render::Camera::default(),
                [160, 180],
                &hidden,
                &vec![false; total],
                true,
            );
        }
    }
    // Uses IMVU head meshes from tests/fixtures, which are not published.
    #[cfg(imvu_fixtures)]
    #[test]
    fn base_shape_edits_follow_expressions_and_undo_without_adding_morphs() {
        let mut d = Document::load(include_bytes!("../tests/fixtures/anime07.cmf"), true).unwrap();
        let before = d.morphs.clone();
        d.checkpoint();
        let mut ps = d.positions().to_vec();
        ps[0].0 += 3.0;
        d.change(ps);
        for (name, positions) in &before {
            assert!((d.morphs[name][0].0 - positions[0].0 - 3.0).abs() < 0.001);
        }
        d.undo(false);
        assert_eq!(before, d.morphs);
        d.undo(true);
        let xml = d.export().unwrap();
        assert!(!xml.contains("NAME=\"Base mesh\""));
        let loaded = Document::load(xml.as_bytes(), true).unwrap();
        for name in before.keys() {
            assert!(loaded.morphs[name][0].sub(d.morphs[name][0]).len() < 0.001);
        }
    }
    // Uses IMVU head meshes from tests/fixtures, which are not published.
    #[cfg(imvu_fixtures)]
    #[test]
    fn male_neck_band_matches_uv_mapped_default_and_restores_bone_weights() {
        let source =
            Document::load(include_bytes!("../tests/fixtures/male-default.cmf"), false).unwrap();
        assert!(!source.needs_neck_repair());
        let original = source
            .root
            .get_child("MESH")
            .unwrap()
            .get_child("SUBMESH")
            .unwrap();
        // The original template is repaired at load; the default head (made
        // already repaired) must carry the same neck band and weights.
        let template =
            Document::parse(include_str!("../assets/MaleHeadStart_9b.xmf"), false).unwrap();
        assert!(template.neck_repaired);
        let default = Document::template(false);
        assert!(!default.needs_neck_repair());
        for repaired in [template, default] {
            let output = repaired.export().unwrap();
            let root = Element::parse(Cursor::new(format!("<root>{output}</root>"))).unwrap();
            let skin = root
                .get_child("MESH")
                .unwrap()
                .get_child("SUBMESH")
                .unwrap();
            let reference = Element::parse(Cursor::new(include_str!(
                "../assets/male-neck-reference.xml"
            )))
            .unwrap();
            for expected in children(&reference, "VERTEX") {
                let actual = children(skin, "VERTEX")
                    .find(|v| v.attributes["ID"] == expected.attributes["ID"])
                    .unwrap();
                let original = children(original, "VERTEX")
                    .find(|v| v.attributes["ID"] == expected.attributes["SOURCE_VERTEXID"])
                    .unwrap();
                for tag in ["POS", "NORM", "TEXCOORD", "INFLUENCE"] {
                    let values = |v: &Element| {
                        children(v, tag)
                            .map(|e| {
                                (
                                    e.attributes.clone(),
                                    numbers(&e.get_text().unwrap()).unwrap(),
                                )
                            })
                            .collect::<Vec<_>>()
                    };
                    assert_eq!(
                        values(actual),
                        values(original),
                        "{}: {tag}",
                        expected.attributes["ID"]
                    );
                }
                assert_eq!(
                    attr(actual, "NUMINFLUENCES").unwrap(),
                    children(actual, "INFLUENCE").count()
                );
            }
            let back = children(skin, "VERTEX")
                .find(|v| v.attributes["ID"] == "719")
                .unwrap();
            assert_eq!(children(back, "INFLUENCE").count(), 1);
            assert_eq!(back.get_child("INFLUENCE").unwrap().attributes["ID"], "20");
        }
    }
    #[test]
    fn old_male_neck_is_offered_a_repair_and_keeps_unrelated_face_edits() {
        let mut d =
            Document::load(include_bytes!("../assets/MaleHeadStart_9b.xmf"), false).unwrap();
        assert!(d.needs_neck_repair());
        let before = d.positions().to_vec();
        d.repair_neck().unwrap();
        assert!(d.dirty);
        assert!(!d.needs_neck_repair());
        for i in 0..before.len() {
            if !crate::neck::BAND.contains(&i) {
                assert_eq!(d.positions()[i], before[i]);
            }
        }
        let loaded = Document::load(d.export().unwrap().as_bytes(), false).unwrap();
        for &i in crate::neck::BAND {
            assert!(loaded.positions()[i].sub(d.positions()[i]).len() < 0.001);
        }
    }
    // Uses IMVU head meshes from tests/fixtures, which are not published.
    #[cfg(imvu_fixtures)]
    #[test]
    fn actual_head_size_exports_and_preserves_the_locked_neck() {
        for mut d in [
            Document::template(true),
            Document::template(false),
            Document::load(include_bytes!("../tests/fixtures/Head-Srs.cmf"), true).unwrap(),
        ] {
            let before = d.positions().to_vec();
            let scaled = d.scaled_head_positions(V(1.2, 1.2, 1.2), false);
            for &i in &d.neck_vertices {
                assert_eq!(before[i], scaled[i]);
            }
            assert!(scaled[0].sub(before[0]).len() > 1.0);
            d.checkpoint();
            d.change(scaled);
            let changed = d.positions().to_vec();
            let saved = Document::load(d.export().unwrap().as_bytes(), d.female).unwrap();
            assert!(saved.positions()[0].sub(changed[0]).len() < 0.001);
            d.undo(false);
            assert_eq!(d.positions(), before);
            d.undo(true);
            assert_eq!(d.positions(), changed);
        }
    }
}
