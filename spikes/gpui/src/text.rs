//! Plain message text to Markdown for `TextView`, which only renders
//! Markdown or HTML.

/// Escapes `body` so it renders verbatim, keeps line breaks, and turns
/// `http(s)://` URLs into links.
pub fn message_markdown(body: &str) -> String {
    let lines: Vec<&str> = body.split('\n').collect();
    let mut out = String::with_capacity(body.len() + body.len() / 4);
    for (ix, line) in lines.iter().enumerate() {
        push_line(&mut out, line);
        if let Some(next) = lines.get(ix + 1) {
            // A trailing backslash is a hard break; blank lines stay paragraph breaks.
            if !line.trim().is_empty() && !next.trim().is_empty() {
                out.push('\\');
            }
            out.push('\n');
        }
    }
    out
}

fn push_line(out: &mut String, line: &str) {
    let mut rest = line;
    // Leading spaces would start an indented code block.
    let indent = rest.len() - rest.trim_start_matches(' ').len();
    for _ in 0..indent {
        out.push('\u{a0}');
    }
    rest = &rest[indent..];

    while let Some(start) = find_url(rest) {
        push_escaped(out, &rest[..start]);
        let url = url_at(&rest[start..]);
        out.push('<');
        out.push_str(url);
        out.push('>');
        rest = &rest[start + url.len()..];
    }
    push_escaped(out, rest);
}

fn find_url(text: &str) -> Option<usize> {
    let http = text.find("http://");
    let https = text.find("https://");
    match (http, https) {
        (Some(a), Some(b)) => Some(a.min(b)),
        (a, b) => a.or(b),
    }
}

/// The URL at the start of `text`, without trailing sentence punctuation.
fn url_at(text: &str) -> &str {
    let end = text
        .find(|c: char| c.is_whitespace() || c == '<' || c == '>')
        .unwrap_or(text.len());
    text[..end].trim_end_matches(['.', ',', '!', '?', ';', ':', ')', '\'', '"'])
}

fn push_escaped(out: &mut String, text: &str) {
    for c in text.chars() {
        if c.is_ascii_punctuation() {
            out.push('\\');
        }
        out.push(c);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escapes_markdown_syntax() {
        assert_eq!(message_markdown("*hi* #1"), "\\*hi\\* \\#1");
    }

    #[test]
    fn keeps_line_and_paragraph_breaks() {
        assert_eq!(message_markdown("a\nb\n\nc"), "a\\\nb\n\nc");
    }

    #[test]
    fn links_urls() {
        assert_eq!(
            message_markdown("see https://x.org/a_b. ok"),
            "see <https://x.org/a_b>\\. ok"
        );
    }
}
