//! User settings and the app's own data folder, named [`APP_DIR_NAME`]:
//! `%APPDATA%\IMVUWU` on Windows, `~/Library/Application Support/IMVUWU` on
//! macOS and `$XDG_DATA_HOME/IMVUWU` (default `~/.local/share/IMVUWU`)
//! elsewhere. Older Windows builds used `%APPDATA%\VUTools`, which is moved
//! to the new name the first time the folder is needed.
//!
//! Custom default textures: a user can choose their own head skin, eyes,
//! eyelashes and eyebrows per gender. The chosen image is copied into
//! `textures\<female|male>\<slot>.<ext>` byte for byte, and becomes what
//! "default" means: at launch, on Reset head and on Default skin. Loading a
//! skin in an editor during a session never changes these files.

use std::path::{Path, PathBuf};

/// The app's folder name inside the system's app data folder.
pub const APP_DIR_NAME: &str = "IMVUWU";
/// The folder's name before the app was named IMVUWU.
const OLD_APP_DIR_NAME: &str = "VUTools";

/// The system's folder for per-user app data, if it can be found.
fn data_root() -> Option<PathBuf> {
    let var = |name: &str| std::env::var_os(name).filter(|v| !v.is_empty());
    if cfg!(windows) {
        var("APPDATA").map(PathBuf::from)
    } else if cfg!(target_os = "macos") {
        var("HOME").map(|h| PathBuf::from(h).join("Library/Application Support"))
    } else {
        var("XDG_DATA_HOME")
            .map(PathBuf::from)
            .or_else(|| var("HOME").map(|h| PathBuf::from(h).join(".local/share")))
    }
}

/// The app's data folder (see the module documentation), or `None` when the
/// system's app data folder cannot be found.
pub fn app_dir() -> Option<PathBuf> {
    let appdata = data_root()?;
    let dir = appdata.join(APP_DIR_NAME);
    // Before the app was named, its folder was "VUTools": move it once so
    // settings, textures and built avatars carry over.
    static MIGRATED: std::sync::Once = std::sync::Once::new();
    MIGRATED.call_once(|| {
        let old = appdata.join(OLD_APP_DIR_NAME);
        if !dir.exists() && old.is_dir() {
            let _ = std::fs::rename(&old, &dir);
        }
    });
    Some(dir)
}

/// Opens `dir` in the system's file manager.
pub fn open_folder(dir: &Path) {
    let program = if cfg!(windows) {
        "explorer"
    } else if cfg!(target_os = "macos") {
        "open"
    } else {
        "xdg-open"
    };
    let _ = std::process::Command::new(program).arg(dir).spawn();
}

/// The four head texture slots, in the order of `pose_render::head_textures`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Slot {
    Head,
    Eyes,
    Eyelashes,
    Eyebrows,
}

impl Slot {
    pub const ALL: [Slot; 4] = [Slot::Head, Slot::Eyes, Slot::Eyelashes, Slot::Eyebrows];
    pub fn index(self) -> usize {
        self as usize
    }
    pub fn file_stem(self) -> &'static str {
        match self {
            Slot::Head => "head",
            Slot::Eyes => "eyes",
            Slot::Eyelashes => "eyelashes",
            Slot::Eyebrows => "eyebrows",
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            Slot::Head => "Head skin",
            Slot::Eyes => "Eyes",
            Slot::Eyelashes => "Eyelashes",
            Slot::Eyebrows => "Eyebrows",
        }
    }
}

fn gender_dir(root: &Path, female: bool) -> PathBuf {
    root.join("textures")
        .join(if female { "female" } else { "male" })
}

const IMAGE_EXTENSIONS: [&str; 6] = ["png", "jpg", "jpeg", "tga", "bmp", "gif"];
const MAX_TEXTURE_BYTES: u64 = 64 * 1024 * 1024;

/// The stored custom texture file for a slot, if there is one.
pub fn custom_texture_path(root: &Path, female: bool, slot: Slot) -> Option<PathBuf> {
    let dir = gender_dir(root, female);
    IMAGE_EXTENSIONS
        .iter()
        .map(|ext| dir.join(format!("{}.{ext}", slot.file_stem())))
        .find(|p| p.is_file())
}

/// Decodes an image by content (TGA fallback), bounded to 8192×8192.
pub fn decode_texture(bytes: &[u8]) -> Result<image::RgbaImage, String> {
    let decode = |format: Option<image::ImageFormat>| -> Result<image::RgbaImage, String> {
        let mut reader = image::ImageReader::new(std::io::Cursor::new(bytes));
        match format {
            Some(f) => reader.set_format(f),
            None => reader = reader.with_guessed_format().map_err(|e| e.to_string())?,
        }
        let mut limits = image::Limits::default();
        limits.max_image_width = Some(8192);
        limits.max_image_height = Some(8192);
        reader.limits(limits);
        Ok(reader.decode().map_err(|e| e.to_string())?.into_rgba8())
    };
    decode(None).or_else(|first| decode(Some(image::ImageFormat::Tga)).map_err(|_| first))
}

