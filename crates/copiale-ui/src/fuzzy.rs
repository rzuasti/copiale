//! Fuzzy matching for pickers: the query's characters in order, anywhere in
//! the text, ignoring case and the query's spaces.

use std::ops::Range;

/// How `query` matched a text.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Match {
    /// Higher is better: word starts and runs score, gaps cost.
    pub score: i32,
    /// Byte ranges of the matched characters, merged where adjacent.
    pub ranges: Vec<Range<usize>>,
}

const MATCHED: i32 = 16;
const WORD_START: i32 = 24;
const CONSECUTIVE: i32 = 16;
/// Most a gap, or a late first match, costs.
const MAX_GAP: i32 = 10;

/// How well `query` matches `text`; `None` if it doesn't. An empty query
/// matches everything equally.
pub fn fuzzy_match(query: &str, text: &str) -> Option<Match> {
    let query: Vec<char> = query
        .chars()
        .filter(|c| !c.is_whitespace())
        .map(fold)
        .collect();
    if query.is_empty() {
        return Some(Match {
            score: 0,
            ranges: Vec::new(),
        });
    }
    let chars: Vec<(usize, char)> = text.char_indices().collect();
    let folded: Vec<char> = chars.iter().map(|(_, c)| fold(*c)).collect();
    let starts: Vec<bool> = (0..chars.len())
        .map(|j| j == 0 || !chars[j - 1].1.is_alphanumeric())
        .collect();

    // best[i][j]: best score with query[i] at text[j], and where query[i - 1] was.
    let mut best: Vec<Vec<Option<(i32, usize)>>> = vec![vec![None; chars.len()]; query.len()];
    for (i, &wanted) in query.iter().enumerate() {
        for j in 0..chars.len() {
            if folded[j] != wanted {
                continue;
            }
            let own = MATCHED + if starts[j] { WORD_START } else { 0 };
            best[i][j] = if i == 0 {
                Some((own - gap(j), 0))
            } else {
                (0..j)
                    .filter_map(|k| {
                        let (score, _) = best[i - 1][k]?;
                        let link = if k + 1 == j {
                            CONSECUTIVE
                        } else {
                            -gap(j - k - 1)
                        };
                        Some((score + own + link, k))
                    })
                    .max_by_key(|(score, k)| (*score, std::cmp::Reverse(*k)))
            };
        }
    }

    let last = query.len() - 1;
    let (mut j, score) = (0..chars.len())
        .filter_map(|j| Some((j, best[last][j]?.0)))
        .max_by_key(|(j, score)| (*score, std::cmp::Reverse(*j)))?;
    let mut positions = vec![j; query.len()];
    for i in (1..query.len()).rev() {
        j = best[i][j].map(|(_, k)| k).unwrap_or_default();
        positions[i - 1] = j;
    }

    let mut ranges: Vec<Range<usize>> = Vec::new();
    for j in positions {
        let (start, c) = chars[j];
        let range = start..start + c.len_utf8();
        match ranges.last_mut() {
            Some(last) if last.end == range.start => last.end = range.end,
            _ => ranges.push(range),
        }
    }
    Some(Match { score, ranges })
}

fn gap(len: usize) -> i32 {
    i32::try_from(len).unwrap_or(MAX_GAP).min(MAX_GAP)
}

/// Lowercase, for comparing.
fn fold(c: char) -> char {
    c.to_lowercase().next().unwrap_or(c)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn score(query: &str, text: &str) -> Option<i32> {
        fuzzy_match(query, text).map(|m| m.score)
    }

    fn ranges(query: &str, text: &str) -> Vec<Range<usize>> {
        fuzzy_match(query, text).unwrap().ranges
    }

    #[test]
    fn matches_characters_in_order_ignoring_case_and_spaces() {
        assert!(score("bb", "Bob Builder").is_some());
        assert!(score("BOB B", "bob builder").is_some());
        assert!(score("bdb", "Bob Builder").is_none());
        assert!(score("x", "Bob").is_none());
        assert_eq!(score("", "Bob"), Some(0));
    }

    #[test]
    fn highlights_the_best_placed_characters() {
        // Word starts beat the first occurrence.
        assert_eq!(ranges("bb", "Bob Builder"), [0..1, 4..5]);
        assert_eq!(ranges("bui", "Bob Builder"), vec![4..7]);
        assert_eq!(ranges("ång", "Ångström"), vec![0..4]);
    }

    #[test]
    fn prefers_word_starts_runs_and_early_matches() {
        fn rank<'a>(query: &str, texts: &[&'a str]) -> Vec<&'a str> {
            let mut ranked: Vec<_> = texts
                .iter()
                .filter_map(|text| Some((score(query, text)?, *text)))
                .collect();
            ranked.sort_by_key(|(score, _)| -score);
            ranked.into_iter().map(|(_, text)| text).collect()
        }
        assert_eq!(
            rank("bob", &["Rob Boss", "Jacob Obrien", "Bobby Tables"]),
            ["Bobby Tables", "Jacob Obrien"]
        );
        assert_eq!(rank("al", &["Hal", "Alice"]), ["Alice", "Hal"]);
    }
}
