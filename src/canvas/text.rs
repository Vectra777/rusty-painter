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
    /// Box text: lines wrap at this width (canvas pixels). `None`: point
    /// text, lines only where the text breaks them.
    pub width: Option<f32>,
}

impl Default for TextStyle {
    fn default() -> Self {
        Self {
            size: 64.0,
            line_spacing: 1.0,
            letter_spacing: 0.0,
            align: TextAlign::Left,
            width: None,
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
    /// Where the Transform tool put it: a 3×3 projective map (row-major)
    /// from where the text would be untransformed to the canvas. It's drawn
    /// through it from its glyphs' outlines, so it stays sharp.
    pub transform: Option<[f64; 9]>,
}

/// A line of laid-out text.
#[derive(Clone, Debug, PartialEq)]
pub struct Line {
    /// Its characters, by index in the text: `start..end` (a line break
    /// isn't in either line).
    pub start: usize,
    pub end: usize,
    /// Its top, from the block's top.
    pub top: f32,
    /// Where the caret goes before each of its characters and after the
    /// last: `end - start + 1` positions from the block's left.
    pub carets: Vec<f32>,
}

/// Text set in a font: where its glyphs go and where the caret can be.
/// Positions are from the block's top-left.
pub struct Layout {
    /// Each glyph and which of the fonts it's from (a character the font
    /// doesn't have comes from the next that does).
    pub glyphs: Vec<(usize, ab_glyph::Glyph)>,
    pub lines: Vec<Line>,
    pub size: Vec2,
    /// A line's height, spacing included.
    pub line_height: f32,
    /// Its glyphs' height (the caret's).
    pub text_height: f32,
}

/// A line's glyphs (and their fonts), caret stops and ink width, before
/// alignment.
type LineSet = (Vec<(usize, ab_glyph::Glyph)>, Vec<f32>, f32);

/// `text` set in `fonts` (the first the text's font, the rest used for
/// characters it doesn't have) by `style`.
pub fn layout(fonts: &[FontArc], text: &str, style: &TextStyle) -> Layout {
    let scale = PxScale::from(style.size.max(1.0));
    let main = fonts[0].as_scaled(scale);
    let line_height = (main.height() + main.line_gap()) * style.line_spacing.max(0.1);
    let ascent = main.ascent();
    // Which font each character comes from, its glyph and advance.
    let pick = |c: char| -> (usize, ab_glyph::GlyphId) {
        let c = if c == '\t' { ' ' } else { c };
        fonts
            .iter()
            .enumerate()
            .map(|(i, f)| (i, f.glyph_id(c)))
            .find(|(_, id)| id.0 != 0)
            .unwrap_or((0, fonts[0].glyph_id(c)))
    };
    let chars: Vec<char> = text.chars().collect();
    let wrap = style.width.map(|w| w.max(style.size));
    // Lines as (start, end) in characters, wrapped at spaces (a word too
    // long for the box breaks where it reaches the edge).
    let mut spans: Vec<(usize, usize)> = Vec::new();
    let mut start = 0;
    while start <= chars.len() {
        let para_end = chars[start..]
            .iter()
            .position(|&c| c == '\n')
            .map_or(chars.len(), |p| start + p);
        let mut line_start = start;
        let (mut x, mut last_space, mut prev): (
            f32,
            Option<usize>,
            Option<(usize, ab_glyph::GlyphId)>,
        ) = (0.0, None, None);
        let mut i = start;
        while i < para_end {
            let (f, id) = pick(chars[i]);
            let sf = fonts[f].as_scaled(scale);
            let kern = match prev {
                Some((pf, pid)) if pf == f => sf.kern(pid, id),
                _ => 0.0,
            };
            let advance = sf.h_advance(id) + style.letter_spacing;
            if let Some(w) = wrap
                && i > line_start
                && chars[i] != ' '
                && x + kern + advance - style.letter_spacing > w
            {
                // Break after the last space, or here.
                let at = last_space.filter(|&s| s >= line_start).map_or(i, |s| s + 1);
                spans.push((line_start, at));
                line_start = at;
                (x, last_space, prev) = (0.0, None, None);
                i = at;
                continue;
            }
            if chars[i] == ' ' {
                last_space = Some(i);
            }
            x += kern + advance;
            prev = Some((f, id));
            i += 1;
        }
        spans.push((line_start, para_end));
        start = para_end + 1;
    }
    // Each line's glyphs and caret stops from x = 0, and its ink width
    // (trailing spaces left out, for alignment).
    let mut set: Vec<LineSet> = Vec::new();
    for &(a, b) in &spans {
        let mut x = 0.0;
        let mut prev: Option<(usize, ab_glyph::GlyphId)> = None;
        let mut glyphs = Vec::new();
        let mut carets = vec![0.0];
        let mut ink = 0.0f32;
        for &c in &chars[a..b] {
            let (f, id) = pick(c);
            let sf = fonts[f].as_scaled(scale);
            if let Some((pf, pid)) = prev
                && pf == f
            {
                x += sf.kern(pid, id);
            }
            glyphs.push((
                f,
                id.with_scale_and_position(scale, ab_glyph::point(x, 0.0)),
            ));
            x += sf.h_advance(id);
            if c != ' ' {
                ink = x;
            }
            carets.push(x);
            x += style.letter_spacing;
            prev = Some((f, id));
        }
        set.push((glyphs, carets, ink));
    }
    let widest = set.iter().map(|(_, _, w)| *w).fold(0.0, f32::max);
    let width = wrap.unwrap_or(widest);
    let mut glyphs = Vec::new();
    let mut lines = Vec::new();
    for (i, ((line_glyphs, carets, ink), &(a, b))) in set.into_iter().zip(&spans).enumerate() {
        let dx = match style.align {
            TextAlign::Left => 0.0,
            TextAlign::Center => (width - ink) * 0.5,
            TextAlign::Right => width - ink,
        };
        let top = i as f32 * line_height;
        glyphs.extend(line_glyphs.into_iter().map(|(f, mut g)| {
            g.position.x += dx;
            g.position.y = top + ascent;
            (f, g)
        }));
        lines.push(Line {
            start: a,
            end: b,
            top,
            carets: carets.into_iter().map(|x| x + dx).collect(),
        });
    }
    let text_height = main.height();
    let height = text_height + (lines.len().max(1) - 1) as f32 * line_height;
    Layout {
        glyphs,
        lines,
        size: Vec2::new(width, height),
        line_height,
        text_height,
    }
}

impl Layout {
    /// The line caret position `index` is on: at a wrap, the next line's
    /// start.
    pub fn line_of(&self, index: usize) -> usize {
        self.lines
            .iter()
            .rposition(|l| l.start <= index)
            .unwrap_or(0)
            .min(self.lines.len().saturating_sub(1))
    }

    /// The caret at character `index`: its top and x, from the block's
    /// top-left.
    pub fn caret(&self, index: usize) -> Vec2 {
        let Some(line) = self.lines.get(self.line_of(index)) else {
            return Vec2::ZERO;
        };
        let k = index.clamp(line.start, line.end) - line.start;
        Vec2::new(line.carets[k], line.top)
    }

    /// The caret position nearest point `p` (from the block's top-left).
    pub fn hit(&self, p: Vec2) -> usize {
        let i = ((p.y / self.line_height.max(1e-3)).floor().max(0.0) as usize)
            .min(self.lines.len().saturating_sub(1));
        let Some(line) = self.lines.get(i) else {
            return 0;
        };
        let k = line
            .carets
            .iter()
            .enumerate()
            .min_by(|a, b| (a.1 - p.x).abs().total_cmp(&(b.1 - p.x).abs()))
            .map_or(0, |(k, _)| k);
        line.start + k
    }

    /// The caret position a line up (`-1`) or down (`1`) from `index`,
    /// at the same x; `None` past the first or last line.
    pub fn vertical(&self, index: usize, dir: i32) -> Option<usize> {
        let line = self.line_of(index) as i32 + dir;
        if line < 0 || line as usize >= self.lines.len() {
            return None;
        }
        let at = self.caret(index);
        Some(self.hit(Vec2::new(at.x, self.lines[line as usize].top + 1.0)))
    }

    /// The start and end of caret `index`'s line.
    pub fn line_bounds(&self, index: usize) -> (usize, usize) {
        let line = &self.lines[self.line_of(index)];
        (line.start, line.end)
    }

    /// The highlight for characters `a..b`: one rectangle per line, from
    /// the block's top-left.
    pub fn selection(&self, a: usize, b: usize) -> Vec<eframe::egui::Rect> {
        let (a, b) = (a.min(b), a.max(b));
        self.lines
            .iter()
            .filter(|l| l.end >= a && l.start <= b && a != b)
            .map(|l| {
                let x0 = l.carets[a.clamp(l.start, l.end) - l.start];
                // A selection running on past the line shows its break.
                let x1 = l.carets[b.clamp(l.start, l.end) - l.start]
                    + if b > l.end {
                        self.text_height * 0.25
                    } else {
                        0.0
                    };
                eframe::egui::Rect::from_min_max(
                    eframe::egui::pos2(x0, l.top),
                    eframe::egui::pos2(x1, l.top + self.text_height),
                )
            })
            .collect()
    }
}

/// The text block's size in canvas pixels.
pub fn measure(fonts: &[FontArc], text: &str, style: &TextStyle) -> Vec2 {
    layout(fonts, text, style).size
}

/// `text` as coverage with its top-left at canvas point `origin`, or `None`
/// when nothing would show.
pub fn render(
    fonts: &[FontArc],
    text: &str,
    style: &TextStyle,
    origin: Vec2,
    transform: Option<&[f64; 9]>,
) -> Option<SelectionMask> {
    if let Some(h) = transform {
        return render_transformed(fonts, text, style, origin, h);
    }
    let layout = layout(fonts, text, style);
    let outlined: Vec<_> = layout
        .glyphs
        .into_iter()
        .filter_map(|(f, mut g)| {
            g.position.x += origin.x;
            g.position.y += origin.y;
            fonts[f].outline_glyph(g)
        })
        .collect();
    if outlined.is_empty() {
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
                // Overlapping glyphs (script fonts): the most covered.
                let v = (c.min(1.0) * 255.0).round() as u8;
                data[i] = data[i].max(v);
            }
        });
    }
    Some(SelectionMask::new(x0, y0, w, h, data))
}

