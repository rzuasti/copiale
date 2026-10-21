//! Plain message text to Markdown for `TextView`, which only renders
//! Markdown or HTML.

use copiale_core::text::hidden;
use url::Url;

/// Escapes `body` so it renders verbatim, keeps line breaks, and turns
/// URLs into links (see [`link_target`]).
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
        let text = url_at(&rest[start..]);
        match link_target(text) {
            // Autolinks need a scheme, so the target is spelled out.
            Some(url) => {
                out.push('[');
                push_escaped(out, text);
                out.push_str("](<");
                out.push_str(url.as_str());
                out.push_str(">)");
            }
            None => push_escaped(out, text),
        }
        rest = &rest[start + text.len()..];
    }
    push_escaped(out, rest);
}

/// Where a URL starts: `http://`, `https://` or `www.`, in any case, at the
/// start of a word.
fn find_url(text: &str) -> Option<usize> {
    text.char_indices()
        .map(|(ix, _)| ix)
        .filter(|&ix| !text[..ix].ends_with(char::is_alphanumeric))
        .find(|&ix| {
            ["http://", "https://", WWW]
                .iter()
                .any(|prefix| starts_with_ignore_case(&text[ix..], prefix))
        })
}

const WWW: &str = "www.";

fn starts_with_ignore_case(text: &str, prefix: &str) -> bool {
    text.get(..prefix.len())
        .is_some_and(|start| start.eq_ignore_ascii_case(prefix))
}

/// The URL at the start of `text`, without trailing sentence punctuation.
fn url_at(text: &str) -> &str {
    let end = text
        .find(|c: char| c.is_whitespace() || c == '<' || c == '>')
        .unwrap_or(text.len());
    text[..end].trim_end_matches(['.', ',', '!', '?', ';', ':', ')', '\'', '"'])
}

/// The `http(s)` URL a link to `text` opens, if it has a host and no
/// characters that could disguise it; `www.` opens over `https`.
pub fn link_target(text: &str) -> Option<Url> {
    if text.chars().any(hidden) {
        return None;
    }
    let www = starts_with_ignore_case(text, WWW);
    let url = if www {
        Url::parse(&format!("https://{text}"))
    } else {
        Url::parse(text)
    }
    .ok()?;
    let web = matches!(url.scheme(), "http" | "https");
    let host = url.host_str().filter(|host| !host.is_empty())?;
    let plain = !www || (url.username().is_empty() && url.password().is_none() && domain(host));
    (web && plain).then_some(url)
}

/// `www.`, a name and a top-level domain of letters (or punycode).
fn domain(host: &str) -> bool {
    let labels: Vec<&str> = host.split('.').collect();
    let tld = labels.last().copied().unwrap_or_default();
    host.starts_with(WWW)
        && labels.len() >= 3
        && labels.iter().all(|label| !label.is_empty())
        && (tld.starts_with("xn--")
            || tld.len() >= 2 && tld.chars().all(|c| c.is_ascii_alphabetic()))
}

/// `name` as a bidi isolate, so it can't reorder the text around it.
pub fn isolate(name: &str) -> String {
    format!("\u{2068}{name}\u{2069}")
}

/// `name` with its hidden characters spelled out as code points, isolated.
pub fn visible(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    for c in name.chars() {
        if hidden(c) {
            out.push_str(&format!("<U+{:04X}>", u32::from(c)));
        } else {
            out.push(c);
        }
    }
    isolate(&out)
}

/// The host to show for `url`: ASCII, with IDN labels in punycode, and any
/// port.
pub fn link_host(url: &Url) -> String {
    let host = url.host_str().unwrap_or_default();
    match url.port() {
        Some(port) => format!("{host}:{port}"),
        None => host.to_owned(),
    }
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
            "see [https\\:\\/\\/x\\.org\\/a\\_b](<https://x.org/a_b>)\\. ok"
        );
        assert_eq!(
            message_markdown("(WWW.X.org)"),
            "\\([WWW\\.X\\.org](<https://www.x.org/>)\\)"
        );
    }

    /// The links in `text`, by target.
    fn targets(text: &str) -> Vec<String> {
        let markdown = message_markdown(text);
        markdown
            .split("](<")
            .skip(1)
            .map(|rest| rest[..rest.find(">)").unwrap()].to_owned())
            .collect()
    }

    #[test]
    fn links_any_case_and_bare_www() {
        let cases = [
            ("HTTP://www.google.com", "http://www.google.com/"),
            ("Https://X.org/A", "https://x.org/A"),
            ("www.google.com", "https://www.google.com/"),
            ("go to www.x.co.uk/a?b=1.", "https://www.x.co.uk/a?b=1"),
            ("www.x.com:8080/a", "https://www.x.com:8080/a"),
            ("www.x.xn--p1ai", "https://www.x.xn--p1ai/"),
            ("http://localhost:3000", "http://localhost:3000/"),
        ];
        for (text, target) in cases {
            assert_eq!(targets(text), [target], "{text:?}");
        }
    }

    #[test]
    fn leaves_malformed_www_and_mid_word_urls_unlinked() {
        for text in [
            "www.",
            "www.google",
            "www..com",
            "www.x.c",
            "www.x.123",
            "www.x.com@evil.com",
            "awww.x.com",
            "xhttps://x.org",
            "wwwxcom",
        ] {
            assert_eq!(targets(text), Vec::<String>::new(), "{text:?}");
        }
    }

    #[test]
    fn leaves_disguised_or_hostless_urls_unlinked() {
        for text in [
            "https://x.org/\u{202e}gpj.exe",
            "https://pay\u{200b}pal.com",
            "https://x.org/\u{2066}a",
            "https://x.org\u{ad}",
            "https://",
            "https://:80/",
        ] {
            assert!(!message_markdown(text).contains("](<"), "{text:?} linked");
            assert!(link_target(text).is_none(), "{text:?} accepted");
        }
    }

    #[test]
    fn isolates_wrap_names() {
        assert_eq!(isolate("\u{5d0}"), "\u{2068}\u{5d0}\u{2069}");
    }

    #[test]
    fn hidden_characters_are_spelled_out() {
        assert_eq!(
            visible("a\u{202e}fdp.exe\u{200b}\u{e0001}"),
            "\u{2068}a<U+202E>fdp.exe<U+200B><U+E0001>\u{2069}"
        );
        assert_eq!(visible("report.pdf"), "\u{2068}report.pdf\u{2069}");
    }

    #[test]
    fn hosts_show_in_ascii() {
        let host = |text| link_host(&link_target(text).unwrap());
        assert_eq!(host("https://\u{430}pple.com/a"), "xn--pple-43d.com");
        assert_eq!(host("http://user@x.org:8080/"), "x.org:8080");
        assert_eq!(host("HTTPS://X.ORG"), "x.org");
        assert_eq!(
            link_target("https://\u{430}pple.com/ä").unwrap().as_str(),
            "https://xn--pple-43d.com/%C3%A4"
        );
    }
}
