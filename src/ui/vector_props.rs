//! Edit Vectors' options: the picked shapes' fill and outline (colour or
//! gradient, width, ends, corners, dashes, markers) and opacity, and what
//! to do with the picked items (group, stack, delete). A change goes to
//! every picked shape, one undo step per control however long it's dragged.

use crate::app::PainterApp;
use crate::canvas::gradient::{GradientShape, GradientStops};
use crate::canvas::raster::FillRule;
use crate::canvas::raster::marker::Marker;
use crate::canvas::raster::stroke::{Cap, Join};
use crate::canvas::vector::{Fill, Paint, PaintGradient, PathShape, StrokeStyle, VectorItem};
use eframe::egui::{self, Color32};

/// What a paint is, as the menus offer it.
#[derive(Clone, Copy, PartialEq)]
enum Kind {
    None,
    Colour,
    Gradient,
}

fn kind_of(paint: Option<&Paint>) -> Kind {
    match paint {
        None => Kind::None,
        Some(Paint::Colour(_)) => Kind::Colour,
        Some(Paint::Gradient(_)) => Kind::Gradient,
    }
}

/// The colour a paint shows (a gradient's first).
fn colour_of(paint: Option<&Paint>) -> Color32 {
    match paint {
        Some(Paint::Colour([r, g, b, a])) => Color32::from_rgba_unmultiplied(*r, *g, *b, *a),
        Some(Paint::Gradient(g)) => g.stops.colors.first().map_or(Color32::BLACK, |c| {
            let [r, g, b] = c.color;
            Color32::from_rgb(r, g, b)
        }),
        None => Color32::BLACK,
    }
}

/// `paint` as a gradient across `shape`'s box, from its colour to `to`.
fn gradient_across(shape: &PathShape, from: Color32, to: Color32, kind: GradientShape) -> Paint {
    let b = VectorItem::Shape(shape.clone()).bounds();
    let (x0, y0, x1, y1) = (b[0] as f32, b[1] as f32, b[2] as f32, b[3] as f32);
    let (start, end) = match kind {
        GradientShape::Radial => ([(x0 + x1) * 0.5, (y0 + y1) * 0.5], [x1, (y0 + y1) * 0.5]),
        _ => ([x0, (y0 + y1) * 0.5], [x1, (y0 + y1) * 0.5]),
    };
    let rgb = |c: Color32| [c.r(), c.g(), c.b()];
    Paint::Gradient(Box::new(PaintGradient {
        shape: kind,
        repeat: Default::default(),
        start,
        end,
        stops: GradientStops::from_colors(&[(0.0, rgb(from)), (1.0, rgb(to))]),
        transform: None,
    }))
}

/// The options bar for Edit Vectors; returns its hint.
pub(crate) fn vector_edit_options(app: &mut PainterApp, ui: &mut egui::Ui) -> &'static str {
    let hint = "Click a line or shape (Shift: more); drag a point or handle (Alt: a handle alone), a shape to move it; Alt+click the outline: add a point; Delete removes it";
    let Some((idx, all)) = app.picked_items() else {
        return hint;
    };
    let Some(v) = app.canvas.layers[idx].vector.as_ref() else {
        return hint;
    };
    let shape = all.iter().find_map(|&i| match &*v.items()[i] {
        VectorItem::Shape(s) => Some(s.clone()),
        _ => None,
    });
    let first_is_group = matches!(
        v.items().get(all[0]).map(|i| &**i),
        Some(VectorItem::Group(_))
    );
    let secondary = app.brush_state.secondary_color;
    if let Some(shape) = shape {
        shape_controls(app, ui, &shape, secondary);
    }
    ui.separator();
    if all.len() > 1
        && ui
            .button("Group")
            .on_hover_text("The picked items as one")
            .clicked()
    {
        app.group_vector_items();
    }
    if first_is_group && ui.button("Ungroup").clicked() {
        app.ungroup_vector_items();
    }
    for (label, by, tip) in [
        ("⬆", 1, "Up one"),
        ("⬇", -1, "Down one"),
        ("Top", isize::MAX / 2, "On top of everything"),
        ("Bottom", -(isize::MAX / 2), "Under everything"),
    ] {
        if ui.small_button(label).on_hover_text(tip).clicked() {
            app.restack_vector_item(by);
        }
    }
    if ui.button("Delete").clicked() {
        app.delete_vector_items();
    }
    hint
}

