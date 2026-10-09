//! The UI's icons: Phosphor's (regular weight, MIT, `assets/icons/phosphor`),
//! drawn from their SVGs at the size shown, so they stay crisp at any scale
//! and don't depend on which glyphs the bundled font has. Drawn white once
//! per size, then tinted to the colour asked for.

use eframe::egui::{self, Color32, Rect};
use std::collections::HashMap;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum Icon {
    Brush,
    Eraser,
    SelectRect,
    SelectEllipse,
    Lasso,
    Transform,
    Eyedropper,
    Swap,
    Eye,
    EyeOff,
    Lock,
    Unlock,
    Trash,
    Plus,
    Undo,
    Redo,
    Presets,
    Folder,
    Mask,
    SelectBrush,
    Bucket,
    Liquify,
    Palette,
    Smudge,
    Blur,
    Menu,
    Finger,
    Wand,
    ColorRange,
    Magnetic,
    SelectPolygon,
    Symmetry,
    Flip,
    ShapeLine,
    ShapeRect,
    ShapeEllipse,
    ShapePolygon,
    ShapeCurve,
    Ruler,
    Gradient,
    Text,
    Layers,
    Sliders,
    /// A dotted path to a key: the Animate tool.
    Motion,
    /// A strip of film: the timeline.
    Timeline,
    /// A tick: chosen (the library's Select mode).
    Check,
    /// Into the app: importing.
    Import,
    /// Out of the app: exporting.
    Export,
    Duplicate,
    NewFolder,
    /// More actions, in a menu.
    More,
    /// Draw around an area to erase it.
    LassoDelete,
    SelectAll,
    SelectInvert,
    Deselect,
}

impl Icon {
    #[cfg(test)]
    pub(crate) const ALL: [Icon; 55] = [
        Icon::Brush,
        Icon::Eraser,
        Icon::SelectRect,
        Icon::SelectEllipse,
        Icon::Lasso,
        Icon::Transform,
        Icon::Eyedropper,
        Icon::Swap,
        Icon::Eye,
        Icon::EyeOff,
        Icon::Lock,
        Icon::Unlock,
        Icon::Trash,
        Icon::Plus,
        Icon::Undo,
        Icon::Redo,
        Icon::Presets,
        Icon::Folder,
        Icon::Mask,
        Icon::SelectBrush,
        Icon::Bucket,
        Icon::Liquify,
        Icon::Palette,
        Icon::Smudge,
        Icon::Blur,
        Icon::Menu,
        Icon::Finger,
        Icon::Wand,
        Icon::ColorRange,
        Icon::Magnetic,
        Icon::SelectPolygon,
        Icon::Symmetry,
        Icon::Flip,
        Icon::ShapeLine,
        Icon::ShapeRect,
        Icon::ShapeEllipse,
        Icon::ShapePolygon,
        Icon::ShapeCurve,
        Icon::Ruler,
        Icon::Gradient,
        Icon::Text,
        Icon::Layers,
        Icon::Sliders,
        Icon::Motion,
        Icon::Timeline,
        Icon::Check,
        Icon::Import,
        Icon::Export,
        Icon::Duplicate,
        Icon::NewFolder,
        Icon::More,
        Icon::LassoDelete,
        Icon::SelectAll,
        Icon::SelectInvert,
        Icon::Deselect,
    ];

