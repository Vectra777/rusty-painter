//! Other apps' palette files: GIMP's `.gpl` (also Inkscape's, Aseprite's
//! and many more), Adobe's `.ase` and Photoshop's `.aco`, and Krita's
//! `.kpl`. All are read; `.gpl`, `.ase` and `.kpl` are written. Only
//! Krita's keep gaps; a `.gpl` keeps groups as comments GIMP ignores.

use crate::app::tools::palettes::{Group, Palette, Swatch, hex};
use crate::canvas::palette::cielab_to_rgb;
use eframe::egui::Color32;

/// The extensions palettes are opened from.
pub(crate) const EXTENSIONS: &[&str] = &["gpl", "ase", "aco", "kpl"];

/// What a palette can be saved as.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Format {
    Gpl,
    Ase,
    Kpl,
}

impl Format {
    pub(crate) const ALL: [Format; 3] = [Format::Gpl, Format::Ase, Format::Kpl];

    pub(crate) fn extension(self) -> &'static str {
        match self {
            Format::Gpl => "gpl",
            Format::Ase => "ase",
            Format::Kpl => "kpl",
        }
    }

    pub(crate) fn label(self) -> &'static str {
        match self {
            Format::Gpl => "GIMP palette (.gpl)",
            Format::Ase => "Adobe swatches (.ase)",
            Format::Kpl => "Krita palette (.kpl)",
        }
    }

    pub(crate) fn mime(self) -> &'static str {
        match self {
            Format::Gpl => "text/plain",
            Format::Ase | Format::Kpl => "application/octet-stream",
        }
    }
}

/// The palette in a file called `name` (its name if the file has none).
pub(crate) fn read(name: &str, bytes: &[u8]) -> Result<Palette, String> {
    let stem = std::path::Path::new(name)
        .file_stem()
        .map_or_else(|| "Palette".into(), |s| s.to_string_lossy().into_owned());
    let mut palette =
        if bytes.starts_with(b"GIMP Palette") || bytes.starts_with(b"\xEF\xBB\xBFGIMP Palette") {
            read_gpl(&String::from_utf8_lossy(bytes))?
        } else if bytes.starts_with(b"ASEF") {
            read_ase(bytes)?
        } else if bytes.starts_with(crate::project::zip::SIGNATURE) {
            read_kpl(bytes)?
        } else if matches!(bytes.get(..2), Some([0, 1] | [0, 2])) {
            read_aco(bytes)?
        } else {
            return Err("Not a palette this app knows".into());
        };
    if palette.name.trim().is_empty() {
        palette.name = stem;
    }
    palette.tidy();
    if palette.len() == 0 {
        return Err("The palette has no colours".into());
    }
    Ok(palette)
}

pub(crate) fn write(palette: &Palette, format: Format) -> Result<Vec<u8>, String> {
    match format {
        Format::Gpl => Ok(write_gpl(palette).into_bytes()),
        Format::Ase => Ok(write_ase(palette)),
        Format::Kpl => write_kpl(palette),
    }
}

fn opaque(r: u8, g: u8, b: u8, name: &str) -> Swatch {
    Swatch {
        color: Color32::from_rgb(r, g, b),
        name: name.trim().to_string(),
    }
}

fn unit(v: f32) -> u8 {
    (v.clamp(0.0, 1.0) * 255.0).round() as u8
}

fn rgb(c: Color32) -> [u8; 3] {
    let [r, g, b, _] = c.to_srgba_unmultiplied();
    [r, g, b]
}

/// The groups' swatches in order, gaps left out.
fn swatches(group: &Group) -> impl Iterator<Item = &Swatch> {
    group.slots.iter().flatten()
}

// GIMP

const GPL_GROUP: &str = "# Group: ";

fn read_gpl(text: &str) -> Result<Palette, String> {
    let mut palette = Palette::new("", &[]);
    for line in text.lines().skip(1) {
        let line = line.trim();
        if let Some(name) = line.strip_prefix("Name:") {
            palette.name = name.trim().to_string();
        } else if let Some(n) = line.strip_prefix("Columns:") {
            if let Ok(n) = n.trim().parse::<usize>()
                && n > 0
            {
                palette.columns = n;
            }
        } else if let Some(name) = line.strip_prefix(GPL_GROUP) {
            palette.add_group(name.trim());
        } else if line.is_empty() || line.starts_with('#') {
            continue;
        } else {
            let fields: Vec<&str> = line.split_whitespace().collect();
            let channel = |i: usize| fields.get(i).and_then(|v| v.parse::<u8>().ok());
            let (Some(r), Some(g), Some(b)) = (channel(0), channel(1), channel(2)) else {
                return Err(format!("Damaged GIMP palette line: {line}"));
            };
            // Whatever follows the numbers is the name; apps write their
            // hex code or "Untitled" when there is none.
            let mut name = fields[3..].join(" ");
            let color = Color32::from_rgb(r, g, b);
            if name == "Untitled" || hex::parse(&name) == Some(color) {
                name.clear();
            }
            let group = palette.groups.last_mut().expect("a palette has a group");
            group.slots.push(Some(opaque(r, g, b, &name)));
        }
    }
    Ok(palette)
}

