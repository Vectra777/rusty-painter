//! The New Canvas dialog.

use crate::app::document::{MAX_CANVAS_DIMENSION, MAX_CANVAS_DPI};
use crate::canvas::blend_modes::BlendSpace;
use crate::ui::style::TEXT_DIM;
use crate::ui::widgets::FitScreen;
use crate::{BackgroundChoice, CanvasUnit, ColorModel, NewCanvasSettings, Orientation, PainterApp};
use eframe::egui::{self, RichText};

/// Modal dialog to configure and create a new canvas.
pub fn canvas_creation_modal(app: &mut PainterApp, ctx: &egui::Context) {
    if !app.modal_state.show_new_canvas_modal {
        return;
    }

    let mut open = app.modal_state.show_new_canvas_modal;
    egui::Window::new("New Canvas")
        .fit_screen(ctx)
        .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
        .open(&mut open)
        .collapsible(false)
        .resizable(false)
        // Rows wrap on a narrow screen instead of running off it.
        .max_width(540.0_f32.min(ctx.screen_rect().width() - 16.0))
        .show(ctx, |ui| {
            let clip = (app.workspace.clipboard.clip.as_ref()).map(|c| (c.w, c.h));
            presets_panel(
                ui,
                &mut app.modal_state.canvas_presets,
                &mut app.modal_state.new_canvas,
                clip,
            );
            ui.separator();
            let settings: &mut NewCanvasSettings = &mut app.modal_state.new_canvas;

            ui.horizontal_wrapped(|ui| {
                ui.label("Name");
                ui.text_edit_singleline(&mut settings.name);
            });

            ui.separator();
            ui.heading("Dimensions");
            ui.horizontal_wrapped(|ui| {
                ui.label("Width");
                ui.add(
                    egui::DragValue::new(&mut settings.width)
                        .speed(1.0)
                        .range(1.0..=MAX_CANVAS_DIMENSION as f32)
                        .suffix(settings.unit.label()),
                );
                ui.label("Height");
                ui.add(
                    egui::DragValue::new(&mut settings.height)
                        .speed(1.0)
                        .range(1.0..=MAX_CANVAS_DIMENSION as f32)
                        .suffix(settings.unit.label()),
                );
                egui::ComboBox::from_label("Units")
                    .selected_text(settings.unit.label())
                    .show_ui(ui, |ui| {
                        ui.selectable_value(&mut settings.unit, CanvasUnit::Pixels, "Pixels");
                        ui.selectable_value(&mut settings.unit, CanvasUnit::Inches, "Inches");
                        ui.selectable_value(
                            &mut settings.unit,
                            CanvasUnit::Millimeters,
                            "Millimeters",
                        );
                        ui.selectable_value(
                            &mut settings.unit,
                            CanvasUnit::Centimeters,
                            "Centimeters",
                        );
                    });
            });

            ui.horizontal_wrapped(|ui| {
                ui.label("Resolution (DPI)");
                ui.add(
                    egui::DragValue::new(&mut settings.resolution)
                        .speed(1.0)
                        .range(1.0..=MAX_CANVAS_DPI),
                );
                let mut orientation_changed = false;
                orientation_changed |= ui
                    .selectable_value(&mut settings.orientation, Orientation::Portrait, "Portrait")
                    .changed();
                orientation_changed |= ui
                    .selectable_value(
                        &mut settings.orientation,
                        Orientation::Landscape,
                        "Landscape",
                    )
                    .changed();
                if orientation_changed {
                    std::mem::swap(&mut settings.width, &mut settings.height);
                }
            });

            ui.separator();
            ui.heading("Color");
            ui.horizontal_wrapped(|ui| {
                ui.label("Background");
                ui.radio_value(&mut settings.background, BackgroundChoice::White, "White");
                ui.radio_value(&mut settings.background, BackgroundChoice::Black, "Black");
                ui.radio_value(
                    &mut settings.background,
                    BackgroundChoice::Transparent,
                    "Transparent",
                );
                ui.radio_value(&mut settings.background, BackgroundChoice::Custom, "Custom");
                if settings.background == BackgroundChoice::Custom {
                    ui.color_edit_button_srgba(&mut settings.custom_bg);
                }
            });

            ui.horizontal_wrapped(|ui| {
                ui.label("Color Model");
                egui::ComboBox::from_id_salt("color_model")
                    .selected_text(match settings.color_model {
                        ColorModel::Rgba => "RGBA",
                        ColorModel::Grayscale => "Grayscale",
                    })
                    .show_ui(ui, |ui| {
                        ui.selectable_value(&mut settings.color_model, ColorModel::Rgba, "RGBA");
                        ui.selectable_value(
                            &mut settings.color_model,
                            ColorModel::Grayscale,
                            "Grayscale",
                        );
                    });
            });
            ui.weak("Grayscale paints in a single channel.");

            ui.horizontal_wrapped(|ui| {
                ui.label("Color blending");
                blend_space_picker(ui, &mut settings.blend_space);
            });
            ui.weak(blend_space_hint(settings.blend_space));

            ui.horizontal_wrapped(|ui| {
                ui.label("Colour depth");
                depth_picker(ui, &mut settings.depth);
            });
            ui.weak(depth_hint(settings.depth));

            let validation = settings.validated_dimensions();
            match validation {
                Ok((px_w, px_h)) => {
                    ui.label(format!(
                        "Result: {} × {} px @ {:.0} dpi",
                        px_w, px_h, settings.resolution
                    ));
                }
                Err(ref msg) => {
                    ui.colored_label(egui::Color32::LIGHT_RED, msg);
                }
            }

            ui.separator();
            ui.horizontal_wrapped(|ui| {
                if ui
                    .add_enabled(validation.is_ok(), egui::Button::new("Create"))
                    .clicked()
                {
                    app.modal_state.show_new_canvas_modal = false;
                    app.create_new_canvas();
                    // Create from Clipboard: the copied pixels as the first layer.
                    if std::mem::take(&mut app.modal_state.canvas_presets.paste) {
                        app.paste();
                    }
                }
                if ui.button("Cancel").clicked() {
                    app.modal_state.show_new_canvas_modal = false;
                    app.modal_state.canvas_presets.paste = false;
                }
            });
        });

    // Create and Cancel close it too.
    app.modal_state.show_new_canvas_modal &= open;
}