    /// Its Phosphor icon's SVG.
    fn svg(self) -> &'static str {
        macro_rules! phosphor {
            ($name:literal) => {
                include_str!(concat!("../../assets/icons/phosphor/", $name, ".svg"))
            };
        }
        match self {
            Icon::Brush => phosphor!("paint-brush"),
            Icon::Eraser => phosphor!("eraser"),
            Icon::SelectRect => phosphor!("selection"),
            Icon::SelectEllipse => phosphor!("circle-dashed"),
            Icon::Lasso => phosphor!("scribble-loop"),
            Icon::Transform => phosphor!("bounding-box"),
            Icon::Eyedropper => phosphor!("eyedropper"),
            Icon::Swap => phosphor!("arrows-left-right"),
            Icon::Eye => phosphor!("eye"),
            Icon::EyeOff => phosphor!("eye-slash"),
            Icon::Lock => phosphor!("lock-simple"),
            Icon::Unlock => phosphor!("lock-simple-open"),
            Icon::Trash => phosphor!("trash"),
            Icon::Plus => phosphor!("plus"),
            Icon::Undo => phosphor!("arrow-u-up-left"),
            Icon::Redo => phosphor!("arrow-u-up-right"),
            Icon::Presets => phosphor!("squares-four"),
            Icon::Folder => phosphor!("folder-simple"),
            Icon::Mask => phosphor!("circle-half"),
            Icon::SelectBrush => phosphor!("selection-plus"),
            Icon::Bucket => phosphor!("paint-bucket"),
            Icon::Liquify => phosphor!("waves"),
            Icon::Palette => phosphor!("palette"),
            Icon::Smudge => phosphor!("hand-pointing"),
            Icon::Blur => phosphor!("drop"),
            Icon::Menu => phosphor!("list"),
            Icon::Finger => phosphor!("hand-tap"),
            Icon::Wand => phosphor!("magic-wand"),
            Icon::ColorRange => phosphor!("eyedropper-sample"),
            Icon::Magnetic => phosphor!("magnet"),
            Icon::SelectPolygon | Icon::ShapePolygon => phosphor!("polygon"),
            Icon::Symmetry => phosphor!("butterfly"),
            Icon::Flip => phosphor!("arrows-out-line-horizontal"),
            Icon::ShapeLine => phosphor!("line-segment"),
            Icon::ShapeRect => phosphor!("rectangle"),
            Icon::ShapeEllipse => phosphor!("circle"),
            Icon::ShapeCurve => phosphor!("bezier-curve"),
            Icon::Ruler => phosphor!("ruler"),
            Icon::Gradient => phosphor!("gradient"),
            Icon::Text => phosphor!("text-t"),
            Icon::Layers => phosphor!("stack-simple"),
            Icon::Sliders => phosphor!("sliders-horizontal"),
            Icon::Motion => phosphor!("path"),
            Icon::Timeline => phosphor!("film-strip"),
            Icon::Check => phosphor!("check"),
            Icon::Import => phosphor!("download-simple"),
            Icon::Export => phosphor!("upload-simple"),
            Icon::Duplicate => phosphor!("copy"),
            Icon::NewFolder => phosphor!("folder-simple-plus"),
            Icon::More => phosphor!("dots-three-outline"),
            Icon::LassoDelete => phosphor!("scissors"),
            Icon::SelectAll => phosphor!("selection-all"),
            Icon::SelectInvert => phosphor!("selection-inverse"),
            Icon::Deselect => phosphor!("selection-slash"),
        }
    }

    /// Drawn white, `px` pixels square. `None` if the SVG didn't draw.
    pub(crate) fn render(self, px: u32) -> Option<egui::ColorImage> {
        use resvg::{tiny_skia, usvg};
        let src = self.svg().replace("currentColor", "#ffffff");
        let tree = usvg::Tree::from_str(&src, &usvg::Options::default()).ok()?;
        let px = px.clamp(1, 1024);
        let mut pixmap = tiny_skia::Pixmap::new(px, px)?;
        let scale = px as f32 / tree.size().width().max(tree.size().height());
        resvg::render(
            &tree,
            tiny_skia::Transform::from_scale(scale, scale),
            &mut pixmap.as_mut(),
        );
        // tiny-skia's pixels are premultiplied, as egui's are.
        let pixels = pixmap
            .pixels()
            .iter()
            .map(|p| Color32::from_rgba_premultiplied(p.red(), p.green(), p.blue(), p.alpha()))
            .collect();
        Some(egui::ColorImage {
            size: [px as usize, px as usize],
            pixels,
        })
    }
}

/// Icons uploaded so far, by icon and size in pixels.
type Uploaded = HashMap<(Icon, u32), Option<egui::TextureHandle>>;

fn cache_id() -> egui::Id {
    egui::Id::new("phosphor_icon_textures")
}

