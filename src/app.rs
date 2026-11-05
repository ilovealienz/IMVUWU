use crate::{
    model::{presets, Document, Falloff, Selection, V},
    render::{self, Camera, Frame, Texture},
};
use eframe::egui::{self, Color32, Key, PointerButton, Pos2, Stroke, Vec2};
use std::collections::BTreeMap;
use std::path::PathBuf;

const FACEMAKER_HELP: &[&str] = &[
    "1. Choose Female or Male. Load head is optional; use a saved XMF to preview your own head.",
    "2. Expand Brows, Eyes or Mouth and adjust the sliders. Reset clears one slider; \
     New expression starts a new pose.",
    "3. Save expression XPF exports the pose for IMVU.",
    "Advanced point editing stays in FaceMaker. Choose a shape, move its points, then Done editing. \
     Save head + expression exports the XMF and XPF you must use together.",
    "Animation / timing is optional. Leave Time at zero for a held expression.",
    "HeadMaker and FaceMaker keep separate heads, textures and undo histories. \
     Save a head in HeadMaker and Load head in FaceMaker to use it here.",
    "Right-drag rotates, Shift + right-drag pans, and the wheel zooms.",
];

const HEADMAKER_HELP: &[&str] = &[
    "Choose Female or Male; each keeps its own editing session.",
    "Right-drag orbits. Shift + right-drag pans. Ctrl + right-drag zooms. \
     Middle-drag and the wheel also work.",
    "Set overall head size in IMVU Mesh Scale using the Head node. \
     Its attachment setting carries hair and accessories with the head.",
    "Original arrows is the default: select a point, then drag its arrows. \
     Direct drag also lets you pull points directly and orbit by dragging empty space.",
    "Edit protected head points unlocks the grey head vertices. \
     Edit neck points separately unlocks the neck band. Both start off.",
    "Load custom skin PNG / JPG uses a flat UV skin image, such as girl.png. \
     Texture images are packaged separately from the XMF.",
    "Choose Face.Average for the head shape, or another named morph to edit an expression.",
    "Click a visible vertex to select. Shift-click adds seed points. \
     Range controls the soft selection.",
    "X / Y / Z select a movement axis before dragging a point. \
     G restores the original move arrows / free movement. \
     Drag the white centre for free movement. Esc cancels a drag.",
    "Right-drag rotates the head. Middle-drag pans. Scroll zooms. F resets the camera. \
     Empty left drags do not rotate the head.",
    "Sculpt strength is applied when you press Apply. \
     Presets can affect a selection or the whole head.",
    "Expressions · XPF blends named facial morphs without editing the XMF. \
     Set sliders at time 0 for a held expression, or move time and set more keys for animation. \
     Ctrl+O / Ctrl+S use the current mode.",
    "XMF retains the original base mesh, materials, skin weights, UVs and unedited morphs.",
    "The female neck seam is repaired from the original head. Save the main XMF; \
     n.xmf and the extra neck index.xml are not needed. \
     Existing collar patches should not be layered over a repaired head.",
    "Loaded textures affect the preview; package them separately for IMVU.",
];

