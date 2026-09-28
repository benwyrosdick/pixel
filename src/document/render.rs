//! Draw text and shape layers into pixels, with Pango and Cairo. Neither needs
//! a display, so this runs anywhere the document does.

use image::{Rgba, RgbaImage};
use serde::{Deserialize, Serialize};

/// Editable text, laid out from its top-left corner.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TextSpec {
    /// May run over several lines.
    pub text: String,
    /// A Pango font description without a size, such as "Sans Bold Italic".
    pub font: String,
    /// Height of the font in pixels.
    pub size: f32,
    pub color: [u8; 4],
    pub align: TextAlign,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TextAlign {
    Left,
    Center,
    Right,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ShapeKind {
    Rectangle,
    Ellipse,
    Line,
    Arrow,
}

/// A shape drawn in a box from its top-left corner. `dx` and `dy` run from
/// where a line or arrow starts to where it ends, so they can be negative;
/// the box is their size. Rectangles and ellipses fill the box.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ShapeSpec {
    pub kind: ShapeKind,
    pub dx: f32,
    pub dy: f32,
    pub stroke: [u8; 4],
    pub stroke_width: f32,
    /// Only rectangles and ellipses are filled.
    pub fill: Option<[u8; 4]>,
}

impl ShapeSpec {
    pub fn width(&self) -> f32 {
        self.dx.abs()
    }

    pub fn height(&self) -> f32 {
        self.dy.abs()
    }

    fn arrowhead(&self) -> f32 {
        (self.stroke_width * 3.0).max(10.0)
    }
}

/// Pixels drawn for a text or shape, and where its own top-left corner sits
/// in them. The pixels reach past that corner for strokes, arrowheads, and
/// letters that overhang.
#[derive(Debug, Clone, PartialEq)]
pub struct Rendered {
    pub pixels: RgbaImage,
    pub origin: (i32, i32),
}

/// Room left around what is drawn, so antialiased edges aren't cut off.
const MARGIN: f64 = 2.0;

pub fn render_text(spec: &TextSpec) -> Rendered {
    let measure = surface(1, 1);
    let context = cairo::Context::new(&measure).expect("a cairo context on a fresh surface");
    let layout = pangocairo::functions::create_layout(&context);
    let mut font = pango::FontDescription::from_string(&spec.font);
    font.set_absolute_size(spec.size.max(1.0) as f64 * pango::SCALE as f64);
    layout.set_font_description(Some(&font));
    layout.set_text(&spec.text);
    layout.set_alignment(match spec.align {
        TextAlign::Left => pango::Alignment::Left,
        TextAlign::Center => pango::Alignment::Center,
        TextAlign::Right => pango::Alignment::Right,
    });
    let (ink, logical) = layout.pixel_extents();
    let left = ink.x().min(logical.x()) as f64;
    let top = ink.y().min(logical.y()) as f64;
    let right = (ink.x() + ink.width()).max(logical.x() + logical.width()) as f64;
    let bottom = (ink.y() + ink.height()).max(logical.y() + logical.height()) as f64;
    let width = (right - left + 2.0 * MARGIN).ceil().max(1.0) as i32;
    let height = (bottom - top + 2.0 * MARGIN).ceil().max(1.0) as i32;

    let target = surface(width, height);
    {
        let cr = cairo::Context::new(&target).expect("a cairo context on a fresh surface");
        set_color(&cr, spec.color);
        cr.move_to(MARGIN - left, MARGIN - top);
        pangocairo::functions::update_layout(&cr, &layout);
        pangocairo::functions::show_layout(&cr, &layout);
    }
    Rendered {
        pixels: to_image(target),
        origin: ((MARGIN - left) as i32, (MARGIN - top) as i32),
    }
}

