//! The colours a gradient runs through: the brush's two colours, the
//! built-in presets, and the user's own gradients made in the Gradient
//! Editor, which are kept in `gradients.json` next to the brushes folder.

use crate::canvas::gradient::Stop;
use eframe::egui::Color32;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Which colours a gradient runs through.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GradientColors {
    /// Brush colour to secondary colour.
    ForegroundToBackground,
    /// Brush colour fading out.
    ForegroundToTransparent,
    /// One of [`PRESETS`], by index.
    Preset(usize),
    /// One of the user's own gradients ([`GradientLibrary::custom`]), by index.
    Custom(usize),
}

/// A built-in gradient: unmultiplied sRGBA colours at positions 0..=1.
pub struct GradientPreset {
    pub name: &'static str,
    pub stops: &'static [(f32, [u8; 4])],
}

pub const PRESETS: &[GradientPreset] = &[
    GradientPreset {
        name: "Black to white",
        stops: &[(0.0, [0, 0, 0, 255]), (1.0, [255, 255, 255, 255])],
    },
    GradientPreset {
        name: "Spectrum",
        stops: &[
            (0.0, [255, 0, 0, 255]),
            (1.0 / 6.0, [255, 255, 0, 255]),
            (2.0 / 6.0, [0, 255, 0, 255]),
            (3.0 / 6.0, [0, 255, 255, 255]),
            (4.0 / 6.0, [0, 0, 255, 255]),
            (5.0 / 6.0, [255, 0, 255, 255]),
            (1.0, [255, 0, 0, 255]),
        ],
    },
    GradientPreset {
        name: "Sunset",
        stops: &[
            (0.0, [44, 20, 84, 255]),
            (0.45, [214, 69, 99, 255]),
            (0.75, [247, 146, 76, 255]),
            (1.0, [253, 222, 121, 255]),
        ],
    },
    GradientPreset {
        name: "Sky",
        stops: &[(0.0, [34, 82, 160, 255]), (1.0, [200, 226, 250, 255])],
    },
    GradientPreset {
        name: "Ocean",
        stops: &[
            (0.0, [8, 24, 58, 255]),
            (0.5, [18, 110, 150, 255]),
            (1.0, [140, 220, 220, 255]),
        ],
    },
    GradientPreset {
        name: "Forest",
        stops: &[
            (0.0, [16, 40, 24, 255]),
            (0.5, [58, 110, 52, 255]),
            (1.0, [196, 220, 120, 255]),
        ],
    },
    GradientPreset {
        name: "Fire",
        stops: &[
            (0.0, [0, 0, 0, 255]),
            (0.35, [170, 20, 10, 255]),
            (0.65, [240, 120, 20, 255]),
            (0.85, [255, 210, 60, 255]),
            (1.0, [255, 255, 220, 255]),
        ],
    },
    GradientPreset {
        name: "Copper",
        stops: &[
            (0.0, [60, 24, 10, 255]),
            (0.5, [200, 110, 60, 255]),
            (0.7, [240, 180, 130, 255]),
            (1.0, [120, 50, 20, 255]),
        ],
    },
    GradientPreset {
        name: "Chrome",
        stops: &[
            (0.0, [40, 40, 48, 255]),
            (0.45, [220, 224, 232, 255]),
            (0.5, [90, 92, 100, 255]),
            (0.55, [160, 164, 172, 255]),
            (1.0, [245, 245, 250, 255]),
        ],
    },
    GradientPreset {
        name: "Neon",
        stops: &[
            (0.0, [255, 0, 160, 255]),
            (0.5, [120, 40, 255, 255]),
            (1.0, [0, 230, 255, 255]),
        ],
    },
    GradientPreset {
        name: "Pastel",
        stops: &[
            (0.0, [255, 190, 210, 255]),
            (0.5, [200, 190, 255, 255]),
            (1.0, [180, 235, 220, 255]),
        ],
    },
    GradientPreset {
        name: "White fade",
        stops: &[(0.0, [255, 255, 255, 255]), (1.0, [255, 255, 255, 0])],
    },
    GradientPreset {
        name: "Black fade",
        stops: &[(0.0, [0, 0, 0, 255]), (1.0, [0, 0, 0, 0])],
    },
];

