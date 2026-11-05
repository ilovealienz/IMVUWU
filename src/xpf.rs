use crate::model::{Document, BASE_MESH, V};
use std::{collections::BTreeSet, io::Cursor, path::PathBuf};
use xmltree::{Element, XMLNode};

#[derive(Clone, Debug, PartialEq)]
pub struct Key {
    pub time: f32,
    pub weight: f32,
}
#[derive(Clone, Debug, PartialEq)]
pub struct Track {
    pub name: String,
    pub keys: Vec<Key>,
}
#[derive(Clone, Debug, PartialEq)]
pub struct Animation {
    pub duration: f32,
    pub tracks: Vec<Track>,
}
impl Default for Animation {
    fn default() -> Self {
        Self {
            duration: 1.0,
            tracks: Vec::new(),
        }
    }
}
fn elements<'a>(e: &'a Element, name: &'a str) -> impl Iterator<Item = &'a Element> {
    e.children
        .iter()
        .filter_map(XMLNode::as_element)
        .filter(move |e| e.name == name)
}
fn number(s: &str) -> Result<f32, String> {
    let n = s
        .parse::<f32>()
        .map_err(|_| format!("Invalid number: {s}"))?;
    if !n.is_finite() {
        Err("Non-finite XPF value".into())
    } else {
        Ok(n)
    }
}
fn attribute(e: &Element, name: &str) -> Result<f32, String> {
    number(
        e.attributes
            .get(name)
            .ok_or_else(|| format!("Missing {name}"))?,
    )
}
fn count(e: &Element, name: &str) -> Result<usize, String> {
    e.attributes
        .get(name)
        .ok_or_else(|| format!("Missing {name}"))?
        .parse()
        .map_err(|_| format!("Invalid {name}"))
}
impl Animation {
    pub fn validate(&self) -> Result<(), String> {
        if !self.duration.is_finite() || self.duration <= 0.0 || self.duration > 86400.0 {
            return Err("Duration must be greater than zero and at most 86400 seconds".into());
        }
        if self.tracks.len() > 1024 {
            return Err("Too many XPF tracks".into());
        }
        let mut names = BTreeSet::new();
        let mut total = 0;
        for t in &self.tracks {
            if t.name.is_empty() || t.name.len() > 4096 || !names.insert(&t.name) {
                return Err("Empty or repeated morph track name".into());
            }
            if t.keys.is_empty() {
                return Err(format!("Track {} has no keys", t.name));
            }
            total += t.keys.len();
            if total > 100_000 {
                return Err("Too many XPF keys".into());
            }
            let mut previous = -1.0;
            for k in &t.keys {
                if !k.time.is_finite()
                    || !k.weight.is_finite()
                    || k.time < 0.0
                    || k.time > self.duration
                    || k.time <= previous
                {
                    return Err(format!("Invalid key time or weight in {}", t.name));
                }
                previous = k.time;
            }
        }
        Ok(())
    }
    pub fn load(bytes: &[u8]) -> Result<Self, String> {
        if bytes.len() > 8 * 1024 * 1024 {
            return Err("Expression exceeds 8 MB".into());
        }
        if bytes.starts_with(b"CPF\0") {
            return Self::binary(bytes);
        }
        let s = std::str::from_utf8(bytes)
            .map_err(|_| "Expected text XPF or binary CPF")?
            .trim_start_matches('\u{feff}')
            .trim();
        let s = if s.starts_with("<?xml") {
            &s[s.find("?>").ok_or("Unclosed XML declaration")? + 2..]
        } else {
            s
        };
        let root =
            Element::parse(Cursor::new(format!("<root>{s}</root>"))).map_err(|e| e.to_string())?;
        let h = root.get_child("HEADER").ok_or("No XPF header")?;
        if h.attributes.get("MAGIC").map(String::as_str) != Some("XPF") {
            return Err("This file is not an XPF expression".into());
        }
        let version = count(h, "VERSION")?;
        if !(911..=919).contains(&version) {
            return Err(format!("Unsupported XPF version {version}"));
        }
        let a = root.get_child("ANIMATION").ok_or("Missing ANIMATION")?;
        let mut tracks = Vec::new();
        for t in elements(a, "TRACK") {
            let name = t
                .attributes
                .get("MORPHNAME")
                .ok_or("Missing MORPHNAME")?
                .clone();
            let mut keys = Vec::new();
            for k in elements(t, "KEYFRAME") {
                let value = k
                    .get_child("WEIGHT")
                    .and_then(|w| w.get_text())
                    .ok_or("Missing WEIGHT")?;
                keys.push(Key {
                    time: attribute(k, "TIME")?,
                    weight: number(value.trim())?,
                });
            }
            if keys.len() != count(t, "NUMKEYFRAMES")? {
                return Err("NUMKEYFRAMES mismatch".into());
            }
            tracks.push(Track { name, keys });
        }
        if tracks.len() != count(a, "NUMTRACKS")? {
            return Err("NUMTRACKS mismatch".into());
        }
        let result = Self {
            duration: attribute(a, "DURATION")?,
            tracks,
        };
        result.validate()?;
        Ok(result)
    }
    fn binary(bytes: &[u8]) -> Result<Self, String> {
        struct R<'a> {
            b: &'a [u8],
            p: usize,
        }
        impl R<'_> {
            fn take(&mut self, n: usize) -> Result<&[u8], String> {
                let end = self.p.checked_add(n).ok_or("CPF size overflow")?;
                let b = self.b.get(self.p..end).ok_or("Truncated CPF")?;
                self.p = end;
                Ok(b)
            }
            fn int(&mut self, max: usize) -> Result<usize, String> {
                let n = i32::from_le_bytes(self.take(4)?.try_into().unwrap());
                if n < 0 || n as usize > max {
                    Err("CPF count exceeds limits".into())
                } else {
                    Ok(n as usize)
                }
            }
            fn float(&mut self) -> Result<f32, String> {
                Ok(f32::from_le_bytes(self.take(4)?.try_into().unwrap()))
            }
        }
        let mut r = R { b: bytes, p: 4 };
        let version = r.int(919)?;
        if version < 911 {
            return Err("Unsupported CPF version".into());
        }
        let duration = r.float()?;
        let n = r.int(1024)?;
        let mut tracks = Vec::new();
        let mut total = 0;
        for _ in 0..n {
            let len = r.int(4096)?;
            let name = String::from_utf8(r.take(len)?.to_vec())
                .map_err(|_| "Invalid CPF morph name")?
                .trim_end_matches('\0')
                .to_string();
            let n = r.int(100_000)?;
            total += n;
            if total > 100_000 {
                return Err("Too many CPF keys".into());
            }
            let mut keys = Vec::new();
            for _ in 0..n {
                keys.push(Key {
                    time: r.float()?,
                    weight: r.float()?,
                });
            }
            tracks.push(Track { name, keys });
        }
        if r.p != bytes.len() {
            return Err("Unexpected CPF trailing data".into());
        }
        let result = Self { duration, tracks };
        result.validate()?;
        Ok(result)
    }
    pub fn export(&self) -> Result<String, String> {
        self.validate()?;
        if self.tracks.is_empty() {
            return Err("Set at least one expression weight before exporting".into());
        }
        let mut header = Element::new("HEADER");
        header.attributes.insert("MAGIC".into(), "XPF".into());
        header.attributes.insert("VERSION".into(), "919".into());
        let mut a = Element::new("ANIMATION");
        a.attributes
            .insert("DURATION".into(), self.duration.to_string());
        a.attributes
            .insert("NUMTRACKS".into(), self.tracks.len().to_string());
        for t in &self.tracks {
            let mut track = Element::new("TRACK");
            track.attributes.insert("MORPHNAME".into(), t.name.clone());
            track
                .attributes
                .insert("NUMKEYFRAMES".into(), t.keys.len().to_string());
            for k in &t.keys {
                let mut key = Element::new("KEYFRAME");
                key.attributes.insert("TIME".into(), k.time.to_string());
                let mut weight = Element::new("WEIGHT");
                weight.children.push(XMLNode::Text(k.weight.to_string()));
                key.children.push(XMLNode::Element(weight));
                track.children.push(XMLNode::Element(key));
            }
            a.children.push(XMLNode::Element(track));
        }
        let mut bytes = Vec::new();
        let config = xmltree::EmitterConfig::new()
            .perform_indent(true)
            .line_separator("\r\n")
            .pad_self_closing(false)
            .write_document_declaration(false);
        for e in [header, a] {
            e.write_with_config(&mut bytes, config.clone())
                .map_err(|e| e.to_string())?;
            bytes.extend_from_slice(b"\r\n");
        }
        String::from_utf8(bytes).map_err(|e| e.to_string())
    }
    pub fn weight(&self, name: &str, time: f32) -> f32 {
        let Some(t) = self.tracks.iter().find(|t| t.name == name) else {
            return 0.0;
        };
        if time <= t.keys[0].time {
            return t.keys[0].weight;
        }
        for pair in t.keys.windows(2) {
            if time <= pair[1].time {
                let a = &pair[0];
                let b = &pair[1];
                return a.weight + (b.weight - a.weight) * (time - a.time) / (b.time - a.time);
            }
        }
        t.keys.last().unwrap().weight
    }
    pub fn set_weight(&mut self, name: &str, time: f32, weight: f32) {
        let time = time.clamp(0.0, self.duration);
        let i = if let Some(i) = self.tracks.iter().position(|t| t.name == name) {
            i
        } else {
            self.tracks.push(Track {
                name: name.into(),
                keys: if time > 0.0001 {
                    vec![Key {
                        time: 0.0,
                        weight: 0.0,
                    }]
                } else {
                    Vec::new()
                },
            });
            self.tracks.len() - 1
        };
        let t = &mut self.tracks[i];
        if let Some(k) = t.keys.iter_mut().find(|k| (k.time - time).abs() < 0.00001) {
            k.weight = weight;
        } else {
            t.keys.push(Key { time, weight });
            t.keys.sort_by(|a, b| a.time.total_cmp(&b.time));
        }
    }
    pub fn blend(&self, doc: &Document, time: f32) -> Vec<V> {
        let base = doc
            .morphs
            .get(BASE_MESH)
            .cloned()
            .unwrap_or_else(|| doc.base());
        let mut result = if self.tracks.iter().any(|t| t.name == "Face.Average") {
            base.clone()
        } else {
            doc.morphs
                .get("Face.Average")
                .cloned()
                .unwrap_or_else(|| base.clone())
        };
        for t in &self.tracks {
            if t.name == BASE_MESH {
                continue;
            }
            let Some(target) = doc.morphs.get(&t.name) else {
                continue;
            };
            let weight = self.weight(&t.name, time);
            for ((p, &b), &goal) in result.iter_mut().zip(&base).zip(target) {
                *p = p.add(goal.sub(b).mul(weight));
            }
        }
        result
    }
}

