//! Customisable keyboard shortcuts: every command a key can run, its default
//! keys, and the user's own (Settings → Keyboard Shortcuts), kept in
//! `settings.json` as text such as `"Ctrl+Shift+Z"`.
//!
//! A press runs the command whose keys match it exactly (modifiers
//! included), so Ctrl+Shift+E and Ctrl+E never both fire. Digit and symbol
//! keys also match by their place on the keyboard (see
//! [`crate::app::input::keyboard`]), Shift aside, so brush size is the same
//! two keys on AZERTY without Shift.

use eframe::egui::{self, Key, Modifiers};
use std::collections::BTreeMap;

/// A command a shortcut can run.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Action {
    // Tools
    Brush,
    Eraser,
    RectSelect,
    Lasso,
    Wand,
    Shapes,
    Gradient,
    Ruler,
    Transform,
    Animate,
    Eyedropper,
    Fill,
    Liquify,
    Blend,
    // Brush & colour
    SmallerBrush,
    BiggerBrush,
    Presets,
    Palette,
    SwapColors,
    // View
    TogglePanels,
    ZoomIn,
    ZoomOut,
    FitView,
    ActualPixels,
    FlipView,
    Grid,
    Guides,
    // Animation
    PreviousFrame,
    NextFrame,
    PlayAnimation,
    FirstFrame,
    LastFrame,
    PreviousDrawing,
    NextDrawing,
    NewDrawing,
    CopyDrawing,
    RemoveDrawing,
    HoldLonger,
    HoldShorter,
    ToggleOnion,
    KeyMotion,
    NewAnimationLayer,
    // Edit
    Undo,
    Redo,
    History,
    NewLayer,
    DuplicateLayer,
    NewFolder,
    ClipToBelow,
    MergeDown,
    MergeVisible,
    AlphaLock,
    // Selection
    SelectAll,
    Deselect,
    InvertSelection,
    DeletePixels,
    ContentFill,
    QuickMask,
    // File
    NewCanvas,
    Import,
    Open,
    Save,
    Export,
}

/// One key with its modifiers. `command` is Ctrl (⌘ on a Mac).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Binding {
    pub command: bool,
    pub shift: bool,
    pub alt: bool,
    pub key: Key,
}

impl Binding {
    pub const fn new(modifiers: Modifiers, key: Key) -> Self {
        Self {
            command: modifiers.command,
            shift: modifiers.shift,
            alt: modifiers.alt,
            key,
        }
    }

    pub fn modifiers(&self) -> Modifiers {
        Modifiers {
            alt: self.alt,
            ctrl: false,
            shift: self.shift,
            mac_cmd: false,
            command: self.command,
        }
    }

    /// As kept in `settings.json`: `Ctrl+Shift+Z`, `F5`, `OpenBracket`.
    pub fn to_text(self) -> String {
        let mut text = String::new();
        for (on, name) in [
            (self.command, "Ctrl+"),
            (self.alt, "Alt+"),
            (self.shift, "Shift+"),
        ] {
            if on {
                text += name;
            }
        }
        text + self.key.name()
    }

    /// Read [`Binding::to_text`]'s form back (any order of modifiers,
    /// `Cmd` for `Ctrl`).
    pub fn from_text(text: &str) -> Option<Self> {
        let mut binding = Binding::new(Modifiers::NONE, Key::A);
        // Keys are named in words (`Plus`, `Minus`), so `+` only separates.
        let mut parts: Vec<&str> = text.split('+').collect();
        let key = parts.pop()?;
        for part in parts {
            match part.trim().to_ascii_lowercase().as_str() {
                "ctrl" | "cmd" | "command" => binding.command = true,
                "shift" => binding.shift = true,
                "alt" | "option" => binding.alt = true,
                _ => return None,
            }
        }
        binding.key = Key::from_name(key.trim())?;
        Some(binding)
    }

    /// Whether this is the key pressed: `key` is what it typed,
    /// `position` where it is (its QWERTY name).
    fn matches(&self, key: Key, position: Option<Key>, m: Modifiers) -> bool {
        // Ctrl on a Mac is its own key, not ⌘: a shortcut without ⌘ wants
        // neither.
        let mods = self.command == m.command && self.alt == m.alt && (self.command || !m.ctrl);
        if !mods {
            return false;
        }
        if self.key == key && self.shift == m.shift {
            return true;
        }
        // Digits and symbols by place, Shift aside: some layouts need it
        // to type them.
        super::keyboard::is_position_key(self.key) && position == Some(self.key)
    }