/// Loads the custom texture for a slot. `None` = no custom texture set;
/// `Some(Err)` = one is set but cannot be used (the built-in one is used).
pub fn load_custom_texture(
    root: &Path,
    female: bool,
    slot: Slot,
) -> Option<Result<image::RgbaImage, String>> {
    let path = custom_texture_path(root, female, slot)?;
    Some(
        (|| {
            let size = std::fs::metadata(&path).map_err(|e| e.to_string())?.len();
            if size > MAX_TEXTURE_BYTES {
                return Err("larger than 64 MB".into());
            }
            decode_texture(&std::fs::read(&path).map_err(|e| e.to_string())?)
        })()
        .map_err(|e| format!("{}: {e}", path.display())),
    )
}

/// Copies `source` in as the custom texture for a slot, replacing any
/// earlier one. The image must decode; its bytes are kept unchanged.
pub fn set_custom_texture(
    root: &Path,
    female: bool,
    slot: Slot,
    source: &Path,
) -> Result<(), String> {
    let size = std::fs::metadata(source).map_err(|e| e.to_string())?.len();
    if size > MAX_TEXTURE_BYTES {
        return Err("the image is larger than 64 MB".into());
    }
    let bytes = std::fs::read(source).map_err(|e| e.to_string())?;
    decode_texture(&bytes).map_err(|e| format!("not a usable image: {e}"))?;
    let ext = source
        .extension()
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase)
        .filter(|e| IMAGE_EXTENSIONS.contains(&e.as_str()))
        .unwrap_or_else(|| "png".into());
    let dir = gender_dir(root, female);
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let target = dir.join(format!("{}.{ext}", slot.file_stem()));
    crate::package::write_file(&target, &bytes)?;
    // Remove an older choice stored under another extension.
    for other in IMAGE_EXTENSIONS.iter().filter(|e| **e != ext) {
        let _ = std::fs::remove_file(dir.join(format!("{}.{other}", slot.file_stem())));
    }
    Ok(())
}

/// Removes the custom texture for a slot, going back to the built-in one.
pub fn clear_custom_texture(root: &Path, female: bool, slot: Slot) -> Result<(), String> {
    while let Some(path) = custom_texture_path(root, female, slot) {
        std::fs::remove_file(&path).map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// The built-in head textures with any custom defaults applied, plus a
/// message for each custom texture that could not be used.
pub fn default_textures(female: bool) -> (Vec<crate::render::Texture>, Vec<String>) {
    let mut textures = crate::render::textures(female);
    let mut problems = Vec::new();
    if let Some(root) = app_dir() {
        for slot in Slot::ALL {
            match load_custom_texture(&root, female, slot) {
                Some(Ok(image)) if slot.index() < textures.len() => {
                    textures[slot.index()] = crate::render::Texture::new(image);
                }
                Some(Err(e)) => problems.push(format!(
                    "Custom {} default could not be used ({e}); using the built-in one.",
                    slot.label().to_lowercase()
                )),
                _ => {}
            }
        }
    }
    (textures, problems)
}

/// Which workspace opens at launch.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StartWorkspace {
    Packages,
    HeadMaker,
    FaceMaker,
    AvatarPoser,
}

impl StartWorkspace {
    pub const ALL: [StartWorkspace; 4] = [
        StartWorkspace::Packages,
        StartWorkspace::HeadMaker,
        StartWorkspace::FaceMaker,
        StartWorkspace::AvatarPoser,
    ];
    /// The app's workspace index (0 HeadMaker, 1 FaceMaker, 2 Poser, 3 Packages).
    pub fn workspace(self) -> usize {
        match self {
            StartWorkspace::HeadMaker => 0,
            StartWorkspace::FaceMaker => 1,
            StartWorkspace::AvatarPoser => 2,
            StartWorkspace::Packages => 3,
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            StartWorkspace::Packages => "Packages",
            StartWorkspace::HeadMaker => "HeadMaker",
            StartWorkspace::FaceMaker => "FaceMaker",
            StartWorkspace::AvatarPoser => "Avatar Poser",
        }
    }
    fn key(self) -> &'static str {
        match self {
            StartWorkspace::Packages => "packages",
            StartWorkspace::HeadMaker => "headmaker",
            StartWorkspace::FaceMaker => "facemaker",
            StartWorkspace::AvatarPoser => "poser",
        }
    }
}

/// Settings stored in `settings.json`. Unknown keys are ignored and missing
/// ones take their defaults, so older or newer files still load.
#[derive(Clone, Debug, PartialEq)]
pub struct Settings {
    pub start_workspace: StartWorkspace,
    /// Shortcut keys changed from their defaults.
    pub shortcuts: Vec<(crate::shortcuts::Command, crate::shortcuts::Binding)>,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            // Browse/download is the main window.
            start_workspace: StartWorkspace::Packages,
            shortcuts: Vec::new(),
        }
    }
}

