//! Rig layers and imported animations in the app: Spine, DragonBones and
//! Lottie animations come in as rig layers (fitted to the canvas, the
//! timeline set to play them); videos, GIFs and animated PNGs (what Moho or
//! Alight Motion export, say) come in as animated layers, a drawing a frame.
//! Rig edits are undo steps, one a gesture.

use crate::PainterApp;
use crate::canvas::rig::{Rig, RigAnimation};
use crate::project::anim_import::ImportedRig;
use std::path::{Path, PathBuf};

/// Most frames a video brings in.
const MAX_FRAMES: usize = 600;

/// Where an edit of the selected rig stands: a gesture already has its undo
/// step.
#[derive(Default)]
pub struct RigEditState {
    pub in_gesture: bool,
}

impl PainterApp {
    /// Read the Spine, DragonBones or Lottie animation at `path` (its atlas
    /// and pictures beside it) and add it as a rig layer.
    pub(crate) fn import_animation_in_background(&mut self, path: PathBuf) {
        self.spawn_job(Some("Opening the animation…"), move || {
            let result = std::fs::read(&path)
                .map_err(|e| format!("Couldn't read {}: {e}", path.display()))
                .and_then(|json| {
                    let stem = path
                        .file_stem()
                        .map_or_else(String::new, |s| s.to_string_lossy().into_owned());
                    let dir = path.parent().unwrap_or(Path::new("."));
                    crate::project::anim_import::import(
                        &json,
                        &stem,
                        &crate::project::anim_import::Folder(dir),
                    )
                });
            let name = path
                .file_stem()
                .map_or_else(|| "Animation".into(), |s| s.to_string_lossy().into_owned());
            Box::new(move |app: &mut PainterApp| match result {
                Ok(rig) => app.add_rig_layer(name, rig),
                Err(err) => app.report(err),
            })
        });
    }

    /// `imported` as a new rig layer: placed in the middle of the canvas
    /// (shrunk to fit), the timeline set to play it if nothing else is
    /// animated.
    pub(crate) fn add_rig_layer(&mut self, name: String, imported: ImportedRig) {
        let mut rig = imported.rig;
        let (cw, ch) = (self.canvas.width() as f32, self.canvas.height() as f32);
        let [bx, by, bw, bh] =
            imported
                .bounds
                .unwrap_or([-cw / 4.0, -ch / 4.0, cw / 2.0, ch / 2.0]);
        let scale = (cw * 0.9 / bw.max(1.0))
            .min(ch * 0.9 / bh.max(1.0))
            .min(1.0);
        rig.scale = scale;
        let mid = [bx + bw / 2.0, by + bh / 2.0];
        rig.origin = if rig.y_up {
            [cw / 2.0 - mid[0] * scale, ch / 2.0 + mid[1] * scale]
        } else {
            [cw / 2.0 - mid[0] * scale, ch / 2.0 - mid[1] * scale]
        };
        // The timeline plays it, unless another animation already set it.
        let fps = imported.fps.unwrap_or(self.canvas.timeline.fps).max(1);
        let duration = rig
            .animation
            .and_then(|a| rig.animations.get(a))
            .map_or(0.0, |a| a.duration);
        let first_animation =
            !self.canvas.is_animated() && self.canvas.layers.iter().all(|l| l.rig.is_none());
        if first_animation && duration > 0.0 {
            let canvas = self.canvas_mut();
            canvas.timeline.fps = fps;
            canvas.timeline.end =
                canvas.timeline.start + ((duration * fps as f32).ceil() as u32).max(1) - 1;
        }
        let Some(_) =
            self.add_layer_with_tiles(name, Vec::new(), |layer| layer.rig = Some(Box::new(rig)))
        else {
            return;
        };
        self.canvas.render_rigs();
        self.mark_all_tiles_dirty();
        self.layer_state.thumbnails_dirty = true;
        self.workspace.animation.show_timeline = true;
        if !imported.notes.is_empty() {
            self.report(format!("Brought in, but: {}", imported.notes.join("; ")));
        }
    }

    /// The selected layer's rig, if it's a rig layer.
    pub(crate) fn active_rig(&self) -> Option<&Rig> {
        self.canvas
            .layers
            .get(self.canvas.active_layer_idx)?
            .rig
            .as_deref()
    }

