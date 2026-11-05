//! Repair the female seam using the same vertex IDs and full vertex records
//! recovered from product 59412 revision 17, not the separate collar mesh.
use std::io::Cursor;
use xmltree::{Element, XMLNode};

pub const VERTICES: &[usize] = &[207, 223, 226, 229, 719, 720, 722, 724, 726, 728, 730];
pub const BAND: &[usize] = &[
    44, 45, 206, 207, 208, 215, 216, 223, 224, 225, 226, 229, 230, 342, 343, 398, 515, 521, 522,
    527, 633, 634, 718, 719, 720, 721, 722, 723, 724, 725, 726, 727, 728, 729, 730,
];
pub fn detect_legacy(root: &Element, female: bool) -> bool {
    if root
        .get_child("HEADER")
        .and_then(|h| h.attributes.get("HEADMAKER_NECK_EDITS"))
        .is_some()
    {
        return false;
    }
    let source = if female {
        include_str!("../assets/NewExportHead6.xmf")
    } else {
        include_str!("../assets/MaleHeadStart_9b.xmf")
    };
    let Ok(template) = Element::parse(Cursor::new(format!("<root>{source}</root>"))) else {
        return false;
    };
    let Some(skin) = root.get_child("MESH").and_then(|m| m.get_child("SUBMESH")) else {
        return false;
    };
    if skin.attributes.get("NUMVERTICES").map(String::as_str) != Some("755") {
        return false;
    }
    let reference = template
        .get_child("MESH")
        .unwrap()
        .get_child("SUBMESH")
        .unwrap();
    let mut matches = 0;
    for &id in VERTICES {
        let find = |e: &Element| {
            e.children
                .iter()
                .filter_map(XMLNode::as_element)
                .find(|v| {
                    v.name == "VERTEX"
                        && v.attributes.get("ID").and_then(|n| n.parse::<usize>().ok()) == Some(id)
                })
                .cloned()
        };
        if let (Some(a), Some(b)) = (find(skin), find(reference)) {
            let values = |v: &Element| {
                v.get_child("POS")
                    .and_then(|p| p.get_text())
                    .map(|s| {
                        s.split_whitespace()
                            .filter_map(|n| n.parse::<f32>().ok())
                            .collect::<Vec<_>>()
                    })
                    .unwrap_or_default()
            };
            let a = values(&a);
            let b = values(&b);
            if a.len() == 3 && b.len() == 3 && a.iter().zip(b).all(|(x, y)| (x - y).abs() < 0.02) {
                matches += 1;
            }
        }
    }
    matches >= 9
}

pub fn repair(root: &mut Element, female: bool) -> bool {
    if root
        .get_child("HEADER")
        .and_then(|h| h.attributes.get("HEADMAKER_NECK_EDITS"))
        .map(String::as_str)
        == Some("1")
    {
        return false;
    }
    let Some(mesh) = root.get_mut_child("MESH") else {
        return false;
    };
    let counts: Vec<_> = mesh
        .children
        .iter()
        .filter_map(XMLNode::as_element)
        .filter(|e| e.name == "SUBMESH")
        .map(|s| {
            s.attributes
                .get("NUMVERTICES")
                .and_then(|v| v.parse::<usize>().ok())
        })
        .collect();
    if counts != [Some(755), Some(114), Some(52), Some(35)] {
        return false;
    }
    let Some(skin) = mesh.get_mut_child("SUBMESH") else {
        return false;
    };
    // Do not apply the female reference to a male mesh selected in the wrong tab.
    // The male seam has bone 20 weights; the female seam is fully on bone 21.
    for &id in VERTICES {
        let vertex = skin
            .children
            .iter()
            .filter_map(XMLNode::as_element)
            .find(|e| {
                e.name == "VERTEX"
                    && e.attributes.get("ID").and_then(|v| v.parse::<usize>().ok()) == Some(id)
            });
        let Some(v) = vertex else {
            return false;
        };
        let influences: Vec<_> = v
            .children
            .iter()
            .filter_map(XMLNode::as_element)
            .filter(|e| e.name == "INFLUENCE")
            .collect();
        let female_binding = influences.len() == 1
            && influences[0].attributes.get("ID").map(String::as_str) == Some("21")
            && influences[0]
                .get_text()
                .and_then(|v| v.trim().parse::<f32>().ok())
                == Some(1.0);
        let male_binding = influences.iter().any(|i| {
            i.attributes.get("ID").map(String::as_str) == Some("20")
                && i.get_text()
                    .and_then(|v| v.trim().parse::<f32>().ok())
                    .is_some_and(|w| w >= 0.75)
        });
        if !(if female { female_binding } else { male_binding }) {
            return false;
        }
    }
    let reference = Element::parse(Cursor::new(if female {
        include_str!("../assets/female-neck-reference.xml")
    } else {
        include_str!("../assets/male-neck-reference.xml")
    }))
    .expect("Bundled original neck reference");
    let mut changed = false;
    for entry in reference.children.iter().filter_map(XMLNode::as_element) {
        let id = &entry.attributes["ID"];
        if let Some(v) = skin
            .children
            .iter_mut()
            .filter_map(XMLNode::as_mut_element)
            .find(|e| e.name == "VERTEX" && e.attributes.get("ID") == Some(id))
        {
            // Keep unknown per-vertex fields, replacing only the calibrated seam data.
            for name in ["POS", "NORM", "TEXCOORD", "INFLUENCE"] {
                let old: Vec<_> = v
                    .children
                    .iter()
                    .filter_map(XMLNode::as_element)
                    .filter(|e| e.name == name)
                    .cloned()
                    .collect();
                let replacement: Vec<_> = entry
                    .children
                    .iter()
                    .filter_map(XMLNode::as_element)
                    .filter(|e| e.name == name)
                    .cloned()
                    .collect();
                if old != replacement {
                    changed = true;
                }
                // Replace in place. Cal3D requires POS/NORM/COLOR/UV/INFLUENCE
                // order; removing and appending moves COLOR ahead of POS.
                let mut inserted = false;
                let mut updated = Vec::new();
                for n in std::mem::take(&mut v.children) {
                    if n.as_element().is_some_and(|e| e.name == name) {
                        if !inserted {
                            updated.extend(replacement.iter().cloned().map(XMLNode::Element));
                            inserted = true;
                        }
                    } else {
                        updated.push(n);
                    }
                }
                if !inserted {
                    updated.extend(replacement.into_iter().map(XMLNode::Element));
                }
                v.children = updated;
            }
            v.attributes.insert(
                "NUMINFLUENCES".into(),
                entry.attributes["NUMINFLUENCES"].clone(),
            );
        }
    }
    // Keep both the body seam and its first head-bound transition row intact.
    for m in skin
        .children
        .iter_mut()
        .filter_map(XMLNode::as_mut_element)
        .filter(|e| e.name == "MORPH")
    {
        let before = m.children.len();
        m.children.retain(|n| {
            !n.as_element().is_some_and(|e| {
                e.name == "BLENDVERTEX"
                    && e.attributes
                        .get("VERTEXID")
                        .and_then(|v| v.parse::<usize>().ok())
                        .is_some_and(|i| BAND.contains(&i))
            })
        });
        if before != m.children.len() {
            let count = m
                .children
                .iter()
                .filter_map(XMLNode::as_element)
                .filter(|e| e.name == "BLENDVERTEX")
                .count();
            m.attributes
                .insert("NUMBLENDVERTS".into(), count.to_string());
            changed = true;
        }
    }
    changed
}