fn write_gpl(palette: &Palette) -> String {
    let mut out = format!(
        "GIMP Palette\nName: {}\nColumns: {}\n#\n",
        palette.name.replace('\n', " "),
        palette.columns()
    );
    for (i, group) in palette.groups.iter().enumerate() {
        if i > 0 {
            out.push_str(&format!("{GPL_GROUP}{}\n", group.name.replace('\n', " ")));
        }
        for s in swatches(group) {
            let [r, g, b] = rgb(s.color);
            let name = if s.name.is_empty() {
                hex::format(Color32::from_rgb(r, g, b))
            } else {
                s.name.replace('\n', " ")
            };
            out.push_str(&format!("{r:3} {g:3} {b:3}\t{name}\n"));
        }
    }
    out
}

// Adobe Swatch Exchange: big-endian blocks of colours and groups.

const ASE_GROUP_START: u16 = 0xC001;
const ASE_GROUP_END: u16 = 0xC002;
const ASE_COLOR: u16 = 0x0001;

struct Be<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl<'a> Be<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8], String> {
        let out = self
            .bytes
            .get(self.at..self.at + n)
            .ok_or("The palette file is cut short")?;
        self.at += n;
        Ok(out)
    }
    fn u16(&mut self) -> Result<u16, String> {
        Ok(u16::from_be_bytes(self.take(2)?.try_into().unwrap()))
    }
    fn u32(&mut self) -> Result<u32, String> {
        Ok(u32::from_be_bytes(self.take(4)?.try_into().unwrap()))
    }
    fn f32(&mut self) -> Result<f32, String> {
        Ok(f32::from_be_bytes(self.take(4)?.try_into().unwrap()))
    }
    /// `len` UTF-16 code units, the last of them maybe a terminating 0.
    fn utf16(&mut self, len: usize) -> Result<String, String> {
        let units: Vec<u16> = self
            .take(len * 2)?
            .chunks_exact(2)
            .map(|c| u16::from_be_bytes([c[0], c[1]]))
            .take_while(|&u| u != 0)
            .collect();
        Ok(String::from_utf16_lossy(&units))
    }
}

fn cmyk(c: f32, m: f32, y: f32, k: f32) -> [u8; 3] {
    let ink = |v: f32| unit((1.0 - v) * (1.0 - k));
    [ink(c), ink(m), ink(y)]
}

fn read_ase(bytes: &[u8]) -> Result<Palette, String> {
    let mut r = Be { bytes, at: 4 };
    let _version = (r.u16()?, r.u16()?);
    let blocks = r.u32()?;
    let mut palette = Palette::new("", &[]);
    let mut group = 0;
    for _ in 0..blocks {
        let kind = r.u16()?;
        let len = r.u32()? as usize;
        let mut b = Be {
            bytes: r.take(len)?,
            at: 0,
        };
        match kind {
            ASE_GROUP_START => {
                let n = b.u16()? as usize;
                let name = b.utf16(n)?;
                palette.add_group(&name);
                group = palette.groups.len() - 1;
            }
            ASE_GROUP_END => group = 0,
            ASE_COLOR => {
                let n = b.u16()? as usize;
                let name = b.utf16(n)?;
                let model = b.take(4)?;
                let [red, green, blue] = match model {
                    b"RGB " => [unit(b.f32()?), unit(b.f32()?), unit(b.f32()?)],
                    b"CMYK" => cmyk(b.f32()?, b.f32()?, b.f32()?, b.f32()?),
                    b"LAB " => cielab_to_rgb(b.f32()? * 100.0, b.f32()?, b.f32()?),
                    b"Gray" => [unit(b.f32()?); 3],
                    _ => continue,
                };
                palette.groups[group]
                    .slots
                    .push(Some(opaque(red, green, blue, &name)));
            }
            _ => {}
        }
    }
    Ok(palette)
}

