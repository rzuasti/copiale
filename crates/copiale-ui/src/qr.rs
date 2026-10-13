//! QR codes painted on a canvas.

use gpui_kit::{
    Bounds, IntoElement, ParentElement as _, Styled as _, black, canvas, div, fill, point, px,
    size, white,
};
use qrcode::{Color, QrCode};

/// Light border around the code, in modules, as the QR spec requires.
const QUIET_ZONE: usize = 4;
const MODULE_PX: f32 = 5.0;

/// Encoded QR modules, row by row.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct QrMatrix {
    width: usize,
    dark: Vec<bool>,
}

impl QrMatrix {
    /// `None` if `data` is too long for a QR code.
    pub fn encode(data: &str) -> Option<Self> {
        let code = QrCode::new(data.as_bytes()).ok()?;
        Some(Self {
            width: code.width(),
            dark: code
                .to_colors()
                .into_iter()
                .map(|color| color == Color::Dark)
                .collect(),
        })
    }

    /// Modules per side.
    pub fn width(&self) -> usize {
        self.width
    }

    /// Dark runs in `row` as `(start, len)` module ranges.
    fn dark_runs(&self, row: usize) -> impl Iterator<Item = (usize, usize)> + '_ {
        let cells = &self.dark[row * self.width..(row + 1) * self.width];
        let mut x = 0;
        std::iter::from_fn(move || {
            while x < cells.len() && !cells[x] {
                x += 1;
            }
            let start = x;
            while x < cells.len() && cells[x] {
                x += 1;
            }
            (x > start).then_some((start, x - start))
        })
    }
}

/// Black on white whatever the theme: phone cameras expect it.
pub fn qr_code(matrix: QrMatrix) -> impl IntoElement {
    let side = px((matrix.width + 2 * QUIET_ZONE) as f32 * MODULE_PX);
    div().size(side).flex_none().bg(white()).child(
        canvas(
            |_, _, _| {},
            move |bounds, (), window, _| {
                let module = px(MODULE_PX);
                let origin = bounds.origin + point(module, module) * QUIET_ZONE as f32;
                for row in 0..matrix.width() {
                    for (start, len) in matrix.dark_runs(row) {
                        let corner = origin + point(module * start as f32, module * row as f32);
                        let run = Bounds::new(corner, size(module * len as f32, module));
                        window.paint_quad(fill(run, black()));
                    }
                }
            },
        )
        .size_full(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Shaped like a real provisioning URL.
    const URL: &str = "sgnl://linkdevice?uuid=AbCdEfGhIjKlMnOpQrStUv&pub_key=BQ%2B1YW3pPq0xg9Hk3sR8r5D7fGxW2v1cJ8nLmQpZt4Ue";

    #[test]
    fn encodes_a_provisioning_url() {
        let matrix = QrMatrix::encode(URL).unwrap();
        assert!((21..=177).contains(&matrix.width()));
        assert_eq!(matrix.dark.len(), matrix.width() * matrix.width());
    }

    #[test]
    fn finder_pattern_runs_are_found() {
        let matrix = QrMatrix::encode(URL).unwrap();
        // Top row starts with a 7-module finder pattern and ends with another.
        let runs: Vec<_> = matrix.dark_runs(0).collect();
        assert_eq!(runs.first(), Some(&(0, 7)));
        assert_eq!(runs.last(), Some(&(matrix.width() - 7, 7)));
    }

    #[test]
    fn runs_cover_exactly_the_dark_modules() {
        let matrix = QrMatrix::encode(URL).unwrap();
        for row in 0..matrix.width() {
            let mut cells = vec![false; matrix.width()];
            for (start, len) in matrix.dark_runs(row) {
                cells[start..start + len].fill(true);
            }
            assert_eq!(
                cells,
                matrix.dark[row * matrix.width()..(row + 1) * matrix.width()]
            );
        }
    }
}
