# Colorize mask

A colorize mask (Krita's "lazy brush") fills flat colours into line art from rough scribbles. No AI: it follows the lines.

1. Select the line-art layer and choose **Layer → New Colorize Mask**. A layer named "Colorize 1" appears just above it, set to Multiply so the lines show through the colours.
2. Paint on it with any brush: a quick scribble inside each area, in the colour that area should be. The scribbles are *hints*.
3. Lift the pen. A moment later every area enclosed by the lines fills with the colour of the hint inside it.

The colours update by themselves whenever the hints or the line art change (after a stroke, an undo, an edit to the lines). Erase a hint to take it away; undo and redo work as on any layer.

## Areas without a hint

Every area takes a colour: one with no hint of its own takes a neighbour's, as in Krita. To leave an area empty (usually the background around the figure), scribble in it with the **transparent key** colour (magenta, `#FF00FF`, at first).

## Settings

**Layer → Colorize Mask**, with the mask selected:

- **Update Now**: make the colours again at once.
- **Show hints**: show the scribbles over the colours. They're never exported, copied merged or merged, whether shown or not.
- **Close gaps**: openings in the lines up to this many pixels wide don't let colour through (as the Fill tool's gap closing). Colours from both sides meet in the opening.
- **Extend under lines**: how far colour goes in under the lines from their edge, so no light halo is left under anti-aliased lines. Thick lines aren't filled solid.
- **Line art**: which layer's lines enclose the areas (dark or opaque pixels count: black lines on transparency or on white paper both work).
- **Transparent key**: hints in this colour leave their area empty. **Brush colour** sets it to the current colour.
- **Convert to Paint Layer**: the colours become a plain paint layer in the mask's place (the hints go). One undo step brings the mask back.

## Tips

- Scribble well inside each area, away from its lines: a hint squeezed against a line can lose its area to a neighbour through a gap.
- Merging, export and Copy Merged use the colours as shown (without the hints). Saved projects keep the hints and settings; the colours are made again on opening.
