# Colour

## The colour panel

The brush colour on the right rail opens it: a triangle HSVA picker with an opacity slider, recent colours and swatches. The wheel shows colour harmonies (complementary, split, analogous, triadic, tetradic). `I` is the eyedropper.

**Mixing palette** (View → Mixing palette, under the colours): paint on it with the brush, smear with the smudge (or a colour-mixing brush), right-click or Alt+click to pick a colour. It's kept between sessions.

A picture dropped on the Palette window gives its colours.

## Colour depth

A document keeps 8 bits per channel, 16 bits, or 32-bit floats in linear light: pick it for a new canvas, or Image → Colour Depth. Brush strokes, gradients, colour adjustments and the Image menu's operations work at the document's full depth, so soft airbrushing and gradients don't band and faint glazes build up; undo, saved files and 16-bit/float exports keep it.

At full depth: brushes, smudge, colour adjustments, Gaussian blur and sharpen, fills, liquify and transforms. At 8 bits on the tiles they change: wet paint, the other spatial filters and the remaining tools.

## Colour management

A document has a colour profile (sRGB, Display P3, Adobe RGB, Rec. 2020 or any RGB ICC profile). Image → Colour Profile assigns or converts it (with a rendering intent).

- The canvas is shown converted for your monitor's profile (View → Colour Management).
- View → Colour Management → Proof Colours shows how it would print on a CMYK profile (yours, or one found on the system), with a gamut warning.
- Exports embed the profile (PNG, JPEG, WebP, TIFF, PSD); pictures and Photoshop/Krita files come in with theirs; TIFF (CMYK) writes print-ready CMYK.

Live shader layers show unconverted.
