//! Interface preferences kept in `settings.json` as one entry (`ui`): the
//! interface size, touch helpers, tools taken off the strip and the
//! QuickMenu's slots.

use serde::{Deserialize, Serialize};

/// Smallest and largest interface size offered (egui's zoom factor).
pub(crate) const INTERFACE_SIZES: std::ops::RangeInclusive<f32> = 0.9..=1.5;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct UiPrefs {
    /// Touch mode: a finger held still on the canvas picks a colour.
    pub hold_eyedropper: bool,
    /// The whole interface's scale, within [`INTERFACE_SIZES`].
    pub interface_size: f32,
    /// Touch mode: the first-run gesture hints were dismissed.
    pub hints_seen: bool,
    /// Tools taken off the tool strip (their names, as the strip's
    /// `HIDEABLE` list has them).
    pub hidden_tools: Vec<String>,
    /// Columns of the tool strip: 1, or 2 when pulled wider by its grip.
    pub toolbar_columns: u8,
    /// The QuickMenu's six slots, clockwise from the top: keymap action
    /// ids (`Action::id`), `None` for an empty slot.
    pub quick_menu: [Option<String>; 6],
}

impl Default for UiPrefs {
    fn default() -> Self {
        let slot = |s: &str| Some(s.to_string());
        Self {
            hold_eyedropper: true,
            interface_size: 1.0,
            hints_seen: false,
            hidden_tools: Vec::new(),
            toolbar_columns: 1,
            quick_menu: [
                slot("undo"),
                slot("redo"),
                slot("eyedropper"),
                slot("brush"),
                slot("eraser"),
                slot("fit"),
            ],
        }
    }
}

impl UiPrefs {
    /// The interface size to apply: within range whatever the file said.
    pub(crate) fn zoom(&self) -> f32 {
        if self.interface_size.is_finite() {
            self.interface_size
                .clamp(*INTERFACE_SIZES.start(), *INTERFACE_SIZES.end())
        } else {
            1.0
        }
    }
}