    /// Change the selected rig with `edit`: the first change of a gesture
    /// is an undo step (the rest join it), and the layer is drawn again.
    pub(crate) fn rig_edit(&mut self, edit: impl FnOnce(&mut Rig)) {
        if self.active_rig().is_none() {
            return;
        }
        let i = self.canvas.active_layer_idx;
        if !self.workspace.rig.in_gesture {
            self.workspace.rig.in_gesture = true;
            self.document_step(|_| true);
        }
        self.release_canvas();
        let canvas = crate::app::stroke_ops::exclusive(&mut self.canvas);
        if let Some(rig) = canvas.layers[i].rig.as_deref_mut() {
            edit(rig);
        }
        canvas.render_rigs();
        self.workspace.animation.edits += 1;
        self.mark_all_tiles_dirty();
        self.mark_unsaved();
    }

    /// Layer → Rig → Make Rig from Layers: the picked layers (or the
    /// picked folder's) as one rig layer above them, each a part with a
    /// mesh, the timeline set to key it. The layers stay, hidden. One undo
    /// step (text and vector layers are rasterised first, each its own).
    pub(crate) fn make_rig_from_layers(&mut self) {
        self.quick_mask_leave();
        crate::app::tools::transform::commit_floating_layer(self);
        self.liquify_commit();
        self.puppet_commit();
        use crate::canvas::storage::LayerKind;
        let picked = self.selected_indices();
        // Folders stand for what's in them.
        let folders: Vec<_> = (picked.iter())
            .filter(|&&i| self.canvas.layers[i].kind == LayerKind::Group)
            .map(|&i| self.canvas.layers[i].id)
            .collect();
        let mut candidates: Vec<usize> = (0..self.canvas.layers.len())
            .filter(|&i| {
                let l = &self.canvas.layers[i];
                picked.contains(&i)
                    || folders
                        .iter()
                        .any(|&f| l.id != f && self.canvas.is_within(l.id, f))
            })
            .filter(|&i| self.canvas.layers[i].kind == LayerKind::Paint)
            .collect();
        let before = candidates.len();
        candidates.retain(|&i| {
            let l = &self.canvas.layers[i];
            !l.locked
                && l.rig.is_none()
                && l.adjustment.is_none()
                && l.shader.is_none()
                && l.anim.is_none()
                && l.motion.is_none()
        });
        let left_out = before - candidates.len();
        if candidates.is_empty() {
            self.report(
                "Make Rig from Layers needs unlocked paint layers (pick them, or their folder)"
                    .into(),
            );
            return;
        }
        for &i in &candidates {
            if self.canvas.layers[i].text.is_some() {
                self.rasterise_text_layer(i);
            } else if self.canvas.layers[i].vector.is_some() {
                self.rasterise_vector_layer(i);
            }
            self.bake_impasto(i);
        }
        let settings = self.workspace.puppet;
        let (mut rig, mut notes) = rig_from_layers(
            &self.canvas,
            &candidates,
            settings.density,
            settings.expansion,
        );
        if left_out > 0 {
            notes.push(format!(
                "{left_out} layer(s) left out (locked, animated, moved, or not paint)"
            ));
        }
        if rig.slots.is_empty() {
            self.report(format!("Nothing to make a rig of: {}", notes.join("; ")));
            return;
        }
        rig.animations
            .push(timeline_animation("Animation", &self.canvas.timeline));
        rig.animation = Some(0);
        // Above the topmost one (or the folder), beside it.
        let top = if let [only] = picked[..]
            && folders.len() == 1
        {
            only
        } else {
            *candidates.iter().max().unwrap_or(&0)
        };
        let parent = self.canvas.layers[top].parent;
        let hide: Vec<_> = if folders.len() == 1 && picked.len() == 1 {
            folders.clone()
        } else {
            candidates
                .iter()
                .map(|&i| self.canvas.layers[i].id)
                .collect()
        };
        let name = if folders.len() == 1 && picked.len() == 1 {
            format!("{} rig", self.canvas.layers[top].name)
        } else {
            "Rig".to_string()
        };
        let mut rig = Some(Box::new(rig));
        self.document_step(|canvas| {
            let id = canvas.insert_new_layer(top + 1, name, LayerKind::Paint, parent);
            let Some(i) = canvas.layer_index_of(id) else {
                return false;
            };
            canvas.layers[i].rig = rig.take();
            for l in &mut canvas.layers {
                if hide.contains(&l.id) {
                    l.visible = false;
                }
            }
            canvas.active_layer_idx = i;
            canvas.render_rigs();
            true
        });
        self.layer_state.selected.clear();
        self.workspace.animation.show_timeline = true;
        self.mark_unsaved();
        self.report(if notes.is_empty() {
            "Made a rig: draw bones with the Bone tool, or pin parts with Puppet Warp to key them"
                .into()
        } else {
            format!("Made a rig, but: {}", notes.join("; "))
        });
    }

