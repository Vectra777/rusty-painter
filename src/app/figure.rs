//! 3D figure layers in the app: adding one, editing its scene (one undo
//! step a gesture, like rig edits), pose keys at frames (the figures
//! follow the frame showing), matching the camera to a perspective ruler,
//! rasterising, and baking its render into the layer's pixels — before
//! export, saving and merging, for thumbnails, at every frame of an
//! animation export, and as it's edited while it can't show live on the
//! GPU (in a folder, masked: see `canvas::shader::live_layout`).

use std::collections::HashMap;
use std::time::{Duration, Instant};

use std::sync::Arc;

use crate::PainterApp;
use crate::canvas::figure::presets::{
    self, BODY_PRESETS, HAND_PRESETS, Pose, PoseKind, SavedPose, is_finger,
};
use crate::canvas::figure::skeleton::{Finger, HumanJoint, Side};
use crate::canvas::figure::{EmbeddedModel, FigureInstance, FigureLayer, ModelRef, Vanishing};
use crate::canvas::rig::Curve;
use crate::canvas::storage::LayerId;

/// While a figure shows live, its pixels (thumbnails, colour picking) are
/// baked again once its scene has stayed unchanged this long.
const SETTLED_EDIT: Duration = Duration::from_millis(600);

/// While a figure can't show live, it's baked again at most this often
/// during a drag (and at once when the drag ends).
const FALLBACK_BAKE_EVERY: Duration = Duration::from_millis(120);

#[derive(Default)]
pub struct FigureState {
    /// An edit gesture is going on: it already has its undo step.
    pub in_gesture: bool,
    /// The Figure tool: the selected joint and the drag going on.
    pub tool: crate::app::tools::figure::FigureToolState,
    /// What each layer was last baked from (see `bake_key`).
    baked: HashMap<LayerId, u64>,
    /// When each live layer's scene last changed, and to what.
    changed: HashMap<LayerId, (u64, Instant)>,
    last_fallback_bake: Option<Instant>,
    /// A pose copied, to paste on any figure.
    pub clipboard: Option<Pose>,
    /// The poses the user saved; read from disk when first wanted.
    saved: Option<Vec<SavedPose>>,
    /// Where they're kept (the data folder's `figure_poses.json` unless
    /// set: tests keep theirs elsewhere).
    pub poses_path: Option<std::path::PathBuf>,
    /// The hand pad.
    pub pad: HandPad,
    /// Which hand hand poses go on.
    pub hand: Side,
    /// The name a pose is about to be saved under.
    pub pose_name: String,
    /// The Hand Scanner (a hand posed from the camera).
    pub scanner: crate::app::hand_scanner::HandScanner,
}

/// The hand pad (CSP's): a point on a square, across for how far the
/// fingers spread, down for how far they curl; fingers locked keep theirs.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct HandPad {
    pub spread: f32,
    pub curl: f32,
    pub locked: [bool; 5],
}

impl Default for HandPad {
    fn default() -> Self {
        Self {
            spread: 0.4,
            curl: 0.0,
            locked: [false; 5],
        }
    }
}

impl FigureState {
    /// Drop everything tied to the document's layers (a new one replaces
    /// it; its layer ids may be the same).
    pub fn forget_document(&mut self) {
        self.baked.clear();
        self.changed.clear();
        self.in_gesture = false;
        self.tool = Default::default();
        self.scanner.forget_document();
    }

    fn poses_path(&self) -> std::path::PathBuf {
        self.poses_path
            .clone()
            .unwrap_or_else(|| crate::app::init::data_dir().join("figure_poses.json"))
    }

    /// The poses the user saved.
    pub fn saved_poses(&mut self) -> &[SavedPose] {
        if self.saved.is_none() {
            let read = std::fs::read(self.poses_path())
                .ok()
                .and_then(|bytes| serde_json::from_slice(&bytes).ok())
                .unwrap_or_default();
            self.saved = Some(read);
        }
        self.saved.as_deref().unwrap_or_default()
    }

    /// Change the saved poses with `change`, and write them.
    fn edit_saved(&mut self, change: impl FnOnce(&mut Vec<SavedPose>)) -> Result<(), String> {
        self.saved_poses();
        let saved = self.saved.get_or_insert_with(Vec::new);
        change(saved);
        let bytes = serde_json::to_vec_pretty(saved).map_err(|e| e.to_string())?;
        crate::project::write_atomically(&self.poses_path(), &bytes)
            .map_err(|e| format!("Couldn't save the poses: {e}"))
    }
}

impl PainterApp {
    /// Layer → New 3D Figure: a figure layer above the selected one, the
    /// mannequin standing in it, and the Figure tool to pose it.
    pub(crate) fn add_figure_layer(&mut self) -> Option<usize> {
        let idx = self.add_layer_with_tiles("3D Figure".into(), Vec::new(), |layer| {
            layer.figure = Some(Box::new(FigureLayer::mannequin()));
            // Its pixels come from its scene: no painting on it.
            layer.locked = true;
        })?;
        self.active_tool = crate::app::tools::Tool::Figure;
        self.workspace.figure.tool = Default::default();
        if let Err(err) = self.bake_figure_layer(idx) {
            log::info!("a new figure layer isn't baked yet: {err}");
        }
        Some(idx)
    }

    /// The selected layer's scene, if it's a figure layer.
    pub(crate) fn active_figure(&self) -> Option<&FigureLayer> {
        self.canvas
            .layers
            .get(self.canvas.active_layer_idx)?
            .figure
            .as_deref()
    }

    /// Change the selected figure layer's scene with `edit`: the first
    /// change of a gesture is an undo step, `label` in the History panel
    /// (the rest join it).
    pub(crate) fn figure_edit(&mut self, label: &str, edit: impl FnOnce(&mut FigureLayer)) {
        self.figure_edit_at(self.canvas.active_layer_idx, label, edit);
    }

    /// [`Self::figure_edit`] on figure layer `i` (selected or not).
    fn figure_edit_at(&mut self, i: usize, label: &str, edit: impl FnOnce(&mut FigureLayer)) {
        if self.canvas.layers.get(i).is_none_or(|l| l.figure.is_none()) {
            return;
        }
        if !self.workspace.figure.in_gesture {
            self.workspace.figure.in_gesture = true;
            self.layer_state.history.label_next(label);
            self.document_step(|_| true);
        }
        self.release_canvas();
        let canvas = crate::app::stroke_ops::exclusive(&mut self.canvas);
        if let Some(figure) = canvas.layers[i].figure.as_deref_mut() {
            edit(figure);
        }
        self.mark_unsaved();
    }

    /// The gesture ended: the next change is a new undo step.
    pub(crate) fn figure_edit_done(&mut self) {
        self.workspace.figure.in_gesture = false;
    }