/// Fill, outline and opacity of the picked shapes, shown as `shape`'s.
fn shape_controls(app: &mut PainterApp, ui: &mut egui::Ui, shape: &PathShape, secondary: Color32) {
    // Fill.
    let fill = shape.fill.as_ref();
    let mut kind = kind_of(fill.map(|f| &f.paint));
    let before = kind;
    paint_menu(ui, "Fill", &mut kind);
    if kind != before {
        let s = shape.clone();
        app.edit_picked_items("Fill", move |item| {
            if let VectorItem::Shape(sh) = item {
                let colour = colour_of(
                    s.fill
                        .as_ref()
                        .map(|f| &f.paint)
                        .or(s.stroke.as_ref().map(|o| &o.paint)),
                );
                sh.fill = match kind {
                    Kind::None => None,
                    Kind::Colour => Some(Fill {
                        paint: Paint::Colour(colour.to_srgba_unmultiplied()),
                        rule: sh.fill.as_ref().map_or(FillRule::NonZero, |f| f.rule),
                    }),
                    Kind::Gradient => Some(Fill {
                        paint: gradient_across(sh, colour, secondary, GradientShape::Linear),
                        rule: sh.fill.as_ref().map_or(FillRule::NonZero, |f| f.rule),
                    }),
                };
            }
        });
        app.forget_vector_change();
    }
    if let Some(f) = fill {
        paint_editor(app, ui, &f.paint, "Fill colour", fill_paint);
        let mut even_odd = f.rule == FillRule::EvenOdd;
        if ui
            .checkbox(&mut even_odd, "Even-odd")
            .on_hover_text("Where the path crosses itself, holes")
            .changed()
        {
            app.edit_picked_items("Fill rule", move |item| {
                if let VectorItem::Shape(s) = item
                    && let Some(f) = &mut s.fill
                {
                    f.rule = if even_odd {
                        FillRule::EvenOdd
                    } else {
                        FillRule::NonZero
                    };
                }
            });
            app.forget_vector_change();
        }
    }
    ui.separator();
    // Outline.
    let outline = shape.stroke.as_ref();
    let mut kind = kind_of(outline.map(|o| &o.paint));
    let before = kind;
    paint_menu(ui, "Outline", &mut kind);
    if kind != before {
        let primary = app.brush_state.brush.brush_options.color;
        let width = app.brush_state.brush.brush_options.diameter.max(1.0);
        app.edit_picked_items("Outline", move |item| {
            if let VectorItem::Shape(sh) = item {
                let colour = match &sh.stroke {
                    Some(o) => colour_of(Some(&o.paint)),
                    None => primary,
                };
                let mut st = sh
                    .stroke
                    .clone()
                    .unwrap_or_else(|| StrokeStyle::solid(primary.to_srgba_unmultiplied(), width));
                match kind {
                    Kind::None => sh.stroke = None,
                    Kind::Colour => {
                        st.paint = Paint::Colour(colour.to_srgba_unmultiplied());
                        sh.stroke = Some(st);
                    }
                    Kind::Gradient => {
                        st.paint = gradient_across(sh, colour, secondary, GradientShape::Linear);
                        sh.stroke = Some(st);
                    }
                }
            }
        });
        app.forget_vector_change();
    }
    let Some(o) = outline else {
        opacity_slider(app, ui, shape.opacity);
        return;
    };
    paint_editor(app, ui, &o.paint, "Outline colour", outline_paint);
    let mut width = o.width;
    let r = ui.add(
        egui::DragValue::new(&mut width)
            .range(0.0..=400.0)
            .speed(0.2)
            .suffix(" px"),
    );
    if r.changed() {
        app.edit_picked_items("Outline width", move |item| {
            if let VectorItem::Shape(s) = item
                && let Some(o) = &mut s.stroke
            {
                o.width = width;
            }
        });
    }
    if r.drag_stopped() || r.lost_focus() {
        app.forget_vector_change();
    }
    let (mut cap, mut join) = (o.cap, o.join);
    combo(
        ui,
        "cap",
        &mut cap,
        &[
            (Cap::Butt, "Flat ends"),
            (Cap::Round, "Round ends"),
            (Cap::Square, "Square ends"),
        ],
    );
    combo(
        ui,
        "join",
        &mut join,
        &[
            (Join::Miter, "Sharp corners"),
            (Join::Round, "Round corners"),
            (Join::Bevel, "Cut corners"),
        ],
    );
    if (cap, join) != (o.cap, o.join) {
        app.edit_picked_items("Outline ends", move |item| {
            if let VectorItem::Shape(s) = item
                && let Some(o) = &mut s.stroke
            {
                (o.cap, o.join) = (cap, join);
            }
        });
        app.forget_vector_change();
    }
    // Dashes: a few patterns, in widths.
    let w = o.width.max(1.0);
    let patterns: [(&str, Vec<f32>); 4] = [
        ("Solid", Vec::new()),
        ("Dashed", vec![w * 4.0, w * 2.0]),
        ("Dotted", vec![0.0, w * 2.0]),
        ("Dash-dot", vec![w * 4.0, w * 2.0, 0.0, w * 2.0]),
    ];
    let current = patterns
        .iter()
        .position(|(_, p)| p == &o.dash)
        .map_or("Custom", |i| patterns[i].0);
    let mut picked = None;
    egui::ComboBox::from_id_salt("vector_dash")
        .selected_text(current)
        .show_ui(ui, |ui| {
            for (name, p) in &patterns {
                if ui.selectable_label(current == *name, *name).clicked() {
                    picked = Some(p.clone());
                }
            }
        });
    if let Some(dash) = picked {
        // Dots need round (or square) ends to show.
        let dots = dash.first() == Some(&0.0);
        app.edit_picked_items("Dashes", move |item| {
            if let VectorItem::Shape(s) = item
                && let Some(o) = &mut s.stroke
            {
                o.dash = dash.clone();
                if dots && o.cap == Cap::Butt {
                    o.cap = Cap::Round;
                }
            }
        });
        app.forget_vector_change();
    }
    let markers = [
        (Marker::None, "None"),
        (Marker::Arrow, "Arrow"),
        (Marker::OpenArrow, "Open arrow"),
        (Marker::Circle, "Dot"),
        (Marker::Square, "Square"),
        (Marker::Bar, "Bar"),
    ];
    let (mut start, mut end) = (o.start, o.end);
    ui.label("Ends");
    combo(ui, "start", &mut start, &markers);
    combo(ui, "end", &mut end, &markers);
    if (start, end) != (o.start, o.end) {
        app.edit_picked_items("Markers", move |item| {
            if let VectorItem::Shape(s) = item
                && let Some(o) = &mut s.stroke
            {
                (o.start, o.end) = (start, end);
            }
        });
        app.forget_vector_change();
    }
    opacity_slider(app, ui, shape.opacity);
}

