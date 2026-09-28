//! Color adjustments and filters that rewrite a layer's pixels.

use image::{ImageBuffer, Rgba, RgbaImage};

/// A change to every pixel of a layer. Alpha is kept, except that blur and
/// sharpen spread it along with the color.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Adjustment {
    /// Both from -100 to 100.
    BrightnessContrast {
        brightness: f32,
        contrast: f32,
    },
    /// Hue in degrees from -180 to 180. Saturation and lightness from -100
    /// to 100, where -100 saturation is gray.
    HueSaturation {
        hue: f32,
        saturation: f32,
        lightness: f32,
    },
    /// Input values at or below `black` become black and at or above `white`
    /// become white. `gamma` above 1 brightens the midtones.
    Levels {
        black: u8,
        white: u8,
        gamma: f32,
    },
    Grayscale,
    /// A Gaussian blur. `radius` is its standard deviation in pixels.
    Blur {
        radius: f32,
    },
    /// An unsharp mask. `amount` is a percentage, from 0 to 500, and
    /// `radius` is the blur it sharpens against.
    Sharpen {
        amount: f32,
        radius: f32,
    },
}

impl Adjustment {
    /// Whether applying this would leave every pixel as it is.
    pub fn is_identity(self) -> bool {
        match self {
            Self::BrightnessContrast {
                brightness,
                contrast,
            } => brightness == 0.0 && contrast == 0.0,
            Self::HueSaturation {
                hue,
                saturation,
                lightness,
            } => hue == 0.0 && saturation == 0.0 && lightness == 0.0,
            Self::Levels {
                black,
                white,
                gamma,
            } => black == 0 && white == 255 && (gamma - 1.0).abs() < 1e-4,
            Self::Grayscale => false,
            Self::Blur { radius } => radius <= 0.0,
            Self::Sharpen { amount, radius } => amount <= 0.0 || radius <= 0.0,
        }
    }
}

/// `image` with `adjustment` applied.
pub fn adjust(image: &RgbaImage, adjustment: Adjustment) -> RgbaImage {
    match adjustment {
        Adjustment::BrightnessContrast {
            brightness,
            contrast,
        } => {
            // Contrast pivots on mid-gray. At -100 everything is mid-gray, and
            // the factor grows without bound toward +100, so stop short.
            let c = (contrast.clamp(-100.0, 100.0) / 100.0).min(0.99);
            let factor = if c >= 0.0 { 1.0 / (1.0 - c) } else { 1.0 + c };
            let offset = brightness.clamp(-100.0, 100.0) / 100.0 * 0.6;
            map_channels(image, |v| (v - 0.5) * factor + 0.5 + offset)
        }
        Adjustment::HueSaturation {
            hue,
            saturation,
            lightness,
        } => hue_saturation(image, hue, saturation, lightness),
        Adjustment::Levels {
            black,
            white,
            gamma,
        } => {
            let black = black.min(254);
            let white = white.max(black + 1);
            let (black, white) = (black as f32 / 255.0, white as f32 / 255.0);
            let power = 1.0 / gamma.max(0.01);
            map_channels(image, |v| {
                ((v - black) / (white - black)).clamp(0.0, 1.0).powf(power)
            })
        }
        Adjustment::Grayscale => {
            let mut out = image.clone();
            for pixel in out.pixels_mut() {
                let [r, g, b, a] = pixel.0;
                let luma = 0.2126 * r as f32 + 0.7152 * g as f32 + 0.0722 * b as f32;
                let luma = luma.round().clamp(0.0, 255.0) as u8;
                *pixel = Rgba([luma, luma, luma, a]);
            }
            out
        }
        Adjustment::Blur { radius } => {
            if radius <= 0.0 {
                return image.clone();
            }
            unpremultiply(&image::imageops::blur(&premultiply(image), radius))
        }
        Adjustment::Sharpen { amount, radius } => {
            if amount <= 0.0 || radius <= 0.0 {
                return image.clone();
            }
            let original = premultiply(image);
            let blurred = image::imageops::blur(&original, radius);
            let amount = amount / 100.0;
            let mut out = original.clone();
            for (pixel, soft) in out.pixels_mut().zip(blurred.pixels()) {
                let alpha = pixel[3];
                for channel in 0..3 {
                    let sharp = pixel[channel] + amount * (pixel[channel] - soft[channel]);
                    pixel[channel] = sharp.clamp(0.0, alpha);
                }
            }
            unpremultiply(&out)
        }
    }
}

