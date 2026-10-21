//! Characters that can disguise text: bidi controls and invisible ones.

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

#[cfg(test)]
mod tests {
    use super::*;

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
}
