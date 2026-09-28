//! Snapping dragged layers, handles, and guides to nearby lines.

use pixel::document::{Document, PixelRect};

/// How close, in screen pixels, a dragged edge has to come to a line to snap.
pub const SNAP_DISTANCE: f64 = 6.0;

/// Vertical lines at `x` and horizontal lines at `y` a drag can snap to, in
/// document pixels.
#[derive(Clone, Default)]
pub struct SnapTargets {
    pub x: Vec<f64>,
    pub y: Vec<f64>,
}

impl SnapTargets {
    /// The canvas edges and centre, the edges and centres of what other
    /// visible layers draw, and the guides when they are shown. Layers in
    /// `moving` are left out, since they can't snap to themselves.
    pub fn collect(doc: &Document, moving: &[usize], guides: bool) -> Self {
        let (w, h) = (doc.width as f64, doc.height as f64);
        let mut targets = Self {
            x: vec![0.0, w / 2.0, w],
            y: vec![0.0, h / 2.0, h],
        };
        for (index, layer) in doc.layers().iter().enumerate() {
            if !doc.shown(index) || moving.contains(&index) {
                continue;
            }
            if let Some(rect) = layer.content_bounds() {
                let [left, center_x, right] = x_lines(rect);
                let [top, center_y, bottom] = y_lines(rect);
                targets.x.extend([left, center_x, right]);
                targets.y.extend([top, center_y, bottom]);
            }
        }
        if guides {
            targets.x.extend(doc.guides().x.iter().map(|&x| x as f64));
            targets.y.extend(doc.guides().y.iter().map(|&y| y as f64));
        }
        targets
    }
}

/// Left edge, centre, and right edge.
pub fn x_lines(rect: PixelRect) -> [f64; 3] {
    let left = rect.x as f64;
    let width = rect.width as f64;
    [left, left + width / 2.0, left + width]
}

/// Top edge, centre, and bottom edge.
pub fn y_lines(rect: PixelRect) -> [f64; 3] {
    let top = rect.y as f64;
    let height = rect.height as f64;
    [top, top + height / 2.0, top + height]
}

/// The shift that puts the closest of `lines` onto a target within
/// `tolerance`, and the target it lands on.
pub fn snap_lines(lines: &[f64], targets: &[f64], tolerance: f64) -> Option<(f64, f64)> {
    let mut best: Option<(f64, f64)> = None;
    for &line in lines {
        for &target in targets {
            let shift = target - line;
            if shift.abs() <= tolerance && best.is_none_or(|(held, _)| shift.abs() < held.abs()) {
                best = Some((shift, target));
            }
        }
    }
    best
}

/// The box around the drawn pixels of `indices`, falling back to their layer
/// boxes when they draw nothing. Groups draw nothing of their own, so they
/// add nothing. `None` for an empty list.
pub fn moving_bounds(doc: &Document, indices: &[usize]) -> Option<PixelRect> {
    let rects = indices
        .iter()
        .filter(|&&index| !doc.is_group(index))
        .map(|&index| {
            let layer = &doc.layers()[index];
            layer.content_bounds().unwrap_or(PixelRect {
                x: layer.x,
                y: layer.y,
                width: layer.width(),
                height: layer.height(),
            })
        });
    rects.reduce(|a, b| {
        let left = a.x.min(b.x);
        let top = a.y.min(b.y);
        let right = (a.x + a.width as i32).max(b.x + b.width as i32);
        let bottom = (a.y + a.height as i32).max(b.y + b.height as i32);
        PixelRect {
            x: left,
            y: top,
            width: (right - left) as u32,
            height: (bottom - top) as u32,
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{Rgba, RgbaImage};
    use pixel::document::{Background, Command, Editor, NewCanvas};

    #[test]
    fn the_nearest_line_within_reach_wins() {
        let targets = [0.0, 50.0, 100.0];
        assert_eq!(snap_lines(&[47.0, 60.0], &targets, 5.0), Some((3.0, 50.0)));
        assert_eq!(snap_lines(&[98.5], &targets, 5.0), Some((1.5, 100.0)));
        assert_eq!(snap_lines(&[20.0, 70.0], &targets, 5.0), None);
    }

    #[test]
    fn targets_cover_the_canvas_other_layers_and_guides() {
        let mut editor = Editor::new(
            Document::new(NewCanvas {
                width: 100,
                height: 60,
                ppi: 72.0,
                background: Background::Transparent,
            })
            .unwrap(),
        );
        let square = RgbaImage::from_pixel(10, 20, Rgba([0, 0, 0, 255]));
        editor
            .apply(Command::AddImageLayer {
                name: "Square".into(),
                image: square,
            })
            .unwrap();
        let targets = SnapTargets::collect(editor.document(), &[], false);
        // The blank first layer draws nothing. The square is centred.
        assert_eq!(targets.x, vec![0.0, 50.0, 100.0, 45.0, 50.0, 55.0]);
        assert_eq!(targets.y, vec![0.0, 30.0, 60.0, 20.0, 30.0, 40.0]);
        let without = SnapTargets::collect(editor.document(), &[1], false);
        assert_eq!(without.x, vec![0.0, 50.0, 100.0]);
    }
}
