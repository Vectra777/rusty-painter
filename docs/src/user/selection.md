# Selections

| Tool | Key |
|---|---|
| Rectangle / ellipse | `M` |
| Lasso (with smoothing); again for polygon, magnetic lasso | `L` |
| Magic wand | `Q` |
| Quick mask | `Shift+Q` |

Also colour range and the selection brush. Each tool replaces, adds, erases or intersects; every change is undoable.

- `Ctrl+A` selects all, `Ctrl+D` or `Esc` deselects, `Ctrl+Shift+I` inverts, `Delete` erases the selected pixels.
- Select → Modify grows, shrinks, feathers or borders the selection.
- Ctrl+click a layer's thumbnail selects its paint (Shift adds, Alt subtracts).
- Selections can be saved by name with the project and loaded back.
- **Quick Mask** (`Shift+Q`) paints the selection with the brush and eraser.
- The Transform tool moves, scales, rotates, puts in perspective or distorts the selection outline itself when there's nothing under it.

## Smart Patch

Content-aware fill of the selection from its surroundings (PatchMatch, `Shift+F5`), or paint over something with the selection brush to remove it.

## Fill

`G` fills; press it again for enclose fill and lasso delete. Fills read the layers marked as [reference](layers.md).
