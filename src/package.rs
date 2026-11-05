//! Product packages opened in the Packages tab: a local ZIP/CHKN archive, a
//! local folder, or a revision downloaded from IMVU. Files keep their
//! original names and bytes; nothing here rewrites or re-encodes them.
//!
//! The ZIP reader is deliberately small and strict: stored and Deflate
//! entries only, sizes and CRC-32 checked, bounded entry counts and sizes,
//! no ZIP64, no encryption, and names validated before anything is saved.

use crate::imvu::is_safe_name;
use std::{collections::BTreeMap, io::Read, path::Path, path::PathBuf, sync::Arc};

const MAX_ARCHIVE: u64 = 512 * 1024 * 1024;
const MAX_ENTRIES: usize = 4096;
const MAX_FILE: u64 = 64 * 1024 * 1024;
const MAX_TOTAL: u64 = 512 * 1024 * 1024;
const MAX_FOLDER_DEPTH: usize = 4;

/// Where a package came from.
#[derive(Clone, Debug, PartialEq)]
pub enum Source {
    Archive(PathBuf),
    Folder(PathBuf),
    Remote { product: u64, revision: u32 },
}

/// What a file's bytes look like, from its leading bytes (not its name).
/// TGA has no signature: it is recognised by a `.tga` name, or, for an
/// otherwise unrecognised binary file, by successfully decoding as TGA (the
/// fallback the reference CHKN downloader uses for misnamed textures).
#[derive(Clone, Debug, PartialEq)]
pub enum Kind {
    Png,
    Jpeg,
    Gif,
    Bmp,
    Tga,
    Dds,
    /// Detected but not decoded: WebP support is not built in.
    Webp,
    Xml,
    /// Binary Cal3D: CMF mesh, CSF skeleton, CRF material, CAF animation,
    /// CPF morph animation, with the version number from the header.
    Cal3d(&'static str, u32),
    Zip,
    Cfl,
    Text,
    Binary,
}

impl Kind {
    pub fn detect(name: &str, bytes: &[u8]) -> Self {
        let starts = |m: &[u8]| bytes.starts_with(m);
        if starts(b"\x89PNG\r\n\x1a\n") {
            return Self::Png;
        }
        if starts(b"\xff\xd8\xff") {
            return Self::Jpeg;
        }
        if starts(b"GIF87a") || starts(b"GIF89a") {
            return Self::Gif;
        }
        // "BM" alone matches lots of non-images (any text starting "BM…"),
        // so the bitmap header behind it must be valid too.
        if starts(b"BM") && bmp_header_ok(bytes) {
            return Self::Bmp;
        }
        if starts(b"DDS ") && bytes.len() >= 128 && bytes[4..8] == 124u32.to_le_bytes() {
            return Self::Dds;
        }
        if bytes.len() >= 12 && starts(b"RIFF") && &bytes[8..12] == b"WEBP" {
            return Self::Webp;
        }
        if starts(b"PK\x03\x04") || starts(b"PK\x05\x06") {
            return Self::Zip;
        }
        if starts(b"CFL3") || starts(b"DFL3") {
            return Self::Cfl;
        }
        for (magic, label) in [
            (b"CMF\0", "CMF mesh"),
            (b"CSF\0", "CSF skeleton"),
            (b"CRF\0", "CRF material"),
            (b"CAF\0", "CAF animation"),
            (b"CPF\0", "CPF morph animation"),
        ] {
            if starts(magic) && bytes.len() >= 8 {
                let version = u32::from_le_bytes(bytes[4..8].try_into().unwrap());
                return Self::Cal3d(label, version);
            }
        }
        // TGA has no leading signature. Accept it when the header is valid
        // and either the TGA 2.0 footer is present or the name says .tga;
        // otherwise (checked below, after text) only if it also decodes.
        if tga_header_ok(bytes)
            && (has_tga_footer(bytes) || name.to_ascii_lowercase().ends_with(".tga"))
        {
            return Self::Tga;
        }
        let text = bytes.strip_prefix(b"\xef\xbb\xbf").unwrap_or(bytes);
        let head = &text[..text.len().min(4096)];
        let printable = head
            .iter()
            .all(|&b| b >= 0x20 || matches!(b, b'\t' | b'\r' | b'\n'));
        // The 4 KiB sample may end mid-character; that alone is not binary.
        let utf8 = match std::str::from_utf8(head) {
            Ok(_) => true,
            Err(e) => e.error_len().is_none(),
        };
        if !head.is_empty() && printable && utf8 {
            if text.trim_ascii_start().starts_with(b"<") {
                return Self::Xml;
            }
            return Self::Text;
        }
        if looks_like_tga(bytes) {
            return Self::Tga;
        }
        Self::Binary
    }

    /// File extensions that match this kind (lower case, no dot).
    pub fn extensions(&self) -> &'static [&'static str] {
        match self {
            Self::Png => &["png"],
            Self::Jpeg => &["jpg", "jpeg", "jpe", "jfif"],
            Self::Gif => &["gif"],
            Self::Bmp => &["bmp", "dib"],
            Self::Tga => &["tga", "targa"],
            Self::Dds => &["dds"],
            Self::Webp => &["webp"],
            _ => &[],
        }
    }

    /// The usual extension for an image kind, for corrected-name copies.
    pub fn preferred_extension(&self) -> Option<&'static str> {
        self.extensions().first().copied()
    }

    pub fn label(&self) -> String {
        match self {
            Self::Png => "PNG image".into(),
            Self::Jpeg => "JPEG image".into(),
            Self::Gif => "GIF image".into(),
            Self::Bmp => "BMP image".into(),
            Self::Tga => "TGA image".into(),
            Self::Dds => "DDS texture".into(),
            Self::Webp => "WebP image".into(),
            Self::Xml => "XML".into(),
            Self::Cal3d(label, version) => format!("{label} v{version}"),
            Self::Zip => "ZIP archive".into(),
            Self::Cfl => "CFL archive".into(),
            Self::Text => "Text".into(),
            Self::Binary => "Binary".into(),
        }
    }

    /// An image format, whether or not this build can decode it.
    pub fn is_image(&self) -> bool {
        !self.extensions().is_empty()
    }

    /// An image this build can decode for previews.
    pub fn can_preview(&self) -> bool {
        self.is_image() && *self != Self::Webp
    }
}

/// BMP file header plus the DIB header behind it: reserved fields zero, a
/// known DIB header size (OS/2 core 12, BITMAPINFOHEADER 40, the 52/56/64
/// variants, V4 108, V5 124) and a pixel-data offset inside the file. The
/// file-size field is not checked: writers often get it wrong.
fn bmp_header_ok(bytes: &[u8]) -> bool {
    if bytes.len() < 26 || !bytes.starts_with(b"BM") {
        return false;
    }
    let u32_at = |at: usize| u32::from_le_bytes(bytes[at..at + 4].try_into().unwrap());
    let reserved_zero = bytes[6..10].iter().all(|&b| b == 0);
    let dib = u32_at(14);
    let offset = u32_at(10) as usize;
    reserved_zero
        && matches!(dib, 12 | 40 | 52 | 56 | 64 | 108 | 124)
        && offset >= 14 + dib as usize
        && offset < bytes.len()
}

/// TGA 2.0 files end with a footer whose last 18 bytes are this signature.
fn has_tga_footer(bytes: &[u8]) -> bool {
    bytes.len() >= 18 + 26 && bytes.ends_with(b"TRUEVISION-XFILE.\0")
}