    /// One change that is one undo step of its own, `label`.
    pub(crate) fn figure_step(&mut self, label: &str, edit: impl FnOnce(&mut FigureLayer)) {
        self.figure_edit_done();
        self.figure_edit(label, edit);
        self.figure_edit_done();
    }

    /// The figure the Figure tool has selected (its index in the scene).
    pub(crate) fn figure_selected_instance(&self) -> Option<usize> {
        let tool = &self.workspace.figure.tool;
        let f = tool.figure.or(tool.selected.map(|(f, _)| f))?;
        (f < self.active_figure()?.scene.len()).then_some(f)
    }

    /// The selected figure, or the only one.
    fn figure_target(&self) -> Option<usize> {
        let scene = &self.active_figure()?.scene;
        self.figure_selected_instance()
            .or((scene.len() == 1).then_some(0))
    }

    /// Edit figure `f` of the selected layer (and its model), as one step.
    fn figure_instance_step(
        &mut self,
        label: &str,
        f: usize,
        edit: impl FnOnce(&mut FigureInstance, &crate::canvas::figure::FigureModel),
    ) {
        let Some(model) = (self.active_figure())
            .and_then(|l| l.scene.get(f))
            .and_then(|i| i.model.resolve().ok())
        else {
            return;
        };
        self.figure_step(label, |layer| {
            if let Some(instance) = layer.scene.get_mut(f) {
                edit(instance, &model);
            }
        });
    }

    /// Another mannequin beside the figures there, selected.
    pub(crate) fn figure_add_mannequin(&mut self) {
        if self.active_figure().is_none() {
            self.add_figure_layer();
            return;
        }
        let mut figure = FigureInstance::new(ModelRef::mannequin());
        figure.pose = crate::canvas::figure::mannequin::standing_pose();
        self.figure_add_instance("Add figure", figure);
    }

    /// One of the realistic figures (the figure pack's model `id`) beside
    /// the figures there, selected: a body standing at ease, a hand
    /// open. Into a new figure layer if the selected layer isn't one (a
    /// hand alone has the camera brought close to it).
    pub(crate) fn figure_add_pack_model(&mut self, id: &str) {
        let model_ref = ModelRef::Bundled(id.to_string());
        let model = match model_ref.resolve() {
            Ok(model) => model,
            Err(err) => return self.report(err),
        };
        let mut figure = FigureInstance::new(model_ref);
        let body = model.skeleton.find(HumanJoint::Hips).is_some();
        if body {
            figure.pose = crate::canvas::figure::mannequin::standing_pose_of(&model.skeleton);
        }
        if self.active_figure().is_some() {
            return self.figure_add_instance("Add figure", figure);
        }
        if self.add_figure_layer().is_none() {
            return;
        }
        let (lo, hi) = model.bounds();
        self.figure_step("Add figure", |layer| {
            layer.scene = vec![figure];
            if !body {
                layer.camera.frame(lo, hi);
            }
        });
        let tool = &mut self.workspace.figure.tool;
        tool.figure = Some(0);
        tool.selected = None;
    }

    fn figure_add_instance(&mut self, label: &str, mut figure: FigureInstance) {
        let Some(layer) = self.active_figure() else {
            return;
        };
        // Beside the others, alternately right and left.
        let n = layer.scene.len();
        let step = (n as f32 / 2.0).ceil() * 0.8;
        figure.position[0] = if n % 2 == 1 { step } else { -step };
        self.figure_step(label, |layer| layer.scene.push(figure));
        let tool = &mut self.workspace.figure.tool;
        tool.figure = Some(n);
        tool.selected = None;
    }

    /// A glTF / GLB model brought in as a figure (if it has a human
    /// skeleton) or a prop: into the selected figure layer, else a new one.
    pub(crate) fn figure_import_model(&mut self, name: &str, data: Vec<u8>) {
        let stem = std::path::Path::new(name)
            .file_stem()
            .map_or_else(|| "Model".to_string(), |s| s.to_string_lossy().into_owned());
        let model = ModelRef::Embedded(Arc::new(EmbeddedModel::new(stem, data)));
        let loaded = match model.resolve() {
            Ok(loaded) => loaded,
            Err(err) => return self.report(format!("{name}: {err}")),
        };
        let mut figure = FigureInstance::new(model);
        // Models made in centimetres (or kilometres) come in at a human
        // size.
        let (lo, hi) = loaded.bounds();
        let size = (hi - lo).max_element();
        if size > 20.0 || (size > 0.0 && size < 0.02) {
            figure.scale = 1.75 / size;
        }
        if self.active_figure().is_none() {
            if self.add_figure_layer().is_none() {
                return;
            }
            self.figure_step("Import model", |layer| layer.scene = vec![figure]);
            self.workspace.figure.tool.figure = Some(0);
        } else {
            self.figure_add_instance("Import model", figure);
        }
        let kind = if loaded.skeleton.find(HumanJoint::Hips).is_some() {
            "figure"
        } else {
            "prop"
        };
        self.report(format!("{} added as a {kind}", loaded.name));
    }

    /// Read a model file on another thread, then bring it in (see
    /// [`Self::figure_import_model`]).
    pub(crate) fn import_figure_model_in_background(
        &mut self,
        name: String,
        source: crate::app::import::FileSource,
    ) {
        self.spawn_job(Some("Importing the model…"), move || {
            let bytes = source.read().map(|b| b.into_owned());
            Box::new(move |app: &mut PainterApp| match bytes {
                Ok(bytes) => app.figure_import_model(&name, bytes),
                Err(err) => app.report(err),
            })
        });
    }

    /// Delete the selected figure (or prop) from its layer. False if none
    /// is selected.
    pub(crate) fn figure_delete_selected(&mut self) -> bool {
        let Some(f) = self.figure_selected_instance() else {
            return false;
        };
        self.figure_step("Delete figure", |layer| layer.remove_figure(f));
        let tool = &mut self.workspace.figure.tool;
        tool.figure = None;
        tool.selected = None;
        tool.hover = None;
        true
    }

    /// Built-in body pose `i` on the selected figure (fingers stay), its
    /// lowest point then put on the ground.
    pub(crate) fn figure_apply_body_preset(&mut self, i: usize) {
        let (Some(preset), Some(f)) = (BODY_PRESETS.get(i), self.figure_target()) else {
            return;
        };
        let pose = preset.pose();
        self.figure_instance_step("Apply pose", f, |instance, model| {
            pose.apply(&model.skeleton, &mut instance.pose, |h| !is_finger(h), true);
            snap_to_ground(instance, model);
        });
    }

    /// Built-in hand pose `i` on the selected figure's chosen hand.
    pub(crate) fn figure_apply_hand_preset(&mut self, i: usize) {
        let Some(preset) = HAND_PRESETS.get(i) else {
            return;
        };
        self.figure_apply_hand_shape("Hand pose", &preset.shape, |_| true);
    }