/// Separate sessions: the face editor never changes the HeadMaker document.
pub struct EditorApp {
    gpu: Option<crate::gpu::Shared>,
    use_gpu: bool,
    gpu_error: String,
    head: HeadMaker,
    face: HeadMaker,
    poser: crate::poser::AvatarPoser,
    /// Packages tab. Independent of the editors: browsing or downloading
    /// never changes an open head, expression or pose.
    browser: crate::browser::Browser,
    workspace: usize,
    closing: bool,
    allow_close: bool,
    settings: crate::settings::Settings,
    settings_open: bool,
    settings_status: String,
    /// A running avatar build: progress text and the finished result.
    avatar_job: Option<AvatarJob>,
    /// First-run launcher, shown instead of the app while the IMVU
    /// avatars are missing (the no-embedded-avatars build). It downloads
    /// products 80 and 191 straight away.
    launcher: Option<Launcher>,
    /// The shortcut waiting for a key press in Settings.
    capturing: Option<crate::shortcuts::Command>,
}
#[derive(Default)]
struct Launcher {
    /// Why the last attempt failed, one line per avatar.
    errors: Vec<String>,
}
struct AvatarJob {
    progress: std::sync::Arc<std::sync::Mutex<String>>,
    result: std::sync::mpsc::Receiver<Vec<(bool, Result<std::path::PathBuf, String>)>>,
}
impl EditorApp {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        let mut head = HeadMaker::new(cc);
        head.allow_close = true;
        let mut face = HeadMaker::new(cc);
        face.allow_close = true;
        face.expression_mode = true;
        face.status="Choose a head, move the face sliders, then Save expression. Advanced point editing is optional.".into();
        // wgpu when the window uses it (the default), else OpenGL.
        let gpu_result = match &cc.wgpu_render_state {
            Some(state) => Ok(crate::gpu::Renderer::create_wgpu(state)),
            None => cc
                .gl
                .clone()
                .ok_or("No GPU context".into())
                .and_then(crate::gpu::Renderer::create_glow),
        };
        let gpu_error = gpu_result.as_ref().err().cloned().unwrap_or_default();
        let gpu = gpu_result.ok();
        head.gpu = gpu.clone();
        face.gpu = gpu.clone();
        let mut poser = crate::poser::AvatarPoser::new();
        poser.gpu = gpu.clone();
        let use_gpu = gpu.is_some() && !std::env::args().any(|s| s == "--software-preview");
        // `--packages [product id or link]` opens the Packages tab directly.
        let args: Vec<String> = std::env::args().collect();
        let mut browser = crate::browser::Browser::default();
        let settings = crate::settings::Settings::load();
        crate::shortcuts::set_keymap(&settings.shortcuts);
        // System fonts as fallbacks for scripts the bundled fonts lack
        // (Japanese, Korean, Thai, decorative symbols...), in the background.
        crate::fonts::install_system_fallbacks(&cc.egui_ctx);
        let mut workspace = settings.start_workspace.workspace();
        if let Some(i) = args.iter().position(|a| a == "--packages") {
            workspace = 3;
            // The argument may also be a local ZIP/CHKN file or folder.
            if let Some(arg) = args.get(i + 1) {
                let path = std::path::Path::new(arg);
                if path.exists() {
                    browser.initial_open = Some(path.to_path_buf());
                    browser.initial_select = args
                        .iter()
                        .position(|a| a == "--select")
                        .and_then(|j| args.get(j + 1).cloned());
                } else {
                    browser.initial_lookup = crate::imvu::parse_product_input(arg);
                }
            }
        }
        let missing = [true, false]
            .into_iter()
            .any(|female| !crate::avatars::available(female));
        let mut app = Self {
            gpu,
            use_gpu,
            gpu_error,
            head,
            face,
            poser,
            browser,
            workspace,
            closing: false,
            allow_close: false,
            settings,
            // `--open-settings` opens the Settings window at launch.
            settings_open: std::env::args().any(|a| a == "--open-settings"),
            settings_status: String::new(),
            avatar_job: None,
            launcher: missing.then(Launcher::default),
            capturing: None,
        };
        if missing {
            app.start_avatar_build(&cc.egui_ctx, None);
        }
        app
    }
    /// The first-run launcher: downloads the IMVU avatars this build does
    /// not include, then opens the app.
    fn launcher_screen(&mut self, ctx: &egui::Context) {
        let mut request: Option<Option<(bool, std::path::PathBuf)>> = None;
        let mut skip = false;
        let busy = self.avatar_job.is_some();
        let errors = self
            .launcher
            .as_ref()
            .map(|l| l.errors.clone())
            .unwrap_or_default();
        egui::CentralPanel::default().show(ctx, |ui| {
            ui.vertical_centered(|ui| {
                ui.add_space((ui.available_height() * 0.22).max(16.0));
                ui.set_max_width(460.0);
                ui.label(
                    egui::RichText::new("First-time setup")
                        .size(20.0)
                        .color(crate::theme::FG_BRIGHT),
                );
                ui.add_space(6.0);
                ui.label(
                    egui::RichText::new(
                        "This copy doesn't include IMVU's base avatars. They are downloaded \
                         once from IMVU's public product data (no login): the female avatar \
                         (product 80), its default skin (1133635) and the male avatar (191).",
                    )
                    .color(crate::theme::MUTED),
                );
                ui.add_space(12.0);
                egui::Grid::new("launcher_avatars")
                    .num_columns(2)
                    .spacing([12.0, 6.0])
                    .show(ui, |ui| {
                        for (female, label) in [(true, "Female avatar"), (false, "Male avatar")] {
                            ui.label(label);
                            let ready = crate::avatars::available(female);
                            ui.label(if ready {
                                egui::RichText::new("Ready").color(crate::theme::ACCENT)
                            } else if busy {
                                egui::RichText::new("Downloading…").color(crate::theme::MUTED)
                            } else {
                                egui::RichText::new("Missing").color(crate::theme::RED)
                            });
                            ui.end_row();
                        }
                    });
                ui.add_space(10.0);
                if let Some(job) = &self.avatar_job {
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label(
                            egui::RichText::new(
                                job.progress.lock().map(|p| p.clone()).unwrap_or_default(),
                            )
                            .small(),
                        );
                    });
                }
                for e in &errors {
                    ui.label(egui::RichText::new(e).small().color(crate::theme::RED));
                }
                ui.add_space(10.0);
                ui.horizontal(|ui| {
                    if !busy && ui.add(crate::theme::primary("Retry download")).clicked() {
                        request = Some(None);
                    }
                    for (female, label) in [(true, "Female from file…"), (false, "Male from file…")]
                    {
                        if ui
                            .add_enabled(!busy, egui::Button::new(label))
                            .on_hover_text(
                                "Build from a ZIP or CHKN of the product you already have",
                            )
                            .clicked()
                        {
                            if let Some(path) = rfd::FileDialog::new()
                                .add_filter("IMVU package", &["zip", "chkn"])
                                .pick_file()
                            {
                                request = Some(Some((female, path)));
                            }
                        }
                    }
                });
                ui.add_space(4.0);
                if ui
                    .add_enabled(!busy, egui::Button::new("Continue without avatars"))
                    .on_hover_text(
                        "The editors work; Avatar Poser uses a plain preview mesh. \
                         You can build the avatars later in Settings.",
                    )
                    .clicked()
                {
                    skip = true;
                }
            });
        });
        if skip {
            self.launcher = None;
        }
        if let Some(request) = request {
            self.start_avatar_build(ctx, request);
        }
    }
    /// The SHORTCUT KEYS section of Settings.
    fn shortcut_settings(&mut self, ui: &mut egui::Ui) {
        use crate::shortcuts::{binding, Command};
        crate::theme::group_title(ui, "SHORTCUT KEYS");
        ui.label(
            egui::RichText::new(
                "Click a key, then press the new key or combination (Esc cancels). \
                 Keys don't work while typing in a text field.",
            )
            .small()
            .weak(),
        );
        let mut reset = None;
        egui::Grid::new("shortcut_keys")
            .num_columns(3)
            .spacing([10.0, 4.0])
            .show(ui, |ui| {
                for command in Command::ALL {
                    ui.label(command.label());
                    let current = binding(command);
                    let waiting = self.capturing == Some(command);
                    let text = if waiting {
                        "Press a key…".to_string()
                    } else {
                        current.text()
                    };
                    let clash = Command::ALL
                        .iter()
                        .find(|c| **c != command && binding(**c) == current);
                    let mut label = egui::RichText::new(text);
                    if clash.is_some() {
                        label = label.color(crate::theme::RED);
                    }
                    let mut button = ui.add(
                        egui::Button::new(label)
                            .selected(waiting)
                            .min_size(egui::vec2(120.0, 0.0)),
                    );
                    if let Some(other) = clash {
                        button = button.on_hover_text(format!("Also used by {}", other.label()));
                    }
                    if button.clicked() {
                        self.capturing = if waiting { None } else { Some(command) };
                    }
                    if ui
                        .add_enabled(
                            current != command.default_binding(),
                            egui::Button::new("Default").small(),
                        )
                        .on_hover_text(command.default_binding().text())
                        .clicked()
                    {
                        reset = Some(command);
                    }
                    ui.end_row();
                }
            });
        if let Some(command) = reset {
            self.set_shortcut(command, command.default_binding());
        }
    }
    /// While a shortcut is waiting in Settings, takes the next key press.
    fn capture_shortcut(&mut self, ctx: &egui::Context) {
        let Some(command) = self.capturing else {
            return;
        };
        let pressed = ctx.input_mut(|i| {
            let index = i
                .events
                .iter()
                .position(|e| matches!(e, egui::Event::Key { pressed: true, .. }))?;
            match i.events.remove(index) {
                egui::Event::Key { key, modifiers, .. } => Some((key, modifiers)),
                _ => None,
            }
        });
        let Some((key, modifiers)) = pressed else {
            return;
        };
        self.capturing = None;
        if key == Key::Escape && !modifiers.any() {
            return;
        }
        self.set_shortcut(
            command,
            crate::shortcuts::Binding::from_event(key, modifiers),
        );
    }
    fn set_shortcut(&mut self, command: crate::shortcuts::Command, b: crate::shortcuts::Binding) {
        let map = &mut self.settings.shortcuts;
        map.retain(|(c, _)| *c != command);
        if b != command.default_binding() {
            map.push((command, b));
        }
        crate::shortcuts::set_keymap(map);
        self.settings_status = match self.settings.save() {
            Ok(()) => format!("{}: {}", command.label(), b.text()),
            Err(e) => format!("Could not save settings: {e}"),
        };
    }
    /// Collects a finished avatar build. Returns true when one finished.
    fn poll_avatar_job(&mut self) -> bool {
        let Some(results) = self
            .avatar_job
            .as_ref()
            .and_then(|j| j.result.try_recv().ok())
        else {
            return false;
        };
        self.avatar_job = None;
        let mut lines = Vec::new();
        let mut errors = Vec::new();
        for (female, result) in results {
            let who = if female { "Female" } else { "Male" };
            match result {
                Ok(path) => lines.push(format!("{who} avatar built in {}.", path.display())),
                Err(e) => {
                    errors.push(format!("{who} avatar not built: {e}"));
                    lines.push(format!("{who} avatar not built: {e}"));
                }
            }
        }
        self.settings_status = lines.join(" ");
        self.poser.reload_avatars();
        self.head.refresh_default_textures();
        self.face.refresh_default_textures();
        if let Some(launcher) = &mut self.launcher {
            let all = [true, false].into_iter().all(crate::avatars::available);
            if all {
                self.launcher = None;
            } else {
                launcher.errors = errors;
            }
        }
        true
    }
    fn settings_window(&mut self, ctx: &egui::Context) {
        use crate::settings::{self, Slot, StartWorkspace};
        let mut open = self.settings_open;
        let mut textures_changed = false;
        self.capture_shortcut(ctx);
        // None = download both; Some((female, path)) = one from a file.
        let mut avatar_request: Option<Option<(bool, std::path::PathBuf)>> = None;
        egui::Window::new("Settings")
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .default_width(460.0)
            // Scrolls on small windows now that it has shortcut keys too.
            .vscroll(true)
            .max_height(ctx.screen_rect().height() - 80.0)
            .show(ctx, |ui| {
                let Some(root) = settings::app_dir() else {
                    ui.label("No app data folder (APPDATA or HOME is not set), so settings cannot be stored.");
                    return;
                };
                crate::theme::group_title(ui, "START-UP");
                ui.horizontal(|ui| {
                    ui.label("Open at launch");
                    let before = self.settings.start_workspace;
                    for w in StartWorkspace::ALL {
                        ui.selectable_value(&mut self.settings.start_workspace, w, w.label());
                    }
                    if before != self.settings.start_workspace {
                        self.settings_status = match self.settings.save() {
                            Ok(()) => format!(
                                "{} will open at launch.",
                                self.settings.start_workspace.label()
                            ),
                            Err(e) => format!("Could not save settings: {e}"),
                        };
                    }
                });
                ui.add_space(6.0);
                crate::theme::group_title(ui, "DEFAULT TEXTURES");
                ui.label(
                    egui::RichText::new(
                        "Your own images to use instead of the built-in ones: at launch, on \
                         Reset head and on Default skin. A skin loaded during a session still \
                         only affects that session. Eyelash and eyebrow images should have \
                         transparency.",
                    )
                    .small()
                    .weak(),
                );
                egui::Grid::new("default_textures")
                    .num_columns(4)
                    .spacing(egui::vec2(10.0, 4.0))
                    .show(ui, |ui| {
                        for (female, gender) in [(true, "Female"), (false, "Male")] {
                            for slot in Slot::ALL {
                                ui.label(format!("{gender} {}", slot.label().to_lowercase()));
                                let current = settings::custom_texture_path(&root, female, slot);
                                ui.label(
                                    egui::RichText::new(match &current {
                                        Some(p) => p
                                            .file_name()
                                            .unwrap_or_default()
                                            .to_string_lossy()
                                            .into_owned(),
                                        None => "Built-in".into(),
                                    })
                                    .small()
                                    .color(
                                        if current.is_some() {
                                            crate::theme::ACCENT
                                        } else {
                                            crate::theme::MUTED
                                        },
                                    ),
                                );
                                if ui.small_button("Choose…").clicked() {
                                    if let Some(path) = rfd::FileDialog::new()
                                        .add_filter(
                                            "Images",
                                            &["png", "jpg", "jpeg", "tga", "bmp", "gif"],
                                        )
                                        .pick_file()
                                    {
                                        self.settings_status = match settings::set_custom_texture(
                                            &root, female, slot, &path,
                                        ) {
                                            Ok(()) => {
                                                textures_changed = true;
                                                format!(
                                                    "{gender} {} default set to {}.",
                                                    slot.label().to_lowercase(),
                                                    path.display()
                                                )
                                            }
                                            Err(e) => format!("Not changed: {e}"),
                                        };
                                    }
                                }
                                if ui
                                    .add_enabled(
                                        current.is_some(),
                                        egui::Button::new("Built-in").small(),
                                    )
                                    .on_hover_text("Go back to the built-in texture")
                                    .clicked()
                                {
                                    self.settings_status =
                                        match settings::clear_custom_texture(&root, female, slot) {
                                            Ok(()) => {
                                                textures_changed = true;
                                                format!(
                                                    "{gender} {} back to built-in.",
                                                    slot.label().to_lowercase()
                                                )
                                            }
                                            Err(e) => format!("Could not reset: {e}"),
                                        };
                                }
                                ui.end_row();
                            }
                        }
                    });
                ui.add_space(6.0);
                crate::theme::group_title(ui, "IMVU AVATARS");
                ui.label(
                    egui::RichText::new(
                        "The poser and the default head materials use IMVU's base avatars: \
                         product 80 (female) and 191 (male). They are not distributed with \
                         this app; build them here from IMVU's public product data (no \
                         login) or from a ZIP/CHKN copy you already have.",
                    )
                    .small()
                    .weak(),
                );
                for (female, label) in [(true, "Female (80)"), (false, "Male (191)")] {
                    ui.horizontal(|ui| {
                        ui.label(label);
                        ui.label(
                            egui::RichText::new(crate::avatars::describe(female))
                                .small()
                                .color(crate::theme::MUTED),
                        );
                    });
                }
                let busy = self.avatar_job.is_some();
                ui.horizontal(|ui| {
                    if ui
                        .add_enabled(!busy, crate::theme::primary("Download from IMVU"))
                        .on_hover_text("Downloads products 80 and 191 and installs them")
                        .clicked()
                    {
                        avatar_request = Some(None);
                    }
                    for (female, label) in [(true, "Female from file…"), (false, "Male from file…")]
                    {
                        if ui
                            .add_enabled(!busy, egui::Button::new(label))
                            .on_hover_text("Build from a local ZIP or CHKN of the product")
                            .clicked()
                        {
                            if let Some(path) = rfd::FileDialog::new()
                                .add_filter("IMVU package", &["zip", "chkn"])
                                .pick_file()
                            {
                                avatar_request = Some(Some((female, path)));
                            }
                        }
                    }
                });
                if let Some(job) = &self.avatar_job {
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label(job.progress.lock().map(|p| p.clone()).unwrap_or_default());
                    });
                }
                ui.add_space(6.0);
                self.shortcut_settings(ui);
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    if ui.button("Open settings folder").clicked() {
                        let _ = std::fs::create_dir_all(&root);
                        crate::settings::open_folder(&root);
                    }
                    ui.label(
                        egui::RichText::new(root.display().to_string())
                            .small()
                            .weak(),
                    );
                });
                if !self.settings_status.is_empty() {
                    ui.label(egui::RichText::new(&self.settings_status).small());
                }
            });
        self.settings_open = open;
        if let Some(request) = avatar_request {
            self.start_avatar_build(ctx, request);
        }
        self.poll_avatar_job();
        if textures_changed {
            self.head.refresh_default_textures();
            self.face.refresh_default_textures();
        }
    }
    /// Builds the IMVU avatars on a background thread.
    fn start_avatar_build(
        &mut self,
        ctx: &egui::Context,
        request: Option<(bool, std::path::PathBuf)>,
    ) {
        let Some(root) = crate::settings::app_dir() else {
            self.settings_status =
                "No app data folder (APPDATA or HOME is not set), so avatars cannot be stored."
                    .into();
            return;
        };
        let progress = std::sync::Arc::new(std::sync::Mutex::new("Starting…".to_string()));
        let (sender, receiver) = std::sync::mpsc::channel();
        let (shared, ctx) = (progress.clone(), ctx.clone());
        std::thread::spawn(move || {
            let report = |text: String| {
                if let Ok(mut p) = shared.lock() {
                    *p = text;
                }
                ctx.request_repaint();
            };
            let results = match request {
                Some((female, path)) => vec![(
                    female,
                    crate::avatars::build_from_local(&root, female, &path),
                )],
                None => {
                    let client = crate::imvu::Client::default();
                    [true, false]
                        .into_iter()
                        .map(|female| {
                            let who = if female { "Female" } else { "Male" };
                            let progress = |t: String| report(format!("{who}: {t}"));
                            (
                                female,
                                crate::avatars::build_from_imvu(&client, &root, female, &progress),
                            )
                        })
                        .collect()
                }
            };
            let _ = sender.send(results);
            ctx.request_repaint();
        });
        self.avatar_job = Some(AvatarJob {
            progress,
            result: receiver,
        });
    }
    fn dirty(&self) -> bool {
        self.poser.dirty()
            || [&self.head, &self.face]
                .iter()
                .any(|e| e.docs.iter().any(|d| d.dirty) || e.expressions.iter().any(|x| x.dirty()))
    }
    fn save_all(&mut self) -> bool {
        if self.face.editing_points {
            self.face.preview_point_expression();
        }
        for editor in [&mut self.head, &mut self.face] {
            let previous = editor.active;
            for a in 0..2 {
                editor.active = a;
                if editor.docs[a].dirty && !editor.save() {
                    editor.active = previous;
                    return false;
                }
                if editor.expressions[a].dirty() && !editor.save_expression() {
                    editor.active = previous;
                    return false;
                }
            }
            editor.active = previous;
        }
        if self.poser.dirty() && !self.poser.save_project() {
            return false;
        }
        true
    }
}
impl eframe::App for EditorApp {
    fn on_exit(&mut self, _: Option<&eframe::glow::Context>) {
        if let Some(gpu) = &self.gpu {
            if let Ok(mut renderer) = gpu.lock() {
                renderer.destroy();
            }
        }
    }

    fn update(&mut self, ctx: &egui::Context, frame: &mut eframe::Frame) {
        if std::env::var_os("IMVUWU_DEBUG_REPAINT").is_some() {
            eprintln!(
                "frame {}: {:?}",
                ctx.cumulative_pass_nr(),
                ctx.repaint_causes()
            );
        }
        if ctx.input(|i| i.viewport().close_requested()) && !self.allow_close && self.dirty() {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            self.closing = true;
        }
        if self.launcher.is_some() {
            self.poll_avatar_job();
            if self.launcher.is_some() {
                self.launcher_screen(ctx);
                return;
            }
        }
        let mut settings_clicked = false;
        if crate::theme::header(ctx, &mut self.workspace, &mut settings_clicked) {
            self.poser.suspend();
            self.head.drag = None;
            self.face.drag = None;
            for e in &mut self.face.expressions {
                e.playing = false;
            }
            self.head.redraw = true;
            self.face.redraw = true;
        }
        if settings_clicked {
            self.settings_open = true;
        }
        self.settings_window(ctx);
        if self.use_gpu {
            let failure = self
                .gpu
                .as_ref()
                .and_then(|g| g.lock().ok().and_then(|g| g.failure()));
            if let Some(error) = failure {
                self.use_gpu = false;
                self.gpu_error = format!("GPU preview unavailable: {error}. Using software.");
                self.head.redraw = true;
                self.face.redraw = true;
                self.poser.invalidate();
            }
        }
        egui::TopBottomPanel::bottom("renderer_choice").show(ctx, |ui| {
            ui.horizontal(|ui| {
                if ui
                    .add_enabled(
                        self.gpu.is_some(),
                        egui::Checkbox::new(&mut self.use_gpu, "GPU preview"),
                    )
                    .changed()
                {
                    self.head.redraw = true;
                    self.face.redraw = true;
                    self.poser.invalidate();
                }
                if self.use_gpu {
                    if let Some(gpu) = &self.gpu {
                        if let Ok(r) = gpu.lock() {
                            ui.label(egui::RichText::new(r.label()).small().weak());
                        }
                    }
                } else {
                    ui.label(
                        egui::RichText::new(if self.gpu_error.is_empty() {
                            "Software preview"
                        } else {
                            &self.gpu_error
                        })
                        .small()
                        .weak(),
                    );
                }
            });
        });
        self.head.use_gpu = self.use_gpu;
        self.face.use_gpu = self.use_gpu;
        self.poser.use_gpu = self.use_gpu;
        match self.workspace {
            1 => {
                self.face.update(ctx, frame);
                if std::mem::take(&mut self.face.wants_headmaker_head) {
                    self.face.adopt_head_from(&self.head);
                }
            }
            2 => self.poser.update(ctx),
            3 => self.browser.update(ctx),
            _ => self.head.update(ctx, frame),
        }
        if self.closing {
            egui::Window::new("Unsaved work")
                .anchor(egui::Align2::CENTER_CENTER, Vec2::ZERO)
                .collapsible(false)
                .resizable(false)
                .show(ctx, |ui| {
                    ui.label("HeadMaker, FaceMaker or Avatar Poser has unsaved work.");
                    ui.horizontal(|ui| {
                        if ui.button("Save all and close").clicked() && self.save_all() {
                            self.allow_close = true;
                            self.closing = false;
                            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                        }
                        if ui.button("Close without saving").clicked() {
                            self.allow_close = true;
                            self.closing = false;
                            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                        }
                        if ui.button("Cancel").clicked() {
                            self.closing = false;
                        }
                    });
                });
        }
    }
}

