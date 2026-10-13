# Layers

The layers panel adds and removes layers, drags them to reorder, toggles visibility, sets opacity and blend mode, and picks the active layer. Each layer also has:

- **Lock position**: it can't be moved.
- **Draft**: shown, but left out of export and merging.
- **Reference**: what fills, the magic wand and colour range look at.

Ctrl+click a thumbnail to select its paint (Shift adds, Alt subtracts).

## Folders, masks and clipping

- **Folders**: Normal, or Pass Through, which blends their layers straight into what's below, as Photoshop and Krita do.
- **Masks** hide parts of a layer.
- **Clipping masks** (`Ctrl+Alt+G`): a layer shows only where the one below has paint.

## Merging

Merge Down (`Ctrl+Alt+E`), Merge Visible (`Ctrl+Shift+E`) and Flatten Image, as shown on screen. `Ctrl+J` duplicates a layer.

## Adjustment layers

Filter → New Adjustment Layer: brightness/contrast, hue/saturation, levels, curves, colour balance, gradient map, exposure, temperature, vibrance, sepia, solarize, invert, desaturate, posterize or threshold over everything below, live.

## Fill layers

Layer → New Fill Layer: a colour or a gradient (linear, radial, reflected or angle, any colours). Its mask or clipping says where it shows.

## Borders

Layer → Border…: width, colour and opacity around any layer's paint. It follows the paint as you draw and becomes pixels when merged.

## Vector layers

Layer → New Vector Layer: the brush draws lines that stay lines, smoothed and pressure-sensitive.

- The eraser takes out whole lines, only the part it touches, or the stretch up to where other lines cross it.
- Edit Lines (Layer → Vector) bends a line by its handles, widens it around one or moves it.
- Layer → Vector thickens or thins every line, recolours them or rasterises the layer.
- Any other pixel tool turns it into a plain layer (undo brings the lines back).

## Text layers

The Text tool types onto a text layer in any shipped or system font, with size, alignment, spacing and colour; drag it into place before keeping it. The text stays editable: click it with the Text tool or double-click the layer. Painting on it turns it into pixels (Layer → Rasterise Text does it on purpose).

## Shader layers

The **fx** button in the layers panel adds a layer drawn by a GLSL shader, written Shadertoy style:

```glsl
void mainImage(out vec4 fragColor, in vec2 fragCoord) {
    vec2 uv = fragCoord / iResolution.xy;
    fragColor = vec4(uv, 0.5 + 0.5 * sin(iTime), 1.0);
}
```

It has `iTime`, `iResolution`, `iMouse`, `iFrame`, and `texture(iChannel0, uv)` reads the layers below it. It animates live on the GPU, under and over your painting, with the layer's blend mode and opacity.

Each shader layer gets its own editor window (double-click the layer): syntax colouring, errors at their line as you type (the canvas keeps the last version that worked), play/pause, speed and templates (plasma, rings, clouds, stars, ripple, chromatic split, glow, vignette). Export, merging and saving use the current frame.

It plays live unless the shader layer is in a folder, has a mask, clipping or a border, or has an adjustment layer above it; then it shows as a still frame updated every second.