fn opacity_slider(app: &mut PainterApp, ui: &mut egui::Ui, opacity: f32) {
    let mut percent = opacity * 100.0;
    let r = ui
        .add(
            egui::DragValue::new(&mut percent)
                .range(0.0..=100.0)
                .suffix("%"),
        )
        .on_hover_text("Opacity");
    if r.changed() {
        let o = percent / 100.0;
        app.edit_picked_items("Shape opacity", move |item| match item {
            VectorItem::Shape(s) => s.opacity = o,
            VectorItem::Group(g) => g.opacity = o,
            VectorItem::Line(_) => {}
        });
    }
    if r.drag_stopped() || r.lost_focus() {
        app.forget_vector_change();
    }
}

fn fill_paint(item: &mut VectorItem) -> Option<&mut Paint> {
    match item {
        VectorItem::Shape(s) => s.fill.as_mut().map(|f| &mut f.paint),
        _ => None,
    }
}

fn outline_paint(item: &mut VectorItem) -> Option<&mut Paint> {
    match item {
        VectorItem::Shape(s) => s.stroke.as_mut().map(|o| &mut o.paint),
        _ => None,
    }
}

/// None / Colour / Gradient for a paint.
fn paint_menu(ui: &mut egui::Ui, label: &str, kind: &mut Kind) {
    let name = |k: Kind| match k {
        Kind::None => "none",
        Kind::Colour => "colour",
        Kind::Gradient => "gradient",
    };
    egui::ComboBox::from_id_salt(("vector_paint", label))
        .selected_text(format!("{label}: {}", name(*kind)))
        .show_ui(ui, |ui| {
            for k in [Kind::None, Kind::Colour, Kind::Gradient] {
                ui.selectable_value(kind, k, name(k));
            }
        });
}

