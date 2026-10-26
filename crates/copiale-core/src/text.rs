//! Characters that can disguise text: bidi controls and invisible ones,
//! the longest message body, file name and group title kept, and safe
//! names to save files as.

use std::borrow::Cow;

use unicode_segmentation::UnicodeSegmentation as _;

/// Longest message body stored and rendered, in bytes.
pub const MAX_BODY_BYTES: usize = 64 * 1024;

/// Longest attachment file name stored, in bytes.
pub const MAX_FILE_NAME_BYTES: usize = 255;

/// Longest name [`save_name`] gives, in bytes; room is left for a counter.
const MAX_SAVE_NAME_BYTES: usize = 200;

/// Controls, bidi controls, line and paragraph separators, and invisible
/// characters.
pub fn hidden(c: char) -> bool {
    c.is_control()
        || matches!(
            c,
            '\u{ad}'
                | '\u{34f}'
                | '\u{61c}'
                | '\u{115f}'..='\u{1160}'
                | '\u{17b4}'..='\u{17b5}'
                | '\u{180b}'..='\u{180f}'
                | '\u{200b}'..='\u{200f}'
                | '\u{2028}'..='\u{202e}'
                | '\u{2060}'..='\u{206f}'
                | '\u{3164}'
                | '\u{fe00}'..='\u{fe0f}'
                | '\u{feff}'
                | '\u{ffa0}'
                | '\u{fff9}'..='\u{fffb}'
                | '\u{1d173}'..='\u{1d17a}'
                | '\u{e0000}'..='\u{e0fff}'
        )
}

/// Hidden characters names keep: joiners and variation selectors, for
/// emoji and scripts that need them, and emoji tag sequences.
fn joins(c: char) -> bool {
    matches!(
        c,
        '\u{180b}'..='\u{180d}'
            | '\u{180f}'
            | '\u{200c}'..='\u{200d}'
            | '\u{fe00}'..='\u{fe0f}'
            | '\u{e0020}'..='\u{e007f}'
    )
}

/// `name` without bidi controls or invisible characters, line breaks and
/// tabs as spaces, trimmed; `None` if blank.
pub fn clean_name(name: &str) -> Option<String> {
    let cleaned: String = name
        .chars()
        .filter_map(|c| match c {
            '\u{2028}' | '\u{2029}' => Some(' '),
            c if c.is_control() && c.is_whitespace() => Some(' '),
            c if hidden(c) && !joins(c) => None,
            c => Some(c),
        })
        .collect();
    let trimmed = cleaned.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_owned())
}

/// Longest group title, in graphemes, as in Signal.
pub const MAX_GROUP_TITLE: usize = 32;

/// `title` as [`clean_name`] leaves it; `None` if blank or longer than
/// [`MAX_GROUP_TITLE`].
pub fn group_title(title: &str) -> Option<String> {
    clean_name(title).filter(|title| title.graphemes(true).count() <= MAX_GROUP_TITLE)
}

/// `body` cut to at most [`MAX_BODY_BYTES`] on a character boundary,
/// ending in "…" if cut.
pub fn clip_body(body: &str) -> Cow<'_, str> {
    clip(body, MAX_BODY_BYTES)
}

/// `name` cut to at most [`MAX_FILE_NAME_BYTES`], as [`clip_body`].
pub fn clip_file_name(name: &str) -> Cow<'_, str> {
    clip(name, MAX_FILE_NAME_BYTES)
}

/// A name to save an attachment as: the sender's, without path separators,
/// characters that disguise it or that filesystems refuse, leading dots,
/// or trailing dots and spaces; else `attachment`, with an extension for
/// its content type.
pub fn save_name(name: Option<&str>, content_type: Option<&str>) -> String {
    let replaced: String = name
        .unwrap_or_default()
        .chars()
        .filter(|c| !hidden(*c))
        .map(|c| match c {
            '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => '_',
            c => c,
        })
        .collect();
    let trimmed = replaced
        .trim()
        .trim_start_matches('.')
        .trim_end_matches(['.', ' ']);
    if trimmed.is_empty() {
        return format!("attachment{}", extension(content_type.unwrap_or_default()));
    }
    let stem = trimmed
        .split('.')
        .next()
        .unwrap_or_default()
        .to_ascii_uppercase();
    let device = matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || (stem.len() == 4
            && (stem.starts_with("COM") || stem.starts_with("LPT"))
            && stem.as_bytes()[3].is_ascii_digit());
    let name = if device {
        format!("_{trimmed}")
    } else {
        trimmed.to_owned()
    };
    if name.len() <= MAX_SAVE_NAME_BYTES {
        return name;
    }
    // Keeps a short extension.
    let (stem, extension) = match name.rfind('.') {
        Some(dot) if name.len() - dot <= 16 => name.split_at(dot),
        _ => (name.as_str(), ""),
    };
    let end = stem.floor_char_boundary(MAX_SAVE_NAME_BYTES - extension.len());
    format!("{}{extension}", &stem[..end])
}

