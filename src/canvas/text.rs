//! Text rendering for the Text tool: lines of text in a font, as a coverage
//! mask on the canvas (the tool paints it in the text's colour), and the
//! source a text layer keeps so it can be edited again ([`TextLayer`]).

use ab_glyph::{Font, FontArc, PxScale, ScaleFont};
use eframe::egui::{Color32, Vec2};

use crate::selection::SelectionMask;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub enum TextAlign {
    #[default]
    Left,
    Center,
    Right,
}

/// How to set the text.
#[derive(Clone, Copy, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct TextStyle {
    /// Font size in canvas pixels (the height of a line without spacing).
    pub size: f32,
    /// Line height as a multiple of the font's own.
    pub line_spacing: f32,
    /// Extra space between letters, in pixels.
    pub letter_spacing: f32,
    pub align: TextAlign,
}

impl Default for TextStyle {
    fn default() -> Self {
        Self {
            size: 64.0,
            line_spacing: 1.0,
            letter_spacing: 0.0,
            align: TextAlign::Left,
        }
    }
}

/// What a text layer's pixels are made from, so the text can be edited
/// again. Painting on the layer (anything but moving it) turns it into
/// plain pixels and drops this.
#[derive(Clone, Debug, PartialEq)]
pub struct TextLayer {
    pub text: String,
    /// The font's name as the Text tool lists it (a shipped font, or a font
    /// file's name).
    pub font: String,
    pub style: TextStyle,
    pub color: Color32,
    /// Top-left of the text block, canvas pixels.
    pub pos: Vec2,
}

/// Where each line's glyphs go: `(glyph, x, baseline y)` relative to the
/// text's top-left, and the block's width and height.
fn layout(font: &FontArc, text: &str, style: &TextStyle) -> (Vec<ab_glyph::Glyph>, Vec2) {
    let scaled = font.as_scaled(PxScale::from(style.size.max(1.0)));
    let line_height = (scaled.height() + scaled.line_gap()) * style.line_spacing.max(0.1);
    let lines: Vec<&str> = text.split('\n').collect();
    // Each line's glyphs from x = 0, and its width.
    let set: Vec<(Vec<ab_glyph::Glyph>, f32)> = lines
        .iter()
        .map(|line| {
            let mut x = 0.0;
            let mut prev = None;
            let mut glyphs = Vec::new();
            for c in line.chars() {
                let id = scaled.glyph_id(c);
                if let Some(p) = prev {
                    x += scaled.kern(p, id);
                }
                glyphs.push(id.with_scale_and_position(scaled.scale(), ab_glyph::point(x, 0.0)));
                x += scaled.h_advance(id) + style.letter_spacing;
                prev = Some(id);
            }
            (glyphs, (x - style.letter_spacing).max(0.0))
        })
        .collect();
    let width = set.iter().map(|(_, w)| *w).fold(0.0, f32::max);
    let mut out = Vec::new();
    for (i, (glyphs, w)) in set.into_iter().enumerate() {
        let dx = match style.align {
            TextAlign::Left => 0.0,
            TextAlign::Center => (width - w) * 0.5,
            TextAlign::Right => width - w,
        };
        let baseline = scaled.ascent() + i as f32 * line_height;
        out.extend(glyphs.into_iter().map(|mut g| {
            g.position.x += dx;
            g.position.y = baseline;
            g
        }));
    }
    let height = scaled.height() + (lines.len().max(1) - 1) as f32 * line_height;
    (out, Vec2::new(width, height))
}

/// The text block's size in canvas pixels.
pub fn measure(font: &FontArc, text: &str, style: &TextStyle) -> Vec2 {
    layout(font, text, style).1
}

/// `text` as coverage with its top-left at canvas point `origin`, or `None`
/// when nothing would show.
pub fn render(
    font: &FontArc,
    text: &str,
    style: &TextStyle,
    origin: Vec2,
) -> Option<SelectionMask> {
    let (glyphs, size) = layout(font, text, style);
    let outlined: Vec<_> = glyphs
        .into_iter()
        .filter_map(|mut g| {
            g.position.x += origin.x;
            g.position.y += origin.y;
            font.outline_glyph(g)
        })
        .collect();
    if outlined.is_empty() || size.x <= 0.0 {
        return None;
    }
    let bounds = outlined
        .iter()
        .map(|g| g.px_bounds())
        .reduce(|a, b| ab_glyph::Rect {
            min: ab_glyph::point(a.min.x.min(b.min.x), a.min.y.min(b.min.y)),
            max: ab_glyph::point(a.max.x.max(b.max.x), a.max.y.max(b.max.y)),
        })?;
    let (x0, y0) = (bounds.min.x.floor() as i32, bounds.min.y.floor() as i32);
    let (x1, y1) = (bounds.max.x.ceil() as i32, bounds.max.y.ceil() as i32);
    let (w, h) = ((x1 - x0).max(1) as usize, (y1 - y0).max(1) as usize);
    let mut data = vec![0u8; w * h];
    for g in &outlined {
        let b = g.px_bounds();
        let (gx, gy) = (b.min.x as i32 - x0, b.min.y as i32 - y0);
        g.draw(|x, y, c| {
            let (px, py) = (gx + x as i32, gy + y as i32);
            if px >= 0 && py >= 0 && (px as usize) < w && (py as usize) < h {
                let i = py as usize * w + px as usize;
                // Overlapping glyphs (script fonts) add up.
                let v = data[i] as f32 / 255.0 + c;
                data[i] = (v.min(1.0) * 255.0).round() as u8;
            }
        });
    }
    Some(SelectionMask::new(x0, y0, w, h, data))
}

