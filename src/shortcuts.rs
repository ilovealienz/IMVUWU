use eframe::egui::{self, Key, Modifiers};
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Action {
    Axis(usize),
    Move,
    Rotate,
}

/// Every rebindable keyboard command. The order is the Settings order.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Command {
    Undo,
    Redo,
    Save,
    Open,
    Frame,
    AxisX,
    AxisY,
    AxisZ,
    Move,
    Rotate,
}

impl Command {
    pub const ALL: [Command; 10] = [
        Command::Undo,
        Command::Redo,
        Command::Save,
        Command::Open,
        Command::Frame,
        Command::AxisX,
        Command::AxisY,
        Command::AxisZ,
        Command::Move,
        Command::Rotate,
    ];
    pub fn label(self) -> &'static str {
        match self {
            Command::Undo => "Undo",
            Command::Redo => "Redo",
            Command::Save => "Save",
            Command::Open => "Open",
            Command::Frame => "Reset / frame view",
            Command::AxisX => "X axis",
            Command::AxisY => "Y axis",
            Command::AxisZ => "Z axis",
            Command::Move => "Free move / move gizmo",
            Command::Rotate => "Joint rotation",
        }
    }
    /// Key in settings.json.
    pub fn id(self) -> &'static str {
        match self {
            Command::Undo => "undo",
            Command::Redo => "redo",
            Command::Save => "save",
            Command::Open => "open",
            Command::Frame => "frame",
            Command::AxisX => "axis_x",
            Command::AxisY => "axis_y",
            Command::AxisZ => "axis_z",
            Command::Move => "move",
            Command::Rotate => "rotate",
        }
    }
    pub fn default_binding(self) -> Binding {
        let (key, ctrl, shift) = match self {
            Command::Undo => (Key::Z, true, false),
            Command::Redo => (Key::Z, true, true),
            Command::Save => (Key::S, true, false),
            Command::Open => (Key::O, true, false),
            Command::Frame => (Key::F, false, false),
            Command::AxisX => (Key::X, false, false),
            Command::AxisY => (Key::Y, false, false),
            Command::AxisZ => (Key::Z, false, false),
            Command::Move => (Key::G, false, false),
            Command::Rotate => (Key::R, false, false),
        };
        Binding {
            key,
            ctrl,
            shift,
            alt: false,
        }
    }
}

/// A key with exact modifiers ("Ctrl+Shift+Z" matches only that).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Binding {
    pub key: Key,
    pub ctrl: bool,
    pub shift: bool,
    pub alt: bool,
}

impl Binding {
    pub fn from_event(key: Key, m: Modifiers) -> Self {
        Self {
            key,
            ctrl: m.ctrl || m.command,
            shift: m.shift,
            alt: m.alt,
        }
    }
    pub fn text(self) -> String {
        let mut parts = Vec::new();
        if self.ctrl {
            // The same binding is Ctrl on Windows/Linux and Cmd on macOS
            // (egui reports Cmd as `command`; `from_event` folds both).
            parts.push(if cfg!(target_os = "macos") {
                "Cmd"
            } else {
                "Ctrl"
            });
        }
        if self.shift {
            parts.push("Shift");
        }
        if self.alt {
            parts.push("Alt");
        }
        parts.push(self.key.name());
        parts.join("+")
    }
    pub fn parse(text: &str) -> Option<Self> {
        let mut binding = Binding {
            key: Key::Escape,
            ctrl: false,
            shift: false,
            alt: false,
        };
        let mut key = None;
        for part in text.split('+').map(str::trim) {
            match part.to_ascii_lowercase().as_str() {
                "ctrl" | "cmd" | "command" => binding.ctrl = true,
                "shift" => binding.shift = true,
                "alt" => binding.alt = true,
                _ => key = Some(Key::from_name(part)?),
            }
        }
        binding.key = key?;
        Some(binding)
    }
    fn matches(self, key: Key, m: Modifiers) -> bool {
        self == Binding::from_event(key, m)
    }
}