    /// A key just pressed, as a shortcut: digits and symbols by their
    /// place (so the same key works whatever it types), letters and named
    /// keys by what they are. Shift is left out of a place key, which
    /// matches with or without it.
    pub fn recorded(key: Key, position: Option<Key>, m: Modifiers) -> Self {
        let place = position.filter(|&p| super::keyboard::is_position_key(p));
        let mut binding = Self::new(m, place.unwrap_or(key));
        if place.is_some() {
            binding.shift = false;
        }
        binding
    }

    /// As printed on this keyboard (position keys by their keycap).
    pub fn label(&self, ctx: &egui::Context) -> String {
        super::keyboard::shortcut_label(ctx, self.modifiers(), self.key)
    }
}

/// Everything about one action: its settings name, what it's called, its
/// group in the lists and its keys out of the box.
pub struct ActionInfo {
    pub action: Action,
    pub id: &'static str,
    pub label: &'static str,
    pub group: &'static str,
    pub defaults: &'static [Binding],
}

const NONE: Modifiers = Modifiers::NONE;
const CMD: Modifiers = Modifiers::COMMAND;
const SHIFT: Modifiers = Modifiers::SHIFT;
const CMD_SHIFT: Modifiers = Modifiers {
    shift: true,
    ..Modifiers::COMMAND
};
const CMD_ALT: Modifiers = Modifiers {
    alt: true,
    ..Modifiers::COMMAND
};
const ALT: Modifiers = Modifiers::ALT;

macro_rules! b {
    ($m:expr, $k:ident) => {
        Binding::new($m, Key::$k)
    };
}

