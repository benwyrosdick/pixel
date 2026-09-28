//! Flatten a document to one RGBA image. Export and the viewport both use this.

use super::{Background, Document};
use image::{Rgba, RgbaImage};

pub fn composite(doc: &Document) -> RgbaImage {
    composite_with(doc, None)
}

/// Like [`composite`], but draws `overrides` in place of the matching layer.
/// The viewport uses this for a move or opacity drag that has not been committed.
pub fn composite_with(doc: &Document, overrides: Option<&[LayerOverride]>) -> RgbaImage {
    let mut out = RgbaImage::new(doc.width, doc.height);
    match doc.background {
        Background::Transparent => {}
        Background::Solid(color) => {
            for pixel in out.pixels_mut() {
                *pixel = Rgba(color);
            }
        }
    }
    for (index, layer) in doc.layers.iter().enumerate() {
        let replacement = overrides.and_then(|list| list.iter().find(|item| item.index == index));
        let (x, y, opacity, visible) = replacement
            .map(|item| (item.x, item.y, item.opacity, item.visible))
            .unwrap_or((layer.x, layer.y, layer.opacity, layer.visible));
        if !visible || opacity <= 0.0 {
            continue;
        }
        let pixels = replacement
            .and_then(|item| item.pixels.as_ref())
            .unwrap_or(&layer.pixels);
        blit_normal(&mut out, pixels, x, y, opacity);
    }
    out
}

pub struct LayerOverride {
    pub index: usize,
    pub x: i32,
    pub y: i32,
    pub opacity: f32,
    pub visible: bool,
    /// When set, these pixels are drawn instead of the layer's stored bitmap.
    pub pixels: Option<RgbaImage>,
}

fn blit_normal(dst: &mut RgbaImage, src: &RgbaImage, origin_x: i32, origin_y: i32, opacity: f32) {
    let (dw, dh) = dst.dimensions();
    let (sw, sh) = src.dimensions();
    for sy in 0..sh {
        let dy = origin_y + sy as i32;
        if dy < 0 || dy >= dh as i32 {
            continue;
        }
        for sx in 0..sw {
            let dx = origin_x + sx as i32;
            if dx < 0 || dx >= dw as i32 {
                continue;
            }
            let src_px = src.get_pixel(sx, sy);
            if src_px[3] == 0 || opacity == 0.0 {
                continue;
            }
            let dst_px = dst.get_pixel_mut(dx as u32, dy as u32);
            over(dst_px, *src_px, opacity);
        }
    }
}

fn over(dst: &mut Rgba<u8>, src: Rgba<u8>, opacity: f32) {
    let src_a = src[3] as f32 / 255.0 * opacity;
    if src_a <= 0.0 {
        return;
    }
    let dst_a = dst[3] as f32 / 255.0;
    let out_a = src_a + dst_a * (1.0 - src_a);
    if out_a <= f32::EPSILON {
        *dst = Rgba([0, 0, 0, 0]);
        return;
    }
    for channel in 0..3 {
        let src_c = src[channel] as f32 / 255.0;
        let dst_c = dst[channel] as f32 / 255.0;
        let out = (src_c * src_a + dst_c * dst_a * (1.0 - src_a)) / out_a;
        dst[channel] = (out * 255.0).round().clamp(0.0, 255.0) as u8;
    }
    dst[3] = (out_a * 255.0).round().clamp(0.0, 255.0) as u8;
}
