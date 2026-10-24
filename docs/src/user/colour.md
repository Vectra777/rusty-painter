# Colour

## The colour panel

The brush colour on the right rail opens it: a triangle HSVA picker with an opacity slider, recent colours and the palette shown. The wheel shows colour harmonies (complementary, split, analogous, triadic, tetradic). `I` is the eyedropper.

**Mixing palette** (View → Mixing palette, under the colours): paint on it with the brush, smear with the smudge (or a colour-mixing brush), right-click or Alt+click to pick a colour. It's kept between sessions.

## Palettes

A palette is a grid of swatches with a set number of columns, which can have gaps (to keep a ramp to a row) and named groups below its first rows, as Krita's do. The colour panel shows one, picked from its drop-down:

- Click a swatch to paint with it; drag it to another place (onto a gap, or onto a swatch to swap them); right-click (long-press on a touch screen) to name it, replace it with the brush colour or remove it.
- Click a gap to put the brush colour there; + adds it at the end.

The Palette window (the palette button on the left) manages them:

- **New, Duplicate, Rename, Delete**, the number of columns, and groups (added, renamed, removed: a removed group's colours join the rows above).
- **Keep in this document**: the palette is saved in the project file instead of with your own, and goes where the document goes.
- **Sort** by value (dark to light), hue (the greys, then round the colour wheel), or saturation, or **Rows by hue**: a row for each hue family, dark to light along it. Hue and lightness are measured in Oklab, so the order matches how the colours look.
- **Open palette file…** opens GIMP (`.gpl`, which Inkscape, Aseprite and many sites use), Adobe (`.ase`, `.aco`) and Krita (`.kpl`) palettes as new palettes; dropping one on the window works too. **Save as…** writes `.gpl`, `.ase` or `.kpl`. Only Krita's keeps the gaps; Adobe's keeps groups, and a `.gpl` keeps them as comments GIMP ignores.
- **Take colours from** the picture, the selected layer or an image file (drop a picture on the window): the main colours, added to the palette shown as new rows, or as a new palette with a row for each hue.
- **Ramp from the brush colours**: the brush colour's shades from near black to near white (the colour itself among them), with its saturation easing off away from it and the hue turned toward blue in the shadows and yellow in the lights by up to the hue shift; or the steps from the primary colour to the secondary, even in Oklab. Click a step to paint with it, or add the ramp as a row.
- **Recolour the selected layer** to the palette (or the colours just taken), optionally dithered.

## Colour depth

A document keeps 8 bits per channel, 16 bits, or 32-bit floats in linear light: pick it for a new canvas, or Image → Colour Depth. Brush strokes, gradients, colour adjustments and the Image menu's operations work at the document's full depth, so soft airbrushing and gradients don't band and faint glazes build up; undo, saved files and 16-bit/float exports keep it.

At full depth: brushes, smudge, colour adjustments, Gaussian blur and sharpen, fills, liquify and transforms. At 8 bits on the tiles they change: wet paint, the other spatial filters and the remaining tools.

## Colour management

A document has a colour profile (sRGB, Display P3, Adobe RGB, Rec. 2020 or any RGB ICC profile). Image → Colour Profile assigns or converts it (with a rendering intent).

- The canvas is shown converted for your monitor's profile (View → Colour Management).
- View → Colour Management → Proof Colours shows how it would print on a CMYK profile (yours, or one found on the system), with a gamut warning.
- Exports embed the profile (PNG, JPEG, WebP, TIFF, PSD); pictures and Photoshop/Krita files come in with theirs; TIFF (CMYK) writes print-ready CMYK.

Live shader layers show unconverted.
