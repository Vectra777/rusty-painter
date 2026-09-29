//! Viewing aids: the grid, guide lines and snapping to them, the
//! reference image and the navigator (all under View). Their settings are
//! kept between sessions in `view.json`, next to the brushes folder; the
//! guides themselves are saved with the project.

use crate::app::PainterApp;
use crate::app::tools::Tool;
use crate::app::view::grid::{self, GridSettings};
use crate::app::view::guide_lines::{self, GuideLine, GuideLines};
use crate::selection::SelectionType;
use eframe::egui::{self, Vec2};

/// Points snap to a guide or grid line within this distance, in screen
/// points.
pub(crate) const SNAP_DISTANCE: f32 = 8.0;

#[derive(Default)]
pub struct ViewAids {
    pub grid: GridSettings,
    pub guides: GuideLines,
    pub snap_to_guides: bool,
    pub snap_to_grid: bool,
    pub reference: crate::ui::reference_window::ReferenceState,
    pub navigator: crate::ui::navigator::NavigatorState,
    /// The settings as last read or written; `None` until they're loaded
    /// (nothing is written before then).
    saved: Option<ViewSettings>,
}

/// What `view.json` keeps.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
struct ViewSettings {
    grid: GridSettings,
    show_guides: bool,
    snap_to_guides: bool,
    snap_to_grid: bool,
    /// The reference image last opened, loaded again next time.
    reference_image: Option<std::path::PathBuf>,
}

impl Default for ViewSettings {
    fn default() -> Self {
        Self {
            grid: GridSettings::default(),
            show_guides: true,
            snap_to_guides: false,
            snap_to_grid: false,
            reference_image: None,
        }
    }
}

impl ViewSettings {
    fn of(aids: &ViewAids) -> Self {
        Self {
            grid: aids.grid,
            show_guides: aids.guides.show,
            snap_to_guides: aids.snap_to_guides,
            snap_to_grid: aids.snap_to_grid,
            reference_image: aids.reference.path.clone(),
        }
    }

    fn apply(&self, aids: &mut ViewAids) {
        aids.grid = self.grid;
        aids.guides.show = self.show_guides;
        aids.snap_to_guides = self.snap_to_guides;
        aids.snap_to_grid = self.snap_to_grid;
        aids.reference.path = self.reference_image.clone();
    }
}

/// `p` snapped: each axis onto the nearest guide within `threshold`, and
/// onto the grid of `grid` = (step, isometric) where no guide took it (an
/// isometric grid only when no guide did).
pub(crate) fn snap(
    p: Vec2,
    guides: &[GuideLine],
    grid: Option<(f32, bool)>,
    threshold: f32,
) -> Vec2 {
    let (x, y) = guide_lines::snap_to_lines(p, guides, threshold);
    let on_grid =
        |p: Vec2| grid.map_or(p, |(step, iso)| grid::snap_to_grid(p, step, iso, threshold));
    match (x, y, grid) {
        (None, None, _) => on_grid(p),
        (_, _, Some((_, true))) => Vec2::new(x.unwrap_or(p.x), y.unwrap_or(p.y)),
        _ => {
            let g = on_grid(p);
            Vec2::new(x.unwrap_or(g.x), y.unwrap_or(g.y))
        }
    }
}

impl PainterApp {
    fn snap_threshold(&self) -> f32 {
        SNAP_DISTANCE / self.viewport.zoom.max(0.01)
    }

    /// The grid snapping uses: the finest lines shown, when snapping to it.
    fn snap_grid(&self) -> Option<(f32, bool)> {
        let aids = &self.workspace.view_aids;
        let g = &aids.grid;
        if !(aids.snap_to_grid && g.show) {
            return None;
        }
        let steps = grid::grid_steps(g.spacing, g.subdivisions, self.viewport.zoom)?;
        Some((steps.finest(), g.isometric))
    }

    /// Canvas point `p` snapped to the guides and the grid, when snapping
    /// to them is on (and they're shown).
    pub(crate) fn snap_point(&self, p: Vec2) -> Vec2 {
        let aids = &self.workspace.view_aids;
        let guides: &[GuideLine] = if aids.snap_to_guides && aids.guides.show {
            &aids.guides.lines
        } else {
            &[]
        };
        let grid = self.snap_grid();
        if guides.is_empty() && grid.is_none() {
            return p;
        }
        snap(p, guides, grid, self.snap_threshold())
    }

    /// `p` snapped to the grid alone (a guide being moved).
    pub(crate) fn snap_to_grid_only(&self, p: Vec2) -> Vec2 {
        match self.snap_grid() {
            Some(grid) => snap(p, &[], Some(grid), self.snap_threshold()),
            None => p,
        }
    }

    /// Whether the active tool's points snap: on the press only (`press`)
    /// for the brush, whose stroke then runs freely; throughout for
    /// selection shapes, shapes, the gradient and text.
    fn tool_snaps(&self, press: bool) -> bool {
        match self.active_tool {
            Tool::Brush => press,
            Tool::Select(kind) => matches!(
                kind,
                SelectionType::Rectangle | SelectionType::Circle | SelectionType::Polygon
            ),
            Tool::Shape(_) | Tool::Gradient | Tool::Text => true,
            _ => false,
        }
    }

