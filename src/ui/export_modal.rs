//! The Export dialog: format, size, presets, file name, batch export and
//! progress.

use crate::project::export::{
    BatchChoice, ExportFormat, ExportOptions, ExportPreset, SCALE_RANGE, batch_files, batch_layers,
    run_batch,
};
use crate::ui::widgets::FitScreen;
use crate::{PainterApp, app::document::validate_canvas_size};
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

    // The dialog's own copy of how its export went: the shared notice
    // over the canvas goes after a while (or when dismissed), but what
    // the dialog says stays while it's open. `None` until it exported.
    let result_id = egui::Id::new("export_modal_result");
    let running = app.export_state.in_progress;
    let mut result: Option<ExportResult> = ctx.data(|d| d.get_temp(result_id));
    // (A batch starts once its folder is picked, outside the dialog.)
    if running && result.is_none() {
        result = Some(ExportResult { message: None });
    }
    if let Some(r) = &mut result
        && r.message.is_none()
        && !running
    {
        // Finished: what it said.
        r.message = Some(app.export_state.message.clone().unwrap_or_default());
    }
    let mut open = app.export_state.show_modal;
    egui::Window::new("Export Canvas")
        .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
        .open(&mut open)
        .collapsible(false)
        .resizable(false)
        .show_fitted(ctx, |ui| {
            let canvas_size = (app.canvas.width(), app.canvas.height());
            let settings = &mut app.export_state.settings;
            let mut choose = false;
            let mut batch = false;

            options_ui(ui, &mut settings.options, canvas_size);
            presets_ui(ui, settings);

            ui.separator();
            ui.heading("Destination");
            // Android: into shared storage, by type (see `publish_file`).
            if cfg!(mobile) {
                ui.horizontal(|ui| {
                    ui.label("Name");
                    ui.text_edit_singleline(&mut settings.base_name);
                });
                let place = if settings.options.format.is_layered() {
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
                    if ui.button("Choose…").clicked() {
                        choose = true;
                    }
                });
            }

            ui.separator();
            let layers = batch_layers(&app.canvas).len();
            let comps = app.layer_state.comps.list.len();
            egui::CollapsingHeader::new("Batch export")
                .id_salt("export_batch")
                .show(ui, |ui| {
                    let s = &mut app.export_state.settings;
                    let b = &mut s.batch;
                    let n = s.presets.len();
                    let presets = format!("The picture with each preset ({n})");
                    ui.add_enabled(n > 0, egui::Checkbox::new(&mut b.presets, presets))
                        .on_disabled_hover_text("Save presets above first");
                    let each_comp = format!("Each layer comp ({comps})");
                    ui.add_enabled(comps > 0, egui::Checkbox::new(&mut b.comps, each_comp))
                        .on_disabled_hover_text("Save layer comps in the layers panel (Comps)");
                    let each_layer = format!("Each visible top-level layer or folder ({layers})");
                    ui.checkbox(&mut b.layers, each_layer)
                        .on_hover_text("Each alone, on transparency (the paper hidden)");
                    let count = (b.presets && n > 0) as usize * n
                        + (b.comps && comps > 0) as usize * comps
                        + b.layers as usize * layers;
                    ui.weak(format!(
                        "{count} files named {}-<name>; comps and layers as set above",
                        s.batch_base_name()
                    ));
                    let label = if cfg!(mobile) {
                        "Export All"
                    } else {
                        "Export All to Folder…"
                    };
                    if ui
                        .add_enabled(count > 0 && !running, egui::Button::new(label))
                        .clicked()
                    {
                        batch = true;
                    }
                });

            if choose {
                choose_file(app);
            }
            if batch {
                choose_batch_folder(app);
            }

            if let Some(msg) = result.as_ref().and_then(|r| r.message.as_deref()) {
                let failed = crate::app::layout::notice_kind(msg, false)
                    == crate::app::layout::NoticeKind::Error;
                if failed {
                    ui.colored_label(egui::Color32::from_rgb(232, 110, 100), msg);
                } else {
                    ui.label(msg);
                }
            }
            #[cfg(mobile)]
            if let Some((uri, mime)) = &app.export_state.share
                && ui.button("Share…").clicked()
                && let Err(err) = crate::platform::share_uri(uri, mime, "Share image")
            {
                app.export_state.message = Some(err);
            }

            if app.export_state.in_progress {
                ui.add(
                    egui::ProgressBar::new(app.export_state.progress)
                        .desired_width(200.0)
                        .text("Exporting…"),
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
                    let options = app.export_state.settings.options.clone();

                    let (w, h) = (app.canvas.width(), app.canvas.height());
                    if let Err(msg) = validate_canvas_size(w, h) {
                        let msg = format!("Export blocked: {msg}");
                        app.export_state.message = Some(msg.clone());
                        result = Some(ExportResult { message: Some(msg) });
                        return;
                    }

                    app.export_state.share = None;
                    app.export_state.in_progress = true;
                    app.export_state.progress = 0.05;
                    app.export_state.message = Some("Exporting…".to_string());
                    result = Some(ExportResult { message: None });
                    // Once the strokes are painted: copy the document, then
                    // flatten and save it on another thread.
                    app.when_strokes_painted(move |app| start_export(app, target, options));
                }
                // Exported (Android stays open to share): nothing to cancel.
                let done = result
                    .as_ref()
                    .and_then(|r| r.message.as_deref())
                    .is_some_and(|m| {
                        crate::app::layout::notice_kind(m, false)
                            != crate::app::layout::NoticeKind::Error
                    });
                if ui
                    .add_enabled(
                        !disabled,
                        egui::Button::new(if done { "Done" } else { "Cancel" }),
                    )
                    .clicked()
                {
                    app.export_state.show_modal = false;
                }
            });
        });

    app.export_state.show_modal = open;
    // Closed: the next time it opens fresh.
    let result = result.filter(|_| app.export_state.show_modal || running);
    ctx.data_mut(|d| match result {
        Some(r) => d.insert_temp(result_id, r),
        None => d.remove::<ExportResult>(result_id),
    });
}

