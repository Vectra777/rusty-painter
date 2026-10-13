# Transforms

`V` or `T` transforms the selection or the layer: move, rotate, scale and flip with a live preview. `Enter` applies, `Esc` cancels. The selection follows what was transformed.

- **Perspective**: drag the corners of a plane seen at an angle.
- **Distort**: a grid of points, 1×1 to 5×5 cells, inner points included: the picture bends smoothly through them.

## Liquify

`W`: push the paint along with the brush, twirl it either way, pinch it in or bloat it out; Reconstruct eases it back to the original and Smooth evens out the distortion.

## Puppet Warp

Edit → Puppet Warp (`Shift+W`): pin the paint where you click and drag the pins to bend it (an arm, a tail, a pose), the rest following as rigidly as it can, on a mesh that follows the drawing's outline.

- Shift+click picks several pins, Alt+click removes one.
- Alt+drag turns the paint round one pin (or fixes its angle).
- Depth says which part goes on top where it folds.
- Rigid, Normal or Distort; Density and Expansion.
- `Ctrl+Z` steps back through the pins.

It works on the layer or only the selected part (text and vector layers are rasterised first). The layer remembers the warp (saved with it): Puppet Warp on it again reopens it, pins and all, until the layer is changed some other way.

On a rig layer, Puppet Warp keys the pins at the frame showing: see [Animation](animation.md#rigs).