/// Every action, in the order the lists show them.
pub const ACTIONS: &[ActionInfo] = &[
    info(Action::Brush, "brush", "Brush", "Tools", &[b!(NONE, B)]),
    info(Action::Eraser, "eraser", "Eraser", "Tools", &[b!(NONE, E)]),
    info(
        Action::RectSelect,
        "rect_select",
        "Rectangle / ellipse select",
        "Tools",
        &[b!(NONE, M)],
    ),
    info(
        Action::Lasso,
        "lasso",
        "Lasso select (again: polygon, magnetic)",
        "Tools",
        &[b!(NONE, L)],
    ),
    info(
        Action::Wand,
        "wand",
        "Magic wand (again: colour range)",
        "Tools",
        &[b!(NONE, Q)],
    ),
    info(
        Action::Shapes,
        "shapes",
        "Shapes (again: next shape)",
        "Tools",
        &[b!(NONE, U)],
    ),
    info(
        Action::Gradient,
        "gradient",
        "Gradient",
        "Tools",
        &[b!(SHIFT, G)],
    ),
    info(
        Action::Ruler,
        "ruler",
        "Show / hide the ruler",
        "Tools",
        &[b!(NONE, R)],
    ),
    info(
        Action::Transform,
        "transform",
        "Transform",
        "Tools",
        &[b!(NONE, V), b!(NONE, T)],
    ),
    info(
        Action::Animate,
        "animate",
        "Animate (move, scale, turn, keyed)",
        "Tools",
        &[b!(NONE, A)],
    ),
    info(
        Action::Eyedropper,
        "eyedropper",
        "Eyedropper",
        "Tools",
        &[b!(NONE, I)],
    ),
    info(
        Action::Fill,
        "fill",
        "Fill (again: bucket / enclose / lasso delete)",
        "Tools",
        &[b!(NONE, G)],
    ),
    info(
        Action::Liquify,
        "liquify",
        "Liquify",
        "Tools",
        &[b!(NONE, W)],
    ),
    info(
        Action::Blend,
        "blend",
        "Smudge (again: blur)",
        "Tools",
        &[b!(NONE, S)],
    ),
    info(
        Action::SmallerBrush,
        "smaller_brush",
        "Smaller brush",
        "Brush & colour",
        &[b!(NONE, OpenBracket)],
    ),
    info(
        Action::BiggerBrush,
        "bigger_brush",
        "Larger brush",
        "Brush & colour",
        &[b!(NONE, CloseBracket)],
    ),
    info(
        Action::Presets,
        "presets",
        "Brush presets window",
        "Brush & colour",
        &[b!(NONE, P)],
    ),
    info(
        Action::Palette,
        "palette",
        "Pop-up palette (hold or tap)",
        "Brush & colour",
        &[b!(NONE, K)],
    ),
    info(
        Action::SwapColors,
        "swap_colors",
        "Swap brush and secondary colour",
        "Brush & colour",
        &[b!(NONE, X)],
    ),
    info(
        Action::TogglePanels,
        "panels",
        "Show / hide panels",
        "View",
        &[b!(NONE, Tab)],
    ),
    info(
        Action::ZoomIn,
        "zoom_in",
        "Zoom in",
        "View",
        &[b!(CMD, Equals), b!(CMD, Plus)],
    ),
    info(
        Action::ZoomOut,
        "zoom_out",
        "Zoom out",
        "View",
        &[b!(CMD, Minus)],
    ),
    info(
        Action::FitView,
        "fit",
        "Fit to window",
        "View",
        &[b!(CMD, Num0)],
    ),
    info(
        Action::PreviousFrame,
        "previous_frame",
        "Previous frame",
        "Animation",
        &[b!(NONE, Comma)],
    ),
    info(
        Action::NextFrame,
        "next_frame",
        "Next frame",
        "Animation",
        &[b!(NONE, Period)],
    ),
    info(
        Action::PlayAnimation,
        "play_animation",
        "Play / pause the animation",
        "Animation",
        &[b!(SHIFT, Space)],
    ),
    info(
        Action::FirstFrame,
        "first_frame",
        "First frame",
        "Animation",
        &[b!(NONE, Home)],
    ),
    info(
        Action::LastFrame,
        "last_frame",
        "Last frame",
        "Animation",
        &[b!(NONE, End)],
    ),
    info(
        Action::PreviousDrawing,
        "previous_drawing",
        "Previous drawing or key",
        "Animation",
        &[b!(ALT, Comma)],
    ),
    info(
        Action::NextDrawing,
        "next_drawing",
        "Next drawing or key",
        "Animation",
        &[b!(ALT, Period)],
    ),
    info(
        Action::ToggleOnion,
        "toggle_onion",
        "Onion skin on / off",
        "Animation",
        &[b!(NONE, O)],
    ),
    info(
        Action::ActualPixels,
        "actual_pixels",
        "Actual pixels",
        "View",
        &[b!(CMD, Num1)],
    ),
    info(
        Action::FlipView,
        "flip_view",
        "Flip the view horizontally",
        "View",
        &[b!(NONE, H)],
    ),
    info(
        Action::Grid,
        "grid",
        "Show / hide the grid",
        "View",
        &[b!(CMD, Quote)],
    ),
    info(
        Action::Guides,
        "guides",
        "Show / hide the guides",
        "View",
        &[b!(CMD, Semicolon)],
    ),
    info(Action::Undo, "undo", "Undo", "Edit", &[b!(CMD, Z)]),
    info(
        Action::Redo,
        "redo",
        "Redo",
        "Edit",
        &[b!(CMD_SHIFT, Z), b!(CMD, Y)],
    ),
    info(
        Action::History,
        "history",
        "History panel",
        "Edit",
        &[b!(CMD, H)],
    ),
    info(
        Action::NewLayer,
        "new_layer",
        "New layer",
        "Edit",
        &[b!(CMD_SHIFT, N)],
    ),
    info(
        Action::DuplicateLayer,
        "duplicate_layer",
        "Duplicate layer",
        "Edit",
        &[b!(CMD, J)],
    ),
    info(
        Action::NewFolder,
        "new_folder",
        "New folder",
        "Edit",
        &[b!(CMD, G)],
    ),
    info(
        Action::ClipToBelow,
        "clip",
        "Clip to the layer below",
        "Edit",
        &[b!(CMD_ALT, G)],
    ),
    info(
        Action::MergeDown,
        "merge_down",
        "Merge down",
        "Edit",
        &[b!(CMD_ALT, E)],
    ),
    info(
        Action::MergeVisible,
        "merge_visible",
        "Merge visible",
        "Edit",
        &[b!(CMD_SHIFT, E)],
    ),
    info(
        Action::AlphaLock,
        "alpha_lock",
        "Lock layer transparency",
        "Edit",
        &[b!(NONE, Slash)],
    ),
    info(
        Action::SelectAll,
        "select_all",
        "Select all",
        "Selection",
        &[b!(CMD, A)],
    ),
    info(
        Action::Deselect,
        "deselect",
        "Deselect",
        "Selection",
        &[b!(CMD, D)],
    ),
    info(
        Action::InvertSelection,
        "invert_selection",
        "Invert selection",
        "Selection",
        &[b!(CMD_SHIFT, I)],
    ),
    info(
        Action::DeletePixels,
        "delete",
        "Delete the selected pixels, or the selected layer when nothing is selected (a lasso or shape: its last point)",
        "Selection",
        &[b!(NONE, Delete), b!(NONE, Backspace)],
    ),
    info(
        Action::ContentFill,
        "content_fill",
        "Fill the selection from its surroundings",
        "Selection",
        &[b!(SHIFT, F5)],
    ),
    info(
        Action::QuickMask,
        "quick_mask",
        "Quick mask: paint the selection",
        "Selection",
        &[b!(SHIFT, Q)],
    ),
    info(
        Action::NewCanvas,
        "new_canvas",
        "New canvas",
        "File",
        &[b!(CMD, N)],
    ),
    info(
        Action::Import,
        "import",
        "Import image as a layer",
        "File",
        &[b!(CMD_SHIFT, O)],
    ),
    info(Action::Open, "open", "Open project", "File", &[b!(CMD, O)]),
    info(Action::Save, "save", "Save project", "File", &[b!(CMD, S)]),
    info(
        Action::Export,
        "export",
        "Export image",
        "File",
        &[b!(CMD, E)],
    ),
    info(
        Action::NewDrawing,
        "new_drawing",
        "New blank drawing here",
        "Drawings & keys",
        &[b!(NONE, N)],
    ),
    info(
        Action::CopyDrawing,
        "copy_drawing",
        "New drawing here, a copy of the one showing",
        "Drawings & keys",
        &[b!(SHIFT, N)],
    ),
    info(
        Action::RemoveDrawing,
        "remove_drawing",
        "Remove the drawing starting here",
        "Drawings & keys",
        &[b!(SHIFT, Delete)],
    ),
    info(
        Action::HoldLonger,
        "hold_longer",
        "Hold the drawing showing a frame longer",
        "Drawings & keys",
        &[b!(ALT, Equals), b!(ALT, Plus)],
    ),
    info(
        Action::HoldShorter,
        "hold_shorter",
        "Hold the drawing showing a frame shorter",
        "Drawings & keys",
        &[b!(ALT, Minus)],
    ),
    info(
        Action::KeyMotion,
        "key_motion",
        "Key the layer's motion here",
        "Drawings & keys",
        &[b!(ALT, K)],
    ),
    info(
        Action::NewAnimationLayer,
        "new_animation_layer",
        "New animated layer",
        "Drawings & keys",
        &[b!(CMD_ALT, N)],
    ),
];