/// (gender, point spacing bits, base-mesh hash) for the thinned-point cache.
type ThinKey = (usize, u32, u64);
#[derive(Clone, Copy, PartialEq, Debug)]
enum Tool {
    Screen,
    X,
    Y,
    Z,
}
#[derive(Clone, Copy, PartialEq)]
enum Controls {
    Original,
    Direct,
}
impl Tool {
    fn axis(self) -> V {
        match self {
            Self::X => V(1.0, 0.0, 0.0),
            Self::Y => V(0.0, 1.0, 0.0),
            Self::Z => V(0.0, 0.0, 1.0),
            Self::Screen => V::default(),
        }
    }
    fn color(self) -> Color32 {
        match self {
            Self::X => Color32::from_rgb(255, 85, 85),
            Self::Y => Color32::from_rgb(90, 235, 135),
            Self::Z => Color32::from_rgb(90, 155, 255),
            Self::Screen => Color32::WHITE,
        }
    }
}
#[derive(Clone, Copy, PartialEq)]
enum Sculpt {
    Restore,
    Relax,
    Tense,
    Shrink,
    Grow,
    Preset,
}
#[derive(Clone, Copy)]
enum Pending {
    Reset,
    Open,
    OpenAs(usize),
    ExpressionOpen,
    ExpressionReset,
    Exit,
}
enum Drag {
    Orbit,
    Pan,
    Zoom,
    Move {
        before: Vec<V>,
        delta: Vec2,
        tool: Tool,
        axis_screen: Vec2,
        world_axis: V,
    },
}
pub struct HeadMaker {
    docs: [Document; 2],
    selections: [Selection; 2],
    textures: [Vec<Texture>; 2],
    active: usize,
    camera: Camera,
    hidden_parts: [Vec<bool>; 2],
    frame: Option<Frame>,
    preview: Option<egui::TextureHandle>,
    redraw: bool,
    vertices: bool,
    wire: bool,
    textured: bool,
    xray: bool,
    tool: Tool,
    gpu: Option<crate::gpu::Shared>,
    gpu_scene: Option<std::sync::Arc<crate::gpu::Scene>>,
    use_gpu: bool,
    point_size: f32,
    /// Thinning of point markers, as a fraction of the head's width: 0 shows
    /// every point; otherwise `model::thin_points` keeps points at least that
    /// far apart on the mesh, so the set is the same at any zoom. Thinned
    /// points are neither drawn nor picked; selected/affected ones always show.
    point_spacing: f32,
    /// `thin_points` result for (gender, spacing, mesh key).
    thinned: Option<(ThinKey, std::sync::Arc<Vec<bool>>)>,
    preview_budget: render::PreviewBudget,
    controls: Controls,
    keyboard_axis: Option<Tool>,
    skin_paths: [Option<PathBuf>; 2],
    /// Per gender, head texture slots 0-3 replaced by a skin/eyes/... loaded
    /// this session. Those slots keep their image when defaults change.
    session_textures: [[bool; 4]; 2],
    /// FaceMaker asked to copy HeadMaker's current head (EditorApp does it).
    wants_headmaker_head: bool,
    camera_aligned_gizmo: bool,
    overlays_visible: bool,
    neck_offer: bool,
    choose_import: bool,
    expression_mode: bool,
    editing_points: bool,
    point_target: String,
    expressions: [crate::xpf::Editor; 2],
    custom_name: String,
    point_expression: Option<(usize, String)>,
    point_part: Option<usize>,
    point_side: i32,
    drag: Option<Drag>,
    delta: V,
    sculpt: Sculpt,
    amount: f32,
    preset: usize,
    presets: Vec<(String, Vec<V>)>,
    status: String,
    paths: [Option<PathBuf>; 2],
    pending: Option<Pending>,
    allow_close: bool,
    help: bool,
}
impl HeadMaker {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        crate::theme::install(&cc.egui_ctx);
        Self {
            docs: [Document::template(true), Document::template(false)],
            selections: [Selection::new(956), Selection::new(956)],
            textures: [
                crate::settings::default_textures(true).0,
                crate::settings::default_textures(false).0,
            ],
            active: 0,
            camera: Camera::default(),
            hidden_parts: [vec![false; 4], vec![false; 4]],
            frame: None,
            preview: None,
            redraw: true,
            vertices: true,
            wire: false,
            textured: true,
            xray: false,
            tool: Tool::Screen,
            gpu: None,
            gpu_scene: None,
            use_gpu: false,
            point_size: 3.0,
            // `--points fewer|fewest` starts with thinned points (screenshots).
            point_spacing: match std::env::args()
                .skip_while(|a| a != "--points")
                .nth(1)
                .as_deref()
            {
                Some("fewer") => 0.04,
                Some("fewest") => 0.08,
                _ => 0.0,
            },
            thinned: None,
            preview_budget: Default::default(),
            controls: Controls::Original,
            keyboard_axis: None,
            skin_paths: [None, None],
            session_textures: [[false; 4]; 2],
            wants_headmaker_head: false,
            camera_aligned_gizmo: false,
            overlays_visible: true,
            neck_offer: false,
            choose_import: false,
            expression_mode: false,
            editing_points: false,
            point_target: "mouth.Open.Exclusive".into(),
            expressions: [crate::xpf::Editor::default(), crate::xpf::Editor::default()],
            custom_name: "custom.Expression01.Exclusive".into(),
            point_expression: None,
            point_part: None,
            point_side: 0,
            drag: None,
            delta: V::default(),
            sculpt: Sculpt::Relax,
            amount: 0.25,
            preset: 0,
            presets: presets(),
            status: "Click a blue point, then drag a coloured arrow. Right-drag rotates the head."
                .into(),
            paths: [None, None],
            pending: None,
            allow_close: false,
            help: false,
        }
    }
    fn recalc(&mut self) {
        let editing = self
            .point_expression
            .as_ref()
            .is_some_and(|(a, n)| *a == self.active && n == &self.docs[self.active].active);
        let doc = &self.docs[self.active];
        let s = &mut self.selections[self.active];
        for (i, allowed) in s.editable.iter_mut().enumerate() {
            let p = doc.positions()[i];
            *allowed = !editing
                || (self.point_part.is_none_or(|part| {
                    doc.parts
                        .get(part)
                        .is_some_and(|part| i >= part.offset && i < part.offset + part.base.len())
                }) && (self.point_side == 0
                    || if self.point_side < 0 {
                        p.0 <= 0.01
                    } else {
                        p.0 >= -0.01
                    }));
        }
        self.docs[self.active].neck_editing = self.selections[self.active].allow_neck;
        self.selections[self.active].update(&self.docs[self.active]);
        self.redraw = true;
    }
    fn record_change(&mut self, ps: Vec<V>) {
        if ps != self.docs[self.active].positions() {
            self.docs[self.active].checkpoint();
            self.docs[self.active].change(ps);
            self.recalc();
        }
    }
    fn save(&mut self) -> bool {
        let name = if self.docs[self.active].female {
            "FemaleHead.xmf"
        } else {
            "MaleHead.xmf"
        };
        let mut dialog = rfd::FileDialog::new()
            .add_filter("Cal3D head mesh", &["xmf"])
            .set_file_name(name);
        if let Some(p) = &self.paths[self.active] {
            if let Some(parent) = p.parent() {
                dialog = dialog.set_directory(parent);
            }
        }
        let Some(mut path) = dialog.save_file() else {
            return false;
        };
        if path.extension().is_none() {
            path.set_extension("xmf");
        }
        let result = (|| -> Result<(), String> {
            let xml = self.docs[self.active].export()?;
            // Validate our output before touching the destination.
            Document::parse(&xml, self.active == 0)?;
            let tmp = path.with_extension(format!("xmf.{}.tmp", std::process::id()));
            use std::io::Write;
            let mut file = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&tmp)
                .map_err(|e| e.to_string())?;
            let write = (|| -> Result<(), String> {
                file.write_all(xml.as_bytes()).map_err(|e| e.to_string())?;
                file.sync_all().map_err(|e| e.to_string())?;
                drop(file);
                std::fs::rename(&tmp, &path).map_err(|e| e.to_string())?;
                Ok(())
            })();
            if write.is_err() {
                let _ = std::fs::remove_file(&tmp);
            }
            write
        })();
        match result {
            Ok(()) => {
                let d = &mut self.docs[self.active];
                d.saved_revision = d.revision;
                d.dirty = false;
                self.status = format!(
                    "Saved {} — neck seam protected in the main mesh.",
                    path.display()
                );
                self.paths[self.active] = Some(path);
                true
            }
            Err(e) => {
                self.status = format!("Could not save: {e}");
                false
            }
        }
    }
    fn request(&mut self, p: Pending, ctx: &egui::Context) {
        if matches!(p, Pending::Open) {
            self.choose_import = true;
            return;
        }
        if let Pending::OpenAs(target) = p {
            self.active = target;
            self.drag = None;
            self.camera = Camera::default();
            self.recalc();
        }
        let dirty = match p {
            Pending::Exit => {
                self.docs.iter().any(|d| d.dirty) || self.expressions.iter().any(|e| e.dirty())
            }
            Pending::ExpressionOpen | Pending::ExpressionReset => {
                self.expressions[self.active].dirty()
            }
            _ => self.docs[self.active].dirty,
        };
        if dirty {
            self.pending = Some(p)
        } else {
            self.perform(p, ctx)
        }
    }
    fn perform(&mut self, p: Pending, ctx: &egui::Context) {
        match p {
            Pending::ExpressionReset => {
                self.expressions[self.active].checkpoint();
                self.expressions[self.active].animation = crate::xpf::Animation::default();
                self.expressions[self.active].time = 0.0;
                self.expressions[self.active].playing = false;
                self.redraw = true;
            }
            Pending::ExpressionOpen => {
                if let Some(path) = rfd::FileDialog::new()
                    .add_filter("Facial expressions", &["xpf", "cpf"])
                    .pick_file()
                {
                    let result = (|| -> Result<crate::xpf::Animation, String> {
                        if std::fs::metadata(&path).map_err(|e| e.to_string())?.len()
                            > 8 * 1024 * 1024
                        {
                            return Err("Expression exceeds 8 MB".into());
                        }
                        let bytes = std::fs::read(&path).map_err(|e| e.to_string())?;
                        crate::xpf::Animation::load(&bytes)
                    })();
                    match result {
                        Ok(animation) => {
                            let mut e = crate::xpf::Editor::default();
                            e.animation = animation;
                            e.saved();
                            e.path = Some(path.clone());
                            self.expressions[self.active] = e;
                            self.editing_points = false;
                            self.redraw = true;
                            self.status = format!("Opened expression {}", path.display());
                        }
                        Err(e) => self.status = format!("Could not open expression: {e}"),
                    }
                }
            }
            Pending::Exit => {
                self.allow_close = true;
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            }
            Pending::Reset => {
                self.point_expression = None;
                self.docs[self.active] = Document::template(self.active == 0);
                self.selections[self.active] = Selection::new(956);
                self.textures[self.active] = crate::settings::default_textures(self.active == 0).0;
                self.session_textures[self.active] = [false; 4];
                self.hidden_parts[self.active] = vec![false; 4];
                self.paths[self.active] = None;
                self.camera = Camera::default();
                self.recalc();
                self.status = "Original head restored.".into();
            }
            Pending::Open => {
                self.choose_import = true;
            }
            Pending::OpenAs(target) => {
                self.active = target;
                if let Some(path) = rfd::FileDialog::new()
                    .add_filter("Cal3D head mesh", &["xmf"])
                    .pick_file()
                {
                    let result = (|| -> Result<Document, String> {
                        if std::fs::metadata(&path).map_err(|e| e.to_string())?.len()
                            > 32 * 1024 * 1024
                        {
                            return Err("XMF exceeds 32 MB".into());
                        }
                        let source = std::fs::read(&path).map_err(|e| e.to_string())?;
                        let doc = Document::load(&source, self.active == 0)?;
                        Ok(doc)
                    })();
                    match result {
                        Ok(doc) => {
                            self.neck_offer = doc.needs_neck_repair();
                            let n = doc.positions().len();
                            let part_count = doc.parts.len();
                            self.point_expression = None;
                            self.docs[self.active] = doc;
                            self.selections[self.active] = Selection::new(n);
                            self.hidden_parts[self.active] = vec![false; part_count];
                            self.paths[self.active] = Some(path.clone());
                            self.overlays_visible = true;
                            self.recalc();
                            self.status = format!(
                                "Opened {} — {} parts, {} vertices",
                                path.display(),
                                part_count,
                                n
                            );
                        }
                        Err(e) => self.status = format!("Could not open mesh: {e}"),
                    }
                }
            }
        }
    }
    fn save_expression(&mut self) -> bool {
        let xml = match self.expressions[self.active].animation.export() {
            Ok(x) => x,
            Err(e) => {
                self.status = e;
                return false;
            }
        };
        let mut dialog = rfd::FileDialog::new()
            .add_filter("Cal3D facial expression", &["xpf"])
            .set_file_name("facialExpression.xpf");
        if let Some(p) = &self.expressions[self.active].path {
            if let Some(parent) = p.parent() {
                dialog = dialog.set_directory(parent);
            }
        }
        let Some(mut path) = dialog.save_file() else {
            return false;
        };
        if path.extension().is_none() {
            path.set_extension("xpf");
        }
        let result = (|| -> Result<(), String> {
            crate::xpf::Animation::load(xml.as_bytes())?;
            let temp = path.with_extension(format!("xpf.{}.tmp", std::process::id()));
            use std::io::Write;
            let mut file = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&temp)
                .map_err(|e| e.to_string())?;
            let result = (|| -> Result<(), String> {
                file.write_all(xml.as_bytes()).map_err(|e| e.to_string())?;
                file.sync_all().map_err(|e| e.to_string())?;
                drop(file);
                std::fs::rename(&temp, &path).map_err(|e| e.to_string())?;
                Ok(())
            })();
            if result.is_err() {
                let _ = std::fs::remove_file(&temp);
            }
            result
        })();
        match result {
            Ok(()) => {
                let e = &mut self.expressions[self.active];
                e.path = Some(path.clone());
                e.saved();
                self.status = format!("Saved expression {}", path.display());
                true
            }
            Err(e) => {
                self.status = format!("Could not save XPF: {e}");
                false
            }
        }
    }
    /// FaceMaker: replace this gender's preview head (and skin) with the one
    /// currently open in HeadMaker, including unsaved edits. HeadMaker is
    /// only read. Refused while FaceMaker has unsaved point edits.
    fn adopt_head_from(&mut self, source: &HeadMaker) {
        let a = self.active;
        if self.docs[a].dirty || self.editing_points {
            self.status = "This head has unsaved point edits. Save head + expression or \
                           finish editing first, then use the HeadMaker head."
                .into();
            return;
        }
        let female = a == 0;
        let doc = source.docs[a]
            .export()
            .and_then(|xml| Document::load(xml.as_bytes(), female));
        match doc {
            Ok(doc) => {
                let n = doc.positions().len();
                let parts = doc.parts.len();
                self.point_expression = None;
                self.docs[a] = doc;
                self.selections[a] = Selection::new(n);
                self.hidden_parts[a] = vec![false; parts];
                self.paths[a] = source.paths[a].clone();
                self.textures[a] = source.textures[a]
                    .iter()
                    .map(|t| Texture::shared(t.pixels.clone()))
                    .collect();
                self.skin_paths[a] = source.skin_paths[a].clone();
                self.session_textures[a] = source.session_textures[a];
                self.textured = source.textured;
                self.camera = Camera::default();
                self.recalc();
                self.status = format!(
                    "Using HeadMaker's {} head{} — {n} vertices. HeadMaker is unchanged.",
                    if female { "female" } else { "male" },
                    if source.docs[a].dirty {
                        " (with its unsaved edits)"
                    } else {
                        ""
                    }
                );
            }
            Err(e) => self.status = format!("Could not use HeadMaker's head: {e}"),
        }
    }
    /// After the default textures change in Settings: update every slot
    /// that is still showing a default. Skins loaded this session stay.
    fn refresh_default_textures(&mut self) {
        for (a, female) in [(0, true), (1, false)] {
            let (defaults, _) = crate::settings::default_textures(female);
            for (slot, texture) in defaults.into_iter().enumerate().take(4) {
                if !self.session_textures[a][slot] && slot < self.textures[a].len() {
                    self.textures[a][slot] = texture;
                }
            }
        }
        self.redraw = true;
    }
    fn undo_active(&mut self, redo: bool) {
        if self.expression_mode && !self.editing_points {
            self.expressions[self.active].undo(redo);
            self.redraw = true;
        } else {
            self.docs[self.active].undo(redo);
            self.recalc();
        }
    }
    fn edit_expression_points(&mut self, name: String) {
        self.docs[self.active].active = name.clone();
        self.point_expression = Some((self.active, name));
        self.editing_points = true;
        self.drag = None;
        self.overlays_visible = true;
        let s = &mut self.selections[self.active];
        s.seeds.clear();
        s.hidden.fill(false);
        s.radius = 0.0;
        s.symmetric = false;
        s.critical = false;
        s.allow_neck = false;
        s.single_part = true;
        self.point_part = None;
        self.point_side = 0;
        self.hidden_parts[self.active].fill(false);
        self.recalc();
        self.status="Select a blue point and drag an arrow. Click Done editing to return to your expression.".into();
    }
    fn preview_point_expression(&mut self) {
        if let Some((a, name)) = &self.point_expression {
            if *a == self.active && self.docs[*a].morphs.contains_key(name) {
                let e = &mut self.expressions[*a];
                e.checkpoint();
                let names: Vec<_> = e
                    .animation
                    .tracks
                    .iter()
                    .filter(|t| t.name != "Face.Average")
                    .map(|t| t.name.clone())
                    .collect();
                for n in names {
                    e.animation.set_weight(&n, e.time, 0.0);
                }
                e.animation.set_weight(name, e.time, 1.0);
                e.playing = false;
                e.filter.clear();
            }
        }
        self.editing_points = false;
        self.drag = None;
        self.redraw = true;
        self.status="Adjust the sliders or Save expression above. Point edits are included in the matching head XMF.".into();
    }
    /// Which points "Fewer"/"Fewest" shows, from the base mesh (cached; it
    /// changes only when a head is loaded or the setting changes).
    fn shown_points(&mut self) -> Option<std::sync::Arc<Vec<bool>>> {
        if self.point_spacing <= 0.0 {
            return None;
        }
        let doc = &self.docs[self.active];
        let base = doc.base();
        let key = {
            use std::hash::{Hash, Hasher};
            let mut h = std::collections::hash_map::DefaultHasher::new();
            base.len().hash(&mut h);
            for p in base.iter().step_by(37) {
                [p.0.to_bits(), p.1.to_bits(), p.2.to_bits()].hash(&mut h);
            }
            (self.active, self.point_spacing.to_bits(), h.finish())
        };
        if let Some((k, keep)) = &self.thinned {
            if *k == key {
                return Some(keep.clone());
            }
        }
        let (lo, hi) = base.iter().fold((f32::MAX, f32::MIN), |(lo, hi), p| {
            (lo.min(p.0), hi.max(p.0))
        });
        let keep = std::sync::Arc::new(crate::model::thin_points(
            &base,
            self.point_spacing * (hi - lo).max(1.0),
        ));
        self.thinned = Some((key, keep.clone()));
        Some(keep)
    }
    fn save_face(&mut self) -> bool {
        if self.editing_points {
            self.preview_point_expression();
        }
        if self.docs[self.active].dirty && !self.save() {
            return false;
        }
        self.save_expression()
    }
    fn expression_sidebar(&mut self, ui: &mut egui::Ui) {
        if self.editing_points {
            self.expression_points_sidebar(ui);
            return;
        }
        crate::theme::section(ui, "Facial expressions");
        ui.label("1 Choose head / 2 Adjust face / 3 Save above");
        ui.separator();
        ui.strong("1. Preview head");
        let previous = self.active;
        ui.horizontal(|ui| {
            ui.selectable_value(&mut self.active, 0, "Female");
            ui.selectable_value(&mut self.active, 1, "Male");
        });
        if previous != self.active {
            self.camera = Camera::default();
            self.redraw = true;
        }
        ui.horizontal_wrapped(|ui| {
            if ui
                .small_button("Use HeadMaker head")
                .on_hover_text(
                    "Copy the head (and skin) currently in HeadMaker for this gender, \
                     including unsaved edits. HeadMaker itself is not changed.",
                )
                .clicked()
            {
                self.wants_headmaker_head = true;
            }
            if ui.small_button("Load head…").clicked() {
                self.request(Pending::Open, ui.ctx());
            }
            if ui.small_button("Load skin…").clicked() {
                self.texture(0);
            }
        });
        if let Some(path) = &self.paths[self.active] {
            ui.label(
                egui::RichText::new(path.file_name().unwrap_or_default().to_string_lossy())
                    .small()
                    .weak(),
            );
        } else {
            ui.label(
                egui::RichText::new("Default head ready. Loading your own is optional.")
                    .small()
                    .weak(),
            );
        }
        ui.separator();
        ui.strong("2. Adjust the face");
        ui.horizontal(|ui| {
            if ui.button("New expression").clicked() {
                self.request(Pending::ExpressionReset, ui.ctx());
            }
            if ui.button("Open XPF…").clicked() {
                self.request(Pending::ExpressionOpen, ui.ctx());
            }
        });
        ui.label(
            egui::RichText::new("Expand Brows, Eyes or Mouth below, then move a slider.").small(),
        );
        if self.docs[self.active].dirty {
            ui.label(
                egui::RichText::new(
                    "Shape edited: Save head + expression exports both files for IMVU.",
                )
                .small(),
            );
        }
        ui.separator();
        ui.collapsing("Advanced: edit individual points", |ui| {
            ui.label("Optional. Changing points also needs a matching head XMF.");
            let names: Vec<_> = self.docs[self.active]
                .morphs
                .keys()
                .filter(|n| n.as_str() != crate::model::BASE_MESH && n.as_str() != "Face.Average")
                .cloned()
                .collect();
            if !names.contains(&self.point_target) {
                self.point_target = names.first().cloned().unwrap_or_default();
            }
            egui::ComboBox::from_id_salt("face_target")
                .selected_text(&self.point_target)
                .width(260.0)
                .show_ui(ui, |ui| {
                    for n in names {
                        ui.selectable_value(&mut self.point_target, n.clone(), n);
                    }
                });
            if ui.button("Edit this shape's points").clicked() {
                self.edit_expression_points(self.point_target.clone());
            }
            ui.separator();
            ui.label("Or create your own shape:");
            ui.text_edit_singleline(&mut self.custom_name);
            if ui.button("Create new shape").clicked() {
                let name = self.custom_name.trim().to_string();
                match self.docs[self.active].create_expression(&name) {
                    Ok(()) => self.edit_expression_points(name),
                    Err(e) => self.status = e,
                }
            }
        });
        let editor = &mut self.expressions[self.active];
        let animated = editor.animation.tracks.iter().any(|t| t.keys.len() > 1);
        egui::CollapsingHeader::new("Animation / timing (optional)")
            .default_open(animated)
            .show(ui, |ui| {
                ui.label(
                    "For a still expression, leave Time at 0. \
                     For animation, move Time, then adjust sliders to set a key.",
                );
                let last_key = editor
                    .animation
                    .tracks
                    .iter()
                    .flat_map(|t| t.keys.iter())
                    .map(|k| k.time)
                    .fold(0.01_f32, f32::max);
                let mut duration = editor.animation.duration;
                ui.horizontal(|ui| {
                    ui.label("Duration");
                    let field = egui::DragValue::new(&mut duration)
                        .speed(0.1)
                        .range(last_key..=86400.0)
                        .suffix(" s");
                    if ui.add(field).changed() {
                        editor.checkpoint();
                        editor.animation.duration = duration;
                        self.redraw = true;
                    }
                });
                ui.horizontal(|ui| {
                    if ui
                        .button(if editor.playing { "Pause" } else { "Play" })
                        .clicked()
                    {
                        editor.playing = !editor.playing;
                    }
                    if ui.button("Stop").clicked() {
                        editor.playing = false;
                        editor.time = 0.0;
                        self.redraw = true;
                    }
                    ui.checkbox(&mut editor.looping, "Loop");
                });
                let time = egui::Slider::new(&mut editor.time, 0.0..=editor.animation.duration)
                    .text("Time")
                    .suffix(" s");
                if ui.add(time).changed() {
                    editor.playing = false;
                    self.redraw = true;
                }
            });
        if editor.time > 0.0 {
            ui.label(format!("Editing key at {:.2} seconds", editor.time));
        }
        ui.separator();
        ui.add(egui::TextEdit::singleline(&mut editor.filter).hint_text("Search face controls…"));
        if !editor.filter.is_empty() && ui.small_button("Show all controls").clicked() {
            editor.filter.clear();
        }
        let filter = editor.filter.to_lowercase();
        let unknown: Vec<_> = editor
            .animation
            .tracks
            .iter()
            .filter(|t| !self.docs[self.active].morphs.contains_key(&t.name))
            .map(|t| t.name.clone())
            .collect();
        if !unknown.is_empty() {
            ui.collapsing(
                format!("{} controls missing on this head", unknown.len()),
                |ui| {
                    ui.label("Kept when saving, but this head cannot preview them.");
                    for name in &unknown {
                        ui.label(name);
                    }
                },
            );
        }
        let mut groups: BTreeMap<String, Vec<String>> = BTreeMap::new();
        for name in self.docs[self.active].morphs.keys() {
            if name == crate::model::BASE_MESH
                || name == "Face.Average"
                || !name.to_lowercase().contains(&filter)
            {
                continue;
            }
            let lower = name.to_lowercase();
            let group = if lower.contains("eyebrow") {
                "Brows"
            } else if lower.contains("eye") || lower.contains("blink") {
                "Eyes"
            } else if lower.contains("mouth") || lower.contains("tongue") {
                "Mouth"
            } else {
                "Other"
            };
            groups.entry(group.into()).or_default().push(name.clone());
        }
        for (group, names) in groups {
            egui::CollapsingHeader::new(group)
                .default_open(false)
                .show(ui, |ui| {
                    for name in names {
                        let mut weight = editor.animation.weight(&name, editor.time);
                        let label = name
                            .replace(".Exclusive", "")
                            .replace(".Average", "")
                            .replace(".Clamped", "")
                            .replace('.', " ");
                        ui.horizontal(|ui| {
                            ui.label(egui::RichText::new(label).small());
                            if ui.small_button("Reset").clicked() {
                                editor.checkpoint();
                                editor.animation.set_weight(&name, editor.time, 0.0);
                                editor.playing = false;
                                self.redraw = true;
                            }
                        });
                        let response = ui
                            .add(
                                egui::Slider::new(&mut weight, 0.0..=1.0)
                                    .clamping(egui::SliderClamping::Never)
                                    .fixed_decimals(2),
                            )
                            .on_hover_text(&name);
                        if response.drag_started() {
                            editor.checkpoint();
                            editor.editing = true;
                        }
                        if response.changed() {
                            if !editor.editing {
                                editor.checkpoint();
                            }
                            editor.animation.set_weight(&name, editor.time, weight);
                            editor.playing = false;
                            self.redraw = true;
                        }
                        if response.drag_stopped() {
                            editor.editing = false;
                        }
                    }
                });
        }
    }
    fn expression_points_sidebar(&mut self, ui: &mut egui::Ui) {
        crate::theme::section(ui, "Expression points");
        ui.label(self.docs[self.active].active.clone());
        ui.label("Click a blue point, then drag an arrow. Shift-click adds points.");
        if ui.button("Done editing — preview expression").clicked() {
            self.preview_point_expression();
            return;
        }
        ui.separator();
        let old_scope = (self.point_part, self.point_side);
        egui::ComboBox::from_id_salt("face_point_part")
            .selected_text(
                self.point_part
                    .map(|p| format!("Part {}", p + 1))
                    .unwrap_or("All facial parts".into()),
            )
            .show_ui(ui, |ui| {
                ui.selectable_value(&mut self.point_part, None, "All facial parts");
                for (i, p) in self.docs[self.active].parts.iter().enumerate() {
                    let name = match p.material {
                        2 => "Skin / lips / lids",
                        3 => "Eyes",
                        5 => "Lashes",
                        6 | 8 => "Brows",
                        _ => "Other",
                    };
                    ui.selectable_value(&mut self.point_part, Some(i), name);
                }
            });
        egui::ComboBox::from_id_salt("face_point_side")
            .selected_text(match self.point_side {
                -1 => "Left in front view",
                1 => "Right in front view",
                _ => "Both sides",
            })
            .show_ui(ui, |ui| {
                for (v, n) in [
                    (0, "Both sides"),
                    (-1, "Left in front view"),
                    (1, "Right in front view"),
                ] {
                    ui.selectable_value(&mut self.point_side, v, n);
                }
            });
        let s = &mut self.selections[self.active];
        let mut changed = old_scope != (self.point_part, self.point_side);
        changed |= ui
            .add(egui::Slider::new(&mut s.radius, 0.0..=40.0).text("Nearby points"))
            .changed();
        ui.label(
            egui::RichText::new("0 moves only the points you click.")
                .small()
                .weak(),
        );
        changed |= ui.checkbox(&mut s.symmetric, "Mirror both sides").changed();
        ui.horizontal(|ui| {
            ui.selectable_value(&mut self.controls, Controls::Original, "3D arrows");
            ui.selectable_value(&mut self.controls, Controls::Direct, "Direct drag");
        });
        ui.label(format!(
            "{} points selected",
            s.weights.iter().filter(|&&w| w > 0.0).count()
        ));
        if ui.button("Clear selection").clicked() {
            s.seeds.clear();
            changed = true;
        }
        if changed {
            self.overlays_visible = true;
            self.recalc();
        }
        ui.separator();
        ui.label(
            egui::RichText::new("The neck is protected. Save head + expression after finishing.")
                .small()
                .weak(),
        );
    }
    fn texture(&mut self, part: usize) {
        if part >= self.docs[self.active].parts.len() {
            self.status =
                "This mesh has no separate part in that slot. Use the per-part texture controls."
                    .into();
            return;
        }
        if let Some(p) = rfd::FileDialog::new()
            .add_filter("Textures", &["png", "jpg", "jpeg", "gif"])
            .pick_file()
        {
            let result = (|| -> Result<Texture, String> {
                let reader = image::ImageReader::open(&p)
                    .map_err(|e| e.to_string())?
                    .with_guessed_format()
                    .map_err(|e| e.to_string())?;
                let mut reader = reader;
                let mut limits = image::Limits::default();
                limits.max_image_width = Some(8192);
                limits.max_image_height = Some(8192);
                reader.limits(limits);
                Ok(Texture::new(
                    reader.decode().map_err(|e| e.to_string())?.into_rgba8(),
                ))
            })();
            match result {
                Ok(t) => {
                    let slot =
                        render::texture_slot(self.docs[self.active].parts[part].material, part);
                    while self.textures[self.active].len() <= slot {
                        self.textures[self.active].push(Texture::neutral());
                    }
                    self.textures[self.active][slot] = t;
                    if slot < 4 {
                        self.session_textures[self.active][slot] = true;
                    }
                    if part == 0 {
                        self.skin_paths[self.active] = Some(p.clone());
                    }
                    self.textured = true;
                    self.redraw = true;
                    self.status = format!(
                        "Loaded {}. Skin textures are separate from the exported XMF.",
                        p.display()
                    );
                }
                Err(e) => self.status = format!("Could not load texture: {e}"),
            }
        }
    }
    fn sculpt(&mut self, global: bool) {
        let d = &self.docs[self.active];
        let s = &self.selections[self.active];
        let before = d.positions();
        let base = d.base();
        let mut result = before.to_vec();
        for part in &d.parts {
            for local in 0..part.base.len() {
                let i = part.offset + local;
                let weight = if global {
                    if s.locked(i) {
                        0.0
                    } else {
                        1.0
                    }
                } else {
                    s.weights[i]
                };
                if weight == 0.0 {
                    continue;
                }
                let p = before[i];
                let goal = match self.sculpt {
                    Sculpt::Restore => base[i],
                    Sculpt::Relax | Sculpt::Tense => {
                        let ns = &part.neighbors[local];
                        if ns.is_empty() {
                            continue;
                        }
                        let average = ns
                            .iter()
                            .map(|&j| before[part.offset + j])
                            .fold(V::default(), V::add)
                            .mul(1.0 / ns.len() as f32);
                        if self.sculpt == Sculpt::Tense {
                            p.mul(2.0).sub(average)
                        } else {
                            average
                        }
                    }
                    Sculpt::Shrink | Sculpt::Grow => {
                        let mut center = p;
                        let mut distance = f32::MAX;
                        for &seed in &s.seeds {
                            for q in [
                                before[seed],
                                if s.symmetric {
                                    before[seed].mirror()
                                } else {
                                    before[seed]
                                },
                            ] {
                                let dist = q.sub(p).len();
                                if dist < distance {
                                    distance = dist;
                                    center = q;
                                }
                            }
                        }
                        if self.sculpt == Sculpt::Grow {
                            p.mul(2.0).sub(center)
                        } else {
                            center
                        }
                    }
                    Sculpt::Preset => base[i].add(
                        self.presets[self.preset]
                            .1
                            .get(i)
                            .copied()
                            .unwrap_or_default(),
                    ),
                };
                result[i] = p.lerp(goal, self.amount * weight);
                if s.symmetric && p.0 == 0.0 {
                    result[i].0 = 0.0;
                }
            }
        }
        // Weld original coincident skin seam vertices; keep the seven authored exceptions.
        let exceptions = [
            (527, 506),
            (505, 465),
            (553, 433),
            (260, 105),
            (261, 104),
            (204, 159),
            (233, 205),
        ];
        let part = &d.parts[0];
        let mut visited = vec![false; part.base.len()];
        for i in 0..part.base.len() {
            if visited[i] {
                continue;
            }
            let mut group = vec![i];
            for j in i + 1..part.base.len() {
                if part.base[i]
                    .sub(part.base[j])
                    .dot(part.base[i].sub(part.base[j]))
                    < 0.0005
                    && !exceptions.contains(&(j, i))
                {
                    group.push(j);
                }
            }
            if group.len() > 1
                && group.iter().any(|&j| result[j] != before[j])
                && !group.iter().any(|&j| s.locked(j))
            {
                let avg = group
                    .iter()
                    .map(|&j| result[j])
                    .fold(V::default(), V::add)
                    .mul(1.0 / group.len() as f32);
                for &j in &group {
                    result[j] = avg;
                    visited[j] = true;
                }
            }
        }
        self.record_change(result);
    }
    fn sidebar(&mut self, ui: &mut egui::Ui) {
        if self.expression_mode {
            self.expression_sidebar(ui);
            return;
        }
        crate::theme::section(ui, "Head shape");
        ui.label(
            egui::RichText::new("Edit head geometry · XMF")
                .small()
                .weak(),
        );
        ui.add_space(8.0);
        let old = self.active;
        ui.horizontal(|ui| {
            ui.selectable_value(&mut self.active, 0, "Female");
            ui.selectable_value(&mut self.active, 1, "Male");
        });
        if old != self.active {
            self.drag = None;
            self.camera = Camera::default();
            self.overlays_visible = true;
            self.recalc();
        }
        ui.separator();
        let mut morph = self.docs[self.active].active.clone();
        egui::ComboBox::from_id_salt("morph")
            .selected_text(&morph)
            .width(240.0)
            .show_ui(ui, |ui| {
                for name in self.docs[self.active].morphs.keys() {
                    ui.selectable_value(&mut morph, name.clone(), name);
                }
            });
        if morph != self.docs[self.active].active {
            self.docs[self.active].active = morph;
            self.recalc();
        }
        ui.label(
            egui::RichText::new(
                if self.docs[self.active].active == crate::model::BASE_MESH {
                    "Editing base shape; expressions follow the changes."
                } else {
                    "Editing this morph; base mesh stays intact."
                },
            )
            .small()
            .weak(),
        );
        egui::CollapsingHeader::new("Skin texture")
            .default_open(true)
            .show(ui, |ui| {
                if ui.button("Load custom skin PNG / JPG…").clicked() {
                    self.texture(0);
                }
                if let Some(path) = &self.skin_paths[self.active] {
                    ui.label(path.file_name().unwrap_or_default().to_string_lossy());
                } else {
                    ui.label("Default avatar skin");
                }
                ui.horizontal(|ui| {
                    if ui.small_button("Default skin").clicked() {
                        self.textures[self.active][0] =
                            crate::settings::default_textures(self.active == 0)
                                .0
                                .remove(0);
                        self.session_textures[self.active][0] = false;
                        self.skin_paths[self.active] = None;
                        self.redraw = true;
                    }
                    if ui.small_button("Load eyes…").clicked() {
                        self.texture(1);
                    }
                });
                ui.label(
                    egui::RichText::new("Use a flat head UV texture, like girl.png.")
                        .small()
                        .weak(),
                );
            });
        ui.separator();
        crate::theme::card().show(ui, |ui| {
            crate::theme::group_title(ui, "SELECTION");
            ui.label(egui::RichText::new("Blue: editable   Red: affected   Grey: locked").small());
            let mut changed = false;
            {
                let s = &mut self.selections[self.active];
                changed |= ui
                    .add(egui::Slider::new(&mut s.radius, 0.0..=100.0).text("Range"))
                    .changed();
                let old = s.falloff;
                egui::ComboBox::from_id_salt("falloff")
                    .selected_text(format!("{:?}", s.falloff))
                    .show_ui(ui, |ui| {
                        for (f, n) in [
                            (Falloff::Smooth, "Smooth (sine)"),
                            (Falloff::Linear, "Linear"),
                            (Falloff::EaseIn, "Ease in"),
                            (Falloff::EaseOut, "Ease out"),
                            (Falloff::Flat, "Flat"),
                            (Falloff::All, "All vertices"),
                        ] {
                            ui.selectable_value(&mut s.falloff, f, n);
                        }
                    });
                changed |= old != s.falloff;
                if ui.checkbox(&mut s.symmetric, "Maintain symmetry").changed() {
                    changed = true;
                    if s.symmetric {
                        s.edge_limit = false;
                    }
                }
                let mut edit_head = !s.critical;
                if ui
                    .checkbox(&mut edit_head, "Edit protected head points")
                    .changed()
                {
                    s.critical = !edit_head;
                    changed = true;
                }
                changed |= ui.checkbox(&mut s.allow_neck, "Edit neck points").changed();
                ui.label(
                    egui::RichText::new("Neck unlock is session-only; restarting locks it again.")
                        .small()
                        .weak(),
                );
                if s.allow_neck {
                    ui.label(
                        egui::RichText::new(
                            "Saved XMF files retain your explicit neck geometry edits.",
                        )
                        .small()
                        .color(crate::theme::ORANGE),
                    );
                }
                changed |= ui
                    .checkbox(&mut s.single_part, "Limit selection to submesh")
                    .changed();
                ui.add_enabled_ui(!s.symmetric, |ui| {
                    changed |= ui
                        .checkbox(&mut s.edge_limit, "Limit edge distance")
                        .changed();
                    if s.edge_limit {
                        changed |= ui
                            .add(egui::Slider::new(&mut s.edge_steps, 1..=20).text("Edges"))
                            .changed();
                    }
                });
                ui.label(format!(
                    "{} selected · {} seed points",
                    s.weights.iter().filter(|&&w| w > 0.0).count(),
                    s.seeds.len()
                ));
                ui.horizontal(|ui| {
                    if ui.small_button("Clear").clicked() {
                        s.seeds.clear();
                        changed = true;
                    }
                    if ui.small_button("Unhide all").clicked() {
                        s.hidden.fill(false);
                        changed = true;
                    }
                });
                ui.horizontal(|ui| {
                    if ui.small_button("Hide selected").clicked() {
                        for i in 0..s.weights.len() {
                            if s.weights[i] > 0.0 {
                                s.hidden[i] = true;
                            }
                        }
                        s.seeds.clear();
                        changed = true;
                    }
                    if ui.small_button("Hide others").clicked() {
                        for i in 0..s.weights.len() {
                            if s.weights[i] == 0.0 {
                                s.hidden[i] = true;
                            }
                        }
                        changed = true;
                    }
                });
            }
            if changed {
                self.overlays_visible = true;
                self.recalc();
            }
        });
        ui.add_space(8.0);
        crate::theme::group_title(ui, "MOVEMENT");
        let old_controls = self.controls;
        ui.horizontal(|ui| {
            ui.selectable_value(&mut self.controls, Controls::Original, "Original arrows");
            ui.selectable_value(&mut self.controls, Controls::Direct, "Direct drag");
        });
        if self.controls != old_controls {
            self.drag = None;
            self.tool = Tool::Screen;
            self.keyboard_axis = None;
            self.status = if self.controls == Controls::Direct {
                "Drag a blue point directly. Right-drag rotates the view.".into()
            } else {
                "Select a blue point, then drag a 3D axis arrow.".into()
            };
        }
        if self.controls == Controls::Original {
            ui.checkbox(&mut self.camera_aligned_gizmo, "Camera-aligned arrows");
            ui.label(
                egui::RichText::new(if self.camera_aligned_gizmo {
                    "Arrows follow the view."
                } else {
                    "3D axes stay attached to the head. Rotate to reach Z."
                })
                .small()
                .weak(),
            );
        } else {
            ui.label(
                egui::RichText::new("Drag points directly. No axis arrows.")
                    .small()
                    .weak(),
            );
        }
        ui.label(
            egui::RichText::new({
                use crate::shortcuts::{text, Command};
                format!(
                    "Keys: {} / {} / {} axis · {} free move",
                    text(Command::AxisX),
                    text(Command::AxisY),
                    text(Command::AxisZ),
                    text(Command::Move)
                )
            })
            .small()
            .weak(),
        );
        ui.horizontal(|ui| {
            ui.add(
                egui::DragValue::new(&mut self.delta.0)
                    .speed(0.1)
                    .prefix("X "),
            );
            ui.add(
                egui::DragValue::new(&mut self.delta.1)
                    .speed(0.1)
                    .prefix("Y "),
            );
            ui.add(
                egui::DragValue::new(&mut self.delta.2)
                    .speed(0.1)
                    .prefix("Z "),
            );
        });
        if ui
            .add_enabled(
                self.selections[self.active]
                    .weights
                    .iter()
                    .any(|&w| w > 0.0),
                egui::Button::new("Apply move"),
            )
            .clicked()
        {
            let ps = self.selections[self.active]
                .translate(self.docs[self.active].positions(), self.delta);
            self.record_change(ps);
            self.delta = V::default();
        }
        ui.add_space(8.0);
        crate::theme::group_title(ui, "SCULPT");
        if !self.docs[self.active].legacy && self.sculpt == Sculpt::Preset {
            self.sculpt = Sculpt::Relax;
        }
        if !self.docs[self.active].legacy {
            ui.label(
                egui::RichText::new("Imported mesh: original-head presets unavailable")
                    .small()
                    .weak(),
            );
        }
        egui::ComboBox::from_id_salt("sculpt")
            .selected_text(match self.sculpt {
                Sculpt::Restore => "Restore",
                Sculpt::Relax => "Relax",
                Sculpt::Tense => "Tense",
                Sculpt::Shrink => "Shrink",
                Sculpt::Grow => "Grow",
                Sculpt::Preset => "Preset",
            })
            .show_ui(ui, |ui| {
                for (s, n) in [
                    (Sculpt::Restore, "Restore"),
                    (Sculpt::Relax, "Relax"),
                    (Sculpt::Tense, "Tense"),
                    (Sculpt::Shrink, "Shrink"),
                    (Sculpt::Grow, "Grow"),
                    (Sculpt::Preset, "Preset"),
                ] {
                    if s != Sculpt::Preset || self.docs[self.active].legacy {
                        ui.selectable_value(&mut self.sculpt, s, n);
                    }
                }
            });
        if self.sculpt == Sculpt::Preset {
            egui::ComboBox::from_id_salt("preset")
                .selected_text(&self.presets[self.preset].0)
                .show_ui(ui, |ui| {
                    for (i, p) in self.presets.iter().enumerate() {
                        ui.selectable_value(&mut self.preset, i, &p.0);
                    }
                });
        }
        ui.add(egui::Slider::new(&mut self.amount, 0.0..=1.0).text("Strength"));
        ui.horizontal(|ui| {
            if ui
                .add_enabled(
                    self.selections[self.active]
                        .weights
                        .iter()
                        .any(|&w| w > 0.0),
                    egui::Button::new("Apply to selection"),
                )
                .clicked()
            {
                self.sculpt(false);
            }
            if self.sculpt == Sculpt::Preset && ui.button("Whole head").clicked() {
                self.sculpt(true);
            }
        });
        ui.separator();
        ui.strong("Appearance");
        ui.horizontal(|ui| {
            self.redraw |= ui.checkbox(&mut self.textured, "Texture").changed();
            self.redraw |= ui.checkbox(&mut self.wire, "Wire").changed();
            ui.checkbox(&mut self.vertices, "Points");
        });
        ui.add(egui::Slider::new(&mut self.point_size, 2.0..=5.0).text("Point size"));
        ui.horizontal(|ui| {
            ui.label("Points shown");
            for (label, spacing, tip) in [
                ("All", 0.0, "Every point"),
                (
                    "Fewer",
                    0.04,
                    "About half the points, spread evenly over the head",
                ),
                (
                    "Fewest",
                    0.08,
                    "About a quarter of the points, spread evenly",
                ),
            ] {
                ui.selectable_value(&mut self.point_spacing, spacing, label)
                    .on_hover_text(tip);
            }
        });
        ui.checkbox(&mut self.xray, "Select through surface");
        ui.horizontal(|ui| {
            if ui.small_button("Load skin…").clicked() {
                self.texture(0);
            }
            if ui.small_button("Load eyes…").clicked() {
                self.texture(1);
            }
        });
        egui::CollapsingHeader::new("Mesh parts and materials")
            .default_open(true)
            .show(ui, |ui| {
                for i in 0..self.docs[self.active].parts.len() {
                    let material = self.docs[self.active].parts[i].material;
                    let label = match material {
                        2 => "Skin".to_string(),
                        3 => "Eyes".to_string(),
                        5 => "Lashes".to_string(),
                        6 | 8 => "Brows".to_string(),
                        _ => format!("Material {material}"),
                    };
                    ui.horizontal(|ui| {
                        let mut visible = !self.hidden_parts[self.active][i];
                        if ui
                            .checkbox(&mut visible, format!("{}: {}", i + 1, label))
                            .changed()
                        {
                            self.hidden_parts[self.active][i] = !visible;
                            self.redraw = true;
                        }
                        if ui.small_button("Load texture…").clicked() {
                            self.texture(i);
                        }
                    });
                }
            });
        ui.add_space(5.0);
    }
    fn viewport(&mut self, ui: &mut egui::Ui) {
        let point_editing = self.editing_points
            && self
                .point_expression
                .as_ref()
                .is_some_and(|(a, n)| *a == self.active && n == &self.docs[self.active].active);
        let edit_offset = if point_editing {
            let d = &self.docs[self.active];
            let base = d
                .morphs
                .get(crate::model::BASE_MESH)
                .cloned()
                .unwrap_or_else(|| d.base());
            let neutral = d.morphs.get("Face.Average").unwrap_or(&base);
            neutral
                .iter()
                .zip(&base)
                .map(|(&a, &b)| a.sub(b))
                .collect::<Vec<_>>()
        } else {
            Vec::new()
        };
        let edit_positions = if point_editing {
            self.docs[self.active]
                .positions()
                .iter()
                .zip(&edit_offset)
                .map(|(&p, &offset)| p.add(offset))
                .collect::<Vec<_>>()
        } else {
            Vec::new()
        };
        let available = ui.available_size();
        let size = Vec2::new(available.x.max(10.0), available.y.max(10.0));
        let (rect, response) = ui.allocate_exact_size(size, egui::Sense::click_and_drag());
        let playing = self.expression_mode && self.expressions[self.active].playing;
        let quality = if self.use_gpu {
            crate::gpu::logical_size(size)
        } else {
            self.preview_budget.choose(ui.ctx(), size, playing)
        };
        let [rw, rh] = quality.render;
        if let Some(old) = &self.frame {
            if old.width != rw || old.height != rh {
                self.camera.pan[0] *= rw as f32 / old.width as f32;
                self.camera.pan[1] *= rh as f32 / old.height as f32;
            }
        }
        if self
            .frame
            .as_ref()
            .is_none_or(|f| f.width != rw || f.height != rh)
            || (!self.use_gpu
                && self
                    .preview
                    .as_ref()
                    .is_none_or(|t| t.size() != quality.display))
        {
            self.redraw = true;
        }
        if self.redraw {
            let f = if self.use_gpu {
                let doc = &self.docs[self.active];
                let positions = if self.expression_mode && !self.editing_points {
                    self.expressions[self.active]
                        .animation
                        .blend(doc, self.expressions[self.active].time)
                } else if point_editing {
                    edit_positions.clone()
                } else {
                    doc.positions().to_vec()
                };
                let parts = if self.expression_mode && !self.editing_points {
                    vec![false; doc.parts.len()]
                } else {
                    self.hidden_parts[self.active].clone()
                };
                let hidden = if self.expression_mode && !self.editing_points {
                    vec![false; positions.len()]
                } else {
                    self.selections[self.active].hidden.clone()
                };
                self.gpu_scene = Some(std::sync::Arc::new(crate::gpu::Scene::head(
                    doc,
                    &self.textures[self.active],
                    &positions,
                    &self.camera,
                    [rw, rh],
                    &parts,
                    &hidden,
                    self.textured,
                )));
                render::picking_frame(doc, &positions, &self.camera, [rw, rh], &parts, &hidden)
            } else if self.expression_mode && !self.editing_points {
                let positions = self.expressions[self.active]
                    .animation
                    .blend(&self.docs[self.active], self.expressions[self.active].time);
                render::render_positions(
                    &self.docs[self.active],
                    &self.textures[self.active],
                    &self.camera,
                    [rw, rh],
                    &vec![false; self.docs[self.active].parts.len()],
                    &vec![false; positions.len()],
                    self.textured,
                    &positions,
                )
            } else if point_editing {
                render::render_positions(
                    &self.docs[self.active],
                    &self.textures[self.active],
                    &self.camera,
                    [rw, rh],
                    &self.hidden_parts[self.active],
                    &self.selections[self.active].hidden,
                    self.textured,
                    &edit_positions,
                )
            } else {
                render::render(
                    &self.docs[self.active],
                    &self.textures[self.active],
                    &self.camera,
                    [rw, rh],
                    &self.hidden_parts[self.active],
                    &self.selections[self.active].hidden,
                    self.textured,
                )
            };
            if !self.use_gpu {
                let image = render::antialias_preview(f.image.clone(), quality.display);
                if let Some(t) = &mut self.preview {
                    t.set(image, egui::TextureOptions::LINEAR);
                } else {
                    self.preview = Some(ui.ctx().load_texture(
                        "head-preview",
                        image,
                        egui::TextureOptions::LINEAR,
                    ));
                }
            }
            self.frame = Some(f);
            self.redraw = false;
        }
        let painter = ui.painter_at(rect);
        if self.use_gpu {
            if let (Some(gpu), Some(scene)) = (&self.gpu, &self.gpu_scene) {
                crate::gpu::paint(ui, rect, gpu.clone(), scene.clone());
            }
        } else if let Some(t) = &self.preview {
            painter.image(
                t.id(),
                rect,
                egui::Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
                Color32::WHITE,
            );
        }
        let shown = self.shown_points();
        let frame = self.frame.as_ref().unwrap();
        let screen = |p: [f32; 3]| {
            Pos2::new(
                rect.left() + p[0] / rw as f32 * size.x,
                rect.top() + p[1] / rh as f32 * size.y,
            )
        };
        let selection = &self.selections[self.active];
        if self.wire && self.overlays_visible && !self.expression_mode {
            for (i, part) in self.docs[self.active].parts.iter().enumerate() {
                if self.hidden_parts[self.active][i] {
                    continue;
                }
                for f in &part.faces {
                    let ids = [part.offset + f[0], part.offset + f[1], part.offset + f[2]];
                    if ids.iter().any(|&j| selection.hidden[j]) {
                        continue;
                    }
                    for j in 0..3 {
                        let a = ids[j];
                        let b = ids[(j + 1) % 3];
                        if self.xray || (frame.visible(a) && frame.visible(b)) {
                            painter.line_segment(
                                [screen(frame.projected[a]), screen(frame.projected[b])],
                                Stroke::new(0.6_f32, Color32::from_black_alpha(120)),
                            );
                        }
                    }
                }
            }
        }
        let mut nearest = None;
        let mut distance = 14.0f32;
        let pointer = if response.drag_started_by(PointerButton::Primary) {
            ui.input(|i| i.pointer.press_origin())
        } else {
            response
                .interact_pointer_pos()
                .or(ui.input(|i| i.pointer.hover_pos()))
        };
        let mut handles: Vec<(Tool, Vec2, Vec2, V)> = Vec::new();
        let anchor = selection.seeds.last().copied().filter(|&i| {
            (!self.expression_mode || self.editing_points)
                && self.controls == Controls::Original
                && self.overlays_visible
                && !selection.hidden[i]
                && frame.projected[i][2] > 10.0
        });
        let mut handle_hover = None;
        if let Some(seed) = anchor {
            let point = if point_editing {
                edit_positions[seed]
            } else {
                self.docs[self.active].positions()[seed]
            };
            let origin = screen(frame.projected[seed]);
            for tool in [Tool::X, Tool::Y, Tool::Z] {
                let world_axis = if self.camera_aligned_gizmo {
                    self.camera.unrotate(tool.axis())
                } else {
                    tool.axis()
                };
                let p = self.camera.project(point.add(world_axis), rw, rh);
                let mut axis = screen(p) - origin;
                let display = if self.camera_aligned_gizmo {
                    let direction = match tool {
                        Tool::X => Vec2::new(1.0, 0.0),
                        Tool::Y => Vec2::new(0.0, -1.0),
                        _ => Vec2::new(0.65, 0.76).normalized(),
                    };
                    if tool == Tool::Z {
                        axis = direction
                            * (size.y * 1.2 * self.camera.zoom / frame.projected[seed][2]).max(0.5);
                    }
                    direction * 86.0
                } else {
                    // Project a real 30-unit segment. Never turn an edge-on
                    // world axis into a diagonal screen-space handle.
                    screen(self.camera.project(point.add(world_axis.mul(30.0)), rw, rh)) - origin
                };
                if display.length() < 10.0 {
                    continue;
                }
                handles.push((tool, axis, display, world_axis));
            }
            if let Some(mouse) = pointer {
                let local = mouse - origin;
                if local.length() < 11.0 {
                    handle_hover = Some((Tool::Screen, Vec2::ZERO, V::default()));
                } else {
                    let mut closest = 12.0;
                    for &(tool, axis, end, world_axis) in &handles {
                        let dist = crate::gizmo::segment_distance(local, end);
                        if dist < closest && local.dot(end) > 0.0 {
                            closest = dist;
                            handle_hover = Some((tool, axis, world_axis));
                        }
                    }
                }
            }
        }
        for (i, &p) in frame.projected.iter().enumerate() {
            if selection.hidden[i]
                || self.docs[self.active]
                    .parts
                    .iter()
                    .enumerate()
                    .any(|(j, part)| {
                        i >= part.offset
                            && i < part.offset + part.base.len()
                            && self.hidden_parts[self.active][j]
                    })
            {
                continue;
            }
            if !self.xray && !frame.visible(i) && !selection.seeds.contains(&i) {
                continue;
            }
            let xy = screen(p);
            let locked = selection.locked(i);
            let weight = selection.weights[i];
            // "Fewer points": keep one point per screen cell. Selected and
            // affected points always show; thinned points are not pickable,
            // so a click never lands on an invisible point.
            if shown
                .as_ref()
                .is_some_and(|keep| !keep.get(i).copied().unwrap_or(true))
                && !selection.seeds.contains(&i)
                && weight <= 0.0
            {
                continue;
            }
            if !self.expression_mode || self.editing_points {
                if let Some(mouse) = pointer {
                    let d = xy.distance(mouse);
                    if d < distance && !locked && rect.contains(mouse) {
                        distance = d;
                        nearest = Some(i);
                    }
                }
            }
            if (!self.expression_mode || self.editing_points)
                && self.overlays_visible
                && (self.vertices || weight > 0.0)
            {
                let color = if locked {
                    Color32::from_rgba_unmultiplied(155, 155, 155, 90)
                } else if selection.seeds.contains(&i) {
                    Color32::WHITE
                } else if weight > 0.0 {
                    Color32::from_rgb(255, (115.0 * (1.0 - weight)) as u8, 90)
                } else {
                    Color32::from_rgb(65, 85, 255)
                };
                let marker = egui::Rect::from_center_size(
                    xy,
                    Vec2::splat(if selection.seeds.contains(&i) {
                        8.0
                    } else {
                        self.point_size * 2.0
                    }),
                );
                if !locked {
                    painter.rect_filled(marker.expand(0.8), 0.0, Color32::from_black_alpha(140));
                }
                painter.rect_filled(marker, 0.0, color);
            }
        }
        if response.hovered() && self.overlays_visible {
            if let Some(i) = nearest {
                painter.circle_stroke(
                    screen(frame.projected[i]),
                    8.0,
                    Stroke::new(2.0_f32, Color32::WHITE),
                );
            }
        }
        if let Some(seed) = anchor {
            let origin = screen(frame.projected[seed]);
            painter.circle_stroke(
                origin,
                12.0,
                Stroke::new(3.0_f32, Color32::from_black_alpha(210)),
            );
            painter.circle_stroke(origin, 11.0, Stroke::new(1.5_f32, Color32::WHITE));
            for &(tool, _, display, _) in &handles {
                let direction = display.normalized();
                let end = origin + display;
                let highlighted = handle_hover.is_some_and(|h| h.0 == tool)
                    || matches!(&self.drag,Some(Drag::Move{tool:t,..}) if *t==tool);
                let color = if highlighted {
                    Color32::from_rgb(255, 235, 120)
                } else {
                    tool.color()
                };
                painter.line_segment(
                    [origin, end],
                    Stroke::new(7.0_f32, Color32::from_black_alpha(210)),
                );
                painter.line_segment(
                    [origin, end],
                    Stroke::new(if highlighted { 4.5_f32 } else { 3.0_f32 }, color),
                );
                let side = Vec2::new(-direction.y, direction.x);
                painter.add(egui::Shape::convex_polygon(
                    vec![
                        end + direction * 4.0,
                        end - direction * 15.0 + side * 7.0,
                        end - direction * 15.0 - side * 7.0,
                    ],
                    color,
                    Stroke::new(1.0_f32, Color32::from_black_alpha(180)),
                ));
                let label = match tool {
                    Tool::X => "X",
                    Tool::Y => "Y",
                    _ => "Z depth",
                };
                painter.text(
                    end + direction * 19.0,
                    egui::Align2::CENTER_CENTER,
                    label,
                    egui::FontId::proportional(15.0),
                    Color32::BLACK,
                );
                painter.text(
                    end + direction * 19.0 - Vec2::splat(1.0),
                    egui::Align2::CENTER_CENTER,
                    label,
                    egui::FontId::proportional(15.0),
                    color,
                );
            }
            painter.rect_filled(
                egui::Rect::from_center_size(origin, Vec2::splat(12.0)),
                2.0,
                Color32::WHITE,
            );
            let note = if let Some(Drag::Move { delta, .. }) = &self.drag {
                format!("Moving · {:.0}, {:.0} px  |  Esc cancels", delta.x, delta.y)
            } else {
                "Drag an arrow · white centre moves freely".into()
            };
            painter.text(
                origin + Vec2::new(0.0, 125.0),
                egui::Align2::CENTER_TOP,
                &note,
                egui::FontId::proportional(13.0),
                Color32::WHITE,
            );
        }
        if response.hovered() {
            ui.ctx()
                .set_cursor_icon(if matches!(self.drag, Some(Drag::Move { .. })) {
                    egui::CursorIcon::Grabbing
                } else if handle_hover.is_some() || nearest.is_some() {
                    egui::CursorIcon::Grab
                } else {
                    egui::CursorIcon::Default
                });
        }
        painter.text(
            rect.left_top() + Vec2::new(20.0, 18.0),
            egui::Align2::LEFT_TOP,
            format!(
                "{} / {}{}",
                if self.active == 0 { "FEMALE" } else { "MALE" },
                if self.expression_mode && !self.editing_points {
                    "Expression preview"
                } else {
                    &self.docs[self.active].active
                },
                if if self.expression_mode && !self.editing_points {
                    self.expressions[self.active].dirty()
                } else {
                    self.docs[self.active].dirty
                } {
                    "  •"
                } else {
                    ""
                }
            ),
            egui::FontId::proportional(15.0),
            crate::theme::FG_BRIGHT,
        );
        let hint = if self.expression_mode && !self.editing_points {
            "Expression preview     •     Right-drag: rotate     •     \
             Shift + right-drag: pan     •     Scroll: zoom"
        } else if self.controls == Controls::Original {
            "1  Select a point     2  Drag its 3D arrows     •     Right-drag: rotate     •     Scroll: zoom"
        } else {
            "Drag a blue point directly     •     Right-drag: rotate     •     Scroll: zoom"
        };
        painter.text(
            rect.left_bottom() + Vec2::new(20.0, -18.0),
            egui::Align2::LEFT_BOTTOM,
            hint,
            egui::FontId::proportional(13.0),
            crate::theme::FG,
        );
        if self.neck_offer || self.choose_import || self.pending.is_some() {
            return;
        }
        let hit_surface = pointer.is_some_and(|p| {
            let x = ((p.x - rect.left()) / size.x * rw as f32) as i32;
            let y = ((p.y - rect.top()) / size.y * rh as f32) as i32;
            x >= 0
                && y >= 0
                && (x as usize) < rw
                && (y as usize) < rh
                && frame.depth[y as usize * rw + x as usize] < f32::MAX
        });
        // A point drawn under one of the selected point's arrows stays
        // clickable: a plain click right on it (within its marker) selects
        // it. Dragging still grabs the arrow, so moving along it is unchanged.
        let on_other_point =
            nearest.is_some_and(|i| anchor != Some(i) && distance <= self.point_size + 3.0);
        if (!self.expression_mode || self.editing_points)
            && response.clicked_by(PointerButton::Primary)
            && (handle_hover.is_none() || on_other_point)
        {
            if nearest.is_none() && !hit_surface {
                self.overlays_visible = false;
                self.recalc();
            } else {
                self.overlays_visible = true;
            }
            if let Some(i) = nearest {
                let add = ui.input(|i| i.modifiers.shift);
                let s = &mut self.selections[self.active];
                if !add {
                    s.seeds.clear();
                }
                if !s.seeds.contains(&i) {
                    s.seeds.push(i);
                }
                self.recalc();
            }
        }
        if (!self.expression_mode || self.editing_points)
            && response.drag_started_by(PointerButton::Primary)
        {
            if let Some((tool, axis_screen, world_axis)) = handle_hover {
                self.tool = tool;
                self.keyboard_axis = None;
                self.docs[self.active].checkpoint();
                self.drag = Some(Drag::Move {
                    before: self.docs[self.active].positions().to_vec(),
                    delta: Vec2::ZERO,
                    tool,
                    axis_screen,
                    world_axis,
                });
            } else if let Some(i) = nearest {
                let add = ui.input(|i| i.modifiers.shift);
                let s = &mut self.selections[self.active];
                if !s.seeds.contains(&i) {
                    if !add {
                        s.seeds.clear();
                    }
                    s.seeds.push(i);
                }
                self.recalc();
                let tool = self.keyboard_axis.unwrap_or(Tool::Screen);
                let world_axis = if self.camera_aligned_gizmo {
                    self.camera.unrotate(tool.axis())
                } else {
                    tool.axis()
                };
                let p = if point_editing {
                    edit_positions[i]
                } else {
                    self.docs[self.active].positions()[i]
                };
                let a = self.camera.project(p, rw, rh);
                let b = self.camera.project(p.add(world_axis), rw, rh);
                let mut axis_screen = Vec2::new(
                    (b[0] - a[0]) * size.x / rw as f32,
                    (b[1] - a[1]) * size.y / rh as f32,
                );
                if axis_screen.length() < 0.35 {
                    axis_screen =
                        Vec2::new(0.65, 0.76) * (size.y * 1.2 * self.camera.zoom / a[2]).max(0.5);
                }
                if self.controls == Controls::Direct || tool != Tool::Screen {
                    self.docs[self.active].checkpoint();
                    self.drag = Some(Drag::Move {
                        before: self.docs[self.active].positions().to_vec(),
                        delta: Vec2::ZERO,
                        tool,
                        axis_screen,
                        world_axis,
                    });
                }
            } else if self.controls == Controls::Direct {
                self.drag = Some(Drag::Orbit);
            }
        }
        if response.drag_started_by(PointerButton::Secondary) {
            let modifiers = ui.input(|i| i.modifiers);
            self.drag = Some(if modifiers.ctrl {
                Drag::Zoom
            } else if modifiers.shift {
                Drag::Pan
            } else {
                Drag::Orbit
            });
        }
        if response.drag_started_by(PointerButton::Middle) {
            self.drag = Some(Drag::Pan);
        }
        if response.dragged() {
            let delta = ui.input(|i| i.pointer.delta());
            match &mut self.drag {
                Some(Drag::Orbit) => {
                    self.camera.yaw -= delta.x * 0.009;
                    self.camera.pitch = (self.camera.pitch - delta.y * 0.009).clamp(-1.5, 1.5);
                    self.redraw = true;
                }
                Some(Drag::Pan) => {
                    self.camera.pan[0] += delta.x * rw as f32 / size.x;
                    self.camera.pan[1] += delta.y * rh as f32 / size.y;
                    self.redraw = true;
                }
                Some(Drag::Zoom) => {
                    self.camera.zoom = (self.camera.zoom * (-delta.y * 0.01).exp()).clamp(0.3, 6.0);
                    self.redraw = true;
                }
                Some(Drag::Move {
                    before,
                    delta: total,
                    tool,
                    axis_screen,
                    world_axis,
                }) => {
                    *total += delta;
                    let depth = self.selections[self.active]
                        .seeds
                        .last()
                        .map(|&i| {
                            self.camera.project(
                                before[i].add(edit_offset.get(i).copied().unwrap_or_default()),
                                rw,
                                rh,
                            )[2]
                        })
                        .unwrap_or(360.0);
                    let raw = self.camera.screen_delta(
                        total.x * rw as f32 / size.x,
                        total.y * rh as f32 / size.y,
                        rh as f32,
                        depth,
                    );
                    let movement = if *tool == Tool::Screen {
                        raw
                    } else {
                        world_axis.mul(crate::gizmo::axis_amount(*total, *axis_screen))
                    };
                    let ps = self.selections[self.active].translate(before, movement);
                    self.docs[self.active].change(ps);
                    self.redraw = true;
                }
                None => {}
            }
        }
        if response.drag_stopped() {
            self.drag = None;
            self.recalc();
        }
        if ui.input(|i| i.key_pressed(Key::Escape)) {
            if let Some(Drag::Move { .. }) = self.drag.take() {
                self.undo_active(false);
            }
        }
        if response.hovered() {
            let scroll = ui.input(|i| i.smooth_scroll_delta.y);
            if scroll != 0.0 {
                self.camera.zoom = (self.camera.zoom * (scroll * 0.002).exp()).clamp(0.3, 6.0);
                self.redraw = true;
            }
        }
        if self.redraw {
            ui.ctx().request_repaint();
        }
    }
}
impl eframe::App for HeadMaker {
    fn update(&mut self, ctx: &egui::Context, _: &mut eframe::Frame) {
        if self.expression_mode && !self.editing_points {
            let editor = &mut self.expressions[self.active];
            if editor.playing {
                editor.time += ctx.input(|i| i.stable_dt).min(0.1);
                if editor.time > editor.animation.duration {
                    if editor.looping {
                        editor.time %= editor.animation.duration;
                    } else {
                        editor.time = editor.animation.duration;
                        editor.playing = false;
                    }
                }
                self.redraw = true;
                ctx.request_repaint_after(std::time::Duration::from_millis(16));
            }
        }
        if ctx.input(|i| i.viewport().close_requested()) && !self.allow_close {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            self.request(Pending::Exit, ctx);
        }
        if !ctx.wants_keyboard_input()
            && self.pending.is_none()
            && !self.neck_offer
            && !self.choose_import
        {
            if let Some(redo) = crate::shortcuts::undo_redo(ctx) {
                self.undo_active(redo);
            }
            if crate::shortcuts::pressed(ctx, crate::shortcuts::Command::Save) {
                if self.expression_mode {
                    self.save_face();
                } else {
                    self.save();
                }
            }
            if crate::shortcuts::pressed(ctx, crate::shortcuts::Command::Open) {
                self.request(
                    if self.expression_mode {
                        Pending::ExpressionOpen
                    } else {
                        Pending::Open
                    },
                    ctx,
                );
            }
            if (!self.expression_mode || self.editing_points) && self.drag.is_none() {
                if let Some(action) = crate::shortcuts::consume(ctx) {
                    match action {
                        crate::shortcuts::Action::Axis(axis) => {
                            self.tool = [Tool::X, Tool::Y, Tool::Z][axis];
                            self.keyboard_axis = Some(self.tool);
                            self.status=format!("{} axis selected. Drag a point or its matching arrow. G restores free movement.",["X","Y","Z"][axis]);
                        }
                        crate::shortcuts::Action::Move => {
                            self.controls = Controls::Original;
                            self.tool = Tool::Screen;
                            self.keyboard_axis = None;
                            self.status="Move points: use the arrows or white centre. X / Y / Z select an axis.".into();
                        }
                        crate::shortcuts::Action::Rotate => {
                            self.status="Right-drag rotates the camera. Head points are moved, not rotated.".into();
                        }
                    }
                    self.overlays_visible = true;
                }
            }
            if crate::shortcuts::pressed(ctx, crate::shortcuts::Command::Frame) {
                self.camera = Camera::default();
                self.redraw = true;
            }
        }
        egui::TopBottomPanel::top("toolbar")
            .frame(crate::theme::toolbar_frame())
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    if self.expression_mode {
                        ui.label(crate::theme::title(if self.editing_points {
                            "FACEMAKER · POINTS"
                        } else {
                            "FACEMAKER · XPF"
                        }));
                        let can_save = self.editing_points
                            || !self.expressions[self.active].animation.tracks.is_empty();
                        if ui
                            .add_enabled(
                                can_save,
                                egui::Button::new(if self.docs[self.active].dirty {
                                    "3. Save head + expression…"
                                } else {
                                    "3. Save expression XPF…"
                                }),
                            )
                            .clicked()
                        {
                            self.save_face();
                        }
                    } else {
                        ui.label(crate::theme::title("HEADMAKER · XMF"));
                        if ui.button("Open head…").clicked() {
                            self.request(Pending::Open, ctx);
                        }
                        if ui.add(crate::theme::primary("Save head…")).clicked() {
                            self.save();
                        }
                        if ui.button("Reset head").clicked() {
                            self.request(Pending::Reset, ctx);
                        }
                    }
                    ui.separator();
                    for (name, yaw) in [
                        ("Front", 0.0),
                        ("¾", 0.55),
                        ("Side", std::f32::consts::FRAC_PI_2),
                        ("Back", std::f32::consts::PI),
                    ] {
                        if ui.button(name).clicked() {
                            self.camera = Camera {
                                yaw,
                                ..Default::default()
                            };
                            self.redraw = true;
                        }
                    }
                    if ui.button("Help").clicked() {
                        self.help = true;
                    }
                    let (can_undo, can_redo) = if self.expression_mode && !self.editing_points {
                        (
                            self.expressions[self.active].can_undo(),
                            self.expressions[self.active].can_redo(),
                        )
                    } else {
                        (
                            self.docs[self.active].can_undo(),
                            self.docs[self.active].can_redo(),
                        )
                    };
                    if let Some(redo) = crate::shortcuts::undo_redo_buttons(ui, can_undo, can_redo)
                    {
                        self.undo_active(redo);
                    }
                });
            });
        egui::TopBottomPanel::bottom("status")
            .frame(crate::theme::status_frame())
            .show(ctx, |ui| {
                ui.label(&self.status);
            });
        egui::SidePanel::left("tools")
            .exact_width(306.0)
            .resizable(false)
            .frame(crate::theme::inspector_frame())
            .show(ctx, |ui| {
                ui.add_enabled_ui(
                    !self.neck_offer && !self.choose_import && self.pending.is_none(),
                    |ui| {
                        egui::ScrollArea::vertical()
                            .id_salt(if self.expression_mode {
                                "face_scroll"
                            } else {
                                "head_scroll"
                            })
                            .show(ui, |ui| self.sidebar(ui));
                    },
                );
            });
        egui::CentralPanel::default()
            .frame(egui::Frame::NONE)
            .show(ctx, |ui| self.viewport(ui));
        if let Some(pending) = self.pending {
            egui::Window::new("Unsaved changes")
                .anchor(egui::Align2::CENTER_CENTER, Vec2::ZERO)
                .collapsible(false)
                .resizable(false)
                .show(ctx, |ui| {
                    ui.label(
                        if matches!(pending, Pending::ExpressionOpen | Pending::ExpressionReset) {
                            "This facial expression has unsaved changes."
                        } else if matches!(pending, Pending::Exit) {
                            "There are unsaved head or expression changes."
                        } else {
                            "This head has unsaved edits."
                        },
                    );
                    ui.label("Save before continuing, or discard these changes.");
                    ui.horizontal(|ui| {
                        if ui.button("Save and continue").clicked() {
                            let previous = self.active;
                            let mut success = true;
                            if matches!(pending, Pending::ExpressionOpen | Pending::ExpressionReset)
                            {
                                success = self.save_expression();
                            } else {
                                for a in 0..2 {
                                    self.active = a;
                                    let exiting = matches!(pending, Pending::Exit);
                                    let head_failed = self.docs[a].dirty
                                        && (exiting || a == previous)
                                        && !self.save();
                                    let expression_failed = !head_failed
                                        && exiting
                                        && self.expressions[a].dirty()
                                        && !self.save_expression();
                                    if head_failed || expression_failed {
                                        success = false;
                                        break;
                                    }
                                }
                            }
                            self.active = previous;
                            if success {
                                self.pending = None;
                                self.perform(pending, ctx);
                            }
                        }
                        if ui.button("Discard and continue").clicked() {
                            self.pending = None;
                            self.perform(pending, ctx);
                        }
                        if ui.button("Cancel").clicked() {
                            self.pending = None;
                        }
                    });
                });
        }
        if self.neck_offer {
            egui::Window::new("Old-tool neck gap detected")
                .anchor(egui::Align2::CENTER_CENTER, Vec2::ZERO)
                .collapsible(false)
                .resizable(false)
                .show(ctx, |ui| {
                    ui.label(
                        "This mesh matches the raised neck edge from the original Flash tool.",
                    );
                    ui.label(
                        "Restore the original neck transition while keeping the rest of the face?",
                    );
                    ui.horizontal(|ui| {
                        if ui.button("Fix neck").clicked() {
                            match self.docs[self.active].repair_neck() {
                                Ok(()) => {
                                    self.recalc();
                                    self.status =
                                        "Neck repaired. Save the XMF to keep this change.".into();
                                }
                                Err(e) => self.status = e,
                            }
                            self.neck_offer = false;
                        }
                        if ui.button("Keep as-is").clicked() {
                            self.neck_offer = false;
                        }
                    });
                });
        }
        if self.choose_import {
            egui::Window::new("Open a female or male head?")
                .anchor(egui::Align2::CENTER_CENTER, Vec2::ZERO)
                .collapsible(false)
                .resizable(false)
                .show(ctx, |ui| {
                    ui.label("Choose which head this XMF belongs to before selecting the file.");
                    ui.label("This selects the matching textures, neck repair and editing rules.");
                    ui.horizontal(|ui| {
                        if ui.button("Female head").clicked() {
                            self.choose_import = false;
                            self.request(Pending::OpenAs(0), ctx);
                        }
                        if ui.button("Male head").clicked() {
                            self.choose_import = false;
                            self.request(Pending::OpenAs(1), ctx);
                        }
                        if ui.button("Cancel").clicked() {
                            self.choose_import = false;
                        }
                    });
                });
        }
        egui::Window::new("Using HeadMaker")
            .open(&mut self.help)
            .resizable(false)
            .show(ctx, |ui| {
                let lines = if self.expression_mode {
                    FACEMAKER_HELP
                } else {
                    HEADMAKER_HELP
                };
                for line in lines {
                    ui.label(*line);
                }
            });
    }
}