/// A file extension for common content types, with its dot.
fn extension(content_type: &str) -> &'static str {
    match content_type {
        "image/jpeg" => ".jpg",
        "image/png" => ".png",
        "image/gif" => ".gif",
        "image/webp" => ".webp",
        "image/heic" => ".heic",
        "video/mp4" => ".mp4",
        "video/quicktime" => ".mov",
        "audio/aac" => ".aac",
        "audio/mpeg" => ".mp3",
        "audio/ogg" => ".ogg",
        "application/pdf" => ".pdf",
        "application/zip" => ".zip",
        "text/plain" => ".txt",
        _ => "",
    }
}

fn clip(text: &str, max: usize) -> Cow<'_, str> {
    if text.len() <= max {
        return Cow::Borrowed(text);
    }
    let end = text.floor_char_boundary(max - '…'.len_utf8());
    Cow::Owned(format!("{}…", &text[..end]))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn group_titles_are_cleaned_and_bounded_in_graphemes() {
        assert_eq!(group_title(" Trip\u{202e} ").as_deref(), Some("Trip"));
        assert_eq!(group_title(" \u{200b} "), None);
        let longest = "👩‍👩‍👧".repeat(MAX_GROUP_TITLE);
        assert_eq!(group_title(&longest).as_deref(), Some(longest.as_str()));
        assert_eq!(group_title(&format!("{longest}a")), None);
    }

    #[test]
    fn names_lose_bidi_controls_and_invisible_characters() {
        let cases = [
            ("Alice", "Alice"),
            ("  Alice\u{202e}gnp.exe ", "Alicegnp.exe"),
            ("Al\u{200b}ice\u{2066}\u{2069}", "Alice"),
            ("\u{feff}Bob\u{ad}\u{2060}", "Bob"),
            ("Bob\u{e0001}\u{1d173}\u{3164}", "Bob"),
            ("Ann\nLee\tSmith\u{2028}Jr", "Ann Lee Smith Jr"),
            ("Ann\u{0}\u{7}", "Ann"),
        ];
        for (name, expected) in cases {
            assert_eq!(clean_name(name).as_deref(), Some(expected), "{name:?}");
        }
    }

    #[test]
    fn names_keep_joiners_and_variation_selectors() {
        for name in [
            "👩\u{200d}💻 Dev",
            "❤\u{fe0f} Ann",
            "\u{645}\u{6cc}\u{200c}\u{62e}\u{648}\u{627}\u{647}\u{645}",
            "🏴\u{e0067}\u{e0062}\u{e0065}\u{e006e}\u{e0067}\u{e007f}",
            "\u{5e9}\u{5dc}\u{5d5}\u{5dd}",
        ] {
            assert_eq!(clean_name(name).as_deref(), Some(name), "{name:?}");
        }
    }

    #[test]
    fn blank_names_are_none() {
        for name in ["", "  ", "\u{200b}", "\u{202e}\u{2069} \u{feff}", "\n\t"] {
            assert_eq!(clean_name(name), None, "{name:?}");
        }
    }

    #[test]
    fn long_bodies_are_clipped_on_a_character_boundary() {
        assert!(matches!(clip_body("hi"), Cow::Borrowed("hi")));
        let fits = "a".repeat(MAX_BODY_BYTES);
        assert!(matches!(clip_body(&fits), Cow::Borrowed(_)));

        let long = format!("a{}", "é".repeat(MAX_BODY_BYTES));
        let clipped = clip_body(&long);
        assert!(clipped.len() <= MAX_BODY_BYTES);
        assert!(clipped.ends_with("é…"));
        assert!(long.starts_with(clipped.trim_end_matches('…')));
    }

    #[test]
    fn saved_names_are_safe() {
        let cases = [
            (Some("report.pdf"), None, "report.pdf"),
            (Some("../../.bashrc"), None, "_.._.bashrc"),
            (Some("a/b\\c:d*e?f\"g<h>i|j"), None, "a_b_c_d_e_f_g_h_i_j"),
            (Some("photo\u{202e}gpj.exe"), None, "photogpj.exe"),
            (Some(".hidden"), None, "hidden"),
            (Some("name. . "), None, "name"),
            (Some("CON.txt"), None, "_CON.txt"),
            (Some("com1"), None, "_com1"),
            (Some("console.txt"), None, "console.txt"),
            (None, Some("image/jpeg"), "attachment.jpg"),
            (Some(" \u{200b} "), Some("text/x-unknown"), "attachment"),
        ];
        for (name, content_type, expected) in cases {
            assert_eq!(save_name(name, content_type), expected, "{name:?}");
        }
        let long = format!("{}.tar.gz", "ä".repeat(MAX_SAVE_NAME_BYTES));
        let saved = save_name(Some(&long), None);
        assert!(saved.len() <= MAX_SAVE_NAME_BYTES);
        assert!(saved.ends_with("ä.gz"));
    }

    #[test]
    fn long_file_names_are_clipped() {
        assert!(matches!(clip_file_name("a.txt"), Cow::Borrowed("a.txt")));
        let long = format!("{}.pdf", "ü".repeat(MAX_FILE_NAME_BYTES));
        let clipped = clip_file_name(&long);
        assert!(clipped.len() <= MAX_FILE_NAME_BYTES);
        assert!(clipped.ends_with("ü…"));
    }
}
