//! Interface palette and style, matching the user's NixOS desktop theme
//! (nixos-config theme.nix, "Copper" dark preset): warm browns, copper
//! accent, square corners and an accent underline on the active tab.
use eframe::egui::{self, Color32, Stroke, Vec2};

const fn rgb(c: [u8; 3]) -> Color32 {
    Color32::from_rgb(c[0], c[1], c[2])
}

// theme.nix palette, as RGB so the CPU/GPU viewports can share it.
pub const BG_DARK_RGB: [u8; 3] = [0x1d, 0x19, 0x16];
pub const BG_RGB: [u8; 3] = [0x2b, 0x20, 0x1a];
pub const BG_ALT_RGB: [u8; 3] = [0x35, 0x24, 0x1c];
pub const SURFACE_RGB: [u8; 3] = [0x47, 0x3c, 0x31];
pub const FG_RGB: [u8; 3] = [0xd4, 0xa9, 0x7b];
pub const MUTED_RGB: [u8; 3] = [0x8d, 0x74, 0x4a];

pub const BG_DARK: Color32 = rgb(BG_DARK_RGB);
pub const BG: Color32 = rgb(BG_RGB);
pub const BG_ALT: Color32 = rgb(BG_ALT_RGB);
pub const SURFACE: Color32 = rgb(SURFACE_RGB);
pub const FG: Color32 = rgb(FG_RGB);
pub const FG_BRIGHT: Color32 = Color32::from_rgb(0xed, 0xe0, 0xc8);
pub const MUTED: Color32 = rgb(MUTED_RGB);
pub const ACCENT: Color32 = Color32::from_rgb(0xc8, 0x6f, 0x42);
pub const ON_ACCENT: Color32 = Color32::from_rgb(0x24, 0x22, 0x1c);
/// theme.nix derived selectionBg: accent mixed 67% over bg.
pub const SELECTION_BG: Color32 = Color32::from_rgb(0x94, 0x55, 0x35);
pub const RED: Color32 = Color32::from_rgb(0xe5, 0x6b, 0x55);
pub const ORANGE: Color32 = Color32::from_rgb(0xe1, 0x82, 0x45);

/// Viewport stage gradient (top to bottom) and floor colours.
pub const STAGE_TOP: [u8; 3] = BG_DARK_RGB;
pub const STAGE_BOTTOM: [u8; 3] = BG_RGB;
pub const FLOOR_FILL: [u8; 3] = BG_ALT_RGB;
pub const FLOOR_GRID: [u8; 3] = SURFACE_RGB;
pub const FLOOR_BORDER: [u8; 3] = [0x6a, 0x58, 0x3d];
pub const FLOOR_DECAL: [u8; 3] = FG_RGB;
pub fn stage_row(y: usize, h: usize) -> Color32 {
    let t = y as f32 / h.max(1) as f32;
    let c =
        |i: usize| (STAGE_TOP[i] as f32 + (STAGE_BOTTOM[i] as f32 - STAGE_TOP[i] as f32) * t) as u8;
    Color32::from_rgb(c(0), c(1), c(2))
}

