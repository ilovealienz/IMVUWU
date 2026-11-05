//! Ordered Cal3D 919 XML checks, separate from our name-based editor parser.
//! IMVU's xmlformat.cpp reads these records using first_node/next_sibling.
use xmltree::{Element, XMLNode};

fn elements(e: &Element) -> Vec<&Element> {
    e.children.iter().filter_map(XMLNode::as_element).collect()
}
fn count(e: &Element, key: &str) -> Result<usize, String> {
    e.attributes
        .get(key)
        .ok_or_else(|| format!("{} missing {key}", e.name))?
        .parse()
        .map_err(|_| format!("Invalid {key}"))
}
fn take<'a>(nodes: &[&'a Element], i: &mut usize, name: &str) -> Result<&'a Element, String> {
    let e = nodes
        .get(*i)
        .ok_or_else(|| format!("Expected {name}, found end of record"))?;
    if e.name != name {
        return Err(format!("Expected {name}, found {}", e.name));
    }
    *i += 1;
    Ok(e)
}
pub fn order_vertex_fields(root: &mut Element) {
    fn walk(e: &mut Element) {
        if e.name == "VERTEX" {
            e.children
                .sort_by_key(|n| match n.as_element().map(|e| e.name.as_str()) {
                    Some("POS") => 0,
                    Some("NORM") => 1,
                    Some("COLOR") => 2,
                    Some("COLLAPSEID") => 3,
                    Some("COLLAPSECOUNT") => 4,
                    Some("TEXCOORD") => 5,
                    Some("INFLUENCE") => 6,
                    Some("PHYSIQUE") => 7,
                    _ => 8,
                });
        }
        for c in e.children.iter_mut().filter_map(XMLNode::as_mut_element) {
            walk(c);
        }
    }
    walk(root);
}
pub fn validate(root: &Element) -> Result<(), String> {
    let header = root.get_child("HEADER").ok_or("Missing XMF header")?;
    if header.attributes.get("MAGIC").map(String::as_str) != Some("XMF") {
        return Err("Not an XMF mesh".into());
    }
    let version = count(header, "VERSION")?;
    let mesh = root.get_child("MESH").ok_or("Missing MESH")?;
    let parts = elements(mesh);
    if parts.len() != count(mesh, "NUMSUBMESH")? {
        return Err("Submesh count mismatch".into());
    }
    for (part_id, sm) in parts.iter().enumerate() {
        if sm.name != "SUBMESH" {
            return Err("Expected SUBMESH".into());
        }
        let nv = count(sm, "NUMVERTICES")?;
        let nt = count(sm, "NUMTEXCOORDS")?;
        let nodes = elements(sm);
        let mut cursor = 0;
        for id in 0..nv {
            let v = take(&nodes, &mut cursor, "VERTEX")?;
            if count(v, "ID")? != id {
                return Err(format!("Part {part_id}: vertices must be in ID order"));
            }
            let fields = elements(v);
            let mut k = 0;
            for tag in ["POS", "NORM"] {
                take(&fields, &mut k, tag)
                    .map_err(|e| format!("Part {part_id} vertex {id}: {e}"))?;
            }
            if version >= 911 || fields.get(k).is_some_and(|e| e.name == "COLOR") {
                take(&fields, &mut k, "COLOR")?;
            }
            if fields.get(k).is_some_and(|e| e.name == "COLLAPSEID") {
                take(&fields, &mut k, "COLLAPSEID")?;
                take(&fields, &mut k, "COLLAPSECOUNT")?;
            }
            for _ in 0..nt {
                take(&fields, &mut k, "TEXCOORD")?;
            }
            for _ in 0..count(v, "NUMINFLUENCES")? {
                take(&fields, &mut k, "INFLUENCE")?;
            }
            if count(sm, "NUMSPRINGS")? > 0 {
                take(&fields, &mut k, "PHYSIQUE")?;
            }
            if k != fields.len() {
                return Err(format!(
                    "Part {part_id} vertex {id}: unexpected field {}",
                    fields[k].name
                ));
            }
        }
        for _ in 0..count(sm, "NUMSPRINGS")? {
            take(&nodes, &mut cursor, "SPRING")?;
        }
        for _ in 0..count(sm, "NUMMORPHS")? {
            let m = take(&nodes, &mut cursor, "MORPH")?;
            let blends = elements(m);
            if blends.len() != count(m, "NUMBLENDVERTS")? {
                return Err("Morph count mismatch".into());
            }
            let mut previous = None;
            for bv in blends {
                if bv.name != "BLENDVERTEX" {
                    return Err("Expected BLENDVERTEX".into());
                }
                let id = count(bv, "VERTEXID")?;
                if id >= nv || previous.is_some_and(|p| id <= p) {
                    return Err("Blend vertices must be sorted and unique".into());
                }
                previous = Some(id);
                let fields = elements(bv);
                let mut k = 0;
                take(&fields, &mut k, "POSITION")?;
                take(&fields, &mut k, "NORMAL")?;
                for _ in 0..nt {
                    take(&fields, &mut k, "TEXCOORD")?;
                }
                // Legacy exports can leave extra UVs in morphs; Cal3D ignores them.
                if fields[k..].iter().any(|e| e.name != "TEXCOORD") {
                    return Err("Unexpected blend vertex field".into());
                }
            }
        }
        for _ in 0..count(sm, "NUMFACES")? {
            take(&nodes, &mut cursor, "FACE")?;
        }
        if cursor != nodes.len() {
            return Err("Unexpected submesh records".into());
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;
    fn parse(s: &str) -> Element {
        Element::parse(Cursor::new(format!("<root>{s}</root>"))).unwrap()
    }
    #[test]
    fn reproduces_reported_import_rejection() {
        let good = parse(include_str!("../tests/fixtures/Head-working.xmf"));
        validate(&good).unwrap();
        let bad = parse(include_str!("../tests/fixtures/testfrom2-failed.xmf"));
        let error = validate(&bad).unwrap_err();
        assert!(
            error.contains("vertex 207: Expected POS, found COLOR"),
            "{error}"
        );
        let mut repaired = bad;
        order_vertex_fields(&mut repaired);
        validate(&repaired).unwrap();
    }
    #[test]
    fn all_edits_export_enough_uvs_for_cal3d() {
        for female in [true, false] {
            let mut d = crate::model::Document::template(female);
            // Skin, eyes, lashes and brows must all produce loader-compatible morphs.
            let mut ps = d.positions().to_vec();
            for i in [0, 755, 869, 921] {
                ps[i].0 += 1.0;
            }
            d.change(ps);
            validate(&parse(&d.export().unwrap())).unwrap();
        }
    }
    #[test]
    fn failed_export_can_be_resaved_without_losing_face_edits() {
        let doc = crate::model::Document::parse(
            include_str!("../tests/fixtures/testfrom2-failed.xmf"),
            true,
        )
        .unwrap();
        let output = doc.export().unwrap();
        validate(&parse(&output)).unwrap();
        let reloaded = crate::model::Document::parse(&output, true).unwrap();
        assert_eq!(doc.morphs, reloaded.morphs);
        assert_eq!(
            output.as_bytes().iter().filter(|&&b| b == b'\n').count(),
            output.matches("\r\n").count()
        );
    }
}