/// `icon` drawn `px` pixels square, uploaded once.
fn texture(ctx: &egui::Context, icon: Icon, px: u32) -> Option<egui::TextureHandle> {
    let key = (icon, px);
    let cached = ctx.data(|d| {
        d.get_temp::<Uploaded>(cache_id())
            .and_then(|c| c.get(&key).cloned())
    });
    match cached {
        Some(texture) => texture,
        None => {
            // Uploaded outside `data_mut`: the context is locked in there.
            let texture = icon.render(px).map(|image| {
                ctx.load_texture(
                    format!("icon-{icon:?}-{px}"),
                    image,
                    egui::TextureOptions::LINEAR,
                )
            });
            ctx.data_mut(|d| {
                d.get_temp_mut_or_default::<Uploaded>(cache_id())
                    .insert(key, texture.clone())
            });
            texture
        }
    }
}

/// Paints `icon` into `rect` (as large as fits, centred) using `color`.
pub(crate) fn paint_icon(painter: &egui::Painter, rect: Rect, icon: Icon, color: Color32) {
    let ctx = painter.ctx();
    let ppp = ctx.pixels_per_point();
    let side = rect.width().min(rect.height());
    if side <= 0.0 {
        return;
    }
    let px = (side * ppp).round().max(1.0) as u32;
    let Some(texture) = texture(ctx, icon, px) else {
        return;
    };
    // On whole pixels, so its lines stay sharp.
    let size = egui::Vec2::splat(px as f32 / ppp);
    let min = ((rect.center() - size * 0.5) * ppp).round() / ppp;
    let uv = Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0));
    painter.image(texture.id(), Rect::from_min_size(min, size), uv, color);
}

/// `icon` as an image `side` points square, tinted `color` (for buttons
/// with an icon and text).
pub(crate) fn icon_image(
    ctx: &egui::Context,
    icon: Icon,
    side: f32,
    color: Color32,
) -> Option<egui::Image<'static>> {
    let px = (side * ctx.pixels_per_point()).round().max(1.0) as u32;
    let texture = texture(ctx, icon, px)?;
    Some(
        egui::Image::from_texture(egui::load::SizedTexture::new(
            texture.id(),
            egui::Vec2::splat(side),
        ))
        .tint(color),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_icon_draws_at_small_and_large_sizes() {
        for icon in Icon::ALL {
            for px in [16, 48] {
                let image = icon
                    .render(px)
                    .unwrap_or_else(|| panic!("{icon:?} didn't draw"));
                let covered = image.pixels.iter().filter(|p| p.a() > 0).count();
                assert!(
                    covered > px as usize,
                    "{icon:?} at {px} px is (nearly) empty"
                );
            }
        }
    }

    /// Every icon on one sheet, white on the panel grey at 48 px, to look
    /// over by eye: `cargo test --lib icon_sheet -- --ignored`, then open
    /// `target/icon_sheet.png`.
    #[test]
    #[ignore]
    fn icon_sheet() {
        let (px, gap, cols) = (48_u32, 16_u32, 10_u32);
        let rows = (Icon::ALL.len() as u32).div_ceil(cols);
        let cell = px + gap;
        let bg = crate::ui::style::BG_PANEL;
        let mut sheet = image::RgbaImage::from_pixel(
            cols * cell + gap,
            rows * cell + gap,
            image::Rgba([bg.r(), bg.g(), bg.b(), 255]),
        );
        for (i, icon) in Icon::ALL.into_iter().enumerate() {
            let img = icon.render(px).unwrap();
            let (x0, y0) = (gap + i as u32 % cols * cell, gap + i as u32 / cols * cell);
            for (k, p) in img.pixels.iter().enumerate() {
                let (x, y) = (x0 + k as u32 % px, y0 + k as u32 / px);
                let under = sheet.get_pixel(x, y).0;
                let a = p.a() as u32;
                let mix =
                    |c: u8, over: u8| ((over as u32 + c as u32 * (255 - a) / 255).min(255)) as u8;
                sheet.put_pixel(
                    x,
                    y,
                    image::Rgba([
                        mix(under[0], p.r()),
                        mix(under[1], p.g()),
                        mix(under[2], p.b()),
                        255,
                    ]),
                );
            }
        }
        let out = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("target/icon_sheet.png");
        sheet.save(&out).unwrap();
        println!("{}", out.display());
    }
}
