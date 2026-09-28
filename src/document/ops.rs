//! Geometry and pixel transforms. Each function mutates the document in place.
//! Callers snapshot the document for undo before calling.

use super::{Anchor, Axis, Document, Error, QuarterTurn};
use image::imageops::{self, FilterType};
use image::{Rgba, RgbaImage};
use imageproc::geometric_transformations::{rotate_about_center, Interpolation};

pub fn reorder(doc: &mut Document, from: usize, to: usize) -> Result<(), Error> {
    if from >= doc.layers.len() || to >= doc.layers.len() {
        return Err(Error::BadLayer);
    }
    if from == to {
        return Ok(());
    }
    let active_id = doc.active.map(|index| doc.layers[index].id);
    let layer = doc.layers.remove(from);
    doc.layers.insert(to, layer);
    doc.active = active_id.and_then(|id| doc.layers.iter().position(|layer| layer.id == id));
    Ok(())
}

pub fn crop(doc: &mut Document, x: i32, y: i32, width: u32, height: u32) -> Result<(), Error> {
    if width == 0 || height == 0 {
        return Err(Error::EmptyCrop);
    }
    let x0 = x.max(0);
    let y0 = y.max(0);
    let x1 = (x.saturating_add(width as i32)).min(doc.width as i32);
    let y1 = (y.saturating_add(height as i32)).min(doc.height as i32);
    if x1 <= x0 || y1 <= y0 {
        return Err(Error::EmptyCrop);
    }
    for layer in &mut doc.layers {
        let lx = layer.x;
        let ly = layer.y;
        let lw = layer.pixels.width() as i32;
        let lh = layer.pixels.height() as i32;
        let ix0 = lx.max(x0);
        let iy0 = ly.max(y0);
        let ix1 = (lx + lw).min(x1);
        let iy1 = (ly + lh).min(y1);
        if ix1 <= ix0 || iy1 <= iy0 {
            layer.pixels = RgbaImage::new(1, 1);
            layer.x = 0;
            layer.y = 0;
            continue;
        }
        let local_x = (ix0 - lx) as u32;
        let local_y = (iy0 - ly) as u32;
        let cropped = imageops::crop_imm(
            &layer.pixels,
            local_x,
            local_y,
            (ix1 - ix0) as u32,
            (iy1 - iy0) as u32,
        )
        .to_image();
        layer.pixels = cropped;
        layer.x = ix0 - x0;
        layer.y = iy0 - y0;
    }
    doc.width = (x1 - x0) as u32;
    doc.height = (y1 - y0) as u32;
    Ok(())
}

pub fn resize_canvas(
    doc: &mut Document,
    width: u32,
    height: u32,
    anchor: Anchor,
) -> Result<(), Error> {
    if width == 0 || height == 0 {
        return Err(Error::ZeroSize);
    }
    let dx = axis_offset(doc.width, width, anchor.horizontal());
    let dy = axis_offset(doc.height, height, anchor.vertical());
    for layer in &mut doc.layers {
        layer.x += dx;
        layer.y += dy;
    }
    doc.width = width;
    doc.height = height;
    Ok(())
}

#[derive(Clone, Copy)]
enum Align {
    Start,
    Center,
    End,
}

impl Anchor {
    fn horizontal(self) -> Align {
        match self {
            Self::NorthWest | Self::West | Self::SouthWest => Align::Start,
            Self::North | Self::Center | Self::South => Align::Center,
            Self::NorthEast | Self::East | Self::SouthEast => Align::End,
        }
    }

    fn vertical(self) -> Align {
        match self {
            Self::NorthWest | Self::North | Self::NorthEast => Align::Start,
            Self::West | Self::Center | Self::East => Align::Center,
            Self::SouthWest | Self::South | Self::SouthEast => Align::End,
        }
    }
}