    /// The hand pad's shape on the chosen hand, its unlocked fingers.
    pub(crate) fn figure_apply_pad(&mut self) {
        let pad = self.workspace.figure.pad;
        let shape = presets::pad_shape(pad.spread, pad.curl);
        let locked = pad.locked;
        self.figure_apply_hand_shape("Hand pad", &shape, move |finger| {
            let k = Finger::ALL.iter().position(|&f| f == finger).unwrap_or(0);
            !locked[k]
        });
    }

    fn figure_apply_hand_shape(
        &mut self,
        label: &str,
        shape: &presets::HandShape,
        fingers: impl Fn(Finger) -> bool,
    ) {
        let Some(f) = self.figure_target() else {
            return;
        };
        let side = self.workspace.figure.hand;
        let Some(model) = (self.active_figure())
            .and_then(|l| l.scene.get(f))
            .and_then(|i| i.model.resolve().ok())
        else {
            return;
        };
        let pose = presets::hand_pose(&model.skeleton, side, shape, fingers);
        let on_hand = move |h: HumanJoint| matches!(h, HumanJoint::Finger(s, ..) if s == side);
        // A pad drag joins one step; a click on a preset is its own.
        self.figure_edit(label, |layer| {
            if let Some(instance) = layer.scene.get_mut(f) {
                pose.apply(&model.skeleton, &mut instance.pose, on_hand, false);
            }
        });
    }

    /// Saved pose `i` on the selected figure (a hand pose on the chosen
    /// hand).
    pub(crate) fn figure_apply_saved(&mut self, i: usize) {
        let Some(saved) = self.workspace.figure.saved_poses().get(i).cloned() else {
            return;
        };
        let Some(f) = self.figure_target() else {
            return;
        };
        let side = self.workspace.figure.hand;
        self.figure_instance_step("Apply pose", f, |instance, model| match saved.kind {
            PoseKind::Body => {
                saved
                    .pose
                    .apply(&model.skeleton, &mut instance.pose, |_| true, true);
                snap_to_ground(instance, model);
            }
            PoseKind::Hand => {
                let pose = match side {
                    Side::Left => saved.pose.clone(),
                    Side::Right => saved.pose.mirrored(),
                };
                pose.apply(
                    &model.skeleton,
                    &mut instance.pose,
                    |h| matches!(h, HumanJoint::Finger(s, ..) if s == side),
                    false,
                );
            }
        });
    }

    /// Save the selected figure's pose (or its chosen hand's) as `name`.
    pub(crate) fn figure_save_pose(&mut self, name: &str, kind: PoseKind) {
        let Some(f) = self.figure_target() else {
            return;
        };
        let Some(instance) = self.active_figure().and_then(|l| l.scene.get(f)) else {
            return;
        };
        let Ok(model) = instance.model.resolve() else {
            return;
        };
        let turns = instance.turns(&model);
        let side = self.workspace.figure.hand;
        let pose = match kind {
            PoseKind::Body => presets::Pose::capture(&model.skeleton, &turns, |_| true),
            PoseKind::Hand => {
                let hand = presets::Pose::capture(
                    &model.skeleton,
                    &turns,
                    |h| matches!(h, HumanJoint::Finger(s, ..) if s == side),
                );
                // Kept as the left hand.
                if side == Side::Right {
                    hand.mirrored()
                } else {
                    hand
                }
            }
        };
        let name = name.trim();
        let name = if name.is_empty() { "My pose" } else { name }.to_string();
        let saved = SavedPose { name, kind, pose };
        if let Err(err) = self.workspace.figure.edit_saved(|list| list.push(saved)) {
            self.report(err);
        }
    }

    /// Forget saved pose `i`.
    pub(crate) fn figure_delete_saved(&mut self, i: usize) {
        let result = self.workspace.figure.edit_saved(|list| {
            if i < list.len() {
                list.remove(i);
            }
        });
        if let Err(err) = result {
            self.report(err);
        }
    }

    /// The selected figure's pose mirrored, left to right.
    pub(crate) fn figure_mirror_pose(&mut self) {
        let Some(f) = self.figure_target() else {
            return;
        };
        self.figure_instance_step("Mirror pose", f, |instance, model| {
            instance.pose = presets::mirror_figure(&model.skeleton, &instance.pose);
        });
    }

    /// Copy the selected figure's pose.
    pub(crate) fn figure_copy_pose(&mut self) {
        let Some(instance) =
            (self.figure_target()).and_then(|f| self.active_figure()?.scene.get(f))
        else {
            return;
        };
        let Ok(model) = instance.model.resolve() else {
            return;
        };
        let pose = presets::Pose::capture(&model.skeleton, &instance.turns(&model), |_| true);
        self.workspace.figure.clipboard = Some(pose);
    }

    /// The pose copied, on the selected figure (any model: by joint name).
    pub(crate) fn figure_paste_pose(&mut self) {
        let (Some(pose), Some(f)) = (
            self.workspace.figure.clipboard.clone(),
            self.figure_target(),
        ) else {
            return;
        };
        self.figure_instance_step("Paste pose", f, |instance, model| {
            pose.apply(&model.skeleton, &mut instance.pose, |_| true, true);
        });
    }

    /// The selected joint and everything below it back at rest.
    pub(crate) fn figure_reset_part(&mut self) {
        let Some((f, joint)) = self.workspace.figure.tool.selected else {
            return;
        };
        self.figure_instance_step("Reset part", f, |instance, model| {
            for j in model.skeleton.subtree(joint) {
                instance.set_joint_pose(j, glam::Quat::IDENTITY);
            }
        });
    }

    /// The selected figure's lowest point on the ground.
    pub(crate) fn figure_snap_to_ground(&mut self) {
        let Some(f) = self.figure_target() else {
            return;
        };
        self.figure_instance_step("Snap to ground", f, snap_to_ground);
    }

    /// Pin joint `joint` of figure `f`, or unpin it.
    pub(crate) fn figure_toggle_pin(&mut self, f: usize, joint: usize) {
        self.figure_instance_step("Pin joint", f, |instance, _| instance.toggle_pin(joint));
    }

    /// Turn the selected joint's limits off, or back on (it goes back
    /// inside them).
    pub(crate) fn figure_toggle_limits(&mut self) {
        let Some((f, joint)) = self.workspace.figure.tool.selected else {
            return;
        };
        self.figure_instance_step("Joint limits", f, |instance, model| {
            instance.toggle_limits(joint);
            if !instance.is_unlimited(joint)
                && let Some(j) = model.skeleton.joints.get(joint)
            {
                let turn = j.limit.clamp(instance.joint_pose(joint));
                instance.set_joint_pose(joint, turn);
            }
        });
    }

