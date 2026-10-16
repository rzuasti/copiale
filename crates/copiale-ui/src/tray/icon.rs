//! The placeholder tray icon, drawn at any size: a white `C` on a blue
//! rounded square, with an orange dot for unread messages.

const BACKGROUND: [u8; 3] = [0x3d, 0x6f, 0xd6];
const GLYPH: [u8; 3] = [0xff, 0xff, 0xff];
const DOT: [u8; 3] = [0xf2, 0x8c, 0x28];

/// Samples per pixel side, for antialiasing.
const SAMPLES: u32 = 4;

/// Straight-alpha RGBA pixels, row by row.
pub fn rgba(size: u32, unread: bool) -> Vec<u8> {
    let mut pixels = Vec::with_capacity((size * size * 4) as usize);
    for y in 0..size {
        for x in 0..size {
            pixels.extend(pixel(x, y, size, unread));
        }
    }
    pixels
}

fn pixel(x: u32, y: u32, size: u32, unread: bool) -> [u8; 4] {
    // Each layer's coverage, averaged over the samples.
    let (mut background, mut glyph, mut dot, mut cutout) = (0., 0., 0., 0.);
    for sy in 0..SAMPLES {
        for sx in 0..SAMPLES {
            // In 0..1 across the icon.
            let u = (x as f32 + (sx as f32 + 0.5) / SAMPLES as f32) / size as f32;
            let v = (y as f32 + (sy as f32 + 0.5) / SAMPLES as f32) / size as f32;
            background += f32::from(in_rounded_square(u, v));
            glyph += f32::from(in_c(u, v));
            if unread {
                dot += f32::from(distance(u, v, 0.8, 0.2) <= 0.18);
                cutout += f32::from(distance(u, v, 0.8, 0.2) <= 0.25);
            }
        }
    }
    let samples = (SAMPLES * SAMPLES) as f32;
    let (background, glyph, dot, cutout) = (
        background / samples,
        glyph / samples,
        dot / samples,
        cutout / samples,
    );
    // The dot sits in a transparent ring cut from the square.
    let square = background * (1. - cutout);
    let alpha = (square + dot).min(1.);
    if alpha == 0. {
        return [0; 4];
    }
    let color: [f32; 3] = std::array::from_fn(|ix| {
        let base = f32::from(BACKGROUND[ix]) * (1. - glyph) + f32::from(GLYPH[ix]) * glyph;
        (base * square + f32::from(DOT[ix]) * dot) / (square + dot)
    });
    let [r, g, b] = color.map(|channel| channel.round().clamp(0., 255.) as u8);
    [r, g, b, (alpha * 255.).round() as u8]
}

fn in_rounded_square(u: f32, v: f32) -> bool {
    const RADIUS: f32 = 0.22;
    let dx = (u - 0.5).abs() - (0.5 - RADIUS);
    let dy = (v - 0.5).abs() - (0.5 - RADIUS);
    dx.max(0.).hypot(dy.max(0.)) <= RADIUS
}

/// A ring open to the right.
fn in_c(u: f32, v: f32) -> bool {
    let r = distance(u, v, 0.5, 0.5);
    let opening = (v - 0.5).abs() < 0.12 && u > 0.5;
    (0.18..=0.3).contains(&r) && !opening
}

fn distance(u: f32, v: f32, cu: f32, cv: f32) -> f32 {
    (u - cu).hypot(v - cv)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(pixels: &[u8], size: u32, x: u32, y: u32) -> [u8; 4] {
        let ix = ((y * size + x) * 4) as usize;
        pixels[ix..ix + 4].try_into().unwrap()
    }

    #[test]
    fn draws_a_c_on_a_rounded_square() {
        let size = 32;
        let pixels = rgba(size, false);
        assert_eq!(pixels.len(), 32 * 32 * 4);
        assert_eq!(at(&pixels, size, 0, 0)[3], 0, "rounded corner");
        assert_eq!(at(&pixels, size, 16, 3), [0x3d, 0x6f, 0xd6, 0xff]);
        assert_eq!(at(&pixels, size, 16, 16), [0x3d, 0x6f, 0xd6, 0xff], "hole");
        assert_eq!(at(&pixels, size, 8, 16), [0xff, 0xff, 0xff, 0xff], "ring");
        assert_eq!(
            at(&pixels, size, 25, 16),
            [0x3d, 0x6f, 0xd6, 0xff],
            "opening"
        );
    }

    #[test]
    fn unread_adds_a_dot() {
        let size = 32;
        let (read, unread) = (rgba(size, false), rgba(size, true));
        assert_eq!(at(&unread, size, 25, 6), [0xf2, 0x8c, 0x28, 0xff]);
        assert_ne!(at(&read, size, 25, 6), at(&unread, size, 25, 6));
        // Away from the dot, unchanged.
        assert_eq!(at(&read, size, 6, 26), at(&unread, size, 6, 26));
    }
}