/// Structural TGA header check, following the Truevision spec and the
/// `imagesize` crate's heuristics: colour-map type 0/1; image type 1, 2, 3
/// (raw) or 9, 10, 11 (RLE); colour-mapped types need a colour map and
/// colour-map-free files need zero colour-map fields; a sane colour-map
/// entry size; pixel depth 8/15/16/24/32 with alpha bits that fit it; the
/// reserved descriptor bit clear; non-zero width and height.
fn tga_header_ok(bytes: &[u8]) -> bool {
    if bytes.len() < 18 {
        return false;
    }
    let colour_map = bytes[1];
    let image_type = bytes[2];
    let map_length = u16::from_le_bytes([bytes[5], bytes[6]]);
    let map_first = u16::from_le_bytes([bytes[3], bytes[4]]);
    let map_entry = bytes[7];
    let width = u16::from_le_bytes([bytes[12], bytes[13]]);
    let height = u16::from_le_bytes([bytes[14], bytes[15]]);
    let depth = bytes[16];
    let descriptor = bytes[17];
    let alpha = descriptor & 0x0f;
    let colour_mapped = matches!(image_type, 1 | 9);
    colour_map <= 1
        && matches!(image_type, 1 | 2 | 3 | 9 | 10 | 11)
        && (!colour_mapped || colour_map == 1)
        && match colour_map {
            0 => map_first == 0 && map_length == 0 && map_entry == 0,
            _ => matches!(map_entry, 15 | 16 | 24 | 32) && map_length > 0,
        }
        && descriptor & 0x10 == 0
        && match depth {
            8 | 15 | 24 => alpha == 0 || (depth == 8 && image_type == 3 && alpha == 8),
            16 => alpha <= 1,
            32 => alpha == 0 || alpha == 8,
            _ => false,
        }
        && width > 0
        && height > 0
}

/// For binary files with no name hint: a valid TGA header and a successful
/// decode. The header check rejects almost everything first, so the decode
/// only runs on plausible candidates.
fn looks_like_tga(bytes: &[u8]) -> bool {
    bytes.len() <= 32 * 1024 * 1024
        && tga_header_ok(bytes)
        && image::load_from_memory_with_format(bytes, image::ImageFormat::Tga).is_ok()
}

/// The extension of a file name, lower case, if it has a short one with at
/// least one letter. Blender-style copy suffixes such as `back [16].003` are
/// part of the name, not an extension.
pub fn extension(name: &str) -> Option<String> {
    let base = name.rsplit('/').next().unwrap_or(name);
    let (stem, ext) = base.rsplit_once('.')?;
    (!stem.is_empty()
        && (1..=5).contains(&ext.len())
        && ext.chars().all(|c| c.is_ascii_alphanumeric())
        && ext.chars().any(|c| c.is_ascii_alphabetic()))
    .then(|| ext.to_ascii_lowercase())
}

/// The name without its extension (as `extension` defines it).
pub fn stem(name: &str) -> &str {
    let base = name.rsplit('/').next().unwrap_or(name);
    match extension(base) {
        Some(ext) => &base[..base.len() - ext.len() - 1],
        None => base,
    }
}

/// Decodes `%XX` escapes. Some packages store names percent-encoded (e.g.
/// `back%20%5B16%5D.xrf` for `back [16].xrf`); the stored name is what
/// `index.xml` refers to, so this is only used to display and to match.
pub fn percent_decode(name: &str) -> String {
    let bytes = name.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).ok();
            if let Some(b) = hex.and_then(|h| u8::from_str_radix(h, 16).ok()) {
                out.push(b);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8(out).unwrap_or_else(|_| name.to_string())
}

#[derive(Clone, Debug)]
pub struct PackageFile {
    /// Original name (relative path with `/` for folder and archive members).
    pub name: String,
    /// The file's original bytes, or why they are not available.
    pub bytes: Result<Arc<Vec<u8>>, String>,
    pub kind: Option<Kind>,
    /// Manifest tags such as `original` / `thumbnail` (remote revisions).
    pub tags: Vec<String>,
    /// Manifest `original_dimensions`, as listed (remote revisions).
    pub listed_dimensions: Option<String>,
    /// The `Content-Type` the server sent (remote revisions).
    pub server_type: Option<String>,
}

impl PackageFile {
    pub fn new(name: String, bytes: Result<Vec<u8>, String>) -> Self {
        let kind = bytes.as_ref().ok().map(|b| Kind::detect(&name, b));
        Self {
            name,
            bytes: bytes.map(Arc::new),
            kind,
            tags: Vec::new(),
            listed_dimensions: None,
            server_type: None,
        }
    }
    pub fn size(&self) -> Option<usize> {
        self.bytes.as_ref().ok().map(|b| b.len())
    }
    /// For an image whose name says another format (e.g. a PNG called
    /// `.jpg`, or a TGA with no extension): the misleading extension, or
    /// `""` when there is none. `None` when the name already matches.
    pub fn extension_mismatch(&self) -> Option<String> {
        let kind = self.kind.as_ref().filter(|k| k.is_image())?;
        match extension(&self.name) {
            Some(ext) if kind.extensions().contains(&ext.as_str()) => None,
            Some(ext) => Some(ext),
            None => Some(String::new()),
        }
    }
    /// The name a corrected-extension copy would use.
    pub fn corrected_name(&self) -> Option<String> {
        self.extension_mismatch()?;
        let ext = self.kind.as_ref()?.preferred_extension()?;
        let base = self.name.rsplit('/').next().unwrap_or(&self.name);
        let stem = match extension(base) {
            Some(old) => &base[..base.len() - old.len() - 1],
            None => base,
        };
        Some(format!("{stem}.{ext}"))
    }
}

/// One texture map listed in a material.
#[derive(Clone, Debug, PartialEq)]
pub struct MaterialMap {
    /// e.g. "Diffuse Color", "Opacity"; empty when the file does not say.
    pub kind: String,
    /// The texture file name exactly as the material stores it.
    pub file: String,
}

/// Texture maps of a Cal3D material: binary CRF (IMVU stores these as
/// `.xrf`) or XML. `None` when the bytes are not a material.
pub fn material_maps(bytes: &[u8]) -> Option<Result<Vec<MaterialMap>, String>> {
    if bytes.starts_with(b"CRF\0") {
        return Some(binary_material_maps(bytes));
    }
    let text = std::str::from_utf8(bytes.strip_prefix(b"\xef\xbb\xbf").unwrap_or(bytes)).ok()?;
    if !text.contains("<MATERIAL") {
        return None;
    }
    // XRF is a HEADER/MATERIAL fragment without one root, like XMF.
    let body = text.trim_start();
    let body = match body.strip_prefix("<?xml") {
        Some(rest) => rest.split_once("?>").map_or("", |(_, r)| r),
        None => body,
    };
    let wrapped = format!("<root>{body}</root>");
    Some(
        xmltree::Element::parse(wrapped.as_bytes())
            .map_err(|e| e.to_string())
            .map(|root| {
                let mut maps = Vec::new();
                fn walk(e: &xmltree::Element, maps: &mut Vec<MaterialMap>) {
                    if e.name.eq_ignore_ascii_case("MAP") {
                        if let Some(file) = e.get_text().map(|t| t.trim().to_string()) {
                            if !file.is_empty() {
                                maps.push(MaterialMap {
                                    kind: e.attributes.get("TYPE").cloned().unwrap_or_default(),
                                    file,
                                });
                            }
                        }
                    }
                    for child in e.children.iter().filter_map(xmltree::XMLNode::as_element) {
                        walk(child, maps);
                    }
                }
                walk(&root, &mut maps);
                maps
            }),
    )
}