/// Installs the Copper theme. The app is dark-only: egui follows the OS
/// light/dark setting by default and keeps a separate style per theme, so
/// the theme is pinned to dark and the same style is installed for both.
pub fn install(ctx: &egui::Context) {
    ctx.set_theme(egui::ThemePreference::Dark);
    let mut s = (*ctx.style_of(egui::Theme::Dark)).clone();
    s.visuals = egui::Visuals::dark();
    let v = &mut s.visuals;
    v.panel_fill = BG;
    v.window_fill = BG_ALT;
    v.extreme_bg_color = BG_DARK;
    v.faint_bg_color = BG_ALT;
    v.code_bg_color = BG_DARK;
    v.override_text_color = None;
    v.hyperlink_color = ACCENT;
    v.warn_fg_color = ORANGE;
    v.error_fg_color = RED;
    v.selection.bg_fill = SELECTION_BG;
    v.selection.stroke = Stroke::new(1.0_f32, FG_BRIGHT);
    v.window_stroke = Stroke::new(2.0_f32, ACCENT);
    v.window_shadow = egui::epaint::Shadow::NONE;
    v.popup_shadow = egui::epaint::Shadow::NONE;
    v.widgets.noninteractive.bg_fill = BG;
    v.widgets.noninteractive.weak_bg_fill = BG;
    v.widgets.noninteractive.fg_stroke = Stroke::new(1.0_f32, FG);
    v.widgets.noninteractive.bg_stroke = Stroke::new(1.0_f32, SURFACE);
    v.widgets.inactive.bg_fill = BG_ALT;
    v.widgets.inactive.weak_bg_fill = BG_ALT;
    v.widgets.inactive.bg_stroke = Stroke::new(1.0_f32, SURFACE);
    v.widgets.inactive.fg_stroke = Stroke::new(1.0_f32, FG);
    v.widgets.hovered.bg_fill = SURFACE;
    v.widgets.hovered.weak_bg_fill = SURFACE;
    v.widgets.hovered.bg_stroke = Stroke::new(1.0_f32, ACCENT);
    // Text (and the scrollbar handle, which uses this colour) stays at fg in
    // every state, as in the user's theme-edit app; hover is shown by the
    // surface fill and copper outline instead of a near-white flare.
    v.widgets.hovered.fg_stroke = Stroke::new(1.0_f32, FG);
    v.widgets.active.bg_fill = SELECTION_BG;
    v.widgets.active.weak_bg_fill = SURFACE;
    v.widgets.active.bg_stroke = Stroke::new(1.0_f32, ACCENT);
    v.widgets.active.fg_stroke = Stroke::new(1.0_f32, FG);
    v.widgets.open = v.widgets.active;
    for w in [
        &mut v.widgets.noninteractive,
        &mut v.widgets.inactive,
        &mut v.widgets.hovered,
        &mut v.widgets.active,
        &mut v.widgets.open,
    ] {
        w.corner_radius = egui::CornerRadius::ZERO;
        w.expansion = 0.0;
    }
    v.window_corner_radius = egui::CornerRadius::ZERO;
    v.menu_corner_radius = egui::CornerRadius::ZERO;
    v.slider_trailing_fill = true;
    s.spacing.item_spacing = Vec2::new(7.0, 5.0);
    s.spacing.button_padding = Vec2::new(8.0, 3.0);
    s.spacing.interact_size = Vec2::new(24.0, 22.0);
    s.spacing.combo_height = 280.0;
    for (kind, size) in [
        (egui::TextStyle::Body, 13.0),
        (egui::TextStyle::Button, 13.0),
        (egui::TextStyle::Small, 11.5),
        (egui::TextStyle::Heading, 17.0),
        (egui::TextStyle::Monospace, 12.0),
    ] {
        let family = if kind == egui::TextStyle::Monospace {
            egui::FontFamily::Monospace
        } else {
            egui::FontFamily::Proportional
        };
        s.text_styles.insert(kind, egui::FontId::new(size, family));
    }
    ctx.set_style_of(egui::Theme::Light, s.clone());
    ctx.set_style_of(egui::Theme::Dark, s);
    // Dark window title bar too, whatever the OS uses (Windows, macOS and
    // Wayland desktops that support it).
    ctx.send_viewport_cmd(egui::ViewportCommand::SetTheme(egui::SystemTheme::Dark));
}
/// Workspace tabs styled like the desktop bar: accent text plus a 2px accent underline when active.
/// Workspace tabs (Packages first: browse/download is the main window) and,
/// at the right, a Settings button that sets `settings_clicked`.
pub fn header(ctx: &egui::Context, workspace: &mut usize, settings_clicked: &mut bool) -> bool {
    let old = *workspace;
    egui::TopBottomPanel::top("workspaces")
        .frame(
            egui::Frame::NONE
                .fill(BG_DARK)
                .inner_margin(egui::Margin::symmetric(12, 0)),
        )
        .show_separator_line(false)
        .show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 0.0;
                // (workspace index, label): indices are stable; only the
                // display order puts Packages first.
                for (i, label) in [
                    (3, "Packages"),
                    (0, "HeadMaker"),
                    (1, "FaceMaker"),
                    (2, "Avatar Poser"),
                ] {
                    let active = *workspace == i;
                    let text = egui::RichText::new(label).color(if active { ACCENT } else { FG });
                    let response = ui.add(
                        egui::Button::new(text)
                            .frame(false)
                            .min_size(Vec2::new(0.0, 26.0)),
                    );
                    let r = response.rect;
                    let line = if active {
                        ACCENT
                    } else if response.hovered() {
                        SURFACE
                    } else {
                        Color32::TRANSPARENT
                    };
                    ui.painter().line_segment(
                        [
                            egui::pos2(r.left() + 2.0, r.bottom() - 1.0),
                            egui::pos2(r.right() - 2.0, r.bottom() - 1.0),
                        ],
                        Stroke::new(2.0_f32, line),
                    );
                    if response.clicked() {
                        *workspace = i;
                    }
                    ui.add_space(14.0);
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui
                        .add(
                            egui::Button::new(egui::RichText::new("Settings").color(FG))
                                .frame(false)
                                .min_size(Vec2::new(0.0, 26.0)),
                        )
                        .on_hover_text("Default textures, start-up tab and the settings folder")
                        .clicked()
                    {
                        *settings_clicked = true;
                    }
                });
            });
        });
    old != *workspace
}
/// Compact monospace toolbar title, like the desktop's headerbar titles.
pub fn title(text: &str) -> egui::RichText {
    egui::RichText::new(text)
        .monospace()
        .size(12.0)
        .color(MUTED)
}
pub fn section(ui: &mut egui::Ui, title: &str) {
    ui.add_space(4.0);
    ui.label(
        egui::RichText::new(title)
            .size(17.0)
            .strong()
            .color(FG_BRIGHT),
    );
    ui.add_space(4.0);
}
pub fn group_title(ui: &mut egui::Ui, title: &str) {
    ui.add_space(3.0);
    ui.label(egui::RichText::new(title).size(12.0).strong().color(MUTED));
    ui.add_space(2.0);
}
pub fn card() -> egui::Frame {
    egui::Frame::NONE.fill(BG_ALT).inner_margin(5)
}
pub fn inspector_frame() -> egui::Frame {
    egui::Frame::NONE
        .fill(BG)
        .inner_margin(egui::Margin::symmetric(9, 8))
}
pub fn toolbar_frame() -> egui::Frame {
    egui::Frame::NONE
        .fill(BG_DARK)
        .inner_margin(egui::Margin::symmetric(9, 6))
}
pub fn status_frame() -> egui::Frame {
    egui::Frame::NONE
        .fill(BG_DARK)
        .inner_margin(egui::Margin::symmetric(9, 4))
}
pub fn primary(label: &str) -> egui::Button<'_> {
    egui::Button::new(egui::RichText::new(label).strong().color(ON_ACCENT))
        .fill(ACCENT)
        .stroke(Stroke::NONE)
}