pub fn render_shape(spec: &ShapeSpec) -> Rendered {
    let (w, h) = (spec.width() as f64, spec.height() as f64);
    let stroke = spec.stroke_width.max(0.0) as f64;
    let head = match spec.kind {
        ShapeKind::Arrow => spec.arrowhead() as f64,
        _ => 0.0,
    };
    let pad = (stroke / 2.0 + head + MARGIN).ceil();
    let width = (w + 2.0 * pad).ceil().max(1.0) as i32;
    let height = (h + 2.0 * pad).ceil().max(1.0) as i32;
    let target = surface(width, height);
    {
        let cr = cairo::Context::new(&target).expect("a cairo context on a fresh surface");
        cr.translate(pad, pad);
        cr.set_line_width(stroke);
        cr.set_line_cap(cairo::LineCap::Round);
        cr.set_line_join(cairo::LineJoin::Round);
        match spec.kind {
            ShapeKind::Rectangle | ShapeKind::Ellipse => {
                if spec.kind == ShapeKind::Rectangle {
                    cr.rectangle(0.0, 0.0, w, h);
                } else if w > 0.0 && h > 0.0 {
                    cr.save().ok();
                    cr.translate(w / 2.0, h / 2.0);
                    cr.scale(w / 2.0, h / 2.0);
                    cr.arc(0.0, 0.0, 1.0, 0.0, std::f64::consts::TAU);
                    cr.restore().ok();
                }
                if let Some(fill) = spec.fill {
                    set_color(&cr, fill);
                    cr.fill_preserve().ok();
                }
                if stroke > 0.0 {
                    set_color(&cr, spec.stroke);
                    cr.stroke().ok();
                }
                cr.new_path();
            }
            ShapeKind::Line | ShapeKind::Arrow => {
                let start = (
                    if spec.dx < 0.0 { w } else { 0.0 },
                    if spec.dy < 0.0 { h } else { 0.0 },
                );
                let end = (start.0 + spec.dx as f64, start.1 + spec.dy as f64);
                let length = (spec.dx as f64).hypot(spec.dy as f64);
                set_color(&cr, spec.stroke);
                if spec.kind == ShapeKind::Arrow && length > 0.0 {
                    let (ux, uy) = (spec.dx as f64 / length, spec.dy as f64 / length);
                    // Stop the shaft inside the head so its round cap can't
                    // poke out past the tip.
                    let shaft = (length - head * 0.8).max(0.0);
                    cr.move_to(start.0, start.1);
                    cr.line_to(start.0 + ux * shaft, start.1 + uy * shaft);
                    cr.stroke().ok();
                    let base = (end.0 - ux * head, end.1 - uy * head);
                    let spread = head * 0.55;
                    cr.move_to(end.0, end.1);
                    cr.line_to(base.0 - uy * spread, base.1 + ux * spread);
                    cr.line_to(base.0 + uy * spread, base.1 - ux * spread);
                    cr.close_path();
                    cr.fill().ok();
                } else {
                    cr.move_to(start.0, start.1);
                    cr.line_to(end.0, end.1);
                    cr.stroke().ok();
                }
            }
        }
    }
    Rendered {
        pixels: to_image(target),
        origin: (pad as i32, pad as i32),
    }
}

fn surface(width: i32, height: i32) -> cairo::ImageSurface {
    cairo::ImageSurface::create(cairo::Format::ARgb32, width, height)
        .expect("cairo can make an image surface of this size")
}

fn set_color(cr: &cairo::Context, [r, g, b, a]: [u8; 4]) {
    let unit = |v: u8| v as f64 / 255.0;
    cr.set_source_rgba(unit(r), unit(g), unit(b), unit(a));
}