/// CRF: magic, version, ambient/diffuse/specular RGBA (4 bytes each),
/// shininess (f32), map count, then per map a length-prefixed file name and,
/// in IMVU's files, a length-prefixed map type. Read strictly: the data must
/// end exactly after the last map, so a layout without types is retried.
fn binary_material_maps(bytes: &[u8]) -> Result<Vec<MaterialMap>, String> {
    fn read(bytes: &[u8], with_types: bool) -> Option<Vec<MaterialMap>> {
        let int = |at: usize| -> Option<i32> {
            Some(i32::from_le_bytes(bytes.get(at..at + 4)?.try_into().ok()?))
        };
        let mut at = 4 + 4 + 12 + 4;
        let count = usize::try_from(int(at)?).ok().filter(|&c| c <= 64)?;
        at += 4;
        let string = |at: &mut usize| -> Option<String> {
            let len = usize::try_from(int(*at)?).ok().filter(|&l| l <= 4096)?;
            let s = bytes.get(*at + 4..*at + 4 + len)?;
            *at += 4 + len;
            Some(
                String::from_utf8_lossy(s)
                    .trim_end_matches('\0')
                    .to_string(),
            )
        };
        let mut maps = Vec::with_capacity(count);
        for _ in 0..count {
            let file = string(&mut at)?;
            let kind = if with_types {
                string(&mut at)?
            } else {
                String::new()
            };
            maps.push(MaterialMap { kind, file });
        }
        (at == bytes.len()).then_some(maps)
    }
    read(bytes, true)
        .or_else(|| read(bytes, false))
        .ok_or_else(|| "damaged or unknown CRF material layout".into())
}

/// Whether a file name referenced by `index.xml` or a material is in the
/// package.
#[derive(Clone, Debug, PartialEq)]
pub enum RefState {
    /// Present under exactly this name (ignoring case).
    Present,
    /// Not here: inherited from the parent product, or genuinely missing.
    NotHere,
    /// Not under this name, but a file with the same name and a different
    /// extension is (e.g. `skin.tga` referenced, `skin.png` present). Usually
    /// a downloader renamed it to match its contents.
    OtherExtension(String),
    /// The reference has no extension and a file with one added is present
    /// (e.g. `tex` referenced, `tex.png` present). Also a downloader rename.
    AddedExtension(String),
}

#[derive(Clone, Debug, PartialEq)]
pub struct Reference {
    /// The file that makes the reference: `index.xml` or a material.
    pub from: String,
    /// Map type for material references, e.g. "Diffuse Color".
    pub kind: String,
    pub target: String,
    pub state: RefState,
}

/// What `index.xml` says about inheritance and assets.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct IndexSummary {
    /// Products named by `<__DATAIMPORT>product://N/index.xml`, in order.
    pub imports: Vec<u64>,
    /// Paths (e.g. `Template/Mesh0`) of elements marked `delete="true"`.
    pub deletions: Vec<String>,
    /// Every `<Asset>` file name referenced, in document order, deduplicated.
    pub assets: Vec<String>,
}

impl IndexSummary {
    pub fn parse(bytes: &[u8]) -> Result<Self, String> {
        let root =
            xmltree::Element::parse(std::io::Cursor::new(bytes)).map_err(|e| e.to_string())?;
        let mut summary = Self::default();
        fn walk(e: &xmltree::Element, path: &str, s: &mut IndexSummary) {
            let path = if path.is_empty() {
                e.name.clone()
            } else {
                format!("{path}/{}", e.name)
            };
            if e.attributes.get("delete").is_some_and(|v| v == "true") {
                s.deletions.push(path.clone());
            }
            let text = e
                .get_text()
                .map(|t| t.trim().to_string())
                .unwrap_or_default();
            if e.name == "__DATAIMPORT" {
                if let Some(id) = text
                    .strip_prefix("product://")
                    .and_then(|r| r.split('/').next())
                    .and_then(|n| n.parse().ok())
                {
                    if !s.imports.contains(&id) {
                        s.imports.push(id);
                    }
                }
            }
            if e.name == "Asset" && !text.is_empty() && !s.assets.contains(&text) {
                s.assets.push(text);
            }
            for child in e.children.iter().filter_map(xmltree::XMLNode::as_element) {
                walk(child, &path, s);
            }
        }
        walk(&root, "", &mut summary);
        Ok(summary)
    }
}

#[derive(Clone, Debug)]
pub struct Package {
    pub source: Source,
    pub title: String,
    pub files: Vec<PackageFile>,
    /// Problems with the package as a whole (partial download, skipped
    /// members…). Per-file problems live in `PackageFile::bytes`.
    pub notes: Vec<String>,
}

impl Package {
    pub fn index(&self) -> Option<Result<IndexSummary, String>> {
        let file = self
            .files
            .iter()
            .find(|f| f.name.eq_ignore_ascii_case("index.xml"))?;
        Some(match &file.bytes {
            Ok(bytes) => IndexSummary::parse(bytes),
            Err(e) => Err(e.clone()),
        })
    }

    pub fn is_complete(&self) -> bool {
        self.notes.is_empty() && self.files.iter().all(|f| f.bytes.is_ok())
    }

    #[cfg(test)]
    pub fn has_file(&self, name: &str) -> bool {
        self.find(name).is_some()
    }

    /// The file a name refers to (ignoring case and `%XX` escapes).
    pub fn file(&self, name: &str) -> Option<&PackageFile> {
        self.find(name)
    }

    /// The file a reference resolves to: exact name ignoring case, or the
    /// same name once `%XX` escapes are decoded on either side.
    fn find(&self, name: &str) -> Option<&PackageFile> {
        self.files
            .iter()
            .find(|f| f.name.eq_ignore_ascii_case(name))
            .or_else(|| {
                let wanted = percent_decode(name);
                self.files
                    .iter()
                    .find(|f| percent_decode(&f.name).eq_ignore_ascii_case(&wanted))
            })
    }

    /// Whether `target` (a name some file refers to) is in this package.
    pub fn state_of(&self, target: &str) -> RefState {
        if self.find(target).is_some() {
            return RefState::Present;
        }
        let wanted = percent_decode(target).to_lowercase();
        let has_extension = extension(&wanted).is_some();
        let wanted_stem = stem(&wanted).to_string();
        for file in &self.files {
            let name = percent_decode(&file.name).to_lowercase();
            if has_extension && extension(&name).is_some() && stem(&name) == wanted_stem {
                return RefState::OtherExtension(file.name.clone());
            }
            if !has_extension && stem(&name) == wanted && name != wanted {
                return RefState::AddedExtension(file.name.clone());
            }
        }
        RefState::NotHere
    }

    /// Every file name `index.xml` (`<Asset>`) and the materials (texture
    /// maps) refer to, and whether this package has it.
    pub fn references(&self) -> Vec<Reference> {
        let mut out = Vec::new();
        if let Some(Ok(index)) = self.index() {
            for asset in index.assets {
                out.push(Reference {
                    from: "index.xml".into(),
                    kind: String::new(),
                    state: self.state_of(&asset),
                    target: asset,
                });
            }
        }
        for file in &self.files {
            let Ok(bytes) = &file.bytes else { continue };
            let Some(Ok(maps)) = material_maps(bytes) else {
                continue;
            };
            for map in maps {
                out.push(Reference {
                    from: file.name.clone(),
                    kind: map.kind,
                    state: self.state_of(&map.file),
                    target: map.file,
                });
            }
        }
        out
    }

    /// Image files no reference points to. If the package was renamed by a
    /// downloader, their original names cannot be recovered from it (e.g.
    /// the base female avatar's unreferenced `eyelashes.tga`).
    pub fn unreferenced_images(&self) -> Vec<String> {
        let references = self.references();
        self.files
            .iter()
            .filter(|f| f.kind.as_ref().is_some_and(Kind::is_image))
            .filter(|f| {
                !references.iter().any(|r| match &r.state {
                    RefState::Present => self
                        .find(&r.target)
                        .is_some_and(|found| found.name == f.name),
                    RefState::OtherExtension(c) | RefState::AddedExtension(c) => *c == f.name,
                    RefState::NotHere => false,
                })
            })
            .map(|f| f.name.clone())
            .collect()
    }

