//! Embeds `assets/icon.ico` and version details into the Windows executable.
//!
//! The compiled resource file (`.res`) is written here directly, so no
//! Windows SDK tool (rc.exe) is needed; the linker takes the `.res` as an
//! input. It holds one RT_ICON per image in the .ico, the RT_GROUP_ICON
//! that lists them (the exe's icon), and an RT_VERSION block.
//! Formats: "Resource File Formats" and "VS_VERSIONINFO" in the Win32 docs.
use std::path::PathBuf;

const RT_ICON: u16 = 3;
const RT_GROUP_ICON: u16 = 14;
const RT_VERSION: u16 = 16;
const LANG_EN_US: u16 = 0x0409;

fn main() {
    println!("cargo:rerun-if-changed=assets/icon.ico");
    println!("cargo:rerun-if-changed=build.rs");
    let manifest = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    // IMVU-derived files are kept out of the public repository: the
    // developer's avatar copies (feature `bundled-imvu-avatars`) and the IMVU
    // head meshes some tests use. `imvu_fixtures` enables those tests only
    // when the meshes are present.
    println!("cargo::rustc-check-cfg=cfg(imvu_fixtures)");
    println!("cargo:rerun-if-changed=tests/fixtures");
    if manifest.join("tests/fixtures/Head-Srs.cmf").is_file() {
        println!("cargo::rustc-cfg=imvu_fixtures");
    }
    if std::env::var_os("CARGO_FEATURE_BUNDLED_IMVU_AVATARS").is_some()
        && !manifest.join("assets/poser/female").is_dir()
    {
        panic!(
            "feature bundled-imvu-avatars needs the developer's IMVU avatar files in              assets/poser/female and assets/poser/male, which are not published;              build without it (the app downloads the avatars on first launch)"
        );
    }
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    let out = PathBuf::from(std::env::var("OUT_DIR").unwrap());
    let ico = std::fs::read(manifest.join("assets").join("icon.ico")).expect("read icon.ico");
    let version = std::env::var("CARGO_PKG_VERSION").unwrap();
    let res = resource_file(&ico, &version).expect("assets/icon.ico is not a valid .ico");
    let path = out.join("app.res");
    std::fs::write(&path, res).expect("write app.res");
    println!("cargo:rustc-link-arg-bins={}", path.display());
}

/// The whole `.res` file: the empty header entry, then each resource.
fn resource_file(ico: &[u8], version: &str) -> Option<Vec<u8>> {
    let mut res = Vec::new();
    // Every .res starts with an empty entry of type 0 and name 0.
    entry(&mut res, 0, 0, 0, 0, &[]);
    let word = |at: usize| u16::from_le_bytes([ico[at], ico[at + 1]]);
    let dword = |at: usize| u32::from_le_bytes(ico[at..at + 4].try_into().unwrap());
    if ico.len() < 6 || word(0) != 0 || word(2) != 1 {
        return None;
    }
    let count = word(4) as usize;
    // GRPICONDIR: the ICONDIR header, with entries that name RT_ICON ids
    // instead of file offsets.
    let mut group = vec![0, 0, 1, 0];
    group.extend_from_slice(&(count as u16).to_le_bytes());
    for i in 0..count {
        let at = 6 + 16 * i;
        let (size, offset) = (dword(at + 8) as usize, dword(at + 12) as usize);
        let image = ico.get(offset..offset + size)?;
        let id = (i + 1) as u16;
        entry(&mut res, RT_ICON, id, 0x1010, LANG_EN_US, image);
        group.extend_from_slice(&ico[at..at + 12]); // size, colours, planes, bpp, bytes
        group.extend_from_slice(&id.to_le_bytes());
    }
    entry(&mut res, RT_GROUP_ICON, 1, 0x1030, LANG_EN_US, &group);
    entry(
        &mut res,
        RT_VERSION,
        1,
        0x0030,
        LANG_EN_US,
        &version_info(version),
    );
    Some(res)
}

