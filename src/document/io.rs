//! Open images, save the layered project, and export a flattened file.

use super::composite::composite;
use super::{Background, BlendMode, Document, Error, Layer};
use image::codecs::jpeg::JpegEncoder;
use image::codecs::webp::WebPEncoder;
use image::{ImageEncoder, ImageReader, RgbaImage};
use serde::{Deserialize, Serialize};
use std::fs::File;
use std::io::{Cursor, Read, Write};
use std::path::Path;

pub const PROJECT_VERSION: u32 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExportFormat {
    Png,
    Jpeg { quality: u8 },
    Webp,
}

pub fn open_image(path: &Path) -> Result<Document, Error> {
    let reader = ImageReader::open(path).map_err(|err| Error::Decode(err.to_string()))?;
    let image = reader
        .decode()
        .map_err(|err| Error::Decode(err.to_string()))?
        .into_rgba8();
    let ppi = png_ppi(path).unwrap_or(72.0);
    Document::from_image(image, ppi)
}

pub fn export(doc: &Document, path: &Path, format: ExportFormat) -> Result<(), Error> {
    let image = composite(doc);
    match format {
        ExportFormat::Png => write_png(&image, doc.ppi, path),
        ExportFormat::Jpeg { quality } => write_jpeg(doc, &image, quality, path),
        ExportFormat::Webp => write_webp(&image, path),
    }
}

pub fn save_project(doc: &Document, path: &Path) -> Result<(), Error> {
    let file = File::create(path).map_err(|err| Error::Write(err.to_string()))?;
    let mut zip = zip::ZipWriter::new(file);
    let json_opts = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated);
    let png_opts =
        zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);

    let manifest = Manifest::from_document(doc);
    zip.start_file("manifest.json", json_opts)
        .map_err(|err| Error::Write(err.to_string()))?;
    let json = serde_json::to_vec_pretty(&manifest).map_err(|err| Error::Write(err.to_string()))?;
    zip.write_all(&json)
        .map_err(|err| Error::Write(err.to_string()))?;

    for (index, layer) in manifest.layers.iter().enumerate() {
        let mut bytes = Cursor::new(Vec::new());
        write_png_to(&doc.layers[index].pixels, doc.ppi, &mut bytes)?;
        zip.start_file(&layer.file, png_opts)
            .map_err(|err| Error::Write(err.to_string()))?;
        zip.write_all(bytes.get_ref())
            .map_err(|err| Error::Write(err.to_string()))?;
    }
    zip.finish().map_err(|err| Error::Write(err.to_string()))?;
    Ok(())
}

pub fn open_project(path: &Path) -> Result<Document, Error> {
    let file = File::open(path).map_err(|err| Error::Decode(err.to_string()))?;
    let mut archive = zip::ZipArchive::new(file).map_err(|_| Error::BadProject)?;
    let manifest: Manifest = {
        let mut entry = archive
            .by_name("manifest.json")
            .map_err(|_| Error::BadProject)?;
        let mut text = String::new();
        entry
            .read_to_string(&mut text)
            .map_err(|err| Error::Decode(err.to_string()))?;
        serde_json::from_str(&text).map_err(|_| Error::BadProject)?
    };
    if manifest.version != PROJECT_VERSION || manifest.layers.is_empty() {
        return Err(Error::BadProject);
    }
    let mut layers = Vec::with_capacity(manifest.layers.len());
    let mut next_id = 1u64;
    for layer in &manifest.layers {
        if !is_layer_file(&layer.file) {
            return Err(Error::BadProject);
        }
        let mut entry = archive
            .by_name(&layer.file)
            .map_err(|_| Error::BadProject)?;
        let mut bytes = Vec::new();
        entry
            .read_to_end(&mut bytes)
            .map_err(|err| Error::Decode(err.to_string()))?;
        let pixels = image::load_from_memory(&bytes)
            .map_err(|err| Error::Decode(err.to_string()))?
            .into_rgba8();
        next_id = next_id.max(layer.id.saturating_add(1));
        layers.push(Layer {
            id: layer.id,
            name: layer.name.clone(),
            visible: layer.visible,
            locked: layer.locked,
            opacity: layer.opacity.clamp(0.0, 1.0),
            blend: layer.blend,
            x: layer.x,
            y: layer.y,
            pixels,
        });
    }
    let active = manifest.active.map(|index| index.min(layers.len() - 1));
    Document::from_parts(
        manifest.width,
        manifest.height,
        manifest.ppi,
        manifest.background.into(),
        layers,
        active,
        next_id,
    )
}

fn is_layer_file(name: &str) -> bool {
    let Some(rest) = name.strip_prefix("layer-") else {
        return false;
    };
    let Some(number) = rest.strip_suffix(".png") else {
        return false;
    };
    !number.is_empty() && number.bytes().all(|byte| byte.is_ascii_digit())
}