    /// A copy where files that were evidently renamed (by extension only)
    /// get back the exact names `index.xml` and the materials use, so the
    /// package is consistent again. Bytes are unchanged. Returns the copy
    /// and the (current name, restored name) pairs. A file is only renamed
    /// when every reference to it agrees on one name.
    pub fn with_referenced_names(&self) -> (Package, Vec<(String, String)>) {
        let mut wanted: BTreeMap<String, std::collections::BTreeSet<String>> = BTreeMap::new();
        for r in self.references() {
            if let RefState::OtherExtension(current) | RefState::AddedExtension(current) = &r.state
            {
                wanted
                    .entry(current.clone())
                    .or_default()
                    .insert(r.target.clone());
            }
        }
        let mut copy = self.clone();
        let mut renamed = Vec::new();
        for file in &mut copy.files {
            let Some(targets) = wanted.get(&file.name) else {
                continue;
            };
            let [target] = targets.iter().collect::<Vec<_>>()[..] else {
                continue;
            };
            let taken = self
                .files
                .iter()
                .any(|f| f.name.eq_ignore_ascii_case(target));
            if !taken && is_safe_relative(target) {
                renamed.push((file.name.clone(), target.clone()));
                file.name = target.clone();
            }
        }
        if !renamed.is_empty() {
            copy.title = format!("{} (original names restored)", self.title);
        }
        (copy, renamed)
    }

    /// A short safe base name for saving, e.g. `69190922_rev1`.
    pub fn save_name(&self) -> String {
        match &self.source {
            Source::Remote { product, revision } => format!("{product}_rev{revision}"),
            Source::Archive(p) | Source::Folder(p) => p
                .file_stem()
                .and_then(|s| s.to_str())
                .filter(|s| is_safe_name(s))
                .unwrap_or("package")
                .to_string(),
        }
    }
}

/// `true` for `a/b/c.png`-style names whose every component is safe.
pub fn is_safe_relative(name: &str) -> bool {
    !name.is_empty() && name.split('/').all(is_safe_name)
}

// ---------------------------------------------------------------- reading

fn le16(b: &[u8], at: usize) -> Result<u16, String> {
    b.get(at..at + 2)
        .map(|s| u16::from_le_bytes([s[0], s[1]]))
        .ok_or_else(|| "truncated ZIP".to_string())
}
fn le32(b: &[u8], at: usize) -> Result<u32, String> {
    b.get(at..at + 4)
        .map(|s| u32::from_le_bytes([s[0], s[1], s[2], s[3]]))
        .ok_or_else(|| "truncated ZIP".to_string())
}

/// Reads every member of a ZIP (or `.chkn`, which is a ZIP). Structural
/// problems fail the whole archive; a bad member keeps its name with an
/// error instead of bytes.
pub fn read_zip(bytes: &[u8]) -> Result<(Vec<PackageFile>, Vec<String>), String> {
    // End of central directory: last 22 bytes plus up to 64 KiB of comment.
    let search_start = bytes.len().saturating_sub(22 + 65535);
    let eocd = (search_start..bytes.len().saturating_sub(21))
        .rev()
        .find(|&i| bytes[i..].starts_with(b"PK\x05\x06"))
        .ok_or("not a ZIP archive (no end-of-directory record)")?;
    if le16(bytes, eocd + 4)? != 0 || le16(bytes, eocd + 6)? != 0 {
        return Err("multi-part ZIP archives are not supported".into());
    }
    let count = le16(bytes, eocd + 10)? as usize;
    let directory_size = le32(bytes, eocd + 12)? as usize;
    let directory_offset = le32(bytes, eocd + 16)? as usize;
    if count == 0xffff || directory_offset == 0xffff_ffff {
        return Err("ZIP64 archives are not supported".into());
    }
    if count > MAX_ENTRIES {
        return Err(format!(
            "{count} entries is more than the {MAX_ENTRIES} allowed"
        ));
    }
    if directory_offset
        .checked_add(directory_size)
        .is_none_or(|end| end > eocd)
    {
        return Err("ZIP directory points outside the file".into());
    }
    let mut files = Vec::new();
    let mut notes = Vec::new();
    let mut seen = std::collections::BTreeSet::new();
    let mut total = 0u64;
    let mut at = directory_offset;
    for _ in 0..count {
        if le32(bytes, at)? != 0x0201_4b50 {
            return Err("damaged ZIP directory".into());
        }
        let flags = le16(bytes, at + 8)?;
        let method = le16(bytes, at + 10)?;
        let crc = le32(bytes, at + 16)?;
        let compressed = le32(bytes, at + 20)? as u64;
        let size = le32(bytes, at + 24)? as u64;
        let name_len = le16(bytes, at + 28)? as usize;
        let extra_len = le16(bytes, at + 30)? as usize;
        let comment_len = le16(bytes, at + 32)? as usize;
        let local = le32(bytes, at + 42)? as usize;
        let raw_name = bytes
            .get(at + 46..at + 46 + name_len)
            .ok_or("truncated ZIP directory")?;
        at += 46 + name_len + extra_len + comment_len;
        // Bit 11 marks UTF-8; older tools used code page 437, so keep only
        // names that are valid UTF-8 either way.
        let Ok(name) = std::str::from_utf8(raw_name).map(|n| n.replace('\\', "/")) else {
            notes.push("Skipped a member whose name is not UTF-8".into());
            continue;
        };
        if name.ends_with('/') {
            continue; // directory entry
        }
        if !is_safe_relative(&name) {
            notes.push(format!("Skipped unsafe member name {name:?}"));
            continue;
        }
        if !seen.insert(name.to_lowercase()) {
            notes.push(format!("Skipped duplicate member {name:?}"));
            continue;
        }
        let result = (|| -> Result<Vec<u8>, String> {
            if flags & 1 != 0 {
                return Err("encrypted member".into());
            }
            if size > MAX_FILE {
                return Err(format!("{size} bytes is over the {MAX_FILE}-byte limit"));
            }
            total += size;
            if total > MAX_TOTAL {
                return Err("archive total is over the size limit".into());
            }
            if le32(bytes, local)? != 0x0403_4b50 {
                return Err("damaged local header".into());
            }
            let start =
                local + 30 + le16(bytes, local + 26)? as usize + le16(bytes, local + 28)? as usize;
            let data = bytes
                .get(start..start.saturating_add(compressed as usize))
                .ok_or("member data is truncated")?;
            let out = match method {
                0 => data.to_vec(),
                8 => {
                    let mut out = Vec::with_capacity(size as usize);
                    flate2::read::DeflateDecoder::new(data)
                        .take(size + 1)
                        .read_to_end(&mut out)
                        .map_err(|e| format!("Deflate error: {e}"))?;
                    out
                }
                m => return Err(format!("unsupported compression method {m}")),
            };
            if out.len() as u64 != size {
                return Err("size does not match the directory".into());
            }
            let mut check = flate2::Crc::new();
            check.update(&out);
            if check.sum() != crc {
                return Err("CRC-32 mismatch (damaged data)".into());
            }
            Ok(out)
        })();
        files.push(PackageFile::new(name, result));
    }
    Ok((files, notes))
}

pub fn open_archive(path: &Path) -> Result<Package, String> {
    let size = std::fs::metadata(path).map_err(|e| e.to_string())?.len();
    if size > MAX_ARCHIVE {
        return Err("archive is larger than 512 MB".into());
    }
    let bytes = std::fs::read(path).map_err(|e| e.to_string())?;
    let (files, notes) = read_zip(&bytes)?;
    Ok(Package {
        source: Source::Archive(path.to_path_buf()),
        title: path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default(),
        files,
        notes,
    })
}

