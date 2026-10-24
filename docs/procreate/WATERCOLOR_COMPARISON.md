# "Free Realistic Watercolor (Stas)": Procreate vs rusty-painter

The set is 38 brushes (52 archives: 13 brushes have a dual sub-brush in `Sub01/`). For each feature it uses: what Procreate does (from `NOTES.md`), what the importer did before, and what it does now.

| Feature (how many archives use it) | Procreate | Before | Now |
|---|---|---|---|
| **Wet edges** (31; mostly 0.9–1.0) | Each pixel's coverage c is mixed towards 1 − \|sin(4.5c)/4.5c\|: the soft band of each stamp turns solid and full paint thins to about 78% | ×0.25 into this app's blur rim, so faint, and a different look (pooled at the stroke's outline) | `WetEdgeStyle::Ring`: the same per-pixel curve, no blur, unscaled |
| **Burnt edges** (19; multiply ×37, colour burn ×7) | Where paint is thin, the stroke is blended over what's under it with a second blend mode, weighted (1 − c)·amount | Ignored | `Brush.burnt_edges`, applied after the stroke is resolved, only where it changed pixels (selections and alpha lock kept); deep documents written at full depth |
| **Rendering mode** (recursive 27, max transfer 21, modulated 18) | Glazes add each stamp at the glaze flow; "max transfer" caps the stroke at its opacity; "modulated" lets pressure drive the flow | Ignored: every brush built up at full flow | Max transfer → `PaintingMode::Wash`, otherwise build-up; flow from `dynamicsGlazedFlow`; modulated → pressure on flow. Older brushes fall back to `dynamicsRenderType`, then `dynamicsGlazeType`, then `dynamicsGlaze` |
| **Wet Mix** (24 with dilution or charge) | Per stamp on the GPU: canvas colour pulled into a carry buffer, brush colour mixed in, laid down by grade; the carry continues across segments | Ignored (an import note said so) | Approximated with `Mixing`: colour rate = 1 − dilution, load from charge, smudge length from pull. **Needs tuning by eye**: the two models aren't equivalent |
| **Blend modes** (brush 1/6/8; dual 6/7/9/10/11; burnt 0/1/9/10/25) | 0–26 document numbers (table in NOTES.md, Pass 4) | Only 1 (multiply); dual 4/7 were guesses, and wrong | Full table for the brush and burnt modes; dual mapped to the nearest of the 5 dual modes (subtract, burn, darken, height, multiply) |
| **Grain blend mode** (27 ×5, 7 ×4, 6 ×2…) | Height and linear-height formulas (12·depth·coverage − grain), or a blend mode mixed by depth | Ignored (always multiply) | 27/28 → height, 7/8 → subtract, 9 → colour dodge, 20 → hard mix, the rest multiply |
| **Wetness jitter** (12) | Each stamp's wetness varies at random | Ignored | A random-per-dab input on smudge length and colour rate, at the jitter's amount |
| **Wet Mix blur** (8; 4 at ≥ 0.25) | The paint picked up is blurred | Ignored | At ≥ 0.25, Krita's dulling smudge (one colour averaged over that share of the dab); fainter blur stays a smear |
| **Gamma-correct blending** (2) | Mixes in squared colour space | Ignored | `Mixing.gamma_correct`: that brush mixes in linear light even in a gamma document |
| **Dense spacing** (`plotSpacing` 0–0.04) | Gap has a floor that grows with size | Clamped to ≥ 1% | Unchanged; fine |

## Built-in images
34 of 38 brushes name one of Procreate's own grains and 22 a built-in shape, and neither is in the `.brushset`. The importer now loads them by name from a folder you supply: `$RUSTY_PAINTER_PROCREATE_IMAGES`, else `procreate-images` in the app's data folder. That folder is the `ValkyrieResources.bundle/Brushes/Images` folder of your own Procreate (287 images). They're Procreate's, so the app doesn't ship them. With the folder in place, every brush in the set imports with its own grain and shape.

## What's still approximate
1. **Wet Mix model.** Procreate pulls canvas colour into a carry buffer, mixes in the brush colour, and lays it down by grade per stamp. This app mixes along the stroke instead, by smudge length and colour rate. Long wet strokes will differ. Tuning `wet_mix()` needs Procreate output to compare against.
2. **Burnt edges on gamma documents** blend in linear light, where Procreate blends in its own space. A slight colour difference is possible.
3. **Dual-brush and grain blend modes** map to the nearest of this engine's 5 modes each. Procreate's others (screen, overlay, …) fall back to multiply.

## Brushes worth checking first
"Wide Brush (Dry + Wet) #3/#4" (wet mix plus wet edges), "Splatter Flow Brush (Edge)" (burnt edges), "Round Glaze Brush" (rendering mode), and "Blender Brush" / "Edge Softener #2" (wet mix with no colour of their own).
