//! The keyboard layout, so shortcuts land on the same keys whatever the
//! layout and the menus name the keys as printed on the user's keyboard.
//!
//! Letter shortcuts follow the letter (Ctrl+Z is the key marked Z). Symbol
//! and digit shortcuts (brush size, fit, zoom, lock transparency) follow the
//! key's position, so on AZERTY they need neither Shift nor AltGr; their
//! labels show that key's own character (`^ $` for brush size on AZERTY).
//!
//! The layout comes from the Settings choice, else from the key presses seen
//! (a Q key typing A is AZERTY), else from the system settings at start.

use eframe::egui::{self, Key, Modifiers};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub enum KeyboardLayout {
    #[default]
    Qwerty,
    Azerty,
    Qwertz,
}

impl KeyboardLayout {
    pub const ALL: [KeyboardLayout; 3] = [Self::Qwerty, Self::Azerty, Self::Qwertz];

    pub fn label(self) -> &'static str {
        match self {
            Self::Qwerty => "QWERTY",
            Self::Azerty => "AZERTY",
            Self::Qwertz => "QWERTZ",
        }
    }

    /// What the key at `position` (its QWERTY name) has printed on it.
    pub fn keycap(self, position: Key) -> &'static str {
        use Key::*;
        match (self, position) {
            (Self::Azerty, Num0) => "à",
            (Self::Azerty, Num1) => "&",
            (Self::Azerty, OpenBracket) => "^",
            (Self::Azerty, CloseBracket) => "$",
            (Self::Azerty, Slash) => "!",
            (Self::Azerty, Minus) => ")",
            (Self::Azerty, Equals) => "=",
            (Self::Qwertz, OpenBracket) => "Ü",
            (Self::Qwertz, CloseBracket) => "+",
            (Self::Qwertz, Slash) => "-",
            (Self::Qwertz, Minus) => "ß",
            (Self::Qwertz, Equals) => "´",
            (_, Num0) => "0",
            (_, Num1) => "1",
            (_, OpenBracket) => "[",
            (_, CloseBracket) => "]",
            (_, Slash) => "/",
            (_, Minus) => "-",
            (_, Equals) => "=",
            (_, key) => key.symbol_or_name(),
        }
    }
}

/// The layout setting and what has been detected.
#[derive(Default)]
pub struct KeyboardState {
    /// Chosen in Settings; `None` = automatic.
    pub choice: Option<KeyboardLayout>,
    /// Learnt from key presses.
    learnt: Option<KeyboardLayout>,
    /// Seen a Q key typing Q (so not AZERTY) / a Y key typing Y (not QWERTZ).
    q_is_q: bool,
    y_is_y: bool,
    /// From the system settings at start.
    pub system: Option<KeyboardLayout>,
}

impl KeyboardState {
    pub fn new() -> Self {
        Self {
            system: system_layout(),
            ..Default::default()
        }
    }

    /// The detected layout (ignoring the Settings choice).
    pub fn detected(&self) -> KeyboardLayout {
        self.learnt.or(self.system).unwrap_or_default()
    }

    pub fn layout(&self) -> KeyboardLayout {
        self.choice.unwrap_or_else(|| self.detected())
    }

    /// Learn from this frame's key presses, and publish the layout for the
    /// labels drawn this frame.
    pub fn observe(&mut self, ctx: &egui::Context) {
        ctx.input(|i| {
            for event in &i.events {
                if let egui::Event::Key {
                    key,
                    physical_key: Some(position),
                    pressed: true,
                    ..
                } = *event
                {
                    self.learn(position, key);
                }
            }
        });
        let layout = self.layout();
        ctx.data_mut(|d| d.insert_temp(layout_id(), layout));
    }

    /// A key at `position` (QWERTY name) typed `key`.
    fn learn(&mut self, position: Key, key: Key) {
        use Key::*;
        match (position, key) {
            (Q, A) | (A, Q) | (W, Z) | (Z, W) | (Semicolon, M) => {
                self.learnt = Some(KeyboardLayout::Azerty)
            }
            (Y, Z) | (Z, Y) => self.learnt = Some(KeyboardLayout::Qwertz),
            (Q, Q) | (A, A) => self.q_is_q = true,
            (Y, Y) | (Z, Z) => self.y_is_y = true,
            _ => {}
        }
        if self.q_is_q && self.y_is_y {
            self.learnt = Some(KeyboardLayout::Qwerty);
        }
    }
}

fn layout_id() -> egui::Id {
    egui::Id::new("rusty_painter_keyboard_layout")
}

/// The layout in use this frame (for labels).
pub fn current(ctx: &egui::Context) -> KeyboardLayout {
    ctx.data(|d| d.get_temp(layout_id())).unwrap_or_default()
}

