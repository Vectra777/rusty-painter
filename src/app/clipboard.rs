//! Copy, cut and paste of pixels, and duplicating a layer.
//!
//! Copy takes the selection (soft edges included) from the selected layer,
//! or from everything visible for Copy Merged; without a selection, the
//! whole layer (or picture). The pixels are kept here exactly, and on the
//! desktop also put on the system clipboard as an image, so they paste into
//! other programs. Paste puts them on a new layer (one undo step) with the
//! Transform tool ready: where they were copied from if that's on the
//! canvas, or centred for an image from another program.

use crate::PainterApp;
use crate::app::tools::Tool;
use crate::canvas::storage::LayerKind;
use crate::selection::SelectionMask;
use crate::selection::transform::TransformInfo;
use eframe::egui::{self, Color32};

/// Copied pixels (premultiplied), and where they were on the canvas.
#[derive(Clone, Debug, PartialEq)]
pub struct Clip {
    pub x: i32,
    pub y: i32,
    pub w: usize,
    pub h: usize,
    pub pixels: Vec<Color32>,
}

#[derive(Default)]
pub struct ClipboardState {
    pub clip: Option<Clip>,
    /// Size and fingerprint of the image last put on the system clipboard,
    /// to tell on paste whether it still holds our copy (then the exact
    /// pixels and position are used) or something from another program.
    exported: Option<(usize, usize, u64)>,
    /// A V press was swallowed as a paste command (see
    /// [`PainterApp::clipboard_keys`]).
    paste_key_down: bool,
    /// And this press already pasted.
    pasted_this_press: bool,
}

impl Clip {
    /// Unmultiplied RGBA bytes, row-major.
    fn to_rgba(&self) -> Vec<u8> {
        self.pixels
            .iter()
            .flat_map(|p| p.to_srgba_unmultiplied())
            .collect()
    }

    /// Shrunk to its non-transparent pixels; `None` if there are none.
    fn trimmed(self) -> Option<Clip> {
        let (mut x0, mut y0, mut x1, mut y1) = (usize::MAX, usize::MAX, 0, 0);
        for y in 0..self.h {
            for x in 0..self.w {
                if self.pixels[y * self.w + x].a() > 0 {
                    x0 = x0.min(x);
                    y0 = y0.min(y);
                    x1 = x1.max(x + 1);
                    y1 = y1.max(y + 1);
                }
            }
        }
        if x0 == usize::MAX {
            return None;
        }
        let (w, h) = (x1 - x0, y1 - y0);
        let pixels = (y0..y1)
            .flat_map(|y| {
                self.pixels[y * self.w + x0..y * self.w + x1]
                    .iter()
                    .copied()
            })
            .collect();
        Some(Clip {
            x: self.x + x0 as i32,
            y: self.y + y0 as i32,
            w,
            h,
            pixels,
        })
    }
}

fn fingerprint(bytes: &[u8]) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    bytes.hash(&mut h);
    h.finish()
}