pub struct Editor {
    pub animation: Animation,
    pub time: f32,
    pub playing: bool,
    pub looping: bool,
    pub filter: String,
    pub path: Option<PathBuf>,
    saved: Animation,
    undo: Vec<Animation>,
    redo: Vec<Animation>,
    pub editing: bool,
}
impl Default for Editor {
    fn default() -> Self {
        Self {
            animation: Animation::default(),
            time: 0.0,
            playing: false,
            looping: true,
            filter: String::new(),
            path: None,
            saved: Animation::default(),
            undo: Vec::new(),
            redo: Vec::new(),
            editing: false,
        }
    }
}
impl Editor {
    pub fn dirty(&self) -> bool {
        self.animation != self.saved
    }
    pub fn saved(&mut self) {
        self.saved = self.animation.clone();
    }
    pub fn checkpoint(&mut self) {
        self.undo.push(self.animation.clone());
        if self.undo.len() > 100 {
            self.undo.remove(0);
        }
        self.redo.clear();
    }
    pub fn undo(&mut self, redo: bool) {
        if let Some(a) = if redo {
            self.redo.pop()
        } else {
            self.undo.pop()
        } {
            let old = std::mem::replace(&mut self.animation, a);
            if redo {
                self.undo.push(old)
            } else {
                self.redo.push(old)
            }
            self.time = self.time.min(self.animation.duration);
        }
    }
    pub fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }
    pub fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    // Uses IMVU head meshes from tests/fixtures, which are not published.
    #[cfg(imvu_fixtures)]
    #[test]
    fn individual_expression_points_round_trip_on_original_and_custom_heads() {
        for mut d in [
            Document::template(true),
            Document::template(false),
            Document::load(include_bytes!("../tests/fixtures/anime07.cmf"), true).unwrap(),
        ] {
            let base = d.base();
            let vertex = (0..base.len())
                .find(|i| !d.neck_vertices.contains(i) && base[*i].0.abs() > 1.0)
                .unwrap();
            let neutral = Animation::default().blend(&d, 0.0);
            let originals = d.morphs.clone();
            d.create_expression("custom.Lip.Exclusive").unwrap();
            d.checkpoint();
            let mut ps = d.positions().to_vec();
            ps[vertex].0 += 2.0;
            d.change(ps);
            for (n, p) in &originals {
                assert_eq!(p, &d.morphs[n]);
            }
            let xml = d.export().unwrap();
            let r = Document::load(xml.as_bytes(), d.female).unwrap();
            let mut a = Animation::default();
            a.set_weight("custom.Lip.Exclusive", 0.0, 1.0);
            let result = a.blend(&r, 0.0);
            for i in 0..result.len() {
                let expected = if i == vertex {
                    neutral[i].add(V(2.0, 0.0, 0.0))
                } else {
                    neutral[i]
                };
                assert!(
                    result[i].sub(expected).len() < 0.001,
                    "vertex {i}, female={}, actual={:?}, expected={:?}, target={:?}, base={:?}",
                    d.female,
                    result[i],
                    expected,
                    d.morphs["custom.Lip.Exclusive"][i],
                    base[i]
                );
            }
            for &i in &d.neck_vertices {
                assert_eq!(r.morphs["custom.Lip.Exclusive"][i], base[i]);
            }
            Animation::load(a.export().unwrap().as_bytes()).unwrap();
        }
    }
    #[test]
    fn custom_target_creation_and_edits_undo_and_redo() {
        let mut d = Document::template(true);
        let revision = d.revision;
        d.create_expression("custom.Test.Exclusive").unwrap();
        d.checkpoint();
        let neutral = d.positions().to_vec();
        let mut ps = neutral.clone();
        ps[100].1 += 1.0;
        d.change(ps.clone());
        d.undo(false);
        assert_eq!(d.positions(), neutral);
        d.undo(false);
        assert!(!d.morphs.contains_key("custom.Test.Exclusive"));
        assert_eq!(d.revision, revision);
        d.undo(true);
        assert_eq!(d.positions(), neutral);
        d.undo(true);
        assert_eq!(d.positions(), ps);
    }
    #[test]
    fn zero_radius_moves_only_clicked_points_and_honors_part_mask() {
        use crate::model::Selection;
        let d = Document::template(true);
        let mut s = Selection::new(d.positions().len());
        s.critical = false;
        s.symmetric = false;
        s.radius = 0.0;
        s.seeds = vec![100];
        s.update(&d);
        assert_eq!(s.weights.iter().filter(|&&w| w > 0.0).count(), 1);
        s.editable[100] = false;
        s.update(&d);
        assert!(s.weights.iter().all(|&w| w == 0.0));
    }
    #[test]
    fn custom_target_names_cannot_replace_existing_or_invalid_channels() {
        let mut d = Document::template(true);
        for name in ["Face.Average", "Bad channel", "custom.Bad", ".Exclusive"] {
            assert!(d.create_expression(name).is_err());
        }
    }
    #[test]
    fn static_export_matches_original_single_key_track_structure() {
        let mut a = Animation::default();
        a.set_weight("mouth.Smile.Exclusive", 0.0, 0.75);
        let s = a.export().unwrap();
        assert!(s.contains("MAGIC=\"XPF\""));
        assert!(s.contains("NUMKEYFRAMES=\"1\""));
        assert_eq!(Animation::load(s.as_bytes()).unwrap(), a);
    }
    #[test]
    fn curves_interpolate_and_retain_all_keys() {
        let mut a = Animation {
            duration: 2.0,
            ..Default::default()
        };
        a.set_weight("blink.Left.Upper.Clamped", 0.0, 0.0);
        a.set_weight("blink.Left.Upper.Clamped", 1.0, 1.0);
        a.set_weight("blink.Left.Upper.Clamped", 2.0, 0.0);
        assert_eq!(a.weight("blink.Left.Upper.Clamped", 0.5), 0.5);
        assert_eq!(Animation::load(a.export().unwrap().as_bytes()).unwrap(), a);
    }
    #[test]
    fn blending_does_not_edit_head_geometry() {
        let d = Document::template(true);
        let before = d.export().unwrap();
        let mut a = Animation::default();
        a.set_weight("mouth.Open.Exclusive", 0.0, 1.0);
        let result = a.blend(&d, 0.0);
        assert_eq!(result.len(), d.positions().len());
        assert!(result
            .iter()
            .zip(d.positions())
            .any(|(a, b)| a.sub(*b).len() > 0.1));
        assert_eq!(before, d.export().unwrap());
    }
    #[test]
    fn unknown_tracks_survive_export() {
        let mut a = Animation::default();
        a.set_weight("custom.Head.Something", 0.0, -0.5);
        assert_eq!(Animation::load(a.export().unwrap().as_bytes()).unwrap(), a);
    }
    #[test]
    fn invalid_time_and_nonfinite_weights_are_rejected() {
        let mut a = Animation::default();
        a.set_weight("mouth.Open.Exclusive", 0.0, f32::NAN);
        assert!(a.export().is_err());
        a.tracks[0].keys[0].weight = 0.5;
        a.tracks[0].keys[0].time = 5.0;
        assert!(a.export().is_err());
    }
    #[test]
    fn binary_cpf_import_uses_named_tracks() {
        let mut b = b"CPF\0".to_vec();
        b.extend(919_i32.to_le_bytes());
        b.extend(1_f32.to_le_bytes());
        b.extend(1_i32.to_le_bytes());
        let name = b"mouth.Open.Exclusive\0";
        b.extend((name.len() as i32).to_le_bytes());
        b.extend(name);
        b.extend(1_i32.to_le_bytes());
        b.extend(0_f32.to_le_bytes());
        b.extend(0.5_f32.to_le_bytes());
        let a = Animation::load(&b).unwrap();
        assert_eq!(a.weight("mouth.Open.Exclusive", 0.0), 0.5);
        assert!(Animation::load(&b[..b.len() - 1]).is_err());
    }
}
