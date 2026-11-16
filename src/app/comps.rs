//! Layer comps: named snapshots of which layers show, at what opacity and
//! blend mode, to switch a picture between versions (with and without the
//! text, day and night colouring...) and export each. Kept in the project
//! file; applying one is a single undo step through the layer settings
//! history, like an edit in the layers panel.

use crate::PainterApp;
use crate::canvas::blend_modes::LayerBlend;
use crate::canvas::storage::{Layer, LayerId};
use serde::{Deserialize, Serialize};

/// One layer's part in a comp.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct CompLayer {
    pub id: u64,
    pub visible: bool,
    /// (A NaN is written as null: read back as fully opaque rather than
    /// failing the whole file.)
    #[serde(deserialize_with = "opacity")]
    pub opacity: f32,
    #[serde(default)]
    pub blend: LayerBlend,
}

fn opacity<'de, D: serde::Deserializer<'de>>(d: D) -> Result<f32, D::Error> {
    Ok(Option::<f32>::deserialize(d)?.unwrap_or(1.0))
}

/// A named snapshot of every layer's visibility, opacity and blend mode.
/// Layers added after it was taken are left as they are when it's applied.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct LayerComp {
    pub name: String,
    pub layers: Vec<CompLayer>,
}

impl LayerComp {
    /// The layers as they are now.
    pub fn capture(name: String, layers: &[Layer]) -> Self {
        Self {
            name,
            layers: layers
                .iter()
                .map(|l| CompLayer {
                    id: l.id.0,
                    visible: l.visible,
                    opacity: l.opacity,
                    blend: l.blend,
                })
                .collect(),
        }
    }

    /// Set the layers it knows (by id) as it has them; whether any changed.
    pub fn apply_to(&self, layers: &mut [Layer]) -> bool {
        let mut changed = false;
        for l in layers.iter_mut() {
            let Some(c) = self.layers.iter().find(|c| c.id == l.id.0) else {
                continue;
            };
            if (l.visible, l.opacity, l.blend) != (c.visible, c.opacity, c.blend) {
                (l.visible, l.opacity, l.blend) = (c.visible, c.opacity, c.blend);
                changed = true;
            }
        }
        changed
    }

    /// Read from a file: only the layers the document has (an id left over
    /// from a deleted layer could be a new layer's later), opacities that
    /// make sense.
    pub fn tidy(&mut self, ids: &[LayerId]) {
        self.layers.retain(|c| ids.contains(&LayerId(c.id)));
        for c in &mut self.layers {
            c.opacity = if c.opacity.is_finite() {
                c.opacity.clamp(0.0, 1.0)
            } else {
                1.0
            };
        }
    }
}

/// The document's comps, and a count of edits to them (they're work to
/// save, though not undo steps).
#[derive(Clone, Debug, Default)]
pub struct LayerComps {
    pub list: Vec<LayerComp>,
    pub edits: u64,
}

impl PainterApp {
    /// A new comp of the layers as they are, named `name` (or "Comp N").
    pub(crate) fn comp_add(&mut self, name: &str) {
        let comps = &mut self.layer_state.comps;
        let name = match name.trim() {
            "" => (1..)
                .map(|n| format!("Comp {n}"))
                .find(|n| !comps.list.iter().any(|c| &c.name == n))
                .unwrap_or_default(),
            name => name.to_string(),
        };
        comps
            .list
            .push(LayerComp::capture(name, &self.canvas.layers));
        comps.edits += 1;
    }

    /// Comp `i` takes the layers as they are now.
    pub(crate) fn comp_update(&mut self, i: usize) {
        let layers = &self.canvas.layers;
        let comps = &mut self.layer_state.comps;
        if let Some(c) = comps.list.get_mut(i) {
            *c = LayerComp::capture(std::mem::take(&mut c.name), layers);
            comps.edits += 1;
        }
    }

    pub(crate) fn comp_rename(&mut self, i: usize, name: &str) {
        let comps = &mut self.layer_state.comps;
        if let Some(c) = comps.list.get_mut(i)
            && !name.trim().is_empty()
            && c.name != name.trim()
        {
            c.name = name.trim().to_string();
            comps.edits += 1;
        }
    }

    pub(crate) fn comp_delete(&mut self, i: usize) {
        let comps = &mut self.layer_state.comps;
        if i < comps.list.len() {
            comps.list.remove(i);
            comps.edits += 1;
        }
    }

    /// Show comp `i`: one undo step (a layers panel edit pending is filed
    /// as its own first).
    pub(crate) fn comp_apply(&mut self, i: usize) {
        let Some(comp) = self.layer_state.comps.list.get(i).cloned() else {
            return;
        };
        self.settings_record(false);
        self.settings_watch();
        if comp.apply_to(&mut self.canvas_mut().layers) {
            self.mark_all_tiles_dirty();
            self.layer_state.thumbnails_dirty = true;
        }
        self.settings_record(false);
    }
}