fn write_ase(palette: &Palette) -> Vec<u8> {
    fn name(out: &mut Vec<u8>, s: &str) {
        let units: Vec<u16> = s.encode_utf16().chain([0]).collect();
        out.extend((units.len() as u16).to_be_bytes());
        out.extend(units.iter().flat_map(|u| u.to_be_bytes()));
    }
    let mut blocks: Vec<(u16, Vec<u8>)> = Vec::new();
    for (i, group) in palette.groups.iter().enumerate() {
        if i > 0 {
            let mut b = Vec::new();
            name(&mut b, &group.name);
            blocks.push((ASE_GROUP_START, b));
        }
        for s in swatches(group) {
            let mut b = Vec::new();
            name(&mut b, &s.name);
            b.extend(b"RGB ");
            for v in rgb(s.color) {
                b.extend((v as f32 / 255.0).to_be_bytes());
            }
            b.extend(2u16.to_be_bytes()); // A normal (not global or spot) colour.
            blocks.push((ASE_COLOR, b));
        }
        if i > 0 {
            blocks.push((ASE_GROUP_END, Vec::new()));
        }
    }
    let mut out = b"ASEF".to_vec();
    out.extend(1u16.to_be_bytes());
    out.extend(0u16.to_be_bytes());
    out.extend((blocks.len() as u32).to_be_bytes());
    for (kind, b) in blocks {
        out.extend(kind.to_be_bytes());
        out.extend((b.len() as u32).to_be_bytes());
        out.extend(b);
    }
    out
}

// Photoshop's colour swatches: a version 1 list of colours, and usually a
// version 2 list after it with their names.

fn read_aco(bytes: &[u8]) -> Result<Palette, String> {
    let mut r = Be { bytes, at: 0 };
    let mut swatches = Vec::new();
    while let Ok(version) = r.u16() {
        if version != 1 && version != 2 {
            break;
        }
        let count = r.u16()?;
        let mut list = Vec::with_capacity(count as usize);
        for _ in 0..count {
            let space = r.u16()?;
            let v: [u16; 4] = [r.u16()?, r.u16()?, r.u16()?, r.u16()?];
            let name = if version == 2 {
                let len = r.u32()? as usize;
                r.utf16(len)?
            } else {
                String::new()
            };
            let f = |i: usize| v[i] as f32 / 65535.0;
            let [red, green, blue] = match space {
                0 => [(v[0] >> 8) as u8, (v[1] >> 8) as u8, (v[2] >> 8) as u8],
                1 => {
                    let c = eframe::egui::ecolor::Hsva::new(f(0), f(1), f(2), 1.0).to_srgb();
                    [c[0], c[1], c[2]]
                }
                // 0 is full ink.
                2 => cmyk(1.0 - f(0), 1.0 - f(1), 1.0 - f(2), 1.0 - f(3)),
                7 => cielab_to_rgb(
                    v[0] as f32 / 100.0,
                    v[1] as i16 as f32 / 100.0,
                    v[2] as i16 as f32 / 100.0,
                ),
                8 => [unit(1.0 - v[0] as f32 / 10000.0); 3],
                _ => continue,
            };
            list.push(Some(opaque(red, green, blue, &name)));
        }
        // The named list, when there is one, says the same with names.
        swatches = list;
        if version == 2 {
            break;
        }
    }
    let mut palette = Palette::new("", &[]);
    palette.groups[0].slots = swatches;
    Ok(palette)
}

// Krita: a ZIP holding `colorset.xml`, with each swatch's place in the grid.