/// A shortcut as printed on this keyboard: position keys (digits, symbols)
/// by their keycap, letters and named keys as egui names them.
pub fn shortcut_label(ctx: &egui::Context, modifiers: Modifiers, key: Key) -> String {
    if is_position_key(key) {
        let is_mac = matches!(
            ctx.os(),
            egui::os::OperatingSystem::Mac | egui::os::OperatingSystem::IOS
        );
        let names = egui::ModifierNames::NAMES;
        let mut label = names.format(&modifiers, is_mac);
        if !label.is_empty() {
            label += names.concat;
        }
        label + current(ctx).keycap(key)
    } else if let Some(symbol) = punctuation(key) {
        let label = ctx.format_shortcut(&egui::KeyboardShortcut::new(modifiers, Key::A));
        format!("{}{symbol}", label.strip_suffix('A').unwrap_or(&label))
    } else {
        ctx.format_shortcut(&egui::KeyboardShortcut::new(modifiers, key))
    }
}

/// `text` with `{key}` placeholders (`{[}`, `{]}`, `{/}`, `{0}`, `{1}`,
/// `{=}`, `{-}`) replaced by this keyboard's keycaps.
pub fn with_keycaps(ctx: &egui::Context, text: &str) -> String {
    let layout = current(ctx);
    let mut out = text.to_string();
    for (token, key) in [
        ("{[}", Key::OpenBracket),
        ("{]}", Key::CloseBracket),
        ("{/}", Key::Slash),
        ("{0}", Key::Num0),
        ("{1}", Key::Num1),
        ("{=}", Key::Equals),
        ("{-}", Key::Minus),
    ] {
        out = out.replace(token, layout.keycap(key));
    }
    out
}

/// Keys egui spells out ("Quote") that read better as what they type.
fn punctuation(key: Key) -> Option<&'static str> {
    use Key::*;
    Some(match key {
        Quote => "'",
        Semicolon => ";",
        Comma => ",",
        Period => ".",
        Backslash => "\\",
        Backtick => "`",
        Plus => "+",
        _ => return None,
    })
}

/// Shortcuts on these keys go by position.
pub(crate) fn is_position_key(key: Key) -> bool {
    use Key::*;
    matches!(
        key,
        Num0 | Num1 | OpenBracket | CloseBracket | Slash | Minus | Equals
    )
}

/// The layout the system is set to, where that can be read (Linux: the
/// XKB setting or the console keymap).
fn system_layout() -> Option<KeyboardLayout> {
    let from_name = |name: &str| {
        let name = name.trim().trim_matches('"').to_ascii_lowercase();
        let first = name.split([',', ' ', '-']).next().unwrap_or_default();
        match first {
            "fr" | "be" | "azerty" => Some(KeyboardLayout::Azerty),
            "de" | "at" | "ch" | "cz" | "hu" | "sk" | "si" | "hr" | "qwertz" => {
                Some(KeyboardLayout::Qwertz)
            }
            "" => None,
            _ => Some(KeyboardLayout::Qwerty),
        }
    };
    if let Ok(layout) = std::env::var("XKB_DEFAULT_LAYOUT")
        && let Some(l) = from_name(&layout)
    {
        return Some(l);
    }
    #[cfg(target_os = "linux")]
    {
        let read = |path: &str| std::fs::read_to_string(path).ok();
        if let Some(conf) = read("/etc/X11/xorg.conf.d/00-keyboard.conf") {
            for line in conf.lines() {
                let line = line.trim();
                if let Some(rest) = line.strip_prefix("Option \"XkbLayout\"") {
                    return from_name(rest);
                }
            }
        }
        if let Some(conf) = read("/etc/vconsole.conf") {
            for line in conf.lines() {
                if let Some(rest) = line.trim().strip_prefix("KEYMAP=") {
                    return from_name(rest);
                }
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_layout_is_learnt_from_what_keys_type() {
        let mut k = KeyboardState::default();
        assert_eq!(k.layout(), KeyboardLayout::Qwerty, "nothing known yet");
        k.learn(Key::Q, Key::A);
        assert_eq!(k.layout(), KeyboardLayout::Azerty);
        let mut k = KeyboardState::default();
        k.learn(Key::Y, Key::Z);
        assert_eq!(k.layout(), KeyboardLayout::Qwertz);
        let mut k = KeyboardState {
            system: Some(KeyboardLayout::Azerty),
            ..Default::default()
        };
        k.learn(Key::Q, Key::Q);
        k.learn(Key::Y, Key::Y);
        assert_eq!(k.layout(), KeyboardLayout::Qwerty, "keys beat the system");
        k.choice = Some(KeyboardLayout::Qwertz);
        assert_eq!(k.layout(), KeyboardLayout::Qwertz, "the setting beats both");
    }

    #[test]
    fn azerty_names_its_own_keycaps() {
        let a = KeyboardLayout::Azerty;
        assert_eq!(
            [Key::OpenBracket, Key::CloseBracket, Key::Slash, Key::Num0].map(|k| a.keycap(k)),
            ["^", "$", "!", "à"]
        );
        assert_eq!(KeyboardLayout::Qwerty.keycap(Key::OpenBracket), "[");
    }
}
