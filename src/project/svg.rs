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
use crate::canvas::vector::{Paint, PathShape, VectorItem, VectorStroke};
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
                if let Some(fill) = &layer.style.fill {
                    fill_svg(canvas, i, fill, out, defs)?;
                } else if let Some(v) = &layer.vector {
                    let mut ids = 0;
                    items_svg(v.items().iter().map(|i| &**i), out, defs, &mut ids);
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

fn fill_svg(
    canvas: &Canvas,
    idx: usize,
    fill: &LayerFill,
    out: &mut String,
    defs: &mut String,
) -> Result<(), String> {
    use crate::canvas::gradient::{GradientRepeat, GradientShape};
    let (w, h) = (canvas.width(), canvas.height());
    let (colours, shape, start, end, repeat, reverse) = match fill {
        LayerFill::Colour(c) => {
            let _ = writeln!(
                out,
                r#"<rect width="{w}" height="{h}" fill="{}"/>"#,
                hex(*c)
            );
            return Ok(());
        }
        LayerFill::Gradient {
            colours,
            shape,
            start,
            end,
            repeat,
            reverse,
        } => (colours, *shape, *start, *end, *repeat, *reverse),
    };
    // SVG has no conic or diamond gradient: the layer as a picture.
    if matches!(shape, GradientShape::Angle | GradientShape::Diamond) {
        return pixels_svg(canvas, idx, out);
    }
    let gid = format!("fill-{}", canvas.layers[idx].id.0);
    let colours = if reverse {
        colours.reversed()
    } else {
        colours.clone()
    };
    // SVG mixes stops straight in sRGB, with no midpoints or separate
    // opacity: anything else is sampled closely enough to look the same.
    let exact = canvas.blend_space == crate::canvas::blend_modes::BlendSpace::Gamma
        && colours.opacity.is_empty()
        && colours.colors.iter().all(|s| (s.mid - 0.5).abs() < 1e-4);
    let mut points: Vec<(f32, [f32; 4])> = if exact {
        colours
            .colors
            .iter()
            .map(|s| {
                let [r, g, b] = s.color.map(|v| v as f32);
                (s.pos, [r, g, b, 1.0])
            })
            .collect()
    } else {
        (0..=64)
            .map(|i| {
                let t = i as f32 / 64.0;
                (t, colours.eval(t, canvas.blend_space))
            })
            .collect()
    };
    // Reflected: the stops mirrored on both sides of the start, so it stops
    // at the ends (SVG's own reflection would carry on past them).
    let (x1, y1) = if shape == GradientShape::Reflected {
        let mut both: Vec<(f32, [f32; 4])> = points
            .iter()
            .rev()
            .map(|&(t, c)| (0.5 - t / 2.0, c))
            .collect();
        both.extend(points.iter().map(|&(t, c)| (0.5 + t / 2.0, c)));
        points = both;
        (2.0 * start[0] - end[0], 2.0 * start[1] - end[1])
    } else {
        (start[0], start[1])
    };
    let stops: String = points
        .iter()
        .map(|&(t, [r, g, b, a])| {
            let q = |v: f32| v.round().clamp(0.0, 255.0) as u8;
            let opacity = if a < 1.0 {
                format!(r#" stop-opacity="{a:.3}""#)
            } else {
                String::new()
            };
            format!(
                r#"<stop offset="{t:.4}" stop-color="{}"{opacity}/>"#,
                hex([q(r), q(g), q(b)])
            )
        })
        .collect();
    let spread = match repeat {
        GradientRepeat::None => "",
        GradientRepeat::Repeat => r#" spreadMethod="repeat""#,
        GradientRepeat::Mirror => r#" spreadMethod="reflect""#,
    };
    if shape == GradientShape::Radial {
        let r = ((end[0] - start[0]).powi(2) + (end[1] - start[1]).powi(2)).sqrt();
        let _ = writeln!(
            defs,
            r#"<radialGradient id="{gid}" gradientUnits="userSpaceOnUse" cx="{:.2}" cy="{:.2}" r="{r:.2}"{spread}>{stops}</radialGradient>"#,
            start[0], start[1]
        );
    } else {
        let _ = writeln!(
            defs,
            r#"<linearGradient id="{gid}" gradientUnits="userSpaceOnUse" x1="{x1:.2}" y1="{y1:.2}" x2="{:.2}" y2="{:.2}"{spread}>{stops}</linearGradient>"#,
            end[0], end[1],
        );
    }
    let _ = writeln!(
        out,
        r#"<rect width="{w}" height="{h}" fill="url(#{gid})"/>"#
    );
    Ok(())
}

/// A vector layer's items as SVG: shapes as paths (fill, outline, dashes,
/// gradients), groups as groups, brush lines as filled outlines. What SVG
/// doesn't hold as this app does is noted in `data-rp-` attributes, so an
/// import gets it back: a line's points and widths, a shape's markers
/// (drawn as paths of their own, which an import leaves out). `ids`
/// numbers what needs an id.
pub(crate) fn items_svg<'a>(
    items: impl IntoIterator<Item = &'a VectorItem>,
    out: &mut String,
    defs: &mut String,
    ids: &mut usize,
) {
    for item in items {
        *ids += 1;
        let n = *ids;
        match item {
            VectorItem::Line(l) => {
                let path = stroke_svg(l);
                if let Some(rest) = path.strip_prefix("<path ") {
                    let _ = write!(out, r#"<path id="rpl-{n}" data-rp-points=""#);
                    for (i, &[x, y, w]) in l.points.iter().enumerate() {
                        if i > 0 {
                            out.push(' ');
                        }
                        fixed(out, x, 2);
                        out.push(',');
                        fixed(out, y, 2);
                        out.push(',');
                        fixed(out, w, 2);
                    }
                    out.push_str("\" ");
                    out.push_str(rest);
                }
            }
            VectorItem::Shape(s) => shape_svg(s, n, out, defs),
            VectorItem::Group(g) => {
                let mut attrs = format!(r#" id="rpg-{n}""#);
                if !g.name.is_empty() {
                    let _ = write!(attrs, r#" data-name="{}""#, xml(&g.name));
                }
                if g.opacity < 1.0 {
                    let _ = write!(attrs, r#" opacity="{:.3}""#, g.opacity);
                }
                if !g.visible {
                    attrs.push_str(r#" display="none""#);
                }
                let _ = writeln!(out, "<g{attrs}>");
                items_svg(&g.items, out, defs, ids);
                out.push_str("</g>\n");
            }
        }
    }
}

/// A path's outline in SVG's path syntax.
pub(crate) fn path_d(path: &crate::canvas::path::Path) -> String {
    let mut d = String::new();
    for sub in &path.subpaths {
        let Some(first) = sub.knots.first() else {
            continue;
        };
        let point = |d: &mut String, p: eframe::egui::Vec2| {
            fixed(d, p.x, 3);
            d.push(' ');
            fixed(d, p.y, 3);
            d.push(' ');
        };
        d.push('M');
        point(&mut d, first.anchor);
        for [a, b, c, e] in sub.segments() {
            if b == a && c == e {
                d.push('L');
                point(&mut d, e);
            } else {
                d.push('C');
                point(&mut d, b);
                point(&mut d, c);
                point(&mut d, e);
            }
        }
        if sub.closed {
            d.push_str("Z ");
        }
    }
    d.trim_end().to_string()
}

/// A paint as an SVG paint (a gradient put in `defs` as `id`): the value,
/// and its opacity when it has one.
fn paint_svg(paint: &Paint, id: &str, defs: &mut String) -> (String, Option<f32>) {
    use crate::canvas::gradient::{GradientRepeat, GradientShape};
    match paint {
        Paint::Colour([r, g, b, a]) => (hex([*r, *g, *b]), (*a < 255).then(|| *a as f32 / 255.0)),
        Paint::Gradient(g) => {
            let stops: String = (g.stops.colors.iter())
                .map(|s| {
                    format!(
                        r#"<stop offset="{:.4}" stop-color="{}"/>"#,
                        s.pos,
                        hex(s.color)
                    )
                })
                .collect();
            let spread = match g.repeat {
                GradientRepeat::None => "",
                GradientRepeat::Repeat => r#" spreadMethod="repeat""#,
                GradientRepeat::Mirror => r#" spreadMethod="reflect""#,
            };
            let transform = g.transform.map_or(String::new(), |[a, b, c, d, e, f]| {
                format!(r#" gradientTransform="matrix({a} {b} {c} {d} {e} {f})""#)
            });
            let ([x1, y1], [x2, y2]) = (g.start, g.end);
            // SVG has no conic, diamond or mirrored-about-the-start
            // gradient: the nearest it has.
            if matches!(
                g.shape,
                GradientShape::Radial | GradientShape::Diamond | GradientShape::Angle
            ) {
                let r = ((x2 - x1).powi(2) + (y2 - y1).powi(2)).sqrt();
                let _ = writeln!(
                    defs,
                    r#"<radialGradient id="{id}" gradientUnits="userSpaceOnUse" cx="{x1:.2}" cy="{y1:.2}" r="{r:.2}"{spread}{transform}>{stops}</radialGradient>"#
                );
            } else {
                let _ = writeln!(
                    defs,
                    r#"<linearGradient id="{id}" gradientUnits="userSpaceOnUse" x1="{x1:.2}" y1="{y1:.2}" x2="{x2:.2}" y2="{y2:.2}"{spread}{transform}>{stops}</linearGradient>"#
                );
            }
            (format!("url(#{id})"), None)
        }
    }
}

fn marker_name(m: crate::canvas::raster::marker::Marker) -> &'static str {
    use crate::canvas::raster::marker::Marker;
    match m {
        Marker::None => "none",
        Marker::Arrow => "arrow",
        Marker::OpenArrow => "open-arrow",
        Marker::Circle => "circle",
        Marker::Square => "square",
        Marker::Bar => "bar",
    }
}

fn shape_svg(s: &PathShape, n: usize, out: &mut String, defs: &mut String) {
    use crate::canvas::raster::marker::Marker;
    use crate::canvas::raster::stroke::{Cap, Join};
    let mut attrs = format!(r#" id="rps-{n}" d="{}""#, path_d(&s.path));
    match &s.fill {
        Some(f) => {
            let (paint, opacity) = paint_svg(&f.paint, &format!("rpf-{n}"), defs);
            let _ = write!(attrs, r#" fill="{paint}""#);
            if let Some(o) = opacity {
                let _ = write!(attrs, r#" fill-opacity="{o:.3}""#);
            }
            if f.rule == crate::canvas::raster::FillRule::EvenOdd {
                attrs.push_str(r#" fill-rule="evenodd""#);
            }
        }
        None => attrs.push_str(r#" fill="none""#),
    }
    if let Some(o) = &s.stroke {
        let (paint, opacity) = paint_svg(&o.paint, &format!("rpo-{n}"), defs);
        let _ = write!(attrs, r#" stroke="{paint}" stroke-width="{:.3}""#, o.width);
        if let Some(op) = opacity {
            let _ = write!(attrs, r#" stroke-opacity="{op:.3}""#);
        }
        let cap = match o.cap {
            Cap::Butt => "butt",
            Cap::Round => "round",
            Cap::Square => "square",
        };
        let join = match o.join {
            Join::Miter => "miter",
            Join::Round => "round",
            Join::Bevel => "bevel",
        };
        let _ = write!(
            attrs,
            r#" stroke-linecap="{cap}" stroke-linejoin="{join}" stroke-miterlimit="{:.3}""#,
            o.miter_limit
        );
        if !o.dash.is_empty() {
            let dash: Vec<String> = o.dash.iter().map(|d| format!("{d:.3}")).collect();
            let _ = write!(attrs, r#" stroke-dasharray="{}""#, dash.join(" "));
            if o.dash_offset != 0.0 {
                let _ = write!(attrs, r#" stroke-dashoffset="{:.3}""#, o.dash_offset);
            }
        }
        if o.start != Marker::None || o.end != Marker::None {
            let _ = write!(
                attrs,
                r#" data-rp-markers="{} {}""#,
                marker_name(o.start),
                marker_name(o.end)
            );
        }
    }
    if s.opacity < 1.0 {
        let _ = write!(attrs, r#" opacity="{:.3}""#, s.opacity);
    }
    let _ = writeln!(out, "<path{attrs}/>");
    // The markers, as SVG viewers will show them: shapes of their own.
    if let Some(o) = s
        .stroke
        .as_ref()
        .filter(|o| o.start != Marker::None || o.end != Marker::None)
    {
        let mut polys: Vec<Vec<eframe::egui::Vec2>> = Vec::new();
        for (pts, closed) in s.flattened() {
            if closed || pts.len() < 2 {
                continue;
            }
            let n = pts.len();
            let (a, b) = (
                (pts[1] - pts[0]).normalized(),
                (pts[n - 1] - pts[n - 2]).normalized(),
            );
            let mut one = |m: Marker, at, dir| {
                let mut e = crate::canvas::raster::Edges::recording();
                m.add(&mut e, at, dir, o.width);
                polys.extend(e.polygons().iter().cloned());
            };
            one(o.start, pts[0], -a);
            one(o.end, pts[n - 1], b);
        }
        if !polys.is_empty() {
            let mut d = String::new();
            for p in polys {
                for (i, q) in p.iter().enumerate() {
                    let _ = write!(
                        d,
                        "{}{:.2} {:.2} ",
                        if i == 0 { "M" } else { "L" },
                        q.x,
                        q.y
                    );
                }
                d.push_str("Z ");
            }
            let (paint, opacity) = paint_svg(&o.paint, &format!("rpm-paint-{n}"), defs);
            let op = opacity.map_or(String::new(), |o| format!(r#" fill-opacity="{o:.3}""#));
            let _ = writeln!(
                out,
                r#"<path id="rpm-{n}" d="{}" fill="{paint}"{op}/>"#,
                d.trim_end()
            );
        }
    }
}

/// `v` written to `places` decimals (as `{:.places$}` writes it, but
/// without the formatter's exact float printing, which made most of an
/// export's time): rounded once, the integer then written out.
fn fixed(out: &mut String, v: f32, places: u32) {
    let scale = 10i64.pow(places);
    // Exact (an f32 times 10³ fits an f64), so this rounds as `{:.3}` does.
    let scaled = (v as f64 * scale as f64).round_ties_even();
    if !scaled.is_finite() || scaled.abs() >= 1e15 {
        let _ = write!(out, "{v:.*}", places as usize);
        return;
    }
    let n = scaled as i64;
    if n < 0 {
        out.push('-');
    }
    let n = n.unsigned_abs();
    let (int, frac) = (n / scale as u64, n % scale as u64);
    let _ = match places {
        0 => write!(out, "{int}"),
        _ => write!(out, "{int}.{frac:0width$}", width = places as usize),
    };
}

/// A vector line as a filled outline: its width at every point kept, round
/// at the ends.
pub(crate) fn stroke_svg(stroke: &VectorStroke) -> String {
    let outline = stroke_outline(stroke);
    if outline.is_empty() {
        return String::new();
    }
    let mut d = String::with_capacity(outline.len() * 16);
    for (i, &[x, y]) in outline.iter().enumerate() {
        d.push(if i == 0 { 'M' } else { 'L' });
        fixed(&mut d, x, 2);
        d.push(' ');
        fixed(&mut d, y, 2);
        d.push(' ');
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
    fn numbers_are_written_as_the_formatter_writes_them() {
        for v in [
            0.0f32, 1.0, -1.0, 0.125, 2.345_678, -2.345_678, 1234.5, 3999.996, -0.7, 0.004,
            123_456.79,
        ] {
            for places in [2u32, 3] {
                let mut s = String::new();
                fixed(&mut s, v, places);
                let p = places as usize;
                assert_eq!(s, format!("{v:.p$}"), "{v} {places}");
            }
        }
    }

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
        canvas.layers[vi].vector = Some(Box::new(VectorLayer::from_strokes(vec![VectorStroke {
            points: vec![[5.0, 5.0, 2.0], [40.0, 20.0, 4.0]],
            colour: [0, 128, 255],
            opacity: 0.5,
        }])));
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
