//! Decodes blurhashes (blurha.sh): a few colors that sketch an image,
//! shown until it's downloaded.

use std::f32::consts::PI;

const ALPHABET: &[u8] =
    b"0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz#$%*+,-.:;=?@[]^_{|}~";

/// `hash` drawn as `width` by `height` RGBA pixels; `None` if it isn't a
/// valid blurhash.
pub fn decode(hash: &str, width: u32, height: u32) -> Option<Vec<u8>> {
    let digits: Vec<u32> = hash.bytes().map(digit).collect::<Option<_>>()?;
    let size = *digits.first()?;
    let (columns, rows) = (size % 9 + 1, size / 9 + 1);
    if digits.len() != 4 + 2 * (columns * rows) as usize {
        return None;
    }
    let max_ac = (number(&digits[1..2]) + 1) as f32 / 166.;
    let mut colors = vec![dc(number(&digits[2..6]))];
    for pair in digits[6..].chunks(2) {
        colors.push(ac(number(pair), max_ac));
    }
    let mut pixels = Vec::with_capacity((width * height * 4) as usize);
    for y in 0..height {
        for x in 0..width {
            let mut pixel = [0f32; 3];
            for row in 0..rows {
                for column in 0..columns {
                    let basis = (PI * x as f32 * column as f32 / width as f32).cos()
                        * (PI * y as f32 * row as f32 / height as f32).cos();
                    let color = colors[(column + row * columns) as usize];
                    for (channel, value) in pixel.iter_mut().zip(color) {
                        *channel += value * basis;
                    }
                }
            }
            pixels.extend(pixel.map(to_srgb));
            pixels.push(255);
        }
    }
    Some(pixels)
}

fn digit(byte: u8) -> Option<u32> {
    ALPHABET
        .iter()
        .position(|known| *known == byte)
        .map(|ix| ix as u32)
}

fn number(digits: &[u32]) -> u32 {
    digits.iter().fold(0, |sum, digit| sum * 83 + digit)
}

fn dc(value: u32) -> [f32; 3] {
    [value >> 16, (value >> 8) & 255, value & 255].map(|channel| to_linear(channel as u8))
}

fn ac(value: u32, max: f32) -> [f32; 3] {
    [value / (19 * 19), (value / 19) % 19, value % 19].map(|quantized| {
        let centered = (quantized as f32 - 9.) / 9.;
        centered.signum() * centered.abs().powi(2) * max
    })
}

fn to_linear(value: u8) -> f32 {
    let value = f32::from(value) / 255.;
    if value <= 0.04045 {
        value / 12.92
    } else {
        ((value + 0.055) / 1.055).powf(2.4)
    }
}

fn to_srgb(value: f32) -> u8 {
    let value = value.clamp(0., 1.);
    let srgb = if value <= 0.003_130_8 {
        value * 12.92
    } else {
        1.055 * value.powf(1. / 2.4) - 0.055
    };
    (srgb * 255. + 0.5) as u8
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The hash of one flat color.
    fn flat(r: u8, g: u8, b: u8) -> String {
        let encode = |mut value: u32, length: usize| {
            let mut digits = vec![0u8; length];
            for digit in digits.iter_mut().rev() {
                *digit = ALPHABET[(value % 83) as usize];
                value /= 83;
            }
            String::from_utf8(digits).unwrap()
        };
        let color = (u32::from(r) << 16) | (u32::from(g) << 8) | u32::from(b);
        format!("{}{}{}", encode(0, 1), encode(0, 1), encode(color, 4))
    }

    #[test]
    fn a_flat_hash_draws_its_color() {
        let pixels = decode(&flat(200, 100, 50), 4, 3).unwrap();

        assert_eq!(pixels.len(), 4 * 3 * 4);
        for pixel in pixels.chunks(4) {
            assert_eq!(pixel, [200, 100, 50, 255]);
        }
    }

    #[test]
    fn real_hashes_decode() {
        let pixels = decode("LEHV6nWB2yk8pyo0adR*.7kCMdnj", 32, 32).unwrap();
        assert_eq!(pixels.len(), 32 * 32 * 4);
        // Not flat.
        assert_ne!(pixels[..4], pixels[pixels.len() - 4..]);
    }

    #[test]
    fn invalid_hashes_are_refused() {
        for hash in [
            "",
            "L",
            "LEHV6nWB2yk8pyo0adR*.7kCMdn",
            "LEHV6nWB2yk8pyo0adR*.7kCMdnj0",
            "LEHV\u{e9}",
        ] {
            assert_eq!(decode(hash, 4, 4), None, "{hash:?}");
        }
    }
}
