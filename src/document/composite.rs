//! Flatten a document to one RGBA image. Export and the viewport both use this.

use super::{Background, BlendMode, Document, LayerKind};
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
    draw_range(&mut out, doc, 0..doc.layers.len(), overrides);
    out
}

/// Draw the layers in `range`, bottom to top. Each group in it draws its
/// contents. A group at full opacity in Normal mode lets them mix straight
/// into what is below, as Photoshop's pass-through does. Otherwise they are
/// flattened on their own first, and the result mixes in with the group's
/// opacity and blend mode.
fn draw_range(
    out: &mut RgbaImage,
    doc: &Document,
    range: std::ops::Range<usize>,
    overrides: Option<&[LayerOverride]>,
) {
    // Walk down from the top, one block at a time, then draw bottom up.
    let mut items = Vec::new();
    let mut top = range.end;
    while top > range.start {
        let block = doc.block(top - 1);
        let start = block.start.max(range.start);
        items.push((start, top - 1));
        top = start;
    }
    for (start, index) in items.into_iter().rev() {
        let layer = &doc.layers[index];
        let replacement = overrides.and_then(|list| list.iter().find(|item| item.index == index));
        let (x, y, opacity, visible) = replacement
            .map(|item| (item.x, item.y, item.opacity, item.visible))
            .unwrap_or((layer.x, layer.y, layer.opacity, layer.visible));
        if !visible || opacity <= 0.0 {
            continue;
        }
        if let LayerKind::Group { .. } = layer.kind {
            if opacity >= 1.0 && layer.blend == BlendMode::Normal {
                draw_range(out, doc, start..index, overrides);
            } else {
                let mut contents = RgbaImage::new(out.width(), out.height());
                draw_range(&mut contents, doc, start..index, overrides);
                blit(out, &contents, 0, 0, opacity, layer.blend);
            }
            continue;
        }
        let pixels = replacement
            .and_then(|item| item.pixels.as_ref())
            .unwrap_or(&layer.pixels);
        blit(out, pixels, x, y, opacity, layer.blend);
    }
}

/// The listed layers flattened on their own, without the canvas background,
/// and trimmed to the pixels they draw. `None` when they draw nothing on the
/// canvas. Copying a selection uses this.
pub fn composite_layers(doc: &Document, indices: &[usize]) -> Option<RgbaImage> {
    let mut out = RgbaImage::new(doc.width, doc.height);
    let indices = doc.with_contents(indices);
    for (index, layer) in doc.layers.iter().enumerate() {
        if indices.contains(&index) && doc.shown(index) && layer.opacity > 0.0 {
            blit(
                &mut out,
                &layer.pixels,
                layer.x,
                layer.y,
                layer.opacity,
                layer.blend,
            );
        }
    }
    trim(&out)
}

/// `image` cut down to the box around its non-transparent pixels.
fn trim(image: &RgbaImage) -> Option<RgbaImage> {
    let (mut x0, mut y0, mut x1, mut y1) = (u32::MAX, u32::MAX, 0, 0);
    for (x, y, pixel) in image.enumerate_pixels() {
        if pixel[3] > 0 {
            x0 = x0.min(x);
            y0 = y0.min(y);
            x1 = x1.max(x + 1);
            y1 = y1.max(y + 1);
        }
    }
    (x0 < x1).then(|| image::imageops::crop_imm(image, x0, y0, x1 - x0, y1 - y0).to_image())
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

fn blit(
    dst: &mut RgbaImage,
    src: &RgbaImage,
    origin_x: i32,
    origin_y: i32,
    opacity: f32,
    blend: BlendMode,
) {
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
            over(dst_px, *src_px, opacity, blend);
        }
    }
}

/// Source-over with a blend mode. Where the layer covers what is below, its
/// color is the blend of both. Where nothing is below, it is the layer's own.
fn over(dst: &mut Rgba<u8>, src: Rgba<u8>, opacity: f32, blend: BlendMode) {
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
        let mixed = (1.0 - dst_a) * src_c + dst_a * blend.mix(dst_c, src_c);
        let out = (mixed * src_a + dst_c * dst_a * (1.0 - src_a)) / out_a;
        dst[channel] = (out * 255.0).round().clamp(0.0, 255.0) as u8;
    }
    dst[3] = (out_a * 255.0).round().clamp(0.0, 255.0) as u8;
}