/// Reads regular files up to a few folders deep. Symbolic links and other
/// reparse points are skipped rather than followed.
pub fn open_folder(path: &Path) -> Result<Package, String> {
    let mut files = Vec::new();
    let mut notes = Vec::new();
    let mut total = 0u64;
    fn walk(
        root: &Path,
        dir: &Path,
        depth: usize,
        files: &mut Vec<PackageFile>,
        notes: &mut Vec<String>,
        total: &mut u64,
    ) -> Result<(), String> {
        let mut entries: Vec<_> = std::fs::read_dir(dir)
            .map_err(|e| e.to_string())?
            .filter_map(Result::ok)
            .collect();
        entries.sort_by_key(|e| e.file_name());
        for entry in entries {
            let Ok(kind) = entry.file_type() else {
                continue;
            };
            let path = entry.path();
            let Some(name) = path
                .strip_prefix(root)
                .ok()
                .and_then(|p| p.to_str())
                .map(|p| p.replace('\\', "/"))
            else {
                continue;
            };
            if kind.is_symlink() {
                notes.push(format!("Skipped link {name}"));
            } else if kind.is_dir() {
                if depth < MAX_FOLDER_DEPTH {
                    walk(root, &path, depth + 1, files, notes, total)?;
                } else {
                    notes.push(format!("Skipped deep folder {name}"));
                }
            } else if kind.is_file() {
                if files.len() >= MAX_ENTRIES {
                    notes.push("Stopped: too many files".into());
                    return Ok(());
                }
                let size = entry.metadata().map(|m| m.len()).unwrap_or(0);
                let bytes = if size > MAX_FILE {
                    Err(format!("{size} bytes is over the size limit"))
                } else if *total + size > MAX_TOTAL {
                    Err("folder total is over the size limit".into())
                } else {
                    *total += size;
                    std::fs::read(&path).map_err(|e| e.to_string())
                };
                files.push(PackageFile::new(name, bytes));
            }
        }
        Ok(())
    }
    walk(path, path, 0, &mut files, &mut notes, &mut total)?;
    Ok(Package {
        source: Source::Folder(path.to_path_buf()),
        title: path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| path.display().to_string()),
        files,
        notes,
    })
}

/// Builds a package from a downloaded revision.
pub fn from_download(
    product: u64,
    revision: u32,
    downloaded: Vec<crate::imvu::DownloadedFile>,
) -> Package {
    let mut notes = Vec::new();
    let failed = downloaded.iter().filter(|d| d.result.is_err()).count();
    if failed > 0 {
        notes.push(format!(
            "{failed} of {} files could not be downloaded; this copy is partial",
            downloaded.len()
        ));
    }
    let files = downloaded
        .into_iter()
        .map(|d| {
            let mut file = PackageFile::new(d.entry.name, d.result.map_err(|e| e.to_string()));
            file.tags = d.entry.tags;
            file.listed_dimensions = d.entry.original_dimensions;
            file.server_type = d.content_type;
            file
        })
        .collect();
    Package {
        source: Source::Remote { product, revision },
        title: format!("{product} revision {revision}"),
        files,
        notes,
    }
}

// ----------------------------------------------------------------- saving

/// Writes `bytes` to a new file, refusing to overwrite. The data goes to a
/// temporary name first and is renamed into place once complete.
pub fn write_new_file(path: &Path, bytes: &[u8]) -> Result<(), String> {
    if path.exists() {
        return Err(format!("{} already exists", path.display()));
    }
    let temp = write_temp(path, bytes)?;
    std::fs::rename(&temp, path).map_err(|e| {
        let _ = std::fs::remove_file(&temp);
        e.to_string()
    })
}

/// Writes `bytes` to a new `<name>.partial` file next to `path`. The
/// temporary file is created exclusively, so an unrelated existing file with
/// that name is never overwritten.
fn write_temp(path: &Path, bytes: &[u8]) -> Result<PathBuf, String> {
    use std::io::Write;
    let temp = path.with_file_name(format!(
        "{}.partial",
        path.file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("download")
    ));
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temp)
        .map_err(|e| format!("{}: {e}", temp.display()))?;
    if let Err(e) = file.write_all(bytes).and_then(|()| file.sync_all()) {
        drop(file);
        let _ = std::fs::remove_file(&temp);
        return Err(e.to_string());
    }
    Ok(temp)
}

/// Writes `bytes` to `path`, replacing it only after the user confirmed that
/// in the save dialog. Written to a temporary name first, so a failure never
/// leaves a half-written file under the chosen name.
pub fn write_file(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let temp = write_temp(path, bytes)?;
    // On Windows, rename replaces an existing file in one step, so the old
    // file is never deleted before the new one is in place.
    std::fs::rename(&temp, path).map_err(|e| {
        let _ = std::fs::remove_file(&temp);
        e.to_string()
    })
}

/// Saves every available file into a new folder `parent/name`, with original
/// names. Returns the folder and the names that could not be saved.
pub fn extract_all(package: &Package, parent: &Path) -> Result<(PathBuf, Vec<String>), String> {
    let folder = parent.join(package.save_name());
    if folder.exists() {
        return Err(format!(
            "{} already exists; choose another location",
            folder.display()
        ));
    }
    std::fs::create_dir(&folder).map_err(|e| e.to_string())?;
    let mut skipped = Vec::new();
    for file in &package.files {
        let Ok(bytes) = &file.bytes else {
            skipped.push(file.name.clone());
            continue;
        };
        if !is_safe_relative(&file.name) {
            skipped.push(file.name.clone());
            continue;
        }
        let target = file
            .name
            .split('/')
            .fold(folder.clone(), |p, part| p.join(part));
        if let Some(dir) = target.parent() {
            if std::fs::create_dir_all(dir).is_err() {
                skipped.push(file.name.clone());
                continue;
            }
        }
        if write_new_file(&target, bytes).is_err() {
            skipped.push(file.name.clone());
        }
    }
    Ok((folder, skipped))
}

/// A derived copy for use outside IMVU: every image file, unchanged bytes,
/// in a new folder `parent/{name}_images`. An image whose name says another
/// format (a PNG called `.jpg`, a TGA with no extension) is saved with the
/// extension that matches its contents, and `renamed.txt` lists each change.
/// This is not a working product package: `index.xml` still uses the
/// original names, so use the normal saves for that.
pub fn save_images_with_corrected_names(
    package: &Package,
    parent: &Path,
) -> Result<ImageCopy, String> {
    let folder = parent.join(format!("{}_images", package.save_name()));
    if folder.exists() {
        return Err(format!(
            "{} already exists; choose another location",
            folder.display()
        ));
    }
    let images: Vec<&PackageFile> = package
        .files
        .iter()
        .filter(|f| f.bytes.is_ok() && f.kind.as_ref().is_some_and(Kind::is_image))
        .collect();
    if images.is_empty() {
        return Err("this package has no image files".into());
    }
    std::fs::create_dir(&folder).map_err(|e| e.to_string())?;
    let mut renamed = Vec::new();
    let mut used = std::collections::BTreeSet::new();
    let mut saved = 0;
    for file in images {
        let Ok(bytes) = &file.bytes else { continue };
        let original = file
            .name
            .rsplit('/')
            .next()
            .unwrap_or(&file.name)
            .to_string();
        let mut name = file.corrected_name().unwrap_or_else(|| original.clone());
        // Two files can map to the same corrected name (a.jpg + a.png).
        if !used.insert(name.to_lowercase()) {
            let (stem, ext) = name.rsplit_once('.').unwrap_or((&name, ""));
            let (stem, ext) = (stem.to_string(), ext.to_string());
            name = (2..)
                .map(|n| format!("{stem}_{n}.{ext}"))
                .find(|candidate| used.insert(candidate.to_lowercase()))
                .unwrap();
        }
        if !is_safe_name(&name) || write_new_file(&folder.join(&name), bytes).is_err() {
            continue;
        }
        saved += 1;
        if name != original {
            renamed.push((file.name.clone(), name));
        }
    }
    let mut list = String::from(
        "Images saved with extensions matching their contents. These names are\n\
         for use outside IMVU; the product's index.xml uses the original names.\n\n",
    );
    for (old, new) in &renamed {
        list.push_str(&format!("{old}\t->\t{new}\n"));
    }
    if renamed.is_empty() {
        list.push_str("(No names needed changing.)\n");
    }
    let _ = write_new_file(&folder.join("renamed.txt"), list.as_bytes());
    Ok(ImageCopy {
        folder,
        saved,
        renamed,
    })
}

