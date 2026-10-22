//! The emoji picker's grid: recent emoji, then every emoji by category, or
//! the matches for a search.

use std::ops::Range;

use copiale_core::MAX_RECENT_EMOJI;

use super::emoji::{Shortcode, search, table};

/// Emoji per row.
pub const COLUMNS: usize = 9;

/// Recent emoji shown: two rows.
const SHOWN_RECENT: usize = 2 * COLUMNS;

/// Search matches shown.
const MAX_RESULTS: usize = 10 * COLUMNS;

pub const RECENT: &str = "Recently used";

/// Categories in order, with the emoji standing for each; skin tone
/// swatches ("Component") are left out.
pub const CATEGORIES: [(&str, &str); 9] = [
    ("Smileys & Emotion", "😀"),
    ("People & Body", "👋"),
    ("Animals & Nature", "🐻"),
    ("Food & Drink", "🍔"),
    ("Travel & Places", "🚗"),
    ("Activities", "⚽"),
    ("Objects", "💡"),
    ("Symbols", "❤️"),
    ("Flags", "🏁"),
];

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum GridRow {
    Header(&'static str),
    /// Indices into [`EmojiGrid::cells`].
    Cells(Range<usize>),
}

#[derive(Clone, Debug, Default)]
pub struct EmojiGrid {
    cells: Vec<Shortcode>,
    rows: Vec<GridRow>,
    selected: usize,
}

impl EmojiGrid {
    /// Every emoji after the `recent` ones, or the matches for `query`.
    pub fn new(query: &str, recent: &[String]) -> Self {
        let mut grid = Self::default();
        let query = query.trim().to_lowercase();
        if !query.is_empty() {
            grid.section(None, search(&query, MAX_RESULTS));
            return grid;
        }
        let recent = recent
            .iter()
            .filter_map(|emoji| table().iter().find(|entry| entry.emoji == emoji))
            .take(SHOWN_RECENT)
            .copied()
            .collect();
        grid.section(Some(RECENT), recent);
        for (category, _) in CATEGORIES {
            let mut cells: Vec<Shortcode> = Vec::new();
            for entry in table().iter().filter(|entry| entry.category == category) {
                if cells.last().is_none_or(|last| last.emoji != entry.emoji) {
                    cells.push(*entry);
                }
            }
            grid.section(Some(category), cells);
        }
        grid
    }

    fn section(&mut self, header: Option<&'static str>, cells: Vec<Shortcode>) {
        if cells.is_empty() {
            return;
        }
        self.rows.extend(header.map(GridRow::Header));
        let start = self.cells.len();
        self.cells.extend(cells);
        let rows = (start..self.cells.len())
            .step_by(COLUMNS)
            .map(|row| GridRow::Cells(row..(row + COLUMNS).min(self.cells.len())));
        self.rows.extend(rows);
    }

    pub fn cells(&self) -> &[Shortcode] {
        &self.cells
    }

    pub fn rows(&self) -> &[GridRow] {
        &self.rows
    }

    pub fn selected(&self) -> Option<&Shortcode> {
        self.cells.get(self.selected)
    }

    pub fn selected_ix(&self) -> usize {
        self.selected
    }

    /// The row holding the selected emoji.
    pub fn selected_row(&self) -> Option<usize> {
        self.rows
            .iter()
            .position(|row| matches!(row, GridRow::Cells(cells) if cells.contains(&self.selected)))
    }

    pub fn select(&mut self, ix: usize) {
        if ix < self.cells.len() {
            self.selected = ix;
        }
    }

    /// Moves along the emoji, in reading order.
    pub fn step(&mut self, step: isize) {
        if let Some(ix) = self.selected.checked_add_signed(step) {
            self.select(ix);
        }
    }

    /// Moves to the row above or below, keeping the column where it can.
    pub fn step_row(&mut self, step: isize) {
        let Some(row) = self.selected_row() else {
            return;
        };
        let GridRow::Cells(current) = &self.rows[row] else {
            return;
        };
        let column = self.selected - current.start;
        let rows = self.rows.iter();
        let target = if step < 0 {
            rows.take(row).rev().find_map(cells)
        } else {
            rows.skip(row + 1).find_map(cells)
        };
        if let Some(target) = target {
            self.selected = (target.start + column).min(target.end - 1);
        }
    }

    /// Selects the first emoji of `category`; returns its header's row.
    pub fn jump(&mut self, category: &str) -> Option<usize> {
        let row = self
            .rows
            .iter()
            .position(|row| matches!(row, GridRow::Header(header) if *header == category))?;
        if let Some(GridRow::Cells(cells)) = self.rows.get(row + 1) {
            self.selected = cells.start;
        }
        Some(row)
    }
}

fn cells(row: &GridRow) -> Option<&Range<usize>> {
    match row {
        GridRow::Cells(cells) => Some(cells),
        GridRow::Header(_) => None,
    }
}

/// `recent` after using `emoji`: it comes first, once.
pub fn used(recent: &mut Vec<String>, emoji: &str) {
    recent.retain(|seen| seen != emoji);
    recent.insert(0, emoji.to_owned());
    recent.truncate(MAX_RECENT_EMOJI);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn emoji(grid: &EmojiGrid) -> &str {
        grid.selected().unwrap().emoji
    }

    #[test]
    fn categories_follow_recent_emoji() {
        let grid = EmojiGrid::new("", &[]);
        assert_eq!(grid.rows()[0], GridRow::Header(CATEGORIES[0].0));
        assert_eq!(emoji(&grid), "😀");
        let headers: Vec<_> = grid
            .rows()
            .iter()
            .filter_map(|row| match row {
                GridRow::Header(header) => Some(*header),
                GridRow::Cells(_) => None,
            })
            .collect();
        assert_eq!(headers, CATEGORIES.map(|(category, _)| category));
        // One cell per emoji, aliases aside.
        let hearts = grid.cells().iter().filter(|cell| cell.emoji == "❤️");
        assert_eq!(hearts.count(), 1);

        let recent = ["❤️".to_owned(), "not an emoji".into(), "👍".into()];
        let grid = EmojiGrid::new("", &recent);
        assert_eq!(
            grid.rows()[..2],
            [GridRow::Header(RECENT), GridRow::Cells(0..2)]
        );
        assert_eq!(grid.cells()[1].emoji, "👍");
    }

    #[test]
    fn search_lists_matches() {
        let grid = EmojiGrid::new(" Thumb", &["❤️".to_owned()]);
        assert!(matches!(grid.rows(), [GridRow::Cells(_)]));
        assert_eq!(emoji(&grid), "👍");
        assert!(EmojiGrid::new("zzzz", &[]).rows().is_empty());
    }

    #[test]
    fn arrows_move_through_the_grid() {
        let mut grid = EmojiGrid::new("", &[]);
        grid.step(-1);
        assert_eq!(grid.selected_ix(), 0);
        grid.step(2);
        grid.step_row(1);
        assert_eq!(grid.selected_ix(), COLUMNS + 2);
        grid.step_row(-1);
        grid.step_row(-1);
        assert_eq!(grid.selected_ix(), 2);

        // Into the shorter last row of a category, then past a header.
        let people = grid
            .rows()
            .iter()
            .position(|row| *row == GridRow::Header(CATEGORIES[1].0))
            .unwrap();
        let GridRow::Cells(last) = grid.rows()[people - 1].clone() else {
            panic!("expected cells");
        };
        grid.select(last.start - 1);
        grid.step_row(1);
        assert_eq!(grid.selected_ix(), last.end - 1);
        grid.step_row(1);
        assert_eq!(grid.selected_row(), Some(people + 1));

        assert_eq!(grid.jump("Flags"), grid.selected_row().map(|row| row - 1));
        assert_eq!(emoji(&grid), "🏁");
    }

    #[test]
    fn using_an_emoji_puts_it_first() {
        let mut recent = vec!["👍".to_owned(), "❤️".into()];
        used(&mut recent, "❤️");
        used(&mut recent, "🙂");
        assert_eq!(recent, ["🙂", "❤️", "👍"]);
        for ix in 0..MAX_RECENT_EMOJI {
            used(&mut recent, &ix.to_string());
        }
        assert_eq!(recent.len(), MAX_RECENT_EMOJI);
    }
}
