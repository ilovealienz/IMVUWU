//! IMVU base avatars (product 80, female; product 191, male) used by the
//! poser and for the head materials.
//!
//! The app can be distributed without any IMVU files: the user builds them
//! once, by downloading the two products from IMVU's public product data or
//! from a local ZIP/CHKN copy. Built avatars live in
//! `%APPDATA%\IMVUWU\avatars\<female|male>\` with their original IMVU file
//! names and bytes, plus `source.json` recording where they came from.
//!
//! Developer builds can still embed the copies in `assets/poser` with the
//! `bundled-imvu-avatars` cargo feature (on by default for now). Built
//! avatars always take precedence; without either, callers fall back to
//! non-IMVU placeholders.

use std::path::{Path, PathBuf};

/// Product ID and the revision verified with this app (newest at the time).
pub const FEMALE_PRODUCT: (u64, u32) = (80, 21);
pub const MALE_PRODUCT: (u64, u32) = (191, 29);

/// The default female skin is its own product: the female avatar does not
/// carry the skin IMVU shows on it (the male avatar does). Its two textures
/// replace the same-named ones from product 80. Revision 13 was verified to
/// match the skin this app shipped with, byte for byte.
pub const FEMALE_SKIN_PRODUCT: (u64, u32) = (1_133_635, 13);

pub fn product(female: bool) -> (u64, u32) {
    if female {
        FEMALE_PRODUCT
    } else {
        MALE_PRODUCT
    }
}

fn gender(female: bool) -> &'static str {
    if female {
        "female"
    } else {
        "male"
    }
}

/// Where an avatar package came from.
#[derive(Clone, Debug, PartialEq)]
pub enum Origin {
    /// Built by the user: product, revision and how it was obtained.
    Built {
        product: u64,
        revision: u32,
        from: String,
    },
    /// Embedded in a developer build.
    #[cfg_attr(not(feature = "bundled-imvu-avatars"), allow(dead_code))]
    Bundled,
}

/// An avatar package's files with their original names.
pub struct AvatarPackage {
    pub files: Vec<(String, Vec<u8>)>,
    pub origin: Origin,
}

impl AvatarPackage {
    pub fn slices(&self) -> Vec<(&str, &[u8])> {
        self.files
            .iter()
            .map(|(n, b)| (n.as_str(), b.as_slice()))
            .collect()
    }
}

pub fn avatars_dir(root: &Path, female: bool) -> PathBuf {
    root.join("avatars").join(gender(female))
}

/// The built avatar package in the app folder, if one is installed.
pub fn installed(root: &Path, female: bool) -> Option<AvatarPackage> {
    let dir = avatars_dir(root, female);
    let manifest: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(dir.join("source.json")).ok()?).ok()?;
    let mut files = Vec::new();
    for entry in std::fs::read_dir(&dir).ok()?.filter_map(Result::ok) {
        let name = entry.file_name().to_string_lossy().into_owned();
        if name == "source.json" || !entry.file_type().ok()?.is_file() {
            continue;
        }
        files.push((name, std::fs::read(entry.path()).ok()?));
    }
    files.sort_by(|a, b| a.0.cmp(&b.0));
    Some(AvatarPackage {
        files,
        origin: Origin::Built {
            product: manifest.get("product")?.as_u64()?,
            revision: manifest.get("revision")?.as_u64()? as u32,
            from: manifest
                .get("from")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string(),
        },
    })
}

#[cfg(feature = "bundled-imvu-avatars")]
mod bundled_files {
    include!("avatar_assets.rs");
}

/// The embedded developer copy, when built with `bundled-imvu-avatars`.
pub fn bundled(female: bool) -> Option<AvatarPackage> {
    #[cfg(feature = "bundled-imvu-avatars")]
    {
        let files = if female {
            bundled_files::FEMALE
        } else {
            bundled_files::MALE
        };
        Some(AvatarPackage {
            files: files
                .iter()
                .map(|(n, b)| (n.to_string(), b.to_vec()))
                .collect(),
            origin: Origin::Bundled,
        })
    }
    #[cfg(not(feature = "bundled-imvu-avatars"))]
    {
        let _ = female;
        None
    }
}