    /// Bind every mesh part of the selected rig to its bones (`smooth`:
    /// each point weighted between the bones near it). One undo step.
    pub(crate) fn bind_rig(&mut self, smooth: bool) {
        if self.active_rig().is_none_or(|r| r.bones.len() < 2) {
            self.report("Draw bones with the Bone tool first".into());
            return;
        }
        self.rig_edit(|rig| {
            for s in 0..rig.slots.len() {
                rig.bind(s, smooth);
            }
        });
        self.rig_edit_done();
    }

    /// A new animation in the selected rig, as long as the timeline, picked.
    pub(crate) fn new_rig_animation(&mut self) {
        let timeline = self.canvas.timeline;
        self.rig_edit(|rig| {
            let name = (1..)
                .map(|n| format!("Animation {n}"))
                .find(|n| rig.animations.iter().all(|a| &a.name != n))
                .unwrap_or_default();
            rig.animations.push(timeline_animation(&name, &timeline));
            rig.animation = Some(rig.animations.len() - 1);
        });
        self.rig_edit_done();
    }

    /// The gesture ended: the next change is a new undo step.
    pub(crate) fn rig_edit_done(&mut self) {
        self.workspace.rig.in_gesture = false;
    }

    /// Read the video, GIF, animated PNG or WebP at `path` and add it as an
    /// animated layer, a drawing a frame.
    pub(crate) fn import_frames_in_background(&mut self, path: PathBuf) {
        let size = (self.canvas.width() as u32, self.canvas.height() as u32);
        self.spawn_job(Some("Reading the frames…"), move || {
            let result = decode_frames(&path).map(|(frames, fps)| {
                let frames: Vec<image::RgbaImage> = frames
                    .into_iter()
                    .map(|f| crate::app::import::fit_image(f, size))
                    .collect();
                (frames, fps)
            });
            let name = path
                .file_stem()
                .map_or_else(|| "Animation".into(), |s| s.to_string_lossy().into_owned());
            Box::new(move |app: &mut PainterApp| match result {
                Ok((frames, fps)) => app.add_frames_layer(name, frames, fps),
                Err(err) => app.report(err),
            })
        });
    }

    /// `frames` as a new animated layer, one drawing each, centred; the
    /// timeline set to play them if nothing else is animated.
    pub(crate) fn add_frames_layer(
        &mut self,
        name: String,
        frames: Vec<image::RgbaImage>,
        fps: u32,
    ) {
        let ts = self.canvas.tile_size() as i32;
        let (cw, ch) = (self.canvas.width() as i32, self.canvas.height() as i32);
        let tiles_of = |img: &image::RgbaImage| {
            let (w, h) = (img.width() as i32, img.height() as i32);
            crate::app::import::pixels_to_tiles(ts, ((cw - w) / 2, (ch - h) / 2), (w, h), |x, y| {
                let [r, g, b, a] = img.get_pixel(x as u32, y as u32).0;
                eframe::egui::Color32::from_rgba_unmultiplied(r, g, b, a)
            })
        };
        let Some(first) = frames.first() else {
            return;
        };
        if !self.canvas.is_animated() {
            let canvas = self.canvas_mut();
            canvas.timeline.fps = fps.max(1);
            canvas.timeline.end = canvas.timeline.start + frames.len() as u32 - 1;
        }
        let start = self.canvas.timeline.start;
        if self
            .add_layer_with_tiles(name, tiles_of(first), |_| {})
            .is_none()
        {
            return;
        }
        let i = self.canvas.active_layer_idx;
        let rest: Vec<_> = frames[1..].iter().map(tiles_of).collect();
        self.document_step(move |canvas| {
            canvas.time = start;
            let Some(track) = canvas.animate_layer(i) else {
                return false;
            };
            for (k, tiles) in rest.into_iter().enumerate() {
                if let Some(d) = canvas.add_frame(track, start + k as u32 + 1, false) {
                    for ((tx, ty), data) in tiles {
                        canvas.set_layer_tile_data(d, tx, ty, data);
                    }
                }
            }
            true
        });
        self.mark_all_tiles_dirty();
        self.workspace.animation.show_timeline = true;
    }
}

