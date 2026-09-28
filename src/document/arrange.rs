//! Align and distribute layers by their visible pixels.

use super::{Alignment, Axis, Document, Error, PixelRect};
use std::collections::BTreeSet;

/// A layer and how far to shift it.
pub(crate) type Shift = (usize, i32, i32);

/// The listed layers an arrange command may move, bottom to top, with their
/// content boxes. Locked and empty layers are left out.
fn movable(doc: &Document, indices: &[usize]) -> Result<Vec<(usize, PixelRect)>, Error> {
    let unique: BTreeSet<usize> = indices.iter().copied().collect();
    let mut items = Vec::with_capacity(unique.len());
    for index in unique {
        let layer = doc.layers.get(index).ok_or(Error::BadLayer)?;
        if layer.locked {
            continue;
        }
        if let Some(bounds) = layer.content_bounds() {
            items.push((index, bounds));
        }
    }
    Ok(items)
}

/// One layer lines up with the canvas. Several line up with the box around
/// them all. Layers already in place are left out of the result.
pub(crate) fn align_shifts(
    doc: &Document,
    indices: &[usize],
    to: Alignment,
) -> Result<Vec<Shift>, Error> {
    let items = movable(doc, indices)?;
    let target = match items.as_slice() {
        [] => return Ok(Vec::new()),
        [_] => (0, 0, doc.width as i64, doc.height as i64),
        _ => items.iter().fold(
            (i64::MAX, i64::MAX, i64::MIN, i64::MIN),
            |(left, top, right, bottom), (_, rect)| {
                let (l, t, r, b) = edges(*rect);
                (left.min(l), top.min(t), right.max(r), bottom.max(b))
            },
        ),
    };
    let (left, top, right, bottom) = target;
    let shifts = items.into_iter().map(|(index, rect)| {
        let (l, t, r, b) = edges(rect);
        let (dx, dy) = match to {
            Alignment::Left => (left - l, 0),
            Alignment::HorizontalCenter => (((left + right) - (l + r)).div_euclid(2), 0),
            Alignment::Right => (right - r, 0),
            Alignment::Top => (0, top - t),
            Alignment::VerticalCenter => (0, ((top + bottom) - (t + b)).div_euclid(2)),
            Alignment::Bottom => (0, bottom - b),
        };
        (index, dx as i32, dy as i32)
    });
    Ok(shifts.filter(|&(_, dx, dy)| dx != 0 || dy != 0).collect())
}

/// Space layers so the gaps between them along `axis` are equal. The two
/// outermost layers stay put, so this needs at least three. Layers already
/// in place are left out of the result.
pub(crate) fn distribute_shifts(
    doc: &Document,
    indices: &[usize],
    axis: Axis,
) -> Result<Vec<Shift>, Error> {
    let mut items: Vec<(usize, i64, i64)> = movable(doc, indices)?
        .into_iter()
        .map(|(index, rect)| {
            let (l, t, r, b) = edges(rect);
            match axis {
                Axis::Horizontal => (index, l, r - l),
                Axis::Vertical => (index, t, b - t),
            }
        })
        .collect();
    if items.len() < 3 {
        return Ok(Vec::new());
    }
    items.sort_by_key(|&(index, start, _)| (start, index));
    let start = items[0].1;
    let end = items
        .iter()
        .map(|&(_, start, size)| start + size)
        .max()
        .unwrap_or(start);
    let sizes: i64 = items.iter().map(|&(_, _, size)| size).sum();
    let gap = (end - start - sizes) as f64 / (items.len() - 1) as f64;
    let mut cursor = start as f64;
    let mut shifts = Vec::new();
    for (index, from, size) in items {
        let delta = cursor.round() as i64 - from;
        cursor += size as f64 + gap;
        if delta != 0 {
            shifts.push(match axis {
                Axis::Horizontal => (index, delta as i32, 0),
                Axis::Vertical => (index, 0, delta as i32),
            });
        }
    }
    Ok(shifts)
}

