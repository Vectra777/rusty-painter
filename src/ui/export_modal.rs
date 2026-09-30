//! The Export dialog: format, file name and progress.

use crate::{
    PainterApp,
    app::document::validate_canvas_size,
    project::export::{ExportFormat, save_color_image},
};
use eframe::egui;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::thread;

/// Modal dialog to export the current canvas to disk with a native file picker.
pub fn export_modal(app: &mut PainterApp, ctx: &egui::Context) {
    if !app.export_state.show_modal {
        return;
    }
    // The quick mask layer isn't part of the picture.
    app.quick_mask_leave();

    let mut open = app.export_state.show_modal;
    egui::Window::new("Export Canvas")
        .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
        .open(&mut open)
        .collapsible(false)
        .resizable(false)
        .show(ctx, |ui| {
            let settings = &mut app.export_state.settings;

            ui.horizontal(|ui| {
                ui.label("Format");
                egui::ComboBox::from_label("Format")
                    .selected_text(settings.format.label())
                    .show_ui(ui, |ui| {
                        for format in ExportFormat::ALL {
                            // Android saves to the photo library, which
                            // takes pictures only.
                            if cfg!(target_os = "android") && format.is_layered() {
                                continue;
                            }
                            ui.selectable_value(&mut settings.format, format, format.label());
                        }
                    });
            });

            ui.separator();
            ui.heading("Destination");
            ui.horizontal(|ui| {
                ui.label("File");
                let display = settings
                    .chosen_path
                    .as_ref()
                    .map(|p| p.display().to_string())
                    .unwrap_or_else(|| settings.default_file_name());
                ui.monospace(display);
                if ui.button("Choose...").clicked()
                    && let Some(path) = pick_file(&settings.default_file_name())
                {
                    settings.chosen_path = Some(path);
                }
            });

            if let Some(msg) = &app.export_state.message {
                ui.label(msg);
            }

            if app.export_state.in_progress {
                ui.add(
                    egui::ProgressBar::new(app.export_state.progress)
                        .desired_width(200.0)
                        .text("Exporting..."),
                );
            }

            ui.separator();
            ui.horizontal(|ui| {
                let disabled = app.export_state.in_progress;
                if ui
                    .add_enabled(!disabled, egui::Button::new("Export"))
                    .clicked()
                {
                    let target = app.export_state.settings.output_path();
                    let format = app.export_state.settings.format;

                    let (w, h) = (app.canvas.width(), app.canvas.height());
                    if let Err(msg) = validate_canvas_size(w, h) {
                        app.export_state.message = Some(format!("Export blocked: {msg}"));
                        return;
                    }

                    // Flatten on the UI thread, then save on a worker thread.
                    // The shared canvas is not cloned across threads; size limits keep this bounded.
                    app.stroke_worker.wait_idle();
                    // Shader layers export as their current frame.
                    app.bake_shader_layers();
                    enum Data {
                        Image(egui::ColorImage),
                        Layers(Box<crate::project::psd::PsdDocument>),
                        Svg(Result<String, String>),
                    }
                    let data = match format {
                        ExportFormat::Psd => Data::Layers(Box::new(
                            crate::project::psd::PsdDocument::from_canvas(&app.canvas),
                        )),
                        ExportFormat::Svg => {
                            Data::Svg(crate::project::svg::document_svg(&app.canvas))
                        }
                        _ => Data::Image(app.canvas.flatten_final()),
                    };

                    app.export_state.in_progress = true;
                    app.export_state.progress = 0.05;
                    app.export_state.message = Some("Exporting...".to_string());
                    let (tx, rx) = mpsc::channel();
                    app.export_state.progress_rx = Some(rx);
                    app.export_state.task = Some(thread::spawn(move || {
                        let _ = tx.send(ExportProgress {
                            progress: 0.2,
                            message: Some("Saving file...".to_string()),
                        });
                        let result = match data {
                            Data::Image(img) => save_color_image(img, target.clone(), format),
                            Data::Layers(doc) => {
                                crate::project::export::save_psd(&doc, target.clone())
                            }
                            Data::Svg(svg) => svg.and_then(|svg| {
                                crate::project::export::save_svg(&svg, target.clone())
                            }),
                        }
                        .map(|_| target.clone());
                        match result {
                            Ok(path) => {
                                let msg = format!("Saved to {}", path.display());
                                let _ = tx.send(ExportProgress {
                                    progress: 1.0,
                                    message: Some(msg.clone()),
                                });
                                Ok(msg)
                            }
                            Err(err) => {
                                let msg = format!("Export failed: {err}");
                                let _ = tx.send(ExportProgress {
                                    progress: 1.0,
                                    message: Some(msg.clone()),
                                });
                                Err(msg)
                            }
                        }
                    }));
                }
                if ui
                    .add_enabled(!disabled, egui::Button::new("Cancel"))
                    .clicked()
                {
                    app.export_state.show_modal = false;
                }
            });
        });

    app.export_state.show_modal = open;
}

#[cfg(not(target_os = "android"))]
fn pick_file(default_name: &str) -> Option<PathBuf> {
    crate::app::settings::file_dialog()
        .set_file_name(default_name)
        .save_file()
        .inspect(|p| crate::app::settings::remember_dir(p))
}

#[cfg(target_os = "android")]
fn pick_file(_default_name: &str) -> Option<PathBuf> {
    None
}

/// Export settings tracked by the app.
#[derive(Clone)]
pub struct ExportSettings {
    pub format: ExportFormat,
    pub chosen_path: Option<PathBuf>,
    pub base_name: String,
}

impl Default for ExportSettings {
    fn default() -> Self {
        Self::new()
    }
}

impl ExportSettings {
    pub fn new() -> Self {
        Self {
            format: ExportFormat::Png,
            chosen_path: None,
            base_name: "export".to_string(),
        }
    }

    pub fn default_file_name(&self) -> String {
        format!("{}.{}", self.base_name, self.format.extension())
    }

    pub fn output_path(&self) -> PathBuf {
        if let Some(path) = &self.chosen_path {
            ensure_extension(path.clone(), self.format.extension())
        } else {
            Path::new(&self.default_file_name()).to_path_buf()
        }
    }
}

fn ensure_extension(mut path: PathBuf, ext: &str) -> PathBuf {
    match path.extension().and_then(|e| e.to_str()) {
        Some(current) if current.eq_ignore_ascii_case(ext) => path,
        _ => {
            path.set_extension(ext);
            path
        }
    }
}

pub struct ExportProgress {
    pub progress: f32,
    pub message: Option<String>,
}
