//! SVG into a vector layer: paths become shapes (fill, outline, dashes,
//! ends, corners, gradients as SVG has them), groups groups, and what this
//! app wrote back what it was (brush lines with their widths, shapes'
//! markers). usvg reads the file (styles, `use`, units, transforms); what
//! it hands over is turned into this app's items, transforms applied to
//! the points. Text, pictures, clipping, masks, filters and patterns have
//! no form here: they're left out, and counted.

use crate::canvas::gradient::{ColorStop, GradientRepeat, GradientShape, GradientStops};
use crate::canvas::path::{Knot, Path, Subpath};
use crate::canvas::raster::FillRule;
use crate::canvas::raster::marker::Marker;
use crate::canvas::raster::stroke::{Cap, Join};
use crate::canvas::vector::{
    Fill, Paint, PaintGradient, PathShape, StrokeStyle, VectorGroup, VectorItem, VectorLayer,
    VectorStroke,
};
use eframe::egui::Vec2;
use resvg::usvg;
use std::collections::HashMap;

/// What an import couldn't bring across.
#[derive(Debug, Default, PartialEq)]
pub struct ImportReport {
    /// Things left out (text, pictures, ...).
    pub left_out: usize,
    /// Things brought across approximately (a pattern as a colour, an
    /// outline stretched one way as one width, an effect dropped).
    pub approximated: usize,
}

impl ImportReport {
    /// A line for the user, if anything's to be said.
    pub fn message(&self) -> Option<String> {
        match (self.left_out, self.approximated) {
            (0, 0) => None,
            (l, 0) => Some(format!(
                "{l} part(s) of the SVG have no vector form here and were left out"
            )),
            (0, a) => Some(format!(
                "{a} part(s) of the SVG were brought across approximately"
            )),
            (l, a) => Some(format!(
                "{l} part(s) of the SVG were left out, {a} approximated"
            )),
        }
    }
}

/// An SVG document's drawing: the layer, its size in pixels, and what
/// didn't come across.
pub fn import_svg(text: &str) -> Result<(VectorLayer, [u32; 2], ImportReport), String> {
    let tree = usvg::Tree::from_str(text, &usvg::Options::default())
        .map_err(|e| format!("Not an SVG this can read: {e}"))?;
    let extra = written_by_us(text);
    // (Read without fonts, usvg drops text before it's seen: counted here.)
    let mut report = ImportReport {
        left_out: extra.text,
        ..Default::default()
    };
    let mut items = Vec::new();
    group_items(tree.root(), &extra, &mut items, &mut report);
    let size = tree.size();
    Ok((
        VectorLayer::from_items(items),
        [
            size.width().ceil().max(1.0) as u32,
            size.height().ceil().max(1.0) as u32,
        ],
        report,
    ))
}

/// An SVG file as a document: its size, its drawing as vector layers and
/// its pictures (`<image>`, as this app's own SVG keeps paint layers) as
/// paint layers, in their order. When something has no form here (text,
/// clipping, masks, filters), the whole picture comes too, as a hidden top
/// layer, and the user hears what was left out.
pub fn decode_svg(bytes: &[u8]) -> Result<crate::project::psd::PsdDocument, String> {
    use crate::project::psd::PsdLayer;
    let text = std::str::from_utf8(bytes).map_err(|_| "The SVG isn't UTF-8 text")?;
    let tree = usvg::Tree::from_str(text, &usvg::Options::default())
        .map_err(|e| format!("Not an SVG this can read: {e}"))?;
    let size = tree.size();
    let (w, h) = (
        size.width().ceil().max(1.0) as usize,
        size.height().ceil().max(1.0) as usize,
    );
    crate::app::document::validate_canvas_size(w, h)?;
    let ours = written_by_us(text);
    let mut report = ImportReport {
        left_out: ours.text,
        ..Default::default()
    };
    let full = [0, 0, w as i32, h as i32];
    let picture_layer = |name: &str, pixels| PsdLayer {
        vector: None,
        ..vector_layer(name, full, pixels, VectorLayer::default())
    };
    let mut layers = Vec::new();
    // Vector items in a row become one vector layer.
    let mut run = Vec::new();
    let flush = |run: &mut Vec<VectorItem>, layers: &mut Vec<PsdLayer>| {
        if !run.is_empty() {
            let layer = VectorLayer::from_items(std::mem::take(run));
            let pixels = crate::canvas::vector::render_region(layer.items(), full);
            layers.push(vector_layer("Vector", full, pixels, layer));
        }
    };
    // Each top-level element in turn (this app's SVG: one per layer).
    for node in tree.root().children() {
        let mut images = Vec::new();
        node_items(node, &ours, &mut run, &mut report, Some(&mut images), 1.0);
        if images.is_empty() {
            continue;
        }
        flush(&mut run, &mut layers);
        let name = ours.names.get(node.id()).map_or("Picture", String::as_str);
        for (image, opacity) in images {
            match image_pixels(&image, w, h) {
                Some(pixels) => {
                    let mut layer = picture_layer(name, pixels);
                    layer.opacity = opacity;
                    layers.push(layer);
                }
                None => report.left_out += 1,
            }
        }
    }
    flush(&mut run, &mut layers);
    if layers.is_empty() {
        layers.push(vector_layer(
            "Vector",
            full,
            Vec::new(),
            VectorLayer::default(),
        ));
    }
    let mut notes = Vec::new();
    if let Some(message) = report.message() {
        // (Drawn without fonts or embedded pictures: text isn't in it, and
        // the pictures are layers of their own.)
        let beyond_text = report.approximated > 0 || report.left_out > ours.text;
        notes.push(
            match beyond_text.then(|| whole_picture(&tree, w, h)).flatten() {
                Some(pixels) => {
                    let mut layer = picture_layer("SVG picture (parts not brought across)", pixels);
                    layer.visible = false;
                    layers.push(layer);
                    format!("{message}: the whole picture was added as a hidden top layer")
                }
                None => message,
            },
        );
    }
    Ok(crate::project::psd::PsdDocument {
        width: w,
        height: h,
        layers,
        composite: Vec::new(),
        notes,
        depth: Default::default(),
        composite_deep: None,
        profile: Default::default(),
    })
}

