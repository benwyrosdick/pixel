//! A layered raster document and the undoable commands that edit it.

mod adjust;
mod arrange;
mod composite;
mod groups;
mod handles;
mod io;
mod ops;
mod render;

pub use adjust::{adjust, Adjustment};
pub use composite::{composite, composite_layers, composite_with, LayerOverride};
pub use handles::{
    clockwise_delta, hit_handle, pointer_angle, resize_rect, rotate_handle_point, rotated_bounds,
    snap_angle, Handle, PixelRect, HANDLE_RADIUS, ROTATE_OFFSET,
};
pub use io::{export, open_image, open_project, save_project, ExportFormat};
pub use ops::rotate_bitmap;
pub use render::{render_shape, render_text, Rendered, ShapeKind, ShapeSpec, TextAlign, TextSpec};

use image::RgbaImage;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

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
    #[error("a document needs at least one layer")]
    LastLayer,
    #[error("the crop is empty")]
    EmptyCrop,
    #[error("could not read the image: {0}")]
    Decode(String),
    #[error("could not write the file: {0}")]
    Write(String),
    #[error("that file is not a Pixel project")]
    BadProject,
    #[error("the layer is locked")]
    Locked,
    #[error("that layer can't be edited this way")]
    WrongKind,
}

/// How a layer's colors mix with the layers under it. The formulas are the
/// W3C Compositing and Blending ones, which most editors share.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum BlendMode {
    Normal,
    Multiply,
    Screen,
    Overlay,
    Darken,
    Lighten,
    ColorDodge,
    ColorBurn,
    HardLight,
    SoftLight,
    Difference,
    Exclusion,
}

impl BlendMode {
    /// Every mode, in the order menus list them.
    pub const ALL: [BlendMode; 12] = [
        Self::Normal,
        Self::Multiply,
        Self::Screen,
        Self::Overlay,
        Self::Darken,
        Self::Lighten,
        Self::ColorDodge,
        Self::ColorBurn,
        Self::HardLight,
        Self::SoftLight,
        Self::Difference,
        Self::Exclusion,
    ];

    /// The mixed color of one channel, from the color below (`cb`) and the
    /// layer's color (`cs`), both from 0 to 1.
    pub fn mix(self, cb: f32, cs: f32) -> f32 {
        let screen = |b: f32, s: f32| b + s - b * s;
        let hard_light = |b: f32, s: f32| {
            if s <= 0.5 {
                b * 2.0 * s
            } else {
                screen(b, 2.0 * s - 1.0)
            }
        };
        match self {
            Self::Normal => cs,
            Self::Multiply => cb * cs,
            Self::Screen => screen(cb, cs),
            Self::Overlay => hard_light(cs, cb),
            Self::Darken => cb.min(cs),
            Self::Lighten => cb.max(cs),
            Self::ColorDodge => {
                if cb <= 0.0 {
                    0.0
                } else if cs >= 1.0 {
                    1.0
                } else {
                    (cb / (1.0 - cs)).min(1.0)
                }
            }
            Self::ColorBurn => {
                if cb >= 1.0 {
                    1.0
                } else if cs <= 0.0 {
                    0.0
                } else {
                    1.0 - ((1.0 - cb) / cs).min(1.0)
                }
            }
            Self::HardLight => hard_light(cb, cs),
            Self::SoftLight => {
                if cs <= 0.5 {
                    cb - (1.0 - 2.0 * cs) * cb * (1.0 - cb)
                } else {
                    let d = if cb <= 0.25 {
                        ((16.0 * cb - 12.0) * cb + 4.0) * cb
                    } else {
                        cb.sqrt()
                    };
                    cb + (2.0 * cs - 1.0) * (d - cb)
                }
            }
            Self::Difference => (cb - cs).abs(),
            Self::Exclusion => cb + cs - 2.0 * cb * cs,
        }
    }
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

/// Which edges or centers [`Command::AlignLayers`] lines up.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Alignment {
    Left,
    HorizontalCenter,
    Right,
    Top,
    VerticalCenter,
    Bottom,
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
    /// A locked layer's pixels, placement, and opacity can't be edited, and
    /// the move tool passes over it. It can still be selected, copied, and
    /// duplicated. Canvas-wide edits still apply to it.
    pub locked: bool,
    pub opacity: f32,
    pub blend: BlendMode,
    pub x: i32,
    pub y: i32,
    /// What the layer draws. Text and shape layers keep their drawing here
    /// too, redrawn from their description whenever it changes.
    pub pixels: RgbaImage,
    pub kind: LayerKind,
    /// The id of the group the layer is in, if any.
    pub parent: Option<u64>,
}

/// What a layer is made of.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub enum LayerKind {
    #[default]
    Raster,
    /// `origin` is where the text's own top-left corner sits in the pixels.
    Text { spec: TextSpec, origin: (i32, i32) },
    /// `origin` is where the shape's box starts in the pixels.
    Shape { spec: ShapeSpec, origin: (i32, i32) },
    /// Holds the layers right below it that name it as their group. It draws
    /// nothing itself. `collapsed` hides its contents in the layers panel.
    Group { collapsed: bool },
}

impl Layer {
    pub fn width(&self) -> u32 {
        self.pixels.width()
    }

    pub fn height(&self) -> u32 {
        self.pixels.height()
    }

    /// Whether the layer is text or a shape, drawn from a description.
    pub fn is_vector(&self) -> bool {
        matches!(self.kind, LayerKind::Text { .. } | LayerKind::Shape { .. })
    }

    /// Where a text or shape layer's own top-left corner sits in the document.
    fn anchor(&self) -> Option<(i32, i32)> {
        match self.kind {
            LayerKind::Raster | LayerKind::Group { .. } => None,
            LayerKind::Text { origin, .. } | LayerKind::Shape { origin, .. } => {
                Some((self.x + origin.0, self.y + origin.1))
            }
        }
    }

    /// Draw a text or shape layer again from its description, with its own
    /// top-left corner at `anchor`.
    fn redraw(&mut self, anchor: (i32, i32)) {
        let rendered = match &self.kind {
            LayerKind::Raster | LayerKind::Group { .. } => return,
            LayerKind::Text { spec, .. } => render_text(spec),
            LayerKind::Shape { spec, .. } => render_shape(spec),
        };
        match &mut self.kind {
            LayerKind::Text { origin, .. } | LayerKind::Shape { origin, .. } => {
                *origin = rendered.origin;
            }
            LayerKind::Raster | LayerKind::Group { .. } => {}
        }
        self.pixels = rendered.pixels;
        self.x = anchor.0 - rendered.origin.0;
        self.y = anchor.1 - rendered.origin.1;
    }

    /// Keep the layer as it looks now, as plain pixels.
    fn rasterize(&mut self) {
        if self.is_vector() {
            self.kind = LayerKind::Raster;
        }
    }