#[cfg(test)]
mod glyph_tests {
    use eframe::egui;

    /// Every non-ASCII character in a string literal in src/*.rs (written
    /// directly or as `\u{..}`), with where it is.
    fn ui_characters() -> Vec<(char, String)> {
        let src = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut found = Vec::new();
        for entry in std::fs::read_dir(src).unwrap() {
            let path = entry.unwrap().path();
            if path.extension().is_none_or(|e| e != "rs") {
                continue;
            }
            let text = std::fs::read_to_string(&path).unwrap();
            let file = path.file_name().unwrap().to_string_lossy().into_owned();
            for (n, line) in text.lines().enumerate() {
                if line.trim_start().starts_with("//") {
                    continue;
                }
                let mut in_string = false;
                let mut chars = line.chars().peekable();
                while let Some(c) = chars.next() {
                    match c {
                        '"' => in_string = !in_string,
                        '\\' if in_string => {
                            if chars.peek() == Some(&'u') {
                                chars.next();
                                let hex: String = chars
                                    .by_ref()
                                    .skip_while(|&c| c == '{')
                                    .take_while(|&c| c != '}')
                                    .collect();
                                if let Some(c) =
                                    u32::from_str_radix(&hex, 16).ok().and_then(char::from_u32)
                                {
                                    found.push((c, format!("{file}:{}", n + 1)));
                                }
                            } else {
                                chars.next();
                            }
                        }
                        c if in_string && !c.is_ascii() => {
                            found.push((c, format!("{file}:{}", n + 1)));
                        }
                        _ => {}
                    }
                }
            }
        }
        found
    }

    /// The built-in fonts have no fallback for a missing character; it is
    /// drawn as an empty box. Every symbol shown in the UI must exist.
    #[test]
    fn every_ui_character_is_in_the_bundled_fonts() {
        let ctx = egui::Context::default();
        let _ = ctx.run(Default::default(), |_| {});
        let font = egui::FontId::proportional(14.0);
        let chars = ui_characters();
        assert!(
            chars.iter().any(|(c, _)| *c == '\u{2026}'),
            "scanner found nothing"
        );
        let missing: Vec<String> = chars
            .iter()
            .filter(|(c, _)| !ctx.fonts(|f| f.has_glyph(&font, *c)))
            .map(|(c, at)| format!("U+{:04X} '{c}' at {at}", *c as u32))
            .collect();
        assert!(missing.is_empty(), "drawn as boxes: {missing:?}");
    }
}