/// A paint's colour (a gradient's two ends) picked: `paint_of` finds the
/// paint in each picked item.
fn paint_editor(
    app: &mut PainterApp,
    ui: &mut egui::Ui,
    paint: &Paint,
    label: &'static str,
    paint_of: fn(&mut VectorItem) -> Option<&mut Paint>,
) {
    match paint {
        Paint::Colour([r, g, b, a]) => {
            let mut c = Color32::from_rgba_unmultiplied(*r, *g, *b, *a);
            let resp = ui.color_edit_button_srgba(&mut c);
            if resp.changed() {
                let rgba = c.to_srgba_unmultiplied();
                app.edit_picked_items(label, move |item| {
                    if let Some(p) = paint_of(item) {
                        *p = Paint::Colour(rgba);
                    }
                });
            }
        }
        Paint::Gradient(g) => {
            let ends = [0usize, g.stops.colors.len().saturating_sub(1)];
            for (n, &i) in ends.iter().enumerate() {
                let Some(stop) = g.stops.colors.get(i) else {
                    continue;
                };
                let mut rgb = stop.color;
                if ui.color_edit_button_srgb(&mut rgb).changed() {
                    app.edit_picked_items(label, move |item| {
                        if let Some(Paint::Gradient(g)) = paint_of(item) {
                            let last = g.stops.colors.len().saturating_sub(1);
                            if let Some(s) = g.stops.colors.get_mut(if n == 0 { 0 } else { last }) {
                                s.color = rgb;
                            }
                        }
                    });
                }
            }
            let mut radial = g.shape == GradientShape::Radial;
            if ui.checkbox(&mut radial, "Radial").changed() {
                app.edit_picked_items(label, move |item| {
                    if let Some(Paint::Gradient(g)) = paint_of(item) {
                        let (s, e) = (g.start, g.end);
                        g.shape = if radial {
                            GradientShape::Radial
                        } else {
                            GradientShape::Linear
                        };
                        // Radial from the middle; linear across.
                        let mid = [(s[0] + e[0]) * 0.5, (s[1] + e[1]) * 0.5];
                        (g.start, g.end) = if radial {
                            (mid, e)
                        } else {
                            ([2.0 * s[0] - e[0], 2.0 * s[1] - e[1]], e)
                        };
                    }
                });
                app.forget_vector_change();
            }
        }
    }
}

fn combo<T: PartialEq + Copy>(ui: &mut egui::Ui, id: &str, value: &mut T, options: &[(T, &str)]) {
    let current = options
        .iter()
        .find(|(v, _)| v == value)
        .map_or("", |(_, n)| *n);
    egui::ComboBox::from_id_salt(("vector_combo", id))
        .selected_text(current)
        .show_ui(ui, |ui| {
            for (v, name) in options {
                ui.selectable_value(value, *v, *name);
            }
        });
}

#[cfg(test)]
mod tests {
    use crate::canvas::Canvas;
    use crate::canvas::path::{Knot, Path, Subpath};
    use eframe::egui::{self, Color32, Vec2};

    #[test]
    fn the_options_show_a_picked_shapes_look() {
        let mut app = crate::project::tests::test_app_pub(Canvas::new(128, 96, Color32::WHITE, 64));
        app.canvas_mut().active_layer_idx = 1;
        app.add_vector_layer();
        let idx = app.canvas.active_layer_idx;
        let path = Path {
            subpaths: vec![Subpath {
                knots: [(10.0, 10.0), (60.0, 10.0), (60.0, 50.0), (10.0, 50.0)]
                    .map(|(x, y)| Knot::corner(Vec2::new(x, y)))
                    .to_vec(),
                closed: true,
            }],
        };
        app.add_vector_shape(idx, path, true, true);
        app.active_tool = crate::app::tools::Tool::VectorEdit;
        app.viewport.zoom = 1.0;
        app.line_edit_press(Vec2::new(30.0, 30.0), false, false);
        app.line_edit_release();
        assert!(app.picked_items().is_some(), "picked");
        let ctx = egui::Context::default();
        let mut texts = Vec::new();
        for _ in 0..2 {
            let out = ctx.run(egui::RawInput::default(), |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    ui.horizontal_wrapped(|ui| {
                        super::vector_edit_options(&mut app, ui);
                    });
                });
            });
            texts = (out.shapes.into_iter())
                .filter_map(|s| match s.shape {
                    egui::epaint::Shape::Text(t) => Some(t.galley.text().to_string()),
                    _ => None,
                })
                .collect();
        }
        for want in [
            "Fill: colour",
            "Outline: colour",
            "Round ends",
            "Round corners",
            "Solid",
            "Delete",
        ] {
            assert!(texts.iter().any(|t| t == want), "{want} in {texts:?}");
        }
        assert!(
            !texts.iter().any(|t| t == "Group"),
            "one item: nothing to group"
        );
    }
}