const fn info(
    action: Action,
    id: &'static str,
    label: &'static str,
    group: &'static str,
    defaults: &'static [Binding],
) -> ActionInfo {
    ActionInfo {
        action,
        id,
        label,
        group,
        defaults,
    }
}

impl Action {
    pub fn info(self) -> &'static ActionInfo {
        ACTIONS
            .iter()
            .find(|i| i.action == self)
            .expect("every action is listed")
    }
}

/// The keys each action has: its defaults unless the user changed them.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Keymap {
    /// Actions the user changed, and their keys (none: no shortcut).
    custom: BTreeMap<Action, Vec<Binding>>,
}

impl Keymap {
    pub fn bindings(&self, action: Action) -> &[Binding] {
        self.custom
            .get(&action)
            .map_or(action.info().defaults, |b| b.as_slice())
    }

    pub fn is_default(&self, action: Action) -> bool {
        !self.custom.contains_key(&action)
    }

    /// Give `action` these keys (its defaults drop the custom entry).
    pub fn set(&mut self, action: Action, bindings: Vec<Binding>) {
        if bindings == action.info().defaults {
            self.custom.remove(&action);
        } else {
            self.custom.insert(action, bindings);
        }
    }

    /// Add `binding` to `action`, taking it from any action that had it.
    /// Returns the actions it was taken from.
    pub fn assign(&mut self, action: Action, binding: Binding) -> Vec<Action> {
        let mut taken = Vec::new();
        for info in ACTIONS {
            let other = info.action;
            let keys = self.bindings(other);
            if other != action && keys.contains(&binding) {
                let kept: Vec<Binding> = keys.iter().copied().filter(|b| *b != binding).collect();
                self.set(other, kept);
                taken.push(other);
            }
        }
        let mut keys = self.bindings(action).to_vec();
        if !keys.contains(&binding) {
            keys.push(binding);
        }
        self.set(action, keys);
        taken
    }

    pub fn reset(&mut self, action: Action) {
        self.custom.remove(&action);
    }