    /// Change the selected figure's proportions (a slider's drag is one
    /// step); a figure standing on the ground stays on it.
    pub(crate) fn figure_edit_shape(&mut self, f: usize, edit: impl FnOnce(&mut FigureInstance)) {
        let Some(model) = (self.active_figure())
            .and_then(|l| l.scene.get(f))
            .and_then(|i| i.model.resolve().ok())
        else {
            return;
        };
        self.figure_edit("Body shape", |layer| {
            let Some(instance) = layer.scene.get_mut(f) else {
                return;
            };
            let grounded = instance
                .lowest_point(&model)
                .is_some_and(|y| y.abs() < 0.03);
            edit(instance);
            if grounded {
                snap_to_ground(instance, &model);
            }
        });
    }

    /// One change to figure layer `i` that is one undo step, `label`.
    fn figure_step_at(&mut self, i: usize, label: &str, edit: impl FnOnce(&mut FigureLayer)) {
        self.figure_edit_done();
        self.figure_edit_at(i, label, edit);
        self.figure_edit_done();
    }

    /// Key pose: the selected figure layer's figures as they are, kept at
    /// the frame showing (one undo step). Between keys they ease from one
    /// to the next.
    pub(crate) fn figure_key_pose(&mut self) {
        let t = self.canvas.time;
        self.figure_step("Key pose", |f| f.key_pose(t));
    }

    /// Whether the selected figure layer has a pose key at the frame
    /// showing.
    pub(crate) fn figure_keyed_here(&self) -> bool {
        (self.active_figure()).is_some_and(|f| f.key_at(self.canvas.time).is_some())
    }

    /// Take away the selected figure layer's pose key at the frame showing.
    pub(crate) fn figure_remove_key(&mut self) {
        let (i, t) = (self.canvas.active_layer_idx, self.canvas.time);
        self.figure_remove_key_at(i, t);
    }

    /// Whether layer `i` is a figure layer with a pose key at `frame`.
    pub(crate) fn figure_key_at(&self, i: usize, frame: u32) -> Option<Curve> {
        let figure = self.canvas.layers.get(i)?.figure.as_deref()?;
        figure.key_at(frame).map(|k| k.curve)
    }

    /// The frames layer `i`'s pose keys are on (none for other layers).
    pub(crate) fn figure_key_frames(&self, i: usize) -> Vec<u32> {
        (self.canvas.layers.get(i))
            .and_then(|l| l.figure.as_deref())
            .map_or_else(Vec::new, FigureLayer::key_frames)
    }

    /// Take away figure layer `i`'s pose key at `frame` (one undo step).
    pub(crate) fn figure_remove_key_at(&mut self, i: usize, frame: u32) {
        if self.figure_key_at(i, frame).is_some() {
            self.figure_step_at(i, "Remove pose key", |f| {
                f.remove_key(frame);
            });
        }
    }

    /// Move figure layer `i`'s pose key from frame `from` to `to` (not
    /// onto another; one undo step).
    pub(crate) fn figure_move_key(&mut self, i: usize, from: u32, to: u32) {
        if self.figure_key_at(i, from).is_some() && self.figure_key_at(i, to).is_none() {
            self.figure_step_at(i, "Move pose key", |f| {
                f.move_key(from, to);
            });
        }
    }

    /// How figure layer `i`'s pose key at `frame` eases to the next.
    pub(crate) fn figure_set_key_curve(&mut self, i: usize, frame: u32, curve: Curve) {
        if self.figure_key_at(i, frame).is_some_and(|c| c != curve) {
            self.figure_step_at(i, "Pose key easing", |f| {
                f.set_key_curve(frame, curve);
            });
        }
    }

    /// The figure layers with pose keys pose their figures as the keys
    /// have them at the frame showing (after going to another frame). Not
    /// an undo step: the keys say what the frame shows.
    pub(crate) fn figures_follow_time(&mut self) {
        let keyed: Vec<usize> = (0..self.canvas.layers.len())
            .filter(|&i| {
                (self.canvas.layers[i].figure.as_deref()).is_some_and(|f| !f.keys.is_empty())
            })
            .collect();
        if keyed.is_empty() {
            return;
        }
        let t = self.canvas.time as f32;
        self.release_canvas();
        let canvas = crate::app::stroke_ops::exclusive(&mut self.canvas);
        for i in keyed {
            if let Some(figure) = canvas.layers[i].figure.as_deref_mut() {
                figure.follow_time(t);
            }
        }
    }

    /// Something to bake the keyed figure layers at each frame on another
    /// thread (an animation export, frames played from memory); None
    /// without keyed figures (or a GPU).
    pub(crate) fn figure_animator(&self) -> Option<FigureAnimator> {
        let keyed = (self.canvas.layers.iter())
            .any(|l| l.figure.as_deref().is_some_and(|f| !f.keys.is_empty()));
        let gpu = self.workspace.shaders.gpu.as_ref().filter(|_| keyed)?;
        Some(FigureAnimator {
            device: gpu.device.clone(),
            queue: gpu.queue.clone(),
            gpu: None,
        })
    }

    /// The perspective ruler the figure camera can be matched to: the
    /// last added shown one-, two- or three-point assistant's points.
    pub(crate) fn figure_vanishing(&self) -> Option<Vanishing> {
        use crate::app::tools::assistants::AssistantKind;
        (self.workspace.guides.assistants.iter().rev())
            .filter(|a| a.enabled)
            .find_map(|a| {
                let [p, q, r, _] = a.points;
                match a.kind {
                    AssistantKind::VanishingPoint => Some(Vanishing::One(p)),
                    AssistantKind::TwoPoint => Some(Vanishing::Two(p, q, r)),
                    AssistantKind::ThreePoint => Some(Vanishing::Three(p, q, r)),
                    _ => None,
                }
            })
    }

    /// Turn the selected figure layer's camera and set its lens to the
    /// perspective ruler's vanishing points (one undo step).
    pub(crate) fn figure_match_perspective(&mut self) {
        let (Some(vanishing), Some(layer)) = (self.figure_vanishing(), self.active_figure()) else {
            return;
        };
        let mut camera = layer.camera;
        let (w, h) = (self.canvas.width(), self.canvas.height());
        if !camera.match_perspective(vanishing, w, h) {
            return self.report(
                "The perspective ruler's vanishing points don't fit a camera: put them further apart, either side of the canvas's middle".into(),
            );
        }
        self.figure_step("Match perspective", |f| f.camera = camera);
    }