/// Where a stop's colour comes from.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum StopColor {
    /// The brush colour at the time the gradient is drawn.
    Primary,
    /// The secondary colour.
    Secondary,
    /// A fixed sRGB colour.
    Fixed([u8; 3]),
}

/// A stop of a user gradient.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct EditStop {
    /// 0 (start) to 1 (end).
    pub pos: f32,
    pub color: StopColor,
    /// 0 (clear) to 1.
    pub opacity: f32,
}

/// A gradient made in the Gradient Editor.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CustomGradient {
    pub name: String,
    /// In any order while being edited; sorted when used.
    pub stops: Vec<EditStop>,
}

impl CustomGradient {
    /// The stops with real colours, sorted by position.
    pub fn resolve(&self, primary: Color32, secondary: Color32) -> Vec<Stop> {
        let mut stops: Vec<Stop> = self
            .stops
            .iter()
            .map(|s| {
                let [r, g, b] = match s.color {
                    StopColor::Primary => rgb(primary),
                    StopColor::Secondary => rgb(secondary),
                    StopColor::Fixed(c) => c,
                };
                let a = (s.opacity.clamp(0.0, 1.0) * 255.0).round() as u8;
                Stop {
                    pos: s.pos.clamp(0.0, 1.0),
                    color: Color32::from_rgba_unmultiplied(r, g, b, a),
                }
            })
            .collect();
        stops.sort_by(|a, b| a.pos.total_cmp(&b.pos));
        if stops.is_empty() {
            stops.push(Stop {
                pos: 0.0,
                color: primary,
            });
        }
        stops
    }
}

impl CustomGradient {
    /// Put the stops in order; returns where stop `selected` went.
    pub fn sort_stops(&mut self, selected: usize) -> usize {
        let mut order: Vec<usize> = (0..self.stops.len()).collect();
        order.sort_by(|&a, &b| self.stops[a].pos.total_cmp(&self.stops[b].pos));
        self.stops = order.iter().map(|&i| self.stops[i]).collect();
        order.iter().position(|&i| i == selected).unwrap_or(0)
    }

    /// A new fixed stop at `pos` with the colour the gradient has there;
    /// returns its index.
    pub fn add_stop(&mut self, pos: f32, primary: Color32, secondary: Color32) -> usize {
        let pos = pos.clamp(0.0, 1.0);
        let stops = self.resolve(primary, secondary);
        let after = stops.iter().position(|s| s.pos > pos);
        let color = match after {
            None => stops[stops.len() - 1].color,
            Some(0) => stops[0].color,
            Some(i) => {
                let (a, b) = (stops[i - 1], stops[i]);
                let t = if b.pos > a.pos {
                    (pos - a.pos) / (b.pos - a.pos)
                } else {
                    0.0
                };
                let (ca, cb) = (
                    a.color.to_srgba_unmultiplied(),
                    b.color.to_srgba_unmultiplied(),
                );
                let mix: [u8; 4] = std::array::from_fn(|k| {
                    (ca[k] as f32 + (cb[k] as f32 - ca[k] as f32) * t).round() as u8
                });
                Color32::from_rgba_unmultiplied(mix[0], mix[1], mix[2], mix[3])
            }
        };
        let [r, g, b, a] = color.to_srgba_unmultiplied();
        self.stops.push(EditStop {
            pos,
            color: StopColor::Fixed([r, g, b]),
            opacity: a as f32 / 255.0,
        });
        self.stops.len() - 1
    }

    /// Mirror the stops end to end.
    pub fn reverse(&mut self) {
        for s in &mut self.stops {
            s.pos = 1.0 - s.pos;
        }
        self.sort_stops(0);
    }

    /// Space the stops evenly from 0 to 1, keeping their order.
    pub fn spread(&mut self) {
        self.sort_stops(0);
        let last = self.stops.len().saturating_sub(1).max(1) as f32;
        for (i, s) in self.stops.iter_mut().enumerate() {
            s.pos = i as f32 / last;
        }
    }
}