#[derive(Serialize, Deserialize)]
struct Manifest {
    version: u32,
    width: u32,
    height: u32,
    ppi: f32,
    background: BackgroundSer,
    /// `null` when no layer was selected.
    active: Option<usize>,
    layers: Vec<LayerSer>,
}

#[derive(Serialize, Deserialize)]
struct LayerSer {
    id: u64,
    name: String,
    visible: bool,
    /// Missing from projects saved before layers could be locked.
    #[serde(default)]
    locked: bool,
    opacity: f32,
    blend: BlendMode,
    x: i32,
    y: i32,
    file: String,
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
enum BackgroundSer {
    Transparent,
    Solid { rgba: [u8; 4] },
}

impl From<BackgroundSer> for Background {
    fn from(value: BackgroundSer) -> Self {
        match value {
            BackgroundSer::Transparent => Background::Transparent,
            BackgroundSer::Solid { rgba } => Background::Solid(rgba),
        }
    }
}

impl Manifest {
    fn from_document(doc: &Document) -> Self {
        Self {
            version: PROJECT_VERSION,
            width: doc.width,
            height: doc.height,
            ppi: doc.ppi,
            background: match doc.background {
                Background::Transparent => BackgroundSer::Transparent,
                Background::Solid(rgba) => BackgroundSer::Solid { rgba },
            },
            active: doc.active_index(),
            layers: doc
                .layers()
                .iter()
                .enumerate()
                .map(|(index, layer)| LayerSer {
                    id: layer.id,
                    name: layer.name.clone(),
                    visible: layer.visible,
                    locked: layer.locked,
                    opacity: layer.opacity,
                    blend: layer.blend,
                    x: layer.x,
                    y: layer.y,
                    file: format!("layer-{index:03}.png"),
                })
                .collect(),
        }
    }
}

fn pixels_per_meter(ppi: f32) -> u32 {
    (ppi as f64 / 0.0254).round().max(1.0) as u32
}

fn png_ppi(path: &Path) -> Option<f32> {
    let file = File::open(path).ok()?;
    let decoder = png::Decoder::new(file);
    let reader = decoder.read_info().ok()?;
    let dims = reader.info().pixel_dims?;
    if dims.unit != png::Unit::Meter || dims.xppu == 0 {
        return None;
    }
    Some(dims.xppu as f32 * 0.0254)
}

fn write_png(image: &RgbaImage, ppi: f32, path: &Path) -> Result<(), Error> {
    let file = File::create(path).map_err(|err| Error::Write(err.to_string()))?;
    write_png_to(image, ppi, file)
}

fn write_png_to(image: &RgbaImage, ppi: f32, writer: impl Write) -> Result<(), Error> {
    let mut encoder = png::Encoder::new(writer, image.width(), image.height());
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    let ppm = pixels_per_meter(ppi);
    encoder.set_pixel_dims(Some(png::PixelDimensions {
        xppu: ppm,
        yppu: ppm,
        unit: png::Unit::Meter,
    }));
    let mut writer = encoder
        .write_header()
        .map_err(|err| Error::Write(err.to_string()))?;
    writer
        .write_image_data(image.as_raw())
        .map_err(|err| Error::Write(err.to_string()))?;
    Ok(())
}

fn write_jpeg(doc: &Document, image: &RgbaImage, quality: u8, path: &Path) -> Result<(), Error> {
    let flat = flatten_for_jpeg(doc.background, image);
    let file = File::create(path).map_err(|err| Error::Write(err.to_string()))?;
    let mut encoder = JpegEncoder::new_with_quality(file, quality.clamp(1, 100));
    let ppi = doc.ppi.round().clamp(1.0, u16::MAX as f32) as u16;
    encoder.set_pixel_density(image::codecs::jpeg::PixelDensity {
        density: (ppi, ppi),
        unit: image::codecs::jpeg::PixelDensityUnit::Inches,
    });
    encoder
        .write_image(
            flat.as_raw(),
            flat.width(),
            flat.height(),
            image::ExtendedColorType::Rgb8,
        )
        .map_err(|err| Error::Write(err.to_string()))?;
    Ok(())
}

/// JPEG has no alpha. Opaque backgrounds are used as-is. A transparent
/// background is flattened onto white.
fn flatten_for_jpeg(background: Background, image: &RgbaImage) -> image::RgbImage {
    let base = match background {
        Background::Solid([r, g, b, a]) if a == 255 => [r, g, b],
        _ => [255, 255, 255],
    };
    let mut out = image::RgbImage::new(image.width(), image.height());
    for (pixel, src) in out.pixels_mut().zip(image.pixels()) {
        let src_a = src[3] as f32 / 255.0;
        let mut rgb = [0u8; 3];
        for channel in 0..3 {
            let blended = src[channel] as f32 * src_a + base[channel] as f32 * (1.0 - src_a);
            rgb[channel] = blended.round().clamp(0.0, 255.0) as u8;
        }
        *pixel = image::Rgb(rgb);
    }
    out
}

fn write_webp(image: &RgbaImage, path: &Path) -> Result<(), Error> {
    let file = File::create(path).map_err(|err| Error::Write(err.to_string()))?;
    let encoder = WebPEncoder::new_lossless(file);
    encoder
        .write_image(
            image.as_raw(),
            image.width(),
            image.height(),
            image::ExtendedColorType::Rgba8,
        )
        .map_err(|err| Error::Write(err.to_string()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::document::{composite, Background, Command, Editor, NewCanvas};
    use image::{GenericImageView, Rgba};

    fn sample_doc() -> Document {
        let mut doc = Document::new(NewCanvas {
            width: 3,
            height: 2,
            ppi: 144.0,
            background: Background::Solid([10, 20, 30, 255]),
        })
        .unwrap();
        doc.layers[0].pixels.put_pixel(1, 0, Rgba([255, 0, 0, 255]));
        doc.layers[0].pixels.put_pixel(2, 1, Rgba([0, 255, 0, 128]));
        doc
    }

    #[test]
    fn png_export_matches_the_composite_and_stores_ppi() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("out.png");
        let doc = sample_doc();
        export(&doc, &path, ExportFormat::Png).unwrap();
        let opened = image::open(&path).unwrap().into_rgba8();
        assert_eq!(opened, composite(&doc));

        let decoder = png::Decoder::new(File::open(&path).unwrap());
        let reader = decoder.read_info().unwrap();
        let dims = reader.info().pixel_dims.unwrap();
        assert_eq!(dims.unit, png::Unit::Meter);
        assert_eq!(dims.xppu, pixels_per_meter(144.0));

        let reopened = open_image(&path).unwrap();
        assert_eq!(reopened.layers()[0].pixels, opened);
        assert!((reopened.ppi - 144.0).abs() < 0.2);
    }

    #[test]
    fn project_roundtrip_keeps_layers_that_hang_off_the_canvas() {
        let mut editor = Editor::new(sample_doc());
        editor
            .apply(Command::MoveLayer {
                index: 0,
                x: -4,
                y: 3,
            })
            .unwrap();
        editor
            .apply(Command::SetOpacity {
                index: 0,
                opacity: 0.4,
            })
            .unwrap();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("doc.pixel");
        save_project(editor.document(), &path).unwrap();
        let opened = open_project(&path).unwrap();
        assert_eq!(opened.width, editor.document().width);
        assert_eq!(opened.background, editor.document().background);
        assert_eq!(opened.layers().len(), 1);
        assert_eq!(opened.layers()[0].x, -4);
        assert_eq!(opened.layers()[0].y, 3);
        assert!((opened.layers()[0].opacity - 0.4).abs() < 1e-5);
        assert_eq!(
            opened.layers()[0].pixels,
            editor.document().layers()[0].pixels
        );
        assert_eq!(composite(&opened), composite(editor.document()));
    }

    #[test]
    fn project_roundtrip_keeps_the_lock_and_reads_older_manifests_as_unlocked() {
        let mut editor = Editor::new(sample_doc());
        editor
            .apply(Command::SetLocked {
                index: 0,
                locked: true,
            })
            .unwrap();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("doc.pixel");
        save_project(editor.document(), &path).unwrap();
        assert!(open_project(&path).unwrap().layers()[0].locked);

        let old: LayerSer = serde_json::from_str(
            r#"{"id":1,"name":"Layer 1","visible":true,"opacity":1.0,"blend":"Normal","x":0,"y":0,"file":"layer-000.png"}"#,
        )
        .unwrap();
        assert!(!old.locked);
    }

    #[test]
    fn project_roundtrip_keeps_no_selection_and_reads_an_older_index() {
        let mut editor = Editor::new(sample_doc());
        editor.deselect();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("doc.pixel");
        save_project(editor.document(), &path).unwrap();
        assert_eq!(open_project(&path).unwrap().active_index(), None);

        let old: Manifest = serde_json::from_str(
            r#"{"version":1,"width":1,"height":1,"ppi":72.0,"background":{"type":"transparent"},"active":0,"layers":[]}"#,
        )
        .unwrap();
        assert_eq!(old.active, Some(0));
    }

    #[test]
    fn webp_export_decodes_to_the_same_pixels() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("out.webp");
        let doc = sample_doc();
        export(&doc, &path, ExportFormat::Webp).unwrap();
        let opened = image::open(&path).unwrap().into_rgba8();
        assert_eq!(opened, composite(&doc));
    }

    #[test]
    fn jpeg_export_has_the_document_size() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("out.jpg");
        let doc = sample_doc();
        export(&doc, &path, ExportFormat::Jpeg { quality: 90 }).unwrap();
        let opened = image::open(&path).unwrap();
        assert_eq!(opened.dimensions(), (doc.width, doc.height));
    }

    #[test]
    fn rejects_a_manifest_that_points_outside_the_archive() {
        assert!(!is_layer_file("../secret.png"));
        assert!(!is_layer_file("layer-../x.png"));
        assert!(is_layer_file("layer-000.png"));
    }
}
