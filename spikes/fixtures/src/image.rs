use crate::color::Rgb;
use crate::model::ImageAttachment;
use crate::rng::Rng;

/// Aspect ratios (width, height) with weights; includes extremes to test
/// layout.
const ASPECT_RATIOS: &[((u32, u32), u32)] = &[
    ((1, 1), 3),
    ((4, 3), 4),
    ((3, 4), 4),
    ((3, 2), 3),
    ((2, 3), 3),
    ((16, 9), 3),
    ((9, 16), 3),
    ((3, 1), 1),
    ((1, 3), 1),
];

impl ImageAttachment {
    pub(crate) fn random(rng: &mut Rng) -> Self {
        let (ratio_w, ratio_h) = *rng.weighted(ASPECT_RATIOS);
        let long_side = rng.range(800, 2401) as u32;
        let (width, height) = if ratio_w >= ratio_h {
            (long_side, long_side * ratio_h / ratio_w)
        } else {
            (long_side * ratio_w / ratio_h, long_side)
        };
        Self {
            width,
            height,
            seed: rng.next_u64(),
        }
    }

    /// Average color, for a placeholder shown while the image decodes.
    pub fn placeholder_color(&self) -> Rgb {
        let (start, end) = self.gradient();
        start.lerp(end, 0.5)
    }

    /// Encodes the image as an RGB PNG. Expensive for large images: call off
    /// the UI thread.
    pub fn encode_png(&self) -> Vec<u8> {
        let (start, end) = self.gradient();
        let stripe = (self.width.max(self.height) / 12).max(1);
        let mut pixels = Vec::with_capacity(self.width as usize * self.height as usize * 3);
        for y in 0..self.height {
            for x in 0..self.width {
                let t = (x as f32 / self.width as f32 + y as f32 / self.height as f32) / 2.0;
                let mut color = start.lerp(end, t);
                if ((x + y) / stripe).is_multiple_of(2) {
                    color = color.lerp(Rgb::new(0, 0, 0), 0.08);
                }
                pixels.extend([color.r, color.g, color.b]);
            }
        }

        let mut png = Vec::new();
        let mut encoder = png::Encoder::new(&mut png, self.width, self.height);
        encoder.set_color(png::ColorType::Rgb);
        encoder.set_depth(png::BitDepth::Eight);
        let mut writer = encoder
            .write_header()
            .expect("writing to a Vec cannot fail");
        writer
            .write_image_data(&pixels)
            .expect("pixel buffer matches the header");
        writer.finish().expect("writing to a Vec cannot fail");
        png
    }

    fn gradient(&self) -> (Rgb, Rgb) {
        let mut rng = Rng::new(self.seed);
        let hue = rng.range(0, 360) as f32;
        let shift = rng.range(40, 140) as f32;
        (
            Rgb::from_hsv(hue, 0.55, 0.9),
            Rgb::from_hsv(hue + shift, 0.65, 0.6),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn png_has_declared_dimensions() {
        let image = ImageAttachment {
            width: 120,
            height: 40,
            seed: 7,
        };
        let bytes = image.encode_png();
        let decoder = png::Decoder::new(std::io::Cursor::new(bytes));
        let reader = decoder.read_info().unwrap();
        assert_eq!(reader.info().width, 120);
        assert_eq!(reader.info().height, 40);
    }

    #[test]
    fn random_respects_bounds() {
        let mut rng = Rng::new(1);
        for _ in 0..1000 {
            let image = ImageAttachment::random(&mut rng);
            assert!((800..=2400).contains(&image.width.max(image.height)));
            assert!(image.width.min(image.height) >= 800 / 3);
        }
    }
}