/// Result of `save_images_with_corrected_names`.
pub struct ImageCopy {
    pub folder: PathBuf,
    pub saved: usize,
    /// (original name, saved name) for each file that was renamed.
    pub renamed: Vec<(String, String)>,
}

/// Builds a ZIP (stored entries, original names and bytes) of every
/// available file. CHKN files are ZIPs with a `.chkn` extension.
pub fn zip_bytes(package: &Package) -> Vec<u8> {
    let mut out = Vec::new();
    let mut directory = Vec::new();
    let mut count = 0u16;
    for file in &package.files {
        let Ok(bytes) = &file.bytes else { continue };
        let name = file.name.as_bytes();
        let mut crc = flate2::Crc::new();
        crc.update(bytes);
        let offset = out.len() as u32;
        let header = |sig: u32, central: bool| {
            let mut h = Vec::new();
            h.extend(sig.to_le_bytes());
            if central {
                h.extend(20u16.to_le_bytes()); // made by
            }
            h.extend(20u16.to_le_bytes()); // version needed
            h.extend(0x0800u16.to_le_bytes()); // UTF-8 names
            h.extend(0u16.to_le_bytes()); // stored
            h.extend(0u32.to_le_bytes()); // DOS time/date
            h.extend(crc.sum().to_le_bytes());
            h.extend((bytes.len() as u32).to_le_bytes());
            h.extend((bytes.len() as u32).to_le_bytes());
            h.extend((name.len() as u16).to_le_bytes());
            h.extend(0u16.to_le_bytes()); // extra
            if central {
                h.extend(0u16.to_le_bytes()); // comment
                h.extend(0u16.to_le_bytes()); // disk
                h.extend(0u16.to_le_bytes()); // internal attributes
                h.extend(0u32.to_le_bytes()); // external attributes
                h.extend(offset.to_le_bytes());
            }
            h.extend(name);
            h
        };
        out.extend(header(0x0403_4b50, false));
        out.extend(bytes.iter());
        directory.extend(header(0x0201_4b50, true));
        count += 1;
    }
    let directory_offset = out.len() as u32;
    out.extend(&directory);
    out.extend(0x0605_4b50u32.to_le_bytes());
    out.extend([0u8; 4]);
    out.extend(count.to_le_bytes());
    out.extend(count.to_le_bytes());
    out.extend((directory.len() as u32).to_le_bytes());
    out.extend(directory_offset.to_le_bytes());
    out.extend(0u16.to_le_bytes());
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn deflate_zip(name: &str, data: &[u8]) -> Vec<u8> {
        use std::io::Write;
        let mut encoder =
            flate2::write::DeflateEncoder::new(Vec::new(), flate2::Compression::default());
        encoder.write_all(data).unwrap();
        let compressed = encoder.finish().unwrap();
        let mut crc = flate2::Crc::new();
        crc.update(data);
        let mut zip = Vec::new();
        let local = |zip: &mut Vec<u8>| {
            zip.extend(0x0403_4b50u32.to_le_bytes());
            zip.extend([20, 0, 0, 0, 8, 0, 0, 0, 0, 0]);
            zip.extend(crc.sum().to_le_bytes());
            zip.extend((compressed.len() as u32).to_le_bytes());
            zip.extend((data.len() as u32).to_le_bytes());
            zip.extend((name.len() as u16).to_le_bytes());
            zip.extend(0u16.to_le_bytes());
            zip.extend(name.as_bytes());
        };
        local(&mut zip);
        zip.extend(&compressed);
        let dir_at = zip.len() as u32;
        zip.extend(0x0201_4b50u32.to_le_bytes());
        zip.extend([20, 0, 20, 0, 0, 0, 8, 0, 0, 0, 0, 0]);
        zip.extend(crc.sum().to_le_bytes());
        zip.extend((compressed.len() as u32).to_le_bytes());
        zip.extend((data.len() as u32).to_le_bytes());
        zip.extend((name.len() as u16).to_le_bytes());
        zip.extend([0u8; 12]);
        zip.extend(0u32.to_le_bytes());
        zip.extend(name.as_bytes());
        let dir_len = zip.len() as u32 - dir_at;
        zip.extend(0x0605_4b50u32.to_le_bytes());
        zip.extend([0, 0, 0, 0, 1, 0, 1, 0]);
        zip.extend(dir_len.to_le_bytes());
        zip.extend(dir_at.to_le_bytes());
        zip.extend(0u16.to_le_bytes());
        zip
    }

    fn package(files: Vec<PackageFile>) -> Package {
        Package {
            source: Source::Remote {
                product: 1,
                revision: 2,
            },
            title: String::new(),
            files,
            notes: vec![],
        }
    }

    #[test]
    fn stored_zip_round_trips_exact_bytes_and_names() {
        let png = b"\x89PNG\r\n\x1a\nrest-of-png".to_vec();
        let original = package(vec![
            PackageFile::new("particle.png".into(), Ok(png.clone())),
            PackageFile::new("index.xml".into(), Ok(b"<Template/>".to_vec())),
        ]);
        let (files, notes) = read_zip(&zip_bytes(&original)).unwrap();
        assert!(notes.is_empty());
        assert_eq!(files[0].name, "particle.png");
        assert_eq!(files[0].bytes.as_ref().unwrap().as_slice(), png.as_slice());
        assert_eq!(files[0].kind, Some(Kind::Png));
        assert_eq!(files[1].kind, Some(Kind::Xml));
    }

    #[test]
    fn deflate_members_are_inflated_and_crc_checked() {
        let data = b"hello hello hello hello".repeat(50);
        let zip = deflate_zip("a.txt", &data);
        let (files, _) = read_zip(&zip).unwrap();
        assert_eq!(files[0].bytes.as_ref().unwrap().as_slice(), data.as_slice());
        // Corrupt the stored CRC in the central directory.
        let mut bad = zip.clone();
        let dir = bad.len() - 22 - 46 - 5;
        bad[dir + 16] ^= 0xff;
        let (files, _) = read_zip(&bad).unwrap();
        assert!(files[0].bytes.as_ref().unwrap_err().contains("CRC"));
    }

    #[test]
    fn unsafe_or_duplicate_member_names_are_skipped() {
        for name in ["../evil.png", "C:/x.png", "a/../b.png", "a//b.png"] {
            let (files, notes) = read_zip(&deflate_zip(name, b"x")).unwrap();
            assert!(files.is_empty(), "{name}");
            assert_eq!(notes.len(), 1, "{name}");
        }
        let (files, _) = read_zip(&deflate_zip("textures/ok.png", b"x")).unwrap();
        assert_eq!(files[0].name, "textures/ok.png");
        assert!(read_zip(b"not a zip").is_err());
    }

    #[test]
    fn index_summary_reads_imports_deletions_and_assets() {
        // Shapes from the supplied 69190922 and 29030566 revisions.
        let particle = br#"<?xml version="1.0" encoding="UTF-8"?>
<Template>
<__DATAIMPORT>product://44379985/index.xml</__DATAIMPORT><BodyPartId1 delete="true"></BodyPartId1>
<Mesh0 delete="true"></Mesh0>
</Template>"#;
        let s = IndexSummary::parse(particle).unwrap();
        assert_eq!(s.imports, vec![44379985]);
        assert_eq!(s.deletions, vec!["Template/BodyPartId1", "Template/Mesh0"]);
        assert!(s.assets.is_empty());
        let earrings = br#"<Template><__DATAIMPORT>product://15498252/index.xml</__DATAIMPORT>
<Material><TextureMap1><Asset>female01.anime01.head01.xrf</Asset></TextureMap1></Material>
<Mesh0><Asset>Blaise3.xmf</Asset></Mesh0><Skeleton><Asset>Blaise1.XSF</Asset></Skeleton></Template>"#;
        let s = IndexSummary::parse(earrings).unwrap();
        assert_eq!(s.imports, vec![15498252]);
        assert_eq!(s.assets.len(), 3);
    }

    fn tiny_tga() -> Vec<u8> {
        // 2x1 uncompressed 24-bit true-colour TGA.
        let mut tga = vec![0, 0, 2, 0, 0, 0, 0, 0, 0, 0, 0, 0, 2, 0, 1, 0, 24, 0];
        tga.extend([0, 0, 255, 0, 255, 0]);
        tga
    }

    #[test]
    fn misnamed_images_are_detected_and_flagged() {
        let png = PackageFile::new("skin.jpg".into(), Ok(b"\x89PNG\r\n\x1a\nxx".to_vec()));
        assert_eq!(png.kind, Some(Kind::Png));
        assert_eq!(png.extension_mismatch().as_deref(), Some("jpg"));
        assert_eq!(png.corrected_name().as_deref(), Some("skin.png"));
        let ok = PackageFile::new("skin.JPEG".into(), Ok(b"\xff\xd8\xff\xe0".to_vec()));
        assert_eq!(ok.extension_mismatch(), None);
        // A TGA with a misleading name is found by decoding it.
        let tga = PackageFile::new("opacity.xrf".into(), Ok(tiny_tga()));
        assert_eq!(tga.kind, Some(Kind::Tga));
        assert_eq!(tga.corrected_name().as_deref(), Some("opacity.tga"));
        let bare = PackageFile::new("texture".into(), Ok(tiny_tga()));
        assert_eq!(bare.extension_mismatch().as_deref(), Some(""));
        assert_eq!(bare.corrected_name().as_deref(), Some("texture.tga"));
        // Non-images are never "corrected".
        let mesh = PackageFile::new("hair.png".into(), Ok(b"CMF\0\x97\x03\0\0".to_vec()));
        assert_eq!(mesh.extension_mismatch(), None);
        assert_eq!(Kind::detect("x.bin", b"RIFF\0\0\0\0WEBPVP8 "), Kind::Webp);
        let mut dds = b"DDS \x7c\0\0\0".to_vec();
        dds.resize(128, 0);
        assert_eq!(Kind::detect("x", &dds), Kind::Dds);
    }

    #[test]
    fn corrected_image_copy_renames_only_mismatches_and_lists_them() {
        let parent = std::env::temp_dir().join(format!("hm-images-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&parent);
        std::fs::create_dir_all(&parent).unwrap();
        let png = b"\x89PNG\r\n\x1a\nxx".to_vec();
        let p = package(vec![
            PackageFile::new("skin.jpg".into(), Ok(png.clone())),
            PackageFile::new("eyes.png".into(), Ok(png.clone())),
            PackageFile::new("index.xml".into(), Ok(b"<Template/>".to_vec())),
        ]);
        let ImageCopy {
            folder,
            saved,
            renamed,
        } = save_images_with_corrected_names(&p, &parent).unwrap();
        assert_eq!(saved, 2);
        assert_eq!(
            renamed,
            vec![("skin.jpg".to_string(), "skin.png".to_string())]
        );
        assert_eq!(std::fs::read(folder.join("skin.png")).unwrap(), png);
        assert!(folder.join("eyes.png").exists());
        assert!(!folder.join("index.xml").exists());
        assert!(std::fs::read_to_string(folder.join("renamed.txt"))
            .unwrap()
            .contains("skin.jpg\t->\tskin.png"));
        std::fs::remove_dir_all(&parent).unwrap();
    }

    fn crf(maps: &[(&str, &str)]) -> Vec<u8> {
        let mut b = b"CRF\0".to_vec();
        b.extend(919u32.to_le_bytes());
        b.extend([255u8; 12]);
        b.extend(0f32.to_le_bytes());
        b.extend((maps.len() as i32).to_le_bytes());
        for (file, kind) in maps {
            for s in [file, kind] {
                let mut bytes = s.as_bytes().to_vec();
                bytes.push(0);
                b.extend((bytes.len() as i32).to_le_bytes());
                b.extend(bytes);
            }
        }
        b
    }

    #[test]
    fn numeric_suffixes_are_not_extensions_and_names_decode() {
        assert_eq!(extension("back [16].003"), None);
        assert_eq!(extension("back [16].003.xrf").as_deref(), Some("xrf"));
        assert_eq!(
            stem("COLOR-CCCCCC-back [16].003-DiffuseColor.png"),
            "COLOR-CCCCCC-back [16].003-DiffuseColor"
        );
        assert_eq!(
            percent_decode("back%20%5B16%5D.003.xrf"),
            "back [16].003.xrf"
        );
        assert_eq!(percent_decode("100%"), "100%");
        let tex = PackageFile::new("back [16].003".into(), Ok(b"\x89PNG\r\n\x1a\nxx".to_vec()));
        assert_eq!(tex.corrected_name().as_deref(), Some("back [16].003.png"));
    }

    #[test]
    fn material_references_find_renamed_textures() {
        // Shapes from the user's 53924901 rev 1 (material names .tga, ZIP
        // has .png) and 57809610 rev 4 (material names without extension,
        // ZIP adds .png; encoded material names used by index.xml).
        let png = b"\x89PNG\r\n\x1a\nxx".to_vec();
        let p = package(vec![
            PackageFile::new(
                "female01.anime01.head01.xrf".into(),
                Ok(crf(&[("Female01_head.tga", "Diffuse Color")])),
            ),
            PackageFile::new("Female01_head.png".into(), Ok(png.clone())),
            PackageFile::new(
                "back%20%5B16%5D.xrf".into(),
                Ok(crf(&[(
                    "COLOR-CCCCCC-back [16]-DiffuseColor",
                    "Diffuse Color",
                )])),
            ),
            PackageFile::new("COLOR-CCCCCC-back [16]-DiffuseColor.png".into(), Ok(png)),
            PackageFile::new(
                "index.xml".into(),
                Ok(
                    b"<Template><Material><TextureMap1><Asset>back%20%5B16%5D.xrf</Asset>\
                     </TextureMap1></Material><Mesh0><Asset>parent.xmf</Asset></Mesh0></Template>"
                        .to_vec(),
                ),
            ),
        ]);
        let refs = p.references();
        let state = |target: &str| {
            refs.iter()
                .find(|r| r.target == target)
                .unwrap()
                .state
                .clone()
        };
        assert_eq!(state("back%20%5B16%5D.xrf"), RefState::Present);
        assert_eq!(state("parent.xmf"), RefState::NotHere);
        assert_eq!(
            state("Female01_head.tga"),
            RefState::OtherExtension("Female01_head.png".into())
        );
        assert_eq!(
            state("COLOR-CCCCCC-back [16]-DiffuseColor"),
            RefState::AddedExtension("COLOR-CCCCCC-back [16]-DiffuseColor.png".into())
        );
        let (restored, renamed) = p.with_referenced_names();
        assert_eq!(renamed.len(), 2);
        assert!(p.unreferenced_images().is_empty());
        let mut with_orphan = p.clone();
        with_orphan.files.push(PackageFile::new(
            "eyelashes.jpg".into(),
            Ok(b"\xff\xd8\xff\xe0".to_vec()),
        ));
        assert_eq!(
            with_orphan.unreferenced_images(),
            vec!["eyelashes.jpg".to_string()]
        );
        assert!(restored.has_file("Female01_head.tga"));
        assert!(restored.has_file("COLOR-CCCCCC-back [16]-DiffuseColor"));
        assert!(restored
            .references()
            .iter()
            .all(|r| r.state == RefState::Present || r.target == "parent.xmf"));
    }

    #[test]
    fn xml_materials_are_read_too() {
        let xml = br#"<HEADER MAGIC="XRF" VERSION="919"/><MATERIAL NUMMAPS="2"><AMBIENT>1 1 1 1</AMBIENT>
            <MAP TYPE="Diffuse Color">skin.jpg</MAP><MAP TYPE="Opacity">skin_alpha.tga</MAP></MATERIAL>"#;
        let maps = material_maps(xml).unwrap().unwrap();
        assert_eq!(maps.len(), 2);
        assert_eq!(maps[1].kind, "Opacity");
        assert!(material_maps(b"<Template/>").is_none());
        assert!(material_maps(b"CRF\0\x97\x03\0\0").unwrap().is_err());
    }

    /// A small RGBA test image encoded with the image crate.
    fn encoded(format: image::ImageFormat) -> Vec<u8> {
        let mut img = image::RgbaImage::new(5, 3);
        for (x, y, p) in img.enumerate_pixels_mut() {
            *p = image::Rgba([x as u8 * 50, y as u8 * 80, 120, 255]);
        }
        let mut out = std::io::Cursor::new(Vec::new());
        match format {
            // JPEG and GIF encoders take RGB/RGBA as they support.
            image::ImageFormat::Jpeg => image::DynamicImage::ImageRgba8(img)
                .to_rgb8()
                .write_to(&mut out, format)
                .unwrap(),
            _ => img.write_to(&mut out, format).unwrap(),
        }
        out.into_inner()
    }

    #[test]
    fn every_imvu_texture_format_is_found_by_content_under_any_name() {
        use image::ImageFormat as F;
        for (format, kind, right, wrong) in [
            (F::Png, Kind::Png, "a.png", "a.tga"),
            (F::Jpeg, Kind::Jpeg, "a.JPG", "a.png"),
            (F::Gif, Kind::Gif, "a.gif", "a.jpg"),
            (F::Bmp, Kind::Bmp, "a.bmp", "a.png"),
            (F::Tga, Kind::Tga, "a.tga", "a.xrf"),
        ] {
            let bytes = encoded(format);
            for name in [right, wrong, "noext", "blend [16].003"] {
                let file = PackageFile::new(name.into(), Ok(bytes.clone()));
                assert_eq!(file.kind, Some(kind.clone()), "{format:?} named {name}");
                assert_eq!(
                    file.extension_mismatch().is_none(),
                    name == right,
                    "{format:?} named {name}"
                );
            }
            // Whatever it is called, a preview decodes it.
            assert!(
                image::load_from_memory(&bytes).is_ok()
                    || image::load_from_memory_with_format(&bytes, F::Tga).is_ok(),
                "{format:?}"
            );
        }
    }

    #[test]
    fn near_misses_are_not_taken_for_images() {
        // Text that happens to start with "BM".
        let text = b"BMW parts list: bumper, mirror, wheel, tyre, seat".to_vec();
        assert_eq!(Kind::detect("parts.bmp", &text), Kind::Text);
        // "BM" with an impossible DIB header size.
        let mut fake_bmp = vec![b'B', b'M'];
        fake_bmp.extend([0u8; 12]);
        fake_bmp.extend(99u32.to_le_bytes());
        fake_bmp.extend([0xffu8; 40]);
        assert_eq!(Kind::detect("x.bmp", &fake_bmp), Kind::Binary);
        // A file called .tga whose header is not a TGA header.
        let mut bad_tga = vec![0u8, 7, 77];
        bad_tga.extend([0xab; 40]);
        assert_eq!(Kind::detect("skin.tga", &bad_tga), Kind::Binary);
        // "DDS " without the 124-byte DDS header.
        assert_eq!(Kind::detect("x", b"DDS \x01\x02\x03\x04"), Kind::Binary);
        // A TGA with the 2.0 footer is recognised without a .tga name.
        let mut footer = tiny_tga();
        footer.extend([0u8; 8]);
        footer.extend(b"TRUEVISION-XFILE.\0");
        assert_eq!(Kind::detect("texture", &footer), Kind::Tga);
    }

    #[test]
    fn random_binary_is_not_mistaken_for_tga() {
        let mut state = 0x9e37_79b9_u32;
        let mut next = || {
            state ^= state << 13;
            state ^= state >> 17;
            state ^= state << 5;
            state
        };
        let mut false_positives = 0;
        for _ in 0..2000 {
            let len = 18 + (next() % 600) as usize;
            let mut bytes: Vec<u8> = (0..len).map(|_| next() as u8).collect();
            // Keep it binary (a NUL) and away from other signatures.
            bytes[0] = 0;
            if Kind::detect("blob", &bytes) == Kind::Tga {
                false_positives += 1;
            }
        }
        assert_eq!(false_positives, 0);
    }

    #[test]
    fn kinds_come_from_bytes_not_extensions() {
        assert_eq!(
            Kind::detect("x.xmf", b"CMF\0\x97\x03\0\0rest"),
            Kind::Cal3d("CMF mesh", 919)
        );
        assert_eq!(Kind::detect("x.jpg", b"\x89PNG\r\n\x1a\n"), Kind::Png);
        assert_eq!(
            Kind::detect("x.xmf", b"  <HEADER MAGIC=\"XMF\"/>"),
            Kind::Xml
        );
        assert_eq!(Kind::detect("eyes.tga", &tiny_tga()), Kind::Tga);
        // A .tga name alone is not enough: the header must be a TGA header.
        assert_eq!(Kind::detect("eyes.tga", b"\0\0\x02\0"), Kind::Binary);
        assert_eq!(Kind::detect("x.bin", &[0, 1, 2, 0xff, 0xfe]), Kind::Binary);
    }

    #[test]
    fn extraction_refuses_existing_folder_and_keeps_names() {
        let parent = std::env::temp_dir().join(format!("hm-extract-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&parent);
        std::fs::create_dir_all(&parent).unwrap();
        let p = package(vec![
            PackageFile::new("Cargo Shorties.jpg".into(), Ok(vec![1, 2, 3])),
            PackageFile::new("missing.png".into(), Err("HTTP 404".into())),
        ]);
        let (folder, skipped) = extract_all(&p, &parent).unwrap();
        assert_eq!(folder, parent.join("1_rev2"));
        assert_eq!(
            std::fs::read(folder.join("Cargo Shorties.jpg")).unwrap(),
            vec![1, 2, 3]
        );
        assert_eq!(skipped, vec!["missing.png".to_string()]);
        assert!(extract_all(&p, &parent).is_err());
        assert!(write_new_file(&folder.join("Cargo Shorties.jpg"), b"x").is_err());
        // Replacing keeps the new bytes and leaves no temporary file.
        let target = folder.join("replace.bin");
        write_file(&target, b"old").unwrap();
        write_file(&target, b"new").unwrap();
        assert_eq!(std::fs::read(&target).unwrap(), b"new");
        assert!(!folder.join("replace.bin.partial").exists());
        // An unrelated file that happens to use the temporary name survives.
        std::fs::write(folder.join("other.png.partial"), b"keep").unwrap();
        assert!(write_new_file(&folder.join("other.png"), b"x").is_err());
        assert_eq!(
            std::fs::read(folder.join("other.png.partial")).unwrap(),
            b"keep"
        );
        std::fs::remove_dir_all(&parent).unwrap();
    }
}