/// A new animation for a rig made here: as long as the timeline (the rig
/// is posed at the document's time in seconds, and an animation loops at
/// its length, so it covers every frame).
pub(crate) fn timeline_animation(
    name: &str,
    timeline: &crate::canvas::animation::Timeline,
) -> RigAnimation {
    RigAnimation {
        name: name.into(),
        duration: (timeline.end + 1) as f32 / timeline.fps.max(1) as f32,
        ..Default::default()
    }
}

/// A rig of `sources` (layer indices, bottom first): each layer a slot
/// with its paint as a mesh picture (the mesh Puppet Warp makes, points
/// `density` pixels apart), on one root bone in the canvas's middle. Rig
/// units are canvas pixels. Also what's left out of it, to say.
pub(crate) fn rig_from_layers(
    canvas: &crate::canvas::Canvas,
    sources: &[usize],
    density: f32,
    expansion: f32,
) -> (Rig, Vec<String>) {
    use crate::canvas::blend_modes::LayerBlend;
    use crate::canvas::puppet::mesh::PuppetShape;
    use crate::canvas::rig::{Attachment, Bone, MeshVertices, RigImage, Shape, Slot, SlotBlend};
    use eframe::egui::Color32;
    let ts = canvas.tile_size() as i32;
    let root = [canvas.width() as f32 / 2.0, canvas.height() as f32 / 2.0];
    let mut rig = Rig {
        bones: vec![Bone {
            name: "root".into(),
            x: root[0],
            y: root[1],
            ..Default::default()
        }],
        origin: [0.0, 0.0],
        scale: 1.0,
        y_up: false,
        ..Default::default()
    };
    let mut notes = Vec::new();
    for &i in sources {
        let layer = &canvas.layers[i];
        let tiles = canvas.capture_layer_pixels(i);
        // Where its paint is.
        let (mut x0, mut y0, mut x1, mut y1) = (i32::MAX, i32::MAX, i32::MIN, i32::MIN);
        for (&(tx, ty), data) in &tiles {
            for (k, c) in data.iter().enumerate() {
                if c.a() > 0 {
                    let (x, y) = (tx * ts + k as i32 % ts, ty * ts + k as i32 / ts);
                    (x0, y0, x1, y1) = (x0.min(x), y0.min(y), x1.max(x + 1), y1.max(y + 1));
                }
            }
        }
        if x0 >= x1 {
            notes.push(format!("{} is empty", layer.name));
            continue;
        }
        let at = |x: i32, y: i32| -> Color32 {
            let (tx, ty) = (x.div_euclid(ts), y.div_euclid(ts));
            tiles.get(&(tx, ty)).map_or(Color32::TRANSPARENT, |d| {
                d[(y.rem_euclid(ts) * ts + x.rem_euclid(ts)) as usize]
            })
        };
        let Some((mesh, _)) = PuppetShape::from_paint(
            [x0, y0, x1, y1],
            density,
            expansion.max(0.0).round() as usize,
            |x, y| at(x, y).a() > 0,
        )
        .and_then(|shape| shape.mesh(&[])) else {
            notes.push(format!("{} couldn't be meshed", layer.name));
            continue;
        };
        // Its picture: the paint and a margin as wide as the mesh reaches.
        let (mut px0, mut py0, mut px1, mut py1) = (x0, y0, x1, y1);
        for p in &mesh.rest {
            px0 = px0.min(p[0].floor() as i32 - 1);
            py0 = py0.min(p[1].floor() as i32 - 1);
            px1 = px1.max(p[0].ceil() as i32 + 1);
            py1 = py1.max(p[1].ceil() as i32 + 1);
        }
        let (w, h) = ((px1 - px0) as usize, (py1 - py0) as usize);
        let pixels = (0..w * h)
            .map(|k| at(px0 + (k % w) as i32, py0 + (k / w) as i32))
            .collect();
        let image = rig.images.len();
        rig.images.push(RigImage {
            name: layer.name.clone(),
            width: w,
            height: h,
            pixels: std::sync::Arc::new(pixels),
        });
        let uvs = (mesh.rest.iter())
            .map(|p| {
                [
                    (p[0] - px0 as f32) / w as f32,
                    (p[1] - py0 as f32) / h as f32,
                ]
            })
            .collect();
        let vertices = (mesh.rest.iter())
            .map(|p| [p[0] - root[0], p[1] - root[1]])
            .collect();
        let blend = match layer.blend {
            LayerBlend::Normal => SlotBlend::Normal,
            LayerBlend::Multiply => SlotBlend::Multiply,
            LayerBlend::Screen => SlotBlend::Screen,
            LayerBlend::LinearDodge => SlotBlend::Additive,
            _ => {
                notes.push(format!("{}'s blend mode is Normal in the rig", layer.name));
                SlotBlend::Normal
            }
        };
        let mut name = layer.name.clone();
        while rig.slot_index(&name).is_some() {
            name.push('\'');
        }
        rig.slots.push(Slot {
            name: name.clone(),
            bone: 0,
            color: [1.0, 1.0, 1.0, layer.opacity],
            attachment: layer.visible.then(|| name.clone()),
            blend,
            attachments: vec![Attachment {
                name,
                image,
                region: [0.0, 0.0, w as f32, h as f32],
                rotated: false,
                shape: Shape::Mesh {
                    uvs,
                    triangles: mesh.tris,
                    vertices: MeshVertices::Plain(vertices),
                },
            }],
        });
    }
    (rig, notes)
}