impl Settings {
    pub fn from_json(text: &str) -> Self {
        let mut settings = Settings::default();
        if let Ok(value) = serde_json::from_str::<serde_json::Value>(text) {
            if let Some(key) = value.get("start_workspace").and_then(|v| v.as_str()) {
                if let Some(w) = StartWorkspace::ALL.into_iter().find(|w| w.key() == key) {
                    settings.start_workspace = w;
                }
            }
            if let Some(map) = value.get("shortcuts").and_then(|v| v.as_object()) {
                for command in crate::shortcuts::Command::ALL {
                    let binding = map
                        .get(command.id())
                        .and_then(|v| v.as_str())
                        .and_then(crate::shortcuts::Binding::parse);
                    if let Some(b) = binding.filter(|b| *b != command.default_binding()) {
                        settings.shortcuts.push((command, b));
                    }
                }
            }
        }
        settings
    }
    pub fn to_json(&self) -> String {
        let shortcuts: serde_json::Map<String, serde_json::Value> = self
            .shortcuts
            .iter()
            .map(|(c, b)| (c.id().to_string(), b.text().into()))
            .collect();
        serde_json::json!({
            "start_workspace": self.start_workspace.key(),
            "shortcuts": shortcuts,
        })
        .to_string()
    }
    pub fn load() -> Self {
        app_dir()
            .and_then(|d| std::fs::read_to_string(d.join("settings.json")).ok())
            .map(|t| Self::from_json(&t))
            .unwrap_or_default()
    }
    pub fn save(&self) -> Result<(), String> {
        let dir = app_dir().ok_or("No app data folder (APPDATA or HOME is not set)")?;
        std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        crate::package::write_file(&dir.join("settings.json"), self.to_json().as_bytes())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shortcut_overrides_round_trip_and_ignore_bad_keys() {
        use crate::shortcuts::{Binding, Command};
        let mut settings = Settings::default();
        let b = Binding::parse("Ctrl+Alt+K").unwrap();
        settings.shortcuts.push((Command::Save, b));
        let back = Settings::from_json(&settings.to_json());
        assert_eq!(back, settings);
        let odd = Settings::from_json(r#"{"shortcuts":{"save":"Ctrl+???","undo":"Ctrl+Z"}}"#);
        assert!(odd.shortcuts.is_empty());
    }

    fn temp_root(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("vutools-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn png(colour: [u8; 4]) -> Vec<u8> {
        let image = image::RgbaImage::from_pixel(4, 2, image::Rgba(colour));
        let mut out = std::io::Cursor::new(Vec::new());
        image.write_to(&mut out, image::ImageFormat::Png).unwrap();
        out.into_inner()
    }

    #[test]
    fn custom_textures_are_stored_byte_for_byte_and_can_be_cleared() {
        let root = temp_root("tex");
        let source = root.join("my skin.PNG");
        let bytes = png([10, 20, 30, 255]);
        std::fs::write(&source, &bytes).unwrap();
        assert!(load_custom_texture(&root, true, Slot::Head).is_none());
        set_custom_texture(&root, true, Slot::Head, &source).unwrap();
        let stored = custom_texture_path(&root, true, Slot::Head).unwrap();
        assert_eq!(stored.file_name().unwrap(), "head.png");
        assert_eq!(std::fs::read(&stored).unwrap(), bytes);
        let image = load_custom_texture(&root, true, Slot::Head)
            .unwrap()
            .unwrap();
        assert_eq!(image.get_pixel(0, 0).0, [10, 20, 30, 255]);
        // Other gender and slots are untouched.
        assert!(load_custom_texture(&root, false, Slot::Head).is_none());
        assert!(load_custom_texture(&root, true, Slot::Eyes).is_none());
        // A replacement under another extension removes the old file.
        let jpg_named = root.join("skin2.jpg");
        std::fs::write(&jpg_named, png([1, 2, 3, 255])).unwrap();
        set_custom_texture(&root, true, Slot::Head, &jpg_named).unwrap();
        assert!(!stored.exists());
        clear_custom_texture(&root, true, Slot::Head).unwrap();
        assert!(load_custom_texture(&root, true, Slot::Head).is_none());
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn unusable_images_are_refused_or_reported() {
        let root = temp_root("bad");
        let junk = root.join("junk.png");
        std::fs::write(&junk, b"not an image").unwrap();
        assert!(set_custom_texture(&root, false, Slot::Eyes, &junk).is_err());
        assert!(custom_texture_path(&root, false, Slot::Eyes).is_none());
        // A stored file that later becomes unreadable is reported, not fatal.
        let dir = gender_dir(&root, false);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("eyes.png"), b"broken").unwrap();
        assert!(matches!(
            load_custom_texture(&root, false, Slot::Eyes),
            Some(Err(_))
        ));
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn settings_round_trip_and_tolerate_unknown_or_missing_keys() {
        let s = Settings {
            start_workspace: StartWorkspace::AvatarPoser,
            ..Settings::default()
        };
        assert_eq!(Settings::from_json(&s.to_json()), s);
        assert_eq!(Settings::from_json("{}"), Settings::default());
        assert_eq!(Settings::from_json("not json"), Settings::default());
        assert_eq!(
            Settings::from_json(r#"{"start_workspace":"facemaker","future":1}"#).start_workspace,
            StartWorkspace::FaceMaker
        );
        assert_eq!(
            Settings::default().start_workspace,
            StartWorkspace::Packages
        );
    }
}