/// A tiny-skia picture's pixels (premultiplied).
fn pixmap_pixels(pixmap: &resvg::tiny_skia::Pixmap) -> Vec<eframe::egui::Color32> {
    (pixmap.pixels().iter())
        .map(|p| {
            eframe::egui::Color32::from_rgba_premultiplied(p.red(), p.green(), p.blue(), p.alpha())
        })
        .collect()
}

/// `tree` drawn whole at `w`×`h`.
fn whole_picture(tree: &usvg::Tree, w: usize, h: usize) -> Option<Vec<eframe::egui::Color32>> {
    let mut pixmap = resvg::tiny_skia::Pixmap::new(w as u32, h as u32)?;
    resvg::render(
        tree,
        resvg::tiny_skia::Transform::identity(),
        &mut pixmap.as_mut(),
    );
    Some(pixmap_pixels(&pixmap))
}

/// An `<image>` drawn where it sits on a `w`×`h` canvas, premultiplied;
/// `None` if its picture can't be read.
fn image_pixels(image: &usvg::Image, w: usize, h: usize) -> Option<Vec<eframe::egui::Color32>> {
    use eframe::egui::Color32;
    use resvg::tiny_skia;
    if !image.is_visible() {
        return None;
    }
    let size = image.size();
    let data = match image.kind() {
        usvg::ImageKind::PNG(d)
        | usvg::ImageKind::JPEG(d)
        | usvg::ImageKind::GIF(d)
        | usvg::ImageKind::WEBP(d) => d,
        usvg::ImageKind::SVG(tree) => {
            let mut pixmap = tiny_skia::Pixmap::new(w as u32, h as u32)?;
            let t = image.abs_transform().pre_scale(
                size.width() / tree.size().width(),
                size.height() / tree.size().height(),
            );
            resvg::render(tree, t, &mut pixmap.as_mut());
            return Some(pixmap_pixels(&pixmap));
        }
    };
    let img = image::load_from_memory(data).ok()?.to_rgba8();
    let (iw, ih) = (img.width(), img.height());
    if iw == 0 || ih == 0 {
        return None;
    }
    let t = image
        .abs_transform()
        .pre_scale(size.width() / iw as f32, size.height() / ih as f32);
    // Pixel for pixel where it sits on whole pixels (this app's own
    // pictures): copied as they are, not resampled.
    let whole = |v: f32| (v - v.round()).abs() < 1e-4;
    if (t.sx - 1.0).abs() < 1e-4
        && (t.sy - 1.0).abs() < 1e-4
        && t.kx == 0.0
        && t.ky == 0.0
        && whole(t.tx)
        && whole(t.ty)
    {
        let mut out = vec![Color32::TRANSPARENT; w * h];
        let (ox, oy) = (t.tx.round() as i64, t.ty.round() as i64);
        for (y, row) in img.rows().enumerate() {
            let cy = oy + y as i64;
            if cy < 0 || cy >= h as i64 {
                continue;
            }
            for (x, p) in row.enumerate() {
                let cx = ox + x as i64;
                if cx >= 0 && cx < w as i64 {
                    out[cy as usize * w + cx as usize] =
                        Color32::from_rgba_unmultiplied(p[0], p[1], p[2], p[3]);
                }
            }
        }
        return Some(out);
    }
    let premultiplied: Vec<u8> = (img.pixels())
        .flat_map(|p| Color32::from_rgba_unmultiplied(p[0], p[1], p[2], p[3]).to_array())
        .collect();
    let src = tiny_skia::Pixmap::from_vec(premultiplied, tiny_skia::IntSize::from_wh(iw, ih)?)?;
    let mut pixmap = tiny_skia::Pixmap::new(w as u32, h as u32)?;
    let quality = match image.rendering_mode() {
        usvg::ImageRendering::OptimizeSpeed => tiny_skia::FilterQuality::Nearest,
        _ => tiny_skia::FilterQuality::Bicubic,
    };
    let paint = tiny_skia::PixmapPaint {
        quality,
        ..Default::default()
    };
    pixmap.draw_pixmap(0, 0, src.as_ref(), &paint, t, None);
    Some(pixmap_pixels(&pixmap))
}