fn axis_offset(old: u32, new: u32, align: Align) -> i32 {
    let old = old as i32;
    let new = new as i32;
    match align {
        Align::Start => 0,
        Align::Center => (new - old) / 2,
        Align::End => new - old,
    }
}

pub fn scale_document(
    doc: &mut Document,
    width: u32,
    height: u32,
    filter: FilterType,
) -> Result<(), Error> {
    if width == 0 || height == 0 {
        return Err(Error::ZeroSize);
    }
    let sx = width as f64 / doc.width as f64;
    let sy = height as f64 / doc.height as f64;
    for layer in &mut doc.layers {
        let new_w = ((layer.pixels.width() as f64) * sx).round().max(1.0) as u32;
        let new_h = ((layer.pixels.height() as f64) * sy).round().max(1.0) as u32;
        layer.pixels = imageops::resize(&layer.pixels, new_w, new_h, filter);
        layer.x = (layer.x as f64 * sx).round() as i32;
        layer.y = (layer.y as f64 * sy).round() as i32;
    }
    doc.width = width;
    doc.height = height;
    Ok(())
}

pub fn scale_layer(
    doc: &mut Document,
    index: usize,
    x: i32,
    y: i32,
    width: u32,
    height: u32,
    filter: FilterType,
) -> Result<(), Error> {
    if width == 0 || height == 0 {
        return Err(Error::ZeroSize);
    }
    let layer = doc.layers.get_mut(index).ok_or(Error::BadLayer)?;
    layer.pixels = imageops::resize(&layer.pixels, width, height, filter);
    layer.x = x;
    layer.y = y;
    Ok(())
}

pub fn rotate_canvas(doc: &mut Document, turn: QuarterTurn) {
    let (width, height) = (doc.width, doc.height);
    for layer in &mut doc.layers {
        let (x, y, w, h) = (layer.x, layer.y, layer.width(), layer.height());
        let (pixels, nx, ny) = match turn {
            QuarterTurn::Cw => {
                let pixels = imageops::rotate90(&layer.pixels);
                (pixels, height as i32 - y - h as i32, x)
            }
            QuarterTurn::Ccw => {
                let pixels = imageops::rotate270(&layer.pixels);
                (pixels, y, width as i32 - x - w as i32)
            }
            QuarterTurn::Half => {
                let pixels = imageops::rotate180(&layer.pixels);
                (
                    pixels,
                    width as i32 - x - w as i32,
                    height as i32 - y - h as i32,
                )
            }
        };
        layer.pixels = pixels;
        layer.x = nx;
        layer.y = ny;
    }
    match turn {
        QuarterTurn::Cw | QuarterTurn::Ccw => {
            doc.width = height;
            doc.height = width;
        }
        QuarterTurn::Half => {}
    }
}

pub fn flip_canvas(doc: &mut Document, axis: Axis) {
    let (width, height) = (doc.width, doc.height);
    for layer in &mut doc.layers {
        let (x, y, w, h) = (
            layer.x,
            layer.y,
            layer.width() as i32,
            layer.height() as i32,
        );
        match axis {
            Axis::Horizontal => {
                layer.pixels = imageops::flip_horizontal(&layer.pixels);
                layer.x = width as i32 - x - w;
            }
            Axis::Vertical => {
                layer.pixels = imageops::flip_vertical(&layer.pixels);
                layer.y = height as i32 - y - h;
            }
        }
    }
}

pub fn flip_layer(doc: &mut Document, index: usize, axis: Axis) -> Result<(), Error> {
    let layer = doc.layers.get_mut(index).ok_or(Error::BadLayer)?;
    layer.pixels = match axis {
        Axis::Horizontal => imageops::flip_horizontal(&layer.pixels),
        Axis::Vertical => imageops::flip_vertical(&layer.pixels),
    };
    Ok(())
}