/// Bone `b`'s turn and place at `seconds` (its setup plus the animation
/// playing).
pub(crate) fn bone_now(rig: &Rig, b: usize, seconds: f32) -> (f32, f32, f32) {
    use crate::canvas::rig::eval::sample;
    let bone = &rig.bones[b];
    let (mut rot, mut x, mut y) = (bone.rotation, bone.x, bone.y);
    if let Some(anim) = rig.animation.and_then(|a| rig.animations.get(a)) {
        let t = if anim.duration > 0.0 {
            seconds.rem_euclid(anim.duration)
        } else {
            seconds
        };
        if let Some(track) = anim.bones.iter().find(|tr| tr.bone == b) {
            rot += sample(&track.rotate, t).unwrap_or(0.0);
            let [dx, dy] = sample(&track.translate, t).unwrap_or([0.0, 0.0]);
            (x, y) = (x + dx, y + dy);
        }
    }
    (rot, x, y)
}

/// Turn and place bone `b` at `seconds`: a key in the animation playing
/// (its setup pose left as it is), or its setup pose without one.
pub(crate) fn set_bone(rig: &mut Rig, b: usize, seconds: f32, rotation: f32, at: [f32; 2]) {
    use crate::canvas::rig::{BoneTrack, set_key};
    let Some(a) = rig.animation.filter(|&a| a < rig.animations.len()) else {
        let bone = &mut rig.bones[b];
        (bone.rotation, bone.x, bone.y) = (rotation, at[0], at[1]);
        return;
    };
    let (setup_rot, setup_x, setup_y) = (rig.bones[b].rotation, rig.bones[b].x, rig.bones[b].y);
    let anim = &mut rig.animations[a];
    let t = if anim.duration > 0.0 {
        seconds.rem_euclid(anim.duration)
    } else {
        seconds
    };
    let i = match anim.bones.iter().position(|tr| tr.bone == b) {
        Some(i) => i,
        None => {
            anim.bones.push(BoneTrack {
                bone: b,
                ..Default::default()
            });
            anim.bones.len() - 1
        }
    };
    let track = &mut anim.bones[i];
    set_key(&mut track.rotate, t, rotation - setup_rot);
    set_key(&mut track.translate, t, [at[0] - setup_x, at[1] - setup_y]);
    anim.duration = anim.duration.max(t);
}

/// A file's frames and its frame rate: GIF, animated PNG and WebP read
/// here, other videos through ffmpeg.
pub(crate) fn decode_frames(path: &Path) -> Result<(Vec<image::RgbaImage>, u32), String> {
    use image::AnimationDecoder;
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    let open = || {
        std::fs::File::open(path)
            .map(std::io::BufReader::new)
            .map_err(|e| e.to_string())
    };
    let collect = |frames: image::Frames<'_>| -> Result<(Vec<image::RgbaImage>, u32), String> {
        let mut out = Vec::new();
        let mut fps = 12;
        for f in frames.take(MAX_FRAMES) {
            let f = f.map_err(|e| e.to_string())?;
            let (n, d) = f.delay().numer_denom_ms();
            if out.is_empty() && n > 0 {
                fps = ((1000.0 * d as f32 / n as f32).round() as u32).clamp(1, 120);
            }
            out.push(f.into_buffer());
        }
        if out.is_empty() {
            return Err("No frames".into());
        }
        Ok((out, fps))
    };
    match ext.as_str() {
        "gif" => collect(
            image::codecs::gif::GifDecoder::new(open()?)
                .map_err(|e| e.to_string())?
                .into_frames(),
        ),
        "png" | "apng" => {
            let decoder =
                image::codecs::png::PngDecoder::new(open()?).map_err(|e| e.to_string())?;
            collect(decoder.apng().map_err(|e| e.to_string())?.into_frames())
        }
        "webp" => collect(
            image::codecs::webp::WebPDecoder::new(open()?)
                .map_err(|e| e.to_string())?
                .into_frames(),
        ),
        _ => decode_with_ffmpeg(path),
    }
}