/// 8-bit / 16-bit / 32-bit float choice for a document.
pub(crate) fn depth_picker(ui: &mut egui::Ui, depth: &mut crate::canvas::storage::Depth) -> bool {
    let mut changed = false;
    for d in crate::canvas::storage::Depth::ALL {
        changed |= ui.selectable_value(depth, d, d.label()).changed();
    }
    changed
}

pub(crate) fn depth_hint(depth: crate::canvas::storage::Depth) -> &'static str {
    use crate::canvas::storage::Depth;
    match depth {
        Depth::U8 => "The usual: fast and small.",
        Depth::U16 => {
            "Smooth gradients and soft airbrushing without banding; faint glazes build up. Twice the memory."
        }
        Depth::F32 => {
            "Linear light with values past white, for compositing and HDR. Four times the memory."
        }
    }
}

/// Linear / Gamma choice for how a document blends colours.
pub(crate) fn blend_space_picker(ui: &mut egui::Ui, space: &mut BlendSpace) -> bool {
    let mut changed = false;
    changed |= ui
        .selectable_value(space, BlendSpace::Linear, "Linear light")
        .changed();
    changed |= ui
        .selectable_value(space, BlendSpace::Gamma, "Gamma (Krita / Photoshop)")
        .changed();
    changed
}

pub(crate) fn blend_space_hint(space: BlendSpace) -> &'static str {
    match space {
        BlendSpace::Linear => {
            "Physically correct mixing: bright, clean colour blends and lighter soft edges."
        }
        BlendSpace::Gamma => {
            "Mixes stored sRGB values like Krita and Photoshop: familiar soft-brush falloff and blend modes."
        }
    }
}

/// A canvas size to start from.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct CanvasPreset {
    pub name: String,
    pub width: f32,
    pub height: f32,
    pub unit: CanvasUnit,
    pub resolution: f32,
}

#[derive(Clone, Copy, PartialEq, Eq, Default)]
enum Shelf {
    #[default]
    Screen,
    Print,
    Comic,
    Photo,
    Texture,
    Mine,
}