/// The keymap in use: settings overrides on top of the defaults.
static KEYMAP: std::sync::RwLock<Vec<(Command, Binding)>> = std::sync::RwLock::new(Vec::new());

/// Replaces the keymap overrides (commands not listed use their default).
pub fn set_keymap(overrides: &[(Command, Binding)]) {
    if let Ok(mut map) = KEYMAP.write() {
        *map = overrides.to_vec();
    }
}

pub fn binding(command: Command) -> Binding {
    KEYMAP
        .read()
        .ok()
        .and_then(|map| map.iter().find(|(c, _)| *c == command).map(|(_, b)| *b))
        .unwrap_or_else(|| command.default_binding())
}

/// The key text for hints, e.g. "Ctrl+Z".
pub fn text(command: Command) -> String {
    binding(command).text()
}

/// Removes one press of exactly `binding` from this frame's input.
fn take(i: &mut egui::InputState, binding: Binding) -> bool {
    let found = i.events.iter().position(|e| {
        matches!(e, egui::Event::Key { key, pressed: true, modifiers, .. }
            if binding.matches(*key, *modifiers))
    });
    if let Some(index) = found {
        i.events.remove(index);
    }
    found.is_some()
}

/// Whether `command`'s key was pressed this frame (consumed). Never fires
/// while a text field has focus.
pub fn pressed(ctx: &egui::Context, command: Command) -> bool {
    if ctx.wants_keyboard_input() {
        return false;
    }
    let binding = binding(command);
    ctx.input_mut(|i| take(i, binding))
}

/// Undo/redo keys: `Some(false)` = undo, `Some(true)` = redo. Ctrl+Y also
/// redoes while no command is bound to it.
pub fn undo_redo(ctx: &egui::Context) -> Option<bool> {
    if ctx.wants_keyboard_input() {
        return None;
    }
    let ctrl_y = Binding {
        key: Key::Y,
        ctrl: true,
        shift: false,
        alt: false,
    };
    let ctrl_y_free = Command::ALL.iter().all(|c| binding(*c) != ctrl_y);
    let (undo, redo) = (binding(Command::Undo), binding(Command::Redo));
    ctx.input_mut(|i| {
        if take(i, redo) || (ctrl_y_free && take(i, ctrl_y)) {
            Some(true)
        } else if take(i, undo) {
            Some(false)
        } else {
            None
        }
    })
}

/// Undo and redo arrow buttons for the right end of a workspace toolbar.
/// Returns `Some(false)` for undo, `Some(true)` for redo.
pub fn undo_redo_buttons(ui: &mut egui::Ui, can_undo: bool, can_redo: bool) -> Option<bool> {
    let mut result = None;
    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
        let redo = arrow_button(ui, can_redo, true)
            .on_hover_text(format!("Redo ({})", text(Command::Redo)));
        let undo = arrow_button(ui, can_undo, false)
            .on_hover_text(format!("Undo ({})", text(Command::Undo)));
        if redo.clicked() {
            result = Some(true);
        }
        if undo.clicked() {
            result = Some(false);
        }
    });
    result
}

/// A small button with a drawn curved arrow (the default fonts have no
/// curved-arrow glyphs): pointing left for undo, mirrored for redo.
fn arrow_button(ui: &mut egui::Ui, enabled: bool, redo: bool) -> egui::Response {
    let response = ui.add_enabled(
        enabled,
        egui::Button::new("").min_size(egui::vec2(32.0, 24.0)),
    );
    let colour = ui.style().interact(&response).fg_stroke.color;
    let colour = if enabled {
        colour
    } else {
        colour.gamma_multiply(0.4)
    };
    let centre = response.rect.center() + egui::vec2(0.0, 3.0);
    let radius = 6.5;
    let flip = if redo { -1.0 } else { 1.0 };
    // A half circle over the top; the arrowhead sits at the open end.
    let points: Vec<egui::Pos2> = (0..=12)
        .map(|step| {
            let angle = std::f32::consts::PI * (step as f32 / 12.0);
            centre + egui::vec2(flip * radius * angle.cos(), -radius * angle.sin())
        })
        .collect();
    let tip = *points.last().unwrap();
    let painter = ui.painter();
    painter.add(egui::Shape::line(
        points,
        egui::Stroke::new(1.8_f32, colour),
    ));
    painter.add(egui::Shape::convex_polygon(
        vec![
            tip + egui::vec2(0.0, 4.5),
            tip + egui::vec2(-flip * 4.0, -1.5),
            tip + egui::vec2(flip * 4.0, -1.5),
        ],
        colour,
        egui::Stroke::NONE,
    ));
    response
}