/// The embedded file list itself, for tests.
#[cfg(all(test, feature = "bundled-imvu-avatars"))]
pub fn bundled_static(female: bool) -> &'static [(&'static str, &'static [u8])] {
    if female {
        bundled_files::FEMALE
    } else {
        bundled_files::MALE
    }
}

/// The avatar package to use: built first, then bundled. Tests only use the
/// bundled copy, so their results do not depend on this machine's APPDATA.
pub fn load(female: bool) -> Option<AvatarPackage> {
    let built = if cfg!(test) {
        None
    } else {
        crate::settings::app_dir().and_then(|root| installed(&root, female))
    };
    built.or_else(|| bundled(female))
}

/// Whether an avatar is available, without reading the package: the
/// embedded copy, or a built one (`install` only writes validated packages).
pub fn available(female: bool) -> bool {
    cfg!(feature = "bundled-imvu-avatars")
        || crate::settings::app_dir()
            .is_some_and(|root| avatars_dir(&root, female).join("source.json").is_file())
}

/// A one-line description of what is in use, for the settings window.
pub fn describe(female: bool) -> String {
    match load(female).map(|p| p.origin) {
        Some(Origin::Built {
            product,
            revision,
            from,
        }) => format!("Built: product {product} revision {revision} ({from})"),
        Some(Origin::Bundled) => "Using the copy embedded in this developer build".into(),
        None => "Not built yet".into(),
    }
}

/// Checks that `files` really are this gender's IMVU base avatar: the
/// package must load as a poser avatar (skeleton matching the poser's bone
/// IDs and hierarchy) and contain the head materials the editors use.
pub fn validate(files: &[(&str, &[u8])], female: bool) -> Result<(), String> {
    crate::pose_render::validate_avatar_package(files, female)
}

/// Writes a validated package into the app folder, replacing any earlier
/// build only once the new copy is completely written.
pub fn install(
    root: &Path,
    female: bool,
    files: &[(String, Vec<u8>)],
    product: u64,
    revision: u32,
    from: &str,
) -> Result<PathBuf, String> {
    let slices: Vec<(&str, &[u8])> = files
        .iter()
        .map(|(n, b)| (n.as_str(), b.as_slice()))
        .collect();
    validate(&slices, female)?;
    let target = avatars_dir(root, female);
    let parent = target.parent().ok_or("bad avatar folder")?;
    std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    let staging = parent.join(format!("{}.partial-{}", gender(female), std::process::id()));
    let _ = std::fs::remove_dir_all(&staging);
    std::fs::create_dir(&staging).map_err(|e| e.to_string())?;
    let written = (|| -> Result<(), String> {
        for (name, bytes) in files {
            if !crate::imvu::is_safe_name(name) {
                return Err(format!("unsafe file name {name:?}"));
            }
            crate::package::write_new_file(&staging.join(name), bytes)?;
        }
        let manifest = serde_json::json!({
            "product": product,
            "revision": revision,
            "from": from,
            "files": files.iter().map(|(n, b)| serde_json::json!({"name": n, "size": b.len()})).collect::<Vec<_>>(),
        });
        crate::package::write_new_file(
            &staging.join("source.json"),
            serde_json::to_string_pretty(&manifest).unwrap().as_bytes(),
        )
    })();
    if let Err(e) = written {
        let _ = std::fs::remove_dir_all(&staging);
        return Err(e);
    }
    let old = parent.join(format!("{}.old-{}", gender(female), std::process::id()));
    if target.exists() {
        std::fs::rename(&target, &old).map_err(|e| e.to_string())?;
    }
    if let Err(e) = std::fs::rename(&staging, &target) {
        // Put the previous build back if the swap fails.
        let _ = std::fs::rename(&old, &target);
        let _ = std::fs::remove_dir_all(&staging);
        return Err(e.to_string());
    }
    let _ = std::fs::remove_dir_all(&old);
    Ok(target)
}

/// A product's files: original name and bytes.
type ProductFiles = Vec<(String, Vec<u8>)>;

