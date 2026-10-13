//! SVG export, layer by layer, so the file stays editable in a vector
//! editor: vector layers become filled outlines (their pressure kept as
//! width), colour and gradient fill layers become rectangles, folders
//! groups, and paint layers embedded PNG pictures cropped to their paint
//! (their mask applied). Opacity, visibility and the blend modes CSS has go
//! across; clipping, adjustment layers and borders don't (the SVG notes
//! it).

use crate::canvas::Canvas;
use crate::canvas::blend::Unmultiply;
use crate::canvas::blend_modes::LayerBlend;
use crate::canvas::layer_style::LayerFill;
use crate::canvas::storage::{LayerId, LayerKind};
use crate::canvas::vector::VectorStroke;
use eframe::egui::Color32;
use std::fmt::Write;

/// The document as SVG text.
pub fn document_svg(canvas: &Canvas) -> Result<String, String> {
    let (w, h) = (canvas.width(), canvas.height());
    let mut out = String::new();
    let _ = writeln!(out, r#"<?xml version="1.0" encoding="UTF-8"?>"#);
    let _ = writeln!(
        out,
        r#"<svg xmlns="http://www.w3.org/2000/svg" xmlns:xlink="http://www.w3.org/1999/xlink" width="{w}" height="{h}" viewBox="0 0 {w} {h}">"#
    );
    let _ = writeln!(out, "<title>Rusty Painter export</title>");
    let unsupported = canvas
        .layers
        .iter()
        .any(|l| l.clipped || l.adjustment.is_some() || l.style.border.is_some());
    if unsupported {
        let _ = writeln!(
            out,
            "<!-- Clipping, adjustment layers and borders have no SVG form here: \
             export a PNG for the exact picture. -->"
        );
    }
    let mut defs = String::new();
    let mut body = String::new();
    children(canvas, None, 1, &mut body, &mut defs)?;
    if !defs.is_empty() {
        let _ = writeln!(out, "<defs>\n{defs}</defs>");
    }
    out.push_str(&body);
    out.push_str("</svg>\n");
    Ok(out)
}

/// The layers in `parent`, bottom first.
fn children(
    canvas: &Canvas,
    parent: Option<LayerId>,
    depth: usize,
    out: &mut String,
    defs: &mut String,
) -> Result<(), String> {
    if depth > canvas.layers.len() + 1 {
        return Ok(());
    }
    for (i, layer) in canvas.layers.iter().enumerate() {
        if layer.parent != parent || matches!(layer.kind, LayerKind::Mask { .. }) || layer.draft {
            continue;
        }
        let mut attrs = format!(
            r#" id="layer-{}" data-name="{}""#,
            layer.id.0,
            xml(&layer.name)
        );
        if layer.opacity < 1.0 {
            let _ = write!(attrs, r#" opacity="{:.3}""#, layer.opacity);
        }
        if !layer.visible {
            attrs.push_str(r#" display="none""#);
        }
        if let Some(mode) = css_blend(layer.blend) {
            let _ = write!(attrs, r#" style="mix-blend-mode:{mode}""#);
        }
        let _ = writeln!(out, "<g{attrs}>");
        match layer.kind {
            LayerKind::Group => children(canvas, Some(layer.id), depth + 1, out, defs)?,
            _ if layer.adjustment.is_some() => {}
            _ => {
                if let Some(fill) = layer.style.fill {
                    fill_svg(canvas, layer.id, fill, out, defs);
                } else if let Some(v) = &layer.vector {
                    for s in &v.strokes {
                        out.push_str(&stroke_svg(s));
                    }
                } else {
                    // The background shows its colour where it's unpainted.
                    if i == 0 {
                        let [r, g, b, _] = canvas.clear_color().unmultiplied();
                        let (w, h) = (canvas.width(), canvas.height());
                        let _ = writeln!(
                            out,
                            r#"<rect width="{w}" height="{h}" fill="{}"/>"#,
                            hex([r, g, b])
                        );
                    }
                    pixels_svg(canvas, i, out)?;
                }
            }
        }
        out.push_str("</g>\n");
    }
    Ok(())
}

/// A blend mode's CSS name, where CSS has it.
fn css_blend(blend: LayerBlend) -> Option<&'static str> {
    Some(match blend {
        LayerBlend::Multiply => "multiply",
        LayerBlend::Screen => "screen",
        LayerBlend::Overlay => "overlay",
        LayerBlend::Darken => "darken",
        LayerBlend::Lighten => "lighten",
        LayerBlend::ColorDodge => "color-dodge",
        LayerBlend::ColorBurn => "color-burn",
        LayerBlend::HardLight => "hard-light",
        LayerBlend::SoftLight => "soft-light",
        LayerBlend::Difference => "difference",
        LayerBlend::Exclusion => "exclusion",
        LayerBlend::Hue => "hue",
        LayerBlend::Saturation => "saturation",
        LayerBlend::Color => "color",
        LayerBlend::Luminosity => "luminosity",
        _ => return None,
    })
}

fn xml(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

fn hex([r, g, b]: [u8; 3]) -> String {
    format!("#{r:02x}{g:02x}{b:02x}")
}

fn fill_svg(canvas: &Canvas, id: LayerId, fill: LayerFill, out: &mut String, defs: &mut String) {
    let (w, h) = (canvas.width(), canvas.height());
    match fill {
        LayerFill::Colour(c) => {
            let _ = writeln!(out, r#"<rect width="{w}" height="{h}" fill="{}"/>"#, hex(c));
        }
        LayerFill::Gradient {
            colours,
            shape,
            start,
            end,
        } => {
            use crate::canvas::gradient::GradientShape;
            let gid = format!("fill-{}", id.0);
            let stops: String = colours
                .stops()
                .iter()
                .map(|&(pos, c)| format!(r#"<stop offset="{pos:.4}" stop-color="{}"/>"#, hex(c)))
                .collect();
            match shape {
                GradientShape::Radial => {
                    let r = ((end[0] - start[0]).powi(2) + (end[1] - start[1]).powi(2)).sqrt();
                    let _ = writeln!(
                        defs,
                        r#"<radialGradient id="{gid}" gradientUnits="userSpaceOnUse" cx="{:.2}" cy="{:.2}" r="{r:.2}">{stops}</radialGradient>"#,
                        start[0], start[1]
                    );
                }
                // SVG has no reflected or conic gradient: linear ones stand in.
                _ => {
                    let _ = writeln!(
                        defs,
                        r#"<linearGradient id="{gid}" gradientUnits="userSpaceOnUse" x1="{:.2}" y1="{:.2}" x2="{:.2}" y2="{:.2}"{}>{stops}</linearGradient>"#,
                        start[0],
                        start[1],
                        end[0],
                        end[1],
                        if shape == GradientShape::Reflected {
                            r#" spreadMethod="reflect""#
                        } else {
                            ""
                        }
                    );
                }
            }
            let _ = writeln!(
                out,
                r#"<rect width="{w}" height="{h}" fill="url(#{gid})"/>"#
            );
        }
    }
}

/// A vector line as a filled outline: its width at every point kept, round
/// at the ends.
pub(crate) fn stroke_svg(stroke: &VectorStroke) -> String {
    let outline = stroke_outline(stroke);
    if outline.is_empty() {
        return String::new();
    }
    let mut d = String::new();
    for (i, [x, y]) in outline.iter().enumerate() {
        let _ = write!(d, "{}{x:.2} {y:.2} ", if i == 0 { "M" } else { "L" });
    }
    d.push('Z');
    let opacity = if stroke.opacity < 1.0 {
        format!(r#" fill-opacity="{:.3}""#, stroke.opacity)
    } else {
        String::new()
    };
    format!(
        "<path d=\"{d}\" fill=\"{}\"{opacity}/>\n",
        hex(stroke.colour)
    )
}

/// The outline of a line of varying width: along one side, round the end,
/// back along the other, round the start.
fn stroke_outline(stroke: &VectorStroke) -> Vec<[f32; 2]> {
    let path = stroke.smoothed();
    let Some(&first) = path.first() else {
        return Vec::new();
    };
    let arc = |c: [f32; 3], from: f32, out: &mut Vec<[f32; 2]>| {
        let r = c[2] * 0.5;
        // Round the outside: the way the line doesn't go.
        for k in 0..=8 {
            let a = from - std::f32::consts::PI * k as f32 / 8.0;
            out.push([c[0] + a.cos() * r, c[1] + a.sin() * r]);
        }
    };
    if path.len() == 1 {
        let mut out = Vec::new();
        arc(first, 0.0, &mut out);
        arc(first, std::f32::consts::PI, &mut out);
        return out;
    }
    let n = path.len();
    let normal = |i: usize| {
        let (a, b) = (path[i.saturating_sub(1)], path[(i + 1).min(n - 1)]);
        let (dx, dy) = (b[0] - a[0], b[1] - a[1]);
        let len = (dx * dx + dy * dy).sqrt().max(1e-6);
        [-dy / len, dx / len]
    };
    let side = |i: usize, sign: f32| {
        let [nx, ny] = normal(i);
        let r = path[i][2] * 0.5 * sign;
        [path[i][0] + nx * r, path[i][1] + ny * r]
    };
    let mut out: Vec<[f32; 2]> = (0..n).map(|i| side(i, 1.0)).collect();
    // Round the end: from the left side over to the right.
    let [nx, ny] = normal(n - 1);
    arc(path[n - 1], ny.atan2(nx), &mut out);
    out.extend((0..n).rev().map(|i| side(i, -1.0)));
    let [nx, ny] = normal(0);
    arc(path[0], (-ny).atan2(-nx), &mut out);
    out
}

/// A paint layer as a PNG cropped to its paint, its mask applied.
fn pixels_svg(canvas: &Canvas, idx: usize, out: &mut String) -> Result<(), String> {
    let (w, h) = (canvas.width(), canvas.height());
    let mut px = canvas.render_reference(Some(idx), 0, 0, w, h);
    if let Some(m) = canvas.mask_index_of(canvas.layers[idx].id)
        && canvas.layers[m].visible
    {
        apply_mask(canvas, m, &mut px, w);
    }
    // The painted part only.
    let (mut x0, mut y0, mut x1, mut y1) = (w, h, 0, 0);
    for (i, c) in px.iter().enumerate() {
        if c.a() > 0 {
            let (x, y) = (i % w, i / w);
            x0 = x0.min(x);
            y0 = y0.min(y);
            x1 = x1.max(x + 1);
            y1 = y1.max(y + 1);
        }
    }
    if x0 >= x1 {
        return Ok(());
    }
    let (cw, ch) = (x1 - x0, y1 - y0);
    let mut crop = eframe::egui::ColorImage::new([cw, ch], Color32::TRANSPARENT);
    for y in 0..ch {
        crop.pixels[y * cw..(y + 1) * cw]
            .copy_from_slice(&px[(y0 + y) * w + x0..(y0 + y) * w + x1]);
    }
    let png = crate::project::export::encode_color_image(
        crop,
        crate::project::export::ExportFormat::Png,
    )?;
    let _ = writeln!(
        out,
        r#"<image x="{x0}" y="{y0}" width="{cw}" height="{ch}" xlink:href="data:image/png;base64,{}"/>"#,
        base64(&png)
    );
    Ok(())
}

/// `px` (the whole canvas, row-major) times mask layer `m`'s coverage
/// (missing tiles show everything).
fn apply_mask(canvas: &Canvas, m: usize, px: &mut [Color32], w: usize) {
    let ts = canvas.tile_size();
    for (i, c) in px.iter_mut().enumerate() {
        if c.a() == 0 {
            continue;
        }
        let (x, y) = (i % w, i / w);
        let Some(tile) = canvas.get_layer_tile_data(m, (x / ts) as i32, (y / ts) as i32) else {
            continue;
        };
        let mp = tile[(y % ts) * ts + x % ts];
        let k = (mp.r() as u32 + mp.g() as u32 + mp.b() as u32) as f32 / (3.0 * 255.0);
        let scale = |v: u8| (v as f32 * k).round() as u8;
        *c = Color32::from_rgba_premultiplied(
            scale(c.r()),
            scale(c.g()),
            scale(c.b()),
            scale(c.a()),
        );
    }
}

/// Standard base64 (for the embedded pictures).
fn base64(bytes: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b = [
            chunk[0],
            *chunk.get(1).unwrap_or(&0),
            *chunk.get(2).unwrap_or(&0),
        ];
        let n = (b[0] as u32) << 16 | (b[1] as u32) << 8 | b[2] as u32;
        for k in 0..4 {
            if k <= chunk.len() {
                out.push(TABLE[(n >> (18 - 6 * k) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_matches_the_standard() {
        assert_eq!(base64(b""), "");
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foo"), "Zm9v");
        assert_eq!(base64(b"foobar"), "Zm9vYmFy");
    }

    #[test]
    fn a_line_outline_is_as_wide_as_the_line() {
        let s = VectorStroke {
            points: vec![[10.0, 20.0, 6.0], [50.0, 20.0, 6.0]],
            colour: [0, 0, 0],
            opacity: 1.0,
        };
        let o = stroke_outline(&s);
        let (min_y, max_y) = o
            .iter()
            .fold((f32::MAX, f32::MIN), |(a, b), p| (a.min(p[1]), b.max(p[1])));
        assert!(
            (min_y - 17.0).abs() < 0.01 && (max_y - 23.0).abs() < 0.01,
            "{min_y} {max_y}"
        );
        let (min_x, max_x) = o
            .iter()
            .fold((f32::MAX, f32::MIN), |(a, b), p| (a.min(p[0]), b.max(p[0])));
        assert!(
            (min_x - 7.0).abs() < 0.01 && (max_x - 53.0).abs() < 0.01,
            "round caps: {min_x} {max_x}"
        );
    }

    #[test]
    fn a_document_becomes_layered_svg() {
        use crate::canvas::vector::VectorLayer;
        let mut canvas = Canvas::new(64, 32, Color32::WHITE, 64);
        canvas.set_layer_tile_data(1, 0, 0, {
            let mut t = vec![Color32::TRANSPARENT; 64 * 64];
            t[10 * 64 + 10] = Color32::RED;
            t
        });
        canvas.layers[1].name = "Ink & <paint>".into();
        let v = canvas.insert_new_layer(2, "Lines".into(), LayerKind::Paint, None);
        let vi = canvas.layer_index_of(v).unwrap();
        canvas.layers[vi].vector = Some(Box::new(VectorLayer {
            strokes: vec![VectorStroke {
                points: vec![[5.0, 5.0, 2.0], [40.0, 20.0, 4.0]],
                colour: [0, 128, 255],
                opacity: 0.5,
            }],
        }));
        canvas.layers[vi].opacity = 0.75;
        canvas.layers[vi].blend = LayerBlend::Multiply;
        let svg = document_svg(&canvas).unwrap();
        assert!(svg.starts_with("<?xml"));
        assert!(svg.contains(r#"width="64" height="32""#));
        assert!(svg.contains("Ink &amp; &lt;paint&gt;"), "names escaped");
        assert!(
            svg.contains(r#"<image x="10" y="10" width="1" height="1""#),
            "cropped paint"
        );
        assert!(
            svg.contains(r##"fill="#0080ff" fill-opacity="0.500""##),
            "the line"
        );
        assert!(svg.contains(r#"opacity="0.750""#) && svg.contains("mix-blend-mode:multiply"));
        // Well formed enough: every group closed, one root.
        assert_eq!(svg.matches("<g").count(), svg.matches("</g>").count());
        assert!(svg.trim_end().ends_with("</svg>"));
    }

    #[test]
    fn the_svg_parses_and_its_pictures_hold_the_layer_exactly() {
        let mut canvas = Canvas::new(100, 70, Color32::WHITE, 64);
        // Paint across two tiles, with half-transparent pixels.
        for (tx, ty) in [(0, 0), (1, 0), (0, 1), (1, 1)] {
            let t: Vec<Color32> = (0..64 * 64)
                .map(|i| {
                    let (x, y) = (tx * 64 + i % 64, ty * 64 + i / 64);
                    if (20..90).contains(&x) && (15..60).contains(&y) {
                        Color32::from_rgba_unmultiplied(x as u8 * 2, y as u8 * 3, 90, 60 + x as u8)
                    } else {
                        Color32::TRANSPARENT
                    }
                })
                .collect();
            canvas.set_layer_tile_data(1, tx, ty, t);
        }
        let svg = document_svg(&canvas).unwrap();
        let root = crate::brush_engine::import::krita::parse_xml(&svg).expect("well-formed XML");
        let image = root.find("image").expect("the paint as a picture");
        let href = image.attr("xlink:href").unwrap();
        let data = href.strip_prefix("data:image/png;base64,").unwrap();
        // Undo the base64.
        let alphabet = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        let mut bits = 0u32;
        let mut n = 0;
        let mut png = Vec::new();
        for c in data.bytes().filter(|&c| c != b'=') {
            bits = bits << 6 | alphabet.iter().position(|&a| a == c).unwrap() as u32;
            n += 6;
            if n >= 8 {
                n -= 8;
                png.push((bits >> n) as u8);
            }
        }
        let img = image::load_from_memory(&png).unwrap().to_rgba8();
        let at = |k: &str| image.attr(k).unwrap().parse::<u32>().unwrap();
        assert_eq!(
            (at("x"), at("y"), at("width"), at("height")),
            (20, 15, 70, 45)
        );
        assert_eq!((img.width(), img.height()), (70, 45));
        canvas.layers[0].visible = false;
        let flat = crate::project::export::to_rgba_image(canvas.flatten()).unwrap();
        for y in 0..45 {
            for x in 0..70 {
                let (a, b) = (img.get_pixel(x, y), flat.get_pixel(20 + x, 15 + y));
                let off = (0..4)
                    .map(|c| (a[c] as i32 - b[c] as i32).abs())
                    .max()
                    .unwrap();
                assert!(off <= 1, "{:?} vs {:?} at {x},{y}", a, b);
            }
        }
    }
}