fn rgb(c: Color32) -> [u8; 3] {
    let [r, g, b, _] = c.to_srgba_unmultiplied();
    [r, g, b]
}

/// The gradients to choose from: presets and the user's own.
#[derive(Default)]
pub struct GradientLibrary {
    pub custom: Vec<CustomGradient>,
    /// Where the user's gradients are saved; `None` keeps them in memory.
    path: Option<PathBuf>,
    /// Changed since last saved.
    unsaved: bool,
}

impl GradientLibrary {
    /// The user's gradients from `path` (none if it doesn't exist or can't
    /// be read), saved back there when they change.
    pub fn load(path: PathBuf) -> Self {
        let custom = std::fs::read(&path)
            .ok()
            .and_then(|bytes| match serde_json::from_slice(&bytes) {
                Ok(list) => Some(list),
                Err(err) => {
                    log::warn!("Ignoring {}: {err}", path.display());
                    None
                }
            })
            .unwrap_or_default();
        Self {
            custom,
            path: Some(path),
            unsaved: false,
        }
    }

    /// Every choice, in the order the picker lists them.
    pub fn choices(&self) -> impl Iterator<Item = GradientColors> {
        [
            GradientColors::ForegroundToBackground,
            GradientColors::ForegroundToTransparent,
        ]
        .into_iter()
        .chain((0..PRESETS.len()).map(GradientColors::Preset))
        .chain((0..self.custom.len()).map(GradientColors::Custom))
    }

    pub fn name(&self, colors: GradientColors) -> &str {
        match colors {
            GradientColors::ForegroundToBackground => "Primary to secondary",
            GradientColors::ForegroundToTransparent => "Primary to clear",
            GradientColors::Preset(i) => PRESETS.get(i).map_or("?", |p| p.name),
            GradientColors::Custom(i) => self.custom.get(i).map_or("?", |g| g.name.as_str()),
        }
    }

    /// The colours along the gradient, using the brush's `primary` and
    /// `secondary` colours where it does.
    pub fn stops(&self, colors: GradientColors, primary: Color32, secondary: Color32) -> Vec<Stop> {
        self.editable(colors).resolve(primary, secondary)
    }

    /// Any choice as a gradient the editor can change (a copy for the
    /// brush colours and presets).
    pub fn editable(&self, colors: GradientColors) -> CustomGradient {
        let stop = |pos, color, opacity| EditStop {
            pos,
            color,
            opacity,
        };
        let name = self.name(colors).to_string();
        let stops = match colors {
            GradientColors::ForegroundToBackground => vec![
                stop(0.0, StopColor::Primary, 1.0),
                stop(1.0, StopColor::Secondary, 1.0),
            ],
            GradientColors::ForegroundToTransparent => vec![
                stop(0.0, StopColor::Primary, 1.0),
                stop(1.0, StopColor::Primary, 0.0),
            ],
            GradientColors::Preset(i) => PRESETS.get(i).map_or_else(Vec::new, |p| {
                p.stops
                    .iter()
                    .map(|&(pos, [r, g, b, a])| {
                        stop(pos, StopColor::Fixed([r, g, b]), a as f32 / 255.0)
                    })
                    .collect()
            }),
            GradientColors::Custom(i) => {
                return self.custom.get(i).cloned().unwrap_or(CustomGradient {
                    name,
                    stops: Vec::new(),
                });
            }
        };
        CustomGradient { name, stops }
    }

    /// Add `gradient` to the user's; returns how to choose it.
    pub fn add(&mut self, gradient: CustomGradient) -> GradientColors {
        self.custom.push(gradient);
        self.unsaved = true;
        GradientColors::Custom(self.custom.len() - 1)
    }

    /// Remove the user's gradient `index`, and return what `selected`
    /// becomes (the brush colours if it was the one removed).
    pub fn remove(&mut self, index: usize, selected: GradientColors) -> GradientColors {
        if index >= self.custom.len() {
            return selected;
        }
        self.custom.remove(index);
        self.unsaved = true;
        match selected {
            GradientColors::Custom(i) if i == index => GradientColors::ForegroundToBackground,
            GradientColors::Custom(i) if i > index => GradientColors::Custom(i - 1),
            other => other,
        }
    }