fn edges(rect: PixelRect) -> (i64, i64, i64, i64) {
    let (x, y) = (rect.x as i64, rect.y as i64);
    (x, y, x + rect.width as i64, y + rect.height as i64)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::document::{Background, Layer, NewCanvas};
    use image::{Rgba, RgbaImage};

    /// A 20×20 transparent canvas with one solid layer per `(x, y, w, h)`.
    fn doc_of(rects: &[(i32, i32, u32, u32)]) -> Document {
        let mut doc = Document::new(NewCanvas {
            width: 20,
            height: 20,
            ppi: 72.0,
            background: Background::Transparent,
        })
        .unwrap();
        doc.layers.clear();
        for (i, &(x, y, width, height)) in rects.iter().enumerate() {
            let mut layer = doc_layer(&mut doc, i);
            layer.pixels = RgbaImage::from_pixel(width, height, Rgba([0, 0, 0, 255]));
            layer.x = x;
            layer.y = y;
            doc.layers.push(layer);
        }
        doc
    }

    fn doc_layer(doc: &mut Document, i: usize) -> Layer {
        super::super::blank_layer(doc, &format!("Layer {i}"))
    }

    #[test]
    fn several_layers_align_to_the_box_around_them() {
        let doc = doc_of(&[(2, 1, 4, 4), (10, 6, 6, 2)]);
        assert_eq!(
            align_shifts(&doc, &[0, 1], Alignment::Left).unwrap(),
            vec![(1, -8, 0)]
        );
        assert_eq!(
            align_shifts(&doc, &[0, 1], Alignment::Right).unwrap(),
            vec![(0, 10, 0)]
        );
        assert_eq!(
            align_shifts(&doc, &[0, 1], Alignment::Bottom).unwrap(),
            vec![(0, 0, 3)]
        );
        // The box runs 2..16, so its center is 9. The layers' centers are 4 and 13.
        assert_eq!(
            align_shifts(&doc, &[0, 1], Alignment::HorizontalCenter).unwrap(),
            vec![(0, 5, 0), (1, -4, 0)]
        );
    }

    #[test]
    fn one_layer_aligns_to_the_canvas() {
        let doc = doc_of(&[(2, 1, 4, 4)]);
        assert_eq!(
            align_shifts(&doc, &[0], Alignment::Right).unwrap(),
            vec![(0, 14, 0)]
        );
        assert_eq!(
            align_shifts(&doc, &[0], Alignment::VerticalCenter).unwrap(),
            vec![(0, 0, 7)]
        );
    }

    #[test]
    fn alignment_uses_visible_pixels_and_skips_locked_and_empty_layers() {
        let mut doc = doc_of(&[(0, 0, 10, 10), (12, 12, 4, 4), (6, 6, 2, 2)]);
        // Only the bottom-right 2×2 of layer 0 is drawn.
        for (x, y, pixel) in doc.layers[0].pixels.enumerate_pixels_mut() {
            if x < 8 || y < 8 {
                *pixel = Rgba([0, 0, 0, 0]);
            }
        }
        doc.layers[2].locked = true;
        let empty = doc_layer(&mut doc, 3);
        doc.layers.push(empty);
        assert_eq!(
            align_shifts(&doc, &[0, 1, 2, 3], Alignment::Left).unwrap(),
            vec![(1, -4, 0)]
        );
        assert!(align_shifts(&doc, &[9], Alignment::Left).is_err());
    }

    #[test]
    fn distributing_evens_the_gaps_and_keeps_the_outer_layers() {
        let doc = doc_of(&[(0, 0, 2, 2), (3, 6, 4, 2), (16, 10, 4, 2)]);
        // Across: a 20 px span holding 10 px of layers leaves two 5 px gaps.
        // Down: a 12 px span holding 6 px leaves two 3 px gaps.
        assert_eq!(
            distribute_shifts(&doc, &[0, 1, 2], Axis::Horizontal).unwrap(),
            vec![(1, 4, 0)]
        );
        assert_eq!(
            distribute_shifts(&doc, &[0, 1, 2], Axis::Vertical).unwrap(),
            vec![(1, 0, -1)]
        );
        assert!(distribute_shifts(&doc, &[0, 1], Axis::Horizontal)
            .unwrap()
            .is_empty());
    }
}