/// Downloads every file of a product revision (the pinned revision, or the
/// newest if that one is gone). Fails if any file cannot be downloaded.
fn download_product(
    client: &crate::imvu::Client,
    (id, pinned): (u64, u32),
    progress: &(dyn Fn(String) + Sync),
) -> Result<(u32, ProductFiles), String> {
    progress(format!("Reading product {id} revision {pinned}…"));
    let (revision, entries) = match client.contents(id, pinned) {
        Ok(entries) => (pinned, entries),
        Err(crate::imvu::Failure::Http(404)) => {
            progress(format!(
                "Product {id} revision {pinned} is gone; looking for the newest…"
            ));
            let newest = newest_revision(client, id)
                .ok_or_else(|| format!("no revision of product {id} was found"))?;
            (
                newest,
                client.contents(id, newest).map_err(|e| e.to_string())?,
            )
        }
        Err(e) => return Err(format!("product {id}: {e}")),
    };
    let total = entries.len();
    let cancel = std::sync::atomic::AtomicBool::new(false);
    let report = |done: usize| progress(format!("Product {id}: {done} of {total} files…"));
    let downloaded =
        crate::imvu::download_revision(client, id, revision, &entries, &cancel, &report);
    let mut files = Vec::new();
    for d in downloaded {
        files.push((
            d.entry.name.clone(),
            d.result
                .map_err(|e| format!("{} could not be downloaded: {e}", d.entry.name))?,
        ));
    }
    Ok((revision, files))
}

/// Replaces the avatar's textures with the skin product's same-named
/// images. Returns the names replaced; at least one must match.
pub fn apply_skin(
    files: &mut [(String, Vec<u8>)],
    skin: &[(String, Vec<u8>)],
) -> Result<Vec<String>, String> {
    let mut replaced = Vec::new();
    for (name, bytes) in skin {
        if name.eq_ignore_ascii_case("index.xml") {
            continue;
        }
        if let Some(entry) = files.iter_mut().find(|(n, _)| n.eq_ignore_ascii_case(name)) {
            entry.1 = bytes.clone();
            replaced.push(name.clone());
        }
    }
    if replaced.is_empty() {
        Err("the skin product has no texture that the avatar uses".into())
    } else {
        Ok(replaced)
    }
}

/// Downloads the avatar from IMVU's public product data (no login) and
/// installs it. The female avatar also gets the default female skin
/// (product 1133635) applied.
pub fn build_from_imvu(
    client: &crate::imvu::Client,
    root: &Path,
    female: bool,
    progress: &(dyn Fn(String) + Sync),
) -> Result<PathBuf, String> {
    let (id, _) = product(female);
    let (revision, mut files) = download_product(client, product(female), progress)?;
    let mut from = format!("downloaded from IMVU {}", today());
    if female {
        let (skin_id, _) = FEMALE_SKIN_PRODUCT;
        let (skin_revision, skin) = download_product(client, FEMALE_SKIN_PRODUCT, progress)?;
        let replaced = apply_skin(&mut files, &skin)?;
        from.push_str(&format!(
            ", skin from product {skin_id} revision {skin_revision} ({} textures)",
            replaced.len()
        ));
    }
    progress("Checking and installing…".into());
    install(root, female, &files, id, revision, &from)
}

/// Installs the avatar from a local ZIP/CHKN/folder copy of the product.
/// Copies renamed by other downloaders get their original names back first.
pub fn build_from_local(root: &Path, female: bool, path: &Path) -> Result<PathBuf, String> {
    let package = if path.is_dir() {
        crate::package::open_folder(path)?
    } else {
        crate::package::open_archive(path)?
    };
    let (restored, renamed) = package.with_referenced_names();
    let mut files = Vec::new();
    for file in &restored.files {
        let bytes = file
            .bytes
            .as_ref()
            .map_err(|e| format!("{}: {e}", file.name))?;
        // Product files are flat; ignore anything in sub-folders.
        if !file.name.contains('/') {
            files.push((file.name.clone(), bytes.as_ref().clone()));
        }
    }
    let (id, revision) = product(female);
    let file_name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let from = if renamed.is_empty() {
        format!("from {file_name}")
    } else {
        format!("from {file_name}, {} names restored", renamed.len())
    };
    install(root, female, &files, id, revision, &from)
}