/// How the export started from the dialog went: `message` once it ended.
#[derive(Clone, Debug, Default)]
struct ExportResult {
    message: Option<String>,
}

/// The format, size and trim.
fn options_ui(ui: &mut egui::Ui, options: &mut ExportOptions, (w, h): (usize, usize)) {
    ui.horizontal(|ui| {
        ui.label("Format");
        egui::ComboBox::from_id_salt("export_format")
            .selected_text(options.format.label())
            .show_ui(ui, |ui| {
                for format in ExportFormat::ALL {
                    ui.selectable_value(&mut options.format, format, format.label());
                }
            });
    });
    if options.format == ExportFormat::Jpeg {
        ui.horizontal(|ui| {
            ui.label("Quality");
            ui.add(egui::Slider::new(&mut options.jpeg_quality, 1..=100));
        });
    }
    let reshape = options.can_reshape();
    ui.add_enabled_ui(reshape, |ui| {
        ui.horizontal(|ui| {
            ui.label("Size");
            ui.add(
                egui::DragValue::new(&mut options.scale_percent)
                    .range(SCALE_RANGE)
                    .suffix(" %"),
            );
            let [ew, eh] = options.size_of(w, h);
            ui.weak(format!("{ew} × {eh} px"));
        });
        ui.checkbox(&mut options.trim, "Trim transparent edges");
    })
    .response
    .on_disabled_hover_text("PSD and SVG are written at the canvas's size");
}

