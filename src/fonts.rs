//! Fallback fonts from the operating system.
//!
//! egui draws all text with its own bundled fonts (Ubuntu-Light, Hack, Noto
//! Emoji, an icon font), identical on every OS. They cover Latin, Greek and
//! Cyrillic but not Japanese, Chinese, Korean, Thai, Vietnamese tone marks,
//! many decorative symbols or "fancy" maths letters, all common in IMVU
//! product and creator names, so those were drawn as empty boxes.
//!
//! At start-up a background thread looks through the system's installed
//! fonts (fontdb: the Windows Fonts folders, macOS's font folders, and on
//! Linux every folder fontconfig lists, NixOS included), picks one regular
//! face for each sample character the bundled fonts lack, and appends those
//! faces as fallbacks behind the bundled ones, so the app's own look is
//! unchanged. Nothing is bundled; a font that is not installed is simply not
//! used. Set `IMVUWU_NO_SYSTEM_FONTS=1` to skip this.
//!
//! Limits (egui's): no text shaping, so Arabic letters are not joined and
//! right-to-left text is not reordered; colour emoji draw single-colour.
use eframe::egui;
use fontdb::{Database, Family, Query, Stretch, Style, Weight};

/// One character per script or symbol group, the preferred families for it
/// on each OS (tried in order), then any installed face that has it.
const SAMPLES: &[(&str, char, &[&str])] = &[
    (
        "Japanese",
        'あ',
        &[
            "Yu Gothic",
            "Yu Gothic UI",
            "Meiryo",
            "Hiragino Sans",
            "Hiragino Kaku Gothic ProN",
            "Noto Sans CJK JP",
            "Noto Sans JP",
            "Source Han Sans JP",
            "IPAexGothic",
            "MS Gothic",
        ],
    ),
    (
        "Chinese",
        // Simplified-only, so a Japanese font (which has 漢) doesn't count.
        '们',
        &[
            "Microsoft YaHei",
            "Microsoft YaHei UI",
            "PingFang SC",
            "Hiragino Sans GB",
            "Noto Sans CJK SC",
            "Noto Sans SC",
            "Source Han Sans SC",
            "WenQuanYi Micro Hei",
            "SimSun",
        ],
    ),
    (
        "Korean",
        '한',
        &[
            "Malgun Gothic",
            "Apple SD Gothic Neo",
            "Noto Sans CJK KR",
            "Noto Sans KR",
            "Source Han Sans KR",
            "NanumGothic",
        ],
    ),
    (
        "Thai",
        'ก',
        &["Leelawadee UI", "Thonburi", "Noto Sans Thai", "Tahoma"],
    ),
    (
        "Vietnamese",
        'ệ',
        &[
            "Segoe UI",
            "Helvetica Neue",
            "Noto Sans",
            "DejaVu Sans",
            "Arial",
        ],
    ),
    (
        "Arabic",
        'ب',
        &[
            "Segoe UI",
            "Geeza Pro",
            "Noto Sans Arabic",
            "DejaVu Sans",
            "Arial",
        ],
    ),
    (
        "Hebrew",
        'ש',
        &[
            "Segoe UI",
            "Arial Hebrew",
            "Noto Sans Hebrew",
            "DejaVu Sans",
            "Arial",
        ],
    ),
    (
        "Symbols",
        '✧',
        &[
            "Segoe UI Symbol",
            "Apple Symbols",
            "Noto Sans Symbols 2",
            "Noto Sans Symbols",
            "DejaVu Sans",
        ],
    ),
    (
        "Maths letters",
        '𝓈',
        &[
            "Segoe UI Symbol",
            "Cambria Math",
            "STIX Two Math",
            "Noto Sans Math",
            "DejaVu Sans",
        ],
    ),
    (
        "Phonetic",
        'ʕ',
        &["Segoe UI", "Helvetica Neue", "Noto Sans", "DejaVu Sans"],
    ),
    (
        "Yi",
        '꒰',
        &["Microsoft Yi Baiti", "Noto Sans Yi", "Nuosu SIL"],
    ),
    (
        "Emoji",
        '🖤',
        &[
            "Segoe UI Emoji",
            "Noto Emoji",
            "Noto Color Emoji",
            "Twemoji",
        ],
    ),
];

