//! Decodes downloaded images for GPUI: JPEG, PNG, WebP and GIF only,
//! recognized by their contents, never what the sender says they are.
//!
//! Others' files are untrusted: sizes and memory are limited, and decoding
//! runs off the main thread.

use std::io::Cursor;

use image::codecs::gif::GifDecoder;
use image::imageops::FilterType;
use image::{
    AnimationDecoder as _, DynamicImage, Frame, ImageDecoder as _, ImageFormat, ImageReader,
    Limits, RgbaImage,
};

/// Widest or tallest image decoded.
const MAX_SIDE: u32 = 16_384;
/// Most memory a decoder may take.
const MAX_ALLOC: u64 = 512 * 1024 * 1024;
/// Most frames of an animation; longer ones show their first.
const MAX_FRAMES: usize = 500;
/// Most pixels of all an animation's frames, scaled; more shows the first.
const MAX_ANIMATION_PIXELS: u64 = 24_000_000;

/// An image scaled to fit, as BGRA frames.
pub struct Decoded {
    pub frames: Vec<Frame>,
    pub width: u32,
    pub height: u32,
}

#[derive(Debug, PartialEq, Eq)]
pub enum DecodeError {
    /// Not an image this decodes.
    Unsupported,
    /// Corrupt, or over the limits.
    Invalid,
}

/// Decodes `bytes`, scaled down to fit `fit` by `fit` pixels, upright.
pub fn decode(bytes: &[u8], fit: u32) -> Result<Decoded, DecodeError> {
    let reader = ImageReader::new(Cursor::new(bytes))
        .with_guessed_format()
        .map_err(|_| DecodeError::Unsupported)?;
    match reader.format() {
        Some(ImageFormat::Gif) => {
            if let Some(animation) = animation(bytes, fit) {
                return Ok(animation);
            }
        }
        Some(ImageFormat::Jpeg | ImageFormat::Png | ImageFormat::WebP) => {}
        _ => return Err(DecodeError::Unsupported),
    }
    still(reader, fit)
}

fn limits() -> Limits {
    let mut limits = Limits::default();
    limits.max_image_width = Some(MAX_SIDE);
    limits.max_image_height = Some(MAX_SIDE);
    limits.max_alloc = Some(MAX_ALLOC);
    limits
}

fn still(mut reader: ImageReader<Cursor<&[u8]>>, fit: u32) -> Result<Decoded, DecodeError> {
    reader.limits(limits());
    let mut decoder = reader.into_decoder().map_err(|_| DecodeError::Invalid)?;
    let orientation = decoder.orientation().map_err(|_| DecodeError::Invalid)?;
    let mut image = DynamicImage::from_decoder(decoder).map_err(|_| DecodeError::Invalid)?;
    image.apply_orientation(orientation);
    let image = scaled(image.into_rgba8(), fit);
    let (width, height) = image.dimensions();
    Ok(Decoded {
        frames: vec![Frame::new(bgra(image))],
        width,
        height,
    })
}

/// A GIF's frames scaled to fit; `None` for a still one, or one too long
/// or large to animate.
fn animation(bytes: &[u8], fit: u32) -> Option<Decoded> {
    let mut decoder = GifDecoder::new(Cursor::new(bytes)).ok()?;
    decoder.set_limits(limits()).ok()?;
    let mut frames = Vec::new();
    let mut pixels = 0u64;
    for frame in decoder.into_frames() {
        let frame = frame.ok()?;
        if frames.len() == MAX_FRAMES {
            return None;
        }
        let (left, top, delay) = (frame.left(), frame.top(), frame.delay());
        let image = scaled(frame.into_buffer(), fit);
        pixels += u64::from(image.width()) * u64::from(image.height());
        if pixels > MAX_ANIMATION_PIXELS {
            return None;
        }
        frames.push(Frame::from_parts(bgra(image), left, top, delay));
    }
    if frames.len() < 2 {
        return None;
    }
    let (width, height) = frames[0].buffer().dimensions();
    Some(Decoded {
        frames,
        width,
        height,
    })
}