    pub fn reset_all(&mut self) {
        self.custom.clear();
    }

    /// The first key of `action`, as printed on this keyboard (for menus).
    pub fn label(&self, ctx: &egui::Context, action: Action) -> Option<String> {
        self.bindings(action).first().map(|b| b.label(ctx))
    }

    /// All keys of `action`, as printed on this keyboard, or "None".
    pub fn labels(&self, ctx: &egui::Context, action: Action) -> String {
        let keys = self.bindings(action);
        if keys.is_empty() {
            return "None".into();
        }
        keys.iter()
            .map(|b| b.label(ctx))
            .collect::<Vec<_>>()
            .join(", ")
    }

    /// The action a key press runs: an exact match first, then one by the
    /// key's place with Shift aside.
    pub fn action_for_press(
        &self,
        key: Key,
        position: Option<Key>,
        m: Modifiers,
    ) -> Option<Action> {
        let exact = ACTIONS.iter().map(|i| i.action).find(|&a| {
            self.bindings(a)
                .iter()
                .any(|b| b.key == key && b.matches(key, None, m))
        });
        exact.or_else(|| {
            ACTIONS
                .iter()
                .map(|i| i.action)
                .find(|&a| self.bindings(a).iter().any(|b| b.matches(key, position, m)))
        })
    }

    /// Take this frame's key presses that run an action, in order.
    pub fn take_actions(&self, ctx: &egui::Context) -> Vec<Action> {
        ctx.input_mut(|i| {
            let mut actions = Vec::new();
            i.events.retain(|e| {
                if let egui::Event::Key {
                    key,
                    physical_key,
                    pressed: true,
                    modifiers,
                    ..
                } = *e
                    && let Some(action) = self.action_for_press(key, physical_key, modifiers)
                {
                    actions.push(action);
                    return false;
                }
                true
            });
            actions
        })
    }

    /// The user's changes, for `settings.json`.
    pub fn to_settings(&self) -> BTreeMap<String, Vec<String>> {
        self.custom
            .iter()
            .map(|(a, keys)| {
                (
                    a.info().id.to_string(),
                    keys.iter().map(|b| b.to_text()).collect(),
                )
            })
            .collect()
    }