#[cfg(test)]
mod tests {
    use crate::canvas::Canvas;
    use crate::canvas::blend_modes::LayerBlend;
    use crate::project::tests::test_app_pub;
    use eframe::egui::Color32;

    fn looks(app: &crate::PainterApp) -> Vec<(bool, f32, LayerBlend)> {
        (app.canvas.layers.iter())
            .map(|l| (l.visible, l.opacity, l.blend))
            .collect()
    }

    #[test]
    fn applying_a_comp_is_one_undo_step() {
        let mut canvas = Canvas::new(64, 64, Color32::WHITE, 64);
        canvas.add_layer();
        let mut app = test_app_pub(canvas);
        app.comp_add("");
        let first = looks(&app);
        {
            let layers = &mut app.canvas_mut().layers;
            layers[0].visible = false;
            layers[1].opacity = 0.25;
            layers[2].blend = LayerBlend::Multiply;
        }
        app.comp_add("Night");
        let second = looks(&app);
        assert_eq!(app.layer_state.comps.list[0].name, "Comp 1");
        assert_eq!(app.layer_state.comps.list[1].name, "Night");
        let steps = app.layer_state.history.stacks().0.len();

        app.comp_apply(0);
        assert_eq!(looks(&app), first);
        assert_eq!(app.layer_state.history.stacks().0.len(), steps + 1);
        // Showing what already shows is no step.
        app.comp_apply(0);
        assert_eq!(app.layer_state.history.stacks().0.len(), steps + 1);

        app.apply_history(false);
        assert_eq!(looks(&app), second, "undo");
        app.apply_history(true);
        assert_eq!(looks(&app), first, "redo");
    }

    #[test]
    fn a_comp_leaves_layers_it_doesnt_know() {
        let mut app = test_app_pub(Canvas::new(64, 64, Color32::WHITE, 64));
        app.comp_add("Before");
        let id = app.canvas_mut().add_layer();
        let i = app.canvas.layer_index_of(id).unwrap();
        app.canvas_mut().layers[i].opacity = 0.5;
        app.canvas_mut().layers[1].visible = false;
        app.comp_apply(0);
        assert!(app.canvas.layers[1].visible);
        assert_eq!(app.canvas.layers[i].opacity, 0.5);
    }

    #[test]
    fn comps_are_renamed_updated_deleted_and_count_as_changes() {
        let mut app = test_app_pub(Canvas::new(64, 64, Color32::WHITE, 64));
        let version = app.doc_version();
        app.comp_add("A");
        assert_ne!(app.doc_version(), version, "a change to save");
        app.comp_rename(0, "  B ");
        assert_eq!(app.layer_state.comps.list[0].name, "B");
        app.comp_rename(0, "   ");
        assert_eq!(app.layer_state.comps.list[0].name, "B");
        app.canvas_mut().layers[1].visible = false;
        app.comp_update(0);
        let comp = &app.layer_state.comps.list[0];
        assert_eq!(comp.name, "B");
        assert!(!comp.layers[1].visible);
        app.comp_delete(0);
        assert!(app.layer_state.comps.list.is_empty());
    }

    #[test]
    fn comps_are_saved_with_the_project() {
        let mut app = test_app_pub(Canvas::new(64, 64, Color32::WHITE, 64));
        // Without comps the file has no entry for them (as older files).
        let bytes = crate::project::encode_project(&app).unwrap();
        let loaded = crate::project::decode_project(&bytes).unwrap();
        assert!(loaded.comps.is_empty());

        app.canvas_mut().layers[1].opacity = 0.4;
        app.canvas_mut().layers[1].blend = LayerBlend::Screen;
        app.comp_add("Soft");
        app.canvas_mut().layers[0].visible = false;
        app.comp_add("No paper");
        // A layer the document no longer has is dropped on the way in.
        let mut stale = app.layer_state.comps.list[1].clone();
        stale.name = "Stale".into();
        stale.layers.push(super::CompLayer {
            id: 999,
            visible: true,
            opacity: f32::NAN,
            blend: LayerBlend::Normal,
        });
        app.layer_state.comps.list.push(stale);
        let want = app.layer_state.comps.list.clone();

        let bytes = crate::project::encode_project(&app).unwrap();
        let loaded = crate::project::decode_project(&bytes).unwrap();
        assert_eq!(loaded.comps[..2], want[..2]);
        assert_eq!(loaded.comps[2].layers, want[1].layers);

        let mut other = test_app_pub(Canvas::new(8, 8, Color32::WHITE, 64));
        other.open_document(crate::project::OpenedDocument::Project(Box::new(loaded)));
        assert_eq!(other.layer_state.comps.list.len(), 3);
        assert!(!other.has_unsaved_work(), "just opened");
        other.comp_apply(0);
        assert!(other.canvas.layers[0].visible);
        assert_eq!(other.canvas.layers[1].blend, LayerBlend::Screen);
    }
}