    /// The box around the layer's non-transparent pixels, in document space.
    /// `None` when every pixel is transparent.
    pub fn content_bounds(&self) -> Option<PixelRect> {
        let (mut x0, mut y0, mut x1, mut y1) = (u32::MAX, u32::MAX, 0, 0);
        for (x, y, pixel) in self.pixels.enumerate_pixels() {
            if pixel[3] > 0 {
                x0 = x0.min(x);
                y0 = y0.min(y);
                x1 = x1.max(x + 1);
                y1 = y1.max(y + 1);
            }
        }
        (x0 < x1).then(|| PixelRect {
            x: self.x + x0 as i32,
            y: self.y + y0 as i32,
            width: x1 - x0,
            height: y1 - y0,
        })
    }
}

/// Guide lines in document pixels: vertical guides by `x`, horizontal ones
/// by `y`. A guide may sit off the canvas.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Guides {
    pub x: Vec<i32>,
    pub y: Vec<i32>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Document {
    pub width: u32,
    pub height: u32,
    pub ppi: f32,
    pub background: Background,
    layers: Vec<Layer>,
    /// Ids of the selected layers. Ids, unlike indices, survive a reorder.
    selection: BTreeSet<u64>,
    guides: Guides,
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
            selection: BTreeSet::new(),
            guides: Guides::default(),
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
            selection: BTreeSet::new(),
            guides: Guides::default(),
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

    /// The selected layer when exactly one is selected. Single-layer edits,
    /// such as opacity and the resize handles, act on it.
    pub fn active_index(&self) -> Option<usize> {
        match self.selection.len() {
            1 => self.selected_indices().first().copied(),
            _ => None,
        }
    }

    pub fn active_layer(&self) -> Option<&Layer> {
        self.active_index().map(|index| &self.layers[index])
    }

    /// Selected layers, bottom to top.
    pub fn selected_indices(&self) -> Vec<usize> {
        (0..self.layers.len())
            .filter(|&index| self.is_selected(index))
            .collect()
    }

    pub fn guides(&self) -> &Guides {
        &self.guides
    }

    pub fn is_selected(&self, index: usize) -> bool {
        self.layers
            .get(index)
            .is_some_and(|layer| self.selection.contains(&layer.id))
    }

    fn select_indices(&mut self, indices: &[usize]) -> Result<(), Error> {
        let mut selection = BTreeSet::new();
        for &index in indices {
            selection.insert(self.layers.get(index).ok_or(Error::BadLayer)?.id);
        }
        self.selection = selection;
        Ok(())
    }

    /// The topmost visible layer with a non-transparent pixel at a document
    /// point. Pixels outside the canvas are not drawn, so they never hit.
    pub fn layer_at(&self, x: f64, y: f64) -> Option<usize> {
        self.topmost_at(x, y, true)
    }

    /// Like [`Document::layer_at`], but looks through locked layers to the
    /// ones under them. The move tool picks with this.
    pub fn unlocked_layer_at(&self, x: f64, y: f64) -> Option<usize> {
        self.topmost_at(x, y, false)
    }

    fn topmost_at(&self, x: f64, y: f64, include_locked: bool) -> Option<usize> {
        if !(x >= 0.0 && y >= 0.0 && x < self.width as f64 && y < self.height as f64) {
            return None;
        }
        let (px, py) = (x.floor() as i64, y.floor() as i64);
        (0..self.layers.len()).rev().find(|&index| {
            let layer = &self.layers[index];
            if !self.shown(index) || (layer.locked && !include_locked) || layer.opacity <= 0.0 {
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

    /// The box around every pixel the visible layers draw on the canvas. The
    /// canvas background doesn't count. `None` when they draw nothing.
    pub fn content_bounds(&self) -> Option<PixelRect> {
        let (mut x0, mut y0, mut x1, mut y1) = (i64::MAX, i64::MAX, i64::MIN, i64::MIN);
        for (index, layer) in self.layers.iter().enumerate() {
            if !self.shown(index) || layer.opacity <= 0.0 {
                continue;
            }
            let (ox, oy) = (layer.x as i64, layer.y as i64);
            let left = ox.max(0);
            let top = oy.max(0);
            let right = (ox + layer.width() as i64).min(self.width as i64);
            let bottom = (oy + layer.height() as i64).min(self.height as i64);
            for y in top..bottom {
                for x in left..right {
                    if layer.pixels.get_pixel((x - ox) as u32, (y - oy) as u32)[3] > 0 {
                        x0 = x0.min(x);
                        y0 = y0.min(y);
                        x1 = x1.max(x + 1);
                        y1 = y1.max(y + 1);
                    }
                }
            }
        }
        (x0 < x1).then(|| PixelRect {
            x: x0 as i32,
            y: y0 as i32,
            width: (x1 - x0) as u32,
            height: (y1 - y0) as u32,
        })
    }

    /// Visible layers whose drawn pixels all fall inside `area`. Only pixels on
    /// the canvas count, and a layer with none is skipped.
    pub fn layers_within(&self, area: PixelRect) -> Vec<usize> {
        let (ax0, ay0) = (area.x as i64, area.y as i64);
        let (ax1, ay1) = (ax0 + area.width as i64, ay0 + area.height as i64);
        (0..self.layers.len())
            .filter(|&index| {
                let layer = &self.layers[index];
                if !self.shown(index) || layer.opacity <= 0.0 || self.is_group(index) {
                    return false;
                }
                let (ox, oy) = (layer.x as i64, layer.y as i64);
                let x0 = ox.max(0);
                let y0 = oy.max(0);
                let x1 = (ox + layer.width() as i64).min(self.width as i64);
                let y1 = (oy + layer.height() as i64).min(self.height as i64);
                // Nothing drawn can be inside an area the layer doesn't reach.
                if x0 >= ax1 || y0 >= ay1 || x1 <= ax0 || y1 <= ay0 {
                    return false;
                }
                let mut found = false;
                for y in y0..y1 {
                    for x in x0..x1 {
                        if layer.pixels.get_pixel((x - ox) as u32, (y - oy) as u32)[3] == 0 {
                            continue;
                        }
                        if x < ax0 || y < ay0 || x >= ax1 || y >= ay1 {
                            return false;
                        }
                        found = true;
                    }
                }
                found
            })
            .collect()
    }

    fn layer_mut(&mut self, index: usize) -> Result<&mut Layer, Error> {
        self.layers.get_mut(index).ok_or(Error::BadLayer)
    }

    fn insert_layer(&mut self, layer: Layer) {
        self.selection = BTreeSet::from([layer.id]);
        self.layers.push(layer);
    }

    pub(crate) fn from_parts(
        width: u32,
        height: u32,
        ppi: f32,
        background: Background,
        layers: Vec<Layer>,
        selected: &[usize],
        guides: Guides,
        next_id: u64,
    ) -> Result<Self, Error> {
        check_size(width, height)?;
        if layers.is_empty() || selected.iter().any(|&index| index >= layers.len()) {
            return Err(Error::BadProject);
        }
        let mut seen = BTreeSet::new();
        for layer in &layers {
            if !seen.insert(layer.id) {
                return Err(Error::BadProject);
            }
        }
        let selection = selected.iter().map(|&index| layers[index].id).collect();
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
            selection,
            guides,
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
        locked: false,
        opacity: 1.0,
        blend: BlendMode::Normal,
        x: 0,
        y: 0,
        pixels: RgbaImage::new(doc.width, doc.height),
        kind: LayerKind::Raster,
        parent: None,
    }
}

/// A layer's name from the first line of its text.
fn text_name(text: &str) -> String {
    let line = text.lines().next().unwrap_or("").trim();
    if line.is_empty() {
        return "Text".into();
    }
    let mut name: String = line.chars().take(30).collect();
    if line.chars().count() > 30 {
        name.push('…');
    }
    name
}

/// Edits the UI is allowed to make. Selection changes are not commands.
#[derive(Debug, Clone)]
pub enum Command {
    AddLayer,
    AddImageLayer {
        name: String,
        image: RgbaImage,
    },
    /// A text layer whose top-left corner is at `(x, y)`.
    AddText {
        spec: TextSpec,
        x: i32,
        y: i32,
    },
    /// A shape layer whose box starts at `(x, y)`.
    AddShape {
        spec: ShapeSpec,
        x: i32,
        y: i32,
    },
    /// Change a text layer's text or style. Its top-left corner stays put.
    SetText {
        index: usize,
        spec: TextSpec,
    },
    /// Change a shape layer's shape or style. Its box stays where it starts.
    SetShape {
        index: usize,
        spec: ShapeSpec,
    },
    /// Turn a text or shape layer into plain pixels.
    Rasterize {
        index: usize,
    },
    /// Gather layers into a new group, where the topmost of them was.
    Group {
        indices: Vec<usize>,
    },
    /// Take a group apart, leaving its contents in place.
    Ungroup {
        index: usize,
    },
    /// Move a layer, or a group with its contents, to the top of a group.
    MoveIntoGroup {
        index: usize,
        group: usize,
    },
    DuplicateLayer {
        index: usize,
    },
    /// Remove every listed layer in one step.
    DeleteLayers {
        indices: Vec<usize>,
    },
    /// Move `from`, and everything in it, to where `to` is, beside it in the
    /// same group: above it when moving up, below it when moving down.
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
    SetLocked {
        index: usize,
        locked: bool,
    },
    SetOpacity {
        index: usize,
        opacity: f32,
    },
    SetBlend {
        index: usize,
        blend: BlendMode,
    },
    /// Rewrite one layer's pixels with a color adjustment or filter.
    Adjust {
        index: usize,
        adjustment: Adjustment,
    },
    MoveLayer {
        index: usize,
        x: i32,
        y: i32,
    },
    /// Offset every listed layer by the same amount in one step.
    MoveLayers {
        indices: Vec<usize>,
        dx: i32,
        dy: i32,
    },
    /// Line up the visible pixels of the listed layers. One layer lines up
    /// with the canvas, and several with the box around them all. Locked and
    /// empty layers are left alone.
    AlignLayers {
        indices: Vec<usize>,
        to: Alignment,
    },
    /// Replace every guide.
    SetGuides {
        guides: Guides,
    },
    /// Even out the gaps between the listed layers' visible pixels along
    /// `axis`, keeping the outermost two in place. Horizontal spaces them
    /// left to right. Locked and empty layers are left alone.
    DistributeLayers {
        indices: Vec<usize>,
        axis: Axis,
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
                | Self::DeleteLayers { .. }
                | Self::Crop { .. }
                | Self::ScaleDocument { .. }
                | Self::ScaleLayer { .. }
                | Self::RotateCanvas { .. }
                | Self::RotateLayer { .. }
                | Self::FlipCanvas { .. }
                | Self::FlipLayer { .. }
                | Self::Adjust { .. }
                | Self::AddText { .. }
                | Self::AddShape { .. }
                | Self::SetText { .. }
                | Self::SetShape { .. }
                | Self::Rasterize { .. }
                | Self::Group { .. }
                | Self::Ungroup { .. }
        )
    }
}

#[derive(Clone)]
struct GeomLayer {
    id: u64,
    parent: Option<u64>,
    name: String,
    visible: bool,
    locked: bool,
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
    selection: BTreeSet<u64>,
    guides: Guides,
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

    /// Select only this layer.
    pub fn set_active(&mut self, index: usize) -> Result<(), Error> {
        self.doc.select_indices(&[index])
    }

    pub fn set_selection(&mut self, indices: &[usize]) -> Result<(), Error> {
        self.doc.select_indices(indices)
    }

    /// Add the layer to the selection, or take it out if it is already in.
    pub fn toggle_selected(&mut self, index: usize) -> Result<(), Error> {
        let id = self.doc.layers.get(index).ok_or(Error::BadLayer)?.id;
        if !self.doc.selection.remove(&id) {
            self.doc.selection.insert(id);
        }
        Ok(())
    }

    /// Fold a group away in the layers panel, or open it. This is not an
    /// edit, so it isn't undone.
    pub fn set_collapsed(&mut self, index: usize, collapsed: bool) -> Result<(), Error> {
        match &mut self.doc.layer_mut(index)?.kind {
            LayerKind::Group { collapsed: folded } => {
                *folded = collapsed;
                Ok(())
            }
            _ => Err(Error::WrongKind),
        }
    }

    pub fn deselect(&mut self) {
        self.doc.selection.clear();
    }

    /// `Ok(true)` when the document changed. A valid no-op is `Ok(false)`.
    pub fn apply(&mut self, command: Command) -> Result<bool, Error> {
        if !command_changes(&self.doc, &command)? {
            return Ok(false);
        }
        check_unlocked(&self.doc, &command)?;
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
        selection: doc.selection.clone(),
        guides: doc.guides.clone(),
        layers: doc
            .layers
            .iter()
            .map(|layer| GeomLayer {
                id: layer.id,
                parent: layer.parent,
                name: layer.name.clone(),
                visible: layer.visible,
                locked: layer.locked,
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
        layer.parent = meta.parent;
        layer.visible = meta.visible;
        layer.locked = meta.locked;
        layer.opacity = meta.opacity;
        layer.blend = meta.blend;
        layer.x = meta.x;
        layer.y = meta.y;
        ordered.push(layer);
    }
    doc.layers = ordered;
    doc.selection = snap.selection;
    doc.guides = snap.guides;
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
        Command::SetLocked { index, locked } => {
            let layer = doc.layers.get(*index).ok_or(Error::BadLayer)?;
            Ok(layer.locked != *locked)
        }
        Command::SetOpacity { index, opacity } => {
            let layer = doc.layers.get(*index).ok_or(Error::BadLayer)?;
            Ok((layer.opacity - opacity.clamp(0.0, 1.0)).abs() > f32::EPSILON)
        }
        Command::MoveLayer { index, x, y } => {
            let layer = doc.layers.get(*index).ok_or(Error::BadLayer)?;
            Ok(layer.x != *x || layer.y != *y)
        }
        Command::MoveLayers { indices, dx, dy } => {
            if indices.iter().any(|&index| index >= doc.layers.len()) {
                return Err(Error::BadLayer);
            }
            Ok(!indices.is_empty() && (*dx != 0 || *dy != 0))
        }
        Command::AlignLayers { indices, to } => {
            Ok(!arrange::align_shifts(doc, indices, *to)?.is_empty())
        }
        Command::SetGuides { guides } => Ok(doc.guides != *guides),
        Command::SetBlend { index, blend } => {
            let layer = doc.layers.get(*index).ok_or(Error::BadLayer)?;
            Ok(layer.blend != *blend)
        }
        Command::Adjust { index, adjustment } => {
            doc.layers.get(*index).ok_or(Error::BadLayer)?;
            if doc.is_group(*index) {
                return Err(Error::WrongKind);
            }
            Ok(!adjustment.is_identity())
        }
        Command::AddText { .. } | Command::AddShape { .. } => Ok(true),
        Command::SetText { index, spec } => {
            match &doc.layers.get(*index).ok_or(Error::BadLayer)?.kind {
                LayerKind::Text { spec: current, .. } => Ok(current != spec),
                _ => Err(Error::WrongKind),
            }
        }
        Command::SetShape { index, spec } => {
            match &doc.layers.get(*index).ok_or(Error::BadLayer)?.kind {
                LayerKind::Shape { spec: current, .. } => Ok(current != spec),
                _ => Err(Error::WrongKind),
            }
        }
        Command::Rasterize { index } => {
            Ok(doc.layers.get(*index).ok_or(Error::BadLayer)?.is_vector())
        }
        Command::DistributeLayers { indices, axis } => {
            Ok(!arrange::distribute_shifts(doc, indices, *axis)?.is_empty())
        }
        Command::Reorder { from, to } => {
            if *from >= doc.layers.len() || *to >= doc.layers.len() {
                return Err(Error::BadLayer);
            }
            Ok(!doc.block(*from).contains(to))
        }
        Command::Group { indices } => {
            if indices.iter().any(|&index| index >= doc.layers.len()) {
                return Err(Error::BadLayer);
            }
            Ok(!indices.is_empty())
        }
        Command::Ungroup { index } => {
            if doc.is_group(*index) {
                Ok(true)
            } else {
                Err(Error::WrongKind)
            }
        }
        Command::MoveIntoGroup { index, group } => {
            if *index >= doc.layers.len() || !doc.is_group(*group) {
                return Err(Error::BadLayer);
            }
            if doc.block(*index).contains(group) {
                return Err(Error::WrongKind);
            }
            Ok(true)
        }
        Command::DeleteLayers { indices } => {
            if indices.iter().any(|&index| index >= doc.layers.len()) {
                return Err(Error::BadLayer);
            }
            let unique = doc.with_contents(indices);
            if unique.len() >= doc.layers.len() {
                return Err(Error::LastLayer);
            }
            Ok(!unique.is_empty())
        }
        Command::RotateLayer { index, degrees_cw } => {
            if *index >= doc.layers.len() {
                return Err(Error::BadLayer);
            }
            if doc.is_group(*index) {
                return Err(Error::WrongKind);
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
            if doc.is_group(*index) {
                return Err(Error::WrongKind);
            }
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
            if let Command::FlipLayer { index, .. } = command {
                if doc.is_group(*index) {
                    return Err(Error::WrongKind);
                }
            }
            if let Command::AddImageLayer { image, .. } = command {
                check_size(image.width(), image.height())?;
            }
            Ok(true)
        }
    }
}

/// Refuse edits to a locked layer. Runs after [`command_changes`], so a no-op
/// on a locked layer is still a quiet `Ok(false)`.
fn check_unlocked(doc: &Document, command: &Command) -> Result<(), Error> {
    let indices = match command {
        Command::SetOpacity { index, .. }
        | Command::SetBlend { index, .. }
        | Command::Adjust { index, .. }
        | Command::SetText { index, .. }
        | Command::SetShape { index, .. }
        | Command::Rasterize { index }
        | Command::MoveLayer { index, .. }
        | Command::ScaleLayer { index, .. }
        | Command::RotateLayer { index, .. }
        | Command::FlipLayer { index, .. } => std::slice::from_ref(index),
        Command::MoveLayers { indices, .. } => indices.as_slice(),
        _ => return Ok(()),
    };
    for &index in indices {
        if doc.layers.get(index).ok_or(Error::BadLayer)?.locked {
            return Err(Error::Locked);
        }
    }
    Ok(())
}

/// Move each listed layer, or everything in each listed group.
fn shift_layers(doc: &mut Document, shifts: Vec<arrange::Shift>) -> Result<(), Error> {
    for (index, dx, dy) in shifts {
        let layers: Vec<usize> = doc.block(index).filter(|&at| !doc.is_group(at)).collect();
        for at in layers {
            let layer = doc.layer_mut(at)?;
            layer.x += dx;
            layer.y += dy;
        }
    }
    Ok(())
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
        Command::DuplicateLayer { index } => groups::duplicate(doc, index)?,
        Command::DeleteLayers { indices } => {
            if indices.iter().any(|&index| index >= doc.layers.len()) {
                return Err(Error::BadLayer);
            }
            let unique: BTreeSet<usize> = doc.with_contents(&indices).into_iter().collect();
            if unique.len() >= doc.layers.len() {
                return Err(Error::LastLayer);
            }
            let lowest = *unique.first().ok_or(Error::BadLayer)?;
            let mut removed_selected = false;
            for &index in unique.iter().rev() {
                let layer = doc.layers.remove(index);
                removed_selected |= doc.selection.remove(&layer.id);
            }
            // Keep something selected so repeated deletes walk down the stack.
            if removed_selected && doc.selection.is_empty() {
                let next = lowest.min(doc.layers.len() - 1);
                doc.selection.insert(doc.layers[next].id);
            }
        }
        Command::Reorder { from, to } => groups::reorder(doc, from, to)?,
        Command::Group { indices } => groups::group(doc, &indices)?,
        Command::Ungroup { index } => groups::ungroup(doc, index)?,
        Command::MoveIntoGroup { index, group } => groups::move_into(doc, index, group)?,
        Command::Rename { index, name } => {
            doc.layer_mut(index)?.name = name;
        }
        Command::SetVisibility { index, visible } => {
            doc.layer_mut(index)?.visible = visible;
        }
        Command::SetLocked { index, locked } => {
            doc.layer_mut(index)?.locked = locked;
        }
        Command::SetOpacity { index, opacity } => {
            doc.layer_mut(index)?.opacity = opacity.clamp(0.0, 1.0);
        }
        Command::MoveLayer { index, x, y } => {
            let layer = doc.layer_mut(index)?;
            layer.x = x;
            layer.y = y;
        }
        Command::MoveLayers { indices, dx, dy } => {
            // A group has no place of its own. Moving it moves what it holds.
            let contents: Vec<usize> = doc
                .with_contents(&indices)
                .into_iter()
                .filter(|&index| !doc.is_group(index))
                .collect();
            for index in contents {
                let layer = doc.layer_mut(index)?;
                layer.x += dx;
                layer.y += dy;
            }
        }
        Command::AlignLayers { indices, to } => {
            let shifts = arrange::align_shifts(doc, &indices, to)?;
            shift_layers(doc, shifts)?;
        }
        Command::SetGuides { guides } => doc.guides = guides,
        Command::SetBlend { index, blend } => doc.layer_mut(index)?.blend = blend,
        Command::Adjust { index, adjustment } => {
            let layer = doc.layer_mut(index)?;
            layer.rasterize();
            layer.pixels = adjust(&layer.pixels, adjustment);
        }
        Command::AddText { spec, x, y } => {
            let mut layer = blank_layer(doc, &text_name(&spec.text));
            layer.kind = LayerKind::Text {
                spec,
                origin: (0, 0),
            };
            layer.redraw((x, y));
            doc.insert_layer(layer);
        }
        Command::AddShape { spec, x, y } => {
            let name = match spec.kind {
                ShapeKind::Rectangle => "Rectangle",
                ShapeKind::Ellipse => "Ellipse",
                ShapeKind::Line => "Line",
                ShapeKind::Arrow => "Arrow",
            };
            let mut layer = blank_layer(doc, name);
            layer.kind = LayerKind::Shape {
                spec,
                origin: (0, 0),
            };
            layer.redraw((x, y));
            doc.insert_layer(layer);
        }
        Command::SetText { index, spec: new } => {
            let layer = doc.layer_mut(index)?;
            let anchor = layer.anchor().ok_or(Error::WrongKind)?;
            match &mut layer.kind {
                LayerKind::Text { spec, .. } => *spec = new,
                _ => return Err(Error::WrongKind),
            }
            layer.redraw(anchor);
        }
        Command::SetShape { index, spec: new } => {
            let layer = doc.layer_mut(index)?;
            let anchor = layer.anchor().ok_or(Error::WrongKind)?;
            match &mut layer.kind {
                LayerKind::Shape { spec, .. } => *spec = new,
                _ => return Err(Error::WrongKind),
            }
            layer.redraw(anchor);
        }
        Command::Rasterize { index } => doc.layer_mut(index)?.rasterize(),
        Command::DistributeLayers { indices, axis } => {
            let shifts = arrange::distribute_shifts(doc, &indices, axis)?;
            shift_layers(doc, shifts)?;
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
        Command::RotateLayer { index, degrees_cw } => {
            doc.layer_mut(index)?.rasterize();
            ops::rotate_layer(doc, index, degrees_cw)?
        }
        Command::FlipCanvas { axis } => ops::flip_canvas(doc, axis),
        Command::FlipLayer { index, axis } => {
            doc.layer_mut(index)?.rasterize();
            ops::flip_layer(doc, index, axis)?
        }
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
    fn a_locked_layer_refuses_layer_edits_but_not_canvas_edits() {
        let mut editor = doc_with(4, 4, (1, 1), rgba(255, 0, 0, 255));
        editor
            .apply(Command::SetLocked {
                index: 0,
                locked: true,
            })
            .unwrap();
        let refused = [
            Command::MoveLayer {
                index: 0,
                x: 1,
                y: 0,
            },
            Command::SetOpacity {
                index: 0,
                opacity: 0.5,
            },
            Command::ScaleLayer {
                index: 0,
                x: 0,
                y: 0,
                width: 2,
                height: 2,
                filter: ScaleFilter::Nearest,
            },
            Command::RotateLayer {
                index: 0,
                degrees_cw: 90.0,
            },
            Command::FlipLayer {
                index: 0,
                axis: Axis::Horizontal,
            },
        ];
        for command in refused {
            assert!(matches!(editor.apply(command), Err(Error::Locked)));
        }
        assert!(!editor
            .apply(Command::MoveLayer {
                index: 0,
                x: 0,
                y: 0,
            })
            .unwrap());
        assert!(editor
            .apply(Command::Rename {
                index: 0,
                name: "Base".into(),
            })
            .unwrap());
        assert!(editor
            .apply(Command::FlipCanvas {
                axis: Axis::Horizontal,
            })
            .unwrap());
        assert_eq!(editor.document().layers[0].pixels.get_pixel(2, 1)[0], 255);
        assert!(editor.document().layers[0].locked);
    }

    #[test]
    fn locking_is_undoable_and_duplicates_keep_it() {
        let mut editor = doc_with(2, 2, (0, 0), rgba(255, 0, 0, 255));
        editor
            .apply(Command::SetLocked {
                index: 0,
                locked: true,
            })
            .unwrap();
        editor.apply(Command::DuplicateLayer { index: 0 }).unwrap();
        assert!(editor.document().layers[1].locked);
        editor.undo();
        editor.undo();
        assert!(!editor.document().layers[0].locked);
        editor.redo();
        assert!(editor.document().layers[0].locked);
    }

    #[test]
    fn deselecting_survives_layer_edits_and_a_new_layer_selects_itself() {
        let mut editor = doc_with(2, 2, (0, 0), rgba(255, 0, 0, 255));
        editor.apply(Command::AddLayer).unwrap();
        editor.apply(Command::AddLayer).unwrap();
        editor.deselect();
        assert_eq!(editor.document().active_index(), None);
        assert!(editor.document().active_layer().is_none());

        editor.apply(Command::Reorder { from: 0, to: 2 }).unwrap();
        editor
            .apply(Command::DeleteLayers { indices: vec![1] })
            .unwrap();
        assert_eq!(editor.document().active_index(), None);

        editor.set_active(0).unwrap();
        assert_eq!(editor.document().active_index(), Some(0));
        editor.deselect();
        editor.apply(Command::AddLayer).unwrap();
        assert_eq!(editor.document().active_index(), Some(2));
    }

    #[test]
    fn undo_brings_back_the_selection_the_edit_was_made_with() {
        let mut editor = doc_with(2, 2, (0, 0), rgba(255, 0, 0, 255));
        editor
            .apply(Command::MoveLayer {
                index: 0,
                x: 1,
                y: 0,
            })
            .unwrap();
        editor.deselect();
        editor.undo();
        assert_eq!(editor.document().active_index(), Some(0));

        editor.deselect();
        editor
            .apply(Command::MoveLayer {
                index: 0,
                x: 1,
                y: 1,
            })
            .unwrap();
        editor.set_active(0).unwrap();
        editor.undo();
        assert_eq!(editor.document().active_index(), None);
    }

    /// Three layers on an 8×8 canvas: an opaque background, a 2×2 square at
    /// (1, 1), and a 2×2 square at (5, 5).
    fn three_layers() -> Editor {
        let mut editor = doc_with(8, 8, (0, 0), rgba(255, 0, 0, 255));
        for pixel in editor.doc.layers[0].pixels.pixels_mut() {
            *pixel = rgba(255, 255, 255, 255);
        }
        for origin in [1, 5] {
            let mut square = RgbaImage::new(2, 2);
            for pixel in square.pixels_mut() {
                *pixel = rgba(0, 0, 255, 255);
            }
            editor
                .apply(Command::AddImageLayer {
                    name: format!("Square {origin}"),
                    image: square,
                })
                .unwrap();
            let last = editor.doc.layers.len() - 1;
            editor.doc.layers[last].x = origin;
            editor.doc.layers[last].y = origin;
        }
        editor
    }

    fn area(x: i32, y: i32, width: u32, height: u32) -> PixelRect {
        PixelRect {
            x,
            y,
            width,
            height,
        }
    }

    #[test]
    fn layers_within_needs_every_drawn_pixel_inside_the_area() {
        let editor = three_layers();
        let doc = editor.document();
        assert_eq!(doc.layers_within(area(0, 0, 8, 8)), vec![0, 1, 2]);
        assert_eq!(doc.layers_within(area(1, 1, 7, 7)), vec![1, 2]);
        assert_eq!(doc.layers_within(area(0, 0, 4, 4)), vec![1]);
        assert_eq!(doc.layers_within(area(2, 2, 4, 4)), Vec::<usize>::new());
    }

    #[test]
    fn layers_within_skips_empty_and_hidden_layers_and_clips_to_the_canvas() {
        let mut editor = three_layers();
        editor.apply(Command::AddLayer).unwrap();
        editor
            .apply(Command::SetVisibility {
                index: 1,
                visible: false,
            })
            .unwrap();
        assert_eq!(
            editor.document().layers_within(area(0, 0, 8, 8)),
            vec![0, 2]
        );

        editor.doc.layers[2].x = 7;
        assert_eq!(
            editor.document().layers_within(area(6, 4, 2, 4)),
            vec![2],
            "the part of the square off the canvas does not count"
        );
    }

    #[test]
    fn content_bounds_covers_visible_layers_on_the_canvas_only() {
        let mut editor = three_layers();
        editor
            .apply(Command::SetVisibility {
                index: 0,
                visible: false,
            })
            .unwrap();
        // The squares cover (1, 1) to (7, 7).
        assert_eq!(editor.document().content_bounds(), Some(area(1, 1, 6, 6)));

        editor.doc.layers[2].x = 7;
        assert_eq!(
            editor.document().content_bounds(),
            Some(area(1, 1, 7, 6)),
            "a square hanging off the right edge stops at the canvas"
        );

        for index in [1, 2] {
            editor
                .apply(Command::SetVisibility {
                    index,
                    visible: false,
                })
                .unwrap();
        }
        assert_eq!(editor.document().content_bounds(), None);
    }

    fn set_guides(editor: &mut Editor, x: &[i32], y: &[i32]) {
        editor
            .apply(Command::SetGuides {
                guides: Guides {
                    x: x.to_vec(),
                    y: y.to_vec(),
                },
            })
            .unwrap();
    }

    fn guides_of(editor: &Editor) -> (Vec<i32>, Vec<i32>) {
        let guides = editor.document().guides();
        (guides.x.clone(), guides.y.clone())
    }

    #[test]
    fn guides_are_undoable_and_follow_the_canvas_through_crop_and_resize() {
        let mut editor = doc_with(10, 8, (0, 0), rgba(255, 0, 0, 255));
        set_guides(&mut editor, &[3, 9], &[2]);
        assert!(!editor
            .apply(Command::SetGuides {
                guides: editor.document().guides().clone(),
            })
            .unwrap());
        editor
            .apply(Command::Crop {
                x: 2,
                y: 1,
                width: 6,
                height: 6,
            })
            .unwrap();
        assert_eq!(guides_of(&editor), (vec![1, 7], vec![1]));
        editor
            .apply(Command::ResizeCanvas {
                width: 8,
                height: 8,
                anchor: Anchor::Center,
            })
            .unwrap();
        assert_eq!(guides_of(&editor), (vec![2, 8], vec![2]));
        editor.undo();
        editor.undo();
        assert_eq!(guides_of(&editor), (vec![3, 9], vec![2]));
        editor.undo();
        assert_eq!(guides_of(&editor), (vec![], vec![]));
    }

    #[test]
    fn guides_turn_and_mirror_with_the_canvas() {
        let mut editor = doc_with(10, 6, (0, 0), rgba(255, 0, 0, 255));
        set_guides(&mut editor, &[3], &[1]);
        editor
            .apply(Command::RotateCanvas {
                turn: QuarterTurn::Cw,
            })
            .unwrap();
        // The row at y = 1 of a 6 px tall canvas becomes the column at x = 5.
        assert_eq!(guides_of(&editor), (vec![5], vec![3]));
        editor
            .apply(Command::RotateCanvas {
                turn: QuarterTurn::Ccw,
            })
            .unwrap();
        assert_eq!(guides_of(&editor), (vec![3], vec![1]));
        editor
            .apply(Command::FlipCanvas {
                axis: Axis::Horizontal,
            })
            .unwrap();
        assert_eq!(guides_of(&editor), (vec![7], vec![1]));
        editor
            .apply(Command::ScaleDocument {
                width: 20,
                height: 12,
                filter: ScaleFilter::Nearest,
            })
            .unwrap();
        assert_eq!(guides_of(&editor), (vec![14], vec![2]));
    }

    /// A 1×1 document: `below` on the first layer, `above` on a second one
    /// with `blend`.
    fn blended(below: Rgba<u8>, above: Rgba<u8>, blend: BlendMode) -> Rgba<u8> {
        let mut editor = doc_with(1, 1, (0, 0), below);
        editor.apply(Command::AddLayer).unwrap();
        editor.doc.layers[1].pixels.put_pixel(0, 0, above);
        editor
            .apply(Command::SetBlend { index: 1, blend })
            .unwrap_or(false);
        *composite(editor.document()).get_pixel(0, 0)
    }

    #[test]
    fn blend_modes_mix_with_what_is_below() {
        let white = rgba(255, 255, 255, 255);
        let gray = rgba(128, 128, 128, 255);
        let black = rgba(0, 0, 0, 255);
        assert_eq!(blended(white, gray, BlendMode::Multiply), gray);
        assert_eq!(blended(black, gray, BlendMode::Screen), gray);
        assert_eq!(
            blended(white, gray, BlendMode::Difference),
            rgba(127, 127, 127, 255)
        );
        assert_eq!(blended(black, white, BlendMode::Darken), black);
        assert_eq!(blended(gray, gray, BlendMode::Normal), gray);
    }

    #[test]
    fn a_blend_mode_over_nothing_shows_the_layer_as_it_is() {
        let clear = rgba(0, 0, 0, 0);
        let red = rgba(200, 30, 30, 255);
        for blend in BlendMode::ALL {
            assert_eq!(blended(clear, red, blend), red, "{blend:?}");
        }
    }

    #[test]
    fn setting_a_blend_mode_is_undoable_and_respects_the_lock() {
        let mut editor = doc_with(1, 1, (0, 0), rgba(0, 0, 0, 255));
        assert!(editor
            .apply(Command::SetBlend {
                index: 0,
                blend: BlendMode::Screen,
            })
            .unwrap());
        editor.undo();
        assert_eq!(editor.document().layers[0].blend, BlendMode::Normal);
        editor
            .apply(Command::SetLocked {
                index: 0,
                locked: true,
            })
            .unwrap();
        for command in [
            Command::SetBlend {
                index: 0,
                blend: BlendMode::Screen,
            },
            Command::Adjust {
                index: 0,
                adjustment: Adjustment::Grayscale,
            },
        ] {
            assert!(matches!(editor.apply(command), Err(Error::Locked)));
        }
    }

    #[test]
    fn an_adjustment_is_one_undo_step_and_a_no_op_is_skipped() {
        let mut editor = doc_with(1, 1, (0, 0), rgba(0, 255, 0, 255));
        assert!(!editor
            .apply(Command::Adjust {
                index: 0,
                adjustment: Adjustment::Blur { radius: 0.0 },
            })
            .unwrap());
        editor
            .apply(Command::Adjust {
                index: 0,
                adjustment: Adjustment::Grayscale,
            })
            .unwrap();
        assert_eq!(editor.document().layers[0].pixels.get_pixel(0, 0)[0], 182);
        editor.undo();
        assert_eq!(
            editor.document().layers[0].pixels.get_pixel(0, 0),
            &rgba(0, 255, 0, 255)
        );
    }

    fn label(text: &str) -> TextSpec {
        TextSpec {
            text: text.into(),
            font: "Sans".into(),
            size: 24.0,
            color: [255, 255, 255, 255],
            align: TextAlign::Left,
        }
    }

    fn arrow(dx: f32, dy: f32) -> ShapeSpec {
        ShapeSpec {
            kind: ShapeKind::Arrow,
            dx,
            dy,
            stroke: [255, 0, 0, 255],
            stroke_width: 4.0,
            fill: None,
        }
    }

    #[test]
    fn text_edits_redraw_in_place_and_undo() {
        let mut editor = doc_with(200, 100, (0, 0), rgba(0, 0, 0, 255));
        editor
            .apply(Command::AddText {
                spec: label("Hi"),
                x: 30,
                y: 20,
            })
            .unwrap();
        let layer = &editor.document().layers[1];
        assert_eq!(layer.name, "Hi");
        assert_eq!(layer.anchor(), Some((30, 20)));
        let narrow = layer.width();

        editor
            .apply(Command::SetText {
                index: 1,
                spec: label("Hello there"),
            })
            .unwrap();
        let layer = &editor.document().layers[1];
        assert!(layer.width() > narrow);
        assert_eq!(layer.anchor(), Some((30, 20)), "the corner stays put");
        editor.undo();
        assert_eq!(editor.document().layers[1].width(), narrow);
        assert!(matches!(
            editor.apply(Command::SetShape {
                index: 1,
                spec: arrow(10.0, 0.0),
            }),
            Err(Error::WrongKind)
        ));
    }

    #[test]
    fn resizing_a_shape_redraws_it_and_rotating_turns_it_into_pixels() {
        let mut editor = doc_with(200, 200, (0, 0), rgba(0, 0, 0, 255));
        editor
            .apply(Command::AddShape {
                spec: arrow(-50.0, 20.0),
                x: 40,
                y: 40,
            })
            .unwrap();
        let layer = &editor.document().layers[1];
        let (pad, _) = match layer.kind {
            LayerKind::Shape { origin, .. } => origin,
            _ => panic!("not a shape"),
        };
        let (x, y) = (layer.x, layer.y);
        editor
            .apply(Command::ScaleLayer {
                index: 1,
                x,
                y,
                width: 100 + 2 * pad as u32,
                height: 40 + 2 * pad as u32,
                filter: ScaleFilter::Lanczos3,
            })
            .unwrap();
        match &editor.document().layers[1].kind {
            LayerKind::Shape { spec, .. } => {
                assert_eq!((spec.dx, spec.dy), (-100.0, 40.0), "the direction is kept");
            }
            _ => panic!("scaling should keep the shape editable"),
        }
        editor
            .apply(Command::RotateLayer {
                index: 1,
                degrees_cw: 90.0,
            })
            .unwrap();
        assert_eq!(editor.document().layers[1].kind, LayerKind::Raster);
    }

    #[test]
    fn crop_moves_text_without_cutting_it_and_image_size_scales_it() {
        let mut editor = doc_with(200, 100, (0, 0), rgba(0, 0, 0, 255));
        editor
            .apply(Command::AddText {
                spec: label("Wide label"),
                x: 10,
                y: 10,
            })
            .unwrap();
        let width = editor.document().layers[1].width();
        editor
            .apply(Command::Crop {
                x: 20,
                y: 5,
                width: 30,
                height: 30,
            })
            .unwrap();
        let layer = &editor.document().layers[1];
        assert_eq!(layer.width(), width);
        assert_eq!(layer.anchor(), Some((-10, 5)));
        editor
            .apply(Command::ScaleDocument {
                width: 60,
                height: 60,
                filter: ScaleFilter::Lanczos3,
            })
            .unwrap();
        match &editor.document().layers[1].kind {
            LayerKind::Text { spec, .. } => assert_eq!(spec.size, 48.0),
            _ => panic!("still text"),
        }
        assert_eq!(editor.document().layers[1].anchor(), Some((-20, 10)));
    }

    #[test]
    fn layers_within_includes_locked_layers() {
        let mut editor = three_layers();
        editor
            .apply(Command::SetLocked {
                index: 1,
                locked: true,
            })
            .unwrap();
        assert_eq!(editor.document().layers_within(area(0, 0, 4, 4)), vec![1]);
    }

    #[test]
    fn several_selected_layers_have_no_single_active_layer() {
        let mut editor = three_layers();
        editor.set_selection(&[2, 1]).unwrap();
        let doc = editor.document();
        assert_eq!(doc.selected_indices(), vec![1, 2]);
        assert_eq!(doc.active_index(), None);
        assert!(doc.is_selected(1) && !doc.is_selected(0));

        editor.toggle_selected(2).unwrap();
        assert_eq!(editor.document().active_index(), Some(1));
        editor.toggle_selected(0).unwrap();
        assert_eq!(editor.document().selected_indices(), vec![0, 1]);
        assert!(editor.set_selection(&[3]).is_err());
    }

    #[test]
    fn the_selection_follows_layers_through_a_reorder() {
        let mut editor = three_layers();
        editor.set_selection(&[0, 2]).unwrap();
        let ids: Vec<u64> = [0, 2].map(|index| editor.doc.layers[index].id).to_vec();
        editor.apply(Command::Reorder { from: 0, to: 1 }).unwrap();
        let selected: Vec<u64> = editor
            .document()
            .selected_indices()
            .into_iter()
            .map(|index| editor.doc.layers[index].id)
            .collect();
        assert_eq!(selected, ids);
    }

    #[test]
    fn move_layers_offsets_each_layer_once_as_one_undo_step() {
        let mut editor = three_layers();
        editor
            .apply(Command::MoveLayers {
                indices: vec![1, 2, 2],
                dx: 2,
                dy: -1,
            })
            .unwrap();
        let positions = |editor: &Editor| -> Vec<(i32, i32)> {
            editor
                .doc
                .layers
                .iter()
                .map(|layer| (layer.x, layer.y))
                .collect()
        };
        assert_eq!(positions(&editor), vec![(0, 0), (3, 0), (7, 4)]);
        editor.undo();
        assert_eq!(positions(&editor), vec![(0, 0), (1, 1), (5, 5)]);

        editor
            .apply(Command::SetLocked {
                index: 2,
                locked: true,
            })
            .unwrap();
        let err = editor
            .apply(Command::MoveLayers {
                indices: vec![1, 2],
                dx: 1,
                dy: 0,
            })
            .unwrap_err();
        assert!(matches!(err, Error::Locked));
        assert_eq!(positions(&editor), vec![(0, 0), (1, 1), (5, 5)]);
    }

    #[test]
    fn deleting_several_layers_selects_the_one_that_takes_their_place() {
        let mut editor = three_layers();
        editor.apply(Command::AddLayer).unwrap();
        let top = editor.doc.layers[3].id;
        editor.set_selection(&[1, 2]).unwrap();
        editor
            .apply(Command::DeleteLayers {
                indices: vec![1, 2],
            })
            .unwrap();
        assert_eq!(editor.document().layers().len(), 2);
        assert_eq!(editor.document().active_layer().unwrap().id, top);

        let err = editor
            .apply(Command::DeleteLayers {
                indices: vec![0, 1],
            })
            .unwrap_err();
        assert!(matches!(err, Error::LastLayer));
    }

    #[test]
    fn deleting_unselected_layers_keeps_the_selection() {
        let mut editor = three_layers();
        editor.deselect();
        editor
            .apply(Command::DeleteLayers { indices: vec![1] })
            .unwrap();
        assert_eq!(editor.document().selected_indices(), Vec::<usize>::new());

        editor.set_active(0).unwrap();
        editor
            .apply(Command::DeleteLayers { indices: vec![1] })
            .unwrap();
        assert_eq!(editor.document().active_index(), Some(0));
    }

    #[test]
    fn aligning_is_one_undo_step_and_leaves_locked_layers_in_place() {
        let mut editor = three_layers();
        editor
            .apply(Command::SetLocked {
                index: 0,
                locked: true,
            })
            .unwrap();
        let positions = |editor: &Editor| -> Vec<(i32, i32)> {
            editor
                .doc
                .layers
                .iter()
                .map(|layer| (layer.x, layer.y))
                .collect()
        };
        editor
            .apply(Command::AlignLayers {
                indices: vec![0, 1, 2],
                to: Alignment::Right,
            })
            .unwrap();
        assert_eq!(positions(&editor), vec![(0, 0), (5, 1), (5, 5)]);
        assert!(!editor
            .apply(Command::AlignLayers {
                indices: vec![1, 2],
                to: Alignment::Right,
            })
            .unwrap());
        editor.undo();
        assert_eq!(positions(&editor), vec![(0, 0), (1, 1), (5, 5)]);
    }

    #[test]
    fn composite_layers_flattens_only_the_listed_layers_and_trims_them() {
        let editor = three_layers();
        let doc = editor.document();
        let copied = composite_layers(doc, &[1, 2]).unwrap();
        // The squares sit at (1, 1) and (5, 5), so together they span 6×6.
        assert_eq!(copied.dimensions(), (6, 6));
        assert_eq!(copied.get_pixel(0, 0), &rgba(0, 0, 255, 255));
        assert_eq!(copied.get_pixel(5, 5), &rgba(0, 0, 255, 255));
        assert_eq!(copied.get_pixel(3, 3)[3], 0, "the background is left out");
        assert_eq!(composite_layers(doc, &[0]).unwrap().dimensions(), (8, 8));
        assert!(composite_layers(doc, &[]).is_none());
    }

    #[test]
    fn only_the_unlocked_pick_passes_through_locked_layers() {
        let mut editor = doc_with(2, 2, (0, 0), rgba(255, 0, 0, 255));
        editor.apply(Command::AddLayer).unwrap();
        editor.doc.layers[1]
            .pixels
            .put_pixel(0, 0, rgba(0, 255, 0, 255));
        editor
            .apply(Command::SetLocked {
                index: 1,
                locked: true,
            })
            .unwrap();
        assert_eq!(editor.document().layer_at(0.0, 0.0), Some(1));
        assert_eq!(editor.document().unlocked_layer_at(0.0, 0.0), Some(0));
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
        let err = editor
            .apply(Command::DeleteLayers { indices: vec![0] })
            .unwrap_err();
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
        let active_id = editor.document().active_layer().unwrap().id;
        editor.apply(Command::Reorder { from: 0, to: 2 }).unwrap();
        let after: Vec<_> = editor
            .document()
            .layers()
            .iter()
            .map(|layer| layer.name.clone())
            .collect();
        assert_eq!(after[2], names[0]);
        assert_eq!(editor.document().active_layer().unwrap().id, active_id);
        assert_ne!(editor.document().active_index(), Some(2));
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