fn read_kpl(bytes: &[u8]) -> Result<Palette, String> {
    let entries = crate::project::zip::read_all(bytes)?;
    let xml = entries
        .iter()
        .find(|(name, _)| name == "colorset.xml")
        .ok_or("The Krita palette has no colorset.xml")?;
    let doc = crate::brush_engine::import::krita::parse_xml(&String::from_utf8_lossy(&xml.1))?;
    let root = doc
        .find("Colorset")
        .ok_or("The Krita palette has no colours")?;
    let mut palette = Palette::new(root.attr("name").unwrap_or_default(), &[]);
    if let Some(n) = root.attr("columns").and_then(|n| n.parse::<usize>().ok())
        && n > 0
    {
        palette.columns = n;
    }
    let columns = palette.columns();
    let entries_of = |node: &crate::brush_engine::import::krita::Node,
                      slots: &mut Vec<Option<Swatch>>| {
        let mut next = 0;
        for e in node.children.iter().filter(|c| c.name == "ColorSetEntry") {
            let Some(swatch) = kpl_swatch(e) else {
                continue;
            };
            let at = e
                .children
                .iter()
                .find(|c| c.name == "Position")
                .and_then(|p| {
                    let n = |k| p.attr(k)?.parse::<usize>().ok();
                    Some(n("row")? * columns + n("column")?.min(columns - 1))
                })
                .unwrap_or(next);
            // A damaged file's far-off position shouldn't make a huge grid.
            let at = at.min(1 << 16);
            if at >= slots.len() {
                slots.resize(at + 1, None);
            }
            slots[at] = Some(swatch);
            next = at + 1;
        }
    };
    entries_of(root, &mut palette.groups[0].slots);
    for g in root.children.iter().filter(|c| c.name == "Group") {
        palette.add_group(g.attr("name").unwrap_or_default());
        let group = palette.groups.last_mut().expect("just added");
        entries_of(g, &mut group.slots);
    }
    Ok(palette)
}

fn kpl_swatch(entry: &crate::brush_engine::import::krita::Node) -> Option<Swatch> {
    let name = entry.attr("name").unwrap_or_default();
    entry.children.iter().find_map(|c| {
        let v = |k: &str| c.attr(k)?.parse::<f32>().ok();
        let [r, g, b] = match c.name.as_str() {
            "RGB" | "sRGB" => [unit(v("r")?), unit(v("g")?), unit(v("b")?)],
            "Gray" => [unit(v("g")?); 3],
            "CMYK" => cmyk(v("c")?, v("m")?, v("y")?, v("k")?),
            "Lab" => cielab_to_rgb(v("L")?, v("a")?, v("b")?),
            _ => return None,
        };
        Some(opaque(r, g, b, name))
    })
}

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

fn write_kpl(palette: &Palette) -> Result<Vec<u8>, String> {
    let columns = palette.columns();
    let rows = |g: &Group| g.slots.len().div_ceil(columns).max(1);
    let entries = |g: &Group| {
        let mut out = String::new();
        for (i, s) in g.slots.iter().enumerate() {
            let Some(s) = s else { continue };
            let [r, gr, b] = rgb(s.color).map(|v| v as f32 / 255.0);
            out.push_str(&format!(
                " <ColorSetEntry name=\"{}\" id=\"\" spot=\"false\" bitdepth=\"U8\">\
                 <RGB space=\"sRGB-elle-V2-srgbtrc.icc\" r=\"{r}\" g=\"{gr}\" b=\"{b}\"/>\
                 <Position row=\"{}\" column=\"{}\"/></ColorSetEntry>\n",
                xml_escape(&s.name),
                i / columns,
                i % columns
            ));
        }
        out
    };
    let mut xml = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
         <Colorset version=\"2.0\" name=\"{}\" comment=\"\" columns=\"{columns}\" rows=\"{}\" readonly=\"false\">\n",
        xml_escape(&palette.name),
        rows(&palette.groups[0]),
    );
    xml.push_str(&entries(&palette.groups[0]));
    for g in &palette.groups[1..] {
        xml.push_str(&format!(
            "<Group name=\"{}\" rows=\"{}\">\n{}</Group>\n",
            xml_escape(&g.name),
            rows(g),
            entries(g)
        ));
    }
    xml.push_str("</Colorset>\n");
    let mut zip = crate::project::zip::ZipWriter::default();
    zip.add("mimetype", b"krita/x-colorset")?;
    zip.add("colorset.xml", xml.as_bytes())?;
    zip.add("profiles.xml", b"<Profiles/>\n")?;
    zip.finish()
}