impl PainterApp {
    /// Copy the selection from the selected layer (or everything visible
    /// when `merged`). Returns whether anything was copied.
    pub(crate) fn copy_selection(&mut self, merged: bool) -> bool {
        crate::app::tools::transform::commit_floating_layer(self);
        self.release_canvas();
        let active = self.canvas.active_layer_idx;
        let source = (!merged).then_some(active);
        if let Some(i) = source
            && !matches!(
                self.canvas.layers.get(i).map(|l| l.kind),
                Some(LayerKind::Paint)
            )
        {
            return false;
        }
        let (w, h) = (self.canvas.width() as i32, self.canvas.height() as i32);
        let has_selection = self.selection_manager.has_selection();
        let bounds = if has_selection {
            self.selection_manager
                .get_bounds()
                .map(|b| self.pixel_bounds(b))
        } else if merged {
            Some([0, 0, w, h])
        } else {
            self.canvas
                .get_content_bounds(active, None)
                .map(|b| self.pixel_bounds(b))
        };
        let Some([x0, y0, x1, y1]) = bounds.filter(|b| b[2] > b[0] && b[3] > b[1]) else {
            return false;
        };
        let (cw, ch) = ((x1 - x0) as usize, (y1 - y0) as usize);
        let mut pixels = if merged {
            // The picture exactly as shown (blend modes, masks, background),
            // without the draft layers.
            let mut img = egui::ColorImage::new([0, 0], Color32::TRANSPARENT);
            let view = self.canvas.without_drafts();
            view.as_ref()
                .unwrap_or(&self.canvas)
                .write_region_to_color_image(x0 as usize, y0 as usize, cw, ch, &mut img, 1);
            img.pixels
        } else {
            self.canvas.render_reference(source, x0, y0, cw, ch)
        };
        if has_selection {
            let sel = &self.selection_manager;
            let mask =
                SelectionMask::rasterize([x0, y0, x1, y1], |y, x, out| sel.row_coverage(y, x, out));
            for (p, &cov) in pixels.iter_mut().zip(&mask.data) {
                if cov < 255 {
                    let k = |c: u8| ((c as u32 * cov as u32 + 127) / 255) as u8;
                    let [r, g, b, a] = p.to_array();
                    *p = Color32::from_rgba_premultiplied(k(r), k(g), k(b), k(a));
                }
            }
        }
        let clip = Clip {
            x: x0,
            y: y0,
            w: cw,
            h: ch,
            pixels,
        };
        let Some(clip) = clip.trimmed() else {
            return false;
        };
        self.export_clip(&clip);
        self.workspace.clipboard.clip = Some(clip);
        true
    }

    /// Copy, then erase what was copied from the layer (one undo step).
    pub(crate) fn cut_selection(&mut self) {
        if !self.copy_selection(false) {
            return;
        }
        if self.selection_manager.has_selection() {
            self.delete_selection_contents();
        } else if let Some(clip) = &self.workspace.clipboard.clip {
            // The whole layer was copied: erase all of it.
            let [x0, y0] = [clip.x, clip.y];
            let full = SelectionMask::new(x0, y0, clip.w, clip.h, vec![255; clip.w * clip.h]);
            self.erase_under(full, false);
        }
    }

    /// Paste as a new layer, with the Transform tool ready to place it.
    pub(crate) fn paste(&mut self) {
        let ours = self.workspace.clipboard.clip.clone();
        match self.import_clip() {
            // Another program's image: centred, scaled to fit.
            Some(img) => self.import_rgba("Pasted", img),
            None => {
                let Some(clip) = ours else {
                    return;
                };
                self.paste_clip(&clip);
            }
        }
    }

    /// Our own copy: exact pixels, where they came from (centred if that's
    /// off the canvas now).
    fn paste_clip(&mut self, clip: &Clip) {
        let (cw, ch) = (self.canvas.width() as i32, self.canvas.height() as i32);
        let (w, h) = (clip.w as i32, clip.h as i32);
        let on_canvas = clip.x < cw && clip.y < ch && clip.x + w > 0 && clip.y + h > 0;
        let origin = if on_canvas {
            (clip.x, clip.y)
        } else {
            ((cw - w) / 2, (ch - h) / 2)
        };
        let ts = self.canvas.tile_size() as i32;
        let tiles = crate::app::import::pixels_to_tiles(ts, origin, (w, h), |x, y| {
            clip.pixels[y as usize * clip.w + x as usize]
        });
        if self
            .add_layer_with_tiles("Pasted".to_string(), tiles, |_| {})
            .is_some()
        {
            self.selection_manager.clear_selection();
            self.active_tool = Tool::Transform(TransformInfo::default());
        }
    }