/// Saved export options: pick one, save the dialog's, delete one.
fn presets_ui(ui: &mut egui::Ui, settings: &mut ExportSettings) {
    ui.horizontal(|ui| {
        ui.label("Preset");
        let shown = if settings.presets.is_empty() {
            "None saved"
        } else {
            "Pick…"
        };
        egui::ComboBox::from_id_salt("export_preset")
            .selected_text(shown)
            .show_ui(ui, |ui| {
                for p in &settings.presets {
                    let text = format!("{}: {}", p.name, p.options.summary());
                    if ui
                        .selectable_label(p.options == settings.options, text)
                        .clicked()
                    {
                        settings.options = p.options.clone();
                        settings.preset_name = p.name.clone();
                    }
                }
            });
    });
    ui.horizontal(|ui| {
        ui.add(
            egui::TextEdit::singleline(&mut settings.preset_name)
                .hint_text("Preset name")
                .desired_width(120.0),
        );
        let name = settings.preset_name.trim().to_string();
        let existing = settings.presets.iter().position(|p| p.name == name);
        let save = if existing.is_some() { "Update" } else { "Save" };
        if ui
            .add_enabled(!name.is_empty(), egui::Button::new(save))
            .on_hover_text("Keep these options under this name")
            .clicked()
        {
            settings.save_preset(&name);
        }
        if let Some(i) = existing
            && ui.button("Delete").clicked()
        {
            settings.presets.remove(i);
        }
    });
}

/// The colour a copy of the document is exported with.
fn export_color(app: &mut PainterApp) -> crate::project::export::ExportColor {
    crate::project::export::ExportColor {
        profile: app.canvas.profile.clone(),
        cmyk: app.workspace.color.cmyk_profile(),
        intent: app.workspace.color.intent,
        jpeg_quality: None,
    }
}