/// Upper bound on fallback font data kept in memory.
const MAX_TOTAL_BYTES: usize = 160 * 1024 * 1024;

/// A system face chosen as a fallback.
pub struct Fallback {
    pub family: String,
    pub path: String,
    pub index: u32,
    /// The sample groups this face was chosen for.
    pub covers: Vec<&'static str>,
    data: Vec<u8>,
}

/// Whether a face has an outline for `c` (colour-bitmap-only fonts such as
/// Apple Color Emoji have glyph IDs but nothing egui can draw).
fn has_outline(data: &[u8], index: u32, c: char) -> bool {
    let Ok(face) = ttf_parser::Face::parse(data, index) else {
        return false;
    };
    let tables = face.tables();
    let outlines = tables.glyf.is_some() || tables.cff.is_some() || tables.cff2.is_some();
    outlines && face.glyph_index(c).is_some()
}

/// Picks fallback faces for the sample characters `missing` reports as
/// absent from the current fonts.
pub fn choose(db: &Database, missing: impl Fn(char) -> bool) -> Vec<Fallback> {
    let mut chosen: Vec<Fallback> = Vec::new();
    let mut total = 0;
    for &(group, sample, preferred) in SAMPLES {
        if !missing(sample) {
            continue;
        }
        // Already covered by a face chosen for an earlier group?
        if let Some(f) = chosen
            .iter_mut()
            .find(|f| has_outline(&f.data, f.index, sample))
        {
            f.covers.push(group);
            continue;
        }
        let covers = |id: fontdb::ID| {
            db.with_face_data(id, |data, index| has_outline(data, index, sample))
                .unwrap_or(false)
        };
        let regular = |name: &str| {
            db.query(&Query {
                families: &[Family::Name(name)],
                weight: Weight::NORMAL,
                stretch: Stretch::Normal,
                style: Style::Normal,
            })
        };
        let by_name = preferred
            .iter()
            .filter_map(|name| regular(name))
            .find(|&id| covers(id));
        // Otherwise any regular, upright face that has it; serif and
        // "Mincho"/"Song" styles last.
        let any = || {
            let mut candidates: Vec<_> = db
                .faces()
                .filter(|f| f.style == Style::Normal && f.weight == Weight::NORMAL)
                .collect();
            candidates.sort_by_key(|f| {
                let name = f
                    .families
                    .first()
                    .map(|(n, _)| n.to_lowercase())
                    .unwrap_or_default();
                ["serif", "mincho", "song", "ming"]
                    .iter()
                    .any(|w| name.contains(w) && !name.contains("sans"))
            });
            candidates.into_iter().map(|f| f.id).find(|&id| covers(id))
        };
        let Some(id) = by_name.or_else(any) else {
            continue;
        };
        let Some(face) = db.face(id) else { continue };
        let Some(data) = db.with_face_data(id, |data, _| data.to_vec()) else {
            continue;
        };
        if total + data.len() > MAX_TOTAL_BYTES {
            continue;
        }
        total += data.len();
        let path = match &face.source {
            fontdb::Source::File(p) | fontdb::Source::SharedFile(p, _) => p.display().to_string(),
            fontdb::Source::Binary(_) => String::new(),
        };
        chosen.push(Fallback {
            family: face
                .families
                .first()
                .map(|(n, _)| n.clone())
                .unwrap_or_default(),
            path,
            index: face.index,
            covers: vec![group],
            data,
        });
    }
    chosen
}

/// A face's vertical metrics in em units: ascent, descent (negative) and
/// line gap, as ab_glyph (egui's rasteriser) reads them.
#[derive(Clone, Copy, Debug)]
struct Metrics {
    ascent: f32,
    descent: f32,
    line_gap: f32,
}