    /// Duplicate the selected layer just above it (one undo step).
    pub(crate) fn duplicate_layer(&mut self) {
        crate::app::tools::transform::commit_floating_layer(self);
        self.release_canvas();
        let active = self.canvas.active_layer_idx;
        let Some(source) = self.canvas.layers.get(active) else {
            return;
        };
        if source.kind != LayerKind::Paint || active == 0 {
            return;
        }
        let (name, visible, opacity, alpha_locked, blend) = (
            format!("{} copy", source.name),
            source.visible,
            source.opacity,
            source.alpha_locked,
            source.blend,
        );
        let text = source.text.clone();
        let (position_locked, draft, reference) =
            (source.position_locked, source.draft, source.reference);
        let tiles: Vec<((i32, i32), Vec<Color32>)> = self
            .canvas
            .layer_tile_keys(active)
            .into_iter()
            .filter_map(|(tx, ty)| {
                let data = self.canvas.get_layer_tile_data(active, tx, ty)?;
                data.iter()
                    .any(|&p| p != Color32::TRANSPARENT)
                    .then_some(((tx, ty), data))
            })
            .collect();
        self.add_layer_with_tiles(name, tiles, |layer| {
            layer.visible = visible;
            layer.opacity = opacity;
            layer.alpha_locked = alpha_locked;
            layer.blend = blend;
            layer.text = text;
            layer.position_locked = position_locked;
            layer.draft = draft;
            layer.reference = reference;
        });
    }

    /// Ctrl+C / Ctrl+Shift+C (copy merged), Ctrl+X and Ctrl+V. The window
    /// layer turns these into Copy / Cut / Paste events rather than keys, and
    /// only sends Paste when the clipboard holds text: with an image on it,
    /// the V press never arrives. Its release does, so a V released without
    /// its press having been seen is taken as the paste.
    pub(crate) fn clipboard_keys(&mut self, ctx: &egui::Context) -> bool {
        if ctx.wants_keyboard_input() {
            return false;
        }
        let events = ctx.input(|i| i.events.clone());
        let mut acted = false;
        for event in events {
            match event {
                egui::Event::Copy => {
                    let merged = ctx.input(|i| i.modifiers.shift);
                    acted |= self.copy_selection(merged);
                }
                egui::Event::Cut => {
                    self.cut_selection();
                    acted = true;
                }
                egui::Event::Paste(_) => {
                    let state = &mut self.workspace.clipboard;
                    state.paste_key_down = true;
                    if !state.pasted_this_press {
                        state.pasted_this_press = true;
                        self.paste();
                        acted = true;
                    }
                }
                egui::Event::Key {
                    key: egui::Key::V,
                    pressed,
                    ..
                } => {
                    let state = &mut self.workspace.clipboard;
                    if pressed {
                        // A plain V (seen as a key): not a paste.
                        state.paste_key_down = true;
                        state.pasted_this_press = true;
                    } else {
                        let swallowed = !state.paste_key_down;
                        let already = state.pasted_this_press;
                        state.paste_key_down = false;
                        state.pasted_this_press = false;
                        if swallowed && !already {
                            self.paste();
                            acted = true;
                        }
                    }
                }
                _ => {}
            }
        }
        acted
    }

    /// Put `clip` on the system clipboard (desktop).
    #[cfg(not(target_os = "android"))]
    fn export_clip(&mut self, clip: &Clip) {
        let bytes = clip.to_rgba();
        let fp = fingerprint(&bytes);
        let image = arboard::ImageData {
            width: clip.w,
            height: clip.h,
            bytes: std::borrow::Cow::Owned(bytes),
        };
        match arboard::Clipboard::new().and_then(|mut c| c.set_image(image)) {
            Ok(()) => self.workspace.clipboard.exported = Some((clip.w, clip.h, fp)),
            Err(err) => {
                log::warn!("Couldn't put the image on the system clipboard: {err}");
                self.workspace.clipboard.exported = None;
            }
        }
    }

    #[cfg(target_os = "android")]
    fn export_clip(&mut self, _clip: &Clip) {}

    /// An image on the system clipboard that isn't our own last copy.
    #[cfg(not(target_os = "android"))]
    fn import_clip(&self) -> Option<image::RgbaImage> {
        let image = arboard::Clipboard::new().ok()?.get_image().ok()?;
        let ours = self.workspace.clipboard.exported.is_some_and(|(w, h, fp)| {
            (w, h) == (image.width, image.height) && fp == fingerprint(&image.bytes)
        });
        if ours && self.workspace.clipboard.clip.is_some() {
            return None;
        }
        image::RgbaImage::from_raw(
            image.width as u32,
            image.height as u32,
            image.bytes.into_owned(),
        )
    }