const SHELVES: [(Shelf, &str); 6] = [
    (Shelf::Screen, "Screen"),
    (Shelf::Print, "Print"),
    (Shelf::Comic, "Comic"),
    (Shelf::Photo, "Photo"),
    (Shelf::Texture, "Texture"),
    (Shelf::Mine, "My presets"),
];

use CanvasUnit::{Inches as In, Millimeters as Mm, Pixels as Px};

/// The sizes that come with the app, like Krita's templates:
/// (shelf, name, width, height, unit, dpi).
const BUILT_IN: &[(Shelf, &str, f32, f32, CanvasUnit, f32)] = &[
    (Shelf::Screen, "HD 720p", 1280.0, 720.0, Px, 72.0),
    (Shelf::Screen, "Full HD 1080p", 1920.0, 1080.0, Px, 72.0),
    (Shelf::Screen, "QHD 1440p", 2560.0, 1440.0, Px, 72.0),
    (Shelf::Screen, "4K UHD", 3840.0, 2160.0, Px, 72.0),
    (Shelf::Screen, "Square post", 1080.0, 1080.0, Px, 72.0),
    (Shelf::Screen, "Portrait post 4:5", 1080.0, 1350.0, Px, 72.0),
    (
        Shelf::Screen,
        "Story / phone 9:16",
        1080.0,
        1920.0,
        Px,
        72.0,
    ),
    (Shelf::Screen, "Desktop wallpaper", 2560.0, 1600.0, Px, 72.0),
    (Shelf::Print, "A3", 297.0, 420.0, Mm, 300.0),
    (Shelf::Print, "A4", 210.0, 297.0, Mm, 300.0),
    (Shelf::Print, "A5", 148.0, 210.0, Mm, 300.0),
    (Shelf::Print, "A6", 105.0, 148.0, Mm, 300.0),
    (Shelf::Print, "US Letter", 8.5, 11.0, In, 300.0),
    (Shelf::Print, "US Legal", 8.5, 14.0, In, 300.0),
    (Shelf::Print, "Tabloid", 11.0, 17.0, In, 300.0),
    (Shelf::Print, "Postcard 4×6", 4.0, 6.0, In, 300.0),
    (Shelf::Comic, "US comic page", 6.625, 10.25, In, 600.0),
    (Shelf::Comic, "Manga B5", 182.0, 257.0, Mm, 600.0),
    (Shelf::Comic, "Manga B4 (draft)", 257.0, 364.0, Mm, 600.0),
    (Shelf::Comic, "A4 comic page", 210.0, 297.0, Mm, 600.0),
    (Shelf::Comic, "Webtoon panel", 800.0, 1280.0, Px, 72.0),
    (Shelf::Comic, "Webtoon strip", 800.0, 12800.0, Px, 72.0),
    (Shelf::Comic, "Comic strip", 11.0, 3.5, In, 600.0),
    (Shelf::Photo, "3:2 (24 MP)", 6000.0, 4000.0, Px, 300.0),
    (Shelf::Photo, "4:3 (12 MP)", 4000.0, 3000.0, Px, 300.0),
    (Shelf::Photo, "16:9 (8 MP)", 3840.0, 2160.0, Px, 300.0),
    (Shelf::Photo, "Print 5×7", 5.0, 7.0, In, 300.0),
    (Shelf::Photo, "Print 8×10", 8.0, 10.0, In, 300.0),
    (Shelf::Texture, "256 × 256", 256.0, 256.0, Px, 72.0),
    (Shelf::Texture, "512 × 512", 512.0, 512.0, Px, 72.0),
    (Shelf::Texture, "1024 × 1024", 1024.0, 1024.0, Px, 72.0),
    (Shelf::Texture, "2048 × 2048", 2048.0, 2048.0, Px, 72.0),
    (Shelf::Texture, "4096 × 4096", 4096.0, 4096.0, Px, 72.0),
];

#[derive(Default)]
pub struct PresetsState {
    shelf: Shelf,
    /// The user's presets; `None` until read from disk.
    mine: Option<Vec<CanvasPreset>>,
    new_name: String,
    /// Paste the clipboard into the canvas once created.
    pub paste: bool,
}

fn presets_path() -> std::path::PathBuf {
    crate::app::init::data_dir().join("canvas_presets.json")
}