/// An imported document's layer holding `layer` (its pixels `pixels`,
/// over `rect`).
pub(crate) fn vector_layer(
    name: &str,
    rect: [i32; 4],
    pixels: Vec<eframe::egui::Color32>,
    layer: VectorLayer,
) -> crate::project::psd::PsdLayer {
    crate::project::psd::PsdLayer {
        name: name.into(),
        kind: crate::project::psd::PsdKind::Pixels,
        rect,
        pixels,
        deep: None,
        opacity: 1.0,
        visible: true,
        blend: Default::default(),
        clipped: false,
        alpha_locked: false,
        position_locked: false,
        mask: None,
        vector: Some(Box::new(layer)),
        fill: None,
    }
}

/// An SVG's root drawn at `width` × `height` pixels whatever its own size
/// says (its viewBox scaled to fit): a Krita vector layer's, whose units
/// are points.
pub fn with_size(text: &str, width: usize, height: usize) -> String {
    let Some(start) = text.find("<svg") else {
        return text.to_string();
    };
    let Some(end) = text[start..].find('>').map(|e| start + e) else {
        return text.to_string();
    };
    let mut tag = text[start..end].to_string();
    for (name, value) in [("width", width), ("height", height)] {
        let key = format!(" {name}=\"");
        match tag.find(&key) {
            Some(at) => {
                let from = at + key.len();
                if let Some(len) = tag[from..].find('"') {
                    tag.replace_range(from..from + len, &value.to_string());
                }
            }
            None => tag.push_str(&format!(" {name}=\"{value}\"")),
        }
    }
    format!("{}{tag}{}", &text[..start], &text[end..])
}

/// What this app put in its own SVG for an import to find: brush lines'
/// points (by element id) and shapes' markers.
struct Ours {
    lines: HashMap<String, Vec<[f32; 3]>>,
    markers: HashMap<String, (Marker, Marker)>,
    /// Text elements (which have no vector form here).
    text: usize,
    /// Layer names (`data-name`), by element id.
    names: HashMap<String, String>,
}

fn written_by_us(text: &str) -> Ours {
    use quick_xml::events::Event;
    let mut ours = Ours {
        lines: HashMap::new(),
        markers: HashMap::new(),
        text: 0,
        names: HashMap::new(),
    };
    if !text.contains("data-rp-") && !text.contains("<text") && !text.contains("data-name") {
        return ours;
    }
    let mut reader = quick_xml::Reader::from_str(text);
    loop {
        match reader.read_event() {
            Ok(Event::Empty(e) | Event::Start(e)) => {
                if e.name().as_ref() == b"text" {
                    ours.text += 1;
                }
                let mut id = None;
                let (mut points, mut markers, mut name) = (None, None, None);
                for a in e.attributes().flatten() {
                    let value = String::from_utf8_lossy(&a.value).into_owned();
                    match a.key.as_ref() {
                        b"id" => id = Some(value),
                        // (Escaped as this app's export escapes it.)
                        b"data-name" => {
                            name = Some(
                                (value.replace("&lt;", "<").replace("&gt;", ">"))
                                    .replace("&quot;", "\"")
                                    .replace("&amp;", "&"),
                            )
                        }
                        b"data-rp-points" => points = Some(value),
                        b"data-rp-markers" => markers = Some(value),
                        _ => {}
                    }
                }
                let Some(id) = id else { continue };
                if let Some(name) = name {
                    ours.names.insert(id.clone(), name);
                }
                if let Some(p) = points {
                    let pts: Vec<[f32; 3]> = (p.split_whitespace())
                        .filter_map(|t| {
                            let v: Vec<f32> = t.split(',').filter_map(|n| n.parse().ok()).collect();
                            (v.len() == 3).then(|| [v[0], v[1], v[2]])
                        })
                        .collect();
                    ours.lines.insert(id.clone(), pts);
                }
                if let Some(m) = markers {
                    let mut it = m.split_whitespace().map(marker_named);
                    if let (Some(a), Some(b)) = (it.next(), it.next()) {
                        ours.markers.insert(id, (a, b));
                    }
                }
            }
            Ok(Event::Eof) | Err(_) => break,
            _ => {}
        }
    }
    ours
}

fn marker_named(name: &str) -> Marker {
    match name {
        "arrow" => Marker::Arrow,
        "open-arrow" => Marker::OpenArrow,
        "circle" => Marker::Circle,
        "square" => Marker::Square,
        "bar" => Marker::Bar,
        _ => Marker::None,
    }
}