/// A video's frames through ffmpeg (PNGs in a folder of their own).
fn decode_with_ffmpeg(path: &Path) -> Result<(Vec<image::RgbaImage>, u32), String> {
    let dir = std::env::temp_dir().join(format!(
        "rp-frames-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos())
    ));
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    // At the info level ffmpeg's log names the frame rate (no ffprobe).
    let output = std::process::Command::new("ffmpeg")
        .args(["-hide_banner", "-nostats", "-i"])
        .arg(path)
        .args(["-frames:v", &MAX_FRAMES.to_string()])
        .arg(dir.join("f_%05d.png"))
        .output()
        .map_err(|_| "Videos need ffmpeg installed (GIFs and animated PNGs don't)".to_string())?;
    let result = (|| {
        if !output.status.success() {
            return Err("ffmpeg couldn't read the video".to_string());
        }
        let mut names: Vec<PathBuf> = std::fs::read_dir(&dir)
            .map_err(|e| e.to_string())?
            .flatten()
            .map(|e| e.path())
            .collect();
        names.sort();
        let frames = names
            .iter()
            .map(|p| {
                image::open(p)
                    .map(|i| i.to_rgba8())
                    .map_err(|e| e.to_string())
            })
            .collect::<Result<Vec<_>, _>>()?;
        if frames.is_empty() {
            return Err("The video has no frames".to_string());
        }
        let fps = fps_in_ffmpeg_log(&String::from_utf8_lossy(&output.stderr))
            .unwrap_or(24)
            .clamp(1, 120);
        Ok((frames, fps))
    })();
    let _ = std::fs::remove_dir_all(&dir);
    result
}

/// The first video stream's frame rate in ffmpeg's log: its input line reads
/// "Stream #0:0…: Video: h264 …, 1280x720, 29.97 fps, 29.97 tbr, …".
fn fps_in_ffmpeg_log(log: &str) -> Option<u32> {
    let line = log
        .lines()
        .find(|l| l.contains("Stream #0") && l.contains("Video:"))?;
    let fps = line
        .split(", ")
        .find_map(|part| part.strip_suffix(" fps"))?;
    Some(fps.trim().parse::<f32>().ok()?.round() as u32)
}

#[cfg(test)]
mod tests {
    use crate::canvas::Canvas;

    #[test]
    fn the_frame_rate_comes_from_ffmpegs_log() {
        let log = "Input #0, mov,mp4,m4a,3gp,3g2,mj2, from 'a.mp4':\n  Duration: 00:00:01.00\n  \
            Stream #0:0[0x1](und): Video: h264 (High) (avc1 / 0x31637661), yuv420p(progressive), \
            320x240, 41 kb/s, 29.97 fps, 29.97 tbr, 30k tbn (default)\nOutput #0, image2, to 'f.png':\n  \
            Stream #0:0: Video: png, rgb24, 320x240, q=2-31, 200 kb/s, 25 fps, 25 tbn\n";
        assert_eq!(super::fps_in_ffmpeg_log(log), Some(30));
        assert_eq!(super::fps_in_ffmpeg_log("no video here"), None);
    }
    use eframe::egui::Color32;