fn newest_revision(client: &crate::imvu::Client, id: u64) -> Option<u32> {
    let (sender, receiver) = std::sync::mpsc::channel();
    let plan = crate::imvu::ScanPlan {
        first: 1,
        last: 100,
        stop_after_missing: 15,
    };
    let cancel = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    crate::imvu::scan_revisions(client.clone(), id, plan, cancel, sender);
    receiver
        .into_iter()
        .filter_map(|e| match e {
            crate::imvu::ScanEvent::Checked(r, crate::imvu::RevisionState::Exists) => Some(r),
            _ => None,
        })
        .max()
}

fn today() -> String {
    // Days since 1970 → civil date (UTC), without a date crate.
    let days = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() / 86_400)
        .unwrap_or(0) as i64;
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!("{year:04}-{month:02}-{day:02}")
}

#[cfg(all(test, feature = "bundled-imvu-avatars"))]
mod tests {
    use super::*;

    fn temp_root(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("vutools-avatars-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn bundled_packages_validate_for_their_own_gender_only() {
        for female in [true, false] {
            let package = bundled(female).unwrap();
            assert!(
                validate(&package.slices(), female).is_ok(),
                "female={female}"
            );
            assert!(
                validate(&package.slices(), !female).is_err(),
                "female={female}"
            );
        }
    }

    #[test]
    fn install_round_trips_and_replaces_previous_build() {
        let root = temp_root("install");
        let package = bundled(true).unwrap();
        install(&root, true, &package.files, 80, 21, "test").unwrap();
        let built = installed(&root, true).unwrap();
        let mut expected = package.files.clone();
        expected.sort_by(|a, b| a.0.cmp(&b.0));
        assert_eq!(built.files, expected);
        assert_eq!(
            built.origin,
            Origin::Built {
                product: 80,
                revision: 21,
                from: "test".into()
            }
        );
        // A second install replaces the first; no staging folders are left.
        install(&root, true, &package.files, 80, 22, "again").unwrap();
        assert!(matches!(
            installed(&root, true).unwrap().origin,
            Origin::Built { revision: 22, .. }
        ));
        let leftovers: Vec<_> = std::fs::read_dir(root.join("avatars"))
            .unwrap()
            .filter_map(Result::ok)
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(leftovers, vec!["female".to_string()]);
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn invalid_packages_are_not_installed() {
        let root = temp_root("invalid");
        let mut package = bundled(false).unwrap();
        package.files.retain(|(n, _)| n != "index.xml");
        assert!(install(&root, false, &package.files, 191, 29, "test").is_err());
        assert!(installed(&root, false).is_none());
        assert!(!avatars_dir(&root, false).exists());
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn female_skin_replaces_only_matching_textures() {
        let mut files = vec![
            (
                "Female01_Anime01_head_default_020degrees.tga".to_string(),
                vec![1],
            ),
            ("other.tga".to_string(), vec![2]),
            ("index.xml".to_string(), b"<avatar/>".to_vec()),
        ];
        let skin = vec![
            (
                "Female01_Anime01_head_default_020degrees.tga".to_string(),
                vec![9],
            ),
            ("index.xml".to_string(), b"<skin/>".to_vec()),
        ];
        let replaced = apply_skin(&mut files, &skin).unwrap();
        assert_eq!(
            replaced,
            vec!["Female01_Anime01_head_default_020degrees.tga"]
        );
        assert_eq!(files[0].1, vec![9]);
        assert_eq!(files[1].1, vec![2]);
        assert_eq!(files[2].1, b"<avatar/>".to_vec());
        assert!(apply_skin(&mut files, &[("unrelated.png".into(), vec![0])]).is_err());
    }

    #[test]
    fn dates_are_formatted_as_iso() {
        let d = today();
        assert_eq!(d.len(), 10);
        assert_eq!(&d[4..5], "-");
    }
}