fn group_items(g: &usvg::Group, ours: &Ours, out: &mut Vec<VectorItem>, report: &mut ImportReport) {
    for node in g.children() {
        node_items(node, ours, out, report, None, 1.0);
    }
}

/// One element's items into `out`. Its pictures go into `images` with
/// their opacity (times `opacity`, their groups'), or without it are left
/// out.
fn node_items(
    node: &usvg::Node,
    ours: &Ours,
    out: &mut Vec<VectorItem>,
    report: &mut ImportReport,
    mut images: Option<&mut Vec<(usvg::Image, f32)>>,
    opacity: f32,
) {
    match node {
        usvg::Node::Group(inner) => {
            if inner.clip_path().is_some() || inner.mask().is_some() || !inner.filters().is_empty()
            {
                report.approximated += 1;
            }
            let own = inner.opacity().get();
            let mut items = Vec::new();
            for child in inner.children() {
                let images = images.as_deref_mut();
                node_items(child, ours, &mut items, report, images, opacity * own);
            }
            if items.is_empty() {
                return;
            }
            // usvg makes groups for transforms and the like: only those
            // that are something (faded, or ours) stay groups.
            if own < 1.0
                || inner.id().starts_with("rpg-")
                || items.len() > 1 && !inner.id().is_empty()
            {
                out.push(VectorItem::Group(VectorGroup {
                    name: String::new(),
                    opacity: own,
                    visible: true,
                    items,
                }));
            } else {
                out.extend(items);
            }
        }
        usvg::Node::Path(p) => {
            // Markers this app drew as paths: the shape keeps them.
            if p.id().starts_with("rpm-") || !p.is_visible() {
                return;
            }
            if let Some(points) = ours.lines.get(p.id()) {
                let (colour, opacity) = match p.fill().map(|f| (f.paint(), f.opacity().get())) {
                    Some((usvg::Paint::Color(c), o)) => ([c.red, c.green, c.blue], o),
                    _ => ([0, 0, 0], 1.0),
                };
                out.push(VectorItem::Line(VectorStroke {
                    points: points.clone(),
                    colour,
                    opacity,
                }));
                return;
            }
            if let Some(shape) = path_shape(p, ours, report) {
                out.push(VectorItem::Shape(shape));
            }
        }
        usvg::Node::Image(image) => match images {
            Some(images) => images.push(((**image).clone(), opacity)),
            None => report.left_out += 1,
        },
        // (Counted from the source: see `written_by_us`.)
        usvg::Node::Text(_) => {}
    }
}

/// `[a, b, c, d, e, f]` of a usvg transform.
fn affine(t: usvg::Transform) -> [f32; 6] {
    [t.sx, t.ky, t.kx, t.sy, t.tx, t.ty]
}

fn apply(m: [f32; 6], x: f32, y: f32) -> Vec2 {
    Vec2::new(m[0] * x + m[2] * y + m[4], m[1] * x + m[3] * y + m[5])
}

fn path_shape(p: &usvg::Path, ours: &Ours, report: &mut ImportReport) -> Option<PathShape> {
    let m = affine(p.abs_transform());
    let path = convert_path(p.data(), m);
    if path.is_empty() {
        return None;
    }
    let fill = p.fill().and_then(|f| {
        let paint = paint(f.paint(), f.opacity().get(), m, report)?;
        let rule = match f.rule() {
            usvg::FillRule::NonZero => FillRule::NonZero,
            usvg::FillRule::EvenOdd => FillRule::EvenOdd,
        };
        Some(Fill { paint, rule })
    });
    // Widths scale with the transform's area; stretched one way more than
    // the other, an outline's width varies along it, which isn't kept.
    let det = (m[0] * m[3] - m[1] * m[2]).abs();
    let scale = det.sqrt();
    let (sx, sy) = (
        (m[0] * m[0] + m[1] * m[1]).sqrt(),
        (m[2] * m[2] + m[3] * m[3]).sqrt(),
    );
    let stroke = p.stroke().and_then(|s| {
        let paint = paint(s.paint(), s.opacity().get(), m, report)?;
        if (sx - sy).abs() > 0.01 * sx.max(sy) {
            report.approximated += 1;
        }
        let (start, end) = ours.markers.get(p.id()).copied().unwrap_or_default();
        Some(StrokeStyle {
            paint,
            width: s.width().get() * scale,
            cap: match s.linecap() {
                usvg::LineCap::Butt => Cap::Butt,
                usvg::LineCap::Round => Cap::Round,
                usvg::LineCap::Square => Cap::Square,
            },
            join: match s.linejoin() {
                usvg::LineJoin::Round => Join::Round,
                usvg::LineJoin::Bevel => Join::Bevel,
                _ => Join::Miter,
            },
            miter_limit: s.miterlimit().get(),
            dash: s
                .dasharray()
                .map_or(Vec::new(), |d| d.iter().map(|v| v * scale).collect()),
            dash_offset: s.dashoffset() * scale,
            start,
            end,
        })
    });
    if fill.is_none() && stroke.is_none() {
        return None;
    }
    Some(PathShape {
        path,
        fill,
        stroke,
        opacity: 1.0,
    })
}

