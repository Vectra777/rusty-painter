//! The loading screen shown while the app starts (see
//! [`crate::app::init::LOAD_STEPS`]).

use crate::app::init::LOAD_STEPS;
use crate::ui::style::{ACCENT, BG_CANVAS, TEXT_DIM, TEXT_STRONG};
use eframe::egui::{self, RichText};

/// The app's name, a progress bar and what's loading: `step` of the steps.
pub fn splash(ctx: &egui::Context, step: usize) {
    let total = LOAD_STEPS.len();
    let label = LOAD_STEPS.get(step).map_or("Starting", |(label, _)| label);
    egui::CentralPanel::default()
        .frame(egui::Frame::none().fill(BG_CANVAS))
        .show(ctx, |ui| {
            let width = ui.available_width().min(320.0);
            ui.add_space((ui.available_height() * 0.5 - 50.0).max(0.0));
            ui.vertical_centered(|ui| {
                ui.label(RichText::new(crate::APP_NAME).size(28.0).color(TEXT_STRONG));
                ui.add_space(16.0);
                ui.add(
                    egui::ProgressBar::new(step as f32 / total as f32)
                        .desired_width(width)
                        .fill(ACCENT),
                );
                ui.add_space(8.0);
                ui.label(RichText::new(format!("{label}…")).color(TEXT_DIM));
            });
        });
}