/// Cairo keeps premultiplied pixels in native-endian 32-bit words, which is
/// blue, green, red, alpha in memory on little-endian machines. Pixel only
/// builds for those.
fn to_image(mut surface: cairo::ImageSurface) -> RgbaImage {
    surface.flush();
    let (width, height) = (surface.width() as u32, surface.height() as u32);
    let stride = surface.stride() as usize;
    let data = surface.data().expect("nothing else holds the surface");
    RgbaImage::from_fn(width, height, |x, y| {
        let at = y as usize * stride + x as usize * 4;
        let (b, g, r, a) = (data[at], data[at + 1], data[at + 2], data[at + 3]);
        if a == 0 {
            return Rgba([0, 0, 0, 0]);
        }
        let straight = |v: u8| ((v as u32 * 255 + a as u32 / 2) / a as u32).min(255) as u8;
        Rgba([straight(r), straight(g), straight(b), a])
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(text: &str, size: f32) -> TextSpec {
        TextSpec {
            text: text.into(),
            font: "Sans".into(),
            size,
            color: [255, 0, 0, 255],
            align: TextAlign::Left,
        }
    }

    #[test]
    fn text_draws_in_its_color_and_grows_with_its_size() {
        let small = render_text(&text("Hi", 20.0));
        let big = render_text(&text("Hi", 40.0));
        assert!(big.pixels.width() > small.pixels.width());
        assert!(big.pixels.height() > small.pixels.height());
        let solid = small.pixels.pixels().find(|pixel| pixel[3] == 255);
        assert_eq!(solid.map(|pixel| pixel.0), Some([255, 0, 0, 255]));
        let two_lines = render_text(&text("Hi\nthere", 20.0));
        assert!(two_lines.pixels.height() > small.pixels.height() * 3 / 2);
    }

    /// Writes samples to `PIXEL_RENDER_SAMPLES` when set, for eyeballing.
    #[test]
    fn write_samples_when_asked() {
        let Ok(dir) = std::env::var("PIXEL_RENDER_SAMPLES") else {
            return;
        };
        let red = [229, 72, 77, 255];
        let samples = [
            (
                "text",
                render_text(&TextSpec {
                    text: "Label here\nsecond line".into(),
                    font: "Sans Bold".into(),
                    size: 32.0,
                    color: red,
                    align: TextAlign::Center,
                }),
            ),
            (
                "arrow",
                render_shape(&ShapeSpec {
                    kind: ShapeKind::Arrow,
                    dx: 160.0,
                    dy: -60.0,
                    stroke: red,
                    stroke_width: 5.0,
                    fill: None,
                }),
            ),
            (
                "ellipse",
                render_shape(&ShapeSpec {
                    kind: ShapeKind::Ellipse,
                    dx: 120.0,
                    dy: 70.0,
                    stroke: red,
                    stroke_width: 4.0,
                    fill: Some([229, 72, 77, 60]),
                }),
            ),
        ];
        for (name, rendered) in samples {
            rendered.pixels.save(format!("{dir}/{name}.png")).unwrap();
        }
    }

    #[test]
    fn a_rectangle_fills_its_box_inside_the_stroke() {
        let rendered = render_shape(&ShapeSpec {
            kind: ShapeKind::Rectangle,
            dx: 20.0,
            dy: 10.0,
            stroke: [0, 0, 255, 255],
            stroke_width: 2.0,
            fill: Some([0, 255, 0, 255]),
        });
        let (ox, oy) = rendered.origin;
        let at = |x: i32, y: i32| {
            rendered
                .pixels
                .get_pixel((ox + x) as u32, (oy + y) as u32)
                .0
        };
        assert_eq!(at(10, 5), [0, 255, 0, 255], "the middle is filled");
        assert_eq!(at(0, 5), [0, 0, 255, 255], "the edge is stroked");
        assert_eq!(rendered.pixels.width() as i32, 20 + 2 * ox);
    }

    #[test]
    fn an_arrow_points_where_it_ends() {
        let arrow = ShapeSpec {
            kind: ShapeKind::Arrow,
            dx: -60.0,
            dy: 0.0,
            stroke: [0, 0, 0, 255],
            stroke_width: 4.0,
            fill: None,
        };
        let rendered = render_shape(&arrow);
        let (ox, oy) = rendered.origin;
        // It ends at the left. Near the base of its head, it is far wider than
        // the shaft.
        let column = |x: i32| {
            (0..rendered.pixels.height())
                .filter(|&y| rendered.pixels.get_pixel((ox + x) as u32, y)[3] > 128)
                .count()
        };
        assert!(
            column(10) > column(40) * 2,
            "{} vs {}",
            column(10),
            column(40)
        );
        assert!(rendered.pixels.get_pixel(ox as u32 + 40, oy as u32)[3] > 128);
    }
}
