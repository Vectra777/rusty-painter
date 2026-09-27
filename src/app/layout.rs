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
                    ui::tool_options::fill_options(self.app, ui, false);
                });
            }
            ToolTab::BrushSettings
                if matches!(self.app.active_tool, crate::app::tools::Tool::Liquify) =>
            {
                egui::ScrollArea::vertical().show(ui, |ui| {
                    ui.label(egui::RichText::new("LIQUIFY").small().strong());
                    ui::tool_options::liquify_options(self.app, ui, false);
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

/// Below this window width the side panels take turns instead of both
/// squeezing the canvas.
pub(crate) const NARROW_WIDTH: f32 = 760.0;
/// Canvas strip a side panel leaves free on a narrow screen.
const MIN_CANVAS_WIDTH: f32 = 48.0;
const PANEL_MIN_WIDTH: f32 = 250.0;

/// On a narrow screen keep one side panel open at a time: the one just
/// opened wins (or the color & layers panel, after a resize).
pub(crate) fn fit_panels_to_screen(app: &mut PainterApp, ctx: &egui::Context) {
    let ws = &mut app.workspace;
    if ctx.screen_rect().width() < NARROW_WIDTH && ws.show_left_panel && ws.show_right_panel {
        let (left_was_open, _) = ws.panels_last_frame;
        if left_was_open {
            ws.show_left_panel = false;
        } else {
            ws.show_right_panel = false;
        }
    }
    ws.panels_last_frame = (ws.show_left_panel, ws.show_right_panel);
}

pub(crate) fn show_tool_docks(app: &mut PainterApp, ctx: &egui::Context) {
    let dock_style = crate::ui::theme::dock_style(&ctx.style());
    let panel_frame = egui::Frame::none().fill(BG_CANVAS);
    // A panel never covers the whole canvas on a small screen.
    let max_width = (ctx.available_rect().width() - MIN_CANVAS_WIDTH).max(160.0);
    let min_width = PANEL_MIN_WIDTH.min(max_width);

    // Panels slide in and out; hidden ones take no space.
    egui::SidePanel::left("tool_dock_left")
        .resizable(true)
        .default_width(300.0_f32.min(max_width))
        .min_width(min_width)
        .max_width(max_width)
        .frame(panel_frame)
        .show_animated(ctx, app.workspace.show_left_panel, |ui| {
            show_dock(app, ui, "tool_dock_left", dock_style.clone(), min_width);
        });

    egui::SidePanel::right("tool_dock_right")
        .resizable(true)
        .default_width(290.0_f32.min(max_width))
        .min_width(min_width)
        .max_width(max_width)
        .frame(panel_frame)
        .show_animated(ctx, app.workspace.show_right_panel, |ui| {
            show_dock(app, ui, "tool_dock_right", dock_style, min_width);
        });
}

fn show_dock(
    app: &mut PainterApp,
    ui: &mut egui::Ui,
    dock_id: &'static str,
    style: egui_dock::Style,
    min_width: f32,
) {
    ui.set_min_width(min_width);
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