/// usvg's path data, through `m`, as Bézier subpaths (quadratic curves
/// raised to cubics).
fn convert_path(data: &usvg::tiny_skia_path::Path, m: [f32; 6]) -> Path {
    use usvg::tiny_skia_path::PathSegment;
    let mut subpaths: Vec<Subpath> = Vec::new();
    let mut current: Option<Subpath> = None;
    let mut last = Vec2::ZERO;
    let to = |p: usvg::tiny_skia_path::Point| apply(m, p.x, p.y);
    let finish = |sub: Option<Subpath>, subpaths: &mut Vec<Subpath>| {
        if let Some(s) = sub.filter(|s| s.knots.len() > 1) {
            subpaths.push(s);
        }
    };
    for seg in data.segments() {
        match seg {
            PathSegment::MoveTo(p) => {
                finish(current.take(), &mut subpaths);
                last = to(p);
                current = Some(Subpath {
                    knots: vec![Knot::corner(last)],
                    closed: false,
                });
            }
            PathSegment::LineTo(p) => {
                let p = to(p);
                if let Some(s) = &mut current {
                    s.knots.push(Knot::corner(p));
                }
                last = p;
            }
            PathSegment::QuadTo(c, p) => {
                let (c, p) = (to(c), to(p));
                let (c1, c2) = (last + (c - last) * (2.0 / 3.0), p + (c - p) * (2.0 / 3.0));
                if let Some(s) = &mut current
                    && let Some(k) = s.knots.last_mut()
                {
                    k.out = c1;
                    s.knots.push(Knot {
                        anchor: p,
                        in_: c2,
                        out: p,
                    });
                }
                last = p;
            }
            PathSegment::CubicTo(c1, c2, p) => {
                let (c1, c2, p) = (to(c1), to(c2), to(p));
                if let Some(s) = &mut current
                    && let Some(k) = s.knots.last_mut()
                {
                    k.out = c1;
                    s.knots.push(Knot {
                        anchor: p,
                        in_: c2,
                        out: p,
                    });
                }
                last = p;
            }
            PathSegment::Close => {
                if let Some(mut s) = current.take() {
                    // The closing point repeats the first: it's the first's
                    // way in.
                    if s.knots.len() > 2 {
                        let (first, end) = (s.knots[0], s.knots[s.knots.len() - 1]);
                        if (first.anchor - end.anchor).length() < 1e-3 {
                            s.knots[0].in_ = end.in_;
                            s.knots.pop();
                        }
                    }
                    s.closed = true;
                    last = s.knots[0].anchor;
                    subpaths.push(s);
                    current = Some(Subpath {
                        knots: vec![Knot::corner(last)],
                        closed: false,
                    });
                }
            }
        }
    }
    finish(current, &mut subpaths);
    Path { subpaths }
}

/// A usvg paint (`opacity` on it) as this app's, gradients placed by the
/// path's transform `m`.
fn paint(p: &usvg::Paint, opacity: f32, m: [f32; 6], report: &mut ImportReport) -> Option<Paint> {
    let a = (opacity.clamp(0.0, 1.0) * 255.0).round() as u8;
    match p {
        usvg::Paint::Color(c) => Some(Paint::Colour([c.red, c.green, c.blue, a])),
        usvg::Paint::LinearGradient(g) => Some(gradient(
            GradientShape::Linear,
            [g.x1(), g.y1()],
            [g.x2(), g.y2()],
            g,
            m,
            opacity,
        )),
        usvg::Paint::RadialGradient(g) => {
            if (g.fx() - g.cx()).abs() > 0.01 || (g.fy() - g.cy()).abs() > 0.01 {
                report.approximated += 1;
            }
            Some(gradient(
                GradientShape::Radial,
                [g.cx(), g.cy()],
                [g.cx() + g.r().get(), g.cy()],
                g,
                m,
                opacity,
            ))
        }
        usvg::Paint::Pattern(_) => {
            report.approximated += 1;
            Some(Paint::Colour([128, 128, 128, a]))
        }
    }
}

