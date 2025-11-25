use crate::{
    PainterApp,
    utils::exporter::{ExportFormat, save_color_image},
};
use eframe::egui;
use eframe::egui::ColorImage;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::thread;

/// Modal dialog to export the current canvas to disk with a native file picker.
pub fn export_modal(app: &mut PainterApp, ctx: &egui::Context) {
    if !app.show_export_modal {
        return;
    }

    let mut open = app.show_export_modal;
    egui::Window::new("Export Canvas")
        .open(&mut open)
        .collapsible(false)
        .resizable(false)
        .show(ctx, |ui| {
            let settings = &mut app.export_settings;

            ui.horizontal(|ui| {
                ui.label("Format");
                egui::ComboBox::from_label("Format")
                    .selected_text(settings.format.label())
                    .show_ui(ui, |ui| {
                        ui.selectable_value(&mut settings.format, ExportFormat::PNG, "PNG");
                        ui.selectable_value(&mut settings.format, ExportFormat::JPEG, "JPEG");
                        ui.selectable_value(&mut settings.format, ExportFormat::TIFF, "TIFF");
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
                if ui.button("Choose...").clicked() {
                    if let Some(path) = pick_file(&settings.default_file_name()) {
                        settings.chosen_path = Some(path);
                    }
                }
            });

            if let Some(msg) = &app.export_message {
                ui.label(msg);
            }

            if app.export_in_progress {
                ui.add(
                    egui::ProgressBar::new(app.export_progress)
                        .desired_width(200.0)
                        .text("Exporting..."),
                );
            }

            ui.separator();
            ui.horizontal(|ui| {
                let disabled = app.export_in_progress;
                if ui
                    .add_enabled(!disabled, egui::Button::new("Export"))
                    .clicked()
                {
                    let target = settings.output_path();
                    let format = settings.format;

                    // Flatten on the UI thread, then save on a worker thread to avoid blocking.
                    let (w, h) = (app.canvas.width(), app.canvas.height());
                    let mut img = ColorImage::new([w, h], egui::Color32::TRANSPARENT);
                    app.canvas
                        .write_region_to_color_image(0, 0, w, h, &mut img, 1);

                    app.export_in_progress = true;
                    app.export_progress = 0.05;
                    app.export_message = Some("Exporting...".to_string());
                    let (tx, rx) = mpsc::channel();
                    app.export_progress_rx = Some(rx);
                    app.export_task = Some(thread::spawn(move || {
                        let _ = tx.send(ExportProgress {
                            progress: 0.2,
                            message: Some("Saving file...".to_string()),
                        });
                        let result =
                            save_color_image(img, target.clone(), format).map(|_| target.clone());
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
                    app.show_export_modal = false;
                }
            });
        });

    app.show_export_modal = open;
}

fn pick_file(default_name: &str) -> Option<PathBuf> {
    rfd::FileDialog::new()
        .set_file_name(default_name)
        .save_file()
}

/// Export settings tracked by the app.
#[derive(Clone)]
pub struct ExportSettings {
    pub format: ExportFormat,
    pub chosen_path: Option<PathBuf>,
    pub base_name: String,
}

impl ExportSettings {
    pub fn new() -> Self {
        Self {
            format: ExportFormat::PNG,
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