fn metrics(data: &[u8], index: u32) -> Option<Metrics> {
    let face = ttf_parser::Face::parse(data, index).ok()?;
    let em = face.units_per_em() as f32;
    Some(Metrics {
        ascent: face.ascender() as f32 / em,
        descent: face.descender() as f32 / em,
        line_gap: face.line_gap() as f32 / em,
    })
}

/// egui sizes every font so that ascent minus descent fills the font size,
/// and places each fallback's baseline from its own metrics. Fonts with tall
/// metrics (most CJK and Thai system fonts) therefore draw smaller than the
/// main font and sit too high. This tweak gives the fallback the main font's
/// em size and baseline. (Derived from epaint 0.31's `FontImpl::new` and the
/// row layout in `text_layout.rs`.)
fn matching_tweak(main: Metrics, fallback: Metrics) -> egui::FontTweak {
    let main_height = main.ascent - main.descent;
    let height = fallback.ascent - fallback.descent;
    if main_height <= 0.0 || height <= 0.0 {
        return Default::default();
    }
    // Same em size as the main font.
    let scale = (height / main_height).clamp(0.7, 1.6);
    // Baselines, as fractions of the font size, from the top of the row:
    // the main font's, and the fallback's (which egui centres by the
    // difference in line heights).
    let main_baseline = main.ascent / main_height;
    let fallback_baseline = fallback.ascent / height
        + 0.5
            * ((main_height + main.line_gap) / main_height - (height + fallback.line_gap) / height);
    egui::FontTweak {
        scale,
        // egui keeps a scaled glyph centred on its unscaled box; undo that so
        // it stays on the baseline.
        y_offset_factor: (1.0 - scale) * 0.5 * (fallback.ascent + fallback.descent)
            / (height * scale),
        y_offset: 0.0,
        baseline_offset_factor: (main_baseline - fallback_baseline) / scale,
    }
}

/// Font definitions: egui's bundled fonts first, then `fallbacks`.
pub fn definitions(fallbacks: Vec<Fallback>) -> egui::FontDefinitions {
    let mut defs = egui::FontDefinitions::default();
    let main = defs
        .font_data
        .get("Ubuntu-Light")
        .and_then(|d| metrics(&d.font, d.index));
    for (i, f) in fallbacks.into_iter().enumerate() {
        let key = format!("system-fallback-{i}-{}", f.family);
        let tweak = match (main, metrics(&f.data, f.index)) {
            (Some(main), Some(fallback)) => matching_tweak(main, fallback),
            _ => Default::default(),
        };
        let mut data = egui::FontData::from_owned(f.data).tweak(tweak);
        data.index = f.index;
        defs.font_data.insert(key.clone(), data.into());
        for family in [egui::FontFamily::Proportional, egui::FontFamily::Monospace] {
            if let Some(list) = defs.families.get_mut(&family) {
                list.push(key.clone());
            }
        }
    }
    defs
}

/// Whether egui's bundled fonts lack `c`.
fn bundled_lacks(c: char) -> bool {
    use std::sync::OnceLock;
    static CTX: OnceLock<egui::Context> = OnceLock::new();
    let ctx = CTX.get_or_init(|| {
        let ctx = egui::Context::default();
        let _ = ctx.run(Default::default(), |_| {});
        ctx
    });
    !ctx.fonts(|f| f.has_glyph(&egui::FontId::proportional(14.0), c))
}

/// Loads the system database and chooses fallbacks for what the bundled
/// fonts lack.
pub fn find_fallbacks() -> Vec<Fallback> {
    let mut db = Database::new();
    db.load_system_fonts();
    choose(&db, bundled_lacks)
}

/// Starts the background search and installs the fallbacks when done.
pub fn install_system_fallbacks(ctx: &egui::Context) {
    if std::env::var_os("IMVUWU_NO_SYSTEM_FONTS").is_some() {
        return;
    }
    let ctx = ctx.clone();
    std::thread::spawn(move || {
        let fallbacks = find_fallbacks();
        if !fallbacks.is_empty() {
            ctx.set_fonts(definitions(fallbacks));
            ctx.request_repaint();
        }
    });
}

