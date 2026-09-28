//! Window state that is not part of the undoable document.

use image::RgbaImage;
use pixel::document::{composite_with, Background, Document, Editor, LayerOverride};

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Tool {
    Move,
    Crop,
}

#[derive(Clone, Copy)]
pub struct CropDraft {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
}

pub enum Preview {
    Move { index: usize, x: i32, y: i32 },
    Opacity { index: usize, opacity: f32 },
}

pub struct Session {
    pub editor: Editor,
    pub path: Option<std::path::PathBuf>,
    pub dirty: bool,
    pub tool: Tool,
    pub zoom: f64,
    pub pan_x: f64,
    pub pan_y: f64,
    pub fit_pending: bool,
    pub space_down: bool,
    pub crop: Option<CropDraft>,
    pub preview: Option<Preview>,
    /// Bumped whenever the flattened image changes. Pan and zoom do not bump it.
    pub visual: u64,
    pub cursor: Option<(i32, i32)>,
}

pub struct Model {
    pub session: Option<Session>,
    pub accent: (f32, f32, f32),
    pub background: (f32, f32, f32),
}

impl Model {
    pub fn open_editor(&mut self, editor: Editor, path: Option<std::path::PathBuf>) {
        self.session = Some(Session {
            editor,
            path,
            dirty: false,
            tool: Tool::Move,
            zoom: 1.0,
            pan_x: 0.0,
            pan_y: 0.0,
            fit_pending: true,
            space_down: false,
            crop: None,
            preview: None,
            visual: 1,
            cursor: None,
        });
    }
}

pub fn document_title(session: Option<&Session>) -> String {
    let Some(session) = session else {
        return "Pixel".into();
    };
    let name = session
        .path
        .as_ref()
        .and_then(|path| path.file_name())
        .and_then(|name| name.to_str())
        .unwrap_or("Untitled");
    if session.dirty {
        format!("{name}* · Pixel")
    } else {
        format!("{name} · Pixel")
    }
}

pub fn rendered(session: &Session) -> RgbaImage {
    let doc = session.editor.document();
    let overrides = session.preview.as_ref().map(|preview| {
        let (index, x, y, opacity, visible) = match *preview {
            Preview::Move { index, x, y } => {
                let layer = &doc.layers()[index];
                (index, x, y, layer.opacity, layer.visible)
            }
            Preview::Opacity { index, opacity } => {
                let layer = &doc.layers()[index];
                (index, layer.x, layer.y, opacity, layer.visible)
            }
        };
        vec![LayerOverride {
            index,
            x,
            y,
            opacity,
            visible,
        }]
    });
    composite_with(doc, overrides.as_deref())
}

pub fn fit_view(session: &mut Session, alloc_w: i32, alloc_h: i32) {
    let doc = session.editor.document();
    let avail_w = (alloc_w as f64 - 32.0).max(1.0);
    let avail_h = (alloc_h as f64 - 32.0).max(1.0);
    let zoom = (avail_w / doc.width as f64)
        .min(avail_h / doc.height as f64)
        .clamp(0.05, 32.0);
    session.zoom = zoom;
    session.pan_x = (alloc_w as f64 - doc.width as f64 * zoom) / 2.0;
    session.pan_y = (alloc_h as f64 - doc.height as f64 * zoom) / 2.0;
}

pub fn widget_to_doc(session: &Session, x: f64, y: f64) -> (f64, f64) {
    (
        (x - session.pan_x) / session.zoom,
        (y - session.pan_y) / session.zoom,
    )
}

pub fn jpeg_needs_white(doc: &Document) -> bool {
    matches!(doc.background, Background::Transparent)
        || matches!(doc.background, Background::Solid([_, _, _, alpha]) if alpha < 255)
}
