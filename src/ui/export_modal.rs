//! The Export dialog: format, file name and progress.

use crate::ui::widgets::FitScreen;
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
        .fit_screen(ctx)
        .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
        .open(&mut open)
        .collapsible(false)
        .resizable(false)
        .show(ctx, |ui| {
            let settings = &mut app.export_state.settings;
            let mut choose = false;

            ui.horizontal(|ui| {
                ui.label("Format");
                egui::ComboBox::from_label("Format")
                    .selected_text(settings.format.label())
                    .show_ui(ui, |ui| {
                        for format in ExportFormat::ALL {
                            ui.selectable_value(&mut settings.format, format, format.label());
                        }
                    });
            });

            ui.separator();
            ui.heading("Destination");
            // Android: into shared storage, by type (see `publish_file`).
            if cfg!(target_os = "android") {
                ui.horizontal(|ui| {
                    ui.label("Name");
                    ui.text_edit_singleline(&mut settings.base_name);
                });
                let place = if settings.format.is_layered() {
                    "Download/Rusty Painter"
                } else {
                    "Pictures/Rusty Painter"
                };
                ui.weak(format!("Saved in {place}"));
            } else {
                ui.horizontal(|ui| {
                    ui.label("File");
                    let display = settings
                        .chosen_path
                        .as_ref()
                        .map(|p| p.display().to_string())
                        .unwrap_or_else(|| settings.default_file_name());
                    ui.monospace(display);
                    if ui.button("Choose...").clicked() {
                        choose = true;
                    }
                });
            }

            if choose {
                choose_file(app);
            }

            if let Some(msg) = &app.export_state.message {
                ui.label(msg);
            }
            #[cfg(target_os = "android")]
            if let Some((uri, mime)) = &app.export_state.share
                && ui.button("Share…").clicked()
                && let Err(err) = crate::android::share_uri(uri, mime, "Share image")
            {
                app.export_state.message = Some(err);
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

                    app.export_state.share = None;
                    app.export_state.in_progress = true;
                    app.export_state.progress = 0.05;
                    app.export_state.message = Some("Exporting...".to_string());
                    // Once the strokes are painted: copy the document, then
                    // flatten and save it on another thread.
                    app.when_strokes_painted(move |app| start_export(app, target, format));
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

/// Copy the document as it is now and export the copy on another thread.
fn start_export(app: &mut PainterApp, target: PathBuf, format: ExportFormat) {
    // Shader layers export as their current frame.
    app.bake_shader_layers();
    let canvas = app.canvas.detached_copy();
    let (tx, rx) = mpsc::channel();
    app.export_state.progress_rx = Some(rx);
    app.export_state.task = Some(thread::spawn(move || {
        let _ = tx.send(ExportProgress {
            progress: 0.2,
            message: Some("Saving file...".to_string()),
            share: None,
        });
        let result = match format {
            ExportFormat::Psd => crate::project::export::save_psd(
                &crate::project::psd::PsdDocument::from_canvas(&canvas),
                target.clone(),
            ),
            ExportFormat::Svg => crate::project::svg::document_svg(&canvas)
                .and_then(|svg| crate::project::export::save_svg(&svg, target.clone())),
            _ => save_color_image(canvas.flatten_final(), target.clone(), format),
        }
        .and_then(|_| published(&target, format));
        match result {
            Ok((msg, share)) => {
                let _ = tx.send(ExportProgress {
                    progress: 1.0,
                    message: Some(msg.clone()),
                    share,
                });
                Ok(msg)
            }
            Err(err) => {
                let msg = format!("Export failed: {err}");
                let _ = tx.send(ExportProgress {
                    progress: 1.0,
                    message: Some(msg.clone()),
                    share: None,
                });
                Err(msg)
            }
        }
    }));
}

#[cfg(not(target_os = "android"))]
fn choose_file(app: &mut PainterApp) {
    let dialog = crate::app::settings::file_dialog()
        .set_file_name(app.export_state.settings.default_file_name());
    app.file_dialog_job(dialog, crate::app::jobs::Pick::Save, |app, paths| {
        app.export_state.settings.chosen_path = Some(paths[0].clone());
    });
}

#[cfg(target_os = "android")]
fn choose_file(_app: &mut PainterApp) {}

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
        // Written to the cache, then moved to shared storage.
        if cfg!(target_os = "android") {
            return android_cache().join(self.default_file_name());
        }
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
    /// Android: the exported file's (URI, MIME type), to share.
    pub share: Option<(String, String)>,
}

/// The written export's message, and what to share it by.
#[cfg(not(target_os = "android"))]
fn published(
    path: &Path,
    _format: ExportFormat,
) -> Result<(String, Option<(String, String)>), String> {
    Ok((format!("Saved to {}", path.display()), None))
}

/// Android: moved from the cache into shared storage.
#[cfg(target_os = "android")]
fn published(
    path: &Path,
    format: ExportFormat,
) -> Result<(String, Option<(String, String)>), String> {
    let done = crate::android::publish_file(path, format.mime_type())?;
    Ok((done.message, done.share_uri.zip(done.share_mime)))
}

#[cfg(target_os = "android")]
fn android_cache() -> PathBuf {
    crate::android::cache_dir()
}

#[cfg(not(target_os = "android"))]
fn android_cache() -> PathBuf {
    unreachable!("only on Android")
}