/// `--font-report <file>`: which system fonts would be used, per group.
pub fn report() -> String {
    let start = std::time::Instant::now();
    let mut db = Database::new();
    db.load_system_fonts();
    let scanned = start.elapsed();
    let fallbacks = choose(&db, bundled_lacks);
    let mut out = format!(
        "{} system font faces scanned in {:.0} ms; {} fallback(s) chosen in {:.0} ms total\n",
        db.len(),
        scanned.as_secs_f64() * 1000.0,
        fallbacks.len(),
        start.elapsed().as_secs_f64() * 1000.0
    );
    for &(group, sample, _) in SAMPLES {
        let line = if !bundled_lacks(sample) {
            "bundled font".to_string()
        } else if let Some(f) = fallbacks.iter().find(|f| f.covers.contains(&group)) {
            format!("{} ({}#{})", f.family, f.path, f.index)
        } else {
            "NOT AVAILABLE (no installed font has it)".to_string()
        };
        out.push_str(&format!("{group:<14} {sample}  {line}\n"));
    }
    let main = egui::FontDefinitions::default()
        .font_data
        .get("Ubuntu-Light")
        .and_then(|d| metrics(&d.font, d.index));
    for f in &fallbacks {
        if let (Some(main), Some(m)) = (main, metrics(&f.data, f.index)) {
            let t = matching_tweak(main, m);
            out.push_str(&format!(
                "  {}: ascent {:.3} descent {:.3} gap {:.3} -> scale {:.3}, baseline {:+.3}, y {:+.3}
",
                f.family, m.ascent, m.descent, m.line_gap, t.scale, t.baseline_offset_factor, t.y_offset_factor
            ));
        }
    }
    if let Some(m) = main {
        out.push_str(&format!(
            "  Ubuntu-Light (main): ascent {:.3} descent {:.3} gap {:.3}
",
            m.ascent, m.descent, m.line_gap
        ));
    }
    let bytes: usize = fallbacks.iter().map(|f| f.data.len()).sum();
    out.push_str(&format!(
        "fallback font data: {:.1} MB\n",
        bytes as f64 / 1e6
    ));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Whatever this machine has installed, every chosen face really has an
    /// outline for each group it was chosen for, and nothing is chosen for
    /// characters the bundled fonts already draw.
    #[test]
    fn chosen_fallbacks_cover_their_groups() {
        let mut db = Database::new();
        db.load_system_fonts();
        let chosen = choose(&db, bundled_lacks);
        for f in &chosen {
            for group in &f.covers {
                let (_, sample, _) = SAMPLES.iter().find(|(g, _, _)| g == group).unwrap();
                assert!(bundled_lacks(*sample), "{group} chosen though bundled");
                assert!(
                    has_outline(&f.data, f.index, *sample),
                    "{} lacks {group}",
                    f.family
                );
            }
        }
        let defs = definitions(chosen);
        let proportional = &defs.families[&egui::FontFamily::Proportional];
        // The bundled fonts stay first, so the app's own look is unchanged.
        assert_eq!(proportional[0], "Ubuntu-Light");
    }

    #[test]
    fn a_font_with_the_main_metrics_needs_no_tweak() {
        let m = Metrics {
            ascent: 0.9,
            descent: -0.2,
            line_gap: 0.0,
        };
        let t = matching_tweak(m, m);
        assert!((t.scale - 1.0).abs() < 1e-6);
        assert!(t.y_offset_factor.abs() < 1e-6 && t.baseline_offset_factor.abs() < 1e-6);
        // A taller font is drawn larger to match the main font's em size.
        let tall = Metrics {
            ascent: 1.06,
            descent: -0.26,
            line_gap: 0.0,
        };
        assert!(matching_tweak(m, tall).scale > 1.1);
    }

    #[test]
    fn nothing_is_chosen_when_nothing_is_missing() {
        let mut db = Database::new();
        db.load_system_fonts();
        assert!(choose(&db, |_| false).is_empty());
    }
}