    /// Make figure layer `idx` a plain pixel layer, its figures as they
    /// show now (Layer → Rasterise 3D Figure; one undo step). It's
    /// unlocked for painting.
    pub(crate) fn rasterise_figure_layer(&mut self, idx: usize) {
        if self
            .canvas
            .layers
            .get(idx)
            .is_none_or(|l| l.figure.is_none())
        {
            return;
        }
        if self.stale_figures().contains(&idx)
            && let Err(err) = self.bake_figure_layer(idx)
        {
            return self.report(format!("Couldn't render the figures: {err}"));
        }
        self.figure_edit_done();
        self.layer_state.history.label_next("Rasterise 3D figure");
        self.document_step(|canvas| {
            let layer = &mut canvas.layers[idx];
            layer.figure = None;
            layer.locked = false;
            true
        });
        self.workspace.figure.tool = Default::default();
        self.layer_state.thumbnails_dirty = true;
        self.mark_all_tiles_dirty();
        self.mark_unsaved();
    }

    /// What layer `idx`'s bake depends on: its scene and the canvas size.
    fn figure_bake_key(&self, idx: usize) -> Option<u64> {
        let figure = self.canvas.layers.get(idx)?.figure.as_deref()?;
        let (w, h) = (self.canvas.width() as u64, self.canvas.height() as u64);
        Some(figure.content_hash() ^ (w << 32 | h).rotate_left(17))
    }

    /// Render figure layer `idx` into its tiles (no undo: the tiles only
    /// mirror the scene).
    pub(crate) fn bake_figure_layer(&mut self, idx: usize) -> Result<(), String> {
        let layer = self.canvas.layers.get(idx).ok_or("no such layer")?;
        let id = layer.id;
        let figure = layer.figure.as_deref().ok_or("not a figure layer")?;
        let key = self.figure_bake_key(idx).ok_or("not a figure layer")?;
        let (w, h) = (self.canvas.width(), self.canvas.height());
        let frame = figure.render_frame(w, h);
        // Baked or not, this scene isn't tried again until it changes.
        self.workspace.figure.baked.insert(id, key);
        let gpu = self.workspace.shaders.gpu.clone().ok_or("no GPU")?;
        let pixels = {
            let mut renderer = gpu.renderer.write();
            let canvas_gpu = renderer
                .callback_resources
                .get_mut::<crate::app::view::gpu_canvas::GpuCanvas>()
                .ok_or("no GPU canvas")?;
            canvas_gpu
                .shaders_mut()
                .figures_mut()
                .bake(&gpu.device, &gpu.queue, &frame)?
        };
        self.write_layer_pixels(idx, &pixels);
        Ok(())
    }

    /// Bake every figure layer whose scene changed since it was last baked
    /// (before export, saving and merging).
    pub(crate) fn bake_figure_layers(&mut self) {
        for idx in self.stale_figures() {
            if let Err(err) = self.bake_figure_layer(idx) {
                log::warn!("baking a figure layer: {err}");
            }
        }
    }

    /// Figure layers whose pixels are older than their scene.
    fn stale_figures(&self) -> Vec<usize> {
        let baked = &self.workspace.figure.baked;
        (0..self.canvas.layers.len())
            .filter(|&i| {
                let l = &self.canvas.layers[i];
                l.figure.is_some() && baked.get(&l.id).copied() != self.figure_bake_key(i)
            })
            .collect()
    }

    /// Once a frame (from `shader_tick`, which knows whether live layers
    /// show live): bake the figures whose pixels are out of date — at
    /// once (throttled while dragging) when they show as their pixels,
    /// once their scene has settled when they show live.
    pub(crate) fn figure_tick(&mut self, live: bool) {
        let ids: Vec<LayerId> = (self.canvas.layers.iter())
            .filter(|l| l.figure.is_some())
            .map(|l| l.id)
            .collect();
        let state = &mut self.workspace.figure;
        state.baked.retain(|id, _| ids.contains(id));
        state.changed.retain(|id, _| ids.contains(id));
        if ids.is_empty() || self.brush_state.is_drawing {
            return;
        }
        let stale = self.stale_figures();
        if stale.is_empty() {
            return;
        }
        let now = Instant::now();
        let mut due = Vec::new();
        for idx in stale {
            let id = self.canvas.layers[idx].id;
            let key = self.figure_bake_key(idx).unwrap_or(0);
            let state = &mut self.workspace.figure;
            let since = match state.changed.get(&id) {
                Some(&(k, t)) if k == key => t,
                _ => {
                    state.changed.insert(id, (key, now));
                    now
                }
            };
            let ready = if live {
                !state.in_gesture && now.duration_since(since) >= SETTLED_EDIT
            } else {
                !state.in_gesture
                    || state
                        .last_fallback_bake
                        .is_none_or(|t| now.duration_since(t) >= FALLBACK_BAKE_EVERY)
            };
            if ready {
                due.push(idx);
            }
        }
        if due.is_empty() {
            return;
        }
        self.workspace.figure.last_fallback_bake = Some(now);
        for idx in due {
            if let Err(err) = self.bake_figure_layer(idx) {
                log::warn!("baking a figure layer: {err}");
            }
        }
        if !live {
            self.mark_all_tiles_dirty();
        }
    }

    /// This frame's scene of figure layer `id`, for the live composite.
    pub(crate) fn figure_render_frame(
        &self,
        id: LayerId,
    ) -> Option<crate::canvas::figure::RenderFrame> {
        let idx = self.canvas.layer_index_of(id)?;
        let figure = self.canvas.layers[idx].figure.as_deref()?;
        Some(figure.render_frame(self.canvas.width(), self.canvas.height()))
    }
}

/// Bakes the figure layers with pose keys at each frame, on the thread it
/// was moved to (see [`PainterApp::figure_animator`]).
pub(crate) struct FigureAnimator {
    device: Arc<wgpu::Device>,
    queue: Arc<wgpu::Queue>,
    /// Its own renderer, made on first use.
    gpu: Option<crate::app::view::figure_gpu::FigureGpu>,
}

impl FigureAnimator {
    /// Bake `canvas`'s keyed figure layers as they are at its frame.
    pub(crate) fn bake(&mut self, canvas: &crate::canvas::Canvas) {
        let device = &self.device;
        let gpu =
            (self.gpu).get_or_insert_with(|| crate::app::view::figure_gpu::FigureGpu::new(device));
        bake_keyed_figures(gpu, device, &self.queue, canvas);
    }
}

/// Bake the figure layers of `canvas` that have pose keys, posed as at its
/// frame, into their tiles (new tiles: a shared copy's are the document's).
/// Returns how many were baked.
pub(crate) fn bake_keyed_figures(
    gpu: &mut crate::app::view::figure_gpu::FigureGpu,
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    canvas: &crate::canvas::Canvas,
) -> usize {
    let (w, h) = (canvas.width(), canvas.height());
    let mut baked = 0;
    for (idx, layer) in canvas.layers.iter().enumerate() {
        let Some(figure) = layer.figure.as_deref().filter(|f| !f.keys.is_empty()) else {
            continue;
        };
        let mut posed = figure.clone();
        posed.follow_time(canvas.time as f32);
        match gpu.bake(device, queue, &posed.render_frame(w, h)) {
            Ok(pixels) => {
                crate::app::shader_ops::write_canvas_pixels(canvas, idx, &pixels, true);
                baked += 1;
            }
            Err(err) => log::warn!("baking a figure layer at frame {}: {err}", canvas.time),
        }
    }
    baked
}

