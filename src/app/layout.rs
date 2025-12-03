use crate::{PainterApp, ui};
use eframe::egui;
use egui_dock::{DockArea, DockState, NodeIndex, TabViewer};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum ToolTab {
    BrushSettings,
    BrushPresets,
    ColorPicker,
    Layers,
}

impl ToolTab {
    pub(crate) fn title(self) -> &'static str {
        match self {
            ToolTab::BrushSettings => "Brush Settings",
            ToolTab::BrushPresets => "Brush Presets",
            ToolTab::ColorPicker => "Color Picker",
            ToolTab::Layers => "Layers",
        }
    }
}

pub(crate) fn default_left_dock() -> DockState<ToolTab> {
    let mut dock = DockState::new(vec![ToolTab::BrushSettings]);
    dock.main_surface_mut()
        .split_below(NodeIndex::root(), 0.6, vec![ToolTab::BrushPresets]);
    dock
}

pub(crate) fn default_right_dock() -> DockState<ToolTab> {
    let mut dock = DockState::new(vec![ToolTab::Layers]);
    dock.main_surface_mut()
        .split_above(NodeIndex::root(), 0.45, vec![ToolTab::ColorPicker]);
    dock
}

struct ToolTabViewer<'a> {
    app: &'a mut PainterApp,
}

impl<'a> TabViewer for ToolTabViewer<'a> {
    type Tab = ToolTab;

    fn title(&mut self, tab: &mut Self::Tab) -> egui::WidgetText {
        tab.title().into()
    }

    fn ui(&mut self, ui: &mut egui::Ui, tab: &mut Self::Tab) {
        match tab {
            ToolTab::BrushSettings => {
                ui::brush_settings::brush_settings_panel(
                    ui,
                    &mut self.app.brush,
                    &mut self.app.brush_preview,
                    &self.app.pool,
                    &self.app.loaded_brush_tips,
                )
            }
            ToolTab::BrushPresets => {
                ui::brush_list::brush_list_panel(
                    ui,
                    &mut self.app.brush,
                    &mut self.app.presets,
                    &mut self.app.preset_previews,
                    &self.app.pool,
                    &mut self.app.show_new_preset_modal,
                    &mut self.app.new_preset_name,
                )
            }
            ToolTab::ColorPicker => {
                ui::color_picker::color_picker_panel(ui, &mut self.app.brush, self.app.color_model)
            }
            ToolTab::Layers => {
                let ctx = ui.ctx().clone();
                ui::layers::layers_panel(&ctx, ui, self.app);
            }
        }
    }

    fn closeable(&mut self, _tab: &mut Self::Tab) -> bool {
        false
    }

    fn allowed_in_windows(&self, _tab: &mut Self::Tab) -> bool {
        true
    }
}

pub(crate) fn show_tool_docks(app: &mut PainterApp, ctx: &egui::Context) {
    egui::SidePanel::left("tool_dock_left")
        .resizable(true)
        .default_width(340.0)
        .min_width(260.0)
        .show(ctx, |ui| {
            ui.set_min_width(260.0);
            let mut dock_state = std::mem::replace(&mut app.dock_left, DockState::new(Vec::new()));
            {
                let mut viewer = ToolTabViewer { app };
                DockArea::new(&mut dock_state).show_inside(ui, &mut viewer);
            }
            app.dock_left = dock_state;
        });

    egui::SidePanel::right("tool_dock_right")
        .resizable(true)
        .default_width(320.0)
        .min_width(240.0)
        .show(ctx, |ui| {
            ui.set_min_width(240.0);
            let mut dock_state = std::mem::replace(&mut app.dock_right, DockState::new(Vec::new()));
            {
                let mut viewer = ToolTabViewer { app };
                DockArea::new(&mut dock_state).show_inside(ui, &mut viewer);
            }
            app.dock_right = dock_state;
        });
}
