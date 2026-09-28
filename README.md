# Pixel

A photo editor for Omarchy. The first version sets up a canvas, stacks raster layers, and exports a flattened image. Crop, resize, rotate, and flip are the editing tools. Painting, masks, and Photoshop files are not in this version.

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
| Fit / actual size | Ctrl+0 / Ctrl+1 |
| Select / move / crop | V / M / C |
| Add to or remove from the selection | Shift-click, or Shift-drag a box |
| Nudge the selected layers | Arrow keys, Shift for 10 px |

Projects are zip files with a `.pixel` extension. Export writes PNG, JPEG, or lossless WebP and does not change the open document. JPEG is flattened onto the canvas background, or onto white when the background is transparent.
