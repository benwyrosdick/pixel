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
cargo build --release
install -Dm755 target/release/pixel ~/.local/bin/pixel
install -Dm644 assets/pixel.desktop ~/.local/share/applications/pixel.desktop
install -Dm644 assets/pixel.svg ~/.local/share/icons/hicolor/scalable/apps/pixel.svg
install -Dm644 assets/pixel.xml ~/.local/share/mime/packages/pixel.xml
update-desktop-database ~/.local/share/applications
update-mime-database ~/.local/share/mime
```

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
| Move / crop | V / C |
| Nudge the active layer | Arrow keys, Shift for 10 px |

Projects are zip files with a `.pixel` extension. Export writes PNG, JPEG, or lossless WebP and does not change the open document. JPEG is flattened onto the canvas background, or onto white when the background is transparent.