/// The fonts the app ships (egui's), by name.
pub fn builtin_fonts() -> Vec<(String, FontArc)> {
    let defs = eframe::egui::FontDefinitions::default();
    ["Ubuntu-Light", "Hack"]
        .iter()
        .filter_map(|&name| {
            let data = defs.font_data.get(name)?;
            let font = match &data.font {
                std::borrow::Cow::Borrowed(bytes) => FontArc::try_from_slice(bytes).ok()?,
                std::borrow::Cow::Owned(bytes) => FontArc::try_from_vec(bytes.clone()).ok()?,
            };
            let label = if name == "Hack" {
                "Hack (monospace)"
            } else {
                "Ubuntu Light"
            };
            Some((label.to_string(), font))
        })
        .collect()
}

/// Font files in the system's usual font folders (`.ttf`, `.otf`), by
/// name, sorted. Opened on demand.
pub fn system_font_files() -> Vec<(String, std::path::PathBuf)> {
    let mut dirs: Vec<std::path::PathBuf> = vec![
        "/usr/share/fonts".into(),
        "/usr/local/share/fonts".into(),
        "/system/fonts".into(),
        "/Library/Fonts".into(),
        "/System/Library/Fonts".into(),
    ];
    if let Some(home) = std::env::var_os("HOME") {
        let home = std::path::PathBuf::from(home);
        dirs.push(home.join(".local/share/fonts"));
        dirs.push(home.join(".fonts"));
        dirs.push(home.join("Library/Fonts"));
    }
    if let Some(windir) = std::env::var_os("WINDIR") {
        dirs.push(std::path::PathBuf::from(windir).join("Fonts"));
    }
    let mut found = Vec::new();
    let mut stack = dirs;
    let mut visited = 0;
    // Bounded: a huge or looping font tree can't stall the app.
    while let Some(dir) = stack.pop() {
        visited += 1;
        if visited > 2000 {
            break;
        }
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if path
                .extension()
                .and_then(|e| e.to_str())
                .is_some_and(|e| e.eq_ignore_ascii_case("ttf") || e.eq_ignore_ascii_case("otf"))
                && let Some(stem) = path.file_stem()
            {
                found.push((stem.to_string_lossy().into_owned(), path));
            }
        }
    }
    found.sort_by_key(|(name, _)| name.to_lowercase());
    found.dedup_by(|a, b| a.0 == b.0);
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    fn font() -> FontArc {
        builtin_fonts().remove(0).1
    }

    #[test]
    fn text_renders_where_it_is_placed() {
        let style = TextStyle {
            size: 40.0,
            ..Default::default()
        };
        let mask = render(&font(), "Hi", &style, Vec2::new(100.0, 50.0)).unwrap();
        assert!(mask.x0 >= 100 && mask.y0 >= 50);
        assert!(mask.data.contains(&255), "solid inside the letters");
        assert!(mask.w < 80 && mask.h <= 45, "{}x{}", mask.w, mask.h);
    }

    #[test]
    fn lines_stack_and_align() {
        let f = font();
        let style = TextStyle {
            size: 30.0,
            ..Default::default()
        };
        let one = measure(&f, "wide line", &style);
        let two = measure(&f, "wide line\nx", &style);
        assert_eq!(one.x, two.x, "as wide as the widest line");
        assert!(two.y > one.y * 1.8);
        let right = TextStyle {
            align: TextAlign::Right,
            ..style
        };
        let left_x = render(&f, "wide line\nx", &style, Vec2::ZERO).unwrap();
        let right_x = render(&f, "wide line\nx", &right, Vec2::ZERO).unwrap();
        // The short line moves right: the lower half's leftmost ink too.
        let first_ink = |m: &SelectionMask| {
            (m.h / 2..m.h)
                .flat_map(|y| (0..m.w).map(move |x| (x, y)))
                .filter(|&(x, y)| m.data[y * m.w + x] > 128)
                .map(|(x, _)| x as i32 + m.x0)
                .min()
                .unwrap()
        };
        assert!(first_ink(&right_x) > first_ink(&left_x) + 50);
    }

    #[test]
    fn blank_text_renders_nothing() {
        assert!(render(&font(), "  \n ", &TextStyle::default(), Vec2::ZERO).is_none());
    }
}