type Premultiplied = ImageBuffer<Rgba<f32>, Vec<f32>>;

/// Color weighted by alpha, so blurring doesn't pull the black of fully
/// transparent pixels into the edges.
fn premultiply(image: &RgbaImage) -> Premultiplied {
    ImageBuffer::from_fn(image.width(), image.height(), |x, y| {
        let [r, g, b, a] = image.get_pixel(x, y).0.map(|v| v as f32 / 255.0);
        Rgba([r * a, g * a, b * a, a])
    })
}

fn unpremultiply(image: &Premultiplied) -> RgbaImage {
    let byte = |v: f32| (v * 255.0).round().clamp(0.0, 255.0) as u8;
    ImageBuffer::from_fn(image.width(), image.height(), |x, y| {
        let [r, g, b, a] = image.get_pixel(x, y).0;
        if a <= 0.0 {
            return Rgba([0, 0, 0, 0]);
        }
        Rgba([byte(r / a), byte(g / a), byte(b / a), byte(a)])
    })
}

/// Run `f` over the red, green, and blue of every pixel, from 0 to 1, through
/// a lookup table.
fn map_channels(image: &RgbaImage, f: impl Fn(f32) -> f32) -> RgbaImage {
    let table: Vec<u8> = (0..=255u8)
        .map(|v| (f(v as f32 / 255.0) * 255.0).round().clamp(0.0, 255.0) as u8)
        .collect();
    let mut out = image.clone();
    for pixel in out.pixels_mut() {
        for channel in 0..3 {
            pixel[channel] = table[pixel[channel] as usize];
        }
    }
    out
}

fn hue_saturation(image: &RgbaImage, hue: f32, saturation: f32, lightness: f32) -> RgbaImage {
    let saturation = 1.0 + saturation.clamp(-100.0, 100.0) / 100.0;
    let lightness = lightness.clamp(-100.0, 100.0) / 100.0;
    let mut out = image.clone();
    for pixel in out.pixels_mut() {
        let [r, g, b, a] = pixel.0;
        let (h, s, l) = to_hsl(r as f32 / 255.0, g as f32 / 255.0, b as f32 / 255.0);
        let h = (h + hue / 360.0).rem_euclid(1.0);
        let s = (s * saturation).clamp(0.0, 1.0);
        let (r, g, b) = from_hsl(h, s, l);
        // Lightness blends toward white or black, like Photoshop's.
        let lift = |v: f32| {
            if lightness >= 0.0 {
                v + (1.0 - v) * lightness
            } else {
                v * (1.0 + lightness)
            }
        };
        let byte = |v: f32| (lift(v) * 255.0).round().clamp(0.0, 255.0) as u8;
        *pixel = Rgba([byte(r), byte(g), byte(b), a]);
    }
    out
}

/// Hue, saturation, and lightness, all from 0 to 1.
fn to_hsl(r: f32, g: f32, b: f32) -> (f32, f32, f32) {
    let max = r.max(g).max(b);
    let min = r.min(g).min(b);
    let l = (max + min) / 2.0;
    let d = max - min;
    if d <= f32::EPSILON {
        return (0.0, 0.0, l);
    }
    let s = d / (1.0 - (2.0 * l - 1.0).abs());
    let h = if max == r {
        ((g - b) / d).rem_euclid(6.0)
    } else if max == g {
        (b - r) / d + 2.0
    } else {
        (r - g) / d + 4.0
    };
    (h / 6.0, s, l)
}