/// The palettes that come with the app, and each one's category, in
/// the order of `assets/palettes.gpl`.
pub(crate) fn presets() -> &'static [(String, Palette)] {
    static PRESETS: std::sync::OnceLock<Vec<(String, Palette)>> = std::sync::OnceLock::new();
    PRESETS.get_or_init(|| {
        let text = include_str!("../../assets/palettes.gpl");
        text.split("GIMP Palette\n")
            .skip(1)
            .filter_map(|gpl| {
                let category = gpl.lines().find_map(|l| l.strip_prefix("# Category: "))?;
                let mut palette = read_gpl(&format!("GIMP Palette\n{gpl}")).ok()?;
                palette.tidy();
                Some((category.to_string(), palette))
            })
            .collect()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn presets_all_read() {
        let text = include_str!("../../assets/palettes.gpl");
        assert_eq!(presets().len(), text.matches("GIMP Palette\n").count());
        assert!(presets().iter().all(|(_, p)| p.len() > 0));
    }

    fn sample() -> Palette {
        let mut p = Palette::new("Skin & sky", &[Color32::from_rgb(200, 150, 120)]);
        p.columns = 4;
        p.groups[0].slots[0].as_mut().unwrap().name = "Light <skin>".into();
        // A gap, then a colour on the second row.
        p.set((0, 5), Some(Swatch::new(Color32::from_rgb(90, 60, 50))));
        p.add_group("Sky");
        p.add_rows(1, &[Color32::from_rgb(40, 90, 200), Color32::WHITE]);
        p
    }

    fn colors(p: &Palette) -> Vec<(usize, Color32)> {
        (p.groups.iter().enumerate())
            .flat_map(|(g, group)| swatches(group).map(move |s| (g, s.color)))
            .collect()
    }

    #[test]
    fn each_format_reads_back_what_it_wrote() {
        let p = sample();
        for format in Format::ALL {
            let bytes = write(&p, format).unwrap();
            let back = read(&format!("x.{}", format.extension()), &bytes).unwrap();
            // Adobe's has no name for the palette: the file's is used.
            let name = if format == Format::Ase {
                "x"
            } else {
                p.name.as_str()
            };
            assert_eq!(back.name, name, "{format:?}");
            assert_eq!(colors(&back), colors(&p), "{format:?}");
            assert_eq!(back.groups[1].name, "Sky", "{format:?}");
            assert_eq!(back.get((0, 0)).unwrap().name, "Light <skin>", "{format:?}");
        }
        // Only Krita's keeps the gap.
        let kpl = read("x.kpl", &write(&p, Format::Kpl).unwrap()).unwrap();
        assert_eq!(kpl.groups[0].slots, p.groups[0].slots);
        assert_eq!(kpl.columns, 4);
    }

    #[test]
    fn a_gimp_palette_from_elsewhere() {
        let text = "GIMP Palette\nName: Test\nColumns: 3\n#\n# comment\n255   0   0\tRed one\n  0 128 255 Untitled\n";
        let p = read("t.gpl", text.as_bytes()).unwrap();
        assert_eq!(p.name, "Test");
        assert_eq!(p.columns, 3);
        assert_eq!(p.get((0, 0)).unwrap().name, "Red one");
        assert_eq!(p.get((0, 1)).unwrap().name, "");
        assert_eq!(p.get((0, 1)).unwrap().color, Color32::from_rgb(0, 128, 255));
        assert!(read("t.gpl", b"GIMP Palette\n1 2\n").is_err());
        assert!(read("t.txt", b"hello").is_err());
    }

    #[test]
    fn photoshop_swatches_in_each_colour_space() {
        // Version 1 then version 2 (with names): RGB, CMYK (0 is full
        // ink, so this is white), Lab mid grey, grayscale black.
        let colours: [(u16, [u16; 4]); 4] = [
            (0, [65535, 0, 0, 0]),
            (2, [65535, 65535, 65535, 65535]),
            (7, [5000, 0, 0, 0]),
            (8, [10000, 0, 0, 0]),
        ];
        let mut bytes = Vec::new();
        for version in [1u16, 2] {
            bytes.extend(version.to_be_bytes());
            bytes.extend((colours.len() as u16).to_be_bytes());
            for (i, (space, v)) in colours.iter().enumerate() {
                bytes.extend(space.to_be_bytes());
                v.iter().for_each(|w| bytes.extend(w.to_be_bytes()));
                if version == 2 {
                    let name: Vec<u16> = format!("c{i}").encode_utf16().chain([0]).collect();
                    bytes.extend((name.len() as u32).to_be_bytes());
                    name.iter().for_each(|u| bytes.extend(u.to_be_bytes()));
                }
            }
        }
        let p = read("p.aco", &bytes).unwrap();
        let got: Vec<_> = swatches(&p.groups[0])
            .map(|s| (s.color, s.name.as_str()))
            .collect();
        assert_eq!(got[0], (Color32::from_rgb(255, 0, 0), "c0"));
        assert_eq!(got[1].0, Color32::WHITE);
        let [r, g, b] = rgb(got[2].0);
        assert!(r.abs_diff(119) <= 2 && r == g && g == b, "{:?}", got[2]);
        assert_eq!(got[3].0, Color32::BLACK);
        assert_eq!(p.name, "p");
    }
}