    /// A user gradient was edited.
    pub fn mark_changed(&mut self) {
        self.unsaved = true;
    }

    /// Write the user's gradients if they changed. Errors are logged: a
    /// read-only folder shouldn't get in the way of painting.
    pub fn save_if_changed(&mut self) {
        if !self.unsaved {
            return;
        }
        self.unsaved = false;
        let Some(path) = &self.path else {
            return;
        };
        let result = serde_json::to_vec_pretty(&self.custom)
            .map_err(|e| e.to_string())
            .and_then(|bytes| std::fs::write(path, bytes).map_err(|e| e.to_string()));
        if let Err(err) = result {
            log::warn!("Couldn't save gradients to {}: {err}", path.display());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_custom_gradient_resolves_brush_colours_and_opacity() {
        let g = CustomGradient {
            name: "Test".into(),
            stops: vec![
                EditStop {
                    pos: 1.0,
                    color: StopColor::Secondary,
                    opacity: 0.0,
                },
                EditStop {
                    pos: 0.0,
                    color: StopColor::Primary,
                    opacity: 1.0,
                },
            ],
        };
        let stops = g.resolve(Color32::RED, Color32::BLUE);
        assert_eq!(stops[0].color, Color32::RED, "sorted by position");
        assert_eq!(stops[1].color.a(), 0);
        assert_eq!(stops[1].color.to_srgba_unmultiplied()[3], 0);
    }

    #[test]
    fn every_choice_can_be_edited_and_looks_the_same() {
        let lib = GradientLibrary::default();
        let (p, s) = (Color32::RED, Color32::BLUE);
        for choice in lib.choices() {
            let copy = lib.editable(choice).resolve(p, s);
            assert_eq!(copy, lib.stops(choice, p, s), "{}", lib.name(choice));
        }
    }

    #[test]
    fn removing_a_gradient_keeps_the_selection_pointing_right() {
        let mut lib = GradientLibrary::default();
        let a = lib.add(lib.editable(GradientColors::Preset(0)));
        let b = lib.add(lib.editable(GradientColors::Preset(1)));
        assert_eq!(lib.remove(0, b), GradientColors::Custom(0));
        assert_eq!(
            lib.remove(0, GradientColors::Custom(0)),
            GradientColors::ForegroundToBackground
        );
        assert_ne!(a, b);
    }

    #[test]
    fn stops_are_added_sorted_reversed_and_spread() {
        let lib = GradientLibrary::default();
        let mut g = lib.editable(GradientColors::Preset(0)); // black to white
        let added = g.add_stop(0.5, Color32::RED, Color32::BLUE);
        let StopColor::Fixed(c) = g.stops[added].color else {
            panic!("a new stop has a fixed colour");
        };
        assert!(c[0].abs_diff(128) <= 1, "the colour already there: {c:?}");
        assert_eq!(g.sort_stops(added), 1, "sorted into the middle");
        g.stops[0].pos = 0.2;
        g.reverse();
        let pos: Vec<f32> = g.stops.iter().map(|s| s.pos).collect();
        assert_eq!(pos, vec![0.0, 0.5, 0.8]);
        g.spread();
        let pos: Vec<f32> = g.stops.iter().map(|s| s.pos).collect();
        assert_eq!(pos, vec![0.0, 0.5, 1.0]);
    }

    #[test]
    fn user_gradients_are_saved_and_loaded() {
        let path = std::env::temp_dir().join(format!("rp-gradients-{}.json", std::process::id()));
        let _ = std::fs::remove_file(&path);
        let mut lib = GradientLibrary::load(path.clone());
        assert!(lib.custom.is_empty());
        let mut g = lib.editable(GradientColors::Preset(2));
        g.name = "Mine".into();
        lib.add(g.clone());
        lib.save_if_changed();
        let loaded = GradientLibrary::load(path.clone());
        let _ = std::fs::remove_file(&path);
        assert_eq!(loaded.custom, vec![g]);
    }
}