    #[test]
    fn an_imported_rig_is_a_layer_that_plays_and_undoes() {
        let canvas = Canvas::new(128, 128, Color32::WHITE, 64);
        let mut app = crate::project::tests::test_app_pub(canvas);
        let files = crate::project::anim_import::spine::tests::files();
        let imported = crate::project::anim_import::spine::import(
            crate::project::anim_import::spine::tests::SKELETON.as_bytes(),
            "arm",
            &files,
        )
        .unwrap();
        app.add_rig_layer("arm".into(), imported);
        let i = app.canvas.active_layer_idx;
        assert!(app.canvas.layers[i].rig.is_some());
        assert!(!app.canvas.layer_tile_keys(i).is_empty(), "drawn");
        assert_eq!(app.canvas.timeline.end, 11, "a second at 12 fps");
        let at_start = app.canvas.flatten_final().pixels;
        app.go_to_frame(11);
        assert_ne!(app.canvas.flatten_final().pixels, at_start, "it moved");
        // A rig edit is an undo step; its whole gesture one.
        app.rig_edit(|rig| rig.bones[1].rotation = 30.0);
        app.rig_edit(|rig| rig.bones[1].rotation = 60.0);
        app.rig_edit_done();
        app.apply_history(false);
        assert_eq!(app.active_rig().unwrap().bones[1].rotation, 0.0);
        // Saved and opened again: still a rig, with its pictures.
        let bytes = crate::project::encode_project(&app).unwrap();
        let loaded = crate::project::decode_project(&bytes).unwrap();
        let rig = loaded
            .canvas
            .layers
            .iter()
            .find_map(|l| l.rig.as_deref())
            .unwrap();
        assert_eq!(rig.images.len(), 1);
        assert_eq!(rig.images[0].width, 16);
    }

    #[test]
    fn a_gif_comes_in_as_an_animated_layer() {
        use image::codecs::gif::{GifEncoder, Repeat};
        let path = std::env::temp_dir().join(format!("rp-frames-{}.gif", std::process::id()));
        {
            let mut e = GifEncoder::new(std::fs::File::create(&path).unwrap());
            e.set_repeat(Repeat::Infinite).unwrap();
            for c in [[255u8, 0, 0, 255], [0, 0, 255, 255], [0, 255, 0, 255]] {
                let img = image::RgbaImage::from_pixel(20, 10, image::Rgba(c));
                e.encode_frame(image::Frame::from_parts(
                    img,
                    0,
                    0,
                    image::Delay::from_numer_denom_ms(100, 1),
                ))
                .unwrap();
            }
        }
        let (frames, fps) = super::decode_frames(&path).unwrap();
        let _ = std::fs::remove_file(&path);
        assert_eq!((frames.len(), fps), (3, 10));
        let mut app = crate::project::tests::test_app_pub(Canvas::new(64, 64, Color32::WHITE, 64));
        app.add_frames_layer("clip".into(), frames, fps);
        assert!(app.canvas.is_animated());
        assert_eq!((app.canvas.timeline.fps, app.canvas.timeline.end), (10, 2));
        let px = |app: &crate::PainterApp| app.canvas.flatten_final().pixels[32 * 64 + 32];
        app.go_to_frame(0);
        assert_eq!(px(&app), Color32::RED);
        app.go_to_frame(1);
        assert_eq!(px(&app), Color32::BLUE);
        app.go_to_frame(2);
        assert_eq!(px(&app), Color32::GREEN);
    }
}

#[cfg(test)]
mod keying_tests {
    use super::*;
    use crate::canvas::rig::{Bone, RigAnimation};

    #[test]
    fn a_bone_changed_while_playing_gets_a_key_there() {
        let mut rig = Rig {
            bones: vec![Bone {
                rotation: 10.0,
                ..Default::default()
            }],
            animations: vec![RigAnimation {
                name: "a".into(),
                duration: 2.0,
                ..Default::default()
            }],
            animation: Some(0),
            ..Default::default()
        };
        set_bone(&mut rig, 0, 1.0, 40.0, [3.0, 0.0]);
        assert_eq!(bone_now(&rig, 0, 1.0), (40.0, 3.0, 0.0));
        assert_eq!(rig.bones[0].rotation, 10.0, "the setup pose stays");
        assert_eq!(rig.animations[0].bones[0].rotate[0].value, 30.0);
        // Again at the same time: the key changes, no second one.
        set_bone(&mut rig, 0, 1.0, 50.0, [3.0, 0.0]);
        assert_eq!(rig.animations[0].bones[0].rotate.len(), 1);
        // Without an animation: the setup pose.
        rig.animation = None;
        set_bone(&mut rig, 0, 0.0, 5.0, [1.0, 2.0]);
        assert_eq!(
            (rig.bones[0].rotation, rig.bones[0].x, rig.bones[0].y),
            (5.0, 1.0, 2.0)
        );
    }
}
