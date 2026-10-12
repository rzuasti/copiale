//! Link detection in plain message text.

use std::ops::Range;

/// Byte ranges of `http(s)://` URLs, without trailing sentence punctuation.
pub fn link_ranges(text: &str) -> Vec<Range<usize>> {
    let mut ranges = Vec::new();
    let mut offset = 0;
    while let Some(start) = find_url(&text[offset..]) {
        let start = offset + start;
        let len = url_at(&text[start..]).len();
        ranges.push(start..start + len);
        offset = start + len.max(1);
    }
    ranges
}

fn find_url(text: &str) -> Option<usize> {
    let http = text.find("http://");
    let https = text.find("https://");
    match (http, https) {
        (Some(a), Some(b)) => Some(a.min(b)),
        (a, b) => a.or(b),
    }
}

fn url_at(text: &str) -> &str {
    let end = text
        .find(|c: char| c.is_whitespace() || c == '<' || c == '>')
        .unwrap_or(text.len());
    text[..end].trim_end_matches(['.', ',', '!', '?', ';', ':', ')', '\'', '"'])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_links() {
        let text = "see https://x.org/a. and http://y.io";
        let links: Vec<&str> = link_ranges(text).into_iter().map(|r| &text[r]).collect();
        assert_eq!(links, ["https://x.org/a", "http://y.io"]);
    }

    #[test]
    fn no_links() {
        assert!(link_ranges("plain text").is_empty());
    }
}