    #[cfg(target_os = "android")]
    fn import_clip(&self) -> Option<image::RgbaImage> {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::canvas::Canvas;
    use crate::selection::{SelectionMode, SelectionShape};
    use eframe::egui::Vec2;

    const RED: Color32 = Color32::from_rgb(255, 0, 0);

    fn app() -> PainterApp {
        let mut app = crate::project::tests::test_app_pub(Canvas::new(128, 64, Color32::WHITE, 64));
        app.canvas_mut().active_layer_idx = 1;
        let mut red = vec![Color32::TRANSPARENT; 64 * 64];
        for y in 10..40 {
            for x in 10..40 {
                red[y * 64 + x] = RED;
            }
        }
        app.canvas_mut().set_layer_tile_data(1, 0, 0, red);
        app.selection_manager.canvas_size = [128, 64];
        app
    }

    fn pixel(app: &PainterApp, layer: usize, x: i32, y: i32) -> Color32 {
        app.canvas
            .get_layer_tile_data(layer, x / 64, y / 64)
            .and_then(|t| t.get(((y % 64) * 64 + x % 64) as usize).copied())
            .unwrap_or(Color32::TRANSPARENT)
    }

    fn select(app: &mut PainterApp, x0: f32, y0: f32, x1: f32, y1: f32) {
        app.selection_manager.apply_shape(
            SelectionShape::Rectangle {
                start: Vec2::new(x0, y0),
                end: Vec2::new(x1, y1),
            },
            SelectionMode::Replace,
        );
    }

    #[test]
    fn copy_keeps_the_selected_pixels_trimmed_to_what_is_painted() {
        let mut app = app();
        select(&mut app, 0.0, 0.0, 20.0, 20.0);
        assert!(app.copy_selection(false));
        let clip = app.workspace.clipboard.clip.clone().unwrap();
        assert_eq!((clip.x, clip.y, clip.w, clip.h), (10, 10, 10, 10));
        assert!(clip.pixels.iter().all(|&p| p == RED));
    }

    #[test]
    fn paste_puts_the_copy_on_a_new_layer_in_place_as_one_undo_step() {
        let mut app = app();
        select(&mut app, 0.0, 0.0, 20.0, 20.0);
        app.copy_selection(false);
        let clip = app.workspace.clipboard.clip.clone().unwrap();
        app.paste_clip(&clip);
        assert_eq!(app.canvas.layers.len(), 3);
        assert_eq!(app.canvas.active_layer_idx, 2);
        assert_eq!(pixel(&app, 2, 15, 15), RED, "in place");
        assert_eq!(pixel(&app, 2, 25, 25).a(), 0, "only what was selected");
        assert!(matches!(app.active_tool, Tool::Transform(_)));
        app.apply_history(false);
        assert_eq!(app.canvas.layers.len(), 2, "one undo takes it all back");
        app.apply_history(true);
        assert_eq!(pixel(&app, 2, 15, 15), RED, "redo brings the pixels too");
    }

    #[test]
    fn cut_erases_what_it_copies() {
        let mut app = app();
        select(&mut app, 0.0, 0.0, 20.0, 20.0);
        app.cut_selection();
        assert_eq!(pixel(&app, 1, 15, 15).a(), 0);
        assert_eq!(pixel(&app, 1, 25, 25), RED);
        assert!(app.workspace.clipboard.clip.is_some());
    }

    #[test]
    fn copy_merged_takes_everything_visible() {
        let mut app = app();
        select(&mut app, 50.0, 50.0, 60.0, 60.0);
        assert!(app.copy_selection(true));
        let clip = app.workspace.clipboard.clip.clone().unwrap();
        assert!(
            clip.pixels.iter().all(|&p| p == Color32::WHITE),
            "the white background"
        );
    }

    #[test]
    fn duplicate_copies_the_layer_and_its_look_as_one_step() {
        let mut app = app();
        app.canvas_mut().layers[1].opacity = 0.5;
        app.duplicate_layer();
        assert_eq!(app.canvas.layers.len(), 3);
        let copy = &app.canvas.layers[2];
        assert_eq!(copy.name, "Layer 1 copy");
        assert_eq!(copy.opacity, 0.5);
        assert_eq!(pixel(&app, 2, 20, 20), RED);
        app.apply_history(false);
        assert_eq!(app.canvas.layers.len(), 2);
    }
}