/// One resource entry with ordinal type and name, padded to 4 bytes.
fn entry(res: &mut Vec<u8>, kind: u16, name: u16, flags: u16, language: u16, data: &[u8]) {
    res.extend_from_slice(&(data.len() as u32).to_le_bytes()); // DataSize
    res.extend_from_slice(&32u32.to_le_bytes()); // HeaderSize
    for ordinal in [kind, name] {
        res.extend_from_slice(&0xFFFFu16.to_le_bytes());
        res.extend_from_slice(&ordinal.to_le_bytes());
    }
    res.extend_from_slice(&0u32.to_le_bytes()); // DataVersion
    res.extend_from_slice(&flags.to_le_bytes()); // MemoryFlags
    res.extend_from_slice(&language.to_le_bytes());
    res.extend_from_slice(&0u32.to_le_bytes()); // Version
    res.extend_from_slice(&0u32.to_le_bytes()); // Characteristics
    res.extend_from_slice(data);
    pad(res);
}

fn pad(bytes: &mut Vec<u8>) {
    while !bytes.len().is_multiple_of(4) {
        bytes.push(0);
    }
}

fn utf16z(text: &str) -> Vec<u8> {
    text.encode_utf16()
        .chain([0])
        .flat_map(u16::to_le_bytes)
        .collect()
}

/// A version-resource node: wLength, wValueLength, wType, szKey, padding,
/// value, padding, children. `text` values count UTF-16 units, binary
/// values count bytes.
fn node(key: &str, value: &[u8], text: bool, children: &[Vec<u8>]) -> Vec<u8> {
    let mut n = vec![0u8; 6];
    n.extend_from_slice(&utf16z(key));
    pad(&mut n);
    n.extend_from_slice(value);
    let value_length = if text { value.len() / 2 } else { value.len() };
    for child in children {
        pad(&mut n);
        n.extend_from_slice(child);
    }
    let length = n.len() as u16;
    n[0..2].copy_from_slice(&length.to_le_bytes());
    n[2..4].copy_from_slice(&(value_length as u16).to_le_bytes());
    n[4..6].copy_from_slice(&(text as u16).to_le_bytes());
    n
}

/// VS_VERSIONINFO: the fixed file info plus English strings.
fn version_info(version: &str) -> Vec<u8> {
    let parts: Vec<u16> = version
        .split(['.', '-'])
        .take(3)
        .map(|n| n.parse().unwrap_or(0))
        .chain([0, 0, 0])
        .take(4)
        .collect();
    let ms = ((parts[0] as u32) << 16) | parts[1] as u32;
    let ls = ((parts[2] as u32) << 16) | parts[3] as u32;
    let mut fixed = Vec::new();
    for value in [
        0xFEEF04BDu32, // signature
        0x0001_0000,   // structure version 1.0
        ms,
        ls, // file version
        ms,
        ls,   // product version
        0x3F, // flags mask
        0,    // flags
        0x4,  // VOS_NT_WINDOWS32
        0x1,  // VFT_APP
        0,    // subtype
        0,
        0, // date
    ] {
        fixed.extend_from_slice(&value.to_le_bytes());
    }
    let strings: Vec<Vec<u8>> = [
        ("FileDescription", "IMVUWU"),
        ("ProductName", "IMVUWU"),
        ("FileVersion", version),
        ("ProductVersion", version),
        ("OriginalFilename", "IMVUWU.exe"),
    ]
    .iter()
    .map(|(key, value)| node(key, &utf16z(value), true, &[]))
    .collect();
    let table = node("040904b0", &[], true, &strings);
    let string_info = node("StringFileInfo", &[], true, &[table]);
    let translation = node("Translation", &[0x09, 0x04, 0xb0, 0x04], false, &[]);
    let var_info = node("VarFileInfo", &[], true, &[translation]);
    node("VS_VERSION_INFO", &fixed, false, &[string_info, var_info])
}