fn from_hsl(h: f32, s: f32, l: f32) -> (f32, f32, f32) {
    let c = (1.0 - (2.0 * l - 1.0).abs()) * s;
    let h6 = h * 6.0;
    let x = c * (1.0 - (h6.rem_euclid(2.0) - 1.0).abs());
    let (r, g, b) = match h6 as u32 {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    let m = l - c / 2.0;
    (r + m, g + m, b + m)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn one(pixel: [u8; 4]) -> RgbaImage {
        RgbaImage::from_pixel(1, 1, Rgba(pixel))
    }

    fn after(pixel: [u8; 4], adjustment: Adjustment) -> [u8; 4] {
        adjust(&one(pixel), adjustment).get_pixel(0, 0).0
    }

    #[test]
    fn identity_settings_change_nothing() {
        let image = RgbaImage::from_fn(4, 3, |x, y| Rgba([x as u8 * 60, y as u8 * 90, 17, 200]));
        for adjustment in [
            Adjustment::BrightnessContrast {
                brightness: 0.0,
                contrast: 0.0,
            },
            Adjustment::HueSaturation {
                hue: 0.0,
                saturation: 0.0,
                lightness: 0.0,
            },
            Adjustment::Levels {
                black: 0,
                white: 255,
                gamma: 1.0,
            },
        ] {
            assert!(adjustment.is_identity());
            assert_eq!(adjust(&image, adjustment), image, "{adjustment:?}");
        }
    }

    #[test]
    fn brightness_and_contrast_move_away_from_or_toward_mid_gray() {
        let brighter = after(
            [100, 100, 100, 255],
            Adjustment::BrightnessContrast {
                brightness: 50.0,
                contrast: 0.0,
            },
        );
        assert!(brighter[0] > 100);
        let flat = after(
            [10, 240, 128, 90],
            Adjustment::BrightnessContrast {
                brightness: 0.0,
                contrast: -100.0,
            },
        );
        assert_eq!(flat, [128, 128, 128, 90], "alpha is kept");
    }

    #[test]
    fn hue_turns_red_to_green_and_no_saturation_is_gray() {
        let green = after(
            [255, 0, 0, 255],
            Adjustment::HueSaturation {
                hue: 120.0,
                saturation: 0.0,
                lightness: 0.0,
            },
        );
        assert_eq!(green, [0, 255, 0, 255]);
        let gray = after(
            [200, 40, 40, 255],
            Adjustment::HueSaturation {
                hue: 0.0,
                saturation: -100.0,
                lightness: 0.0,
            },
        );
        assert_eq!(gray[0], gray[1]);
        assert_eq!(gray[1], gray[2]);
    }

    #[test]
    fn levels_stretch_the_range_between_the_points() {
        let levels = Adjustment::Levels {
            black: 50,
            white: 150,
            gamma: 1.0,
        };
        let [low, middle, high, _] = after([50, 100, 150, 255], levels);
        assert_eq!((low, high), (0, 255));
        assert!(
            middle.abs_diff(128) <= 1,
            "halfway lands mid-gray: {middle}"
        );
        assert_eq!(after([20, 200, 100, 7], levels)[0..2], [0, 255]);
    }

    #[test]
    fn grayscale_uses_luminance() {
        assert_eq!(
            after([0, 255, 0, 40], Adjustment::Grayscale),
            [182, 182, 182, 40]
        );
    }

    #[test]
    fn blur_spreads_color_without_darkening_transparent_edges() {
        let mut image = RgbaImage::new(9, 1);
        image.put_pixel(4, 0, Rgba([255, 255, 255, 255]));
        let blurred = adjust(&image, Adjustment::Blur { radius: 1.5 });
        let edge = blurred.get_pixel(5, 0);
        assert!(edge[3] > 0 && edge[3] < 255, "alpha spreads: {edge:?}");
        assert_eq!(&edge.0[0..3], &[255, 255, 255], "the spread stays white");
    }

    #[test]
    fn sharpen_pushes_an_edge_apart() {
        let image = RgbaImage::from_fn(8, 1, |x, _| {
            let v = if x < 4 { 100 } else { 160 };
            Rgba([v, v, v, 255])
        });
        let sharp = adjust(
            &image,
            Adjustment::Sharpen {
                amount: 200.0,
                radius: 1.0,
            },
        );
        assert!(sharp.get_pixel(3, 0)[0] < 100);
        assert!(sharp.get_pixel(4, 0)[0] > 160);
        assert_eq!(sharp.get_pixel(0, 0)[0], 100, "flat areas stay put");
    }
}