/// `image` scaled down to fit `fit` by `fit`, keeping its shape.
fn scaled(image: RgbaImage, fit: u32) -> RgbaImage {
    let (width, height) = image.dimensions();
    if width <= fit && height <= fit {
        return image;
    }
    let scale = f64::from(fit) / f64::from(width.max(height));
    let width = ((f64::from(width) * scale).round() as u32).max(1);
    let height = ((f64::from(height) * scale).round() as u32).max(1);
    image::imageops::resize(&image, width, height, FilterType::Triangle)
}

/// GPUI's pixel order.
fn bgra(mut image: RgbaImage) -> RgbaImage {
    for pixel in image.as_chunks_mut::<4>().0 {
        pixel.swap(0, 2);
    }
    image
}

#[cfg(test)]
mod tests {
    use image::codecs::gif::GifEncoder;
    use image::{Delay, Rgba};

    use super::*;

    fn encoded(image: &RgbaImage, format: ImageFormat) -> Vec<u8> {
        let mut bytes = Vec::new();
        image
            .write_to(&mut Cursor::new(&mut bytes), format)
            .unwrap();
        bytes
    }

    fn red(width: u32, height: u32) -> RgbaImage {
        RgbaImage::from_pixel(width, height, Rgba([255, 0, 0, 255]))
    }

    #[test]
    fn images_are_scaled_to_fit_in_gpui_order() {
        let png = encoded(&red(400, 200), ImageFormat::Png);

        let decoded = decode(&png, 100).unwrap();

        assert_eq!((decoded.width, decoded.height), (100, 50));
        assert_eq!(decoded.frames.len(), 1);
        assert_eq!(
            decoded.frames[0].buffer().get_pixel(0, 0).0,
            [0, 0, 255, 255]
        );
        let small = decode(&png, 1000).unwrap();
        assert_eq!((small.width, small.height), (400, 200));
    }

    #[test]
    fn the_contents_decide_the_format() {
        let png = encoded(&red(10, 10), ImageFormat::Png);
        let bmp = encoded(&red(10, 10), ImageFormat::Bmp);

        assert!(decode(&png, 100).is_ok());
        assert_eq!(decode(&bmp, 100).err(), Some(DecodeError::Unsupported));
        assert_eq!(
            decode(b"%PDF-1.7", 100).err(),
            Some(DecodeError::Unsupported)
        );
        assert_eq!(decode(&png[..20], 100).err(), Some(DecodeError::Invalid));
    }

    #[test]
    fn huge_images_are_refused() {
        // A PNG header claiming 20000 x 20000 pixels.
        let mut png = encoded(&red(1, 1), ImageFormat::Png);
        png[16..20].copy_from_slice(&20_000u32.to_be_bytes());
        png[20..24].copy_from_slice(&20_000u32.to_be_bytes());

        assert_eq!(decode(&png, 100).err(), Some(DecodeError::Invalid));
    }

    fn gif(frames: usize, side: u32) -> Vec<u8> {
        let mut bytes = Vec::new();
        {
            let mut encoder = GifEncoder::new(&mut bytes);
            let frames = (0..frames).map(|ix| {
                let shade = (ix * 40 % 256) as u8;
                let image = RgbaImage::from_pixel(side, side, Rgba([shade, 0, 0, 255]));
                Frame::from_parts(image, 0, 0, Delay::from_numer_denom_ms(100, 1))
            });
            encoder.encode_frames(frames).unwrap();
        }
        bytes
    }

    #[test]
    fn gifs_animate_within_limits() {
        let animated = decode(&gif(3, 20), 10).unwrap();
        assert_eq!(animated.frames.len(), 3);
        assert_eq!((animated.width, animated.height), (10, 10));

        let still = decode(&gif(1, 20), 10).unwrap();
        assert_eq!(still.frames.len(), 1);

        let long = decode(&gif(MAX_FRAMES + 1, 2), 10).unwrap();
        assert_eq!(long.frames.len(), 1, "too long: first frame only");
    }
}
