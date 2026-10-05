# Pixel

A photo editor for Omarchy. The first version sets up a canvas, stacks raster layers, and exports a flattened image. Crop, resize, rotate, and flip are the editing tools, with color adjustments, blur and sharpen, and blend modes. Text and shape layers stay editable, and layers can be grouped. Painting, masks, and Photoshop files are not in this version.

## Run

```bash
cargo run --release
```

Pass an image or a `.pixel` project to open it:

```bash
cargo run --release -- photo.png
```

## Install locally

```bash
just install
```

This builds a release binary and installs it to `~/.local/bin`, with its desktop entry, icon, and `.pixel` file type under `~/.local/share`. `just uninstall` removes them. Run `just` to list the other recipes.

The window reads `~/.local/state/omarchy/current/theme/colors.toml` and follows the active Omarchy theme. If that file is missing, it uses a dark fallback.

## Shortcuts

| Action | Shortcut |
| --- | --- |
| New canvas | Ctrl+N |
| Open image | Ctrl+O |
| Save project | Ctrl+S |
| Save as | Ctrl+Shift+S |
| Export | Ctrl+Shift+E |
| Undo / redo | Ctrl+Z / Ctrl+Shift+Z |
| Cut / copy / paste | Ctrl+X / Ctrl+C / Ctrl+V |
| Copy the whole image | Ctrl+Shift+C |
| Zoom in / out | + / − |
| Fit / actual size | Ctrl+0 / Ctrl+1 |
| Select / move / crop | V / M / C |
| Text / shape | T / U |
| Group / ungroup | Ctrl+G / Ctrl+Shift+G |
| Select all / deselect | Ctrl+A / Ctrl+Shift+A or Esc |
| Add to or remove from the selection | Shift-click, or Shift-drag a box |
| Nudge the selected layers | Arrow keys, Shift for 10 px |
| Delete the selected layers | Delete or Backspace |
| Duplicate layer | Ctrl+J |
| Raise / lower layer | Ctrl+] / Ctrl+[ |
| Show rulers / guides | Ctrl+R / Ctrl+; |
| Snapping on or off | Ctrl+Shift+; |
| Skip snapping for one drag | Hold Ctrl |

Dragging a layer or a resize handle snaps its edges and centre to the canvas, to what other layers draw, and to guides, with a pink line showing what it snapped to. Drag out of a ruler to add a guide, drag a guide to move it, and drag it back onto its ruler to remove it. Guides are saved in the project. The Position and size fields under the layers set the selected layer's exact box.

The Text tool adds text where you click, or changes the text you click on. The Shape tool draws rectangles, ellipses, lines, and arrows, and its options restyle the selected shape. Moving and resizing keep text and shapes editable. Rotating, flipping, or adjusting one turns it into plain pixels, as Layer ▸ Rasterize does.

Layer ▸ Group gathers the selected layers into a group, which moves, hides, and fades as one. Drop a layer onto a group's row to put it inside. A group at full opacity in Normal mode lets its layers blend with what is below it; below full opacity, or in another mode, its layers are flattened together first.

The Adjust menu adjusts the selected layer: brightness and contrast, hue and saturation, levels, grayscale, blur, and sharpen. Each dialog previews on the canvas until you apply or cancel it. Adjustments aren't baked in: the layer keeps its original pixels, and right-clicking its row lists the adjustments to take off one at a time or all at once. They survive moving, resizing, rotating, and saving. Each layer has a blend mode, such as Multiply, Screen, or Overlay, picked under the opacity slider.

The Crop tool trims the canvas, or with Crop set to Selected layer, just that layer: drag over the part to keep, then apply.

Image ▸ Trim to Content crops the canvas to the pixels the visible layers draw, ignoring the canvas background. File ▸ Open Recent and the welcome screen list the files Pixel opened or saved recently.

Paste or drop an image to add it as a layer, or to open it when nothing is open. Copy takes the selected layers, flattened and trimmed to what they draw, or the whole image when nothing is selected.

Projects are zip files with a `.pixel` extension. Export writes PNG, JPEG, or lossless WebP and does not change the open document. JPEG is flattened onto the canvas background, or onto white when the background is transparent.