/// Move `instance` up or down so its lowest point is on the ground.
fn snap_to_ground(instance: &mut FigureInstance, model: &crate::canvas::figure::FigureModel) {
    if let Some(y) = instance.lowest_point(model) {
        instance.position[1] -= y;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::tools::Tool;
    use crate::canvas::Canvas;
    use eframe::egui::Color32;
    use glam::Quat;

    #[test]
    fn a_new_figure_layer_is_locked_and_picks_the_figure_tool() {
        let canvas = Canvas::new(64, 64, Color32::WHITE, 64);
        let mut app = crate::project::tests::test_app_pub(canvas);
        let idx = app.add_figure_layer().unwrap();
        let layer = &app.canvas.layers[idx];
        assert!(layer.locked && layer.figure.is_some());
        assert_eq!(app.canvas.active_layer_idx, idx);
        assert!(matches!(app.active_tool, Tool::Figure));
    }

    #[test]
    fn an_edit_gesture_is_one_undo_step() {
        let canvas = Canvas::new(64, 64, Color32::WHITE, 64);
        let mut app = crate::project::tests::test_app_pub(canvas);
        let idx = app.add_figure_layer().unwrap();
        let pose = |app: &crate::PainterApp| {
            app.canvas.layers[idx].figure.as_ref().unwrap().scene[0].joint_pose(3)
        };
        let before = pose(&app);
        for k in 1..=5 {
            app.figure_edit("Pose", |f| {
                f.scene[0].set_joint_pose(3, Quat::from_rotation_x(0.1 * k as f32))
            });
        }
        app.figure_edit_done();
        assert!(pose(&app).abs_diff_eq(Quat::from_rotation_x(0.5), 1e-5));
        app.apply_history(false);
        assert!(
            pose(&app).abs_diff_eq(before, 1e-6),
            "one undo undoes the drag"
        );
        app.apply_history(true);
        assert!(pose(&app).abs_diff_eq(Quat::from_rotation_x(0.5), 1e-5));
    }

    use crate::canvas::figure::skeleton::{HumanJoint, Side, TurnAngle};

    fn app_with_figure() -> crate::PainterApp {
        let canvas = Canvas::new(64, 64, Color32::WHITE, 64);
        let mut app = crate::project::tests::test_app_pub(canvas);
        app.add_figure_layer().unwrap();
        app
    }

    fn last_label(app: &crate::PainterApp) -> String {
        app.layer_state
            .history
            .labels()
            .0
            .last()
            .cloned()
            .unwrap_or_default()
    }

    fn joint(h: HumanJoint) -> usize {
        crate::canvas::figure::mannequin::mannequin()
            .skeleton
            .find(h)
            .unwrap()
    }

    fn turn(app: &crate::PainterApp, f: usize, h: HumanJoint) -> Quat {
        app.active_figure().unwrap().scene[f].joint_pose(joint(h))
    }

    #[test]
    fn presets_mirror_copy_and_paste_are_undo_steps_with_their_names() {
        let mut app = app_with_figure();
        let walking = BODY_PRESETS
            .iter()
            .position(|p| p.name == "Walking")
            .unwrap();
        app.figure_apply_body_preset(walking);
        assert_eq!(last_label(&app), "Apply pose");
        let left = turn(&app, 0, HumanJoint::UpperLeg(Side::Left));
        let right = turn(&app, 0, HumanJoint::UpperLeg(Side::Right));
        assert!(left.turn_angle(right) > 0.3, "the legs differ");
        // On the ground still.
        let figure = &app.active_figure().unwrap().scene[0];
        let model = figure.model.resolve().unwrap();
        assert!(figure.lowest_point(&model).unwrap().abs() < 1e-4);
        app.figure_mirror_pose();
        assert_eq!(last_label(&app), "Mirror pose");
        let swapped = turn(&app, 0, HumanJoint::UpperLeg(Side::Right));
        assert!(swapped.turn_angle(presets::reflect(left)) < 1e-4);
        // Copied onto a second mannequin.
        app.figure_copy_pose();
        app.figure_add_mannequin();
        assert_eq!(last_label(&app), "Add figure");
        assert_eq!(app.figure_selected_instance(), Some(1));
        app.figure_paste_pose();
        assert_eq!(last_label(&app), "Paste pose");
        assert!(turn(&app, 1, HumanJoint::UpperLeg(Side::Right)).turn_angle(swapped) < 1e-4);
        // A hand pose on the right hand only.
        app.workspace.figure.hand = Side::Right;
        let fist = HAND_PRESETS.iter().position(|p| p.name == "Fist").unwrap();
        app.figure_apply_hand_preset(fist);
        app.figure_edit_done();
        let index = |side| HumanJoint::Finger(side, Finger::Index, 1);
        assert!(turn(&app, 1, index(Side::Right)).turn_angle(Quat::IDENTITY) > 1.0);
        assert!(turn(&app, 1, index(Side::Left)).turn_angle(Quat::IDENTITY) < 1e-6);
        // The pad, the index locked: the others curl, the index stays.
        app.workspace.figure.pad.locked[1] = true;
        app.workspace.figure.pad.curl = 0.0;
        let before = turn(&app, 1, index(Side::Right));
        app.figure_apply_pad();
        app.figure_edit_done();
        assert_eq!(last_label(&app), "Hand pad");
        assert!(turn(&app, 1, index(Side::Right)).turn_angle(before) < 1e-6);
        let middle = HumanJoint::Finger(Side::Right, Finger::Middle, 1);
        assert!(
            turn(&app, 1, middle).turn_angle(Quat::IDENTITY) < 1e-6,
            "opened"
        );
        // Delete the second figure; undo brings it back.
        assert!(app.figure_delete_selected());
        assert_eq!(app.active_figure().unwrap().scene.len(), 1);
        assert_eq!(last_label(&app), "Delete figure");
        app.apply_history(false);
        assert_eq!(app.active_figure().unwrap().scene.len(), 2);
    }

    #[test]
    fn saved_poses_persist_and_apply_by_name() {
        let dir = std::env::temp_dir().join(format!("rp-figure-poses-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("figure_poses.json");
        let _ = std::fs::remove_file(&path);
        let mut app = app_with_figure();
        app.workspace.figure.poses_path = Some(path.clone());
        let sitting = BODY_PRESETS
            .iter()
            .position(|p| p.name == "Sitting")
            .unwrap();
        app.figure_apply_body_preset(sitting);
        app.figure_save_pose("Seated", PoseKind::Body);
        app.workspace.figure.hand = Side::Left;
        let point = HAND_PRESETS.iter().position(|p| p.name == "Point").unwrap();
        app.figure_apply_hand_preset(point);
        app.figure_edit_done();
        app.figure_save_pose("Pointing", PoseKind::Hand);
        // Read back by another app.
        let mut other = app_with_figure();
        other.workspace.figure.poses_path = Some(path.clone());
        let names: Vec<String> = (other.workspace.figure.saved_poses().iter())
            .map(|p| p.name.clone())
            .collect();
        assert_eq!(names, ["Seated", "Pointing"]);
        other.figure_apply_saved(0);
        let knee = HumanJoint::LowerLeg(Side::Left);
        assert!(turn(&other, 0, knee).turn_angle(turn(&app, 0, knee)) < 1e-4);
        // The left hand's pose, saved, goes on the right hand mirrored.
        other.workspace.figure.hand = Side::Right;
        other.figure_apply_saved(1);
        let tip = |side| HumanJoint::Finger(side, Finger::Middle, 1);
        let mirrored = presets::reflect(turn(&app, 0, tip(Side::Left)));
        assert!(turn(&other, 0, tip(Side::Right)).turn_angle(mirrored) < 1e-4);
        other.figure_delete_saved(0);
        assert_eq!(other.workspace.figure.saved_poses().len(), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn importing_a_model_adds_it_to_the_scene() {
        let mut app = app_with_figure();
        let bytes = std::fs::read(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/testdata/figure-tiny.gltf"
        ))
        .unwrap();
        app.figure_import_model("tiny.gltf", bytes.clone());
        let scene = &app.active_figure().unwrap().scene;
        assert_eq!(scene.len(), 2);
        assert_eq!(scene[1].model.name(), "tiny");
        assert_eq!(last_label(&app), "Import model");
        // A preset poses it by name too.
        let a_pose = BODY_PRESETS
            .iter()
            .position(|p| p.name == "A-pose")
            .unwrap();
        app.figure_apply_body_preset(a_pose);
        let figure = &app.active_figure().unwrap().scene[1];
        let model = figure.model.resolve().unwrap();
        let j = model
            .skeleton
            .find(HumanJoint::UpperArm(Side::Left))
            .unwrap();
        let want = BODY_PRESETS[a_pose]
            .pose()
            .get(HumanJoint::UpperArm(Side::Left))
            .unwrap();
        assert!(figure.joint_pose(j).turn_angle(want) < 1e-4);
        // Its morph target drives its shape.
        app.figure_edit_shape(1, |i| i.morphs = vec![1.0]);
        app.figure_edit_done();
        assert_eq!(last_label(&app), "Body shape");
        // Not a model: refused, nothing added.
        app.figure_import_model("junk.glb", b"nope".to_vec());
        assert_eq!(app.active_figure().unwrap().scene.len(), 2);
    }

    #[test]
    fn longer_legs_keep_the_figure_on_the_ground() {
        let mut app = app_with_figure();
        let figure = app.active_figure().unwrap().scene[0].clone();
        let model = figure.model.resolve().unwrap();
        let world = figure.world(&model);
        let head = world[joint(HumanJoint::Head)].w_axis.y;
        app.figure_edit_shape(0, |i| i.shape.legs = 1.3);
        app.figure_edit_done();
        let after = app.active_figure().unwrap().scene[0].clone();
        assert!(after.lowest_point(&model).unwrap().abs() < 1e-4);
        let taller = after.world(&model)[joint(HumanJoint::Head)].w_axis.y;
        assert!(taller > head + 0.15, "{head} → {taller}");
        // Limits off for a joint: it turns past where it stopped.
        app.workspace.figure.tool.selected = Some((0, joint(HumanJoint::LowerArm(Side::Left))));
        app.figure_toggle_limits();
        assert_eq!(last_label(&app), "Joint limits");
        assert!(
            app.active_figure().unwrap().scene[0]
                .is_unlimited(joint(HumanJoint::LowerArm(Side::Left)))
        );
        app.figure_snap_to_ground();
        assert_eq!(last_label(&app), "Snap to ground");
    }

    #[test]
    fn realistic_figures_are_added_standing_and_hands_alone_close_up() {
        use crate::canvas::figure::pack;
        let canvas = Canvas::new(64, 64, Color32::WHITE, 64);
        let mut app = crate::project::tests::test_app_pub(canvas);
        // One the pack doesn't have: nothing is added.
        app.figure_add_pack_model("giant");
        assert!(app.active_figure().is_none());
        if pack::installed().is_none() {
            eprintln!("the figure pack's models aren't pulled from Git LFS; skipping");
            return;
        }
        // A hand alone: a new layer, the camera near it.
        app.figure_add_pack_model("hand-left");
        let layer = app.active_figure().unwrap();
        assert_eq!(layer.scene.len(), 1);
        assert_eq!(layer.scene[0].model.name(), "Left hand");
        assert!(layer.camera.distance < 1.0, "{}", layer.camera.distance);
        assert_eq!(app.workspace.figure.tool.figure, Some(0));
        // A body beside it, standing: its arms down, not in T-pose.
        app.figure_add_pack_model("male");
        let layer = app.active_figure().unwrap();
        assert_eq!(layer.scene.len(), 2);
        let male = &layer.scene[1];
        let model = male.model.resolve().unwrap();
        let world = male.world(&model);
        let at = |h| world[model.skeleton.find(h).unwrap()].w_axis.y;
        assert!(at(HumanJoint::Hand(Side::Left)) < at(HumanJoint::UpperArm(Side::Left)) - 0.3);
        assert_eq!(last_label(&app), "Add figure");
    }

    #[test]
    fn pose_keys_are_undo_steps_and_the_figures_follow_the_frame() {
        let mut app = app_with_figure();
        let idx = app.canvas.active_layer_idx;
        let elbow = HumanJoint::LowerArm(Side::Left);
        let bent = |a: f32| Quat::from_rotation_x(a);
        app.figure_step("Pose", |f| {
            f.scene[0].set_joint_pose(joint(elbow), bent(0.0))
        });
        app.figure_key_pose();
        assert_eq!(last_label(&app), "Key pose");
        app.go_to_frame(10);
        app.figure_step("Pose", |f| {
            f.scene[0].set_joint_pose(joint(elbow), bent(1.0))
        });
        assert!(!app.figure_keyed_here());
        app.figure_key_pose();
        assert!(app.figure_keyed_here());
        assert_eq!(app.figure_key_frames(idx), vec![0, 10]);
        // Between the keys: eased halfway (linear).
        app.go_to_frame(5);
        assert!(turn(&app, 0, elbow).angle_between(bent(0.5)) < 1e-4);
        // One undo takes the key away: the first key's pose holds.
        app.apply_history(false);
        assert_eq!(app.figure_key_frames(idx), vec![0]);
        app.go_to_frame(6);
        assert!(turn(&app, 0, elbow).angle_between(bent(0.0)) < 1e-4);
        app.apply_history(true);
        assert_eq!(app.figure_key_frames(idx), vec![0, 10]);
        // Removed at the frame showing, as one step.
        app.go_to_frame(10);
        app.figure_remove_key();
        assert_eq!(last_label(&app), "Remove pose key");
        assert_eq!(app.figure_key_frames(idx), vec![0]);
        app.apply_history(false);
        assert_eq!(app.figure_key_frames(idx), vec![0, 10]);
        // Moved and eased from the timeline.
        app.figure_move_key(idx, 10, 20);
        assert_eq!(last_label(&app), "Move pose key");
        assert_eq!(app.figure_key_frames(idx), vec![0, 20]);
        app.figure_set_key_curve(idx, 0, Curve::Stepped);
        assert_eq!(app.figure_key_at(idx, 0), Some(Curve::Stepped));
        app.go_to_frame(19);
        assert!(turn(&app, 0, elbow).angle_between(bent(0.0)) < 1e-4, "held");
        app.go_to_frame(20);
        assert!(turn(&app, 0, elbow).angle_between(bent(1.0)) < 1e-4);
    }

    #[test]
    fn rasterising_a_figure_layer_keeps_its_pixels_as_one_undo_step() {
        let mut app = app_with_figure();
        let idx = app.canvas.active_layer_idx;
        // Its render (as a bake leaves it).
        app.canvas
            .set_layer_tile_data(idx, 0, 0, vec![Color32::RED; 64 * 64]);
        app.rasterise_figure_layer(idx);
        assert_eq!(last_label(&app), "Rasterise 3D figure");
        let layer = &app.canvas.layers[idx];
        assert!(layer.figure.is_none() && !layer.locked);
        assert_eq!(
            app.canvas.get_layer_tile_data(idx, 0, 0).unwrap()[0],
            Color32::RED
        );
        app.apply_history(false);
        let layer = &app.canvas.layers[idx];
        assert!(layer.figure.is_some() && layer.locked);
        app.apply_history(true);
        assert!(app.canvas.layers[idx].figure.is_none());
        // Not a figure layer: nothing happens.
        let steps = app.layer_state.history.labels().0.len();
        app.rasterise_figure_layer(idx);
        assert_eq!(app.layer_state.history.labels().0.len(), steps);
    }

    #[test]
    fn the_camera_matches_the_perspective_ruler_as_one_undo_step() {
        use crate::app::tools::assistants::{Assistant, AssistantKind};
        use crate::canvas::figure::FigureCamera;
        let mut app = app_with_figure();
        assert!(app.figure_vanishing().is_none());
        let (w, h) = (64.0, 64.0);
        app.workspace
            .guides
            .assistants
            .push(Assistant::new(AssistantKind::Ellipse, w, h));
        assert!(app.figure_vanishing().is_none(), "not a perspective ruler");
        // A ruler drawn for a known camera.
        let want = FigureCamera {
            yaw: 0.5,
            pitch: 0.2,
            fov: 40.0,
            ..Default::default()
        };
        let view_proj = want.projection(1.0) * want.view();
        let vp = |d: glam::Vec3| {
            let c = view_proj * d.extend(0.0);
            [(c.x / c.w + 1.0) * 0.5 * w, (1.0 - c.y / c.w) * 0.5 * h]
        };
        let mut ruler = Assistant::new(AssistantKind::ThreePoint, w, h);
        ruler.points = [
            vp(glam::Vec3::X),
            vp(glam::Vec3::Z),
            vp(glam::Vec3::Y),
            [0.0; 2],
        ];
        app.workspace.guides.assistants.push(ruler);
        assert!(app.figure_vanishing().is_some());
        app.figure_match_perspective();
        assert_eq!(last_label(&app), "Match perspective");
        let camera = app.active_figure().unwrap().camera;
        assert!((camera.yaw - want.yaw).abs() < 1e-3, "{camera:?}");
        assert!((camera.pitch - want.pitch).abs() < 1e-3, "{camera:?}");
        assert!((camera.fov - want.fov).abs() < 0.05, "{camera:?}");
        app.apply_history(false);
        assert_eq!(app.active_figure().unwrap().camera, FigureCamera::default());
        // A hidden ruler doesn't count.
        app.workspace.guides.assistants[1].enabled = false;
        assert!(app.figure_vanishing().is_none());
    }

    #[test]
    fn keyed_figures_bake_as_each_frame_poses_them() {
        use crate::app::view::figure_gpu::FigureGpu;
        let Some(lock) = crate::app::view::test_gpu() else {
            eprintln!("no GPU adapter available; skipping");
            return;
        };
        let (device, queue) = &*lock;
        let ts = 64;
        let mut canvas = Canvas::new(2 * ts, 2 * ts, Color32::WHITE, ts);
        let mut figure = FigureLayer::mannequin();
        figure.key_pose(0);
        figure.scene[0].position = [0.6, 0.0, 0.0];
        figure.key_pose(10);
        canvas.layers[1].figure = Some(Box::new(figure));
        // The middle of what's drawn, across.
        let middle = |canvas: &Canvas| {
            let (mut sum, mut n) = (0.0, 0.0);
            for (tx, ty) in [(0, 0), (1, 0), (0, 1), (1, 1)] {
                let Some(data) = canvas.get_layer_tile_data(1, tx, ty) else {
                    continue;
                };
                for (i, p) in data.iter().enumerate() {
                    if p.a() > 0 {
                        sum += (tx as usize * ts + i % ts) as f32;
                        n += 1.0;
                    }
                }
            }
            assert!(n > 0.0, "nothing drawn");
            sum / n
        };
        let mut gpu = FigureGpu::new(device);
        assert_eq!(bake_keyed_figures(&mut gpu, device, queue, &canvas), 1);
        let at_start = middle(&canvas);
        assert!((at_start - ts as f32).abs() < 6.0, "{at_start}");
        // At frame 10, on a copy sharing the tiles: moved right there, the
        // document's pixels as they were.
        let mut later = canvas.shared_copy();
        later.set_time(10);
        bake_keyed_figures(&mut gpu, device, queue, &later);
        assert!(middle(&later) > at_start + 10.0, "{}", middle(&later));
        assert_eq!(middle(&canvas), at_start);
        // Without keys, nothing is baked.
        later.layers[1].figure.as_mut().unwrap().keys.clear();
        assert_eq!(bake_keyed_figures(&mut gpu, device, queue, &later), 0);
    }
}