/// Copy the document as it is now and export the copy on another thread.
fn start_export(app: &mut PainterApp, target: PathBuf, options: ExportOptions) {
    // Shader layers export as their current frame, vector layers as
    // they'll show once drawn.
    app.bake_shader_layers();
    app.settle_vectors();
    let canvas = app.canvas.detached_copy();
    let color = export_color(app);
    let format = options.format;
    let (tx, rx) = mpsc::channel();
    app.export_state.progress_rx = Some(rx);
    app.export_state.task = Some(thread::spawn(move || {
        let _ = tx.send(ExportProgress {
            progress: 0.2,
            message: Some("Saving file...".to_string()),
            share: None,
        });
        let result = crate::project::export::export_with(&canvas, &target, &options, &color)
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

/// Start a batch export into `dir`: the files are written on another
/// thread from a copy of the document once the strokes are painted.
fn begin_batch(app: &mut PainterApp, dir: PathBuf) {
    if app.export_state.in_progress {
        return;
    }
    let e = &mut app.export_state;
    e.share = None;
    e.in_progress = true;
    e.progress = 0.0;
    e.message = Some("Exporting…".to_string());
    app.when_strokes_painted(move |app| start_batch(app, dir));
}

fn start_batch(app: &mut PainterApp, dir: PathBuf) {
    app.bake_shader_layers();
    app.settle_vectors();
    let canvas = app.canvas.detached_copy();
    let color = export_color(app);
    let s = &app.export_state.settings;
    let files = batch_files(
        &canvas,
        &s.batch_base_name(),
        &s.options,
        &s.presets,
        &app.layer_state.comps.list,
        s.batch,
    );
    let (tx, rx) = mpsc::channel();
    app.export_state.progress_rx = Some(rx);
    app.export_state.task = Some(thread::spawn(move || {
        let n = files.len();
        let result = run_batch(&canvas, &files, &dir, &color, |k, file, path| {
            published(path, file.options.format)?;
            let _ = tx.send(ExportProgress {
                progress: (k + 1) as f32 / n.max(1) as f32,
                message: Some(format!("Exported {} of {n}: {}", k + 1, file.name)),
                share: None,
            });
            Ok(())
        });
        let result = match result {
            Ok(n) if cfg!(mobile) => Ok(format!("Exported {n} files")),
            Ok(n) => Ok(format!("Exported {n} files to {}", dir.display())),
            Err(err) => Err(format!("Export failed: {err}")),
        };
        let _ = tx.send(ExportProgress {
            progress: 1.0,
            message: Some(result.clone().unwrap_or_else(|e| e)),
            share: None,
        });
        result
    }));
}

#[cfg(not(mobile))]
fn choose_file(app: &mut PainterApp) {
    let dialog = crate::app::settings::file_dialog()
        .set_file_name(app.export_state.settings.default_file_name());
    app.file_dialog_job(dialog, crate::app::jobs::Pick::Save, |app, paths| {
        app.export_state.settings.chosen_path = Some(paths[0].clone());
    });
}

#[cfg(mobile)]
fn choose_file(_app: &mut PainterApp) {}

/// Desktop: the folder a batch goes into, then the batch.
#[cfg(not(mobile))]
fn choose_batch_folder(app: &mut PainterApp) {
    let dialog = crate::app::settings::file_dialog();
    app.file_dialog_job(dialog, crate::app::jobs::Pick::Folder, |app, paths| {
        begin_batch(app, paths[0].clone());
    });
}

/// Android and iOS: written to the cache, each file moved into shared
/// storage once written.
#[cfg(mobile)]
fn choose_batch_folder(app: &mut PainterApp) {
    begin_batch(app, android_cache());
}

/// Export settings tracked by the app.
#[derive(Clone)]
pub struct ExportSettings {
    /// Format, size, trim, JPEG quality.
    pub options: ExportOptions,
    pub chosen_path: Option<PathBuf>,
    pub base_name: String,
    /// Saved options (kept in the app's settings).
    pub presets: Vec<ExportPreset>,
    /// The name typed for a preset to save (or picked, to update or
    /// delete).
    pub preset_name: String,
    /// What a batch export writes.
    pub batch: BatchChoice,
}

impl Default for ExportSettings {
    fn default() -> Self {
        Self::new()
    }
}

impl ExportSettings {
    pub fn new() -> Self {
        Self {
            options: ExportOptions::default(),
            chosen_path: None,
            base_name: "export".to_string(),
            presets: Vec::new(),
            preset_name: String::new(),
            batch: BatchChoice::default(),
        }
    }

    pub fn default_file_name(&self) -> String {
        format!("{}.{}", self.base_name, self.options.format.extension())
    }

    /// What a batch's file names start with: the chosen file's name, or
    /// the base name.
    pub fn batch_base_name(&self) -> String {
        (self.chosen_path.as_ref())
            .and_then(|p| p.file_stem())
            .map_or_else(
                || self.base_name.clone(),
                |s| s.to_string_lossy().into_owned(),
            )
    }

    /// Keep the options as preset `name` (replacing one of that name).
    pub fn save_preset(&mut self, name: &str) {
        let preset = ExportPreset {
            name: name.to_string(),
            options: self.options.clone(),
        };
        match self.presets.iter_mut().find(|p| p.name == name) {
            Some(p) => *p = preset,
            None => self.presets.push(preset),
        }
    }

    pub fn output_path(&self) -> PathBuf {
        // Written to the cache, then moved to shared storage.
        if cfg!(mobile) {
            return android_cache().join(self.default_file_name());
        }
        if let Some(path) = &self.chosen_path {
            ensure_extension(path.clone(), self.options.format.extension())
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
#[cfg(not(mobile))]
fn published(
    path: &Path,
    _format: ExportFormat,
) -> Result<(String, Option<(String, String)>), String> {
    Ok((format!("Saved to {}", path.display()), None))
}

/// Android: moved from the cache into shared storage.
#[cfg(mobile)]
fn published(
    path: &Path,
    format: ExportFormat,
) -> Result<(String, Option<(String, String)>), String> {
    let done = crate::platform::publish_file(path, format.mime_type())?;
    Ok((done.message, done.share_uri.zip(done.share_mime)))
}

#[cfg(mobile)]
fn android_cache() -> PathBuf {
    crate::platform::cache_dir()
}

#[cfg(not(mobile))]
fn android_cache() -> PathBuf {
    unreachable!("only on Android")
}