    /// A tool's pointer position, snapped if that tool snaps (see
    /// [`Self::tool_snaps`]). `clamp` keeps it on the canvas.
    pub(crate) fn tool_snap(&self, p: Vec2, press: bool, clamp: bool) -> Vec2 {
        if !self.tool_snaps(press) {
            return p;
        }
        let q = self.snap_point(p);
        if clamp {
            let (w, h) = (self.canvas.width() as f32, self.canvas.height() as f32);
            Vec2::new(q.x.clamp(0.0, w), q.y.clamp(0.0, h))
        } else {
            q
        }
    }

    fn view_settings_path(&self) -> std::path::PathBuf {
        self.brush_state.brushes_path.with_file_name("view.json")
    }

    /// The view settings saved last time (the defaults if there are none).
    pub(crate) fn load_view_settings(&mut self) {
        let settings = match std::fs::read(self.view_settings_path()) {
            Ok(bytes) => serde_json::from_slice::<ViewSettings>(&bytes).unwrap_or_else(|err| {
                log::warn!("Ignoring view.json: {err}");
                ViewSettings::default()
            }),
            Err(_) => ViewSettings::default(),
        };
        let aids = &mut self.workspace.view_aids;
        settings.apply(aids);
        aids.saved = Some(settings);
    }

    /// Write the view settings when they've changed (once the pointer is
    /// up, not every frame of a slider drag). Errors are logged.
    pub(crate) fn save_view_settings(&mut self, ctx: &egui::Context) {
        let aids = &self.workspace.view_aids;
        let Some(saved) = &aids.saved else {
            return;
        };
        let now = ViewSettings::of(aids);
        if *saved == now || ctx.input(|i| i.pointer.any_down()) {
            return;
        }
        let result = serde_json::to_vec_pretty(&now)
            .map_err(|e| e.to_string())
            .and_then(|bytes| crate::project::write_atomically(&self.view_settings_path(), &bytes));
        if let Err(err) = result {
            log::warn!("Couldn't save the view settings: {err}");
        }
        self.workspace.view_aids.saved = Some(now);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::canvas::Canvas;
    use eframe::egui::Color32;

    const V: fn(f32) -> GuideLine = |pos| GuideLine {
        vertical: true,
        pos,
    };

    #[test]
    fn guides_win_over_the_grid_on_their_axis() {
        let guides = [V(33.0)];
        // x onto the guide, y onto the grid.
        assert_eq!(
            snap(Vec2::new(35.0, 52.0), &guides, Some((50.0, false)), 4.0),
            Vec2::new(33.0, 50.0)
        );
        // Out of reach of both: unchanged.
        let p = Vec2::new(40.0, 60.0);
        assert_eq!(snap(p, &guides, Some((50.0, false)), 4.0), p);
        // No grid: guides alone.
        assert_eq!(
            snap(Vec2::new(31.0, 52.0), &guides, None, 4.0),
            Vec2::new(33.0, 52.0)
        );
    }

    #[test]
    fn snapping_follows_the_view_settings_and_screen_distance() {
        let mut app =
            crate::project::tests::test_app_pub(Canvas::new(400, 300, Color32::WHITE, 64));
        app.add_guide(true, 100.0);
        let p = Vec2::new(105.0, 20.0);
        // Snapping off: nothing moves.
        assert_eq!(app.snap_point(p), p);
        app.workspace.view_aids.snap_to_guides = true;
        // 5 canvas px at 100% is within 8 screen points…
        assert_eq!(app.snap_point(p), Vec2::new(100.0, 20.0));
        // …but at 400% it's 20 points away.
        app.viewport.zoom = 4.0;
        assert_eq!(app.snap_point(p), p);
        // Hidden guides don't snap.
        app.viewport.zoom = 1.0;
        app.workspace.view_aids.guides.show = false;
        assert_eq!(app.snap_point(p), p);
        // The grid snaps to its finest shown lines (25 px quarters here).
        app.workspace.view_aids.grid.show = true;
        app.workspace.view_aids.snap_to_grid = true;
        assert_eq!(app.snap_point(Vec2::new(73.0, 51.0)), Vec2::new(75.0, 50.0));
        // The brush snaps where a stroke starts, not along it.
        assert_eq!(
            app.tool_snap(Vec2::new(73.0, 51.0), true, false),
            Vec2::new(75.0, 50.0)
        );
        assert_eq!(
            app.tool_snap(Vec2::new(73.0, 51.0), false, false),
            Vec2::new(73.0, 51.0)
        );
    }

    #[test]
    fn view_settings_read_older_or_partial_files() {
        let s: ViewSettings = serde_json::from_str(r#"{"snap_to_grid": true}"#).unwrap();
        assert!(s.snap_to_grid && s.show_guides);
        assert_eq!(s.grid, GridSettings::default());
    }
}
