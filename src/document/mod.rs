//! A layered raster document and the undoable commands that edit it.

mod composite;
mod handles;
mod io;
mod ops;

pub use composite::{composite, composite_with, LayerOverride};
pub use handles::{
    clockwise_delta, hit_handle, pointer_angle, resize_rect, rotate_handle_point, rotated_bounds,
    snap_angle, Handle, PixelRect, HANDLE_RADIUS, ROTATE_OFFSET,
};
pub use io::{export, open_image, open_project, save_project, ExportFormat};
pub use ops::rotate_bitmap;

use image::RgbaImage;
use serde::{Deserialize, Serialize};

/// How many undo steps to keep. Pixel-changing steps store full layer buffers.
const UNDO_LIMIT: usize = 30;

/// Longest side accepted for a new or resized canvas.
pub const MAX_EDGE: u32 = 8192;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("width and height must be at least 1")]
    ZeroSize,
    #[error("canvas edges must be at most {MAX_EDGE} pixels")]
    TooLarge,
    #[error("no such layer")]
    BadLayer,
    #[error("the last layer can't be removed")]
    LastLayer,
    #[error("the crop is empty")]
    EmptyCrop,
    #[error("could not read the image: {0}")]
    Decode(String),
    #[error("could not write the file: {0}")]
    Write(String),
    #[error("that file is not a Pixel project")]
    BadProject,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum BlendMode {
    Normal,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Background {
    Transparent,
    Solid([u8; 4]),
}

impl Background {
    pub fn white() -> Self {
        Self::Solid([255, 255, 255, 255])
    }

    pub fn black() -> Self {
        Self::Solid([0, 0, 0, 255])
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Anchor {
    NorthWest,
    North,
    NorthEast,
    West,
    Center,
    East,
    SouthWest,
    South,
    SouthEast,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QuarterTurn {
    Cw,
    Ccw,
    Half,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Axis {
    Horizontal,
    Vertical,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScaleFilter {
    Nearest,
    Triangle,
    Lanczos3,
}

impl ScaleFilter {
    fn image_filter(self) -> image::imageops::FilterType {
        match self {
            Self::Nearest => image::imageops::FilterType::Nearest,
            Self::Triangle => image::imageops::FilterType::Triangle,
            Self::Lanczos3 => image::imageops::FilterType::Lanczos3,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Layer {
    pub id: u64,
    pub name: String,
    pub visible: bool,
    pub opacity: f32,
    pub blend: BlendMode,
    pub x: i32,
    pub y: i32,
    pub pixels: RgbaImage,
}

impl Layer {
    pub fn width(&self) -> u32 {
        self.pixels.width()
    }

    pub fn height(&self) -> u32 {
        self.pixels.height()
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Document {
    pub width: u32,
    pub height: u32,
    pub ppi: f32,
    pub background: Background,
    layers: Vec<Layer>,
    active: usize,
    next_id: u64,
}

#[derive(Debug, Clone, Copy)]
pub struct NewCanvas {
    pub width: u32,
    pub height: u32,
    pub ppi: f32,
    pub background: Background,
}

impl Document {
    pub fn new(spec: NewCanvas) -> Result<Self, Error> {
        check_size(spec.width, spec.height)?;
        if !(spec.ppi.is_finite() && spec.ppi > 0.0) {
            return Err(Error::ZeroSize);
        }
        let mut doc = Self {
            width: spec.width,
            height: spec.height,
            ppi: spec.ppi,
            background: spec.background,
            layers: Vec::new(),
            active: 0,
            next_id: 1,
        };
        let layer = blank_layer(&mut doc, "Layer 1");
        doc.insert_layer(layer);
        Ok(doc)
    }

    /// A document whose single layer is `image`, placed at the origin.
    pub fn from_image(image: RgbaImage, ppi: f32) -> Result<Self, Error> {
        let (width, height) = image.dimensions();
        check_size(width, height)?;
        let ppi = if ppi.is_finite() && ppi > 0.0 {
            ppi
        } else {
            72.0
        };
        let mut doc = Self {
            width,
            height,
            ppi,
            background: Background::Transparent,
            layers: Vec::new(),
            active: 0,
            next_id: 1,
        };
        let mut layer = blank_layer(&mut doc, "Layer 1");
        layer.pixels = image;
        doc.insert_layer(layer);
        Ok(doc)
    }

    pub fn layers(&self) -> &[Layer] {
        &self.layers
    }

    pub fn active_index(&self) -> usize {
        self.active
    }

    pub fn active_layer(&self) -> &Layer {
        &self.layers[self.active]
    }

    /// The topmost visible layer with a non-transparent pixel at a document
    /// point. Pixels outside the canvas are not drawn, so they never hit.
    pub fn layer_at(&self, x: f64, y: f64) -> Option<usize> {
        if !(x >= 0.0 && y >= 0.0 && x < self.width as f64 && y < self.height as f64) {
            return None;
        }
        let (px, py) = (x.floor() as i64, y.floor() as i64);
        self.layers.iter().rposition(|layer| {
            if !layer.visible || layer.opacity <= 0.0 {
                return false;
            }
            let lx = px - layer.x as i64;
            let ly = py - layer.y as i64;
            lx >= 0
                && ly >= 0
                && lx < layer.width() as i64
                && ly < layer.height() as i64
                && layer.pixels.get_pixel(lx as u32, ly as u32)[3] > 0
        })
    }

    fn layer_mut(&mut self, index: usize) -> Result<&mut Layer, Error> {
        self.layers.get_mut(index).ok_or(Error::BadLayer)
    }

    fn insert_layer(&mut self, layer: Layer) {
        self.layers.push(layer);
        self.active = self.layers.len() - 1;
    }

    pub(crate) fn from_parts(
        width: u32,
        height: u32,
        ppi: f32,
        background: Background,
        layers: Vec<Layer>,
        active: usize,
        next_id: u64,
    ) -> Result<Self, Error> {
        check_size(width, height)?;
        if layers.is_empty() || active >= layers.len() {
            return Err(Error::BadProject);
        }
        let mut seen = std::collections::BTreeSet::new();
        for layer in &layers {
            if !seen.insert(layer.id) {
                return Err(Error::BadProject);
            }
        }
        Ok(Self {
            width,
            height,
            ppi: if ppi.is_finite() && ppi > 0.0 {
                ppi
            } else {
                72.0
            },
            background,
            layers,
            active,
            next_id: next_id.max(1),
        })
    }
}

fn check_size(width: u32, height: u32) -> Result<(), Error> {
    if width == 0 || height == 0 {
        Err(Error::ZeroSize)
    } else if width > MAX_EDGE || height > MAX_EDGE {
        Err(Error::TooLarge)
    } else {
        Ok(())
    }
}

fn blank_layer(doc: &mut Document, name: &str) -> Layer {
    let id = doc.next_id;
    doc.next_id += 1;
    Layer {
        id,
        name: name.to_string(),
        visible: true,
        opacity: 1.0,
        blend: BlendMode::Normal,
        x: 0,
        y: 0,
        pixels: RgbaImage::new(doc.width, doc.height),
    }
}

/// Edits the UI is allowed to make. Selection changes are not commands.
#[derive(Debug, Clone)]
pub enum Command {
    AddLayer,
    AddImageLayer {
        name: String,
        image: RgbaImage,
    },
    DuplicateLayer {
        index: usize,
    },
    DeleteLayer {
        index: usize,
    },
    /// `to` is the index the layer occupies after the move.
    Reorder {
        from: usize,
        to: usize,
    },
    Rename {
        index: usize,
        name: String,
    },
    SetVisibility {
        index: usize,
        visible: bool,
    },
    SetOpacity {
        index: usize,
        opacity: f32,
    },
    MoveLayer {
        index: usize,
        x: i32,
        y: i32,
    },
    Crop {
        x: i32,
        y: i32,
        width: u32,
        height: u32,
    },
    ResizeCanvas {
        width: u32,
        height: u32,
        anchor: Anchor,
    },
    ScaleDocument {
        width: u32,
        height: u32,
        filter: ScaleFilter,
    },
    /// Resample one layer and place its top-left at `(x, y)`.
    ScaleLayer {
        index: usize,
        x: i32,
        y: i32,
        width: u32,
        height: u32,
        filter: ScaleFilter,
    },
    RotateCanvas {
        turn: QuarterTurn,
    },
    /// Clockwise degrees. The layer bounds grow so the corners stay inside.
    RotateLayer {
        index: usize,
        degrees_cw: f32,
    },
    FlipCanvas {
        axis: Axis,
    },
    FlipLayer {
        index: usize,
        axis: Axis,
    },
}

impl Command {
    fn touches_pixels(&self) -> bool {
        matches!(
            self,
            Self::AddLayer
                | Self::AddImageLayer { .. }
                | Self::DuplicateLayer { .. }
                | Self::DeleteLayer { .. }
                | Self::Crop { .. }
                | Self::ScaleDocument { .. }
                | Self::ScaleLayer { .. }
                | Self::RotateCanvas { .. }
                | Self::RotateLayer { .. }
                | Self::FlipCanvas { .. }
                | Self::FlipLayer { .. }
        )
    }
}

#[derive(Clone)]
struct GeomLayer {
    id: u64,
    name: String,
    visible: bool,
    opacity: f32,
    blend: BlendMode,
    x: i32,
    y: i32,
}

#[derive(Clone)]
struct GeomSnap {
    width: u32,
    height: u32,
    ppi: f32,
    background: Background,
    active_id: u64,
    layers: Vec<GeomLayer>,
}

enum UndoEntry {
    Geom(GeomSnap),
    Pixels(Document),
}

/// Document plus undo history. The UI mutates a document only through [`Editor::apply`].
pub struct Editor {
    doc: Document,
    undo: Vec<UndoEntry>,
    redo: Vec<UndoEntry>,
}

impl Editor {
    pub fn new(doc: Document) -> Self {
        Self {
            doc,
            undo: Vec::new(),
            redo: Vec::new(),
        }
    }

    pub fn document(&self) -> &Document {
        &self.doc
    }

    pub fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }

    pub fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }

    pub fn set_active(&mut self, index: usize) -> Result<(), Error> {
        if index >= self.doc.layers.len() {
            return Err(Error::BadLayer);
        }
        self.doc.active = index;
        Ok(())
    }

    /// `Ok(true)` when the document changed. A valid no-op is `Ok(false)`.
    pub fn apply(&mut self, command: Command) -> Result<bool, Error> {
        if !command_changes(&self.doc, &command)? {
            return Ok(false);
        }
        let entry = if command.touches_pixels() {
            UndoEntry::Pixels(self.doc.clone())
        } else {
            UndoEntry::Geom(geom_snap(&self.doc))
        };
        apply_command(&mut self.doc, command)?;
        self.undo.push(entry);
        if self.undo.len() > UNDO_LIMIT {
            self.undo.remove(0);
        }
        self.redo.clear();
        Ok(true)
    }

    pub fn undo(&mut self) -> bool {
        let Some(entry) = self.undo.pop() else {
            return false;
        };
        let redo = match &entry {
            UndoEntry::Geom(_) => UndoEntry::Geom(geom_snap(&self.doc)),
            UndoEntry::Pixels(_) => UndoEntry::Pixels(self.doc.clone()),
        };
        restore(&mut self.doc, entry);
        self.redo.push(redo);
        true
    }

    pub fn redo(&mut self) -> bool {
        let Some(entry) = self.redo.pop() else {
            return false;
        };
        let undo = match &entry {
            UndoEntry::Geom(_) => UndoEntry::Geom(geom_snap(&self.doc)),
            UndoEntry::Pixels(_) => UndoEntry::Pixels(self.doc.clone()),
        };
        restore(&mut self.doc, entry);
        self.undo.push(undo);
        true
    }
}

fn geom_snap(doc: &Document) -> GeomSnap {
    GeomSnap {
        width: doc.width,
        height: doc.height,
        ppi: doc.ppi,
        background: doc.background,
        active_id: doc.layers[doc.active].id,
        layers: doc
            .layers
            .iter()
            .map(|layer| GeomLayer {
                id: layer.id,
                name: layer.name.clone(),
                visible: layer.visible,
                opacity: layer.opacity,
                blend: layer.blend,
                x: layer.x,
                y: layer.y,
            })
            .collect(),
    }
}

fn restore(doc: &mut Document, entry: UndoEntry) {
    match entry {
        UndoEntry::Pixels(previous) => *doc = previous,
        UndoEntry::Geom(snap) => restore_geom(doc, snap),
    }
}

fn restore_geom(doc: &mut Document, snap: GeomSnap) {
    doc.width = snap.width;
    doc.height = snap.height;
    doc.ppi = snap.ppi;
    doc.background = snap.background;
    let mut layers = std::mem::take(&mut doc.layers);
    let mut ordered = Vec::with_capacity(snap.layers.len());
    for meta in snap.layers {
        let pos = layers
            .iter()
            .position(|layer| layer.id == meta.id)
            .expect("geometry undo keeps the same layers");
        let mut layer = layers.swap_remove(pos);
        layer.name = meta.name;
        layer.visible = meta.visible;
        layer.opacity = meta.opacity;
        layer.blend = meta.blend;
        layer.x = meta.x;
        layer.y = meta.y;
        ordered.push(layer);
    }
    doc.layers = ordered;
    doc.active = doc
        .layers
        .iter()
        .position(|layer| layer.id == snap.active_id)
        .unwrap_or(0);
}

/// `Ok(false)` means the command is valid and would not change the document.
fn command_changes(doc: &Document, command: &Command) -> Result<bool, Error> {
    match command {
        Command::Rename { index, name } => {
            let layer = doc.layers.get(*index).ok_or(Error::BadLayer)?;
            Ok(layer.name != *name)
        }
        Command::SetVisibility { index, visible } => {
            let layer = doc.layers.get(*index).ok_or(Error::BadLayer)?;
            Ok(layer.visible != *visible)
        }
        Command::SetOpacity { index, opacity } => {
            let layer = doc.layers.get(*index).ok_or(Error::BadLayer)?;
            Ok((layer.opacity - opacity.clamp(0.0, 1.0)).abs() > f32::EPSILON)
        }
        Command::MoveLayer { index, x, y } => {
            let layer = doc.layers.get(*index).ok_or(Error::BadLayer)?;
            Ok(layer.x != *x || layer.y != *y)
        }
        Command::Reorder { from, to } => {
            if *from >= doc.layers.len() || *to >= doc.layers.len() {
                return Err(Error::BadLayer);
            }
            Ok(from != to)
        }
        Command::DeleteLayer { index } => {
            if *index >= doc.layers.len() {
                return Err(Error::BadLayer);
            }
            if doc.layers.len() == 1 {
                return Err(Error::LastLayer);
            }
            Ok(true)
        }
        Command::RotateLayer { index, degrees_cw } => {
            if *index >= doc.layers.len() {
                return Err(Error::BadLayer);
            }
            Ok(degrees_cw.abs() % 360.0 > 1e-3)
        }
        Command::ResizeCanvas { width, height, .. } => {
            check_size(*width, *height)?;
            Ok(*width != doc.width || *height != doc.height)
        }
        Command::ScaleDocument { width, height, .. } => {
            check_size(*width, *height)?;
            Ok(*width != doc.width || *height != doc.height)
        }
        Command::ScaleLayer {
            index,
            x,
            y,
            width,
            height,
            ..
        } => {
            let layer = doc.layers.get(*index).ok_or(Error::BadLayer)?;
            check_size(*width, *height)?;
            Ok(layer.x != *x
                || layer.y != *y
                || layer.pixels.width() != *width
                || layer.pixels.height() != *height)
        }
        Command::Crop { width, height, .. } => {
            if *width == 0 || *height == 0 {
                return Err(Error::EmptyCrop);
            }
            Ok(true)
        }
        Command::AddLayer
        | Command::AddImageLayer { .. }
        | Command::DuplicateLayer { .. }
        | Command::RotateCanvas { .. }
        | Command::FlipCanvas { .. }
        | Command::FlipLayer { .. } => {
            if let Command::DuplicateLayer { index } | Command::FlipLayer { index, .. } = command {
                if *index >= doc.layers.len() {
                    return Err(Error::BadLayer);
                }
            }
            if let Command::AddImageLayer { image, .. } = command {
                check_size(image.width(), image.height())?;
            }
            Ok(true)
        }
    }
}

fn apply_command(doc: &mut Document, command: Command) -> Result<(), Error> {
    match command {
        Command::AddLayer => {
            let name = format!("Layer {}", doc.layers.len() + 1);
            let layer = blank_layer(doc, &name);
            doc.insert_layer(layer);
        }
        Command::AddImageLayer { name, image } => {
            check_size(image.width(), image.height())?;
            let (w, h) = image.dimensions();
            let x = if w < doc.width {
                ((doc.width - w) / 2) as i32
            } else {
                0
            };
            let y = if h < doc.height {
                ((doc.height - h) / 2) as i32
            } else {
                0
            };
            let mut layer = blank_layer(doc, &name);
            layer.pixels = image;
            layer.x = x;
            layer.y = y;
            doc.insert_layer(layer);
        }
        Command::DuplicateLayer { index } => {
            let source = doc.layers.get(index).ok_or(Error::BadLayer)?.clone();
            let mut layer = blank_layer(doc, &format!("{} copy", source.name));
            layer.visible = source.visible;
            layer.opacity = source.opacity;
            layer.blend = source.blend;
            layer.x = source.x + 16;
            layer.y = source.y + 16;
            layer.pixels = source.pixels;
            doc.insert_layer(layer);
        }
        Command::DeleteLayer { index } => {
            if doc.layers.len() == 1 {
                return Err(Error::LastLayer);
            }
            if index >= doc.layers.len() {
                return Err(Error::BadLayer);
            }
            doc.layers.remove(index);
            if doc.active > index {
                doc.active -= 1;
            }
            if doc.active >= doc.layers.len() {
                doc.active = doc.layers.len() - 1;
            }
        }
        Command::Reorder { from, to } => ops::reorder(doc, from, to)?,
        Command::Rename { index, name } => {
            doc.layer_mut(index)?.name = name;
        }
        Command::SetVisibility { index, visible } => {
            doc.layer_mut(index)?.visible = visible;
        }
        Command::SetOpacity { index, opacity } => {
            doc.layer_mut(index)?.opacity = opacity.clamp(0.0, 1.0);
        }
        Command::MoveLayer { index, x, y } => {
            let layer = doc.layer_mut(index)?;
            layer.x = x;
            layer.y = y;
        }
        Command::Crop {
            x,
            y,
            width,
            height,
        } => ops::crop(doc, x, y, width, height)?,
        Command::ResizeCanvas {
            width,
            height,
            anchor,
        } => ops::resize_canvas(doc, width, height, anchor)?,
        Command::ScaleDocument {
            width,
            height,
            filter,
        } => ops::scale_document(doc, width, height, filter.image_filter())?,
        Command::ScaleLayer {
            index,
            x,
            y,
            width,
            height,
            filter,
        } => ops::scale_layer(doc, index, x, y, width, height, filter.image_filter())?,
        Command::RotateCanvas { turn } => ops::rotate_canvas(doc, turn),
        Command::RotateLayer { index, degrees_cw } => ops::rotate_layer(doc, index, degrees_cw)?,
        Command::FlipCanvas { axis } => ops::flip_canvas(doc, axis),
        Command::FlipLayer { index, axis } => ops::flip_layer(doc, index, axis)?,
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::Rgba;

    fn rgba(r: u8, g: u8, b: u8, a: u8) -> Rgba<u8> {
        Rgba([r, g, b, a])
    }

    fn doc_with(width: u32, height: u32, pixel: (u32, u32), color: Rgba<u8>) -> Editor {
        let mut editor = Editor::new(
            Document::new(NewCanvas {
                width,
                height,
                ppi: 72.0,
                background: Background::Transparent,
            })
            .unwrap(),
        );
        editor.doc.layers[0]
            .pixels
            .put_pixel(pixel.0, pixel.1, color);
        editor
    }

    #[test]
    fn composite_places_a_layer_pixel_and_skips_hidden_layers() {
        let mut editor = doc_with(3, 2, (1, 0), rgba(255, 0, 0, 255));
        editor
            .apply(Command::MoveLayer {
                index: 0,
                x: 1,
                y: 1,
            })
            .unwrap();
        let image = composite(editor.document());
        assert_eq!(image.get_pixel(2, 1), &rgba(255, 0, 0, 255));
        assert_eq!(*image.get_pixel(1, 0), Rgba([0, 0, 0, 0]));

        editor
            .apply(Command::SetVisibility {
                index: 0,
                visible: false,
            })
            .unwrap();
        let image = composite(editor.document());
        assert!(image.pixels().all(|pixel| pixel[3] == 0));
    }

    #[test]
    fn opacity_blends_over_an_opaque_background() {
        let mut editor = Editor::new(
            Document::new(NewCanvas {
                width: 1,
                height: 1,
                ppi: 72.0,
                background: Background::white(),
            })
            .unwrap(),
        );
        editor.doc.layers[0]
            .pixels
            .put_pixel(0, 0, rgba(255, 0, 0, 255));
        editor
            .apply(Command::SetOpacity {
                index: 0,
                opacity: 0.5,
            })
            .unwrap();
        let pixel = *composite(editor.document()).get_pixel(0, 0);
        assert_eq!(pixel[0], 255);
        assert_eq!(pixel[1], 128);
        assert_eq!(pixel[2], 128);
        assert_eq!(pixel[3], 255);
    }

    #[test]
    fn pixels_outside_the_canvas_are_not_drawn() {
        let mut editor = doc_with(2, 2, (1, 0), rgba(0, 255, 0, 255));
        editor
            .apply(Command::MoveLayer {
                index: 0,
                x: -1,
                y: 0,
            })
            .unwrap();
        let image = composite(editor.document());
        assert_eq!(image.get_pixel(0, 0), &rgba(0, 255, 0, 255));
        assert_eq!(*image.get_pixel(1, 0), Rgba([0, 0, 0, 0]));
    }

    #[test]
    fn crop_trims_layer_buffers_and_undo_restores_them() {
        let mut editor = doc_with(4, 4, (1, 1), rgba(255, 0, 0, 255));
        editor
            .apply(Command::Crop {
                x: 1,
                y: 1,
                width: 2,
                height: 2,
            })
            .unwrap();
        let doc = editor.document();
        assert_eq!((doc.width, doc.height), (2, 2));
        assert_eq!(doc.layers[0].width(), 2);
        assert_eq!(doc.layers[0].pixels.get_pixel(0, 0), &rgba(255, 0, 0, 255));
        assert!(editor.undo());
        let doc = editor.document();
        assert_eq!((doc.width, doc.height), (4, 4));
        assert_eq!(doc.layers[0].pixels.get_pixel(1, 1), &rgba(255, 0, 0, 255));
        assert!(editor.redo());
        assert_eq!(editor.document().width, 2);
    }

    #[test]
    fn layer_at_picks_the_topmost_visible_opaque_pixel() {
        let mut editor = doc_with(4, 4, (1, 1), rgba(255, 0, 0, 255));
        editor.apply(Command::AddLayer).unwrap();
        editor.doc.layers[1]
            .pixels
            .put_pixel(1, 1, rgba(0, 255, 0, 255));
        editor.doc.layers[1]
            .pixels
            .put_pixel(2, 2, rgba(0, 255, 0, 255));
        let doc = editor.document();
        assert_eq!(doc.layer_at(1.5, 1.5), Some(1));
        assert_eq!(doc.layer_at(2.0, 2.0), Some(1));
        assert_eq!(doc.layer_at(0.0, 0.0), None);
        assert_eq!(doc.layer_at(-0.5, 1.0), None);

        editor
            .apply(Command::SetVisibility {
                index: 1,
                visible: false,
            })
            .unwrap();
        let doc = editor.document();
        assert_eq!(doc.layer_at(1.5, 1.5), Some(0));
        assert_eq!(doc.layer_at(2.0, 2.0), None);
    }

    #[test]
    fn layer_at_follows_the_layer_offset_and_ignores_pixels_off_the_canvas() {
        let mut editor = doc_with(4, 4, (0, 0), rgba(255, 0, 0, 255));
        editor.doc.layers[0].x = 2;
        editor.doc.layers[0].y = 1;
        assert_eq!(editor.document().layer_at(2.0, 1.0), Some(0));
        assert_eq!(editor.document().layer_at(0.0, 0.0), None);

        editor.doc.layers[0].x = -1;
        editor.doc.layers[0].y = 0;
        assert_eq!(editor.document().layer_at(-0.5, 0.0), None);
    }

    #[test]
    fn crop_keeps_the_overlapping_part_of_a_layer_that_hangs_off_the_canvas() {
        let mut editor = Editor::new(
            Document::new(NewCanvas {
                width: 4,
                height: 4,
                ppi: 72.0,
                background: Background::Transparent,
            })
            .unwrap(),
        );
        let mut image = RgbaImage::new(4, 4);
        image.put_pixel(3, 3, rgba(0, 0, 255, 255));
        editor.doc.layers[0].pixels = image;
        editor.doc.layers[0].x = -2;
        editor.doc.layers[0].y = -2;
        editor
            .apply(Command::Crop {
                x: 0,
                y: 0,
                width: 2,
                height: 2,
            })
            .unwrap();
        let layer = &editor.document().layers[0];
        assert_eq!((layer.x, layer.y), (0, 0));
        assert_eq!(layer.pixels.get_pixel(1, 1), &rgba(0, 0, 255, 255));
    }

    #[test]
    fn resize_canvas_anchor_moves_content_without_resampling() {
        let mut editor = doc_with(5, 5, (2, 2), rgba(255, 0, 0, 255));
        editor
            .apply(Command::ResizeCanvas {
                width: 9,
                height: 9,
                anchor: Anchor::Center,
            })
            .unwrap();
        let image = composite(editor.document());
        assert_eq!(image.get_pixel(4, 4), &rgba(255, 0, 0, 255));
        assert_eq!(editor.document().layers[0].pixels.width(), 5);

        editor
            .apply(Command::ResizeCanvas {
                width: 3,
                height: 3,
                anchor: Anchor::SouthEast,
            })
            .unwrap();
        let image = composite(editor.document());
        assert!(image.pixels().all(|pixel| pixel[3] == 0));
        assert_eq!(
            editor.document().layers[0].pixels.get_pixel(2, 2),
            &rgba(255, 0, 0, 255)
        );
    }

    #[test]
    fn scale_nearest_duplicates_pixels() {
        let mut editor = doc_with(2, 2, (0, 0), rgba(255, 0, 0, 255));
        editor.doc.layers[0]
            .pixels
            .put_pixel(1, 1, rgba(0, 0, 255, 255));
        editor
            .apply(Command::ScaleDocument {
                width: 4,
                height: 4,
                filter: ScaleFilter::Nearest,
            })
            .unwrap();
        let image = composite(editor.document());
        assert_eq!(image.dimensions(), (4, 4));
        assert_eq!(image.get_pixel(0, 0), &rgba(255, 0, 0, 255));
        assert_eq!(image.get_pixel(1, 1), &rgba(255, 0, 0, 255));
        assert_eq!(image.get_pixel(2, 2), &rgba(0, 0, 255, 255));
        assert_eq!(image.get_pixel(3, 3), &rgba(0, 0, 255, 255));
    }

    #[test]
    fn canvas_rotation_and_flips_match_imageops_on_the_composite() {
        let mut editor = doc_with(3, 2, (0, 0), rgba(255, 0, 0, 255));
        editor.doc.layers[0]
            .pixels
            .put_pixel(2, 1, rgba(0, 255, 0, 255));
        editor
            .apply(Command::AddImageLayer {
                name: "dot".into(),
                image: {
                    let mut image = RgbaImage::new(1, 1);
                    image.put_pixel(0, 0, rgba(0, 0, 255, 255));
                    image
                },
            })
            .unwrap();
        editor
            .apply(Command::MoveLayer {
                index: 1,
                x: 1,
                y: 1,
            })
            .unwrap();

        let before = composite(editor.document());
        editor
            .apply(Command::RotateCanvas {
                turn: QuarterTurn::Cw,
            })
            .unwrap();
        assert_eq!(
            composite(editor.document()),
            image::imageops::rotate90(&before)
        );

        let before = composite(editor.document());
        editor
            .apply(Command::RotateCanvas {
                turn: QuarterTurn::Ccw,
            })
            .unwrap();
        assert_eq!(
            composite(editor.document()),
            image::imageops::rotate270(&before)
        );

        let before = composite(editor.document());
        editor
            .apply(Command::RotateCanvas {
                turn: QuarterTurn::Half,
            })
            .unwrap();
        assert_eq!(
            composite(editor.document()),
            image::imageops::rotate180(&before)
        );

        let before = composite(editor.document());
        editor
            .apply(Command::FlipCanvas {
                axis: Axis::Horizontal,
            })
            .unwrap();
        assert_eq!(
            composite(editor.document()),
            image::imageops::flip_horizontal(&before)
        );

        let before = composite(editor.document());
        editor
            .apply(Command::FlipCanvas {
                axis: Axis::Vertical,
            })
            .unwrap();
        assert_eq!(
            composite(editor.document()),
            image::imageops::flip_vertical(&before)
        );
    }

    #[test]
    fn layer_flip_does_not_move_the_layer_origin() {
        let mut editor = doc_with(4, 3, (0, 0), rgba(255, 0, 0, 255));
        editor.doc.layers[0]
            .pixels
            .put_pixel(3, 2, rgba(0, 0, 255, 255));
        editor
            .apply(Command::MoveLayer {
                index: 0,
                x: 2,
                y: -1,
            })
            .unwrap();
        editor
            .apply(Command::FlipLayer {
                index: 0,
                axis: Axis::Horizontal,
            })
            .unwrap();
        let layer = &editor.document().layers[0];
        assert_eq!((layer.x, layer.y), (2, -1));
        assert_eq!(layer.pixels.get_pixel(3, 0), &rgba(255, 0, 0, 255));
        assert_eq!(layer.pixels.get_pixel(0, 2), &rgba(0, 0, 255, 255));
    }

    #[test]
    fn arbitrary_layer_rotate_turns_a_split_square_clockwise() {
        let mut editor = Editor::new(
            Document::new(NewCanvas {
                width: 16,
                height: 16,
                ppi: 72.0,
                background: Background::Transparent,
            })
            .unwrap(),
        );
        let mut image = RgbaImage::new(8, 8);
        for y in 0..8 {
            for x in 0..8 {
                let color = if x < 4 {
                    rgba(255, 0, 0, 255)
                } else {
                    rgba(0, 0, 255, 255)
                };
                image.put_pixel(x, y, color);
            }
        }
        editor.doc.layers[0].pixels = image;
        editor.doc.layers[0].x = 4;
        editor.doc.layers[0].y = 4;
        editor
            .apply(Command::RotateLayer {
                index: 0,
                degrees_cw: 90.0,
            })
            .unwrap();
        let doc = editor.document();
        assert_eq!((doc.width, doc.height), (16, 16));
        let layer = &doc.layers[0];
        // Left half swings to the top. Sample inside the halves, away from the seam.
        let top = layer
            .pixels
            .get_pixel(layer.width() / 2, layer.height() / 4);
        let bottom = layer
            .pixels
            .get_pixel(layer.width() / 2, layer.height() * 3 / 4);
        assert!(top[0] > 200 && top[2] < 40, "top pixel {top:?}");
        assert!(bottom[2] > 200 && bottom[0] < 40, "bottom pixel {bottom:?}");
    }

    #[test]
    fn undo_of_a_move_restores_position_and_a_new_edit_drops_redo() {
        let mut editor = doc_with(4, 4, (0, 0), rgba(255, 0, 0, 255));
        editor
            .apply(Command::MoveLayer {
                index: 0,
                x: 3,
                y: 1,
            })
            .unwrap();
        assert!(editor.undo());
        assert_eq!(
            (editor.document().layers[0].x, editor.document().layers[0].y),
            (0, 0)
        );
        assert!(editor.redo());
        editor
            .apply(Command::MoveLayer {
                index: 0,
                x: 1,
                y: 1,
            })
            .unwrap();
        assert!(!editor.can_redo());
    }

    #[test]
    fn undo_history_is_capped() {
        let mut editor = doc_with(8, 8, (0, 0), rgba(1, 0, 0, 255));
        for step in 1..=UNDO_LIMIT + 1 {
            editor
                .apply(Command::MoveLayer {
                    index: 0,
                    x: step as i32,
                    y: 0,
                })
                .unwrap();
        }
        assert_eq!(editor.undo.len(), UNDO_LIMIT);
        let mut undone = 0;
        while editor.undo() {
            undone += 1;
        }
        assert_eq!(undone, UNDO_LIMIT);
        assert_eq!(editor.document().layers[0].x, 1);
    }

    #[test]
    fn cannot_delete_the_last_layer() {
        let mut editor = doc_with(2, 2, (0, 0), rgba(0, 0, 0, 255));
        let err = editor.apply(Command::DeleteLayer { index: 0 }).unwrap_err();
        assert!(matches!(err, Error::LastLayer));
    }

    #[test]
    fn reorder_lands_on_the_requested_index() {
        let mut editor = doc_with(2, 2, (0, 0), rgba(255, 0, 0, 255));
        editor.apply(Command::AddLayer).unwrap();
        editor.apply(Command::AddLayer).unwrap();
        let names: Vec<_> = editor
            .document()
            .layers()
            .iter()
            .map(|layer| layer.name.clone())
            .collect();
        let active_id = editor.document().active_layer().id;
        editor.apply(Command::Reorder { from: 0, to: 2 }).unwrap();
        let after: Vec<_> = editor
            .document()
            .layers()
            .iter()
            .map(|layer| layer.name.clone())
            .collect();
        assert_eq!(after[2], names[0]);
        assert_eq!(editor.document().active_layer().id, active_id);
        assert_ne!(editor.document().active_index(), 2);
    }

    #[test]
    fn scale_layer_resamples_one_layer_and_keeps_the_given_origin() {
        let mut editor = doc_with(8, 8, (0, 0), rgba(255, 0, 0, 255));
        editor
            .apply(Command::ScaleLayer {
                index: 0,
                x: 2,
                y: 3,
                width: 4,
                height: 4,
                filter: ScaleFilter::Nearest,
            })
            .unwrap();
        let layer = &editor.document().layers()[0];
        assert_eq!((layer.x, layer.y), (2, 3));
        assert_eq!((layer.width(), layer.height()), (4, 4));
        assert_eq!((editor.document().width, editor.document().height), (8, 8));
        assert!(editor.undo());
        assert_eq!(editor.document().layers()[0].width(), 8);
    }

    #[test]
    fn rotating_a_wide_layer_sideways_does_not_overflow() {
        let mut editor = Editor::new(
            Document::new(NewCanvas {
                width: 16,
                height: 16,
                ppi: 72.0,
                background: Background::Transparent,
            })
            .unwrap(),
        );
        let mut image = image::RgbaImage::new(8, 2);
        image.put_pixel(0, 0, rgba(255, 0, 0, 255));
        editor.doc.layers[0].pixels = image;
        editor
            .apply(Command::RotateLayer {
                index: 0,
                degrees_cw: 90.0,
            })
            .unwrap();
        let layer = &editor.document().layers()[0];
        assert!(layer.height() > layer.width());
        assert!(layer.pixels.pixels().any(|pixel| pixel[3] > 0));
        let (pixels, _, _) = rotate_bitmap(&layer.pixels, 0, 0, f32::NAN);
        assert_eq!(pixels.dimensions(), layer.pixels.dimensions());
    }
}