/// [`render`] through the projective map `h`: the glyphs' outlines cut
/// into straight pieces, mapped, and filled with exact coverage.
fn render_transformed(
    fonts: &[FontArc],
    text: &str,
    style: &TextStyle,
    origin: Vec2,
    h: &[f64; 9],
) -> Option<SelectionMask> {
    use crate::canvas::raster::{Edges, FillRule};
    use ab_glyph::OutlineCurve;
    let map = |p: Vec2| crate::canvas::storage::apply_homography(h, p);
    let mut edges = Edges::new();
    for (f, g) in layout(fonts, text, style).glyphs {
        let font = &fonts[f];
        let Some(outline) = font.outline(g.id) else {
            continue;
        };
        let sf = font.as_scaled(g.scale);
        let (hs, vs) = (sf.h_scale_factor(), sf.v_scale_factor());
        let at = |p: ab_glyph::Point| {
            map(Vec2::new(
                origin.x + g.position.x + p.x * hs,
                origin.y + g.position.y - p.y * vs,
            ))
        };
        // Pieces short enough at the text's size to look curved.
        let steps = ((style.size / 6.0).ceil() as usize).clamp(4, 24);
        for curve in &outline.curves {
            let points: Vec<Vec2> = match *curve {
                OutlineCurve::Line(a, b) => vec![at(a), at(b)],
                OutlineCurve::Quad(a, b, c) => (0..=steps)
                    .map(|i| {
                        let t = i as f32 / steps as f32;
                        let u = 1.0 - t;
                        at(ab_glyph::point(
                            u * u * a.x + 2.0 * u * t * b.x + t * t * c.x,
                            u * u * a.y + 2.0 * u * t * b.y + t * t * c.y,
                        ))
                    })
                    .collect(),
                OutlineCurve::Cubic(a, b, c, d) => (0..=steps)
                    .map(|i| {
                        let t = i as f32 / steps as f32;
                        let u = 1.0 - t;
                        let (k0, k1, k2, k3) =
                            (u * u * u, 3.0 * u * u * t, 3.0 * u * t * t, t * t * t);
                        at(ab_glyph::point(
                            k0 * a.x + k1 * b.x + k2 * c.x + k3 * d.x,
                            k0 * a.y + k1 * b.y + k2 * c.y + k3 * d.y,
                        ))
                    })
                    .collect(),
            };
            for w in points.windows(2) {
                edges.edge(w[0], w[1]);
            }
        }
    }
    if edges.is_empty() {
        return None;
    }
    // ponytail: a map that throws the text far away is cut to 16k pixels a side.
    let [x0, y0, x1, y1] = edges.pixel_bounds();
    let (x1, y1) = (x1.min(x0 + 16384), y1.min(y0 + 16384));
    let (w, h) = ((x1 - x0).max(1) as usize, (y1 - y0).max(1) as usize);
    let mut cov = vec![0.0f32; w * h];
    if !edges.coverage(FillRule::NonZero, [x0, y0, x1, y1], &mut cov) {
        return None;
    }
    let data = cov
        .iter()
        .map(|c| (c.clamp(0.0, 1.0) * 255.0).round() as u8)
        .collect();
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

    fn font() -> Vec<FontArc> {
        vec![builtin_fonts().remove(0).1]
    }

    #[test]
    fn text_renders_where_it_is_placed() {
        let style = TextStyle {
            size: 40.0,
            ..Default::default()
        };
        let mask = render(&font(), "Hi", &style, Vec2::new(100.0, 50.0), None).unwrap();
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
        let left_x = render(&f, "wide line\nx", &style, Vec2::ZERO, None).unwrap();
        let right_x = render(&f, "wide line\nx", &right, Vec2::ZERO, None).unwrap();
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
    fn box_text_wraps_at_spaces_and_breaks_long_words() {
        let f = font();
        let style = TextStyle {
            size: 20.0,
            width: Some(120.0),
            ..Default::default()
        };
        let l = layout(&f, "one two three four five six", &style);
        assert!(l.lines.len() > 1, "wrapped");
        assert_eq!(l.size.x, 120.0, "as wide as its box");
        for line in &l.lines {
            assert!(line.carets.iter().all(|&x| x <= 120.0 + 20.0));
            // Each line starts a word.
            let chars: Vec<char> = "one two three four five six".chars().collect();
            assert!(line.start == 0 || chars[line.start - 1] == ' ');
        }
        let long = layout(&f, &"m".repeat(40), &style);
        assert!(long.lines.len() > 2, "a word too long for the box breaks");
        // Point text doesn't wrap.
        let point = layout(
            &f,
            "one two three four five six",
            &TextStyle {
                width: None,
                ..style
            },
        );
        assert_eq!(point.lines.len(), 1);
    }

    #[test]
    fn carets_sit_between_letters_and_clicks_find_them() {
        let f = font();
        let style = TextStyle {
            size: 30.0,
            ..Default::default()
        };
        let l = layout(&f, "ab\ncd", &style);
        assert_eq!(l.lines.len(), 2);
        assert_eq!((l.lines[1].start, l.lines[1].end), (3, 5));
        assert_eq!(l.caret(0), Vec2::ZERO);
        let after_a = l.caret(1);
        assert!(after_a.x > 5.0 && after_a.y == 0.0);
        assert_eq!(l.caret(3).y, l.line_height, "the second line");
        assert_eq!(l.hit(after_a + Vec2::new(1.0, 5.0)), 1);
        assert_eq!(
            l.hit(Vec2::new(1000.0, l.line_height + 5.0)),
            5,
            "past the end"
        );
        assert_eq!(l.vertical(1, 1), Some(4), "straight down");
        assert_eq!(l.vertical(1, -1), None);
        assert_eq!(l.line_bounds(4), (3, 5));
        assert_eq!(l.selection(1, 4).len(), 2, "one highlight per line");
    }

    #[test]
    fn a_character_the_font_lacks_comes_from_the_next() {
        let fonts: Vec<FontArc> = builtin_fonts().into_iter().map(|(_, f)| f).collect();
        let (first, second) = (&fonts[0], &fonts[1]);
        let Some(c) = ('\u{100}'..'\u{3000}')
            .find(|&c| first.glyph_id(c).0 == 0 && second.glyph_id(c).0 != 0)
        else {
            return; // the two fonts cover the same characters
        };
        let l = layout(&fonts, &format!("a{c}"), &TextStyle::default());
        assert_eq!(l.glyphs[0].0, 0);
        assert_eq!(l.glyphs[1].0, 1, "{c:?} from the second font");
    }

    #[test]
    fn blank_text_renders_nothing() {
        assert!(render(&font(), "  \n ", &TextStyle::default(), Vec2::ZERO, None).is_none());
    }
}