pub fn consume(ctx: &egui::Context) -> Option<Action> {
    [
        (Command::AxisX, Action::Axis(0)),
        (Command::AxisY, Action::Axis(1)),
        (Command::AxisZ, Action::Axis(2)),
        (Command::Move, Action::Move),
        (Command::Rotate, Action::Rotate),
    ]
    .into_iter()
    .find(|(command, _)| pressed(ctx, *command))
    .map(|(_, action)| action)
}
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bindings_round_trip_as_text_and_match_modifiers_exactly() {
        for command in Command::ALL {
            let b = command.default_binding();
            assert_eq!(Binding::parse(&b.text()), Some(b));
        }
        assert_eq!(Command::Redo.default_binding().text(), "Ctrl+Shift+Z");
        assert_eq!(Binding::parse("ctrl + nonsense"), None);
        let undo = Command::Undo.default_binding();
        assert!(undo.matches(Key::Z, Modifiers::CTRL));
        assert!(!undo.matches(Key::Z, Modifiers::CTRL | Modifiers::SHIFT));
    }
    fn input(key: Key, modifiers: Modifiers) -> egui::RawInput {
        egui::RawInput {
            modifiers,
            events: vec![egui::Event::Key {
                key,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers,
            }],
            ..Default::default()
        }
    }
    #[test]
    fn plain_axis_keys_are_consumed_once() {
        for (key, axis) in [(Key::X, 0), (Key::Y, 1), (Key::Z, 2)] {
            let ctx = egui::Context::default();
            let _ = ctx.run(input(key, Modifiers::NONE), |ctx| {
                assert_eq!(consume(ctx), Some(Action::Axis(axis)));
                assert_eq!(consume(ctx), None);
            });
        }
    }
    #[test]
    fn control_undo_is_not_interpreted_as_axis_z() {
        let ctx = egui::Context::default();
        let _ = ctx.run(input(Key::Z, Modifiers::CTRL), |ctx| {
            assert_eq!(consume(ctx), None);
            assert!(ctx.input_mut(|i| i.consume_key(Modifiers::CTRL, Key::Z)));
        });
    }
    #[test]
    fn control_shift_z_and_control_y_redo_while_control_z_undoes() {
        for (key, modifiers, expected) in [
            (Key::Z, Modifiers::CTRL | Modifiers::SHIFT, Some(true)),
            (Key::Y, Modifiers::CTRL, Some(true)),
            (Key::Z, Modifiers::CTRL, Some(false)),
            (Key::Z, Modifiers::NONE, None),
        ] {
            let ctx = egui::Context::default();
            let _ = ctx.run(input(key, modifiers), |ctx| {
                assert_eq!(undo_redo(ctx), expected, "{key:?} {modifiers:?}");
                // Consumed once: a second check in the same frame sees nothing.
                assert_eq!(undo_redo(ctx), None);
            });
        }
    }
    #[test]
    fn typing_in_text_fields_does_not_switch_tools() {
        let ctx = egui::Context::default();
        let mut text = String::new();
        let _ = ctx.run(Default::default(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                ui.text_edit_singleline(&mut text).request_focus();
            });
        });
        let _ = ctx.run(input(Key::X, Modifiers::NONE), |ctx| {
            assert_eq!(consume(ctx), None);
        });
    }
}