fn load_presets() -> Vec<CanvasPreset> {
    std::fs::read(presets_path())
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default()
}

fn save_presets(presets: &[CanvasPreset]) {
    let result = serde_json::to_vec_pretty(presets)
        .map_err(|e| e.to_string())
        .and_then(|bytes| crate::project::write_atomically(&presets_path(), &bytes));
    if let Err(err) = result {
        log::warn!("Couldn't save canvas presets: {err}");
    }
}

impl CanvasPreset {
    fn apply(&self, settings: &mut NewCanvasSettings) {
        settings.width = self.width;
        settings.height = self.height;
        settings.unit = self.unit;
        settings.resolution = self.resolution;
        settings.orientation = if self.width > self.height {
            Orientation::Landscape
        } else {
            Orientation::Portrait
        };
    }

    fn matches(&self, s: &NewCanvasSettings) -> bool {
        (self.width, self.height, self.unit, self.resolution)
            == (s.width, s.height, s.unit, s.resolution)
    }

    /// "210 × 297 mm, 300 dpi".
    fn describe(&self) -> String {
        let unit = self.unit.label();
        if self.unit == CanvasUnit::Pixels {
            format!("{} × {} {unit}", self.width, self.height)
        } else {
            format!(
                "{} × {} {unit}, {} dpi",
                self.width, self.height, self.resolution
            )
        }
    }
}

/// Shelves of sizes to start from; picking one fills in the form below.
fn presets_panel(
    ui: &mut egui::Ui,
    state: &mut PresetsState,
    settings: &mut NewCanvasSettings,
    clip: Option<(usize, usize)>,
) {
    let mine = state.mine.get_or_insert_with(load_presets);
    ui.horizontal_wrapped(|ui| {
        for (shelf, label) in SHELVES {
            ui.selectable_value(&mut state.shelf, shelf, label);
        }
        if let Some((w, h)) = clip
            && ui
                .button("From clipboard")
                .on_hover_text("The size of the copied pixels, pasted into the new canvas")
                .clicked()
        {
            CanvasPreset {
                name: String::new(),
                width: w as f32,
                height: h as f32,
                unit: CanvasUnit::Pixels,
                resolution: settings.resolution,
            }
            .apply(settings);
            state.paste = true;
        }
    });
    let shown: Vec<CanvasPreset> = if state.shelf == Shelf::Mine {
        mine.clone()
    } else {
        (BUILT_IN.iter())
            .filter(|p| p.0 == state.shelf)
            .map(|&(_, name, width, height, unit, resolution)| CanvasPreset {
                name: name.to_string(),
                width,
                height,
                unit,
                resolution,
            })
            .collect()
    };
    let mut delete = None;
    ui.horizontal_wrapped(|ui| {
        for (i, preset) in shown.iter().enumerate() {
            let on = preset.matches(settings);
            let response = ui
                .add(egui::Button::new(&preset.name).selected(on))
                .on_hover_text(preset.describe());
            if response.clicked() {
                preset.apply(settings);
                state.paste = false;
            }
            if state.shelf == Shelf::Mine {
                response.context_menu(|ui| {
                    if ui.button("Delete preset").clicked() {
                        delete = Some(i);
                        ui.close_menu();
                    }
                });
            }
        }
    });
    if state.shelf == Shelf::Mine {
        if mine.is_empty() {
            ui.label(RichText::new("Save the size below to find it here.").color(TEXT_DIM));
        } else {
            ui.weak("Right-click or long-press a preset to delete it.");
        }
        ui.horizontal_wrapped(|ui| {
            ui.add(
                egui::TextEdit::singleline(&mut state.new_name)
                    .hint_text("Preset name")
                    .desired_width(160.0),
            );
            let name = state.new_name.trim().to_string();
            if ui
                .add_enabled(!name.is_empty(), egui::Button::new("Save current size"))
                .clicked()
            {
                mine.retain(|p| p.name != name);
                mine.push(CanvasPreset {
                    name,
                    width: settings.width,
                    height: settings.height,
                    unit: settings.unit,
                    resolution: settings.resolution,
                });
                save_presets(mine);
                state.new_name.clear();
            }
        });
    }
    if let Some(i) = delete {
        mine.remove(i);
        save_presets(mine);
    }
}
