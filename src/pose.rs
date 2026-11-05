//! Skeletal pose math and bounded Cal3D XSF/XAF XML IO.
//! Original 87-bone skeleton recovered from the supplied Avatar Pose SWF.
//! The supplied IMVU Toolkit's ToolkitExportUtils.py writes quaternion w with
//! opposite sign to Blender. Native file rotations are converted only at IO.
use crate::model::V;
use std::{collections::BTreeSet, io::Cursor};
use xmltree::{Element, XMLNode};

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Quat(pub f32, pub f32, pub f32, pub f32);
impl Default for Quat {
    fn default() -> Self {
        Self::identity()
    }
}
impl Quat {
    pub fn identity() -> Self {
        Self(0.0, 0.0, 0.0, 1.0)
    }
    pub fn dot(self, b: Self) -> f32 {
        self.0 * b.0 + self.1 * b.1 + self.2 * b.2 + self.3 * b.3
    }
    pub fn normalized(self) -> Self {
        let n = self.dot(self).sqrt();
        if n < 1e-12 || !n.is_finite() {
            return Self::identity();
        }
        Self(self.0 / n, self.1 / n, self.2 / n, self.3 / n)
    }
    pub fn conjugate(self) -> Self {
        Self(-self.0, -self.1, -self.2, self.3)
    }
    pub fn mul(self, b: Self) -> Self {
        let Self(x, y, z, w) = self;
        let Self(a, b, c, d) = b;
        Self(
            w * a + x * d + y * c - z * b,
            w * b - x * c + y * d + z * a,
            w * c + x * b - y * a + z * d,
            w * d - x * a - y * b - z * c,
        )
    }
    pub fn rotate(self, v: V) -> V {
        let q = self.normalized();
        let u = V(q.0, q.1, q.2);
        let t = u.cross(v).mul(2.0);
        v.add(t.mul(q.3)).add(u.cross(t))
    }
    pub fn from_euler_degrees(v: V) -> Self {
        let (x, y, z) = (
            v.0.to_radians() * 0.5,
            v.1.to_radians() * 0.5,
            v.2.to_radians() * 0.5,
        );
        let (sx, cx) = x.sin_cos();
        let (sy, cy) = y.sin_cos();
        let (sz, cz) = z.sin_cos();
        Self(0.0, 0.0, sz, cz)
            .mul(Self(0.0, sy, 0.0, cy))
            .mul(Self(sx, 0.0, 0.0, cx))
            .normalized()
    }
    pub fn slerp(self, mut b: Self, t: f32) -> Self {
        let a = self.normalized();
        b = b.normalized();
        let mut dot = a.dot(b);
        if dot < 0.0 {
            b = Self(-b.0, -b.1, -b.2, -b.3);
            dot = -dot;
        }
        let (u, v) = if dot > 0.9995 {
            (1.0 - t, t)
        } else {
            let angle = dot.clamp(-1.0, 1.0).acos();
            let s = angle.sin();
            (((1.0 - t) * angle).sin() / s, (t * angle).sin() / s)
        };
        Self(
            a.0 * u + b.0 * v,
            a.1 * u + b.1 * v,
            a.2 * u + b.2 * v,
            a.3 * u + b.3 * v,
        )
        .normalized()
    }
    pub fn valid(self) -> bool {
        [self.0, self.1, self.2, self.3]
            .iter()
            .all(|v| v.is_finite())
            && self.dot(self).is_finite()
            && self.dot(self) > 1e-12
    }
}
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Transform {
    pub position: V,
    pub rotation: Quat,
}
impl Transform {
    pub fn transform_point(self, p: V) -> V {
        self.position.add(self.rotation.rotate(p))
    }
    pub fn inverse(self) -> Self {
        let rotation = self.rotation.normalized().conjugate();
        Self {
            position: rotation.rotate(self.position.mul(-1.0)),
            rotation,
        }
    }
    pub fn mul(self, b: Self) -> Self {
        Self {
            position: self.transform_point(b.position),
            rotation: self.rotation.mul(b.rotation).normalized(),
        }
    }
    pub fn valid(self) -> bool {
        [self.position.0, self.position.1, self.position.2]
            .iter()
            .all(|v| v.is_finite())
            && self.rotation.valid()
    }
}
#[derive(Clone, Debug)]
pub struct Bone {
    pub id: usize,
    pub name: String,
    pub parent: Option<usize>,
    pub translation: V,
    pub rotation: Quat,
}
#[derive(Clone, Debug)]
pub struct Skeleton {
    pub bones: Vec<Bone>,
}
#[derive(Clone, Debug, PartialEq)]
pub struct Pose {
    pub local: Vec<Transform>,
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
fn number(s: &str) -> Result<f32, String> {
    let n = s.parse::<f32>().map_err(|_| "Invalid pose number")?;
    if n.is_finite() {
        Ok(n)
    } else {
        Err("Non-finite pose number".into())
    }
}
fn nums(s: &str, n: usize) -> Result<Vec<f32>, String> {
    let v = s
        .split_whitespace()
        .map(number)
        .collect::<Result<Vec<_>, _>>()?;
    if v.len() != n {
        return Err(format!("Expected {n} pose coordinates"));
    }
    Ok(v)
}
fn text(e: &Element, n: &str) -> Result<String, String> {
    e.get_child(n)
        .and_then(|e| e.get_text())
        .map(|s| s.into_owned())
        .ok_or_else(|| format!("Missing {n}"))
}
fn parse_v(e: &Element, n: &str) -> Result<V, String> {
    let v = nums(&text(e, n)?, 3)?;
    Ok(V(v[0], v[1], v[2]))
}
fn parse_q(e: &Element, n: &str) -> Result<Quat, String> {
    let v = nums(&text(e, n)?, 4)?;
    let q = Quat(v[0], v[1], v[2], -v[3]);
    if !q.valid() {
        return Err("Invalid zero quaternion".into());
    }
    Ok(q.normalized())
}
fn xml(bytes: &[u8]) -> Result<Element, String> {
    if bytes.len() > 16 * 1024 * 1024 {
        return Err("Pose file exceeds 16 MB".into());
    }
    let s = std::str::from_utf8(bytes)
        .map_err(|_| "Expected XML XSF/XAF, not binary")?
        .trim_start_matches('\u{feff}')
        .trim();
    let s = if s.starts_with("<?xml") {
        &s[s.find("?>").ok_or("Unclosed XML declaration")? + 2..]
    } else {
        s
    };
    Element::parse(Cursor::new(format!("<root>{s}</root>"))).map_err(|e| e.to_string())
}
fn validate_header(root: &Element, magic: &str, required: bool) -> Result<(), String> {
    if let Some(h) = root.get_child("HEADER") {
        if h.attributes.get("MAGIC").map(String::as_str) != Some(magic) {
            return Err(format!("Expected {magic} header"));
        }
        let v = attr(h, "VERSION")?;
        if !(699..=919).contains(&v) {
            return Err(format!("Unsupported {magic} version {v}"));
        }
    } else if required {
        return Err(format!("Missing {magic} header"));
    }
    Ok(())
}
fn node(n: &str, v: String) -> XMLNode {
    let mut e = Element::new(n);
    e.children.push(XMLNode::Text(v));
    XMLNode::Element(e)
}
fn fmt_v(v: V) -> String {
    format!("{:.7} {:.7} {:.7}", v.0, v.1, v.2)
}
fn fmt_q(v: Quat) -> String {
    let q = v.normalized();
    format!("{:.8} {:.8} {:.8} {:.8}", q.0, q.1, q.2, -q.3)
}
impl Skeleton {
    pub fn bundled() -> Self {
        Self::parse(include_bytes!("../assets/poser/original-avatar.xsf"))
            .expect("Bundled avatar skeleton")
    }
    pub fn parse(bytes: &[u8]) -> Result<Self, String> {
        let root = xml(bytes)?;
        validate_header(&root, "XSF", false)?;
        let s = root.get_child("SKELETON").ok_or("Missing SKELETON")?;
        let count = attr(s, "NUMBONES")?;
        if count == 0 || count > 512 {
            return Err("Skeleton must contain 1–512 bones".into());
        }
        let mut bones = Vec::new();
        // Bone IDs, not names, identify Cal3D tracks. The shipped original has
        // a duplicate finger label at IDs 51/52; retain its source spelling.
        for e in elements(s, "BONE") {
            let id = attr(e, "ID")?;
            let name = e.attributes.get("NAME").ok_or("Bone has no name")?.clone();
            if name.is_empty() || name.len() > 256 {
                return Err("Empty or excessive bone name".into());
            }
            let p = text(e, "PARENTID")?
                .parse::<i32>()
                .map_err(|_| "Invalid parent ID")?;
            if p < -1 {
                return Err("Invalid parent ID".into());
            }
            let parent = if p < 0 { None } else { Some(p as usize) };
            bones.push(Bone {
                id,
                name,
                parent,
                translation: parse_v(e, "TRANSLATION")?,
                rotation: parse_q(e, "ROTATION")?,
            });
        }
        if bones.len() != count {
            return Err("Skeleton NUMBONES mismatch".into());
        }
        bones.sort_by_key(|b| b.id);
        for (i, b) in bones.iter().enumerate() {
            if b.id != i || b.parent.is_some_and(|p| p >= count || p == i) {
                return Err(
                    "Bone IDs must be unique, contiguous and reference valid parents".into(),
                );
            }
        }
        for i in 0..count {
            let mut current = Some(i);
            let mut seen = BTreeSet::new();
            while let Some(j) = current {
                if !seen.insert(j) {
                    return Err("Skeleton parent cycle".into());
                }
                current = bones[j].parent;
            }
        }
        Ok(Self { bones })
    }
    pub fn rest_pose(&self) -> Pose {
        Pose {
            local: self
                .bones
                .iter()
                .map(|b| Transform {
                    position: b.translation,
                    rotation: b.rotation,
                })
                .collect(),
        }
    }
    pub fn world(&self, pose: &Pose) -> Vec<Transform> {
        fn calculate(i: usize, s: &Skeleton, p: &Pose, out: &mut [Option<Transform>]) -> Transform {
            if let Some(t) = out[i] {
                return t;
            }
            let b = &s.bones[i];
            let local = p.local.get(i).copied().unwrap_or(Transform {
                position: b.translation,
                rotation: b.rotation,
            });
            let t = if let Some(parent) = b.parent {
                calculate(parent, s, p, out).mul(local)
            } else {
                local
            };
            out[i] = Some(t);
            t
        }
        let mut out = vec![None; self.bones.len()];
        for i in 0..out.len() {
            calculate(i, self, pose, &mut out);
        }
        out.into_iter().map(Option::unwrap).collect()
    }
    /// First matching source label. Track IO always uses the unique bone ID.
    pub fn find(&self, name: &str) -> Option<usize> {
        self.bones.iter().position(|b| b.name == name)
    }
    /// Full avatar controls omit the scene root and its auxiliary pet locator.
    /// On the recovered original this is exactly IDs 1 through 85.
    pub fn control_ids(&self) -> Vec<usize> {
        self.bones
            .iter()
            .filter(|b| b.parent.is_some() && b.name != "zHead")
            .map(|b| b.id)
            .collect()
    }
}
impl Pose {
    pub fn validate(&self, skeleton: &Skeleton) -> Result<(), String> {
        if self.local.len() != skeleton.bones.len() || self.local.iter().any(|t| !t.valid()) {
            Err("Pose must have one finite transform per skeleton bone".into())
        } else {
            Ok(())
        }
    }
    pub fn export_xaf(
        &self,
        skeleton: &Skeleton,
        duration: f32,
        all_bones: bool,
    ) -> Result<String, String> {
        self.validate(skeleton)?;
        if !duration.is_finite() || duration <= 0.0 || duration > 86400.0 {
            return Err("Pose duration must be positive and at most 86400 seconds".into());
        }
        let rest = skeleton.rest_pose();
        let selected: Vec<_> = self
            .local
            .iter()
            .enumerate()
            .filter(|(i, t)| {
                all_bones
                    || t.position.sub(rest.local[*i].position).len() > 0.00001
                    || t.rotation.normalized().dot(rest.local[*i].rotation).abs() < 0.999999
            })
            .collect();
        if selected.is_empty() {
            return Err(
                "No changed joints to export; enable full-body export for a neutral pose".into(),
            );
        }
        let mut header = Element::new("HEADER");
        header.attributes.insert("MAGIC".into(), "XAF".into());
        header.attributes.insert("VERSION".into(), "919".into());
        let mut animation = Element::new("ANIMATION");
        animation
            .attributes
            .insert("DURATION".into(), duration.to_string());
        animation
            .attributes
            .insert("NUMTRACKS".into(), selected.len().to_string());
        for (i, t) in selected {
            let mut track = Element::new("TRACK");
            track
                .attributes
                .insert("BONEID".into(), skeleton.bones[i].id.to_string());
            track.attributes.insert("NUMKEYFRAMES".into(), "2".into());
            for time in [0.0, duration] {
                let mut key = Element::new("KEYFRAME");
                key.attributes.insert("TIME".into(), time.to_string());
                key.children.push(node("TRANSLATION", fmt_v(t.position)));
                key.children.push(node("ROTATION", fmt_q(t.rotation)));
                track.children.push(XMLNode::Element(key));
            }
            animation.children.push(XMLNode::Element(track));
        }
        let mut bytes = Vec::new();
        let config = xmltree::EmitterConfig::new()
            .perform_indent(true)
            .write_document_declaration(false)
            .line_separator("\r\n");
        for e in [header, animation] {
            e.write_with_config(&mut bytes, config.clone())
                .map_err(|e| e.to_string())?;
            bytes.extend_from_slice(b"\r\n");
        }
        String::from_utf8(bytes).map_err(|e| e.to_string())
    }
    pub fn load_xaf(skeleton: &Skeleton, bytes: &[u8], time: f32) -> Result<Self, String> {
        if !time.is_finite() {
            return Err("Pose sample time must be finite".into());
        }
        let animation = Animation::load(skeleton, bytes)?;
        Ok(animation.sample(skeleton, time))
    }
}
#[derive(Clone, Copy, Debug)]
pub struct PoseKey {
    pub time: f32,
    pub transform: Transform,
}
#[derive(Clone, Debug)]
pub struct PoseTrack {
    pub bone: usize,
    pub keys: Vec<PoseKey>,
}
#[derive(Clone, Debug)]
pub struct Animation {
    pub duration: f32,
    pub tracks: Vec<PoseTrack>,
}
impl Animation {
    pub fn validate(&self, skeleton: &Skeleton) -> Result<(), String> {
        if !self.duration.is_finite() || self.duration <= 0.0 || self.duration > 86400.0 {
            return Err("Animation duration must be positive and at most 86400 seconds".into());
        }
        if self.tracks.is_empty() || self.tracks.len() > 512 {
            return Err("Animation must contain 1–512 bone tracks".into());
        }
        let mut seen = BTreeSet::new();
        let mut total = 0;
        for t in &self.tracks {
            if t.bone >= skeleton.bones.len() || !seen.insert(t.bone) {
                return Err("Animation has an unknown or repeated bone ID".into());
            }
            if t.keys.is_empty() {
                return Err("Bone track has no keys".into());
            }
            let mut previous = -1.0;
            for k in &t.keys {
                if !k.time.is_finite()
                    || k.time < 0.0
                    || k.time > self.duration
                    || k.time <= previous
                    || !k.transform.valid()
                {
                    return Err(
                        "Animation keys must be finite, increasing and inside duration".into(),
                    );
                }
                previous = k.time;
                total += 1;
                if total > 200_000 {
                    return Err("Too many animation keys".into());
                }
            }
        }
        Ok(())
    }
    pub fn export(&self, skeleton: &Skeleton) -> Result<String, String> {
        self.validate(skeleton)?;
        let mut header = Element::new("HEADER");
        header.attributes.insert("MAGIC".into(), "XAF".into());
        header.attributes.insert("VERSION".into(), "919".into());
        let mut animation = Element::new("ANIMATION");
        animation
            .attributes
            .insert("DURATION".into(), self.duration.to_string());
        animation
            .attributes
            .insert("NUMTRACKS".into(), self.tracks.len().to_string());
        let mut tracks: Vec<_> = self.tracks.iter().collect();
        tracks.sort_by_key(|t| t.bone);
        for t in tracks {
            let mut track = Element::new("TRACK");
            track
                .attributes
                .insert("BONEID".into(), skeleton.bones[t.bone].id.to_string());
            track
                .attributes
                .insert("NUMKEYFRAMES".into(), t.keys.len().to_string());
            for k in &t.keys {
                let mut key = Element::new("KEYFRAME");
                key.attributes.insert("TIME".into(), k.time.to_string());
                key.children
                    .push(node("TRANSLATION", fmt_v(k.transform.position)));
                key.children
                    .push(node("ROTATION", fmt_q(k.transform.rotation)));
                track.children.push(XMLNode::Element(key));
            }
            animation.children.push(XMLNode::Element(track));
        }
        let mut bytes = Vec::new();
        let config = xmltree::EmitterConfig::new()
            .perform_indent(true)
            .write_document_declaration(false)
            .line_separator("\r\n");
        for e in [header, animation] {
            e.write_with_config(&mut bytes, config.clone())
                .map_err(|e| e.to_string())?;
            bytes.extend_from_slice(b"\r\n");
        }
        String::from_utf8(bytes).map_err(|e| e.to_string())
    }
    pub fn load(skeleton: &Skeleton, bytes: &[u8]) -> Result<Self, String> {
        let root = xml(bytes)?;
        validate_header(&root, "XAF", true)?;
        let a = root.get_child("ANIMATION").ok_or("Missing ANIMATION")?;
        let duration = number(a.attributes.get("DURATION").ok_or("Missing DURATION")?)?;
        if duration <= 0.0 || duration > 86400.0 {
            return Err("Invalid animation duration".into());
        }
        let mut tracks = Vec::new();
        let mut seen = BTreeSet::new();
        let mut total = 0;
        for t in elements(a, "TRACK") {
            let bone = attr(t, "BONEID")?;
            if bone >= skeleton.bones.len() || !seen.insert(bone) {
                return Err("XAF contains an unknown or repeated bone ID".into());
            }
            let mut keys = Vec::new();
            let mut previous = -1.0;
            for k in elements(t, "KEYFRAME") {
                let time = number(k.attributes.get("TIME").ok_or("Missing TIME")?)?;
                if time < 0.0 || time > duration + 0.00001 || time <= previous {
                    return Err(
                        "XAF key times must be unique, increasing and within duration".into(),
                    );
                }
                previous = time;
                keys.push(PoseKey {
                    time,
                    transform: Transform {
                        position: if k.get_child("TRANSLATION").is_some() {
                            parse_v(k, "TRANSLATION")?
                        } else {
                            skeleton.bones[bone].translation
                        },
                        rotation: parse_q(k, "ROTATION")?,
                    },
                });
                total += 1;
                if total > 200_000 {
                    return Err("Too many XAF keys".into());
                }
            }
            if keys.is_empty() || keys.len() != attr(t, "NUMKEYFRAMES")? {
                return Err("XAF NUMKEYFRAMES mismatch".into());
            }
            tracks.push(PoseTrack { bone, keys });
        }
        if tracks.is_empty() || tracks.len() != attr(a, "NUMTRACKS")? || tracks.len() > 512 {
            return Err("XAF NUMTRACKS mismatch".into());
        }
        Ok(Self { duration, tracks })
    }
    pub fn sample(&self, skeleton: &Skeleton, time: f32) -> Pose {
        let mut pose = skeleton.rest_pose();
        let time = time.clamp(0.0, self.duration);
        for t in &self.tracks {
            let key = if time <= t.keys[0].time {
                t.keys[0].transform
            } else if let Some(pair) = t.keys.windows(2).find(|p| time <= p[1].time) {
                let f = (time - pair[0].time) / (pair[1].time - pair[0].time);
                Transform {
                    position: pair[0]
                        .transform
                        .position
                        .lerp(pair[1].transform.position, f),
                    rotation: pair[0]
                        .transform
                        .rotation
                        .slerp(pair[1].transform.rotation, f),
                }
            } else {
                t.keys.last().unwrap().transform
            };
            pose.local[t.bone] = key;
        }
        pose
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn near(a: V, b: V) {
        assert!(a.sub(b).len() < 0.001, "{a:?} != {b:?}");
    }
    #[test]
    fn rotation_composition_and_inverse() {
        let t = Transform {
            position: V(1.0, 2.0, 3.0),
            rotation: Quat::from_euler_degrees(V(0.0, 0.0, 90.0)),
        };
        near(t.transform_point(V(1.0, 0.0, 0.0)), V(1.0, 3.0, 3.0));
        near(
            t.inverse()
                .transform_point(t.transform_point(V(4.0, 5.0, 6.0))),
            V(4.0, 5.0, 6.0),
        );
    }
    #[test]
    fn parent_rotation_moves_descendant_not_local_translation() {
        let s = Skeleton {
            bones: vec![
                Bone {
                    id: 0,
                    name: "root".into(),
                    parent: None,
                    translation: V::default(),
                    rotation: Quat::identity(),
                },
                Bone {
                    id: 1,
                    name: "child".into(),
                    parent: Some(0),
                    translation: V(2.0, 0.0, 0.0),
                    rotation: Quat::identity(),
                },
            ],
        };
        let mut p = s.rest_pose();
        p.local[0].rotation = Quat::from_euler_degrees(V(0.0, 0.0, 90.0));
        near(s.world(&p)[1].position, V(0.0, 2.0, 0.0));
        assert_eq!(p.local[1].position, V(2.0, 0.0, 0.0));
    }
    #[test]
    fn original_skeleton_rest_pose_has_plausible_world_positions() {
        let s = Skeleton::bundled();
        assert_eq!(s.bones.len(), 87);
        assert_eq!(s.find("Head"), Some(22));
        let w = s.world(&s.rest_pose());
        assert!((970.0..990.0).contains(&w[22].position.2));
        assert!((100.0..120.0).contains(&w[5].position.2));
        assert!((w[5].position.2 - w[11].position.2).abs() < 1.0);
    }
    #[test]
    fn xaf_round_trip_preserves_bone_ids_and_transforms() {
        let s = Skeleton::bundled();
        let mut p = s.rest_pose();
        p.local[27].rotation = p.local[27]
            .rotation
            .mul(Quat::from_euler_degrees(V(20.0, 0.0, 0.0)));
        p.local[0].position.0 = 12.0;
        let xml = p.export_xaf(&s, 2.0, true).unwrap();
        let r = Pose::load_xaf(&s, xml.as_bytes(), 1.0).unwrap();
        for (a, b) in p.local.iter().zip(&r.local) {
            near(a.position, b.position);
            assert!(a.rotation.dot(b.rotation).abs() > 0.99999);
        }
        assert_eq!(
            Animation::load(&s, xml.as_bytes()).unwrap().tracks.len(),
            87
        );
    }
    #[test]
    fn sparse_tracks_use_rest_for_omitted_bones() {
        let s = Skeleton::bundled();
        let mut p = s.rest_pose();
        p.local[22].rotation = Quat::identity();
        let xml = p.export_xaf(&s, 1.0, false).unwrap();
        let a = Animation::load(&s, xml.as_bytes()).unwrap();
        assert_eq!(a.tracks.len(), 1);
        assert_eq!(a.tracks[0].bone, 22);
        let sampled = a.sample(&s, 0.5);
        near(sampled.local[10].position, p.local[10].position);
    }
    #[test]
    fn original_rotation_only_tracks_use_the_skeleton_translation() {
        let s = Skeleton::bundled();
        let xml=b"<HEADER MAGIC=\"XAF\" VERSION=\"919\"/><ANIMATION DURATION=\"1\" NUMTRACKS=\"1\"><TRACK BONEID=\"22\" NUMKEYFRAMES=\"1\"><KEYFRAME TIME=\"0\"><ROTATION>0 0 0 1</ROTATION></KEYFRAME></TRACK></ANIMATION>";
        let p = Pose::load_xaf(&s, xml, 0.0).unwrap();
        near(p.local[22].position, s.bones[22].translation);
        assert!(p.local[22].rotation.dot(Quat::identity()).abs() > 0.99999);
    }
    #[test]
    fn animated_control_tracks_export_and_reload_without_root_or_pet_locator() {
        let s = Skeleton::bundled();
        assert_eq!(s.control_ids(), (1..86).collect::<Vec<_>>());
        let rest = s.rest_pose();
        let mut a = Animation {
            duration: 2.0,
            tracks: s
                .control_ids()
                .iter()
                .map(|&bone| PoseTrack {
                    bone,
                    keys: vec![
                        PoseKey {
                            time: 0.0,
                            transform: rest.local[bone],
                        },
                        PoseKey {
                            time: 2.0,
                            transform: rest.local[bone],
                        },
                    ],
                })
                .collect(),
        };
        let t = a.tracks.iter_mut().find(|t| t.bone == 27).unwrap();
        t.keys[1].transform.rotation = t.keys[1]
            .transform
            .rotation
            .mul(Quat::from_euler_degrees(V(20.0, 30.0, -10.0)));
        let xml = a.export(&s).unwrap();
        let loaded = Animation::load(&s, xml.as_bytes()).unwrap();
        assert_eq!(loaded.tracks.len(), 85);
        let expected = a.sample(&s, 1.0);
        let actual = loaded.sample(&s, 1.0);
        for (x, y) in expected.local.iter().zip(&actual.local) {
            near(x.position, y.position);
            assert!(x.rotation.dot(y.rotation).abs() > 0.99999);
        }
        a.tracks[0].keys[1].time = -1.0;
        assert!(a.export(&s).is_err());
        a.tracks[0].keys[1].time = 2.0;
        a.tracks[0].keys[1].transform.position.0 = f32::INFINITY;
        assert!(a.export(&s).is_err());
    }
    #[test]
    fn animation_sampling_interpolates_translation_and_shortest_rotation() {
        let s = Skeleton::bundled();
        let a = Animation {
            duration: 1.0,
            tracks: vec![PoseTrack {
                bone: 0,
                keys: vec![
                    PoseKey {
                        time: 0.0,
                        transform: Transform::default(),
                    },
                    PoseKey {
                        time: 1.0,
                        transform: Transform {
                            position: V(10.0, 0.0, 0.0),
                            rotation: Quat::from_euler_degrees(V(0.0, 0.0, 90.0)),
                        },
                    },
                ],
            }],
        };
        let p = a.sample(&s, 0.5);
        near(p.local[0].position, V(5.0, 0.0, 0.0));
        near(
            p.local[0].rotation.rotate(V(1.0, 0.0, 0.0)),
            V(0.5_f32.sqrt(), 0.5_f32.sqrt(), 0.0),
        );
    }
    #[test]
    fn malformed_pose_and_hierarchy_inputs_are_rejected() {
        let s = Skeleton::bundled();
        let xml = s.rest_pose().export_xaf(&s, 1.0, true).unwrap();
        for bad in [
            xml.replace("NUMTRACKS=\"87\"", "NUMTRACKS=\"88\""),
            xml.replacen("BONEID=\"0\"", "BONEID=\"999\"", 1),
            xml.replacen("<TRANSLATION>0.0000000", "<TRANSLATION>NaN", 1),
        ] {
            assert!(Animation::load(&s, bad.as_bytes()).is_err());
        }
        assert!(Skeleton::parse(b"<SKELETON NUMBONES=\"1\"><BONE ID=\"0\" NAME=\"bad\"><TRANSLATION>0 0 0</TRANSLATION><ROTATION>0 0 0 1</ROTATION><PARENTID>0</PARENTID></BONE></SKELETON>").is_err());
    }
}
