//! Bounded reader for IMVU's uncompressed Cal3D CMF 919 mesh layout.
use xmltree::{Element, XMLNode};
struct Reader<'a> {
    bytes: &'a [u8],
    at: usize,
}
impl Reader<'_> {
    fn bytes(&mut self, n: usize) -> Result<&[u8], String> {
        let end = self.at.checked_add(n).ok_or("CMF size overflow")?;
        let b = self.bytes.get(self.at..end).ok_or("Truncated CMF")?;
        self.at = end;
        Ok(b)
    }
    fn int(&mut self) -> Result<i32, String> {
        Ok(i32::from_le_bytes(self.bytes(4)?.try_into().unwrap()))
    }
    fn count(&mut self, max: usize) -> Result<usize, String> {
        let n = self.int()?;
        if n < 0 || n as usize > max {
            Err("CMF count exceeds supported limits".into())
        } else {
            Ok(n as usize)
        }
    }
    fn float(&mut self) -> Result<f32, String> {
        let v = f32::from_le_bytes(self.bytes(4)?.try_into().unwrap());
        if v.is_finite() {
            Ok(v)
        } else {
            Err("Non-finite CMF coordinate".into())
        }
    }
    fn values(&mut self, n: usize) -> Result<String, String> {
        (0..n)
            .map(|_| self.float().map(|v| v.to_string()))
            .collect::<Result<Vec<_>, _>>()
            .map(|v| v.join(" "))
    }
}
fn child(e: &mut Element, name: &str, value: String) {
    let mut c = Element::new(name);
    c.children.push(XMLNode::Text(value));
    e.children.push(XMLNode::Element(c));
}
pub fn decode(bytes: &[u8]) -> Result<String, String> {
    decode_impl(bytes, false)
}
/// Poser preview keeps the highest-detail vertex/face arrays and drops only
/// runtime LOD collapse metadata. HeadMaker's strict import remains unchanged.
pub fn decode_preview(bytes: &[u8]) -> Result<String, String> {
    decode_impl(bytes, true)
}
fn decode_impl(bytes: &[u8], allow_lod: bool) -> Result<String, String> {
    let mut r = Reader { bytes, at: 0 };
    if r.bytes(4)? != b"CMF\0" {
        return Err("Not a CMF mesh".into());
    }
    let version = r.int()?;
    if version != 919 {
        return Err(format!(
            "CMF version {version} is not supported; expected 919"
        ));
    }
    let count = r.count(64)?;
    let mut mesh = Element::new("MESH");
    mesh.attributes
        .insert("NUMSUBMESH".into(), count.to_string());
    let mut total = 0;
    for _ in 0..count {
        let material = r.int()?;
        let nv = r.count(100_000)?;
        total += nv;
        if total > 200_000 {
            return Err("CMF exceeds 200,000 vertices".into());
        }
        let nf = r.count(400_000)?;
        let lod = r.count(100_000)?;
        let ns = r.count(100_000)?;
        let nt = r.count(8)?;
        let nm = r.count(256)?;
        if ns != 0 || (!allow_lod && lod != 0) {
            return Err("CMF meshes with LOD or springs are not supported".into());
        }
        let mut s = Element::new("SUBMESH");
        for (key, n) in [
            ("MATERIAL", material as usize),
            ("NUMVERTICES", nv),
            ("NUMFACES", nf),
            ("NUMLODSTEPS", if allow_lod { 0 } else { lod }),
            ("NUMSPRINGS", ns),
            ("NUMTEXCOORDS", nt),
            ("NUMMORPHS", nm),
        ] {
            s.attributes.insert(key.into(), n.to_string());
        }
        for id in 0..nv {
            let mut v = Element::new("VERTEX");
            v.attributes.insert("ID".into(), id.to_string());
            for name in ["POS", "NORM", "COLOR"] {
                child(&mut v, name, r.values(3)?);
            }
            let _collapse = r.int()?;
            let _collapsed_faces = r.int()?;
            for _ in 0..nt {
                child(&mut v, "TEXCOORD", r.values(2)?);
            }
            let ni = r.count(64)?;
            v.attributes.insert("NUMINFLUENCES".into(), ni.to_string());
            for _ in 0..ni {
                let id = r.int()?;
                let weight = r.float()?;
                let mut i = Element::new("INFLUENCE");
                i.attributes.insert("ID".into(), id.to_string());
                i.children.push(XMLNode::Text(weight.to_string()));
                v.children.push(XMLNode::Element(i));
            }
            s.children.push(XMLNode::Element(v));
        }
        for id in 0..nm {
            let length = r.count(4096)?;
            let name = String::from_utf8(r.bytes(length)?.to_vec())
                .map_err(|_| "Invalid CMF morph name")?
                .trim_end_matches('\0')
                .to_string();
            let mut m = Element::new("MORPH");
            m.attributes.insert("NAME".into(), name);
            m.attributes.insert("MORPHID".into(), id.to_string());
            let mut next = r.int()?;
            let mut previous = None;
            let mut blends = 0;
            while next >= 0 && (next as usize) < nv {
                if previous.is_some_and(|p| next <= p) {
                    return Err("Invalid sparse CMF morph order".into());
                }
                previous = Some(next);
                let mut v = Element::new("BLENDVERTEX");
                v.attributes.insert("VERTEXID".into(), next.to_string());
                child(&mut v, "POSITION", r.values(3)?);
                child(&mut v, "NORMAL", r.values(3)?);
                for _ in 0..nt {
                    child(&mut v, "TEXCOORD", r.values(2)?);
                }
                m.children.push(XMLNode::Element(v));
                blends += 1;
                next = r.int()?;
            }
            m.attributes
                .insert("NUMBLENDVERTS".into(), blends.to_string());
            s.children.push(XMLNode::Element(m));
        }
        for _ in 0..nf {
            let ids = [
                r.count(nv.saturating_sub(1))?,
                r.count(nv.saturating_sub(1))?,
                r.count(nv.saturating_sub(1))?,
            ];
            let mut f = Element::new("FACE");
            f.attributes.insert(
                "VERTEXID".into(),
                format!("{} {} {}", ids[0], ids[1], ids[2]),
            );
            s.children.push(XMLNode::Element(f));
        }
        mesh.children.push(XMLNode::Element(s));
    }
    if r.at != bytes.len() {
        return Err("CMF has unparsed trailing data".into());
    }
    let mut header = Element::new("HEADER");
    header.attributes.insert("MAGIC".into(), "XMF".into());
    header.attributes.insert("VERSION".into(), "919".into());
    let mut out = Vec::new();
    let config = xmltree::EmitterConfig::new().write_document_declaration(false);
    header
        .write_with_config(&mut out, config.clone())
        .map_err(|e| e.to_string())?;
    mesh.write_with_config(&mut out, config)
        .map_err(|e| e.to_string())?;
    String::from_utf8(out).map_err(|e| e.to_string())
}

// Uses one of the developer's IMVU avatar files, which are not published.
#[cfg(all(test, feature = "bundled-imvu-avatars"))]
mod preview_tests {
    use super::*;
    #[test]
    fn avatar_lod_is_previewed_at_full_detail_without_relaxing_head_import() {
        let bytes = include_bytes!("../assets/poser/female/female01.anime01.nakedLeftFoot01.xmf");
        assert!(decode(bytes).is_err());
        let xml = decode_preview(bytes).unwrap();
        let root = Element::parse(std::io::Cursor::new(format!("<root>{xml}</root>"))).unwrap();
        let part = root
            .get_child("MESH")
            .unwrap()
            .get_child("SUBMESH")
            .unwrap();
        assert_eq!(part.attributes["NUMLODSTEPS"], "0");
        assert_eq!(part.attributes["NUMVERTICES"], "203");
        assert_eq!(part.attributes["NUMFACES"], "296");
        assert_eq!(
            part.children
                .iter()
                .filter(|n| n.as_element().is_some_and(|e| e.name == "VERTEX"))
                .count(),
            203
        );
        assert!(decode_preview(&bytes[..bytes.len() - 1]).is_err());
    }
}