pub fn rotate_layer(doc: &mut Document, index: usize, degrees_cw: f32) -> Result<(), Error> {
    let layer = doc.layers.get_mut(index).ok_or(Error::BadLayer)?;
    if degrees_cw.abs() % 360.0 <= 1e-3 {
        return Ok(());
    }
    let (pixels, x, y) = rotate_bitmap(&layer.pixels, layer.x, layer.y, degrees_cw);
    layer.pixels = pixels;
    layer.x = x;
    layer.y = y;
    Ok(())
}

/// Rotate a layer bitmap clockwise around its center. The returned origin
/// keeps that center in the same place.
pub fn rotate_bitmap(pixels: &RgbaImage, x: i32, y: i32, degrees_cw: f32) -> (RgbaImage, i32, i32) {
    let (w, h) = pixels.dimensions();
    if w == 0 || h == 0 || !degrees_cw.is_finite() {
        return (pixels.clone(), x, y);
    }
    let theta = degrees_cw.to_radians();
    let (nw, nh) = expanded_size(w, h, theta);
    // Bicubic sampling needs a margin. When the rotated box is at least as
    // large as the source, center the source in that box. A wide layer turned
    // sideways makes one side smaller than the source, and `nw - w` would
    // underflow, so that case uses a buffer big enough for both.
    let pad = 2u32;
    let (buffer_w, buffer_h, ox, oy, crop_x, crop_y, crop_w, crop_h) = if nw >= w && nh >= h {
        (
            nw + pad * 2,
            nh + pad * 2,
            (pad + (nw - w) / 2) as i64,
            (pad + (nh - h) / 2) as i64,
            pad,
            pad,
            nw,
            nh,
        )
    } else {
        let buffer_w = nw.max(w).saturating_add(pad * 2);
        let buffer_h = nh.max(h).saturating_add(pad * 2);
        let crop_w = nw.saturating_add(pad * 2).min(buffer_w).max(1);
        let crop_h = nh.saturating_add(pad * 2).min(buffer_h).max(1);
        (
            buffer_w,
            buffer_h,
            ((buffer_w - w) / 2) as i64,
            ((buffer_h - h) / 2) as i64,
            (buffer_w - crop_w) / 2,
            (buffer_h - crop_h) / 2,
            crop_w,
            crop_h,
        )
    };
    if buffer_w.saturating_mul(buffer_h) > 40_000_000
        || crop_x.saturating_add(crop_w) > buffer_w
        || crop_y.saturating_add(crop_h) > buffer_h
    {
        return (pixels.clone(), x, y);
    }
    let mut padded = RgbaImage::new(buffer_w, buffer_h);
    imageops::overlay(&mut padded, pixels, ox, oy);
    let rotated = rotate_about_center(&padded, theta, Interpolation::Bicubic, Rgba([0, 0, 0, 0]));
    let cropped = imageops::crop_imm(&rotated, crop_x, crop_y, crop_w, crop_h).to_image();
    let old_cx = x as f64 + w as f64 / 2.0;
    let old_cy = y as f64 + h as f64 / 2.0;
    let nx = (old_cx - crop_w as f64 / 2.0).round() as i32;
    let ny = (old_cy - crop_h as f64 / 2.0).round() as i32;
    (cropped, nx, ny)
}

fn expanded_size(width: u32, height: u32, theta: f32) -> (u32, u32) {
    let (sin, cos) = theta.sin_cos();
    let (sin, cos) = (sin.abs(), cos.abs());
    if !sin.is_finite() || !cos.is_finite() {
        return (width.max(1), height.max(1));
    }
    let nw = width as f32 * cos + height as f32 * sin;
    let nh = width as f32 * sin + height as f32 * cos;
    (finite_edge(nw, width), finite_edge(nh, height))
}

fn finite_edge(value: f32, fallback: u32) -> u32 {
    if value.is_finite() && value >= 1.0 {
        (value.ceil().min(100_000.0) as u32).max(1)
    } else {
        fallback.max(1)
    }
}