    /// From `settings.json`; unknown actions and keys are skipped.
    pub fn from_settings(settings: &BTreeMap<String, Vec<String>>) -> Self {
        let mut map = Self::default();
        for (id, keys) in settings {
            let Some(info) = ACTIONS.iter().find(|i| i.id == id) else {
                log::warn!("Unknown shortcut action {id} in settings.json");
                continue;
            };
            let keys = keys.iter().filter_map(|k| Binding::from_text(k)).collect();
            map.set(info.action, keys);
        }
        map
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn press(key: Key, m: Modifiers) -> Option<Action> {
        Keymap::default().action_for_press(key, Some(key), m)
    }

    #[test]
    fn presses_run_the_action_with_exactly_those_modifiers() {
        assert_eq!(press(Key::E, Modifiers::COMMAND), Some(Action::Export));
        assert_eq!(press(Key::E, CMD_SHIFT), Some(Action::MergeVisible));
        assert_eq!(press(Key::E, CMD_ALT), Some(Action::MergeDown));
        assert_eq!(press(Key::E, Modifiers::NONE), Some(Action::Eraser));
        assert_eq!(press(Key::Z, CMD_SHIFT), Some(Action::Redo));
        assert_eq!(press(Key::Z, Modifiers::COMMAND), Some(Action::Undo));
        assert_eq!(press(Key::Q, Modifiers::SHIFT), Some(Action::QuickMask));
        assert_eq!(press(Key::Q, Modifiers::NONE), Some(Action::Wand));
        assert_eq!(press(Key::Z, Modifiers::NONE), None);
    }

    #[test]
    fn digit_and_symbol_keys_also_go_by_place() {
        let map = Keymap::default();
        // AZERTY: the key where QWERTY has `[` types `^` (egui: no key), with
        // or without Shift.
        let on_azerty = |m| map.action_for_press(Key::Backtick, Some(Key::OpenBracket), m);
        assert_eq!(on_azerty(Modifiers::NONE), Some(Action::SmallerBrush));
        assert_eq!(on_azerty(Modifiers::SHIFT), Some(Action::SmallerBrush));
        // Ctrl + the `0` key (which types `à`, no egui key) fits the view.
        assert_eq!(
            map.action_for_press(Key::F20, Some(Key::Num0), Modifiers::COMMAND),
            Some(Action::FitView)
        );
    }

    #[test]
    fn position_shortcuts_match_the_key_not_the_character() {
        let map = Keymap::default();
        let ctrl = Modifiers {
            ctrl: true,
            command: true,
            ..Default::default()
        };
        // AZERTY: the 0 key types à (unnamed, so egui passes the position).
        assert_eq!(
            map.action_for_press(Key::Num0, Some(Key::Num0), ctrl),
            Some(Action::FitView)
        );
        // QWERTZ: the / key types -, which egui does name.
        assert_eq!(
            map.action_for_press(Key::Minus, Some(Key::Slash), Modifiers::NONE),
            Some(Action::AlphaLock)
        );
        // Ctrl on it is Ctrl+-, zoom out, not the transparency lock.
        assert_eq!(
            map.action_for_press(Key::Minus, Some(Key::Slash), ctrl),
            Some(Action::ZoomOut)
        );
    }

    #[test]
    fn keys_round_trip_as_text() {
        for info in ACTIONS {
            for b in info.defaults {
                assert_eq!(
                    Binding::from_text(&b.to_text()),
                    Some(*b),
                    "{}",
                    b.to_text()
                );
            }
        }
        assert_eq!(
            Binding::from_text("shift+ctrl+z"),
            Some(Binding::new(CMD_SHIFT, Key::Z))
        );
        assert_eq!(Binding::from_text("Ctrl+"), None);
        assert_eq!(Binding::from_text("Hyper+Z"), None);
        assert_eq!(Binding::from_text("Nope"), None);
    }

    #[test]
    fn a_key_given_to_one_action_leaves_the_other() {
        let mut map = Keymap::default();
        let b = Binding::new(Modifiers::NONE, Key::B);
        let taken = map.assign(Action::Eyedropper, b);
        assert_eq!(taken, vec![Action::Brush]);
        assert_eq!(press_with(&map, Key::B), Some(Action::Eyedropper));
        assert!(map.bindings(Action::Brush).is_empty());
        // Saved and read back.
        let back = Keymap::from_settings(&map.to_settings());
        assert_eq!(back, map);
        // Put back as it was, it's the default again.
        map.set(Action::Brush, vec![b]);
        map.reset(Action::Eyedropper);
        assert_eq!(map, Keymap::default());
        assert!(map.to_settings().is_empty());
    }

    #[test]
    fn a_changed_shortcut_runs_its_new_command_in_the_app() {
        use crate::app::tools::Tool;
        let mut app = crate::project::tests::test_app_pub(crate::canvas::Canvas::new(
            64,
            64,
            egui::Color32::WHITE,
            64,
        ));
        app.workspace
            .keymap
            .assign(Action::Eyedropper, Binding::new(Modifiers::NONE, Key::B));
        let ctx = egui::Context::default();
        let press = |key| egui::Event::Key {
            key,
            physical_key: Some(key),
            pressed: true,
            repeat: false,
            modifiers: Modifiers::NONE,
        };
        let frame = |app: &mut crate::PainterApp, key| {
            let input = egui::RawInput {
                events: vec![press(key)],
                ..Default::default()
            };
            let _ = ctx.run(input, |ctx| {
                crate::app::input::shortcuts::handle_shortcuts(app, ctx);
            });
        };
        frame(&mut app, Key::B);
        assert!(matches!(app.active_tool, Tool::Eyedropper));
        // I still picks the eyedropper too; E the eraser.
        frame(&mut app, Key::E);
        assert!(app.is_eraser_active());
        // Esc stays Esc whatever Deselect is on.
        app.select_all();
        app.workspace.keymap.set(Action::Deselect, Vec::new());
        frame(&mut app, Key::Escape);
        assert!(!app.selection_manager.has_selection());
    }

    fn press_with(map: &Keymap, key: Key) -> Option<Action> {
        map.action_for_press(key, Some(key), Modifiers::NONE)
    }

    #[test]
    fn no_two_default_actions_share_a_key_and_ids_are_unique() {
        let mut seen = std::collections::HashMap::new();
        for info in ACTIONS {
            for b in info.defaults {
                if let Some(other) = seen.insert(*b, info.id) {
                    panic!("{} is on {} and {}", b.to_text(), other, info.id);
                }
            }
        }
        let mut ids: Vec<&str> = ACTIONS.iter().map(|i| i.id).collect();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), ACTIONS.len());
    }
}
