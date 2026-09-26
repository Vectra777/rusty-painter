use crate::ui::style::BG_CANVAS;
use crate::{PainterApp, ui};
use eframe::egui;
use egui_dock::{DockArea, DockState, NodeIndex, TabViewer};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum ToolTab {
    BrushSettings,
    ColorPicker,
    Layers,
}

impl ToolTab {
    pub(crate) fn title(self) -> &'static str {
        match self {
            ToolTab::BrushSettings => "Brush",
            ToolTab::ColorPicker => "Color",
            ToolTab::Layers => "Layers",
        }
    }
}

pub(crate) fn default_left_dock() -> DockState<ToolTab> {
    // Presets live in their own window (toolbar button), not this dock.
    DockState::new(vec![ToolTab::BrushSettings])
}

pub(crate) fn default_right_dock() -> DockState<ToolTab> {
    let mut dock = DockState::new(vec![ToolTab::Layers]);
    dock.main_surface_mut()
        .split_above(NodeIndex::root(), 0.6, vec![ToolTab::ColorPicker]);
    dock
}

struct ToolTabViewer<'a> {
    app: &'a mut PainterApp,
    dock_id: &'static str,
}

impl<'a> TabViewer for ToolTabViewer<'a> {
    type Tab = ToolTab;

    fn title(&mut self, tab: &mut Self::Tab) -> egui::WidgetText {
        // The settings tab shows the active tool's settings.
        match (*tab, self.app.active_tool) {
            (ToolTab::BrushSettings, crate::app::tools::Tool::Fill) => "Fill".into(),
            (ToolTab::BrushSettings, crate::app::tools::Tool::Liquify) => "Liquify".into(),
            _ => tab.title().into(),
        }
    }

    fn id(&mut self, tab: &mut Self::Tab) -> egui::Id {
        egui::Id::new((self.dock_id, *tab))
    }

    fn ui(&mut self, ui: &mut egui::Ui, tab: &mut Self::Tab) {
        ui.push_id((self.dock_id, *tab, "content"), |ui| match tab {
            // Tools with their own settings show them here instead.
            ToolTab::BrushSettings
                if matches!(self.app.active_tool, crate::app::tools::Tool::Fill) =>
            {
                egui::ScrollArea::vertical().show(ui, |ui| {
                    ui.label(egui::RichText::new("FILL").small().strong());
                    ui::top_bar::fill_options(self.app, ui, false);
                });
            }
            ToolTab::BrushSettings
                if matches!(self.app.active_tool, crate::app::tools::Tool::Liquify) =>
            {
                egui::ScrollArea::vertical().show(ui, |ui| {
                    ui.label(egui::RichText::new("LIQUIFY").small().strong());
                    ui::top_bar::liquify_options(self.app, ui, false);
                });
            }
            ToolTab::BrushSettings => ui::brush_settings::brush_settings_panel(
                ui,
                &mut self.app.brush_state.brush,
                &mut self.app.brush_state.brush_preview,
                &self.app.workspace.pool,
                &self.app.brush_state.loaded_brush_tips,
            ),
            ToolTab::ColorPicker => ui::color_picker::color_picker_panel(
                ui,
                &mut self.app.brush_state,
                self.app.workspace.color_model,
            ),
            ToolTab::Layers => {
                let ctx = ui.ctx().clone();
                ui::layers::layers_panel(&ctx, ui, self.app);
            }
        });
    }

    fn closeable(&mut self, _tab: &mut Self::Tab) -> bool {
        false
    }

    fn allowed_in_windows(&self, _tab: &mut Self::Tab) -> bool {
        true
    }
}

pub(crate) fn show_tool_docks(app: &mut PainterApp, ctx: &egui::Context) {
    let dock_style = crate::styling::dock_style(&ctx.style());
    let panel_frame = egui::Frame::none().fill(BG_CANVAS);

    // Panels slide in and out; hidden ones take no space.
    egui::SidePanel::left("tool_dock_left")
        .resizable(true)
        .default_width(300.0)
        .min_width(250.0)
        .frame(panel_frame)
        .show_animated(ctx, app.workspace.show_left_panel, |ui| {
            show_dock(app, ui, "tool_dock_left", dock_style.clone());
        });

    egui::SidePanel::right("tool_dock_right")
        .resizable(true)
        .default_width(290.0)
        .min_width(250.0)
        .frame(panel_frame)
        .show_animated(ctx, app.workspace.show_right_panel, |ui| {
            show_dock(app, ui, "tool_dock_right", dock_style);
        });
}

fn show_dock(
    app: &mut PainterApp,
    ui: &mut egui::Ui,
    dock_id: &'static str,
    style: egui_dock::Style,
) {
    ui.set_min_width(250.0);
    let dock = if dock_id == "tool_dock_left" {
        &mut app.dock_left
    } else {
        &mut app.dock_right
    };
    let mut dock_state = std::mem::replace(dock, DockState::new(Vec::new()));
    {
        let mut viewer = ToolTabViewer { app, dock_id };
        DockArea::new(&mut dock_state)
            .id(egui::Id::new((dock_id, "area")))
            .style(style)
            .show_inside(ui, &mut viewer);
    }
    if dock_id == "tool_dock_left" {
        app.dock_left = dock_state;
    } else {
        app.dock_right = dock_state;
    }
}