fn gradient(
    shape: GradientShape,
    start: [f32; 2],
    end: [f32; 2],
    g: &usvg::BaseGradient,
    m: [f32; 6],
    opacity: f32,
) -> Paint {
    // The gradient's own transform, then the path's.
    let t = affine(g.transform());
    let both = [
        m[0] * t[0] + m[2] * t[1],
        m[1] * t[0] + m[3] * t[1],
        m[0] * t[2] + m[2] * t[3],
        m[1] * t[2] + m[3] * t[3],
        m[0] * t[4] + m[2] * t[5] + m[4],
        m[1] * t[4] + m[3] * t[5] + m[5],
    ];
    let colors: Vec<ColorStop> = (g.stops().iter())
        .map(|s| {
            let c = s.color();
            ColorStop {
                pos: s.offset().get(),
                color: [c.red, c.green, c.blue],
                mid: 0.5,
            }
        })
        .collect();
    let faded = g.stops().iter().any(|s| s.opacity().get() < 1.0) || opacity < 1.0;
    let opacity_stops = if faded {
        (g.stops().iter())
            .map(|s| crate::canvas::gradient::OpacityStop {
                pos: s.offset().get(),
                opacity: s.opacity().get() * opacity,
                mid: 0.5,
            })
            .collect()
    } else {
        Vec::new()
    };
    Paint::Gradient(Box::new(PaintGradient {
        shape,
        repeat: match g.spread_method() {
            usvg::SpreadMethod::Pad => GradientRepeat::None,
            usvg::SpreadMethod::Repeat => GradientRepeat::Repeat,
            usvg::SpreadMethod::Reflect => GradientRepeat::Mirror,
        },
        start,
        end,
        stops: GradientStops {
            colors,
            opacity: opacity_stops,
        },
        transform: Some(both),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shapes_come_in_with_their_look() {
        let svg = r##"<svg xmlns="http://www.w3.org/2000/svg" width="200" height="100">
            <rect x="10" y="10" width="50" height="30" fill="#ff0000" stroke="#0000ff" stroke-width="4" stroke-dasharray="6 3" stroke-linecap="round"/>
            <g opacity="0.5" transform="translate(100 0)">
              <circle cx="30" cy="50" r="20" fill="#00ff00" fill-rule="evenodd"/>
              <path d="M0 0 Q 10 20 20 0" fill="none" stroke="black"/>
            </g>
            <text x="0" y="90">hello</text>
        </svg>"##;
        let (layer, size, report) = import_svg(svg).unwrap();
        assert_eq!(size, [200, 100]);
        assert_eq!(report.left_out, 1, "the text");
        assert_eq!(layer.len(), 2);
        let VectorItem::Shape(rect) = &*layer.items()[0] else {
            panic!()
        };
        assert_eq!(
            rect.fill.as_ref().unwrap().paint,
            Paint::Colour([255, 0, 0, 255])
        );
        let o = rect.stroke.as_ref().unwrap();
        assert_eq!(
            (o.width, o.cap, o.dash.clone()),
            (4.0, Cap::Round, vec![6.0, 3.0])
        );
        let k = &rect.path.subpaths[0];
        assert!(k.closed);
        assert_eq!(k.knots[0].anchor, Vec2::new(10.0, 10.0));
        let VectorItem::Group(g) = &*layer.items()[1] else {
            panic!()
        };
        assert_eq!(g.opacity, 0.5);
        let VectorItem::Shape(circle) = &g.items[0] else {
            panic!()
        };
        // Moved by the group's transform.
        let b = VectorItem::Shape(circle.clone()).bounds();
        assert!((b[0] - 110).abs() <= 2 && (b[2] - 150).abs() <= 3, "{b:?}");
        assert_eq!(circle.fill.as_ref().unwrap().rule, FillRule::EvenOdd);
    }

    /// A rectangle, a gradient ellipse, a dashed curve with arrowheads, an
    /// even-odd star and a faded group: most of what shapes can be.
    fn scene() -> Vec<VectorItem> {
        use crate::canvas::gradient::GradientStops;
        let v = Vec2::new;
        let corners = |pts: &[(f32, f32)]| Subpath {
            knots: pts.iter().map(|&(x, y)| Knot::corner(v(x, y))).collect(),
            closed: true,
        };
        let solid = |c: [u8; 4], w: f32| StrokeStyle::solid(c, w);
        let rect = PathShape {
            path: Path {
                subpaths: vec![corners(&[
                    (10.5, 12.0),
                    (70.0, 12.0),
                    (70.0, 50.25),
                    (10.5, 50.25),
                ])],
            },
            fill: Some(Fill {
                paint: Paint::Colour([220, 40, 30, 255]),
                rule: FillRule::NonZero,
            }),
            stroke: Some(StrokeStyle {
                join: Join::Miter,
                cap: Cap::Butt,
                ..solid([20, 30, 200, 255], 6.0)
            }),
            opacity: 1.0,
        };
        let k = 0.552_284_8;
        let (c, r) = (v(130.0, 40.0), v(35.0, 22.0));
        let ellipse_knots =
            [(1.0, 0.0), (0.0, 1.0), (-1.0, 0.0), (0.0, -1.0)].map(|(x, y): (f32, f32)| {
                let p = c + v(x * r.x, y * r.y);
                let t = v(-y * r.x, x * r.y) * k;
                Knot {
                    anchor: p,
                    in_: p - t,
                    out: p + t,
                }
            });
        let ellipse = PathShape {
            path: Path {
                subpaths: vec![Subpath {
                    knots: ellipse_knots.to_vec(),
                    closed: true,
                }],
            },
            fill: Some(Fill {
                paint: Paint::Gradient(Box::new(PaintGradient {
                    shape: GradientShape::Linear,
                    repeat: GradientRepeat::None,
                    start: [95.0, 40.0],
                    end: [165.0, 40.0],
                    stops: GradientStops::from_colors(&[
                        (0.0, [255, 220, 0]),
                        (1.0, [0, 120, 255]),
                    ]),
                    transform: None,
                })),
                rule: FillRule::NonZero,
            }),
            stroke: None,
            opacity: 1.0,
        };
        let curve = PathShape {
            path: Path {
                subpaths: vec![Subpath {
                    knots: vec![
                        Knot {
                            anchor: v(20.0, 90.0),
                            in_: v(20.0, 90.0),
                            out: v(60.0, 60.0),
                        },
                        Knot {
                            anchor: v(150.0, 95.0),
                            in_: v(100.0, 130.0),
                            out: v(150.0, 95.0),
                        },
                    ],
                    closed: false,
                }],
            },
            fill: None,
            stroke: Some(StrokeStyle {
                cap: Cap::Round,
                dash: vec![12.0, 6.0],
                end: Marker::Arrow,
                ..solid([10, 10, 10, 255], 3.0)
            }),
            opacity: 1.0,
        };
        let star: Vec<(f32, f32)> = (0..5)
            .map(|i| {
                let a = i as f32 * 4.0 * std::f32::consts::PI / 5.0 - std::f32::consts::FRAC_PI_2;
                (40.0 + 25.0 * a.cos(), 140.0 + 25.0 * a.sin())
            })
            .collect();
        let star = PathShape {
            path: Path {
                subpaths: vec![corners(&star)],
            },
            fill: Some(Fill {
                paint: Paint::Colour([40, 160, 60, 255]),
                rule: FillRule::EvenOdd,
            }),
            stroke: None,
            opacity: 1.0,
        };
        let square = |x: f32, colour| {
            VectorItem::Shape(PathShape {
                path: Path {
                    subpaths: vec![corners(&[
                        (x, 120.0),
                        (x + 40.0, 120.0),
                        (x + 40.0, 160.0),
                        (x, 160.0),
                    ])],
                },
                fill: Some(Fill {
                    paint: Paint::Colour(colour),
                    rule: FillRule::NonZero,
                }),
                stroke: None,
                opacity: 1.0,
            })
        };
        let group = VectorItem::Group(VectorGroup {
            name: String::new(),
            opacity: 0.5,
            visible: true,
            items: vec![
                square(100.0, [200, 0, 200, 255]),
                square(120.0, [0, 200, 200, 255]),
            ],
        });
        vec![
            VectorItem::Shape(rect),
            VectorItem::Shape(ellipse),
            VectorItem::Shape(curve),
            VectorItem::Shape(star),
            group,
        ]
    }

    fn svg_of(items: &[VectorItem], w: usize, h: usize) -> String {
        let (mut body, mut defs, mut ids) = (String::new(), String::new(), 0);
        crate::project::svg::items_svg(items, &mut body, &mut defs, &mut ids);
        format!(
            r#"<svg xmlns="http://www.w3.org/2000/svg" width="{w}" height="{h}" viewBox="0 0 {w} {h}"><defs>{defs}</defs>{body}</svg>"#
        )
    }

    #[test]
    fn shapes_draw_as_an_svg_renderer_draws_them() {
        use resvg::tiny_skia;
        let (w, h) = (180, 180);
        let items = scene();
        let ours = crate::canvas::vector::render_region(&items, [0, 0, w as i32, h as i32]);
        let tree = usvg::Tree::from_str(&svg_of(&items, w, h), &usvg::Options::default()).unwrap();
        let mut pixmap = tiny_skia::Pixmap::new(w as u32, h as u32).unwrap();
        resvg::render(
            &tree,
            tiny_skia::Transform::identity(),
            &mut pixmap.as_mut(),
        );
        let mut diffs: Vec<u8> = Vec::new();
        for (a, p) in ours.iter().zip(pixmap.pixels()) {
            let b = [p.red(), p.green(), p.blue(), p.alpha()];
            diffs.extend(a.to_array().iter().zip(b).map(|(x, y)| x.abs_diff(y)));
        }
        let mean = diffs.iter().map(|&d| d as f32).sum::<f32>() / diffs.len() as f32;
        diffs.sort_unstable();
        let p99 = diffs[diffs.len() * 99 / 100];
        // Edges are where two good renderers differ, and only by a little.
        assert!(mean < 1.0, "mean difference {mean}");
        assert!(p99 <= 24, "99th percentile {p99}");
    }

    #[test]
    fn exported_shapes_and_lines_come_back_as_they_were() {
        let mut items = scene();
        items.push(VectorItem::Line(VectorStroke {
            points: vec![[5.0, 170.0, 2.0], [60.0, 165.5, 7.25], [120.0, 172.0, 3.0]],
            colour: [90, 20, 20],
            opacity: 0.75,
        }));
        let (layer, _, report) = import_svg(&svg_of(&items, 180, 180)).unwrap();
        assert_eq!(report, ImportReport::default());
        assert_eq!(layer.len(), items.len());
        let close = |a: f32, b: f32| (a - b).abs() < 0.01;
        for (got, want) in layer.items().iter().zip(&items) {
            match (&**got, want) {
                (VectorItem::Shape(g), VectorItem::Shape(w)) => {
                    assert_eq!(g.fill.is_some(), w.fill.is_some());
                    assert_eq!(g.stroke, w.stroke);
                    if !matches!(w.fill.as_ref().map(|f| &f.paint), Some(Paint::Gradient(_))) {
                        assert_eq!(g.fill, w.fill);
                    }
                    for (gs, ws) in g.path.subpaths.iter().zip(&w.path.subpaths) {
                        assert_eq!(gs.closed, ws.closed);
                        assert_eq!(gs.knots.len(), ws.knots.len());
                        for (gk, wk) in gs.knots.iter().zip(&ws.knots) {
                            for (p, q) in
                                [(gk.anchor, wk.anchor), (gk.in_, wk.in_), (gk.out, wk.out)]
                            {
                                assert!(close(p.x, q.x) && close(p.y, q.y), "{gk:?} {wk:?}");
                            }
                        }
                    }
                }
                (VectorItem::Group(g), VectorItem::Group(w)) => {
                    assert_eq!((g.opacity, g.items.len()), (w.opacity, w.items.len()));
                }
                (VectorItem::Line(g), VectorItem::Line(w)) => {
                    assert_eq!((g.colour, g.opacity), (w.colour, w.opacity));
                    for (p, q) in g.points.iter().zip(&w.points) {
                        assert!(p.iter().zip(q).all(|(a, b)| close(*a, *b)));
                    }
                }
                (g, w) => panic!("{g:?} came back as {w:?}"),
            }
        }
    }

    #[test]
    fn a_kra_layer_is_drawn_at_the_document_size() {
        let svg = r#"<svg xmlns="http://www.w3.org/2000/svg" width="72pt" height="36pt" viewBox="0 0 72 36"><rect width="72" height="36" fill="red"/></svg>"#;
        // 72 pt at 300 ppi is 300 px.
        let (layer, size, _) = import_svg(&with_size(svg, 300, 150)).unwrap();
        assert_eq!(size, [300, 150]);
        let b = layer.items()[0].bounds();
        assert!((b[2] - 300).abs() <= 3, "{b:?}");
    }

    #[test]
    fn our_own_svg_opens_with_its_paint_layers() {
        use crate::canvas::Canvas;
        use eframe::egui::Color32;
        let mut canvas = Canvas::new(100, 70, Color32::WHITE, 64);
        canvas.layers[1].name = "Sky & sea".into();
        let mut tile = vec![Color32::TRANSPARENT; 64 * 64];
        tile[10 * 64 + 20] = Color32::from_rgb(200, 30, 40);
        canvas.set_layer_tile_data(1, 0, 0, tile);
        canvas.layers[1].opacity = 0.5;
        let svg = crate::project::svg::document_svg(&canvas).unwrap();
        let doc = decode_svg(svg.as_bytes()).unwrap();
        assert!(doc.notes.is_empty(), "{:?}", doc.notes);
        let paint = (doc.layers.iter())
            .find(|l| l.name == "Sky & sea")
            .expect("the paint layer came back");
        assert!(paint.vector.is_none());
        assert!((paint.opacity - 0.5).abs() < 0.01);
        assert_eq!(paint.pixels[10 * 100 + 20], Color32::from_rgb(200, 30, 40));
        assert_eq!(paint.pixels[10 * 100 + 21], Color32::TRANSPARENT);
    }

    #[test]
    fn what_has_no_form_here_brings_the_whole_picture_and_a_note() {
        let svg = r##"<svg xmlns="http://www.w3.org/2000/svg" width="40" height="40">
            <clipPath id="c"><rect width="20" height="40"/></clipPath>
            <g clip-path="url(#c)"><rect width="40" height="40" fill="#0000ff"/></g>
        </svg>"##;
        let doc = decode_svg(svg.as_bytes()).unwrap();
        assert_eq!(doc.notes.len(), 1);
        let whole = doc.layers.last().unwrap();
        assert!(!whole.visible && whole.vector.is_none());
        assert_eq!(whole.pixels[5 * 40 + 5].b(), 255, "clipped in");
        assert_eq!(whole.pixels[5 * 40 + 30].a(), 0, "clipped out");
        assert!(doc.layers[0].vector.is_some(), "the shapes still come");
    }
}
